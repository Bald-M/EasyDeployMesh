use easydeploymesh_core::{
    ActivityEvent, ActivitySeverity, ActivitySource, ActivitySubject, ControlPlaneStatus,
    CreateDeploymentJob, DeploymentJob, ImageArtifact, ImageFormat, JobState, Operation, PxeConfig,
    PxeDiscoveredClient, PxeServiceStatus, RegisteredDevice, RuntimeStatus,
};
use easydeploymesh_service::{
    ActivityQuery, ActivityRepository, BootPackage, ControlPlane, DeviceRegistry, ImageLibrary,
    JobRepository, NetworkInterfaceSummary, PxeService, WimlibCapability,
};
use serde::Deserialize;
use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tauri::{Manager, State};
use uuid::Uuid;

#[tauri::command]
async fn runtime_status(state: State<'_, AppState>) -> Result<RuntimeStatus, String> {
    let mut status = easydeploymesh_service::runtime_status();
    let control_status = state.control_plane.status().await;
    status.service_state = control_status.state;
    status.active_interface = control_status.bind_address;
    status.connected_devices = state
        .devices
        .connected_count()
        .map_err(|error| error.to_string())?;
    status.queued_jobs = state
        .jobs
        .queued_count()
        .map_err(|error| error.to_string())?;
    Ok(status)
}

#[tauri::command]
async fn control_plane_status(state: State<'_, AppState>) -> Result<ControlPlaneStatus, String> {
    Ok(state.control_plane.status().await)
}

#[tauri::command]
async fn start_control_plane(
    bind_address: String,
    port: Option<u16>,
    state: State<'_, AppState>,
) -> Result<ControlPlaneStatus, String> {
    start_control_plane_inner(&bind_address, port.unwrap_or(7760), &state).await
}

async fn start_control_plane_inner(
    bind_address: &str,
    port: u16,
    state: &AppState,
) -> Result<ControlPlaneStatus, String> {
    let result = state.control_plane.start(bind_address, port).await;
    let status = match result {
        Ok(status) => status,
        Err(error) => {
            record_activity(
                state,
                ActivitySource::Service,
                "control_service_failed",
                ActivitySeverity::Error,
                Some(service_subject("control")),
                serde_json::Map::new(),
                Some(error.to_string()),
            );
            return Err(error.to_string());
        }
    };
    sync_agent_bootstrap(&status, state).await?;
    let mut details = serde_json::Map::new();
    if let Some(endpoint) = &status.endpoint {
        details.insert("endpoint".into(), endpoint.clone().into());
    }
    record_activity(
        state,
        ActivitySource::Service,
        "control_service_started",
        ActivitySeverity::Success,
        Some(service_subject("control")),
        details,
        None,
    );
    Ok(status)
}

