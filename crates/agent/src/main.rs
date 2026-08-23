use clap::Parser;
use easydeploymesh_core::{
    AgentHeartbeat, AgentHeartbeatAck, AgentInventory, AgentJobCompletion, AgentJobLease,
    AgentJobProgress, AgentRegistration, Architecture, BootMode, DeploymentStage, Disk, JobState,
    SystemDetails,
};
use reqwest::blocking::Client;
use serde::Deserialize;
#[cfg(not(target_os = "windows"))]
use std::path::Path;
#[cfg(target_os = "windows")]
use std::process::Command;
use std::{
    error::Error,
    fs,
    net::{Ipv4Addr, SocketAddrV4, UdpSocket},
    path::PathBuf,
    process::ExitCode,
    thread,
    time::Duration,
};
#[cfg(not(target_os = "windows"))]
use sysinfo::{Disks, System};

mod deployment;
#[cfg(any(target_os = "windows", test))]
mod windows_inventory;
#[cfg(target_os = "windows")]
mod winpe_progress;

#[derive(Debug, Parser)]
#[command(
    name = "easydeploymesh-agent",
    version,
    about = "EasyDeployMesh WinPE and Windows deployment agent"
)]
struct Cli {
    /// EasyDeployMesh control service URL, for example http://192.168.1.10:7760.
    #[arg(long, env = "EASYDEPLOYMESH_SERVER")]
    server: Option<String>,

    /// Ephemeral enrollment token displayed by the EasyDeployMesh desktop app.
    #[arg(long, env = "EASYDEPLOYMESH_ENROLLMENT_TOKEN", hide_env_values = true)]
    enrollment_token: Option<String>,

    /// Read the server URL and enrollment token from a WinPE bootstrap file.
    #[arg(long, env = "EASYDEPLOYMESH_BOOTSTRAP")]
    bootstrap: Option<PathBuf>,

    /// Register, send one heartbeat, and exit. Useful for diagnostics.
    #[arg(long)]
    once: bool,

    /// Read the configured bootstrap and probe the control service without registering.
    #[arg(long, conflicts_with_all = ["once", "heartbeat_interval"])]
    health_check: bool,

    /// Override the server-provided heartbeat interval in seconds.
    #[arg(long, value_parser = clap::value_parser!(u64).range(2..=300))]
    heartbeat_interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapConfig {
    server: String,
    enrollment_token: String,
}

#[derive(Debug, Deserialize)]
struct ControlHealth {
    status: String,
    #[serde(rename = "version")]
    _version: String,
}

const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

struct RetryBackoff {
    recovery_attempt: u64,
    next_delay: Duration,
}

struct HeartbeatOutcome {
    acknowledgment: AgentHeartbeatAck,
    inventory_has_disks: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Ipv4RouteProbe {
    Routed(Ipv4Addr),
    BindFailed,
    NoRoute,
    Indeterminate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NetworkDiagnostics {
    ipv4: &'static str,
    route_to_control: &'static str,
}

impl RetryBackoff {
    fn new() -> Self {
        Self {
            recovery_attempt: 0,
            next_delay: INITIAL_RETRY_DELAY,
        }
    }

    fn reset(&mut self) {
        self.recovery_attempt = 0;
        self.next_delay = INITIAL_RETRY_DELAY;
    }

    fn wait(&mut self, operation: &str, retry_action: &str, error: &dyn Error) {
        self.recovery_attempt = self.recovery_attempt.saturating_add(1);
        let delay = self.next_delay;
        eprintln!(
            "{operation} failed (recovery attempt {}): {error}; retrying {retry_action} in {} second(s), bounded at {} second(s)",
            self.recovery_attempt,
            delay.as_secs(),
            MAX_RETRY_DELAY.as_secs()
        );
        thread::sleep(delay);
        self.next_delay = delay.saturating_mul(2).min(MAX_RETRY_DELAY);
    }
}

fn main() -> ExitCode {
    let result = if is_shell_launcher() {
        run_shell_launcher()
    } else {
        run(Cli::parse())
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("easydeploymesh-agent: {error}");
            ExitCode::FAILURE
        }
    }
}

fn is_shell_launcher() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.file_stem().map(|value| value.to_owned()))
        .is_some_and(|name| name.eq_ignore_ascii_case("easydeploymesh-shell"))
}

