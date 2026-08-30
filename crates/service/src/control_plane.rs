#[path = "installer_deployment.rs"]
mod installer_deployment;

use crate::device_registry::{digest_secret, generate_secret, secret_matches};
use crate::{
    ActivityRepository, DeviceRegistry, DeviceRegistryError, ImageLibrary, ImageLibraryError,
    JobRepository, JobRepositoryError,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use easydeploymesh_core::{
    ActivitySeverity, ActivitySource, ActivitySubject, AgentDeploymentImage, AgentGhoDeployment,
    AgentHeartbeat, AgentHeartbeatAck, AgentInventory, AgentJobCompletion, AgentJobLease,
    AgentJobProgress, AgentRegistration, ControlPlaneStatus, JobState, LinuxInstallerGuardRequest,
    Operation,
};
use installer_deployment::{
    BootOutcome, FirstBootRequest, InstallerDeployment, InstallerDeploymentError,
    InstallerEventRequest, InstallerMediaKind, render_boot_script,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    str::FromStr,
    sync::Arc,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt},
    net::TcpListener,
    sync::{Mutex, oneshot},
    task::JoinHandle,
};
use uuid::Uuid;

const JOB_LEASE_MINUTES: i64 = 120;
const INSTALLER_BODY_LIMIT_BYTES: usize = 512 * 1024;

#[derive(Debug, Error)]
pub enum ControlPlaneError {
    #[error("control service is already running")]
    AlreadyRunning,
    #[error("control service is not running")]
    NotRunning,
    #[error("bind address is invalid: {0}")]
    InvalidBindAddress(String),
    #[error("could not bind control service to {address}: {source}")]
    Bind {
        address: String,
        #[source]
        source: std::io::Error,
    },
    #[error("control service task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error("control service failed: {0}")]
    Serve(String),
}

#[derive(Clone)]
struct ApiState {
    registry: Arc<DeviceRegistry>,
    jobs: Arc<JobRepository>,
    images: Arc<ImageLibrary>,
    enrollment_token_digest: String,
    activities: Arc<ActivityRepository>,
    installer: Arc<InstallerDeployment>,
    installer_base_url: String,
}

struct RunningServer {
    status: ControlPlaneStatus,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<Result<(), String>>,
}

pub struct ControlPlane {
    registry: Arc<DeviceRegistry>,
    jobs: Arc<JobRepository>,
    images: Arc<ImageLibrary>,
    activities: Arc<ActivityRepository>,
    running: Mutex<Option<RunningServer>>,
}

impl ControlPlane {
    pub fn new(
        registry: Arc<DeviceRegistry>,
        jobs: Arc<JobRepository>,
        images: Arc<ImageLibrary>,
        activities: Arc<ActivityRepository>,
    ) -> Self {
        Self {
            registry,
            jobs,
            images,
            activities,
            running: Mutex::new(None),
        }
    }

    pub async fn start(
        &self,
        bind_address: &str,
        port: u16,
    ) -> Result<ControlPlaneStatus, ControlPlaneError> {
        let ip_address = IpAddr::from_str(bind_address)
            .map_err(|_| ControlPlaneError::InvalidBindAddress(bind_address.to_owned()))?;
        if ip_address.is_unspecified() || ip_address.is_multicast() {
            return Err(ControlPlaneError::InvalidBindAddress(
                bind_address.to_owned(),
            ));
        }

        let mut running = self.running.lock().await;
        if running
            .as_ref()
            .is_some_and(|server| !server.task.is_finished())
        {
            return Err(ControlPlaneError::AlreadyRunning);
        }
        if let Some(stale) = running.take() {
            stale.task.abort();
        }

        let requested_address = SocketAddr::new(ip_address, port);
        let listener = TcpListener::bind(requested_address)
            .await
            .map_err(|source| ControlPlaneError::Bind {
                address: requested_address.to_string(),
                source,
            })?;
        let local_address = listener
            .local_addr()
            .map_err(|source| ControlPlaneError::Bind {
                address: requested_address.to_string(),
                source,
            })?;
        let enrollment_token = generate_secret("easydeploymesh_enroll");
        let api_state = ApiState {
            registry: Arc::clone(&self.registry),
            jobs: Arc::clone(&self.jobs),
            images: Arc::clone(&self.images),
            enrollment_token_digest: digest_secret(&enrollment_token),
            activities: Arc::clone(&self.activities),
            installer: Arc::new(InstallerDeployment::new(
                Arc::clone(&self.registry),
                Arc::clone(&self.jobs),
                Arc::clone(&self.images),
            )),
            installer_base_url: format!("http://{local_address}"),
        };
        let router = Router::new()
            .route("/health", get(health))
            .route("/api/v1/agents/register", post(register_agent))
            .route(
                "/api/v1/agents/{device_id}/heartbeat",
                post(agent_heartbeat),
            )
            .route("/api/v1/agents/{device_id}/jobs/claim", post(claim_job))
            .route(
                "/api/v1/agents/{device_id}/jobs/{job_id}/progress",
                post(report_job_progress),
            )
            .route(
                "/api/v1/agents/{device_id}/jobs/{job_id}/control",
                get(job_control_state),
            )
            .route(
                "/api/v1/agents/{device_id}/jobs/{job_id}/complete",
                post(complete_job),
            )
            .route(
                "/api/v1/agents/{device_id}/jobs/{job_id}/image",
                get(download_job_image),
            )
            .route("/api/v1/install/boot.ipxe", get(installer_boot_script))
            .route(
                "/api/v1/install/sessions/{session_id}/seed/{token}/user-data",
                get(installer_user_data),
            )
            .route(
                "/api/v1/install/sessions/{session_id}/seed/{token}/meta-data",
                get(installer_meta_data),
            )
            .route(
                "/api/v1/install/sessions/{session_id}/guard",
                post(installer_guard),
            )
            .route(
                "/api/v1/install/sessions/{session_id}/kernel",
                get(installer_kernel),
            )
            .route(
                "/api/v1/install/sessions/{session_id}/initrd",
                get(installer_initrd),
            )
            .route(
                "/api/v1/install/sessions/{session_id}/iso",
                get(installer_iso),
            )
            .route(
                "/api/v1/install/sessions/{session_id}/events",
                post(installer_event),
            )
            .route(
                "/api/v1/install/sessions/{session_id}/first-boot",
                post(installer_first_boot),
            )
            .layer(DefaultBodyLimit::max(INSTALLER_BODY_LIMIT_BYTES))
            .with_state(api_state);
        let (shutdown, shutdown_receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = shutdown_receiver.await;
            })
            .await
            .map_err(|error| error.to_string())
        });
        let status = ControlPlaneStatus {
            state: "running".to_owned(),
            bind_address: Some(local_address.ip().to_string()),
            port: Some(local_address.port()),
            endpoint: Some(format!("http://{local_address}")),
            enrollment_token: Some(enrollment_token),
        };
        *running = Some(RunningServer {
            status: status.clone(),
            shutdown,
            task,
        });

        Ok(status)
    }

    pub async fn stop(&self) -> Result<ControlPlaneStatus, ControlPlaneError> {
        let server = self
            .running
            .lock()
            .await
            .take()
            .ok_or(ControlPlaneError::NotRunning)?;
        let _ = server.shutdown.send(());
        server.task.await?.map_err(ControlPlaneError::Serve)?;
        Ok(stopped_status())
    }

    pub async fn status(&self) -> ControlPlaneStatus {
        let running = self.running.lock().await;
        match running.as_ref() {
            Some(server) if !server.task.is_finished() => server.status.clone(),
            Some(_) => ControlPlaneStatus {
                state: "error".to_owned(),
                bind_address: None,
                port: None,
                endpoint: None,
                enrollment_token: None,
            },
            None => stopped_status(),
        }
    }
}

