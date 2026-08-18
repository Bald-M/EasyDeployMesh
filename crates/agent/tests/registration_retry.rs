use std::{
    io::{ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const TEST_TIMEOUT: Duration = Duration::from_secs(8);

struct TestServer {
    endpoint: String,
    stop: Arc<AtomicBool>,
    requests: mpsc::Receiver<Vec<String>>,
    thread: thread::JoinHandle<()>,
}

#[test]
fn agent_recovers_until_once_completes_a_heartbeat() {
    let TestServer {
        endpoint,
        stop,
        requests,
        thread: server,
    } = start_server(vec![
        ("503 Service Unavailable", ""),
        (
            "200 OK",
            r#"{"deviceId":"00000000-0000-0000-0000-000000000001","deviceToken":"easydeploymesh_device_test","heartbeatIntervalSeconds":2}"#,
        ),
        ("503 Service Unavailable", ""),
        (
            "200 OK",
            r#"{"deviceId":"00000000-0000-0000-0000-000000000001","deviceToken":"easydeploymesh_device_test_2","heartbeatIntervalSeconds":2}"#,
        ),
        (
            "200 OK",
            r#"{"acceptedAt":"2026-08-16T00:00:00Z","nextHeartbeatSeconds":2}"#,
        ),
    ]);

    let mut child = spawn_agent(&endpoint, true);
    let status = wait_before(&mut child, Instant::now() + TEST_TIMEOUT);
    if status.is_none() {
        child.kill().expect("timed-out Agent should stop");
    }
    stop.store(true, Ordering::Relaxed);
    let output = child
        .wait_with_output()
        .expect("Agent output should be collected");
    server.join().expect("test server should stop");
    let requests = requests.recv().expect("test server should report requests");
    let request_lines = request_lines(&requests);

    assert!(
        status.is_some_and(|status| status.success()),
        "Agent should retry and exit successfully after the control plane recovers.\nstatus: {:?}\nrequests: {request_lines:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        request_lines,
        [
            "POST /api/v1/agents/register HTTP/1.1",
            "POST /api/v1/agents/register HTTP/1.1",
            "POST /api/v1/agents/00000000-0000-0000-0000-000000000001/heartbeat HTTP/1.1",
            "POST /api/v1/agents/register HTTP/1.1",
            "POST /api/v1/agents/00000000-0000-0000-0000-000000000001/heartbeat HTTP/1.1",
        ]
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Agent registration failed (recovery attempt 1)")
            && stderr.contains("Agent heartbeat failed (recovery attempt 1)"),
        "Agent should log both recovery attempts with attempt context.\nstderr: {stderr}"
    );
}

#[test]
fn agent_reregisters_after_a_transient_job_claim_failure() {
    let TestServer {
        endpoint,
        stop,
        requests,
        thread: server,
    } = start_server(vec![
        (
            "200 OK",
            r#"{"deviceId":"00000000-0000-0000-0000-000000000001","deviceToken":"easydeploymesh_device_test","heartbeatIntervalSeconds":2}"#,
        ),
        (
            "200 OK",
            r#"{"acceptedAt":"2026-08-16T00:00:00Z","nextHeartbeatSeconds":2}"#,
        ),
        ("503 Service Unavailable", ""),
        (
            "200 OK",
            r#"{"deviceId":"00000000-0000-0000-0000-000000000001","deviceToken":"easydeploymesh_device_test_2","heartbeatIntervalSeconds":2}"#,
        ),
        (
            "200 OK",
            r#"{"acceptedAt":"2026-08-16T00:00:01Z","nextHeartbeatSeconds":2}"#,
        ),
        ("200 OK", "null"),
    ]);
    let mut child = spawn_agent(&endpoint, false);

    let captured = requests
        .recv_timeout(TEST_TIMEOUT)
        .expect("Agent should recover and finish the scripted requests");
    let status = child
        .try_wait()
        .expect("Agent status should be readable after recovery");
    stop.store(true, Ordering::Relaxed);
    if status.is_none() {
        child
            .kill()
            .expect("running Agent should stop after the test");
    }
    let output = child
        .wait_with_output()
        .expect("Agent output should be collected");
    server.join().expect("test server should stop");
    let request_lines = request_lines(&captured);

    assert!(
        status.is_none(),
        "Agent should remain running after recovering from a failed claim.\nstatus: {:?}\nrequests: {request_lines:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        request_lines,
        [
            "POST /api/v1/agents/register HTTP/1.1",
            "POST /api/v1/agents/00000000-0000-0000-0000-000000000001/heartbeat HTTP/1.1",
            "POST /api/v1/agents/00000000-0000-0000-0000-000000000001/jobs/claim HTTP/1.1",
            "POST /api/v1/agents/register HTTP/1.1",
            "POST /api/v1/agents/00000000-0000-0000-0000-000000000001/heartbeat HTTP/1.1",
            "POST /api/v1/agents/00000000-0000-0000-0000-000000000001/jobs/claim HTTP/1.1",
        ]
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Deployment job claim failed (recovery attempt 1)")
            && stderr.contains("retrying registration in 1 second(s)"),
        "Agent should log claim recovery and backoff context.\nstderr: {stderr}"
    );
}

#[test]
fn agent_continues_after_a_failed_job_is_reported() {
    let TestServer {
        endpoint,
        stop,
        requests,
        thread: server,
    } = start_server(vec![
        (
            "200 OK",
            r#"{"deviceId":"00000000-0000-0000-0000-000000000001","deviceToken":"easydeploymesh_device_test","heartbeatIntervalSeconds":2}"#,
        ),
        (
            "200 OK",
            r#"{"acceptedAt":"2026-08-16T00:00:00Z","nextHeartbeatSeconds":2}"#,
        ),
        (
            "200 OK",
            r#"{"jobId":"00000000-0000-0000-0000-000000000010","leaseId":"00000000-0000-0000-0000-000000000011","expiresAt":"2026-08-17T00:00:00Z","operation":"deploy_gho","image":{"id":"00000000-0000-0000-0000-000000000012","name":"Unsupported image","format":"gho","sizeBytes":1,"sha256":"00","downloadUrl":"/image","index":1},"target":{"deviceId":"00000000-0000-0000-0000-000000000001","targetDiskId":"unused","targetDiskModel":"unused","targetDiskSerial":null,"targetDiskSizeBytes":1},"partitionPlan":{"table":"mbr","partitions":[{"role":"system","sizeMib":550,"fileSystem":"ntfs","label":"System Reserved"},{"role":"windows","sizeMib":null,"fileSystem":"ntfs","label":"Windows"}]}}"#,
        ),
        ("503 Service Unavailable", ""),
        ("204 No Content", ""),
        (
            "200 OK",
            r#"{"acceptedAt":"2026-08-16T00:00:02Z","nextHeartbeatSeconds":2}"#,
        ),
        ("200 OK", "null"),
    ]);
    let mut child = spawn_agent(&endpoint, false);

    let captured = requests
        .recv_timeout(TEST_TIMEOUT)
        .expect("Agent should report the failed job and continue polling");
    let status = child
        .try_wait()
        .expect("Agent status should be readable after reporting the job");
    stop.store(true, Ordering::Relaxed);
    if status.is_none() {
        child
            .kill()
            .expect("running Agent should stop after the test");
    }
    let output = child
        .wait_with_output()
        .expect("Agent output should be collected");
    server.join().expect("test server should stop");
    let request_lines = request_lines(&captured);

    assert!(
        status.is_none(),
        "Agent should keep running after its failed-job report is accepted.\nstatus: {:?}\nrequests: {request_lines:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        request_lines,
        [
            "POST /api/v1/agents/register HTTP/1.1",
            "POST /api/v1/agents/00000000-0000-0000-0000-000000000001/heartbeat HTTP/1.1",
            "POST /api/v1/agents/00000000-0000-0000-0000-000000000001/jobs/claim HTTP/1.1",
            "POST /api/v1/agents/00000000-0000-0000-0000-000000000001/jobs/00000000-0000-0000-0000-000000000010/complete HTTP/1.1",
            "POST /api/v1/agents/00000000-0000-0000-0000-000000000001/jobs/00000000-0000-0000-0000-000000000010/complete HTTP/1.1",
            "POST /api/v1/agents/00000000-0000-0000-0000-000000000001/heartbeat HTTP/1.1",
            "POST /api/v1/agents/00000000-0000-0000-0000-000000000001/jobs/claim HTTP/1.1",
        ]
    );
    assert!(
        captured[3].contains(r#""succeeded":false"#)
            && captured[3].contains("GHO deployment metadata is missing")
            && captured[4].contains(r#""succeeded":false"#),
        "Both completion attempts should report the same deployment error.\nfirst request: {}\nsecond request: {}",
        captured[3],
        captured[4]
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EASYDEPLOYMESH_DIAG_V1|job.completion|reported_failure"),
        "Agent should emit a stable failed-completion marker.\nstdout: {stdout}"
    );
    assert!(
        stderr.contains("Deployment completion report failed (recovery attempt 1)")
            && stderr.contains("retrying completion report in 1 second(s)")
            && stderr.contains("failed and was reported to EasyDeployMesh")
            && stderr.contains("continuing agent loop"),
        "Agent should log that the reported failure is non-fatal.\nstderr: {stderr}"
    );
}

fn start_server(responses: Vec<(&'static str, &'static str)>) -> TestServer {
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
    let (requests_tx, requests) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = Arc::clone(&stop);
    let thread = thread::spawn(move || {
        let deadline = Instant::now() + TEST_TIMEOUT;
        let mut requests = Vec::new();
        for (status, body) in responses {
            let Some(mut stream) = accept_before(&listener, deadline, &server_stop) else {
                break;
            };
            let Some(request) = read_request_before(&mut stream, deadline, &server_stop) else {
                break;
            };
            requests.push(request);
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("test response should write");
        }
        let _ = requests_tx.send(requests);
    });
    TestServer {
        endpoint,
        stop,
        requests,
        thread,
    }
}

fn spawn_agent(endpoint: &str, once: bool) -> std::process::Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_easydeploymesh-agent"));
    command.args([
        "--server",
        endpoint,
        "--enrollment-token",
        "easydeploymesh_enroll_test",
    ]);
    if once {
        command.arg("--once");
    }
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Agent should start")
}

fn request_lines(requests: &[String]) -> Vec<&str> {
    requests
        .iter()
        .map(|request| request.lines().next().unwrap_or_default())
        .collect()
}

fn accept_before(
    listener: &TcpListener,
    deadline: Instant,
    stop: &AtomicBool,
) -> Option<TcpStream> {
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Some(stream),
            Err(error)
                if error.kind() == ErrorKind::WouldBlock
                    && Instant::now() < deadline
                    && !stop.load(Ordering::Relaxed) =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => return None,
            Err(error) => panic!("test listener failed: {error}"),
        }
    }
}

fn read_request_before(
    stream: &mut TcpStream,
    deadline: Instant,
    stop: &AtomicBool,
) -> Option<String> {
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("test stream should have a read timeout");
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return None,
            Ok(read) => {
                request.extend_from_slice(&buffer[..read]);
                if complete_http_request(&request) {
                    return Some(String::from_utf8_lossy(&request).into_owned());
                }
            }
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
                    && Instant::now() < deadline
                    && !stop.load(Ordering::Relaxed) =>
            {
                continue;
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return None;
            }
            Err(error) => panic!("test request failed: {error}"),
        }
    }
}

fn complete_http_request(request: &[u8]) -> bool {
    let Some(headers_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&request[..headers_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
        })
        .unwrap_or(0);
    request.len() >= headers_end + 4 + content_length
}

fn wait_before(child: &mut std::process::Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait().expect("Agent status should be readable") {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}