#[cfg(target_os = "windows")]
fn run_shell_launcher() -> Result<(), Box<dyn Error>> {
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    hide_shell_console();
    let directory = std::env::current_exe()?
        .parent()
        .ok_or("EasyDeployMesh shell launcher has no parent directory")?
        .to_path_buf();
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("easydeploymesh-agent.log"))?;
    let original_shell = directory.join("easydeploymesh-original-shell.cmd");
    let has_original_shell = original_shell.is_file();
    let network_stdout = log.try_clone()?;
    let network_stderr = log.try_clone()?;
    let agent_stdout = log.try_clone()?;
    let agent_stderr = log.try_clone()?;
    let mut failure_log = log;
    let agent_directory = directory.clone();

    run_shell_startup_sequence(
        has_original_shell,
        move || {
            let status = Command::new("wpeinit.exe")
                .stdout(Stdio::from(network_stdout))
                .stderr(Stdio::from(network_stderr))
                .status()?;
            if !status.success() {
                return Err(format!("wpeinit exited with {status}").into());
            }
            Ok(())
        },
        move |error| {
            let _ = writeln!(
                failure_log,
                "WinPE network initialization failed: {error}; continuing with EasyDeployMesh Agent retries and the original WinPE shell"
            );
        },
        move || {
            Command::new(agent_directory.join("easydeploymesh-agent.exe"))
                .arg("--bootstrap")
                .arg(agent_directory.join("easydeploymesh-bootstrap.json"))
                .stdout(Stdio::from(agent_stdout))
                .stderr(Stdio::from(agent_stderr))
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()?;
            Ok(())
        },
        move || {
            let status = Command::new("cmd.exe")
                .arg("/d")
                .arg("/c")
                .arg(&original_shell)
                .status()?;
            if !status.success() {
                return Err(format!("original WinPE shell exited with {status}").into());
            }
            Ok(())
        },
    )
}