async fn sync_agent_bootstrap(status: &ControlPlaneStatus, state: &AppState) -> Result<(), String> {
    let endpoint = status
        .endpoint
        .as_deref()
        .ok_or("control service did not provide an endpoint")?;
    let enrollment_token = status
        .enrollment_token
        .as_deref()
        .ok_or("control service did not provide an enrollment token")?;
    let bootstrap_path = state
        .pxe_boot_root
        .join("boot/easydeploymesh-bootstrap.json");
    fs::create_dir_all(
        bootstrap_path
            .parent()
            .ok_or("PXE bootstrap path has no parent")?,
    )
    .map_err(|error| error.to_string())?;
    let bootstrap = serde_json::to_vec_pretty(&serde_json::json!({
        "server": endpoint,
        "enrollmentToken": enrollment_token,
    }))
    .map_err(|error| error.to_string())?;
    fs::write(&bootstrap_path, &bootstrap).map_err(|error| error.to_string())?;
    if state.pxe_boot_root.join("boot/boot.wim").is_file() {
        let boot_wim = state.pxe_boot_root.join("boot/boot.wim");
        let runtime_wim = boot_wim.clone();
        let agent = state.agent_binary_path.clone();
        tauri::async_runtime::spawn_blocking(move || {
            BootPackage::ensure_agent_runtime(runtime_wim, agent)
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("could not refresh the Agent inside WinPE: {error}"))?;
        tauri::async_runtime::spawn_blocking(move || {
            BootPackage::inject_agent_bootstrap(boot_wim, &bootstrap)
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("could not place the Agent bootstrap inside WinPE: {error}"))?;
    }
    Ok(())
}

#[tauri::command]
async fn stop_control_plane(state: State<'_, AppState>) -> Result<ControlPlaneStatus, String> {
    let status = state
        .control_plane
        .stop()
        .await
        .map_err(|error| error.to_string())?;
    let bootstrap_path = state
        .pxe_boot_root
        .join("boot/easydeploymesh-bootstrap.json");
    if bootstrap_path.exists() {
        fs::remove_file(bootstrap_path).map_err(|error| error.to_string())?;
    }
    record_activity(
        &state,
        ActivitySource::Service,
        "control_service_stopped",
        ActivitySeverity::Info,
        Some(service_subject("control")),
        serde_json::Map::new(),
        None,
    );
    Ok(status)
}

#[tauri::command]
fn list_devices(state: State<'_, AppState>) -> Result<Vec<RegisteredDevice>, String> {
    state.devices.list().map_err(|error| error.to_string())
}

#[tauri::command]
async fn refresh_devices(state: State<'_, AppState>) -> Result<Vec<RegisteredDevice>, String> {
    use easydeploymesh_service::DEVICE_VERIFICATION_WINDOW;

    let now = chrono::Utc::now();
    let wait = state
        .devices
        .list()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|entry| entry.online)
        .filter_map(|entry| {
            let verification_deadline = entry.device.last_seen_at
                + chrono::Duration::from_std(DEVICE_VERIFICATION_WINDOW).ok()?;
            verification_deadline
                .signed_duration_since(now)
                .to_std()
                .ok()
        })
        .max()
        .unwrap_or_default()
        .min(DEVICE_VERIFICATION_WINDOW);

    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }

    state
        .devices
        .list_with_online_window(DEVICE_VERIFICATION_WINDOW)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn remove_device(id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let id = Uuid::parse_str(&id).map_err(|error| error.to_string())?;
    if state
        .jobs
        .references_device(id)
        .map_err(|error| error.to_string())?
    {
        return Err("device is referenced by a deployment job".to_owned());
    }
    state.devices.remove(id).map_err(|error| error.to_string())
}

#[tauri::command]
fn network_interfaces() -> Result<Vec<NetworkInterfaceSummary>, String> {
    easydeploymesh_service::list_network_interfaces()
}

#[tauri::command]
fn load_pxe_config(state: State<'_, AppState>) -> Result<Option<PxeConfig>, String> {
    if !state.pxe_config_path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&state.pxe_config_path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("PXE configuration is invalid: {error}"))
}

#[tauri::command]
fn save_pxe_config(config: PxeConfig, state: State<'_, AppState>) -> Result<PxeConfig, String> {
    easydeploymesh_service::validate_pxe_config(&config).map_err(|error| error.to_string())?;
    persist_pxe_config(&config, &state.pxe_config_path)?;
    Ok(config)
}

fn persist_pxe_config(config: &PxeConfig, path: &std::path::Path) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(&config).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn require_winpe_import_host() -> Result<(), String> {
    let capability = easydeploymesh_service::wimlib_capability();
    if capability.supported {
        Ok(())
    } else {
        Err(capability
            .reason
            .unwrap_or_else(|| "WinPE import is unavailable".into()))
    }
}

#[tauri::command]
fn winpe_import_capability() -> WimlibCapability {
    easydeploymesh_service::wimlib_capability()
}

#[tauri::command]
fn import_pxe_boot_package(
    source: String,
    bios_boot_file: String,
    uefi_x64_boot_file: String,
    state: State<'_, AppState>,
) -> Result<BootPackage, String> {
    require_winpe_import_host()?;
    if !state.agent_binary_path.is_file() {
        return Err(format!(
            "EasyDeployMesh Agent sidecar is missing: {}",
            state.agent_binary_path.display()
        ));
    }
    let package = BootPackage::import_with_agent(
        source,
        &state.pxe_boot_root,
        &bios_boot_file,
        &uefi_x64_boot_file,
        &state.agent_binary_path,
    )
    .map_err(|error| error.to_string())?;
    inject_saved_agent_bootstrap(&state.pxe_boot_root)?;
    Ok(package)
}

#[tauri::command]
fn import_pxe_media(source: String, state: State<'_, AppState>) -> Result<BootPackage, String> {
    require_winpe_import_host()?;
    if !state.agent_binary_path.is_file() {
        return Err(format!(
            "EasyDeployMesh Agent sidecar is missing: {}",
            state.agent_binary_path.display()
        ));
    }
    let package = BootPackage::import_media_with_agent(
        source,
        &state.pxe_boot_root,
        Some(&state.agent_binary_path),
    )
    .map_err(|error| error.to_string())?;
    inject_saved_agent_bootstrap(&state.pxe_boot_root)?;
    Ok(package)
}

