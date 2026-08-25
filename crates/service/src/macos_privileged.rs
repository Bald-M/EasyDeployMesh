use nix::sys::socket::{ControlMessage, ControlMessageOwned, MsgFlags, recvmsg, sendmsg};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    ffi::CString,
    fs,
    io::{IoSlice, IoSliceMut, Read},
    net::{Ipv4Addr, SocketAddrV4, UdpSocket as StdUdpSocket},
    num::NonZeroU32,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{fs::PermissionsExt, net::UnixListener, net::UnixStream},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tokio::net::UdpSocket;

const HELPER_ARGUMENT: &str = "--easydeploymesh-bind-pxe-sockets";
const PRIVILEGED_PORTS: [u16; 3] = [67, 68, 69];

pub(crate) struct PrivilegedPxeSockets {
    pub server: UdpSocket,
    pub client: UdpSocket,
    pub tftp: UdpSocket,
}

fn bound_socket(address: Ipv4Addr, port: u16) -> Result<Socket, String> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .map_err(|error| format!("could not create privileged UDP {port}: {error}"))?;
    socket
        .set_reuse_address(true)
        .map_err(|error| format!("could not configure privileged UDP {port}: {error}"))?;
    socket
        .bind(&SocketAddrV4::new(address, port).into())
        .map_err(|error| format!("could not bind privileged UDP {port} on {address}: {error}"))?;
    socket
        .set_nonblocking(true)
        .map_err(|error| format!("could not configure privileged UDP {port}: {error}"))?;
    Ok(socket)
}

fn interface_index(address: Ipv4Addr) -> Result<NonZeroU32, String> {
    let interface = if_addrs::get_if_addrs()
        .map_err(|error| format!("could not inspect the PXE network interface: {error}"))?
        .into_iter()
        .find(|interface| interface.ip() == address)
        .ok_or_else(|| format!("PXE address {address} is not assigned to a network interface"))?;
    let name = CString::new(interface.name)
        .map_err(|_| "PXE network interface has an invalid name".to_owned())?;
    let index = unsafe { nix::libc::if_nametoindex(name.as_ptr()) };
    NonZeroU32::new(index)
        .ok_or_else(|| format!("could not resolve the PXE network interface for {address}"))
}

fn bound_dhcp_socket(address: Ipv4Addr, port: u16) -> Result<Socket, String> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .map_err(|error| format!("could not create privileged UDP {port}: {error}"))?;
    socket
        .set_reuse_address(true)
        .map_err(|error| format!("could not configure privileged UDP {port}: {error}"))?;
    socket
        .bind_device_by_index_v4(Some(interface_index(address)?))
        .map_err(|error| {
            format!("could not bind privileged UDP {port} to the interface for {address}: {error}")
        })?;
    // DHCP clients send to a subnet or limited broadcast address. On macOS a
    // socket bound to the interface's unicast address does not receive those
    // packets, so receive on INADDR_ANY while IP_BOUND_IF keeps the socket
    // restricted to the selected interface.
    socket
        .bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())
        .map_err(|error| format!("could not bind privileged UDP {port} on {address}: {error}"))?;
    socket
        .set_nonblocking(true)
        .map_err(|error| format!("could not configure privileged UDP {port}: {error}"))?;
    Ok(socket)
}

fn send_sockets(stream: &UnixStream, sockets: &[Socket; 3]) -> Result<(), String> {
    let descriptors = [
        sockets[0].as_raw_fd(),
        sockets[1].as_raw_fd(),
        sockets[2].as_raw_fd(),
    ];
    sendmsg::<()>(
        stream.as_raw_fd(),
        &[IoSlice::new(b"P")],
        &[ControlMessage::ScmRights(&descriptors)],
        MsgFlags::empty(),
        None,
    )
    .map_err(|error| format!("could not transfer privileged PXE sockets: {error}"))?;
    Ok(())
}