#[cfg(target_os = "windows")]
fn hide_shell_console() {
    use windows::Win32::{
        System::Console::GetConsoleWindow,
        UI::WindowsAndMessaging::{SW_HIDE, ShowWindow},
    };

    let window = unsafe { GetConsoleWindow() };
    if !window.is_invalid() {
        unsafe {
            let _ = ShowWindow(window, SW_HIDE);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn run_shell_launcher() -> Result<(), Box<dyn Error>> {
    Err("EasyDeployMesh shell launcher is only supported on Windows".into())
}

#[cfg(any(target_os = "windows", test))]
fn run_shell_startup_sequence<E, Initialize, LogFailure, StartAgent, RunOriginalShell>(
    has_original_shell: bool,
    initialize_network: Initialize,
    log_initialization_failure: LogFailure,
    start_agent: StartAgent,
    run_original_shell: RunOriginalShell,
) -> Result<(), E>
where
    Initialize: FnOnce() -> Result<(), E>,
    LogFailure: FnOnce(&E),
    StartAgent: FnOnce() -> Result<(), E>,
    RunOriginalShell: FnOnce() -> Result<(), E>,
{
    if let Err(error) = initialize_network() {
        log_initialization_failure(&error);
    }
    start_agent()?;
    if has_original_shell {
        run_original_shell()?;
    }
    Ok(())
}

fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    let bootstrap = match connection_settings(&cli) {
        Ok(bootstrap) => bootstrap,
        Err(error) if cli.health_check => {
            print_network_diagnostics(NetworkDiagnostics {
                ipv4: "unknown",
                route_to_control: "unknown",
            });
            println!("EASYDEPLOYMESH_DIAG_V1|control.health|not_run|reason=bootstrap_error");
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    let server = match validated_server(&bootstrap.server) {
        Ok(server) => server,
        Err(error) if cli.health_check => {
            print_network_diagnostics(NetworkDiagnostics {
                ipv4: "unknown",
                route_to_control: "unknown",
            });
            println!("EASYDEPLOYMESH_DIAG_V1|control.health|not_run|reason=server_invalid");
            return Err(error);
        }
        Err(error) => return Err(error),
    };

    let client = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .user_agent(format!(
            "easydeploymesh-agent/{}",
            env!("CARGO_PKG_VERSION")
        ))
        .build()?;
    if cli.health_check {
        print_network_diagnostics(network_diagnostics_with(&server, probe_ipv4_route));
        return check_control_health(&client, &server);
    }
    let mut backoff = RetryBackoff::new();
    loop {
        let registration = match register_agent(&client, &server, &bootstrap.enrollment_token) {
            Ok(registration) => registration,
            Err(error) => {
                backoff.wait("Agent registration", "registration", error.as_ref());
                continue;
            }
        };
        println!(
            "Registered device {} with EasyDeployMesh at {server}",
            registration.device_id
        );
        backoff.reset();

        let mut heartbeat_interval = cli
            .heartbeat_interval
            .unwrap_or(registration.heartbeat_interval_seconds)
            .clamp(2, 300);
        loop {
            let heartbeat = match send_heartbeat(&client, &server, &registration) {
                Ok(heartbeat) => heartbeat,
                Err(error) => {
                    backoff.wait("Agent heartbeat", "registration", error.as_ref());
                    break;
                }
            };
            println!(
                "Heartbeat accepted at {}",
                heartbeat.acknowledgment.accepted_at
            );
            backoff.reset();
            if cli.once {
                return Ok(());
            }

            if !heartbeat.inventory_has_disks {
                eprintln!(
                    "Skipping deployment job claim because the latest heartbeat reported no disks"
                );
            }
            let lease = match claim_job_if_disk_available(heartbeat.inventory_has_disks, || {
                claim_job(&client, &server, &registration)
            }) {
                Ok(lease) => lease,
                Err(error) => {
                    backoff.wait("Deployment job claim", "registration", error.as_ref());
                    break;
                }
            };
            backoff.reset();
            if let Some(lease) = lease {
                execute_lease(&server, &registration, &client, &lease)?;
            }

            if cli.heartbeat_interval.is_none() {
                heartbeat_interval = heartbeat
                    .acknowledgment
                    .next_heartbeat_seconds
                    .clamp(2, 300);
            }
            thread::sleep(Duration::from_secs(heartbeat_interval));
        }
    }
}

fn network_diagnostics_with<Probe>(server: &str, probe: Probe) -> NetworkDiagnostics
where
    Probe: FnOnce(SocketAddrV4) -> Ipv4RouteProbe,
{
    let Some(target) = reqwest::Url::parse(server).ok().and_then(|url| {
        let address = url.host_str()?.parse::<Ipv4Addr>().ok()?;
        Some(SocketAddrV4::new(address, url.port_or_known_default()?))
    }) else {
        return NetworkDiagnostics {
            ipv4: "unknown",
            route_to_control: "unknown",
        };
    };

    match probe(target) {
        Ipv4RouteProbe::Routed(address) if !address.is_unspecified() => NetworkDiagnostics {
            ipv4: "usable",
            route_to_control: "present",
        },
        Ipv4RouteProbe::NoRoute => NetworkDiagnostics {
            ipv4: "unknown",
            route_to_control: "absent",
        },
        Ipv4RouteProbe::BindFailed => NetworkDiagnostics {
            ipv4: "unusable",
            route_to_control: "unknown",
        },
        Ipv4RouteProbe::Routed(_) | Ipv4RouteProbe::Indeterminate => NetworkDiagnostics {
            ipv4: "unknown",
            route_to_control: "unknown",
        },
    }
}

fn probe_ipv4_route(target: SocketAddrV4) -> Ipv4RouteProbe {
    let socket = match UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)) {
        Ok(socket) => socket,
        Err(_) => return Ipv4RouteProbe::BindFailed,
    };
    if let Err(error) = socket.connect(target) {
        return match error.kind() {
            std::io::ErrorKind::NetworkUnreachable | std::io::ErrorKind::HostUnreachable => {
                Ipv4RouteProbe::NoRoute
            }
            _ => Ipv4RouteProbe::Indeterminate,
        };
    }
    match socket.local_addr() {
        Ok(address) => match address.ip() {
            std::net::IpAddr::V4(address) => Ipv4RouteProbe::Routed(address),
            std::net::IpAddr::V6(_) => Ipv4RouteProbe::Indeterminate,
        },
        Err(_) => Ipv4RouteProbe::Indeterminate,
    }
}

fn print_network_diagnostics(diagnostics: NetworkDiagnostics) {
    println!("EASYDEPLOYMESH_DIAG_V1|network.ipv4|{}", diagnostics.ipv4);
    println!(
        "EASYDEPLOYMESH_DIAG_V1|network.route_to_control|{}",
        diagnostics.route_to_control
    );
}

fn check_control_health(client: &Client, server: &str) -> Result<(), Box<dyn Error>> {
    let response = match client
        .get(format!("{server}/health"))
        .timeout(Duration::from_secs(5))
        .send()
    {
        Ok(response) => response,
        Err(error) => {
            let category = if error.is_timeout() {
                "timeout"
            } else if error.is_connect() {
                "connect_error"
            } else {
                "request_error"
            };
            println!("EASYDEPLOYMESH_DIAG_V1|control.health|{category}");
            return Err("control-service health check request failed".into());
        }
    };
    let status = response.status();
    if !status.is_success() {
        println!(
            "EASYDEPLOYMESH_DIAG_V1|control.health|http_error|status={}",
            status.as_u16()
        );
        return Err(format!(
            "control-service health check returned HTTP {}",
            status.as_u16()
        )
        .into());
    }
    let health = match response.json::<ControlHealth>() {
        Ok(health) => health,
        Err(_) => {
            println!(
                "EASYDEPLOYMESH_DIAG_V1|control.health|invalid_response|status={}",
                status.as_u16()
            );
            return Err("control-service health response was invalid".into());
        }
    };
    if health.status != "ok" {
        println!(
            "EASYDEPLOYMESH_DIAG_V1|control.health|unhealthy|status={}",
            status.as_u16()
        );
        return Err("control service reported an unhealthy status".into());
    }
    println!(
        "EASYDEPLOYMESH_DIAG_V1|control.health|ok|status={}",
        status.as_u16()
    );
    Ok(())
}

fn validated_server(value: &str) -> Result<String, Box<dyn Error>> {
    let value = value.trim();
    let parsed = reqwest::Url::parse(value)
        .map_err(|error| format!("server must be a valid http:// or https:// URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("server must be a valid http:// or https:// URL".into());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("server URL must not contain a query string or fragment".into());
    }
    Ok(value.trim_end_matches('/').to_owned())
}

fn register_agent(
    client: &Client,
    server: &str,
    enrollment_token: &str,
) -> Result<AgentRegistration, Box<dyn Error>> {
    let inventory = collect_inventory()?;
    let registration = client
        .post(format!("{server}/api/v1/agents/register"))
        .timeout(Duration::from_secs(15))
        .bearer_auth(enrollment_token)
        .json(&inventory)
        .send()?
        .error_for_status()?
        .json::<AgentRegistration>()?;
    Ok(registration)
}

fn send_heartbeat(
    client: &Client,
    server: &str,
    registration: &AgentRegistration,
) -> Result<HeartbeatOutcome, Box<dyn Error>> {
    let inventory = collect_inventory()?;
    let inventory_has_disks = !inventory.disks.is_empty();
    let acknowledgment = client
        .post(format!(
            "{server}/api/v1/agents/{}/heartbeat",
            registration.device_id
        ))
        .timeout(Duration::from_secs(15))
        .bearer_auth(&registration.device_token)
        .json(&AgentHeartbeat { inventory })
        .send()?
        .error_for_status()?
        .json::<AgentHeartbeatAck>()?;
    Ok(HeartbeatOutcome {
        acknowledgment,
        inventory_has_disks,
    })
}

fn claim_job(
    client: &Client,
    server: &str,
    registration: &AgentRegistration,
) -> Result<Option<AgentJobLease>, Box<dyn Error>> {
    let lease = client
        .post(format!(
            "{server}/api/v1/agents/{}/jobs/claim",
            registration.device_id
        ))
        .timeout(Duration::from_secs(15))
        .bearer_auth(&registration.device_token)
        .send()?
        .error_for_status()?
        .json::<Option<AgentJobLease>>()?;
    Ok(lease)
}

fn claim_job_if_disk_available<Claim>(
    inventory_has_disks: bool,
    claim: Claim,
) -> Result<Option<AgentJobLease>, Box<dyn Error>>
where
    Claim: FnOnce() -> Result<Option<AgentJobLease>, Box<dyn Error>>,
{
    if !inventory_has_disks {
        return Ok(None);
    }
    claim()
}

fn is_transient_request_error(error: &reqwest::Error) -> bool {
    error
        .status()
        .is_none_or(|status| status.is_server_error() || matches!(status.as_u16(), 408 | 425 | 429))
}

fn connection_settings(cli: &Cli) -> Result<BootstrapConfig, Box<dyn Error>> {
    let automatic_bootstrap = [
        PathBuf::from(r"X:\easydeploymesh-bootstrap.json"),
        PathBuf::from(r"X:\Boot\easydeploymesh-bootstrap.json"),
        PathBuf::from(r"X:\Sources\easydeploymesh-bootstrap.json"),
        PathBuf::from(r"X:\Windows\System32\easydeploymesh-bootstrap.json"),
    ]
    .into_iter()
    .find(|path| path.is_file());
    if let Some(path) = cli.bootstrap.as_deref().or(automatic_bootstrap.as_deref()) {
        let bytes = fs::read(path)?;
        let config: BootstrapConfig = serde_json::from_slice(&bytes)?;
        if config.server.trim().is_empty() || config.enrollment_token.trim().is_empty() {
            return Err("bootstrap file is missing server or enrollmentToken".into());
        }
        return Ok(BootstrapConfig {
            server: config.server.trim().to_owned(),
            enrollment_token: config.enrollment_token.trim().to_owned(),
        });
    }
    Ok(BootstrapConfig {
        server: cli
            .server
            .clone()
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_owned())
            .ok_or("--server or --bootstrap is required")?,
        enrollment_token: cli
            .enrollment_token
            .clone()
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_owned())
            .ok_or("--enrollment-token or --bootstrap is required")?,
    })
}

fn execute_lease(
    server: &str,
    registration: &AgentRegistration,
    client: &Client,
    lease: &AgentJobLease,
) -> Result<(), Box<dyn Error>> {
    println!("Claimed deployment job {}", lease.job_id);
    #[cfg(target_os = "windows")]
    let progress_window = winpe_progress::ProgressWindow::open();
    let progress = |stage: DeploymentStage,
                    progress_percent: u8,
                    message: &str|
     -> Result<(), Box<dyn Error>> {
        #[cfg(target_os = "windows")]
        progress_window.update(progress_percent, message);
        client
            .post(format!(
                "{server}/api/v1/agents/{}/jobs/{}/progress",
                registration.device_id, lease.job_id
            ))
            .timeout(Duration::from_secs(15))
            .bearer_auth(&registration.device_token)
            .json(&AgentJobProgress {
                lease_id: lease.lease_id,
                stage,
                progress_percent,
                message: Some(message.to_owned()),
            })
            .send()?
            .error_for_status()?;
        Ok(())
    };
    let control_state = || -> Result<JobState, Box<dyn Error>> {
        Ok(client
            .get(format!(
                "{server}/api/v1/agents/{}/jobs/{}/control?leaseId={}",
                registration.device_id, lease.job_id, lease.lease_id
            ))
            .timeout(Duration::from_secs(15))
            .bearer_auth(&registration.device_token)
            .send()?
            .error_for_status()?
            .json()?)
    };
    let result = collect_inventory().and_then(|inventory| {
        deployment::execute(
            lease,
            &inventory.disks,
            server,
            &registration.device_token,
            client,
            progress,
            control_state,
        )
    });
    let completion = AgentJobCompletion {
        lease_id: lease.lease_id,
        succeeded: result.is_ok(),
        error_message: result.as_ref().err().map(ToString::to_string),
    };
    let mut completion_backoff = RetryBackoff::new();
    loop {
        let response = client
            .post(format!(
                "{server}/api/v1/agents/{}/jobs/{}/complete",
                registration.device_id, lease.job_id
            ))
            .timeout(Duration::from_secs(15))
            .bearer_auth(&registration.device_token)
            .json(&completion)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status);
        match response {
            Ok(_) => break,
            Err(error) if is_transient_request_error(&error) => {
                completion_backoff.wait("Deployment completion report", "completion report", &error)
            }
            Err(error) => {
                return Err(format!(
                    "deployment job {} finished locally, but completion could not be confirmed; stopping before another job can be claimed: {error}",
                    lease.job_id
                )
                .into());
            }
        }
    }
    println!("{}", job_completion_marker(result.is_ok()));
    if let Err(error) = result {
        #[cfg(target_os = "windows")]
        progress_window.failed(&error.to_string());
        eprintln!(
            "Deployment job {} failed and was reported to EasyDeployMesh: {error}; continuing agent loop",
            lease.job_id
        );
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    progress_window.update(100, "Deployment complete; rebooting");
    #[cfg(target_os = "windows")]
    {
        let status = Command::new("wpeutil.exe").arg("reboot").status()?;
        if !status.success() {
            return Err(format!("wpeutil reboot failed: {status}").into());
        }
    }
    Ok(())
}

fn job_completion_marker(succeeded: bool) -> &'static str {
    if succeeded {
        "EASYDEPLOYMESH_DIAG_V1|job.completion|reported_success"
    } else {
        "EASYDEPLOYMESH_DIAG_V1|job.completion|reported_failure"
    }
}

