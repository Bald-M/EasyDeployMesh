use crate::{
    DeviceRegistry, DeviceRegistryError, ImageLibrary, JobRepository, JobRepositoryError,
    device_registry::{digest_secret, generate_secret, secret_matches},
    image_library::PreparedLinuxIsoAsset,
};
use easydeploymesh_core::{
    Architecture, BootMode, DeploymentJob, DeploymentStage, DeploymentTarget, JobState,
    LinuxInstallOptions, LinuxInstallerGuardError, LinuxInstallerGuardRequest,
    LinuxInstallerObservedDisk, Operation,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use thiserror::Error;
use uuid::Uuid;

const OFFER_TTL_MINUTES: i64 = 120;
const INSTALLER_LEASE_MINUTES: i64 = 240;
const MAX_INSTALLER_SESSIONS: usize = 4_096;
const MAX_SESSION_TOKEN_BYTES: usize = 256;
const DISK_SIZE_TOLERANCE_BYTES: u64 = 1024 * 1024;

pub(super) struct InstallerDeployment {
    registry: Arc<DeviceRegistry>,
    jobs: Arc<JobRepository>,
    images: Arc<ImageLibrary>,
    sessions: Mutex<HashMap<Uuid, InstallerSession>>,
}

struct InstallerSession {
    token_digest: String,
    attempt_id: Uuid,
    device_id: Uuid,
    job_id: Uuid,
    image_id: Uuid,
    base_url: String,
    expires_at: chrono::DateTime<chrono::Utc>,
    phase: InstallerSessionPhase,
    lease_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallerSessionPhase {
    Offered,
    Authorizing,
    HandedOff,
    AwaitingFirstBoot,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BootOutcome {
    NoAssignment,
    Denied,
    Assigned(BootAssignment),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BootAssignment {
    pub session_id: Uuid,
    pub attempt_id: Uuid,
    pub token: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub base_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InstallerMediaKind {
    Kernel,
    Initrd,
    Iso,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InstallerMedia {
    pub canonical_path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
    pub content_type: &'static str,
    pub file_name: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GuardAuthorization {
    pub autoinstall: String,
    pub job: DeploymentJob,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum InstallerEventKind {
    Partitioning,
    InstallingSystem,
    ConfiguringBoot,
    Finalizing,
    AwaitingFirstBoot,
    Failed,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct InstallerEventRequest {
    pub kind: InstallerEventKind,
    pub message: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct FirstBootRequest {
    pub attempt_id: Uuid,
}

#[derive(Debug, Error)]
pub(super) enum InstallerDeploymentError {
    #[error("installer request is invalid")]
    InvalidRequest,
    #[error("installer session authentication failed")]
    Unauthorized,
    #[error("installer session has expired")]
    Expired,
    #[error("installer session is not in the required state")]
    InvalidSessionState,
    #[error("installer authorization is already in progress")]
    AuthorizationInProgress,
    #[error("installer media failed integrity validation")]
    MediaIntegrity,
    #[error("installer target disk did not resolve uniquely")]
    TargetDiskMismatch,
    #[error("installer session capacity was reached")]
    SessionCapacity,
    #[error("installer session state is unavailable")]
    SessionState,
    #[error("device registry is unavailable")]
    Registry(#[source] DeviceRegistryError),
    #[error("deployment job repository is unavailable")]
    Jobs(#[source] JobRepositoryError),
    #[error("generated installer configuration is invalid")]
    Configuration,
}

#[derive(Clone)]
struct SessionBinding {
    session_id: Uuid,
    attempt_id: Uuid,
    device_id: Uuid,
    job_id: Uuid,
    image_id: Uuid,
    base_url: String,
    phase: InstallerSessionPhase,
    lease_id: Option<Uuid>,
}

impl InstallerDeployment {
    pub(super) fn new(
        registry: Arc<DeviceRegistry>,
        jobs: Arc<JobRepository>,
        images: Arc<ImageLibrary>,
    ) -> Self {
        Self {
            registry,
            jobs,
            images,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn discover(
        &self,
        mac_address: &str,
        architecture: &str,
        platform: &str,
        base_url: &str,
    ) -> Result<BootOutcome, InstallerDeploymentError> {
        if mac_address.len() > 64
            || architecture.len() > 32
            || platform.len() > 32
            || base_url.len() > 512
        {
            return Ok(BootOutcome::Denied);
        }
        let Some(device) = self
            .registry
            .find_by_mac(mac_address)
            .map_err(InstallerDeploymentError::Registry)?
        else {
            return Ok(BootOutcome::Denied);
        };

        let active_jobs = self
            .jobs
            .list()
            .map_err(InstallerDeploymentError::Jobs)?
            .into_iter()
            .filter(|job| {
                job.operation == Operation::InstallLinux
                    && !job.state.is_terminal()
                    && job.targets.len() == 1
                    && job.targets[0].device_id == device.id
            })
            .collect::<Vec<_>>();
        if active_jobs.is_empty() {
            return Ok(BootOutcome::NoAssignment);
        }
        if active_jobs.len() != 1 || active_jobs[0].state != JobState::Waiting {
            return Ok(BootOutcome::Denied);
        }
        let job = &active_jobs[0];

        if !is_x86_64_architecture(architecture)
            || !is_uefi_platform(platform)
            || device.architecture != Architecture::X86_64
            || device.boot_mode != BootMode::Uefi
        {
            return Ok(BootOutcome::Denied);
        }
        let Some(options) = job.options.linux_install.as_ref() else {
            return Ok(BootOutcome::Denied);
        };
        if options.validate().is_err()
            || job.targets[0]
                .target_disk_serial
                .as_deref()
                .is_none_or(|serial| serial.trim().is_empty())
        {
            return Ok(BootOutcome::Denied);
        }

        let prepared = match self.images.prepare_linux_iso(job.image_id) {
            Ok(prepared) => prepared,
            Err(_) => return Ok(BootOutcome::Denied),
        };
        if prepared.artifact_id != job.image_id
            || prepared.capability.architecture != Architecture::X86_64
            || device.memory_bytes < prepared.capability.minimum_memory_bytes
            || job.targets[0].target_disk_size_bytes < prepared.capability.minimum_disk_bytes
        {
            return Ok(BootOutcome::Denied);
        }

        if self
            .jobs
            .mark_installer_booting(job.id, device.id, job.image_id)
            .is_err()
        {
            return Ok(BootOutcome::Denied);
        }

        let token = generate_secret("easydeploymesh_installer");
        let session_id = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let expires_at = chrono::Utc::now() + chrono::Duration::minutes(OFFER_TTL_MINUTES);
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| InstallerDeploymentError::SessionState)?;
        let now = chrono::Utc::now();
        sessions.retain(|_, session| session.expires_at >= now);
        sessions.retain(|_, session| session.job_id != job.id);
        if sessions.len() >= MAX_INSTALLER_SESSIONS {
            return Err(InstallerDeploymentError::SessionCapacity);
        }
        sessions.insert(
            session_id,
            InstallerSession {
                token_digest: digest_secret(&token),
                attempt_id,
                device_id: device.id,
                job_id: job.id,
                image_id: job.image_id,
                base_url: base_url.to_owned(),
                expires_at,
                phase: InstallerSessionPhase::Offered,
                lease_id: None,
            },
        );

        Ok(BootOutcome::Assigned(BootAssignment {
            session_id,
            attempt_id,
            token,
            expires_at,
            base_url: base_url.to_owned(),
        }))
    }

    pub(super) fn initial_user_data(
        &self,
        session_id: Uuid,
        token: &str,
    ) -> Result<String, InstallerDeploymentError> {
        let binding = self.authenticate_session(session_id, token)?;
        if binding.phase != InstallerSessionPhase::Offered {
            return Err(InstallerDeploymentError::InvalidSessionState);
        }
        self.require_bound_job(&binding, JobState::Waiting)?;
        Ok(render_initial_user_data(&binding, token))
    }

    pub(super) fn initial_meta_data(
        &self,
        session_id: Uuid,
        token: &str,
    ) -> Result<String, InstallerDeploymentError> {
        let binding = self.authenticate_session(session_id, token)?;
        if binding.phase != InstallerSessionPhase::Offered {
            return Err(InstallerDeploymentError::InvalidSessionState);
        }
        self.require_bound_job(&binding, JobState::Waiting)?;
        Ok(format!(
            "instance-id: easydeploymesh-{}\nlocal-hostname: easydeploymesh-installer\n",
            binding.attempt_id
        ))
    }

    pub(super) fn media(
        &self,
        session_id: Uuid,
        token: &str,
        kind: InstallerMediaKind,
    ) -> Result<InstallerMedia, InstallerDeploymentError> {
        let binding = self.authenticate_session(session_id, token)?;
        match binding.phase {
            InstallerSessionPhase::Offered => {
                self.require_bound_job(&binding, JobState::Waiting)?;
            }
            InstallerSessionPhase::HandedOff | InstallerSessionPhase::AwaitingFirstBoot => {
                let job = self.require_bound_job(&binding, JobState::Running)?;
                if job.lease_id != binding.lease_id {
                    return Err(InstallerDeploymentError::InvalidSessionState);
                }
            }
            InstallerSessionPhase::Authorizing | InstallerSessionPhase::Completed => {
                return Err(InstallerDeploymentError::InvalidSessionState);
            }
        }
        let prepared = self
            .images
            .prepare_linux_iso(binding.image_id)
            .map_err(|_| InstallerDeploymentError::MediaIntegrity)?;
        if prepared.artifact_id != binding.image_id {
            return Err(InstallerDeploymentError::MediaIntegrity);
        }
        let (asset, content_type, file_name) = match kind {
            InstallerMediaKind::Kernel => (prepared.kernel, "application/octet-stream", "vmlinuz"),
            InstallerMediaKind::Initrd => (prepared.initrd, "application/octet-stream", "initrd"),
            InstallerMediaKind::Iso => (prepared.iso, "application/x-iso9660-image", "ubuntu.iso"),
        };
        Ok(media_from_prepared(asset, content_type, file_name))
    }

    pub(super) fn authorize_guard(
        &self,
        session_id: Uuid,
        token: &str,
        request: LinuxInstallerGuardRequest,
    ) -> Result<GuardAuthorization, InstallerDeploymentError> {
        request
            .validate()
            .map_err(|_: LinuxInstallerGuardError| InstallerDeploymentError::InvalidRequest)?;
        let binding = self.authenticate_session(session_id, token)?;
        match binding.phase {
            InstallerSessionPhase::Authorizing => {
                return Err(InstallerDeploymentError::AuthorizationInProgress);
            }
            InstallerSessionPhase::Completed => {
                return Err(InstallerDeploymentError::InvalidSessionState);
            }
            InstallerSessionPhase::HandedOff | InstallerSessionPhase::AwaitingFirstBoot => {
                return self.replay_guard_authorization(&binding, token, &request);
            }
            InstallerSessionPhase::Offered => {}
        }
        self.set_session_phase(
            session_id,
            InstallerSessionPhase::Offered,
            InstallerSessionPhase::Authorizing,
        )?;

        let authorization = self.perform_initial_guard_authorization(&binding, token, &request);
        if authorization.is_err() {
            let _ = self.set_session_phase(
                session_id,
                InstallerSessionPhase::Authorizing,
                InstallerSessionPhase::Offered,
            );
        }
        authorization
    }

    pub(super) fn report_event(
        &self,
        session_id: Uuid,
        token: &str,
        request: InstallerEventRequest,
    ) -> Result<DeploymentJob, InstallerDeploymentError> {
        if request
            .message
            .as_ref()
            .is_some_and(|message| message.len() > 2_048 || message.contains('\0'))
        {
            return Err(InstallerDeploymentError::InvalidRequest);
        }
        let binding = self.authenticate_session(session_id, token)?;
        if !matches!(
            binding.phase,
            InstallerSessionPhase::HandedOff | InstallerSessionPhase::AwaitingFirstBoot
        ) {
            return Err(InstallerDeploymentError::InvalidSessionState);
        }
        let lease_id = binding
            .lease_id
            .ok_or(InstallerDeploymentError::InvalidSessionState)?;
        if request.kind == InstallerEventKind::Failed {
            let updated = self
                .jobs
                .complete_installer(binding.job_id, binding.device_id, lease_id, false, None)
                .map_err(InstallerDeploymentError::Jobs)?;
            self.mark_session_completed(session_id)?;
            return Ok(updated);
        }

        let (stage, progress) = installer_event_progress(request.kind);
        let updated = self
            .jobs
            .report_installer_progress(
                binding.job_id,
                binding.device_id,
                lease_id,
                chrono::Duration::minutes(INSTALLER_LEASE_MINUTES),
                stage,
                progress,
                Some(installer_event_message(request.kind).to_owned()),
            )
            .map_err(InstallerDeploymentError::Jobs)?;
        if request.kind == InstallerEventKind::AwaitingFirstBoot
            && binding.phase == InstallerSessionPhase::HandedOff
        {
            self.set_session_phase(
                session_id,
                InstallerSessionPhase::HandedOff,
                InstallerSessionPhase::AwaitingFirstBoot,
            )?;
        }
        Ok(updated)
    }

    pub(super) fn complete_first_boot(
        &self,
        session_id: Uuid,
        token: &str,
        attempt_id: Uuid,
    ) -> Result<Option<DeploymentJob>, InstallerDeploymentError> {
        let binding = self.authenticate_session(session_id, token)?;
        if binding.attempt_id != attempt_id {
            return Err(InstallerDeploymentError::InvalidSessionState);
        }
        if binding.phase == InstallerSessionPhase::Completed {
            return Ok(None);
        }
        if !matches!(
            binding.phase,
            InstallerSessionPhase::HandedOff | InstallerSessionPhase::AwaitingFirstBoot
        ) {
            return Err(InstallerDeploymentError::InvalidSessionState);
        }
        let updated = self
            .jobs
            .complete_installer(
                binding.job_id,
                binding.device_id,
                binding
                    .lease_id
                    .ok_or(InstallerDeploymentError::InvalidSessionState)?,
                true,
                None,
            )
            .map_err(InstallerDeploymentError::Jobs)?;
        self.mark_session_completed(session_id)?;
        Ok(Some(updated))
    }

    fn perform_initial_guard_authorization(
        &self,
        binding: &SessionBinding,
        token: &str,
        request: &LinuxInstallerGuardRequest,
    ) -> Result<GuardAuthorization, InstallerDeploymentError> {
        let prepared = self
            .images
            .prepare_linux_iso(binding.image_id)
            .map_err(|_| InstallerDeploymentError::MediaIntegrity)?;
        if prepared.artifact_id != binding.image_id
            || !prepared
                .iso
                .sha256
                .eq_ignore_ascii_case(&request.image_sha256)
        {
            return Err(InstallerDeploymentError::MediaIntegrity);
        }
        let job = self.require_bound_job(binding, JobState::Waiting)?;
        let target = job
            .targets
            .first()
            .ok_or(InstallerDeploymentError::InvalidSessionState)?;
        let selected_disk = uniquely_matching_disk(target, &request.disks)?;
        if selected_disk.size_bytes < prepared.capability.minimum_disk_bytes {
            return Err(InstallerDeploymentError::TargetDiskMismatch);
        }
        let options = job
            .options
            .linux_install
            .as_ref()
            .ok_or(InstallerDeploymentError::Configuration)?;
        options
            .validate()
            .map_err(|_| InstallerDeploymentError::Configuration)?;
        let autoinstall = render_final_autoinstall(binding, token, options, &selected_disk.path)?;

        let leased = self
            .jobs
            .lease_installer_job(
                binding.job_id,
                binding.device_id,
                binding.image_id,
                chrono::Duration::minutes(INSTALLER_LEASE_MINUTES),
            )
            .map_err(InstallerDeploymentError::Jobs)?;
        let lease_id = leased
            .lease_id
            .ok_or(InstallerDeploymentError::InvalidSessionState)?;
        let expires_at = leased
            .lease_expires_at
            .ok_or(InstallerDeploymentError::InvalidSessionState)?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| InstallerDeploymentError::SessionState)?;
        let session = sessions
            .get_mut(&binding.session_id)
            .ok_or(InstallerDeploymentError::Unauthorized)?;
        if session.phase != InstallerSessionPhase::Authorizing {
            return Err(InstallerDeploymentError::InvalidSessionState);
        }
        session.phase = InstallerSessionPhase::HandedOff;
        session.lease_id = Some(lease_id);
        session.expires_at = expires_at;
        Ok(GuardAuthorization {
            autoinstall,
            job: leased,
        })
    }

    fn replay_guard_authorization(
        &self,
        binding: &SessionBinding,
        token: &str,
        request: &LinuxInstallerGuardRequest,
    ) -> Result<GuardAuthorization, InstallerDeploymentError> {
        let prepared = self
            .images
            .prepare_linux_iso(binding.image_id)
            .map_err(|_| InstallerDeploymentError::MediaIntegrity)?;
        if !prepared
            .iso
            .sha256
            .eq_ignore_ascii_case(&request.image_sha256)
        {
            return Err(InstallerDeploymentError::MediaIntegrity);
        }
        let job = self.require_bound_job(binding, JobState::Running)?;
        if job.lease_id != binding.lease_id {
            return Err(InstallerDeploymentError::InvalidSessionState);
        }
        let selected_disk = uniquely_matching_disk(&job.targets[0], &request.disks)?;
        let options = job
            .options
            .linux_install
            .as_ref()
            .ok_or(InstallerDeploymentError::Configuration)?;
        Ok(GuardAuthorization {
            autoinstall: render_final_autoinstall(binding, token, options, &selected_disk.path)?,
            job,
        })
    }

    fn authenticate_session(
        &self,
        session_id: Uuid,
        token: &str,
    ) -> Result<SessionBinding, InstallerDeploymentError> {
        if token.is_empty() || token.len() > MAX_SESSION_TOKEN_BYTES {
            return Err(InstallerDeploymentError::Unauthorized);
        }
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| InstallerDeploymentError::SessionState)?;
        let session = sessions
            .get(&session_id)
            .ok_or(InstallerDeploymentError::Unauthorized)?;
        if !secret_matches(&session.token_digest, token) {
            return Err(InstallerDeploymentError::Unauthorized);
        }
        if session.expires_at < chrono::Utc::now() {
            return Err(InstallerDeploymentError::Expired);
        }
        Ok(SessionBinding {
            session_id,
            attempt_id: session.attempt_id,
            device_id: session.device_id,
            job_id: session.job_id,
            image_id: session.image_id,
            base_url: session.base_url.clone(),
            phase: session.phase,
            lease_id: session.lease_id,
        })
    }

    fn require_bound_job(
        &self,
        binding: &SessionBinding,
        required_state: JobState,
    ) -> Result<DeploymentJob, InstallerDeploymentError> {
        let job = self
            .jobs
            .list()
            .map_err(InstallerDeploymentError::Jobs)?
            .into_iter()
            .find(|job| job.id == binding.job_id)
            .ok_or(InstallerDeploymentError::InvalidSessionState)?;
        if job.operation != Operation::InstallLinux
            || job.image_id != binding.image_id
            || job.state != required_state
            || job.targets.len() != 1
            || job.targets[0].device_id != binding.device_id
        {
            return Err(InstallerDeploymentError::InvalidSessionState);
        }
        Ok(job)
    }

    fn set_session_phase(
        &self,
        session_id: Uuid,
        expected: InstallerSessionPhase,
        next: InstallerSessionPhase,
    ) -> Result<(), InstallerDeploymentError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| InstallerDeploymentError::SessionState)?;
        let session = sessions
            .get_mut(&session_id)
            .ok_or(InstallerDeploymentError::Unauthorized)?;
        if session.phase != expected {
            return Err(InstallerDeploymentError::InvalidSessionState);
        }
        session.phase = next;
        Ok(())
    }

    fn mark_session_completed(&self, session_id: Uuid) -> Result<(), InstallerDeploymentError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| InstallerDeploymentError::SessionState)?;
        let session = sessions
            .get_mut(&session_id)
            .ok_or(InstallerDeploymentError::Unauthorized)?;
        session.phase = InstallerSessionPhase::Completed;
        Ok(())
    }
}

fn media_from_prepared(
    asset: PreparedLinuxIsoAsset,
    content_type: &'static str,
    file_name: &'static str,
) -> InstallerMedia {
    InstallerMedia {
        canonical_path: asset.canonical_path,
        size_bytes: asset.size_bytes,
        sha256: asset.sha256,
        content_type,
        file_name,
    }
}

fn is_x86_64_architecture(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "x86_64" | "amd64" | "x86-64"
    )
}

fn is_uefi_platform(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "efi" | "uefi")
}

fn uniquely_matching_disk(
    target: &DeploymentTarget,
    disks: &[LinuxInstallerObservedDisk],
) -> Result<LinuxInstallerObservedDisk, InstallerDeploymentError> {
    let expected_serial = target
        .target_disk_serial
        .as_deref()
        .map(str::trim)
        .filter(|serial| !serial.is_empty())
        .ok_or(InstallerDeploymentError::TargetDiskMismatch)?;
    let matching = disks
        .iter()
        .filter(|disk| {
            disk.model
                .trim()
                .eq_ignore_ascii_case(target.target_disk_model.trim())
                && disk.size_bytes.abs_diff(target.target_disk_size_bytes)
                    <= DISK_SIZE_TOLERANCE_BYTES
                && disk
                    .serial
                    .as_deref()
                    .is_some_and(|serial| serial.trim().eq_ignore_ascii_case(expected_serial))
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(InstallerDeploymentError::TargetDiskMismatch);
    }
    Ok(matching[0].clone())
}

fn installer_event_progress(kind: InstallerEventKind) -> (DeploymentStage, u8) {
    match kind {
        InstallerEventKind::Partitioning => (DeploymentStage::Partitioning, 10),
        InstallerEventKind::InstallingSystem => (DeploymentStage::InstallingSystem, 45),
        InstallerEventKind::ConfiguringBoot => (DeploymentStage::ConfiguringBoot, 75),
        InstallerEventKind::Finalizing => (DeploymentStage::Finalizing, 90),
        InstallerEventKind::AwaitingFirstBoot => (DeploymentStage::AwaitingFirstBoot, 99),
        InstallerEventKind::Failed => (DeploymentStage::Finalizing, 99),
    }
}

fn installer_event_message(kind: InstallerEventKind) -> &'static str {
    match kind {
        InstallerEventKind::Partitioning => "Linux installer is partitioning the authorized disk",
        InstallerEventKind::InstallingSystem => "Linux installer is installing the system",
        InstallerEventKind::ConfiguringBoot => "Linux installer is configuring UEFI boot",
        InstallerEventKind::Finalizing => "Linux installer is finalizing the installation",
        InstallerEventKind::AwaitingFirstBoot => {
            "Linux installer is awaiting first-boot confirmation"
        }
        InstallerEventKind::Failed => "Linux installer reported a failure",
    }
}

pub(super) fn render_boot_script(outcome: BootOutcome) -> String {
    match outcome {
        BootOutcome::NoAssignment => concat!(
            "#!ipxe\n",
            "# EasyDeployMesh no Linux assignment\n",
            "chain tftp://${next-server}/boot/easydeploymesh-winpe.ipxe || exit\n"
        )
        .to_owned(),
        BootOutcome::Denied => concat!(
            "#!ipxe\n",
            "# EasyDeployMesh assignment denied; do not fall back to destructive media\n",
            "echo EasyDeployMesh refused this installer assignment\n",
            "exit\n"
        )
        .to_owned(),
        BootOutcome::Assigned(assignment) => {
            let session_path = format!(
                "{}/api/v1/install/sessions/{}",
                assignment.base_url, assignment.session_id
            );
            format!(
                concat!(
                    "#!ipxe\n",
                    "# EasyDeployMesh Ubuntu autoinstall assignment v1\n",
                    "set edm-session {session_path}\n",
                    "set edm-attempt {attempt_id}\n",
                    "set edm-token {token}\n",
                    "kernel ${{edm-session}}/kernel?token=${{edm-token}} ",
                    "initrd=easydeploymesh-initrd ip=dhcp boot=casper autoinstall ",
                    "cloud-config-url=/dev/null ",
                    "ds=nocloud-net\\;s=${{edm-session}}/seed/${{edm-token}}/ ",
                    "iso-url=${{edm-session}}/iso?token=${{edm-token}} || exit\n",
                    "initrd --name easydeploymesh-initrd ",
                    "${{edm-session}}/initrd?token=${{edm-token}} || exit\n",
                    "boot || exit\n"
                ),
                session_path = session_path,
                attempt_id = assignment.attempt_id,
                token = assignment.token,
            )
        }
    }
}

fn render_initial_user_data(binding: &SessionBinding, token: &str) -> String {
    let guard_url = format!(
        "{}/api/v1/install/sessions/{}/guard",
        binding.base_url, binding.session_id
    );
    let script = INSTALLER_GUARD_SCRIPT
        .replace("__GUARD_URL__", &guard_url)
        .replace("__SESSION_TOKEN__", token);
    format!(
        "#cloud-config\nautoinstall:\n  version: 1\n  refresh-installer:\n    update: false\n  early-commands:\n    - |\n{}",
        indent_lines(&script, 8)
    )
}

fn render_final_autoinstall(
    binding: &SessionBinding,
    token: &str,
    options: &LinuxInstallOptions,
    target_path: &str,
) -> Result<String, InstallerDeploymentError> {
    let Some(device_name) = target_path.strip_prefix("/dev/") else {
        return Err(InstallerDeploymentError::Configuration);
    };
    if device_name.is_empty()
        || device_name.len() > 128
        || device_name.contains('/')
        || device_name
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')))
    {
        return Err(InstallerDeploymentError::Configuration);
    }
    let scalar = |value: &str| {
        serde_json::to_string(value).map_err(|_| InstallerDeploymentError::Configuration)
    };
    let hostname = scalar(&options.hostname)?;
    let username = scalar(&options.username)?;
    let target_path = scalar(target_path)?;
    let ssh_keys = options
        .ssh_authorized_keys
        .iter()
        .map(|key| scalar(key).map(|key| format!("      - {key}\n")))
        .collect::<Result<String, _>>()?;
    let completion_url = format!(
        "{}/api/v1/install/sessions/{}/first-boot",
        binding.base_url, binding.session_id
    );
    let event_url = format!(
        "{}/api/v1/install/sessions/{}/events",
        binding.base_url, binding.session_id
    );
    let callback = FIRST_BOOT_SCRIPT
        .replace("__COMPLETION_URL__", &completion_url)
        .replace("__ATTEMPT_ID__", &binding.attempt_id.to_string());
    let service = FIRST_BOOT_SERVICE;
    let awaiting_event = INSTALLER_EVENT_SCRIPT
        .replace("__EVENT_URL__", &event_url)
        .replace("__SESSION_TOKEN__", token)
        .replace("__EVENT_KIND__", "awaiting_first_boot");
    let failed_event = INSTALLER_EVENT_SCRIPT
        .replace("__EVENT_URL__", &event_url)
        .replace("__SESSION_TOKEN__", token)
        .replace("__EVENT_KIND__", "failed");

    Ok(format!(
        concat!(
            "#cloud-config\n",
            "autoinstall:\n",
            "  version: 1\n",
            "  refresh-installer:\n",
            "    update: false\n",
            "  locale: en_US.UTF-8\n",
            "  keyboard:\n",
            "    layout: us\n",
            "  identity:\n",
            "    hostname: {hostname}\n",
            "    username: {username}\n",
            "    password: \"!\"\n",
            "  ssh:\n",
            "    install-server: true\n",
            "    allow-pw: false\n",
            "    authorized-keys:\n",
            "{ssh_keys}",
            "  network:\n",
            "    version: 2\n",
            "    ethernets:\n",
            "      deployment-nic:\n",
            "        match:\n",
            "          name: \"e*\"\n",
            "        dhcp4: true\n",
            "  storage:\n",
            "    config:\n",
            "      - id: target-disk\n",
            "        type: disk\n",
            "        path: {target_path}\n",
            "        ptable: gpt\n",
            "        wipe: superblock-recursive\n",
            "        preserve: false\n",
            "        grub_device: true\n",
            "      - id: efi-partition\n",
            "        type: partition\n",
            "        device: target-disk\n",
            "        size: 1G\n",
            "        flag: boot\n",
            "        grub_device: true\n",
            "      - id: efi-format\n",
            "        type: format\n",
            "        volume: efi-partition\n",
            "        fstype: fat32\n",
            "      - id: efi-mount\n",
            "        type: mount\n",
            "        device: efi-format\n",
            "        path: /boot/efi\n",
            "      - id: root-partition\n",
            "        type: partition\n",
            "        device: target-disk\n",
            "        size: -1\n",
            "      - id: root-format\n",
            "        type: format\n",
            "        volume: root-partition\n",
            "        fstype: ext4\n",
            "      - id: root-mount\n",
            "        type: mount\n",
            "        device: root-format\n",
            "        path: /\n",
            "  late-commands:\n",
            "    - |\n",
            "        install -d -m 0700 /target/etc/easydeploymesh\n",
            "        cat > /target/etc/easydeploymesh/installer-token <<'EDM_TOKEN'\n",
            "        {token}\n",
            "        EDM_TOKEN\n",
            "        chmod 0600 /target/etc/easydeploymesh/installer-token\n",
            "        install -d -m 0755 /target/etc/systemd/system/multi-user.target.wants\n",
            "        cat > /target/usr/local/sbin/easydeploymesh-first-boot <<'EDM_CALLBACK'\n",
            "{callback}",
            "        EDM_CALLBACK\n",
            "        chmod 0700 /target/usr/local/sbin/easydeploymesh-first-boot\n",
            "        cat > /target/etc/systemd/system/easydeploymesh-first-boot.service <<'EDM_SERVICE'\n",
            "{service}",
            "        EDM_SERVICE\n",
            "        ln -sf ../easydeploymesh-first-boot.service /target/etc/systemd/system/multi-user.target.wants/easydeploymesh-first-boot.service\n",
            "        cat > /run/easydeploymesh-awaiting-first-boot.py <<'EDM_AWAITING'\n",
            "{awaiting_event}",
            "        EDM_AWAITING\n",
            "        python3 /run/easydeploymesh-awaiting-first-boot.py\n",
            "        rm -f /run/easydeploymesh-awaiting-first-boot.py\n",
            "  error-commands:\n",
            "    - |\n",
            "        cat > /run/easydeploymesh-install-failed.py <<'EDM_FAILED'\n",
            "{failed_event}",
            "        EDM_FAILED\n",
            "        python3 /run/easydeploymesh-install-failed.py || true\n",
            "        rm -f /run/easydeploymesh-install-failed.py\n",
            "  shutdown: reboot\n"
        ),
        hostname = hostname,
        username = username,
        ssh_keys = ssh_keys,
        target_path = target_path,
        token = token,
        callback = indent_lines(&callback, 8),
        service = indent_lines(service, 8),
        awaiting_event = indent_lines(&awaiting_event, 8),
        failed_event = indent_lines(&failed_event, 8),
    ))
}

fn indent_lines(value: &str, spaces: usize) -> String {
    let prefix = " ".repeat(spaces);
    value
        .lines()
        .map(|line| format!("{prefix}{line}\n"))
        .collect()
}

const INSTALLER_GUARD_SCRIPT: &str = r#"umask 077
cat > /run/easydeploymesh-installer-guard.py <<'EDM_GUARD'
import glob
import hashlib
import json
import os
import pathlib
import subprocess
import time
import urllib.error
import urllib.request

GUARD_URL = "__GUARD_URL__"
SESSION_TOKEN = "__SESSION_TOKEN__"
MAX_RESPONSE_BYTES = 1024 * 1024

def find_iso():
    try:
        for line in pathlib.Path("/proc/self/mounts").read_text().splitlines():
            fields = line.split()
            if len(fields) >= 2 and fields[1] == "/cdrom" and fields[0].startswith("/dev/loop"):
                loop_name = pathlib.Path(fields[0]).name
                backing = pathlib.Path("/sys/class/block") / loop_name / "loop/backing_file"
                candidate = pathlib.Path("/") / backing.read_text().strip().lstrip("/")
                if candidate.is_file():
                    return candidate
    except (OSError, ValueError):
        pass
    for pattern in ("/run/casper/*.iso", "/isodevice/*.iso"):
        for value in glob.glob(pattern):
            candidate = pathlib.Path(value)
            if candidate.is_file():
                return candidate
    raise RuntimeError("installer ISO backing file is unavailable")

def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while True:
            chunk = source.read(4 * 1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
    return digest.hexdigest()

def inventory():
    raw = subprocess.check_output([
        "lsblk", "--json", "--bytes", "--nodeps",
        "--output", "PATH,MODEL,SERIAL,SIZE,TYPE",
    ], timeout=30)
    devices = json.loads(raw).get("blockdevices", [])
    return [
        {
            "path": str(device.get("path") or ""),
            "model": str(device.get("model") or "").strip(),
            "serial": (str(device["serial"]).strip() if device.get("serial") else None),
            "sizeBytes": int(device.get("size") or 0),
        }
        for device in devices if device.get("type") == "disk"
    ]

payload = json.dumps({"imageSha256": sha256(find_iso()), "disks": inventory()}).encode()
request = urllib.request.Request(
    GUARD_URL,
    data=payload,
    headers={
        "Authorization": "Bearer " + SESSION_TOKEN,
        "Content-Type": "application/json",
    },
    method="POST",
)
last_error = None
for attempt in range(120):
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            configuration = response.read(MAX_RESPONSE_BYTES + 1)
        if len(configuration) > MAX_RESPONSE_BYTES:
            raise RuntimeError("installer configuration exceeds limit")
        pathlib.Path("/autoinstall.yaml").write_bytes(configuration)
        break
    except urllib.error.HTTPError as error:
        if error.code not in (409, 429, 503):
            raise
        last_error = error
    except (OSError, urllib.error.URLError) as error:
        last_error = error
    time.sleep(10)
else:
    raise RuntimeError("target authorization did not complete") from last_error
EDM_GUARD
chmod 0700 /run/easydeploymesh-installer-guard.py
python3 /run/easydeploymesh-installer-guard.py
rm -f /run/easydeploymesh-installer-guard.py
"#;

const FIRST_BOOT_SCRIPT: &str = r#"#!/usr/bin/python3
import json
import os
import pathlib
import urllib.request

token_path = pathlib.Path("/etc/easydeploymesh/installer-token")
token = token_path.read_text().strip()
request = urllib.request.Request(
    "__COMPLETION_URL__",
    data=json.dumps({"attemptId": "__ATTEMPT_ID__"}).encode(),
    headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
    method="POST",
)
with urllib.request.urlopen(request, timeout=30) as response:
    if response.status not in (200, 204):
        raise RuntimeError("completion was not accepted")
token_path.unlink(missing_ok=True)
pathlib.Path("/etc/systemd/system/multi-user.target.wants/easydeploymesh-first-boot.service").unlink(missing_ok=True)
"#;

const INSTALLER_EVENT_SCRIPT: &str = r#"import json
import urllib.request

request = urllib.request.Request(
    "__EVENT_URL__",
    data=json.dumps({"kind": "__EVENT_KIND__"}).encode(),
    headers={
        "Authorization": "Bearer __SESSION_TOKEN__",
        "Content-Type": "application/json",
    },
    method="POST",
)
with urllib.request.urlopen(request, timeout=30) as response:
    if response.status not in (200, 204):
        raise RuntimeError("installer event was not accepted")
"#;

const FIRST_BOOT_SERVICE: &str = r#"[Unit]
Description=EasyDeployMesh first-boot completion
After=network-online.target
Wants=network-online.target
ConditionPathExists=/etc/easydeploymesh/installer-token

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/easydeploymesh-first-boot
Restart=on-failure
RestartSec=30

[Install]
WantedBy=multi-user.target
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> DeploymentTarget {
        DeploymentTarget {
            device_id: Uuid::new_v4(),
            target_disk_id: "windows-physical-drive-0".to_owned(),
            target_disk_model: "Exact Model".to_owned(),
            target_disk_serial: Some("SERIAL-01".to_owned()),
            target_disk_size_bytes: 64 * 1024 * 1024 * 1024,
        }
    }

    #[test]
    fn no_assignment_and_denied_boot_paths_are_distinct() {
        let no_assignment = render_boot_script(BootOutcome::NoAssignment);
        let denied = render_boot_script(BootOutcome::Denied);

        assert!(no_assignment.contains("easydeploymesh-winpe.ipxe"));
        assert!(!denied.contains("easydeploymesh-winpe.ipxe"));
        assert!(denied.contains("assignment denied"));
        assert!(denied.ends_with("exit\n"));
    }

    #[test]
    fn assigned_boot_uses_ubuntu_iso_url_and_session_scoped_resources() {
        let assignment = BootAssignment {
            session_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            token: "generated_installer_token".to_owned(),
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(30),
            base_url: "http://192.0.2.10:7760".to_owned(),
        };

        let script = render_boot_script(BootOutcome::Assigned(assignment.clone()));

        assert!(script.contains("iso-url=${edm-session}/iso?token=${edm-token}"));
        assert!(script.contains("boot=casper"));
        assert!(script.contains("/seed/${edm-token}/"));
        assert!(!script.contains("/seed/?token="));
        assert!(script.contains("/kernel?token=${edm-token}"));
        assert!(script.contains("/initrd?token=${edm-token}"));
        assert!(script.contains(&assignment.session_id.to_string()));
        assert!(!script.contains("easydeploymesh-winpe.ipxe"));
    }

    #[test]
    fn initial_seed_has_a_guard_but_no_storage_authorization() {
        let binding = SessionBinding {
            session_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            job_id: Uuid::new_v4(),
            image_id: Uuid::new_v4(),
            base_url: "http://192.0.2.10:7760".to_owned(),
            phase: InstallerSessionPhase::Offered,
            lease_id: None,
        };

        let seed = render_initial_user_data(&binding, "safe_generated_token");

        assert!(seed.contains("early-commands"));
        assert!(seed.contains("/guard"));
        assert!(
            !seed
                .lines()
                .any(|line| line.trim_start().starts_with("storage:"))
        );
        assert!(!seed.contains("wipe: object storage"));
    }

    #[test]
    fn target_matching_requires_one_serial_model_and_size_match() {
        let target = target();
        let matching = LinuxInstallerObservedDisk {
            path: "/dev/nvme0n1".to_owned(),
            model: " exact model ".to_owned(),
            serial: Some("serial-01".to_owned()),
            size_bytes: target.target_disk_size_bytes + 1024,
        };
        assert_eq!(
            uniquely_matching_disk(&target, std::slice::from_ref(&matching))
                .expect("one stable match should be accepted")
                .path,
            "/dev/nvme0n1"
        );
        assert!(matches!(
            uniquely_matching_disk(&target, &[matching.clone(), matching]),
            Err(InstallerDeploymentError::TargetDiskMismatch)
        ));
    }

    #[test]
    fn final_autoinstall_only_targets_the_server_selected_linux_path() {
        let binding = SessionBinding {
            session_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            job_id: Uuid::new_v4(),
            image_id: Uuid::new_v4(),
            base_url: "http://192.0.2.10:7760".to_owned(),
            phase: InstallerSessionPhase::HandedOff,
            lease_id: Some(Uuid::new_v4()),
        };
        let options = LinuxInstallOptions {
            hostname: "lab-linux-01".to_owned(),
            username: "operator".to_owned(),
            ssh_authorized_keys: vec![
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITestInstallerKey".to_owned(),
            ],
        };

        let config =
            render_final_autoinstall(&binding, "safe_generated_token", &options, "/dev/nvme0n1")
                .expect("controlled config should render");

        assert!(config.contains("path: \"/dev/nvme0n1\""));
        assert!(config.contains("wipe: superblock-recursive"));
        assert!(config.contains("fstype: ext4"));
        assert!(config.contains("first-boot"));
        assert!(config.contains("\"kind\": \"awaiting_first_boot\""));
        assert!(config.contains("\"kind\": \"failed\""));
        assert!(config.contains("shutdown: reboot"));
        assert!(config.contains("python3 /run/easydeploymesh-awaiting-first-boot.py"));
        assert!(config.contains("python3 /run/easydeploymesh-install-failed.py || true"));
        assert!(!config.contains("windows-physical-drive-0"));
        assert!(matches!(
            render_final_autoinstall(
                &binding,
                "safe_generated_token",
                &options,
                "/dev/nvme0n1\nunsafe: true",
            ),
            Err(InstallerDeploymentError::Configuration)
        ));
    }
}