fn inject_saved_agent_bootstrap(pxe_boot_root: &std::path::Path) -> Result<bool, String> {
    let bootstrap_path = pxe_boot_root.join("boot/easydeploymesh-bootstrap.json");
    if !bootstrap_path.is_file() {
        return Ok(false);
    }
    let bootstrap = fs::read(&bootstrap_path).map_err(|error| error.to_string())?;
    BootPackage::inject_agent_bootstrap(pxe_boot_root.join("boot/boot.wim"), &bootstrap)
        .map_err(|error| format!("could not place the Agent bootstrap inside WinPE: {error}"))?;
    Ok(true)
}

#[tauri::command]
async fn start_pxe_service(
    mut config: PxeConfig,
    control_port: Option<u16>,
    state: State<'_, AppState>,
) -> Result<PxeServiceStatus, String> {
    if BootPackage::ensure_managed_network_boot(&config.tftp_root)
        .map_err(|error| format!("could not refresh the managed PXE network loaders: {error}"))?
    {
        config.bios_boot_file = "undionly.kpxe".into();
        config.uefi_x64_boot_file = "ipxe.efi".into();
        easydeploymesh_service::validate_pxe_config(&config).map_err(|error| error.to_string())?;
        persist_pxe_config(&config, &state.pxe_config_path)?;
    }
    let control_status = state.control_plane.status().await;
    if control_status.state != "running" {
        start_control_plane_inner(&config.bind_address, control_port.unwrap_or(7760), &state)
            .await?;
    } else {
        sync_agent_bootstrap(&control_status, &state).await?;
    }
    if !state
        .pxe_boot_root
        .join("boot/easydeploymesh-bootstrap.json")
        .is_file()
    {
        return Err("PXE Agent bootstrap file is missing".to_owned());
    }
    let result = state.pxe.start(config).await;
    match result {
        Ok(status) => {
            let mut details = serde_json::Map::new();
            if let Some(address) = &status.bind_address {
                details.insert("ipAddress".into(), address.clone().into());
            }
            record_activity(
                &state,
                ActivitySource::Service,
                "pxe_service_started",
                ActivitySeverity::Success,
                Some(service_subject("pxe")),
                details,
                None,
            );
            Ok(status)
        }
        Err(error) => {
            record_activity(
                &state,
                ActivitySource::Service,
                "pxe_service_failed",
                ActivitySeverity::Error,
                Some(service_subject("pxe")),
                serde_json::Map::new(),
                Some(error.to_string()),
            );
            Err(error.to_string())
        }
    }
}

#[tauri::command]
async fn stop_pxe_service(state: State<'_, AppState>) -> Result<PxeServiceStatus, String> {
    let status = state.pxe.stop().await.map_err(|error| error.to_string())?;
    record_activity(
        &state,
        ActivitySource::Service,
        "pxe_service_stopped",
        ActivitySeverity::Info,
        Some(service_subject("pxe")),
        serde_json::Map::new(),
        None,
    );
    Ok(status)
}

#[tauri::command]
async fn pxe_service_status(state: State<'_, AppState>) -> Result<PxeServiceStatus, String> {
    Ok(state.pxe.status().await)
}