fn collect_inventory() -> Result<AgentInventory, Box<dyn Error>> {
    let hostname = hostname::get()
        .ok()
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.trim().is_empty());
    let mac_address = mac_address::get_mac_address()?
        .map(|address| address.to_string())
        .or_else(platform_mac_address)
        .ok_or("no usable MAC address was found")?;
    let disks = collect_disks_or_empty(collect_disks);
    let hardware = collect_hardware_summary();

    Ok(AgentInventory {
        hostname,
        mac_address,
        model: hardware.model,
        serial: hardware.serial,
        cpu_model: hardware.cpu_model,
        physical_core_count: hardware.physical_core_count,
        logical_processor_count: hardware.logical_processor_count,
        memory_bytes: hardware.memory_bytes,
        system_details: hardware.system_details,
        architecture: current_architecture(),
        boot_mode: detect_boot_mode(),
        disks,
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}

#[cfg(target_os = "windows")]
fn platform_mac_address() -> Option<String> {
    windows_inventory::collect_mac_address().ok().flatten()
}

#[cfg(not(target_os = "windows"))]
fn platform_mac_address() -> Option<String> {
    None
}

struct HardwareSummary {
    model: Option<String>,
    serial: Option<String>,
    cpu_model: Option<String>,
    physical_core_count: Option<u32>,
    logical_processor_count: u32,
    memory_bytes: u64,
    system_details: SystemDetails,
}

#[cfg(not(target_os = "windows"))]
fn collect_hardware_summary() -> HardwareSummary {
    let system = System::new_all();
    let cpu_model = system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().trim().to_owned())
        .filter(|brand| !brand.is_empty());
    let physical_core_count =
        System::physical_core_count().and_then(|count| u32::try_from(count).ok());
    let logical_processor_count = u32::try_from(system.cpus().len()).unwrap_or(u32::MAX);
    HardwareSummary {
        model: None,
        serial: None,
        cpu_model,
        physical_core_count,
        logical_processor_count,
        memory_bytes: system.total_memory(),
        system_details: SystemDetails::default(),
    }
}

