use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;

const REQUIRED_VERSION: &str = "1.14.5";
const ARM64_SHA256: &str = "f5b3a8afc214cd48c96fa1046915d7ea0e21ba1138b2cedac305208729f0ccd5";
const X86_64_SHA256: &str = "3bce822388fc54593a64aeff45b03a0beeedc18e45230268526608375e4d318c";
static WIMLIB_PATH: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WimlibCapability {
    pub supported: bool,
    pub backend: Option<String>,
    pub reason: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Error)]
pub(crate) enum WimlibError {
    #[error("bundled wimlib-imagex has not been configured")]
    NotConfigured,
    #[error("bundled wimlib-imagex is missing: {0}")]
    Missing(String),
    #[error("bundled wimlib-imagex has the wrong CPU architecture")]
    WrongArchitecture,
    #[error("bundled wimlib-imagex version is not the required {REQUIRED_VERSION}")]
    WrongVersion,
    #[error("bundled wimlib-imagex failed its SHA-256 integrity check")]
    Checksum,
    #[error("bundled wimlib-imagex exceeded its execution time limit")]
    Timeout,
    #[error("could not execute bundled wimlib-imagex: {0}")]
    Io(#[from] std::io::Error),
    #[error("wimlib-imagex failed ({status}): {message}")]
    Command { status: String, message: String },
}

pub fn configure_wimlib(path: PathBuf) -> Result<(), String> {
    if let Some(existing) = WIMLIB_PATH.get() {
        return if existing == &path {
            Ok(())
        } else {
            Err("wimlib path was already configured".into())
        };
    }
    WIMLIB_PATH
        .set(path)
        .map_err(|_| "wimlib path was already configured".into())
}

fn configured_path() -> Result<&'static Path, WimlibError> {
    WIMLIB_PATH
        .get()
        .map(PathBuf::as_path)
        .ok_or(WimlibError::NotConfigured)
}

fn expected_cpu_type() -> Option<u32> {
    match std::env::consts::ARCH {
        "aarch64" => Some(0x0100_000c),
        "x86_64" => Some(0x0100_0007),
        _ => None,
    }
}

fn verify_architecture(path: &Path) -> Result<(), WimlibError> {
    #[cfg(target_os = "macos")]
    {
        let bytes = fs::read(path)?;
        if bytes.len() < 8 {
            return Err(WimlibError::WrongArchitecture);
        }
        let magic = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        let cpu = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if magic != 0xfeed_facf || Some(cpu) != expected_cpu_type() {
            return Err(WimlibError::WrongArchitecture);
        }
    }
    Ok(())
}

fn verify_checksum(path: &Path) -> Result<(), WimlibError> {
    let expected = match std::env::consts::ARCH {
        "aarch64" => ARM64_SHA256,
        "x86_64" => X86_64_SHA256,
        _ => return Err(WimlibError::WrongArchitecture),
    };
    let actual = format!("{:x}", Sha256::digest(fs::read(path)?));
    if actual == expected {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    if Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
    {
        return Ok(());
    }
    Err(WimlibError::Checksum)
}

fn sanitized_output(output: &[u8]) -> String {
    const LIMIT: usize = 4096;
    String::from_utf8_lossy(&output[..output.len().min(LIMIT)])
        .replace(['\r', '\n'], " ")
        .trim()
        .to_owned()
}

fn execute_bounded(
    executable: &Path,
    args: &[OsString],
    timeout: Duration,
) -> Result<Output, WimlibError> {
    const CAPTURE_LIMIT: usize = 4096;
    let mut child = Command::new(executable)
        .args(args)
        // WIM paths have Windows semantics even when wimlib runs on macOS.
        // Without this, third-party PE images whose directory casing differs
        // from DISM output are incorrectly reported as missing.
        .env("WIMLIB_IMAGEX_IGNORE_CASE", "yes")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let read_stream = |mut stream: Box<dyn Read + Send>| {
        thread::spawn(move || -> std::io::Result<Vec<u8>> {
            let mut captured = Vec::new();
            let mut buffer = [0_u8; 8192];
            loop {
                let count = stream.read(&mut buffer)?;
                if count == 0 {
                    return Ok(captured);
                }
                let remaining = CAPTURE_LIMIT.saturating_sub(captured.len());
                captured.extend_from_slice(&buffer[..count.min(remaining)]);
            }
        })
    };
    let stdout = read_stream(Box::new(child.stdout.take().expect("stdout was piped")));
    let stderr = read_stream(Box::new(child.stderr.take().expect("stderr was piped")));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout.join();
            let _ = stderr.join();
            return Err(WimlibError::Timeout);
        }
        thread::sleep(Duration::from_millis(25));
    };
    let stdout = stdout
        .join()
        .map_err(|_| WimlibError::Io(std::io::Error::other("wimlib stdout reader failed")))??;
    let stderr = stderr
        .join()
        .map_err(|_| WimlibError::Io(std::io::Error::other("wimlib stderr reader failed")))??;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn raw_output(args: &[OsString]) -> Result<Output, WimlibError> {
    let path = configured_path()?;
    if !path.is_file() {
        return Err(WimlibError::Missing(path.display().to_string()));
    }
    execute_bounded(path, args, Duration::from_secs(5 * 60))
}