fn stopped_status() -> ControlPlaneStatus {
    ControlPlaneStatus {
        state: "idle".to_owned(),
        bind_address: None,
        port: None,
        endpoint: None,
        enrollment_token: None,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn register_agent(
    State(state): State<ApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(inventory): Json<AgentInventory>,
) -> Result<Json<AgentRegistration>, ApiError> {
    let token = bearer_token(&headers).ok_or(ApiError::Unauthorized)?;
    if !secret_matches(&state.enrollment_token_digest, token) {
        return Err(ApiError::Unauthorized);
    }

    let was_known = state
        .registry
        .list()
        .map_err(ApiError::Registry)?
        .iter()
        .any(|entry| entry.device.mac_address == inventory.mac_address);
    let (registered, registration) = state
        .registry
        .register(inventory, peer.ip())
        .map_err(ApiError::Registry)?;
    let kind = if was_known {
        "device_reconnected"
    } else {
        "device_registered"
    };
    record_device_activity(&state, &registered, kind, ActivitySeverity::Success, None);
    Ok(Json(registration))
}

async fn agent_heartbeat(
    State(state): State<ApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(device_id): Path<String>,
    headers: HeaderMap,
    Json(heartbeat): Json<AgentHeartbeat>,
) -> Result<Json<AgentHeartbeatAck>, ApiError> {
    let device_id = Uuid::parse_str(&device_id).map_err(|_| ApiError::NotFound)?;
    let token = bearer_token(&headers).ok_or(ApiError::Unauthorized)?;
    let was_online = state
        .registry
        .list()
        .map_err(ApiError::Registry)?
        .into_iter()
        .find(|entry| entry.device.id == device_id)
        .is_some_and(|entry| entry.online);
    let response = state
        .registry
        .heartbeat(device_id, token, heartbeat.inventory, peer.ip())
        .map_err(ApiError::Registry)?;
    if !was_online {
        if let Some(registered) = state
            .registry
            .list()
            .map_err(ApiError::Registry)?
            .into_iter()
            .find(|entry| entry.device.id == device_id)
        {
            record_device_activity(
                &state,
                &registered,
                "device_reconnected",
                ActivitySeverity::Success,
                None,
            );
        }
    }
    Ok(Json(response))
}

async fn claim_job(
    State(state): State<ApiState>,
    Path(device_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Option<AgentJobLease>>, ApiError> {
    let device_id = authenticated_device(&state, &device_id, &headers)?;
    let device_snapshot = state
        .registry
        .list()
        .map_err(ApiError::Registry)?
        .into_iter()
        .find(|registered| registered.device.id == device_id)
        .ok_or(ApiError::NotFound)?
        .device;
    if device_snapshot.disks.is_empty() {
        return Ok(Json(None));
    }
    let now = chrono::Utc::now();
    let candidates = state
        .jobs
        .list()
        .map_err(ApiError::Jobs)?
        .into_iter()
        .filter(|job| {
            (job.state == JobState::Waiting
                || (job.state == JobState::Running
                    && job
                        .lease_expires_at
                        .is_some_and(|expires_at| expires_at < now)))
                && job.targets.len() == 1
                && job.targets[0].device_id == device_id
                && device_snapshot
                    .disks
                    .iter()
                    .any(|disk| job.targets[0].matches_disk(disk))
                && matches!(job.operation, Operation::DeployWim | Operation::DeployGho)
        })
        .collect::<Vec<_>>();
    let images = Arc::clone(&state.images);
    let eligible_job_ids = tokio::task::spawn_blocking(move || {
        let mut validation_results = HashMap::new();
        candidates
            .into_iter()
            .filter_map(|job| {
                let validation_key = (job.image_id, job.options.image_index, job.operation);
                let eligible =
                    *validation_results.entry(validation_key).or_insert_with(|| {
                        match job.operation {
                            Operation::DeployWim => images
                                .revalidate_for_deployment(job.image_id, job.options.image_index)
                                .is_ok(),
                            Operation::DeployGho => images
                                .prepare_gho_partition_deployment(
                                    job.image_id,
                                    job.options.image_index,
                                )
                                .is_ok(),
                            Operation::CaptureGho | Operation::InstallLinux => false,
                        }
                    });
                eligible.then_some(job.id)
            })
            .collect::<HashSet<_>>()
    })
    .await
    .map_err(|error| ApiError::Conflict(format!("image validation task failed: {error}")))?;
    let lease_result = state
        .registry
        .with_current(device_id, |current_device| {
            state.jobs.lease_for_device_if(
                device_id,
                chrono::Duration::minutes(JOB_LEASE_MINUTES),
                |job| {
                    eligible_job_ids.contains(&job.id)
                        && matches!(job.operation, Operation::DeployWim | Operation::DeployGho)
                        && job.targets.len() == 1
                        && job.targets[0].device_id == device_id
                        && current_device
                            .disks
                            .iter()
                            .any(|disk| job.targets[0].matches_disk(disk))
                },
            )
        })
        .map_err(ApiError::Registry)?;
    let Some(job) = lease_result.map_err(ApiError::Jobs)? else {
        return Ok(Json(None));
    };
    record_job_activity(&state, &job, "job_started", ActivitySeverity::Info, None);
    let target = job
        .targets
        .first()
        .cloned()
        .ok_or(ApiError::Conflict("leased job has no target".to_owned()))?;
    let image = state
        .images
        .get(job.image_id)
        .map_err(ApiError::Images)?
        .ok_or(ApiError::NotFound)?;
    let lease_id = job
        .lease_id
        .ok_or(ApiError::Conflict("leased job has no lease id".to_owned()))?;
    let expires_at = job
        .lease_expires_at
        .ok_or(ApiError::Conflict("leased job has no expiry".to_owned()))?;
    let download_url = format!(
        "/api/v1/agents/{device_id}/jobs/{}/image?leaseId={lease_id}",
        job.id
    );
    let (gho, download_size_bytes, download_sha256) = if job.operation == Operation::DeployGho {
        let prepared = state
            .images
            .prepare_gho_partition_deployment(job.image_id, job.options.image_index)
            .map_err(ApiError::Images)?;
        let metadata =
            AgentGhoDeployment {
                source_partition: prepared.capability.source_partition.ok_or_else(|| {
                    ApiError::Conflict("GHO source partition is missing".to_owned())
                })?,
                expanded_size_bytes: prepared
                    .capability
                    .expanded_size_bytes
                    .ok_or_else(|| ApiError::Conflict("GHO expanded size is missing".to_owned()))?,
                expanded_sha256: prepared.capability.expanded_sha256.ok_or_else(|| {
                    ApiError::Conflict("GHO expanded checksum is missing".to_owned())
                })?,
                parser_version: prepared.capability.parser_version,
            };
        (
            Some(metadata),
            prepared.download_size_bytes,
            prepared.download_sha256,
        )
    } else {
        (
            None,
            image.size_bytes,
            image.sha256.clone().ok_or(ApiError::Conflict(
                "deployment image has no checksum".to_owned(),
            ))?,
        )
    };
    Ok(Json(Some(AgentJobLease {
        job_id: job.id,
        lease_id,
        expires_at,
        operation: job.operation,
        image: AgentDeploymentImage {
            id: image.id,
            name: image.name,
            format: image.format,
            size_bytes: download_size_bytes,
            sha256: download_sha256,
            download_url,
            index: job.options.image_index,
        },
        target,
        partition_plan: job.options.partition_plan,
        gho,
    })))
}

async fn report_job_progress(
    State(state): State<ApiState>,
    Path((device_id, job_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(progress): Json<AgentJobProgress>,
) -> Result<StatusCode, ApiError> {
    let device_id = authenticated_device(&state, &device_id, &headers)?;
    let job_id = Uuid::parse_str(&job_id).map_err(|_| ApiError::NotFound)?;
    let previous = state
        .jobs
        .list()
        .map_err(ApiError::Jobs)?
        .into_iter()
        .find(|job| job.id == job_id);
    let updated = state
        .jobs
        .report_progress(
            job_id,
            device_id,
            chrono::Duration::minutes(JOB_LEASE_MINUTES),
            progress,
        )
        .map_err(ApiError::Jobs)?;
    if previous.as_ref().and_then(|job| job.stage) != updated.stage {
        record_job_activity(
            &state,
            &updated,
            "job_stage_changed",
            ActivitySeverity::Info,
            None,
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobControlQuery {
    lease_id: Uuid,
}

async fn job_control_state(
    State(state): State<ApiState>,
    Path((device_id, job_id)): Path<(String, String)>,
    Query(query): Query<JobControlQuery>,
    headers: HeaderMap,
) -> Result<Json<JobState>, ApiError> {
    let device_id = authenticated_device(&state, &device_id, &headers)?;
    let job_id = Uuid::parse_str(&job_id).map_err(|_| ApiError::NotFound)?;
    let job_state = state
        .jobs
        .renew_control_lease(
            job_id,
            device_id,
            query.lease_id,
            chrono::Duration::minutes(JOB_LEASE_MINUTES),
        )
        .map_err(ApiError::Jobs)?;
    Ok(Json(job_state))
}

async fn complete_job(
    State(state): State<ApiState>,
    Path((device_id, job_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(completion): Json<AgentJobCompletion>,
) -> Result<StatusCode, ApiError> {
    let device_id = authenticated_device(&state, &device_id, &headers)?;
    let job_id = Uuid::parse_str(&job_id).map_err(|_| ApiError::NotFound)?;
    let updated = state
        .jobs
        .complete(
            job_id,
            device_id,
            completion.lease_id,
            completion.succeeded,
            completion.error_message,
        )
        .map_err(ApiError::Jobs)?;
    record_job_activity(
        &state,
        &updated,
        if completion.succeeded {
            "job_succeeded"
        } else {
            "job_failed"
        },
        if completion.succeeded {
            ActivitySeverity::Success
        } else {
            ActivitySeverity::Error
        },
        updated.error_message.clone(),
    );
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageDownloadQuery {
    lease_id: Uuid,
}

async fn download_job_image(
    State(state): State<ApiState>,
    Path((device_id, job_id)): Path<(String, String)>,
    Query(query): Query<ImageDownloadQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let device_id = authenticated_device(&state, &device_id, &headers)?;
    let job_id = Uuid::parse_str(&job_id).map_err(|_| ApiError::NotFound)?;
    let job = state
        .jobs
        .authorized_lease(job_id, device_id, query.lease_id)
        .map_err(ApiError::Jobs)?;
    let image = state
        .images
        .get(job.image_id)
        .map_err(ApiError::Images)?
        .ok_or(ApiError::NotFound)?;
    let (reader, length): (Box<dyn AsyncRead + Unpin + Send>, u64) =
        if job.operation == Operation::DeployGho {
            let prepared = state
                .images
                .prepare_gho_partition_deployment(job.image_id, job.options.image_index)
                .map_err(ApiError::Images)?;
            let primary = tokio::fs::File::open(&prepared.image.primary.canonical_path)
                .await
                .map_err(|_| ApiError::NotFound)?;
            let mut reader: Box<dyn AsyncRead + Unpin + Send> = Box::new(primary);
            for span in prepared.image.spans {
                let mut file = tokio::fs::File::open(&span.canonical_path)
                    .await
                    .map_err(|_| ApiError::NotFound)?;
                file.seek(std::io::SeekFrom::Start(512))
                    .await
                    .map_err(|_| ApiError::NotFound)?;
                reader = Box::new(reader.chain(file));
            }
            (reader, prepared.download_size_bytes)
        } else {
            let path = PathBuf::from(&image.source_path);
            let file = tokio::fs::File::open(&path)
                .await
                .map_err(|_| ApiError::NotFound)?;
            let length = file.metadata().await.map_err(|_| ApiError::NotFound)?.len();
            (Box::new(file), length)
        };
    let stream = tokio_util::io::ReaderStream::new(reader);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, length)
        .header(
            header::CONTENT_DISPOSITION,
            format!(
                "attachment; filename=\"image.{}\"",
                image_format_extension(image.format)
            ),
        )
        .body(Body::from_stream(stream))
        .map_err(|error| ApiError::Conflict(error.to_string()))
}

fn image_format_extension(format: easydeploymesh_core::ImageFormat) -> &'static str {
    match format {
        easydeploymesh_core::ImageFormat::Gho => "gho",
        easydeploymesh_core::ImageFormat::Wim => "wim",
        easydeploymesh_core::ImageFormat::Esd => "esd",
        easydeploymesh_core::ImageFormat::Swm => "swm",
        easydeploymesh_core::ImageFormat::Iso => "iso",
    }
}

#[derive(Deserialize)]
struct InstallerBootQuery {
    mac: String,
    arch: String,
    platform: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallerSessionQuery {
    token: String,
}

async fn installer_boot_script(
    State(state): State<ApiState>,
    Query(query): Query<InstallerBootQuery>,
) -> Response {
    let base_url = state.installer_base_url.clone();
    let installer = Arc::clone(&state.installer);
    let outcome = tokio::task::spawn_blocking(move || {
        installer.discover(&query.mac, &query.arch, &query.platform, &base_url)
    })
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or(BootOutcome::Denied);
    ipxe_response(render_boot_script(outcome))
}

async fn installer_user_data(
    State(state): State<ApiState>,
    Path((session_id, token)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let session_id = installer_session_id(&session_id)?;
    let installer = Arc::clone(&state.installer);
    let user_data =
        run_installer_blocking(move || installer.initial_user_data(session_id, &token)).await?;
    text_body_response("text/cloud-config; charset=utf-8", user_data)
}

async fn installer_meta_data(
    State(state): State<ApiState>,
    Path((session_id, token)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let session_id = installer_session_id(&session_id)?;
    let installer = Arc::clone(&state.installer);
    let meta_data =
        run_installer_blocking(move || installer.initial_meta_data(session_id, &token)).await?;
    text_body_response("text/plain; charset=utf-8", meta_data)
}

async fn installer_guard(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<LinuxInstallerGuardRequest>,
) -> Result<Response, ApiError> {
    let session_id = installer_session_id(&session_id)?;
    let token = installer_bearer_token(&headers)?.to_owned();
    let installer = Arc::clone(&state.installer);
    let authorization =
        run_installer_blocking(move || installer.authorize_guard(session_id, &token, request))
            .await?;
    record_job_activity(
        &state,
        &authorization.job,
        "linux_installer_handoff",
        ActivitySeverity::Info,
        None,
    );
    text_body_response(
        "text/cloud-config; charset=utf-8",
        authorization.autoinstall,
    )
}

async fn installer_kernel(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Query(query): Query<InstallerSessionQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    installer_media_response(
        state,
        session_id,
        query.token,
        headers,
        InstallerMediaKind::Kernel,
    )
    .await
}

async fn installer_initrd(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Query(query): Query<InstallerSessionQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    installer_media_response(
        state,
        session_id,
        query.token,
        headers,
        InstallerMediaKind::Initrd,
    )
    .await
}

async fn installer_iso(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Query(query): Query<InstallerSessionQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    installer_media_response(
        state,
        session_id,
        query.token,
        headers,
        InstallerMediaKind::Iso,
    )
    .await
}

async fn installer_media_response(
    state: ApiState,
    session_id: String,
    token: String,
    headers: HeaderMap,
    kind: InstallerMediaKind,
) -> Result<Response, ApiError> {
    let session_id = installer_session_id(&session_id)?;
    let installer = Arc::clone(&state.installer);
    let media = run_installer_blocking(move || installer.media(session_id, &token, kind)).await?;
    let requested_range = parse_single_range(headers.get(header::RANGE), media.size_bytes)?;
    let mut file = tokio::fs::File::open(&media.canonical_path)
        .await
        .map_err(|_| ApiError::Installer(InstallerDeploymentError::MediaIntegrity))?;
    let actual_size = file
        .metadata()
        .await
        .map_err(|_| ApiError::Installer(InstallerDeploymentError::MediaIntegrity))?
        .len();
    if actual_size != media.size_bytes {
        return Err(ApiError::Installer(
            InstallerDeploymentError::MediaIntegrity,
        ));
    }
    let (status, start, end) = requested_range
        .map_or((StatusCode::OK, 0, media.size_bytes - 1), |(start, end)| {
            (StatusCode::PARTIAL_CONTENT, start, end)
        });
    if start != 0 {
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|_| ApiError::Installer(InstallerDeploymentError::MediaIntegrity))?;
    }
    let length = end - start + 1;
    let stream = tokio_util::io::ReaderStream::new(file.take(length));
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, media.content_type)
        .header(header::CONTENT_LENGTH, length)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::ETAG, format!("\"{}\"", media.sha256))
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", media.file_name),
        );
    if status == StatusCode::PARTIAL_CONTENT {
        response = response.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{}", media.size_bytes),
        );
    }
    response
        .body(Body::from_stream(stream))
        .map_err(|_| ApiError::InstallerTask)
}

async fn installer_event(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<InstallerEventRequest>,
) -> Result<StatusCode, ApiError> {
    let session_id = installer_session_id(&session_id)?;
    let token = installer_bearer_token(&headers)?.to_owned();
    let installer = Arc::clone(&state.installer);
    let updated =
        run_installer_blocking(move || installer.report_event(session_id, &token, request)).await?;
    record_job_activity(
        &state,
        &updated,
        if updated.state == JobState::Failed {
            "linux_installer_failed"
        } else {
            "linux_installer_event"
        },
        if updated.state == JobState::Failed {
            ActivitySeverity::Error
        } else {
            ActivitySeverity::Info
        },
        updated.error_message.clone(),
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn installer_first_boot(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<FirstBootRequest>,
) -> Result<StatusCode, ApiError> {
    let session_id = installer_session_id(&session_id)?;
    let token = installer_bearer_token(&headers)?.to_owned();
    let installer = Arc::clone(&state.installer);
    let updated = run_installer_blocking(move || {
        installer.complete_first_boot(session_id, &token, request.attempt_id)
    })
    .await?;
    if let Some(updated) = updated {
        record_job_activity(
            &state,
            &updated,
            "job_succeeded",
            ActivitySeverity::Success,
            None,
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn run_installer_blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, InstallerDeploymentError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| ApiError::InstallerTask)?
        .map_err(ApiError::Installer)
}

fn installer_session_id(value: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(value).map_err(|_| ApiError::NotFound)
}

fn installer_bearer_token(headers: &HeaderMap) -> Result<&str, ApiError> {
    bearer_token(headers).ok_or(ApiError::Installer(InstallerDeploymentError::Unauthorized))
}

fn ipxe_response(script: String) -> Response {
    text_body_response("text/plain; charset=utf-8", script)
        .unwrap_or_else(|error| error.into_response())
}

fn text_body_response(content_type: &'static str, body: String) -> Result<Response, ApiError> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .map_err(|_| ApiError::InstallerTask)
}

fn parse_single_range(
    header_value: Option<&axum::http::HeaderValue>,
    total_length: u64,
) -> Result<Option<(u64, u64)>, ApiError> {
    let Some(header_value) = header_value else {
        return Ok(None);
    };
    let value = header_value
        .to_str()
        .ok()
        .filter(|value| value.len() <= 128)
        .and_then(|value| value.strip_prefix("bytes="))
        .filter(|value| !value.contains(','))
        .ok_or(ApiError::RangeNotSatisfiable(total_length))?;
    let (start, end) = value
        .split_once('-')
        .ok_or(ApiError::RangeNotSatisfiable(total_length))?;
    if start.is_empty() {
        let suffix_length =
            parse_range_number(end).ok_or(ApiError::RangeNotSatisfiable(total_length))?;
        if suffix_length == 0 || total_length == 0 {
            return Err(ApiError::RangeNotSatisfiable(total_length));
        }
        return Ok(Some((
            total_length.saturating_sub(suffix_length.min(total_length)),
            total_length - 1,
        )));
    }
    let start = parse_range_number(start).ok_or(ApiError::RangeNotSatisfiable(total_length))?;
    if start >= total_length {
        return Err(ApiError::RangeNotSatisfiable(total_length));
    }
    let end = if end.is_empty() {
        total_length - 1
    } else {
        parse_range_number(end)
            .ok_or(ApiError::RangeNotSatisfiable(total_length))?
            .min(total_length - 1)
    };
    if end < start {
        return Err(ApiError::RangeNotSatisfiable(total_length));
    }
    Ok(Some((start, end)))
}

fn parse_range_number(value: &str) -> Option<u64> {
    if value.is_empty() || value.len() > 20 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn authenticated_device(
    state: &ApiState,
    device_id: &str,
    headers: &HeaderMap,
) -> Result<Uuid, ApiError> {
    let device_id = Uuid::parse_str(device_id).map_err(|_| ApiError::NotFound)?;
    let token = bearer_token(headers).ok_or(ApiError::Unauthorized)?;
    state
        .registry
        .authenticate(device_id, token)
        .map_err(ApiError::Registry)?;
    Ok(device_id)
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
}

fn record_device_activity(
    state: &ApiState,
    registered: &easydeploymesh_core::RegisteredDevice,
    kind: &str,
    severity: ActivitySeverity,
    raw: Option<String>,
) {
    let device = &registered.device;
    let name = device
        .hostname
        .clone()
        .unwrap_or_else(|| device.mac_address.clone());
    let mut details = serde_json::Map::new();
    details.insert("macAddress".into(), device.mac_address.clone().into());
    details.insert("ipAddress".into(), device.ip_address.clone().into());
    details.insert(
        "agentVersion".into(),
        registered.agent_version.clone().into(),
    );
    let _ = state.activities.record(
        ActivitySource::Device,
        kind,
        severity,
        Some(ActivitySubject {
            id: device.id.to_string(),
            name,
        }),
        details,
        raw,
    );
}

fn record_job_activity(
    state: &ApiState,
    job: &easydeploymesh_core::DeploymentJob,
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
    let _ = state.activities.record(
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

enum ApiError {
    Unauthorized,
    NotFound,
    Registry(DeviceRegistryError),
    Jobs(JobRepositoryError),
    Images(ImageLibraryError),
    Conflict(String),
    Installer(InstallerDeploymentError),
    InstallerTask,
    RangeNotSatisfiable(u64),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let unsatisfied_length = match &self {
            Self::RangeNotSatisfiable(length) => Some(*length),
            _ => None,
        };
        let (status, message) = match self {
            Self::Unauthorized | Self::Registry(DeviceRegistryError::Unauthorized) => {
                (StatusCode::UNAUTHORIZED, "authentication failed".to_owned())
            }
            Self::NotFound | Self::Registry(DeviceRegistryError::NotFound(_)) => {
                (StatusCode::NOT_FOUND, "device was not found".to_owned())
            }
            Self::Jobs(JobRepositoryError::NotFound(_)) => (
                StatusCode::NOT_FOUND,
                "deployment resource was not found".to_owned(),
            ),
            Self::Jobs(JobRepositoryError::InvalidLease)
            | Self::Jobs(JobRepositoryError::WrongDevice(_)) => (
                StatusCode::FORBIDDEN,
                "deployment lease is invalid".to_owned(),
            ),
            Self::Jobs(error) => (StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
            Self::Images(error) => (StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
            Self::Conflict(message) => (StatusCode::CONFLICT, message),
            Self::Registry(error) => (StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
            Self::Installer(InstallerDeploymentError::Unauthorized) => (
                StatusCode::UNAUTHORIZED,
                "installer session authentication failed".to_owned(),
            ),
            Self::Installer(InstallerDeploymentError::Expired) => {
                (StatusCode::GONE, "installer session expired".to_owned())
            }
            Self::Installer(
                InstallerDeploymentError::AuthorizationInProgress
                | InstallerDeploymentError::InvalidSessionState,
            ) => (
                StatusCode::CONFLICT,
                "installer session is not ready for this request".to_owned(),
            ),
            Self::Installer(
                InstallerDeploymentError::InvalidRequest
                | InstallerDeploymentError::TargetDiskMismatch,
            ) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "installer safety validation failed".to_owned(),
            ),
            Self::Installer(InstallerDeploymentError::MediaIntegrity) => (
                StatusCode::CONFLICT,
                "installer media integrity validation failed".to_owned(),
            ),
            Self::Installer(
                InstallerDeploymentError::SessionCapacity
                | InstallerDeploymentError::SessionState
                | InstallerDeploymentError::Registry(_)
                | InstallerDeploymentError::Jobs(_)
                | InstallerDeploymentError::Configuration,
            )
            | Self::InstallerTask => (
                StatusCode::SERVICE_UNAVAILABLE,
                "installer service is unavailable".to_owned(),
            ),
            Self::RangeNotSatisfiable(_) => (
                StatusCode::RANGE_NOT_SATISFIABLE,
                "requested byte range is not satisfiable".to_owned(),
            ),
        };

        #[derive(Serialize)]
        struct ErrorBody {
            error: String,
        }

        let mut response = (status, Json(ErrorBody { error: message })).into_response();
        if let Some(length) = unsatisfied_length {
            if let Ok(value) = format!("bytes */{length}").parse() {
                response.headers_mut().insert(header::CONTENT_RANGE, value);
            }
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use easydeploymesh_core::{
        AgentJobCompletion, AgentJobLease, AgentJobProgress, Architecture, BootMode,
        CreateDeploymentJob, DeploymentOptions, DeploymentStage, DeploymentTarget, Disk, JobState,
        LinuxInstallOptions, LinuxInstallerGuardRequest, LinuxInstallerObservedDisk, Operation,
        PartitionPlan,
    };
    use hadris_iso::{
        joliet::JolietLevel,
        read::PathSeparator,
        write::{
            InputTree, IsoImageWriter,
            options::{BaseIsoLevel, CreationFeatures, IsoFormatOptions},
        },
    };

    fn disk() -> Disk {
        Disk {
            id: r"\\.\PhysicalDrive0".to_owned(),
            model: "Test disk".to_owned(),
            serial: Some("DISK-SERIAL".to_owned()),
            size_bytes: 64 * 1024 * 1024 * 1024,
            is_system: false,
        }
    }

    fn inventory() -> AgentInventory {
        AgentInventory {
            hostname: Some("winpe-client".to_owned()),
            mac_address: "02:00:00:00:00:01".to_owned(),
            model: None,
            serial: None,
            cpu_model: Some("Test CPU".to_owned()),
            physical_core_count: Some(4),
            logical_processor_count: 8,
            memory_bytes: 8 * 1024 * 1024 * 1024,
            system_details: Default::default(),
            architecture: Architecture::X86_64,
            boot_mode: BootMode::Uefi,
            disks: vec![disk()],
            agent_version: "0.1.0".to_owned(),
        }
    }

    #[tokio::test]
    async fn dynamic_boot_distinguishes_no_assignment_from_a_refused_linux_assignment() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let registry = Arc::new(DeviceRegistry::open(temp.path()).expect("registry should open"));
        let jobs =
            Arc::new(JobRepository::open(temp.path().join("jobs")).expect("jobs should open"));
        let images =
            Arc::new(ImageLibrary::open(temp.path().join("images")).expect("images should open"));
        let (registered, registration) = registry
            .register(
                inventory(),
                "192.0.2.20".parse().expect("test IP should parse"),
            )
            .expect("device should register");
        let control_plane = ControlPlane::new(
            Arc::clone(&registry),
            Arc::clone(&jobs),
            images,
            Arc::new(ActivityRepository::open(temp.path().join("activities.json")).unwrap()),
        );
        let status = control_plane.start("127.0.0.1", 0).await.unwrap();
        let endpoint = status.endpoint.expect("endpoint should be available");
        let client = reqwest::Client::new();
        let boot_url = format!(
            "{endpoint}/api/v1/install/boot.ipxe?mac={}&arch=x86_64&platform=efi",
            registered.device.mac_address
        );

        let no_assignment = client
            .get(&boot_url)
            .send()
            .await
            .expect("boot request should complete")
            .error_for_status()
            .expect("boot request should be accepted")
            .text()
            .await
            .expect("boot script should decode");
        assert!(no_assignment.contains("easydeploymesh-winpe.ipxe"));

        let queued = jobs
            .enqueue(CreateDeploymentJob {
                name: "Refused Linux assignment".to_owned(),
                operation: Operation::InstallLinux,
                image_id: Uuid::new_v4(),
                targets: vec![DeploymentTarget {
                    device_id: registered.device.id,
                    target_disk_id: disk().id,
                    target_disk_model: disk().model,
                    target_disk_serial: disk().serial,
                    target_disk_size_bytes: disk().size_bytes,
                }],
                options: DeploymentOptions {
                    linux_install: Some(LinuxInstallOptions {
                        hostname: "lab-linux-01".to_owned(),
                        username: "operator".to_owned(),
                        ssh_authorized_keys: vec![
                            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITestControlPlaneKey".to_owned(),
                        ],
                    }),
                    ..DeploymentOptions::default()
                },
            })
            .expect("Linux job should enqueue");

        let agent_claim = client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/jobs/claim",
                registered.device.id
            ))
            .bearer_auth(&registration.device_token)
            .send()
            .await
            .expect("Agent claim should complete")
            .error_for_status()
            .expect("Agent claim should be accepted")
            .json::<Option<AgentJobLease>>()
            .await
            .expect("Agent claim should decode");
        assert!(agent_claim.is_none());

        let denied = client
            .get(&boot_url)
            .send()
            .await
            .expect("boot request should complete")
            .error_for_status()
            .expect("denial script should be returned as iPXE")
            .text()
            .await
            .expect("denial script should decode");
        assert!(denied.contains("assignment denied"));
        assert!(!denied.contains("easydeploymesh-winpe.ipxe"));
        assert_eq!(
            jobs.list()
                .expect("jobs should list")
                .into_iter()
                .find(|job| job.id == queued.id)
                .expect("job should remain")
                .state,
            JobState::Waiting
        );
        control_plane.stop().await.unwrap();
    }

    #[test]
    fn installer_range_parser_accepts_one_bounded_range_and_rejects_ambiguity() {
        let explicit = axum::http::HeaderValue::from_static("bytes=10-19");
        assert!(matches!(
            parse_single_range(Some(&explicit), 100),
            Ok(Some((10, 19)))
        ));
        let suffix = axum::http::HeaderValue::from_static("bytes=-5");
        assert!(matches!(
            parse_single_range(Some(&suffix), 100),
            Ok(Some((95, 99)))
        ));
        let multiple = axum::http::HeaderValue::from_static("bytes=0-1,10-11");
        assert!(matches!(
            parse_single_range(Some(&multiple), 100),
            Err(ApiError::RangeNotSatisfiable(100))
        ));
        let overflow = axum::http::HeaderValue::from_static("bytes=100-");
        assert!(matches!(
            parse_single_range(Some(&overflow), 100),
            Err(ApiError::RangeNotSatisfiable(100))
        ));
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

    fn create_ubuntu_iso(path: &std::path::Path) {
        const BOOT_ASSET_BYTES: usize = 1024 * 1024;
        let source = path
            .parent()
            .expect("fixture ISO should have a parent")
            .join(format!("control-plane-iso-source-{}", Uuid::new_v4()));
        std::fs::create_dir_all(source.join(".disk")).unwrap();
        std::fs::create_dir_all(source.join("casper")).unwrap();
        std::fs::write(
            source.join(".disk/info"),
            "Ubuntu-Server 24.04.3 LTS \"Noble Numbat\" - Release amd64 (20250805)",
        )
        .unwrap();
        let mut kernel = vec![0x5a_u8; BOOT_ASSET_BYTES];
        kernel[0x1fe..0x200].copy_from_slice(&[0x55, 0xaa]);
        kernel[0x202..0x206].copy_from_slice(b"HdrS");
        kernel[0x206..0x208].copy_from_slice(&0x020b_u16.to_le_bytes());
        std::fs::write(source.join("casper/vmlinuz"), kernel).unwrap();
        std::fs::write(
            source.join("casper/initrd"),
            vec![0xa5_u8; BOOT_ASSET_BYTES],
        )
        .unwrap();

        let input = InputTree::from_fs(&source, PathSeparator::ForwardSlash).unwrap();
        let options = IsoFormatOptions {
            volume_name: "UBUNTU_SERVER_2404".to_owned(),
            system_id: None,
            volume_set_id: None,
            publisher_id: Some("Canonical".to_owned()),
            preparer_id: Some("EasyDeployMesh tests".to_owned()),
            application_id: Some("Ubuntu Server test media".to_owned()),
            sector_size: 2048,
            path_separator: PathSeparator::ForwardSlash,
            features: CreationFeatures {
                filenames: BaseIsoLevel::Level2 {
                    supports_lowercase: true,
                    supports_rrip: false,
                },
                long_filenames: false,
                joliet: Some(JolietLevel::Level3),
                rock_ridge: None,
                el_torito: None,
                hybrid_boot: None,
            },
            strict_charset: false,
        };
        let output = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        IsoImageWriter::create(output, input, options).unwrap();
    }

    #[tokio::test]
    async fn linux_installer_session_guards_storage_and_requires_matching_first_boot_attempt() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let registry = Arc::new(DeviceRegistry::open(temp.path()).expect("registry should open"));
        let jobs =
            Arc::new(JobRepository::open(temp.path().join("jobs")).expect("jobs should open"));
        let images =
            Arc::new(ImageLibrary::open(temp.path().join("images")).expect("images should open"));
        let (registered, _) = registry
            .register(
                inventory(),
                "192.0.2.20".parse().expect("test IP should parse"),
            )
            .expect("device should register");
        let iso_path = temp.path().join("ubuntu-server.iso");
        create_ubuntu_iso(&iso_path);
        let image = images.import(&iso_path).expect("Ubuntu ISO should import");
        let queued = jobs
            .enqueue(CreateDeploymentJob {
                name: "Guarded Ubuntu deployment".to_owned(),
                operation: Operation::InstallLinux,
                image_id: image.id,
                targets: vec![DeploymentTarget {
                    device_id: registered.device.id,
                    target_disk_id: disk().id,
                    target_disk_model: disk().model,
                    target_disk_serial: disk().serial,
                    target_disk_size_bytes: disk().size_bytes,
                }],
                options: DeploymentOptions {
                    linux_install: Some(LinuxInstallOptions {
                        hostname: "lab-linux-01".to_owned(),
                        username: "operator".to_owned(),
                        ssh_authorized_keys: vec![
                            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITestInstallerSessionKey"
                                .to_owned(),
                        ],
                    }),
                    ..DeploymentOptions::default()
                },
            })
            .expect("Linux job should enqueue");
        let control_plane = ControlPlane::new(
            registry,
            Arc::clone(&jobs),
            images,
            Arc::new(ActivityRepository::open(temp.path().join("activities.json")).unwrap()),
        );
        let status = control_plane.start("127.0.0.1", 0).await.unwrap();
        let endpoint = status.endpoint.expect("endpoint should be available");
        let client = reqwest::Client::new();
        let script = client
            .get(format!(
                "{endpoint}/api/v1/install/boot.ipxe?mac={}&arch=x86_64&platform=efi",
                registered.device.mac_address
            ))
            .header(reqwest::header::HOST, "attacker.invalid:9999")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap();
        let script_value = |name: &str| {
            script
                .lines()
                .find_map(|line| line.strip_prefix(&format!("set {name} ")))
                .expect("assignment variable should exist")
                .to_owned()
        };
        let session_url = script_value("edm-session");
        let attempt_id = Uuid::parse_str(&script_value("edm-attempt")).unwrap();
        let token = script_value("edm-token");
        assert!(session_url.starts_with(&endpoint));
        assert!(!script.contains("attacker.invalid"));
        assert!(script.contains("boot=casper"));
        assert!(script.contains("/seed/${edm-token}/"));

        let seed = client
            .get(format!("{session_url}/seed/{token}/user-data"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(seed.contains("early-commands"));
        assert!(
            !seed
                .lines()
                .any(|line| line.trim_start().starts_with("storage:"))
        );

        let ranged_iso = client
            .get(format!("{session_url}/iso?token={token}"))
            .header(reqwest::header::RANGE, "bytes=0-31")
            .send()
            .await
            .unwrap();
        assert_eq!(ranged_iso.status(), reqwest::StatusCode::PARTIAL_CONTENT);
        assert_eq!(ranged_iso.headers()[reqwest::header::CONTENT_LENGTH], "32");
        assert_eq!(ranged_iso.bytes().await.unwrap().len(), 32);

        let autoinstall = client
            .post(format!("{session_url}/guard"))
            .bearer_auth(&token)
            .json(&LinuxInstallerGuardRequest {
                image_sha256: image.sha256.expect("ISO checksum should exist"),
                disks: vec![LinuxInstallerObservedDisk {
                    path: "/dev/nvme0n1".to_owned(),
                    model: disk().model,
                    serial: disk().serial,
                    size_bytes: disk().size_bytes,
                }],
            })
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(autoinstall.contains("path: \"/dev/nvme0n1\""));
        assert!(autoinstall.contains("shutdown: reboot"));
        assert_eq!(
            jobs.list()
                .unwrap()
                .into_iter()
                .find(|job| job.id == queued.id)
                .unwrap()
                .state,
            JobState::Running
        );

        client
            .post(format!("{session_url}/events"))
            .bearer_auth(&token)
            .json(&serde_json::json!({"kind": "awaiting_first_boot"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .expect("awaiting event should be accepted");
        let mismatch = client
            .post(format!("{session_url}/first-boot"))
            .bearer_auth(&token)
            .json(&serde_json::json!({"attemptId": Uuid::new_v4()}))
            .send()
            .await
            .unwrap();
        assert_eq!(mismatch.status(), reqwest::StatusCode::CONFLICT);
        assert_eq!(
            jobs.list().unwrap()[0].state,
            JobState::Running,
            "a mismatched attempt must not complete the job"
        );

        client
            .post(format!("{session_url}/first-boot"))
            .bearer_auth(&token)
            .json(&serde_json::json!({"attemptId": attempt_id}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .expect("matching first boot should complete");
        let completed = jobs.list().unwrap().remove(0);
        assert_eq!(completed.state, JobState::Succeeded);
        assert_eq!(completed.stage, Some(DeploymentStage::Finalizing));
        control_plane.stop().await.unwrap();
    }

    fn gho_fixture(partitions: &[&[u8]]) -> Vec<u8> {
        let mut contents = vec![0_u8; 512];
        contents[0..8].copy_from_slice(&[0xfe, 0xef, 1, 0, 1, 2, 3, 4]);
        for partition in partitions {
            contents.extend_from_slice(&0x0603_u32.to_le_bytes());
            contents.extend_from_slice(&0x012f_18d8_u32.to_le_bytes());
            contents.extend_from_slice(&20_u16.to_le_bytes());
            contents.extend_from_slice(&[0; 20]);
            let mut header = vec![0_u8; 512];
            header[0..2].copy_from_slice(&0xeffe_u16.to_le_bytes());
            contents.extend(header);
            contents.extend_from_slice(&(partition.len() as u16 + 2).to_le_bytes());
            contents.extend_from_slice(partition);
        }
        contents.extend_from_slice(&0x0023_u32.to_le_bytes());
        contents.extend_from_slice(&0x012f_18d8_u32.to_le_bytes());
        contents.extend_from_slice(&0_u16.to_le_bytes());
        contents
    }

    #[tokio::test]
    async fn gho_lease_and_download_bind_the_selected_partition_and_normalized_spans() {
        use sha2::{Digest, Sha256};

        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let registry = Arc::new(DeviceRegistry::open(temp.path()).expect("registry should open"));
        let jobs =
            Arc::new(JobRepository::open(temp.path().join("jobs")).expect("jobs should open"));
        let images =
            Arc::new(ImageLibrary::open(temp.path().join("images")).expect("images should open"));
        let control_plane = ControlPlane::new(
            Arc::clone(&registry),
            Arc::clone(&jobs),
            Arc::clone(&images),
            Arc::new(ActivityRepository::open(temp.path().join("activities.json")).unwrap()),
        );
        let status = control_plane.start("127.0.0.1", 0).await.unwrap();
        let endpoint = status.endpoint.unwrap();
        let client = reqwest::Client::new();
        let registration = client
            .post(format!("{endpoint}/api/v1/agents/register"))
            .bearer_auth(status.enrollment_token.unwrap())
            .json(&inventory())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json::<AgentRegistration>()
            .await
            .unwrap();

        let normalized = gho_fixture(&[b"\xebR\x90NTFS    reserved", b"\xebR\x90NTFS    windows"]);
        let split_at = 700;
        let primary = temp.path().join("disk.gho");
        std::fs::write(&primary, &normalized[..split_at]).unwrap();
        let mut span = vec![0_u8; 512];
        span[0..8].copy_from_slice(&[0xfe, 0xef, 9, 0, 1, 2, 3, 4]);
        span.extend_from_slice(&normalized[split_at..]);
        std::fs::write(temp.path().join("disk001.ghs"), span).unwrap();
        let image = images.import(&primary).expect("spanned GHO should import");
        let job = jobs
            .create(CreateDeploymentJob {
                name: "Spanned Ghost deployment".to_owned(),
                operation: Operation::DeployGho,
                image_id: image.id,
                targets: vec![DeploymentTarget {
                    device_id: registration.device_id,
                    target_disk_id: disk().id,
                    target_disk_model: disk().model,
                    target_disk_serial: disk().serial,
                    target_disk_size_bytes: disk().size_bytes,
                }],
                options: DeploymentOptions {
                    image_index: 2,
                    partition_plan: PartitionPlan::uefi_gpt(),
                    linux_install: None,
                },
            })
            .unwrap();
        jobs.transition(job.id, JobState::Waiting).unwrap();
        let lease = client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/jobs/claim",
                registration.device_id
            ))
            .bearer_auth(&registration.device_token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json::<Option<AgentJobLease>>()
            .await
            .unwrap()
            .expect("GHO job should lease");
        assert_eq!(lease.gho.as_ref().unwrap().source_partition, 2);
        assert_eq!(lease.image.size_bytes, normalized.len() as u64);
        assert_eq!(
            lease.image.sha256,
            format!("{:x}", Sha256::digest(&normalized))
        );

        let downloaded = client
            .get(format!("{endpoint}{}", lease.image.download_url))
            .bearer_auth(&registration.device_token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(downloaded.as_ref(), normalized);
        control_plane.stop().await.unwrap();
    }

    #[tokio::test]
    async fn destructive_job_waits_until_heartbeat_reports_a_disk() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let registry = Arc::new(DeviceRegistry::open(temp.path()).expect("registry should open"));
        let jobs =
            Arc::new(JobRepository::open(temp.path().join("jobs")).expect("jobs should open"));
        let images =
            Arc::new(ImageLibrary::open(temp.path().join("images")).expect("images should open"));
        let control_plane = ControlPlane::new(
            Arc::clone(&registry),
            Arc::clone(&jobs),
            Arc::clone(&images),
            Arc::new(
                ActivityRepository::open(temp.path().join("activities.json"))
                    .expect("activities should open"),
            ),
        );
        let status = control_plane
            .start("127.0.0.1", 0)
            .await
            .expect("control service should start");
        let endpoint = status.endpoint.expect("endpoint should be available");
        let enrollment_token = status
            .enrollment_token
            .expect("enrollment token should be available");
        let client = reqwest::Client::new();
        let mut empty_inventory = inventory();
        empty_inventory.disks.clear();
        let registration = client
            .post(format!("{endpoint}/api/v1/agents/register"))
            .bearer_auth(enrollment_token)
            .json(&empty_inventory)
            .send()
            .await
            .expect("registration should complete")
            .error_for_status()
            .expect("registration should be accepted")
            .json::<AgentRegistration>()
            .await
            .expect("registration response should decode");

        let image_path = temp.path().join("windows.wim");
        std::fs::write(&image_path, wim_fixture(b"easydeploymesh-test-image"))
            .expect("image should write");
        let image = images.import(&image_path).expect("image should import");
        let job = jobs
            .create(CreateDeploymentJob {
                name: "Disk-gated Windows deployment".to_owned(),
                operation: Operation::DeployWim,
                image_id: image.id,
                targets: vec![DeploymentTarget {
                    device_id: registration.device_id,
                    target_disk_id: r"\\.\PhysicalDrive0".to_owned(),
                    target_disk_model: "Test disk".to_owned(),
                    target_disk_serial: Some("DISK-SERIAL".to_owned()),
                    target_disk_size_bytes: 64 * 1024 * 1024 * 1024,
                }],
                options: DeploymentOptions::default(),
            })
            .expect("job should create");
        jobs.transition(job.id, JobState::Waiting)
            .expect("job should queue");

        let empty_claim = client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/jobs/claim",
                registration.device_id
            ))
            .bearer_auth(&registration.device_token)
            .send()
            .await
            .expect("claim should complete")
            .error_for_status()
            .expect("claim should be accepted")
            .json::<Option<AgentJobLease>>()
            .await
            .expect("claim response should decode");
        assert!(empty_claim.is_none());
        assert_eq!(
            jobs.list().expect("jobs should list")[0].state,
            JobState::Waiting
        );

        let expired = jobs
            .lease_for_device(registration.device_id, chrono::Duration::seconds(-1))
            .expect("test setup lease should succeed")
            .expect("waiting job should be available");
        let expired_lease_id = expired.lease_id.expect("expired lease id should exist");
        let empty_reclaim = client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/jobs/claim",
                registration.device_id
            ))
            .bearer_auth(&registration.device_token)
            .send()
            .await
            .expect("reclaim should complete")
            .error_for_status()
            .expect("reclaim should be accepted")
            .json::<Option<AgentJobLease>>()
            .await
            .expect("reclaim response should decode");
        assert!(empty_reclaim.is_none());
        let stored_expired = &jobs.list().expect("jobs should list")[0];
        assert_eq!(stored_expired.state, JobState::Running);
        assert_eq!(stored_expired.lease_id, Some(expired_lease_id));

        client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/heartbeat",
                registration.device_id
            ))
            .bearer_auth(&registration.device_token)
            .json(&AgentHeartbeat {
                inventory: inventory(),
            })
            .send()
            .await
            .expect("heartbeat should complete")
            .error_for_status()
            .expect("heartbeat should be accepted");
        let disk_claim = client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/jobs/claim",
                registration.device_id
            ))
            .bearer_auth(&registration.device_token)
            .send()
            .await
            .expect("claim should complete")
            .error_for_status()
            .expect("claim should be accepted")
            .json::<Option<AgentJobLease>>()
            .await
            .expect("claim response should decode")
            .expect("job should lease after a disk heartbeat");
        assert_eq!(disk_claim.job_id, job.id);
        assert_ne!(disk_claim.lease_id, expired_lease_id);

        control_plane
            .stop()
            .await
            .expect("control service should stop");
    }

    /* Native GHO lease behavior is covered by image-library verification tests. */
    #[allow(dead_code)]
    async fn removed_legacy_gho_executor_test() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let registry = Arc::new(DeviceRegistry::open(temp.path()).expect("registry should open"));
        let jobs =
            Arc::new(JobRepository::open(temp.path().join("jobs")).expect("jobs should open"));
        let images =
            Arc::new(ImageLibrary::open(temp.path().join("images")).expect("images should open"));
        let control_plane = ControlPlane::new(
            Arc::clone(&registry),
            Arc::clone(&jobs),
            Arc::clone(&images),
            Arc::new(
                ActivityRepository::open(temp.path().join("activities.json"))
                    .expect("activities should open"),
            ),
        );
        let status = control_plane
            .start("127.0.0.1", 0)
            .await
            .expect("control service should start");
        let endpoint = status.endpoint.expect("endpoint should be available");
        let enrollment_token = status
            .enrollment_token
            .expect("enrollment token should be available");
        let client = reqwest::Client::new();
        let registration = client
            .post(format!("{endpoint}/api/v1/agents/register"))
            .bearer_auth(enrollment_token)
            .json(&inventory())
            .send()
            .await
            .expect("registration should complete")
            .error_for_status()
            .expect("registration should be accepted")
            .json::<AgentRegistration>()
            .await
            .expect("registration response should decode");

        let target = DeploymentTarget {
            device_id: registration.device_id,
            target_disk_id: r"\\.\PhysicalDrive0".to_owned(),
            target_disk_model: "Test disk".to_owned(),
            target_disk_serial: Some("DISK-SERIAL".to_owned()),
            target_disk_size_bytes: 64 * 1024 * 1024 * 1024,
        };
        let gho_path = temp.path().join("unsupported.gho");
        std::fs::write(&gho_path, b"unsupported-ghost-image").expect("GHO image should write");
        let gho_image = images.import(&gho_path).expect("GHO image should import");
        let gho_job = jobs
            .create(CreateDeploymentJob {
                name: "Unsupported Ghost deployment".to_owned(),
                operation: Operation::DeployGho,
                image_id: gho_image.id,
                targets: vec![target.clone()],
                options: DeploymentOptions::default(),
            })
            .expect("GHO job should create");
        jobs.transition(gho_job.id, JobState::Waiting)
            .expect("GHO job should queue");

        let unsupported_claim = client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/jobs/claim",
                registration.device_id
            ))
            .bearer_auth(&registration.device_token)
            .send()
            .await
            .expect("claim should complete")
            .error_for_status()
            .expect("claim should be accepted")
            .json::<Option<AgentJobLease>>()
            .await
            .expect("claim response should decode");
        assert!(unsupported_claim.is_none());
        assert_eq!(
            jobs.list()
                .expect("jobs should list")
                .into_iter()
                .find(|job| job.id == gho_job.id)
                .expect("GHO job should remain stored")
                .state,
            JobState::Waiting
        );

        let expired_gho = jobs
            .lease_for_device(registration.device_id, chrono::Duration::seconds(-1))
            .expect("test setup lease should succeed")
            .expect("waiting GHO job should be available");
        let expired_gho_lease_id = expired_gho
            .lease_id
            .expect("expired GHO lease id should exist");
        let unsupported_reclaim = client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/jobs/claim",
                registration.device_id
            ))
            .bearer_auth(&registration.device_token)
            .send()
            .await
            .expect("GHO reclaim should complete")
            .error_for_status()
            .expect("GHO reclaim should be accepted")
            .json::<Option<AgentJobLease>>()
            .await
            .expect("GHO reclaim response should decode");
        assert!(unsupported_reclaim.is_none());
        let stored_gho = jobs
            .list()
            .expect("jobs should list")
            .into_iter()
            .find(|job| job.id == gho_job.id)
            .expect("GHO job should remain stored");
        assert_eq!(stored_gho.state, JobState::Running);
        assert_eq!(stored_gho.lease_id, Some(expired_gho_lease_id));
        jobs.transition(gho_job.id, JobState::Cancelled)
            .expect("unsupported GHO job should cancel before the next deployment");

        let wim_path = temp.path().join("supported.wim");
        std::fs::write(&wim_path, wim_fixture(b"supported-windows-image"))
            .expect("WIM image should write");
        let wim_image = images.import(&wim_path).expect("WIM image should import");
        let invalid_index_options = DeploymentOptions {
            image_index: 2,
            ..DeploymentOptions::default()
        };
        let invalid_index_job = jobs
            .create(CreateDeploymentJob {
                name: "Invalid image-index deployment".to_owned(),
                operation: Operation::DeployWim,
                image_id: wim_image.id,
                targets: vec![target.clone()],
                options: invalid_index_options,
            })
            .expect("invalid-index job should create");
        jobs.transition(invalid_index_job.id, JobState::Waiting)
            .expect("invalid-index job should queue");
        let invalid_index_claim = client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/jobs/claim",
                registration.device_id
            ))
            .bearer_auth(&registration.device_token)
            .send()
            .await
            .expect("invalid-index claim should complete")
            .error_for_status()
            .expect("invalid-index claim should be accepted")
            .json::<Option<AgentJobLease>>()
            .await
            .expect("invalid-index claim response should decode");
        assert!(invalid_index_claim.is_none());
        jobs.transition(invalid_index_job.id, JobState::Cancelled)
            .expect("invalid-index job should cancel before the next deployment");

        let tampered_path = temp.path().join("tampered.wim");
        std::fs::write(&tampered_path, wim_fixture(b"tampered-windows-image"))
            .expect("tampered WIM fixture should write");
        let tampered_image = images
            .import(&tampered_path)
            .expect("tampered image should initially import");
        let mut tampered_contents =
            std::fs::read(&tampered_image.source_path).expect("managed image should be readable");
        *tampered_contents
            .last_mut()
            .expect("managed image should not be empty") ^= 0xff;
        std::fs::write(&tampered_image.source_path, tampered_contents)
            .expect("managed image should be deliberately tampered");
        let tampered_job = jobs
            .create(CreateDeploymentJob {
                name: "Tampered image deployment".to_owned(),
                operation: Operation::DeployWim,
                image_id: tampered_image.id,
                targets: vec![target.clone()],
                options: DeploymentOptions::default(),
            })
            .expect("tampered-image job should create");
        jobs.transition(tampered_job.id, JobState::Waiting)
            .expect("tampered-image job should queue");
        let tampered_claim = client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/jobs/claim",
                registration.device_id
            ))
            .bearer_auth(&registration.device_token)
            .send()
            .await
            .expect("tampered-image claim should complete")
            .error_for_status()
            .expect("tampered-image claim should be accepted")
            .json::<Option<AgentJobLease>>()
            .await
            .expect("tampered-image claim response should decode");
        assert!(tampered_claim.is_none());
        jobs.transition(tampered_job.id, JobState::Cancelled)
            .expect("tampered-image job should cancel before the next deployment");

        let wim_job = jobs
            .create(CreateDeploymentJob {
                name: "Supported Windows deployment".to_owned(),
                operation: Operation::DeployWim,
                image_id: wim_image.id,
                targets: vec![target],
                options: DeploymentOptions::default(),
            })
            .expect("WIM job should create");
        jobs.transition(wim_job.id, JobState::Waiting)
            .expect("WIM job should queue");

        let supported_claim = client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/jobs/claim",
                registration.device_id
            ))
            .bearer_auth(&registration.device_token)
            .send()
            .await
            .expect("claim should complete")
            .error_for_status()
            .expect("claim should be accepted")
            .json::<Option<AgentJobLease>>()
            .await
            .expect("claim response should decode")
            .expect("supported job should be leased");
        assert_eq!(supported_claim.job_id, wim_job.id);
        for ineligible_job_id in [invalid_index_job.id, tampered_job.id] {
            assert_eq!(
                jobs.list()
                    .expect("jobs should list")
                    .into_iter()
                    .find(|job| job.id == ineligible_job_id)
                    .expect("ineligible job should remain stored")
                    .state,
                JobState::Cancelled
            );
        }
        assert_eq!(
            jobs.list()
                .expect("jobs should list")
                .into_iter()
                .find(|job| job.id == gho_job.id)
                .expect("GHO job should remain stored")
                .state,
            JobState::Cancelled
        );

        control_plane
            .stop()
            .await
            .expect("control service should stop");
    }

    #[tokio::test]
    async fn control_plane_authenticates_registration_and_heartbeat() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let registry = Arc::new(DeviceRegistry::open(temp.path()).expect("registry should open"));
        let jobs =
            Arc::new(JobRepository::open(temp.path().join("jobs")).expect("jobs should open"));
        let images =
            Arc::new(ImageLibrary::open(temp.path().join("images")).expect("images should open"));
        let control_plane = ControlPlane::new(
            Arc::clone(&registry),
            Arc::clone(&jobs),
            Arc::clone(&images),
            Arc::new(
                ActivityRepository::open(temp.path().join("activities.json"))
                    .expect("activities should open"),
            ),
        );
        let status = control_plane
            .start("127.0.0.1", 0)
            .await
            .expect("control service should start");
        let endpoint = status.endpoint.expect("endpoint should be available");
        let enrollment_token = status
            .enrollment_token
            .expect("enrollment token should be available");
        let client = reqwest::Client::new();

        let unauthorized = client
            .post(format!("{endpoint}/api/v1/agents/register"))
            .json(&inventory())
            .send()
            .await
            .expect("request should complete");
        assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

        let registration = client
            .post(format!("{endpoint}/api/v1/agents/register"))
            .bearer_auth(enrollment_token)
            .json(&inventory())
            .send()
            .await
            .expect("registration should complete")
            .error_for_status()
            .expect("registration should be accepted")
            .json::<AgentRegistration>()
            .await
            .expect("registration response should decode");

        let heartbeat = client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/heartbeat",
                registration.device_id
            ))
            .bearer_auth(&registration.device_token)
            .json(&AgentHeartbeat {
                inventory: inventory(),
            })
            .send()
            .await
            .expect("heartbeat should complete")
            .error_for_status()
            .expect("heartbeat should be accepted")
            .json::<AgentHeartbeatAck>()
            .await
            .expect("heartbeat response should decode");
        assert_eq!(
            heartbeat.next_heartbeat_seconds,
            registration.heartbeat_interval_seconds
        );
        assert_eq!(registry.connected_count().expect("count should work"), 1);

        let image_path = temp.path().join("windows.wim");
        let image_contents = wim_fixture(b"easydeploymesh-test-image");
        std::fs::write(&image_path, &image_contents).expect("image should write");
        let image = images.import(&image_path).expect("image should import");
        let job = jobs
            .create(CreateDeploymentJob {
                name: "Automated Windows deployment".to_owned(),
                operation: Operation::DeployWim,
                image_id: image.id,
                targets: vec![DeploymentTarget {
                    device_id: registration.device_id,
                    target_disk_id: r"\\.\PhysicalDrive0".to_owned(),
                    target_disk_model: "Test disk".to_owned(),
                    target_disk_serial: Some("DISK-SERIAL".to_owned()),
                    target_disk_size_bytes: 64 * 1024 * 1024 * 1024,
                }],
                options: DeploymentOptions::default(),
            })
            .expect("job should create");
        jobs.transition(job.id, JobState::Waiting)
            .expect("job should queue");

        let lease = client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/jobs/claim",
                registration.device_id
            ))
            .bearer_auth(&registration.device_token)
            .send()
            .await
            .expect("claim should complete")
            .error_for_status()
            .expect("claim should be accepted")
            .json::<Option<AgentJobLease>>()
            .await
            .expect("claim response should decode")
            .expect("queued job should be leased");
        assert_eq!(lease.job_id, job.id);
        assert_eq!(lease.image.index, 1);

        let downloaded = client
            .get(format!("{endpoint}{}", lease.image.download_url))
            .bearer_auth(&registration.device_token)
            .send()
            .await
            .expect("download should complete")
            .error_for_status()
            .expect("download should be authorized")
            .bytes()
            .await
            .expect("image should download");
        assert_eq!(downloaded.as_ref(), image_contents.as_slice());

        client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/jobs/{}/progress",
                registration.device_id, job.id
            ))
            .bearer_auth(&registration.device_token)
            .json(&AgentJobProgress {
                lease_id: lease.lease_id,
                stage: DeploymentStage::ApplyingImage,
                progress_percent: 70,
                message: Some("Applying image".to_owned()),
            })
            .send()
            .await
            .expect("progress should complete")
            .error_for_status()
            .expect("progress should be accepted");
        client
            .post(format!(
                "{endpoint}/api/v1/agents/{}/jobs/{}/complete",
                registration.device_id, job.id
            ))
            .bearer_auth(&registration.device_token)
            .json(&AgentJobCompletion {
                lease_id: lease.lease_id,
                succeeded: true,
                error_message: None,
            })
            .send()
            .await
            .expect("completion should complete")
            .error_for_status()
            .expect("completion should be accepted");
        assert_eq!(
            jobs.list().expect("jobs should list")[0].state,
            JobState::Succeeded
        );

        control_plane
            .stop()
            .await
            .expect("control service should stop");
    }
}
