use std::{
    io::{ErrorKind, Read, Write},
    net::TcpListener,
    process::Command,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[test]
fn health_check_uses_only_the_unauthenticated_read_only_endpoint() {
    let (endpoint, request, server) = serve_once("200 OK", r#"{"status":"ok","version":"0.2.2"}"#);

    let output = Command::new(env!("CARGO_BIN_EXE_easydeploymesh-agent"))
        .args([
            "--server",
            &endpoint,
            "--enrollment-token",
            "easydeploymesh_enroll_must_not_be_sent",
            "--health-check",
        ])
        .output()
        .expect("health-check Agent should run");
    server.join().expect("test server should stop");
    let request = request
        .recv_timeout(Duration::from_secs(2))
        .expect("health check should reach the server");

    assert!(
        output.status.success(),
        "health check should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        request.lines().next(),
        Some("GET /health HTTP/1.1"),
        "health check must not register, heartbeat, or claim a job"
    );
    assert!(!request.to_ascii_lowercase().contains("authorization:"));
    assert!(!request.contains("easydeploymesh_enroll_must_not_be_sent"));
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("EASYDEPLOYMESH_DIAG_V1|control.health|ok|status=200")
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout
            .lines()
            .any(|line| line == "EASYDEPLOYMESH_DIAG_V1|network.ipv4|usable")
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == "EASYDEPLOYMESH_DIAG_V1|network.route_to_control|present")
    );
    for output in [&*stdout, &*stderr] {
        assert!(!output.contains(&endpoint));
        assert!(!output.contains("127.0.0.1"));
        assert!(!output.contains("easydeploymesh_enroll_must_not_be_sent"));
    }
}

#[test]
fn health_check_reports_a_stable_http_error_marker() {
    let (endpoint, request, server) = serve_once("503 Service Unavailable", "");

    let output = Command::new(env!("CARGO_BIN_EXE_easydeploymesh-agent"))
        .args([
            "--server",
            &endpoint,
            "--enrollment-token",
            "easydeploymesh_enroll_must_not_be_sent",
            "--health-check",
        ])
        .output()
        .expect("health-check Agent should run");
    server.join().expect("test server should stop");
    let captured = request
        .recv_timeout(Duration::from_secs(2))
        .expect("health check should reach the server");

    assert!(!output.status.success());
    assert_eq!(captured.lines().next(), Some("GET /health HTTP/1.1"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout
            .lines()
            .any(|line| line == "EASYDEPLOYMESH_DIAG_V1|control.health|http_error|status=503")
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == "EASYDEPLOYMESH_DIAG_V1|network.ipv4|usable")
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == "EASYDEPLOYMESH_DIAG_V1|network.route_to_control|present")
    );
    for output in [&*stdout, &*stderr] {
        assert!(!output.contains(&endpoint));
        assert!(!output.contains("127.0.0.1"));
        assert!(!output.contains("easydeploymesh_enroll_must_not_be_sent"));
    }
}

#[test]
fn health_check_reports_a_stable_marker_without_registration_credentials() {
    let output = Command::new(env!("CARGO_BIN_EXE_easydeploymesh-agent"))
        .args(["--server", "http://127.0.0.1:9", "--health-check"])
        .output()
        .expect("health-check Agent should run");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("EASYDEPLOYMESH_DIAG_V1|control.health|not_run|reason=bootstrap_error")
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line == "EASYDEPLOYMESH_DIAG_V1|network.ipv4|unknown")
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == "EASYDEPLOYMESH_DIAG_V1|network.route_to_control|unknown")
    );
}

#[test]
fn health_check_reports_unknown_network_without_echoing_an_invalid_server() {
    let output = Command::new(env!("CARGO_BIN_EXE_easydeploymesh-agent"))
        .args([
            "--server",
            "invalid-secret-control-host",
            "--enrollment-token",
            "easydeploymesh_enroll_must_not_be_printed",
            "--health-check",
        ])
        .output()
        .expect("health-check Agent should run");

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout
            .lines()
            .any(|line| line == "EASYDEPLOYMESH_DIAG_V1|network.ipv4|unknown")
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == "EASYDEPLOYMESH_DIAG_V1|network.route_to_control|unknown")
    );
    assert!(
        stdout
            .lines()
            .any(|line| line
                == "EASYDEPLOYMESH_DIAG_V1|control.health|not_run|reason=server_invalid")
    );
    for output in [&*stdout, &*stderr] {
        assert!(!output.contains("invalid-secret-control-host"));
        assert!(!output.contains("easydeploymesh_enroll_must_not_be_printed"));
    }
}

fn serve_once(
    status: &'static str,
    body: &'static str,
) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let endpoint = format!(
        "http://{}",
        listener
            .local_addr()
            .expect("test listener should have an address")
    );
    listener
        .set_nonblocking(true)
        .expect("test listener should become nonblocking");
    let (request_tx, request_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    let _ = request_tx.send(String::new());
                    return;
                }
                Err(error) => panic!("health request should connect: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("test stream should time out");
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    bytes.extend_from_slice(&buffer[..read]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) => panic!("health request should read: {error}"),
            }
        }
        request_tx
            .send(String::from_utf8_lossy(&bytes).into_owned())
            .expect("captured request should send");
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("health response should write");
    });
    (endpoint, request_rx, server)
}