pub(crate) fn run(args: &[OsString]) -> Result<Output, WimlibError> {
    let output = raw_output(args)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(WimlibError::Command {
            status: output.status.to_string(),
            message: format!(
                "{} {}",
                sanitized_output(&output.stdout),
                sanitized_output(&output.stderr)
            )
            .trim()
            .into(),
        })
    }
}

fn probe() -> Result<String, WimlibError> {
    let path = configured_path()?;
    if !path.is_file() {
        return Err(WimlibError::Missing(path.display().to_string()));
    }
    verify_architecture(path)?;
    verify_checksum(path)?;
    let output = run(&["--version".into()])?;
    let text = String::from_utf8_lossy(&output.stdout);
    if !text.contains(&format!("wimlib-imagex {REQUIRED_VERSION}")) {
        return Err(WimlibError::WrongVersion);
    }
    Ok(REQUIRED_VERSION.into())
}

pub fn wimlib_capability() -> WimlibCapability {
    #[cfg(target_os = "windows")]
    return WimlibCapability {
        supported: true,
        backend: Some("windows_native".into()),
        reason: None,
        version: None,
    };
    #[cfg(target_os = "macos")]
    return match probe() {
        Ok(version) => WimlibCapability {
            supported: true,
            backend: Some("wimlib".into()),
            reason: None,
            version: Some(version),
        },
        Err(error) => WimlibCapability {
            supported: false,
            backend: None,
            reason: Some(error.to_string()),
            version: None,
        },
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    WimlibCapability {
        supported: false,
        backend: None,
        reason: Some("WinPE import is unsupported on this platform".into()),
        version: None,
    }
}

pub(crate) fn extract(
    wim: &Path,
    index: u32,
    image_path: &str,
    destination: &Path,
) -> Result<(), WimlibError> {
    fs::create_dir_all(destination)?;
    run(&[
        "extract".into(),
        wim.as_os_str().into(),
        index.to_string().into(),
        image_path.into(),
        format!("--dest-dir={}", destination.display()).into(),
        "--no-acls".into(),
        "--no-attributes".into(),
    ])?;
    Ok(())
}

pub(crate) fn image_path_exists(
    wim: &Path,
    index: u32,
    image_path: &str,
) -> Result<bool, WimlibError> {
    let output = raw_output(&[
        "dir".into(),
        wim.as_os_str().into(),
        index.to_string().into(),
        format!("--path={image_path}").into(),
    ])?;
    Ok(output.status.success())
}

fn quote_command_path(path: &Path) -> Result<String, WimlibError> {
    let path = path.to_string_lossy();
    if path.contains(['"', '\r', '\n']) {
        return Err(WimlibError::Command {
            status: "invalid path".into(),
            message: "temporary path contains unsupported characters".into(),
        });
    }
    Ok(format!("\"{path}\""))
}

pub(crate) fn add_tree(wim: &Path, index: u32, source: &Path) -> Result<(), WimlibError> {
    let command = format!("add {} /", quote_command_path(source)?);
    run(&[
        "update".into(),
        wim.as_os_str().into(),
        index.to_string().into(),
        format!("--command={command}").into(),
        "--check".into(),
    ])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_paths_reject_newlines_and_quotes() {
        assert!(
            quote_command_path(Path::new("/tmp/a b"))
                .unwrap()
                .starts_with('"')
        );
        assert!(quote_command_path(Path::new("/tmp/a\nadd evil")).is_err());
        assert!(quote_command_path(Path::new("/tmp/\"evil")).is_err());
    }

    #[test]
    fn output_is_bounded() {
        assert!(sanitized_output(&vec![b'x'; 20_000]).len() <= 4096);
    }

    #[cfg(unix)]
    #[test]
    fn process_adapter_enforces_timeout_and_capture_limit() {
        let output = execute_bounded(
            Path::new("/bin/sh"),
            &["-c".into(), "yes x | head -c 20000".into()],
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(output.stdout.len(), 4096);
        assert!(matches!(
            execute_bounded(
                Path::new("/bin/sh"),
                &["-c".into(), "sleep 1".into()],
                Duration::from_millis(30),
            ),
            Err(WimlibError::Timeout)
        ));
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn bundled_wimlib_can_inspect_extract_and_update_a_wim() {
        let bundled = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/desktop/src-tauri/binaries/wimlib-imagex-aarch64-apple-darwin");
        configure_wimlib(bundled).unwrap();
        assert!(wimlib_capability().supported);
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source/WINDOWS/SYSTEM32/BOOT");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("winload.exe"), b"loader").unwrap();
        let wim = temporary.path().join("boot.wim");
        run(&[
            "capture".into(),
            temporary.path().join("source").into_os_string(),
            wim.as_os_str().into(),
            "Test".into(),
            "--boot".into(),
        ])
        .unwrap();
        assert!(image_path_exists(&wim, 1, "/windows/system32/boot/winload.exe").unwrap());
        let additions = temporary.path().join("add/EasyDeployMesh");
        fs::create_dir_all(&additions).unwrap();
        fs::write(additions.join("agent.exe"), b"agent").unwrap();
        add_tree(&wim, 1, &temporary.path().join("add")).unwrap();
        assert!(image_path_exists(&wim, 1, "/EasyDeployMesh/agent.exe").unwrap());
        let extracted = temporary.path().join("extract");
        extract(&wim, 1, "/EasyDeployMesh/agent.exe", &extracted).unwrap();
        assert_eq!(fs::read(extracted.join("agent.exe")).unwrap(), b"agent");
    }
}