#[cfg(target_os = "windows")]
fn collect_hardware_summary() -> HardwareSummary {
    match windows_inventory::collect_hardware_summary() {
        Ok(summary) => {
            return HardwareSummary {
                model: summary.model,
                serial: summary.serial,
                cpu_model: summary.cpu_model,
                physical_core_count: summary.physical_core_count,
                logical_processor_count: summary.logical_processor_count,
                memory_bytes: summary.memory_bytes,
                system_details: summary.system_details,
            };
        }
        Err(error) => eprintln!(
            "Extended Windows hardware inventory probe failed: {error}; continuing with a minimal hardware summary"
        ),
    }
    let logical_processor_count = std::thread::available_parallelism()
        .ok()
        .and_then(|count| u32::try_from(count.get()).ok())
        .unwrap_or_default();
    HardwareSummary {
        model: None,
        serial: None,
        cpu_model: None,
        physical_core_count: None,
        logical_processor_count,
        memory_bytes: 0,
        system_details: SystemDetails::default(),
    }
}

fn collect_disks_or_empty<Collect, ProbeError>(collect: Collect) -> Vec<Disk>
where
    Collect: FnOnce() -> Result<Vec<Disk>, ProbeError>,
    ProbeError: std::fmt::Display,
{
    match collect() {
        Ok(disks) => disks,
        Err(error) => {
            eprintln!(
                "Disk inventory probe failed: {error}; continuing registration and heartbeats with an empty disk list"
            );
            Vec::new()
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn collect_disks() -> Result<Vec<Disk>, Box<dyn Error>> {
    Ok(Disks::new_with_refreshed_list()
        .list()
        .iter()
        .enumerate()
        .map(|(index, disk)| Disk {
            id: format!(
                "{}:{}",
                index,
                disk.mount_point().to_string_lossy().replace('\\', "/")
            ),
            model: disk.name().to_string_lossy().into_owned(),
            serial: None,
            size_bytes: disk.total_space(),
            is_system: is_system_mount(disk.mount_point()),
        })
        .collect())
}

#[cfg(target_os = "windows")]
fn collect_disks() -> Result<Vec<Disk>, Box<dyn Error>> {
    windows_inventory::collect_disks()
}

fn current_architecture() -> Architecture {
    match std::env::consts::ARCH {
        "x86_64" => Architecture::X86_64,
        "aarch64" => Architecture::Aarch64,
        _ => Architecture::Unknown,
    }
}

fn detect_boot_mode() -> BootMode {
    detect_boot_mode_with(pe_firmware_type, || std::env::var("FIRMWARE_TYPE").ok())
}

fn detect_boot_mode_with<P, E>(pe_firmware_type: P, firmware_type_env: E) -> BootMode
where
    P: FnOnce() -> Option<u32>,
    E: FnOnce() -> Option<String>,
{
    match pe_firmware_type() {
        Some(1) => BootMode::LegacyBios,
        Some(2) => BootMode::Uefi,
        _ => firmware_type_env()
            .map(|value| value.to_ascii_lowercase())
            .map_or(BootMode::Unknown, |value| match value.as_str() {
                "uefi" => BootMode::Uefi,
                "legacy" | "bios" => BootMode::LegacyBios,
                _ => BootMode::Unknown,
            }),
    }
}

#[cfg(target_os = "windows")]
fn pe_firmware_type() -> Option<u32> {
    use windows::Win32::{
        Foundation::ERROR_SUCCESS,
        System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW},
    };
    use windows::core::w;

    let _ = Command::new("wpeutil.exe").arg("UpdateBootInfo").status();

    let mut value = 0_u32;
    let mut value_size = std::mem::size_of::<u32>() as u32;
    // SAFETY: Both registry strings are static and nul-terminated. The data
    // pointer refers to a writable DWORD whose size is supplied to Windows.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SYSTEM\CurrentControlSet\Control"),
            w!("PEFirmwareType"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut value_size),
        )
    };

    (status == ERROR_SUCCESS && value_size == std::mem::size_of::<u32>() as u32).then_some(value)
}