fn run_helper(socket_path: &Path, address: Ipv4Addr) -> Result<(), String> {
    if unsafe { nix::libc::geteuid() } != 0 {
        return Err("the PXE socket helper was not granted administrator privileges".into());
    }
    let sockets = [
        bound_dhcp_socket(address, PRIVILEGED_PORTS[0])?,
        bound_dhcp_socket(address, PRIVILEGED_PORTS[1])?,
        bound_socket(address, PRIVILEGED_PORTS[2])?,
    ];
    let stream = UnixStream::connect(socket_path)
        .map_err(|error| format!("could not connect to the desktop PXE socket: {error}"))?;
    send_sockets(&stream, &sockets)
}

pub fn run_privileged_socket_helper_from_args() -> Option<Result<(), String>> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(HELPER_ARGUMENT)) {
        return None;
    }
    let socket = arguments
        .next()
        .ok_or_else(|| "missing PXE socket path".to_owned());
    let address = arguments
        .next()
        .ok_or_else(|| "missing PXE bind address".to_owned())
        .and_then(|value| {
            value
                .to_string_lossy()
                .parse::<Ipv4Addr>()
                .map_err(|_| "invalid PXE bind address".to_owned())
        });
    Some(
        socket
            .and_then(|socket| address.and_then(|address| run_helper(Path::new(&socket), address))),
    )
}

fn shell_quote(value: &Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
}

fn apple_script_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn receive_sockets(stream: &UnixStream) -> Result<[OwnedFd; 3], String> {
    let mut marker = [0_u8; 1];
    let mut buffers = [IoSliceMut::new(&mut marker)];
    let mut control = nix::cmsg_space!([RawFd; 3]);
    let message = recvmsg::<()>(
        stream.as_raw_fd(),
        &mut buffers,
        Some(&mut control),
        MsgFlags::empty(),
    )
    .map_err(|error| format!("could not receive privileged PXE sockets: {error}"))?;
    let bytes = message.bytes;
    let mut received = None;
    for control_message in message
        .cmsgs()
        .map_err(|error| format!("invalid privileged PXE socket response: {error}"))?
    {
        if let ControlMessageOwned::ScmRights(descriptors) = control_message {
            received = Some(descriptors.try_into().map_err(|_| {
                "privileged PXE socket helper returned the wrong socket count".to_owned()
            })?);
        }
    }
    let _ = message;
    let _ = buffers;
    if bytes != 1 || marker != *b"P" {
        return Err("privileged PXE socket helper returned an invalid response".into());
    }
    let descriptors: [RawFd; 3] =
        received.ok_or_else(|| "privileged PXE socket helper returned no sockets".to_owned())?;
    Ok(descriptors.map(|descriptor| unsafe { OwnedFd::from_raw_fd(descriptor) }))
}

fn bounded_stderr(mut stderr: impl Read) -> String {
    let mut output = Vec::new();
    let _ = stderr.by_ref().take(4096).read_to_end(&mut output);
    String::from_utf8_lossy(&output)
        .replace(['\r', '\n'], " ")
        .trim()
        .to_owned()
}

fn create_authorization_directory() -> Result<PathBuf, String> {
    for _ in 0..8 {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let directory = Path::new("/tmp").join(format!("edmpxe-{}", &token[..12]));
        match fs::create_dir(&directory) {
            Ok(()) => {
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(
                    |error| format!("could not secure PXE authorization directory: {error}"),
                )?;
                return Ok(directory);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "could not create PXE authorization directory: {error}"
                ));
            }
        }
    }
    Err("could not allocate a unique PXE authorization directory".into())
}