#[tauri::command]
async fn pxe_discovered_clients(
    state: State<'_, AppState>,
) -> Result<Vec<PxeDiscoveredClient>, String> {
    Ok(state.pxe.discovered_clients().await)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityQueryInput {
    #[serde(default)]
    sources: Vec<ActivitySource>,
    #[serde(default)]
    severities: Vec<ActivitySeverity>,
    before: Option<chrono::DateTime<chrono::Utc>>,
    after: Option<chrono::DateTime<chrono::Utc>>,
    limit: Option<usize>,
}

#[tauri::command]
fn activity_events(
    query: ActivityQueryInput,
    state: State<'_, AppState>,
) -> Result<Vec<ActivityEvent>, String> {
    capture_offline_devices(&state);
    state
        .activities
        .query(&ActivityQuery {
            sources: query.sources,
            severities: query.severities,
            before: query.before,
            after: query.after,
            limit: query.limit.unwrap_or(200),
        })
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_images(state: State<'_, AppState>) -> Result<Vec<ImageArtifact>, String> {
    state.images.list().map_err(|error| error.to_string())
}

#[tauri::command]
async fn verify_gho_image(id: String, state: State<'_, AppState>) -> Result<ImageArtifact, String> {
    let id = Uuid::parse_str(&id).map_err(|error| error.to_string())?;
    let images = Arc::clone(&state.images);
    tauri::async_runtime::spawn_blocking(move || images.verify_gho_image(id))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

/// Serializes desktop mutations that create or remove image references.
///
/// Image preflight is intentionally part of the same critical section as the policy/job commit:
/// otherwise a concurrent delete can invalidate the successful preflight before the reference is
/// persisted. Read-only list operations remain outside this boundary.
struct DeploymentMutationCoordinator {
    images: Arc<ImageLibrary>,
    jobs: Arc<JobRepository>,
    mutation_lock: tokio::sync::Mutex<()>,
}

impl DeploymentMutationCoordinator {
    fn new(images: Arc<ImageLibrary>, jobs: Arc<JobRepository>) -> Self {
        Self {
            images,
            jobs,
            mutation_lock: tokio::sync::Mutex::new(()),
        }
    }

    async fn remove_image(&self, id: Uuid) -> Result<bool, String> {
        let _mutation = self.mutation_lock.lock().await;
        if self
            .jobs
            .references_image(id)
            .map_err(|error| error.to_string())?
        {
            return Err("image is referenced by a deployment job".to_owned());
        }
        self.images.remove(id).map_err(|error| error.to_string())
    }

    async fn create_job(&self, request: CreateDeploymentJob) -> Result<DeploymentJob, String> {
        let _mutation = self.mutation_lock.lock().await;
        let image = self
            .images
            .get(request.image_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("deployment image was not found: {}", request.image_id))?;
        validate_deployment_compatibility(request.operation, &image)?;
        for target in &request.targets {
            request
                .options
                .partition_plan
                .validate_capacity(target.target_disk_size_bytes, image.size_bytes)
                .map_err(|error| {
                    format!(
                        "target disk capacity is insufficient for {} ({}): {error}",
                        target.target_disk_model, target.target_disk_id
                    )
                })?;
        }
        let images = Arc::clone(&self.images);
        let image_id = request.image_id;
        let image_index = request.options.image_index;
        let operation = request.operation;
        tokio::task::spawn_blocking(move || match operation {
            Operation::DeployWim => images
                .revalidate_for_deployment(image_id, image_index)
                .map(|_| ()),
            Operation::DeployGho => images.prepare_gho_deployment(image_id).map(|_| ()),
            Operation::CaptureGho => Err(
                easydeploymesh_service::ImageLibraryError::UnsupportedFormat(
                    "GHO capture is not supported".to_owned(),
                ),
            ),
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("deployment image preflight failed: {error}"))?;

        self.jobs
            .enqueue(request)
            .map_err(|error| error.to_string())
    }

    async fn remove_job(&self, id: Uuid) -> Result<bool, String> {
        let _mutation = self.mutation_lock.lock().await;
        self.jobs.remove(id).map_err(|error| error.to_string())
    }
}

#[tauri::command]
async fn import_image(path: String, state: State<'_, AppState>) -> Result<ImageArtifact, String> {
    let library = Arc::clone(&state.images);
    tauri::async_runtime::spawn_blocking(move || library.import(path))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn remove_image(id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let id = Uuid::parse_str(&id).map_err(|error| error.to_string())?;
    state.deployment_mutations.remove_image(id).await
}

#[tauri::command]
fn list_jobs(state: State<'_, AppState>) -> Result<Vec<DeploymentJob>, String> {
    state.jobs.list().map_err(|error| error.to_string())
}

#[tauri::command]
async fn create_job(
    request: CreateDeploymentJob,
    state: State<'_, AppState>,
) -> Result<DeploymentJob, String> {
    let job = state.deployment_mutations.create_job(request).await?;
    record_job_activity(&state, &job, "job_queued", ActivitySeverity::Info, None);
    Ok(job)
}

#[tauri::command]
async fn remove_job(id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let id = Uuid::parse_str(&id).map_err(|error| error.to_string())?;
    state.deployment_mutations.remove_job(id).await
}

fn validate_deployment_compatibility(
    operation: Operation,
    image: &ImageArtifact,
) -> Result<(), String> {
    if !image.verified {
        return Err(format!("deployment image is not verified: {}", image.id));
    }

    if operation == Operation::CaptureGho {
        return Err("GHO capture is not supported by the current EasyDeployMesh Agent".to_owned());
    }

    match image.format {
        ImageFormat::Gho if operation == Operation::DeployGho => Ok(()),
        ImageFormat::Gho => Err(format!(
            "deployment operation {operation:?} does not match a GHO image"
        )),
        ImageFormat::Swm => Err(
            "SWM deployment images are not supported by the current EasyDeployMesh Agent"
                .to_owned(),
        ),
        ImageFormat::Wim | ImageFormat::Esd if operation != Operation::DeployWim => Err(format!(
            "deployment operation {operation:?} does not match a {:?} image; current Agents require DeployWim for WIM/ESD images",
            image.format
        )),
        ImageFormat::Wim | ImageFormat::Esd => Ok(()),
    }
}

#[tauri::command]
fn transition_job(
    id: String,
    next_state: JobState,
    state: State<'_, AppState>,
) -> Result<DeploymentJob, String> {
    let id = Uuid::parse_str(&id).map_err(|error| error.to_string())?;
    let updated = state
        .jobs
        .transition(id, next_state)
        .map_err(|error| error.to_string())?;
    let (kind, severity) = match next_state {
        JobState::Waiting => ("job_queued", ActivitySeverity::Info),
        JobState::Running => ("job_resumed", ActivitySeverity::Info),
        JobState::Paused => ("job_paused", ActivitySeverity::Warning),
        JobState::Cancelled => ("job_cancelled", ActivitySeverity::Warning),
        JobState::Succeeded => ("job_succeeded", ActivitySeverity::Success),
        JobState::Failed => ("job_failed", ActivitySeverity::Error),
        JobState::Draft => ("job_created", ActivitySeverity::Info),
    };
    record_job_activity(
        &state,
        &updated,
        kind,
        severity,
        updated.error_message.clone(),
    );
    Ok(updated)
}

struct AppState {
    activities: Arc<ActivityRepository>,
    control_plane: Arc<ControlPlane>,
    deployment_mutations: Arc<DeploymentMutationCoordinator>,
    devices: Arc<DeviceRegistry>,
    images: Arc<ImageLibrary>,
    jobs: Arc<JobRepository>,
    pxe: Arc<PxeService>,
    pxe_config_path: PathBuf,
    pxe_boot_root: PathBuf,
    agent_binary_path: PathBuf,
    offline_devices: Mutex<HashSet<Uuid>>,
}

fn service_subject(id: &str) -> ActivitySubject {
    ActivitySubject {
        id: id.into(),
        name: id.into(),
    }
}

fn record_activity(
    state: &AppState,
    source: ActivitySource,
    kind: &str,
    severity: ActivitySeverity,
    subject: Option<ActivitySubject>,
    details: serde_json::Map<String, serde_json::Value>,
    raw: Option<String>,
) {
    let _ = state
        .activities
        .record(source, kind, severity, subject, details, raw);
}

fn record_job_activity(
    state: &AppState,
    job: &DeploymentJob,
    kind: &str,
    severity: ActivitySeverity,
    raw: Option<String>,
) {
    let mut details = serde_json::Map::new();
    details.insert("jobId".into(), job.id.to_string().into());
    details.insert(
        "operation".into(),
        serde_json::to_value(job.operation).unwrap_or_default(),
    );
    if let Some(stage) = job.stage {
        details.insert(
            "stage".into(),
            serde_json::to_value(stage).unwrap_or_default(),
        );
    }
    record_activity(
        state,
        ActivitySource::Deployment,
        kind,
        severity,
        Some(ActivitySubject {
            id: job.id.to_string(),
            name: job.name.clone(),
        }),
        details,
        raw,
    );
}

fn capture_offline_devices(state: &AppState) {
    let Ok(devices) = state.devices.list() else {
        return;
    };
    let Ok(mut logged) = state.offline_devices.lock() else {
        return;
    };
    for registered in devices {
        let id = registered.device.id;
        if registered.online {
            logged.remove(&id);
            continue;
        }
        if logged.insert(id) {
            let device = registered.device;
            let name = device
                .hostname
                .clone()
                .unwrap_or_else(|| device.mac_address.clone());
            let mut details = serde_json::Map::new();
            details.insert("macAddress".into(), device.mac_address.into());
            details.insert("ipAddress".into(), device.ip_address.into());
            let _ = state.activities.record(
                ActivitySource::Device,
                "device_offline",
                ActivitySeverity::Warning,
                Some(ActivitySubject {
                    id: id.to_string(),
                    name,
                }),
                details,
                None,
            );
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            if let (Some(window), Some(icon)) =
                (app.get_webview_window("main"), app.default_window_icon())
            {
                window.set_icon(icon.clone())?;
            }

            let app_data_dir = app.path().app_data_dir()?;
            let agent_binary_path = app.path().resolve(
                "easydeploymesh-agent.exe",
                tauri::path::BaseDirectory::Resource,
            )?;
            #[cfg(target_os = "macos")]
            {
                let target = if cfg!(target_arch = "aarch64") {
                    "aarch64-apple-darwin"
                } else {
                    "x86_64-apple-darwin"
                };
                let executable = std::env::current_exe()?;
                let resource_dir = app.path().resource_dir()?;
                let candidates = [
                    executable
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .join("wimlib-imagex"),
                    resource_dir.join("wimlib-imagex"),
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join(format!("binaries/wimlib-imagex-{target}")),
                ];
                if let Some(path) = candidates.into_iter().find(|path| path.is_file()) {
                    easydeploymesh_service::configure_wimlib(path)
                        .map_err(std::io::Error::other)?;
                }
            }
            let devices = Arc::new(DeviceRegistry::open(app_data_dir.join("devices"))?);
            let images = Arc::new(ImageLibrary::open(app_data_dir.join("library"))?);
            let jobs = Arc::new(JobRepository::open(app_data_dir.join("jobs"))?);
            let activities = Arc::new(ActivityRepository::open(
                app_data_dir.join("activities.json"),
            )?);
            let deployment_mutations = Arc::new(DeploymentMutationCoordinator::new(
                Arc::clone(&images),
                Arc::clone(&jobs),
            ));
            app.manage(AppState {
                activities: Arc::clone(&activities),
                control_plane: Arc::new(ControlPlane::new(
                    Arc::clone(&devices),
                    Arc::clone(&jobs),
                    Arc::clone(&images),
                    Arc::clone(&activities),
                )),
                deployment_mutations,
                devices,
                images,
                jobs,
                pxe: Arc::new(PxeService::open_with_activity(
                    app_data_dir.join("pxe-leases.json"),
                    activities,
                )?),
                pxe_config_path: app_data_dir.join("pxe-config.json"),
                pxe_boot_root: app_data_dir.join("pxe-boot"),
                agent_binary_path,
                offline_devices: Mutex::new(HashSet::new()),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            runtime_status,
            winpe_import_capability,
            network_interfaces,
            load_pxe_config,
            save_pxe_config,
            import_pxe_boot_package,
            import_pxe_media,
            start_pxe_service,
            stop_pxe_service,
            pxe_service_status,
            pxe_discovered_clients,
            activity_events,
            control_plane_status,
            start_control_plane,
            stop_control_plane,
            list_devices,
            refresh_devices,
            remove_device,
            list_images,
            import_image,
            remove_image,
            verify_gho_image,
            list_jobs,
            create_job,
            remove_job,
            transition_job
        ])
        .run(tauri::generate_context!())
        .expect("error while running EasyDeployMesh");
}

#[cfg(test)]
mod tests {
    use super::*;
    use easydeploymesh_core::{
        DeploymentOptions, DeploymentTarget, ImageFormat, Operation, PartitionPlan,
    };

    #[test]
    fn winpe_import_is_rejected_before_native_tools_run_on_unsupported_hosts() {
        let result = require_winpe_import_host();
        if cfg!(target_os = "windows") {
            assert!(result.is_ok());
        } else {
            assert!(!result.unwrap_err().is_empty());
        }
    }

    fn wim_fixture(payload: &[u8]) -> Vec<u8> {
        const HEADER_SIZE: usize = 208;
        let mut contents = vec![0_u8; HEADER_SIZE];
        contents[..8].copy_from_slice(b"MSWIM\0\0\0");
        contents[8..12].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
        contents[12..16].copy_from_slice(&0x0001_0d00_u32.to_le_bytes());
        contents[40..42].copy_from_slice(&1_u16.to_le_bytes());
        contents[42..44].copy_from_slice(&1_u16.to_le_bytes());
        contents[44..48].copy_from_slice(&1_u32.to_le_bytes());
        contents.extend_from_slice(payload);
        contents
    }

    fn image(format: ImageFormat, verified: bool) -> ImageArtifact {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::new_v4(),
            "name": "deployment-image",
            "format": format,
            "sourcePath": "/tmp/deployment-image",
            "sizeBytes": 1024,
            "sha256": "0123456789abcdef",
            "spans": [],
            "verified": verified,
            "createdAt": "2026-08-16T00:00:00Z"
        }))
        .expect("image fixture should deserialize")
    }

    #[test]
    fn deployment_compatibility_accepts_verified_wim_esd_and_gho_operations() {
        for format in [ImageFormat::Wim, ImageFormat::Esd] {
            assert!(
                validate_deployment_compatibility(Operation::DeployWim, &image(format, true))
                    .is_ok()
            );
        }
        assert!(validate_deployment_compatibility(
            Operation::DeployGho,
            &image(ImageFormat::Gho, true),
        )
        .is_ok());

        let error = validate_deployment_compatibility(
            Operation::DeployWim,
            &image(ImageFormat::Wim, false),
        )
        .expect_err("unverified image should be rejected");
        assert!(error.contains("not verified"));
    }

    #[test]
    fn deployment_compatibility_rejects_unsupported_formats_operations_and_mismatches() {
        let cases = [
            (
                Operation::DeployWim,
                ImageFormat::Gho,
                "does not match a GHO",
            ),
            (Operation::DeployWim, ImageFormat::Swm, "SWM"),
            (Operation::CaptureGho, ImageFormat::Wim, "GHO capture"),
            (Operation::DeployGho, ImageFormat::Wim, "does not match"),
        ];

        for (operation, format, expected) in cases {
            let error = validate_deployment_compatibility(operation, &image(format, true))
                .expect_err("unsupported combination should be rejected");
            assert!(
                error.contains(expected),
                "expected {error:?} to contain {expected:?}"
            );
        }
    }

    #[tokio::test]
    async fn job_creation_and_image_removal_preserve_the_reference_invariant() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let images = Arc::new(
            ImageLibrary::open(temp.path().join("images")).expect("image library should open"),
        );
        let jobs = Arc::new(
            JobRepository::open(temp.path().join("jobs")).expect("job repository should open"),
        );
        let coordinator = Arc::new(DeploymentMutationCoordinator::new(
            Arc::clone(&images),
            Arc::clone(&jobs),
        ));

        let source = temp.path().join("windows.wim");
        fs::write(&source, wim_fixture(b"desktop-job-mutation-coordinator"))
            .expect("WIM fixture should write");
        let image = images.import(&source).expect("WIM fixture should import");
        let request = CreateDeploymentJob {
            name: "Concurrent deployment".to_owned(),
            operation: Operation::DeployWim,
            image_id: image.id,
            targets: vec![DeploymentTarget {
                device_id: Uuid::new_v4(),
                target_disk_id: r"\\.\PhysicalDrive0".to_owned(),
                target_disk_model: "Test disk".to_owned(),
                target_disk_serial: Some("TEST-SERIAL".to_owned()),
                target_disk_size_bytes: 64 * 1024 * 1024 * 1024,
            }],
            options: DeploymentOptions {
                image_index: 1,
                partition_plan: PartitionPlan::uefi_gpt(),
            },
        };

        let ready = Arc::new(tokio::sync::Barrier::new(3));
        let create = {
            let coordinator = Arc::clone(&coordinator);
            let ready = Arc::clone(&ready);
            tokio::spawn(async move {
                ready.wait().await;
                coordinator.create_job(request).await
            })
        };
        let remove = {
            let coordinator = Arc::clone(&coordinator);
            let ready = Arc::clone(&ready);
            tokio::spawn(async move {
                ready.wait().await;
                coordinator.remove_image(image.id).await
            })
        };
        ready.wait().await;

        let create_result = create.await.expect("create task should finish");
        let remove_result = remove.await.expect("remove task should finish");
        assert_ne!(
            create_result.is_ok(),
            remove_result.is_ok(),
            "exactly one conflicting mutation should commit: create={create_result:?}, remove={remove_result:?}"
        );

        let image_exists = images
            .contains(image.id)
            .expect("image library should remain readable");
        let image_is_referenced = jobs
            .references_image(image.id)
            .expect("job repository should remain readable");
        assert!(
            !image_is_referenced || image_exists,
            "a deployment job must never reference a removed image"
        );
    }

    #[tokio::test]
    async fn job_creation_rejects_a_partition_plan_that_exhausts_the_target_disk() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let images = Arc::new(
            ImageLibrary::open(temp.path().join("images")).expect("image library should open"),
        );
        let jobs = Arc::new(
            JobRepository::open(temp.path().join("jobs")).expect("job repository should open"),
        );
        let coordinator = DeploymentMutationCoordinator::new(Arc::clone(&images), jobs);
        let source = temp.path().join("windows.wim");
        fs::write(&source, wim_fixture(b"capacity-preflight")).expect("WIM fixture should write");
        let image = images.import(&source).expect("WIM fixture should import");

        let mut plan = PartitionPlan::uefi_gpt();
        plan.partitions.last_mut().unwrap().size_mib = Some(30 * 1024);
        plan.partitions.push(easydeploymesh_core::PartitionSpec {
            role: easydeploymesh_core::PartitionRole::Data,
            size_mib: Some(70 * 1024),
            file_system: Some(easydeploymesh_core::PartitionFileSystem::Ntfs),
            label: "Software".to_owned(),
            drive_letter: Some('D'),
        });
        plan.partitions.push(easydeploymesh_core::PartitionSpec {
            role: easydeploymesh_core::PartitionRole::Data,
            size_mib: None,
            file_system: Some(easydeploymesh_core::PartitionFileSystem::Ntfs),
            label: "Data".to_owned(),
            drive_letter: Some('E'),
        });
        let request = CreateDeploymentJob {
            name: "Capacity preflight".to_owned(),
            operation: Operation::DeployWim,
            image_id: image.id,
            targets: vec![DeploymentTarget {
                device_id: Uuid::new_v4(),
                target_disk_id: r"\\.\PhysicalDrive0".to_owned(),
                target_disk_model: "100 GiB test disk".to_owned(),
                target_disk_serial: None,
                target_disk_size_bytes: 100_000_000_000,
            }],
            options: DeploymentOptions {
                image_index: 1,
                partition_plan: plan,
            },
        };

        let error = coordinator
            .create_job(request)
            .await
            .expect_err("capacity must fail before the job is persisted");
        assert!(error.contains("capacity is insufficient"));
        assert!(error.contains("fixed partitions"));
    }

    #[tokio::test]
    async fn running_control_plane_can_restore_bootstrap_after_package_replacement() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let devices = Arc::new(
            DeviceRegistry::open(temp.path().join("devices")).expect("device registry should open"),
        );
        let images = Arc::new(
            ImageLibrary::open(temp.path().join("images")).expect("image library should open"),
        );
        let jobs = Arc::new(
            JobRepository::open(temp.path().join("jobs")).expect("job repository should open"),
        );
        let activities = Arc::new(
            ActivityRepository::open(temp.path().join("activities.json"))
                .expect("activity repository should open"),
        );
        let control_plane = Arc::new(ControlPlane::new(
            Arc::clone(&devices),
            Arc::clone(&jobs),
            Arc::clone(&images),
            Arc::clone(&activities),
        ));
        let status = control_plane
            .start("127.0.0.1", 0)
            .await
            .expect("control service should start");
        let state = AppState {
            activities,
            control_plane: Arc::clone(&control_plane),
            deployment_mutations: Arc::new(DeploymentMutationCoordinator::new(
                Arc::clone(&images),
                Arc::clone(&jobs),
            )),
            devices,
            images,
            jobs,
            pxe: Arc::new(
                PxeService::open(temp.path().join("pxe-leases.json"))
                    .expect("PXE service should open"),
            ),
            pxe_config_path: temp.path().join("pxe-config.json"),
            pxe_boot_root: temp.path().join("replaced-pxe-package"),
            agent_binary_path: temp.path().join("easydeploymesh-agent.exe"),
            offline_devices: Mutex::new(HashSet::new()),
        };

        sync_agent_bootstrap(&status, &state)
            .await
            .expect("running control service should recreate the bootstrap");

        let bootstrap: serde_json::Value = serde_json::from_slice(
            &fs::read(
                state
                    .pxe_boot_root
                    .join("boot/easydeploymesh-bootstrap.json"),
            )
            .expect("bootstrap should exist"),
        )
        .expect("bootstrap should be valid JSON");
        assert_eq!(
            bootstrap["server"],
            status.endpoint.expect("status should contain endpoint")
        );
        assert_eq!(
            bootstrap["enrollmentToken"],
            status
                .enrollment_token
                .expect("status should contain enrollment token")
        );

        control_plane
            .stop()
            .await
            .expect("control service should stop");
    }
}