#[cfg(not(target_os = "windows"))]
fn pe_firmware_type() -> Option<u32> {
    None
}

#[cfg(not(target_os = "windows"))]
fn is_system_mount(path: &Path) -> bool {
    path == Path::new("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::cell::RefCell;

    #[test]
    fn parses_diagnostic_mode() {
        let cli = Cli::try_parse_from([
            "easydeploymesh-agent",
            "--server",
            "http://192.168.1.10:7760",
            "--enrollment-token",
            "easydeploymesh_enroll_test",
            "--once",
        ])
        .expect("arguments should parse");

        assert!(cli.once);
        assert_eq!(cli.server.as_deref(), Some("http://192.168.1.10:7760"));
    }

    #[test]
    fn architecture_is_known_on_supported_build_hosts() {
        assert_ne!(current_architecture(), Architecture::Unknown);
    }

    #[test]
    fn network_diagnostics_do_not_infer_ipv4_usability_when_route_is_absent() {
        let diagnostics =
            network_diagnostics_with("http://192.0.2.10:7760", |_| Ipv4RouteProbe::NoRoute);

        assert_eq!(
            diagnostics,
            NetworkDiagnostics {
                ipv4: "unknown",
                route_to_control: "absent",
            }
        );
    }

    #[test]
    fn network_diagnostics_report_ipv4_unusable_when_socket_binding_fails() {
        let diagnostics =
            network_diagnostics_with("http://192.0.2.10:7760", |_| Ipv4RouteProbe::BindFailed);

        assert_eq!(
            diagnostics,
            NetworkDiagnostics {
                ipv4: "unusable",
                route_to_control: "unknown",
            }
        );
    }

    #[test]
    fn network_diagnostics_probe_the_control_ipv4_host_and_port() {
        let diagnostics = network_diagnostics_with("https://192.0.2.10:8443", |target| {
            assert_eq!(target, "192.0.2.10:8443".parse().expect("valid fixture"));
            Ipv4RouteProbe::Routed(Ipv4Addr::new(192, 0, 2, 20))
        });

        assert_eq!(
            diagnostics,
            NetworkDiagnostics {
                ipv4: "usable",
                route_to_control: "present",
            }
        );
    }

    #[test]
    fn network_diagnostics_degrade_dns_targets_to_unknown_without_probing_ipv4() {
        let diagnostics = network_diagnostics_with("https://control.example.test:8443", |_| {
            panic!("DNS targets must not be passed to the IPv4 route probe")
        });

        assert_eq!(
            diagnostics,
            NetworkDiagnostics {
                ipv4: "unknown",
                route_to_control: "unknown",
            }
        );
    }

    #[test]
    fn network_diagnostics_degrade_ipv6_targets_to_unknown_without_probing_ipv4() {
        let diagnostics = network_diagnostics_with("https://[2001:db8::10]:8443", |_| {
            panic!("IPv6 targets must not be passed to the IPv4 route probe")
        });

        assert_eq!(
            diagnostics,
            NetworkDiagnostics {
                ipv4: "unknown",
                route_to_control: "unknown",
            }
        );
    }

    #[test]
    fn winpe_registry_identifies_uefi_without_environment_hint() {
        let boot_mode = detect_boot_mode_with(|| Some(2), || None);

        assert_eq!(boot_mode, BootMode::Uefi);
    }

    #[test]
    fn winpe_registry_identifies_legacy_bios_without_environment_hint() {
        let boot_mode = detect_boot_mode_with(|| Some(1), || None);

        assert_eq!(boot_mode, BootMode::LegacyBios);
    }

    #[test]
    fn winpe_registry_value_takes_precedence_over_environment_hint() {
        let boot_mode = detect_boot_mode_with(|| Some(2), || Some("legacy".to_owned()));

        assert_eq!(boot_mode, BootMode::Uefi);
    }

    #[test]
    fn environment_hint_is_used_when_winpe_registry_value_is_unavailable() {
        let boot_mode = detect_boot_mode_with(|| None, || Some("BIOS".to_owned()));

        assert_eq!(boot_mode, BootMode::LegacyBios);
    }

    #[test]
    fn environment_hint_is_used_when_winpe_registry_value_is_unknown() {
        let boot_mode = detect_boot_mode_with(|| Some(0), || Some("UEFI".to_owned()));

        assert_eq!(boot_mode, BootMode::Uefi);
    }

    #[test]
    fn disk_probe_failure_degrades_to_an_empty_inventory() {
        let disks = collect_disks_or_empty(|| {
            Err::<Vec<Disk>, Box<dyn Error>>("disk probe unavailable".into())
        });

        assert!(disks.is_empty());
    }

    #[test]
    fn zero_disk_heartbeat_does_not_invoke_job_claim() {
        let lease = claim_job_if_disk_available(false, || {
            panic!("job claim must not be invoked without a disk")
        })
        .expect("skipping a claim should succeed");

        assert!(lease.is_none());
    }

    #[test]
    fn job_completion_markers_are_stable_and_secret_free() {
        assert_eq!(
            job_completion_marker(true),
            "EASYDEPLOYMESH_DIAG_V1|job.completion|reported_success"
        );
        assert_eq!(
            job_completion_marker(false),
            "EASYDEPLOYMESH_DIAG_V1|job.completion|reported_failure"
        );
    }

    #[test]
    fn shell_launcher_initializes_network_before_agent_and_vendor_shell() {
        let steps = RefCell::new(Vec::new());

        run_shell_startup_sequence(
            true,
            || {
                steps.borrow_mut().push("initialize network");
                Ok::<(), &'static str>(())
            },
            |_| steps.borrow_mut().push("log initialization failure"),
            || {
                steps.borrow_mut().push("start agent");
                Ok(())
            },
            || {
                steps.borrow_mut().push("run vendor shell");
                Ok(())
            },
        )
        .expect("shell startup should succeed");

        assert_eq!(
            steps.into_inner(),
            ["initialize network", "start agent", "run vendor shell"]
        );
    }

    #[test]
    fn shell_launcher_continues_after_network_initialization_failure() {
        let steps = RefCell::new(Vec::new());

        run_shell_startup_sequence(
            true,
            || {
                steps.borrow_mut().push("initialize network");
                Err::<(), &'static str>("wpeinit failed")
            },
            |error| {
                assert_eq!(*error, "wpeinit failed");
                steps.borrow_mut().push("log initialization failure");
            },
            || {
                steps.borrow_mut().push("start agent");
                Ok(())
            },
            || {
                steps.borrow_mut().push("run vendor shell");
                Ok(())
            },
        )
        .expect("shell startup should continue after wpeinit fails");

        assert_eq!(
            steps.into_inner(),
            [
                "initialize network",
                "log initialization failure",
                "start agent",
                "run vendor shell"
            ]
        );
    }
}