fn acquire_blocking(address: Ipv4Addr) -> Result<[OwnedFd; 3], String> {
    let directory = create_authorization_directory()?;
    let socket_path = directory.join("socket");
    let listener = UnixListener::bind(&socket_path).map_err(|error| {
        let _ = fs::remove_dir(&directory);
        format!("could not create PXE authorization socket: {error}")
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("could not configure PXE authorization socket: {error}"))?;
    let executable = std::env::current_exe()
        .map_err(|error| format!("could not locate the PXE socket helper: {error}"))?;
    let command = format!(
        "{} {} {} {}",
        shell_quote(&executable),
        HELPER_ARGUMENT,
        shell_quote(&socket_path),
        address
    );
    let script = format!(
        "do shell script \"{}\" with administrator privileges",
        apple_script_string(&command)
    );
    let mut child = Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not request PXE administrator authorization: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(120);
    let result = loop {
        match listener.accept() {
            Ok((stream, _)) => break receive_sockets(&stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => break Err(format!("could not accept privileged PXE sockets: {error}")),
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("could not monitor PXE authorization: {error}"))?
        {
            let message = child.stderr.take().map(bounded_stderr).unwrap_or_default();
            break Err(if message.is_empty() {
                format!("PXE administrator authorization failed ({status})")
            } else {
                format!("PXE administrator authorization failed: {message}")
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            break Err("PXE administrator authorization timed out".into());
        }
        thread::sleep(Duration::from_millis(25));
    };
    let _ = child.wait();
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_dir(&directory);
    result
}

pub(crate) async fn acquire_pxe_sockets(address: Ipv4Addr) -> Result<PrivilegedPxeSockets, String> {
    let descriptors = tokio::task::spawn_blocking(move || acquire_blocking(address))
        .await
        .map_err(|error| format!("PXE authorization task failed: {error}"))??;
    let [server, client, tftp] = descriptors.map(|descriptor| {
        let socket = StdUdpSocket::from(descriptor);
        UdpSocket::from_std(socket)
    });
    Ok(PrivilegedPxeSockets {
        server: server.map_err(|error| format!("invalid DHCP server socket: {error}"))?,
        client: client.map_err(|error| format!("invalid DHCP probe socket: {error}"))?,
        tftp: tftp.map_err(|error| format!("invalid TFTP socket: {error}"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfers_only_the_three_bound_udp_descriptors() {
        let sockets = [
            bound_socket(Ipv4Addr::LOCALHOST, 0).unwrap(),
            bound_socket(Ipv4Addr::LOCALHOST, 0).unwrap(),
            bound_socket(Ipv4Addr::LOCALHOST, 0).unwrap(),
        ];
        let expected = [
            sockets[0].local_addr().unwrap().as_socket().unwrap(),
            sockets[1].local_addr().unwrap().as_socket().unwrap(),
            sockets[2].local_addr().unwrap().as_socket().unwrap(),
        ];
        let (sender, receiver) = UnixStream::pair().unwrap();
        send_sockets(&sender, &sockets).unwrap();
        let received = receive_sockets(&receiver).unwrap();
        let actual =
            received.map(|descriptor| StdUdpSocket::from(descriptor).local_addr().unwrap());
        assert_eq!(actual, expected);
    }

    #[test]
    fn dhcp_socket_receives_broadcasts_only_on_the_selected_interface() {
        let expected_index = interface_index(Ipv4Addr::LOCALHOST).unwrap();
        let socket = bound_dhcp_socket(Ipv4Addr::LOCALHOST, 0).unwrap();
        let local = socket.local_addr().unwrap().as_socket().unwrap();

        assert_eq!(local.ip(), std::net::IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(socket.device_index_v4().unwrap(), Some(expected_index));
    }

    #[test]
    fn authorization_command_quotes_paths_for_the_shell_and_apple_script() {
        let quoted = shell_quote(Path::new("/tmp/a b'c"));
        assert_eq!(quoted, "'/tmp/a b'\\''c'");
        assert_eq!(apple_script_string(r#"a\"b"#), r#"a\\\"b"#);
    }

    #[test]
    fn authorization_socket_uses_a_short_owner_only_path() {
        use std::os::unix::ffi::OsStrExt;

        let directory = create_authorization_directory().unwrap();
        let socket = directory.join("socket");
        assert!(socket.as_os_str().as_bytes().len() < 64);
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let listener = UnixListener::bind(&socket).unwrap();
        drop(listener);
        fs::remove_file(socket).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
