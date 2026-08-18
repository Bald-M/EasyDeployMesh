use easydeploymesh_core::{
    AgentJobProgress, CreateDeploymentJob, DeploymentJob, DeploymentStage, JobState,
};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Reverse,
    fs, io,
    path::{Path, PathBuf},
    sync::RwLock,
};
use thiserror::Error;
use uuid::Uuid;

const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum JobRepositoryError {
    #[error("a deployment job requires a name")]
    MissingName,
    #[error("a deployment job requires at least one target")]
    MissingTargets,
    #[error("a deployment job requires exactly one target")]
    RequiresSingleTarget,
    #[error("device already has a non-terminal deployment job: {0}")]
    TargetAlreadyHasJob(Uuid),
    #[error("a deployment job requires a positive image index")]
    InvalidImageIndex,
    #[error("target disk fingerprint is incomplete: {0}")]
    IncompleteDiskFingerprint(String),
    #[error("partition plan is invalid: {0}")]
    InvalidPartitionPlan(#[from] easydeploymesh_core::PartitionPlanError),
    #[error("deployment job was not found: {0}")]
    NotFound(Uuid),
    #[error("only completed, failed, or cancelled deployment jobs can be deleted")]
    NotTerminal,
    #[error("deployment job lease is invalid or expired")]
    InvalidLease,
    #[error("deployment job does not belong to device: {0}")]
    WrongDevice(Uuid),
    #[error("deployment progress must be between 0 and 100")]
    InvalidProgress,
    #[error("deployment job lock was poisoned")]
    LockPoisoned,
    #[error(transparent)]
    InvalidTransition(#[from] easydeploymesh_core::JobTransitionError),
    #[error("could not read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("could not write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("deployment job manifest is invalid: {0}")]
    InvalidManifest(#[from] serde_json::Error),
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobManifest {
    schema_version: u32,
    jobs: Vec<DeploymentJob>,
}

#[derive(Debug)]
pub struct JobRepository {
    manifest_path: PathBuf,
    jobs: RwLock<Vec<DeploymentJob>>,
}

impl JobRepository {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, JobRepositoryError> {
        let data_dir = data_dir.as_ref();
        fs::create_dir_all(data_dir).map_err(|source| JobRepositoryError::Write {
            path: data_dir.display().to_string(),
            source,
        })?;

        let manifest_path = data_dir.join("jobs.json");
        let jobs = if manifest_path.exists() {
            let bytes = fs::read(&manifest_path).map_err(|source| JobRepositoryError::Read {
                path: manifest_path.display().to_string(),
                source,
            })?;
            let manifest: JobManifest = serde_json::from_slice(&bytes)?;
            manifest.jobs
        } else {
            Vec::new()
        };

        Ok(Self {
            manifest_path,
            jobs: RwLock::new(jobs),
        })
    }

    pub fn list(&self) -> Result<Vec<DeploymentJob>, JobRepositoryError> {
        let mut jobs = self
            .jobs
            .read()
            .map_err(|_| JobRepositoryError::LockPoisoned)?
            .clone();
        jobs.sort_by_key(|job| Reverse(job.created_at));
        Ok(jobs)
    }

    pub fn queued_count(&self) -> Result<u32, JobRepositoryError> {
        let count = self
            .jobs
            .read()
            .map_err(|_| JobRepositoryError::LockPoisoned)?
            .iter()
            .filter(|job| job.state == JobState::Waiting)
            .count();
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    pub fn references_image(&self, image_id: Uuid) -> Result<bool, JobRepositoryError> {
        Ok(self
            .jobs
            .read()
            .map_err(|_| JobRepositoryError::LockPoisoned)?
            .iter()
            .any(|job| job.image_id == image_id))
    }

    pub fn references_device(&self, device_id: Uuid) -> Result<bool, JobRepositoryError> {
        Ok(self
            .jobs
            .read()
            .map_err(|_| JobRepositoryError::LockPoisoned)?
            .iter()
            .any(|job| {
                !job.state.is_terminal()
                    && job
                        .targets
                        .iter()
                        .any(|target| target.device_id == device_id)
            }))
    }

    pub fn remove(&self, id: Uuid) -> Result<bool, JobRepositoryError> {
        let mut jobs = self
            .jobs
            .write()
            .map_err(|_| JobRepositoryError::LockPoisoned)?;
        let Some(job) = jobs.iter().find(|job| job.id == id) else {
            return Ok(false);
        };
        if !matches!(
            job.state,
            JobState::Succeeded | JobState::Failed | JobState::Cancelled
        ) {
            return Err(JobRepositoryError::NotTerminal);
        }

        let next_jobs: Vec<_> = jobs.iter().filter(|job| job.id != id).cloned().collect();
        self.persist(&next_jobs)?;
        *jobs = next_jobs;
        Ok(true)
    }

    pub fn create(
        &self,
        request: CreateDeploymentJob,
    ) -> Result<DeploymentJob, JobRepositoryError> {
        self.insert(request, JobState::Draft)
    }

    pub fn enqueue(
        &self,
        request: CreateDeploymentJob,
    ) -> Result<DeploymentJob, JobRepositoryError> {
        self.insert(request, JobState::Waiting)
    }

    fn insert(
        &self,
        request: CreateDeploymentJob,
        initial_state: JobState,
    ) -> Result<DeploymentJob, JobRepositoryError> {
        validate_request(&request)?;
        if request.targets.len() != 1 {
            return Err(JobRepositoryError::RequiresSingleTarget);
        }
        let name = request.name.trim();

        let now = chrono::Utc::now();
        let job = DeploymentJob {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            operation: request.operation,
            image_id: request.image_id,
            targets: request.targets,
            options: request.options,
            state: initial_state,
            stage: None,
            progress_percent: 0,
            status_message: None,
            error_message: None,
            lease_id: None,
            lease_expires_at: None,
            created_at: now,
            updated_at: now,
        };

        let mut jobs = self
            .jobs
            .write()
            .map_err(|_| JobRepositoryError::LockPoisoned)?;
        let target_device_id = job.targets[0].device_id;
        if jobs.iter().any(|existing| {
            !existing.state.is_terminal()
                && existing
                    .targets
                    .iter()
                    .any(|target| target.device_id == target_device_id)
        }) {
            return Err(JobRepositoryError::TargetAlreadyHasJob(target_device_id));
        }
        let mut next_jobs = jobs.clone();
        next_jobs.push(job.clone());
        self.persist(&next_jobs)?;
        *jobs = next_jobs;
        Ok(job)
    }

    pub fn transition(
        &self,
        id: Uuid,
        next_state: JobState,
    ) -> Result<DeploymentJob, JobRepositoryError> {
        let mut jobs = self
            .jobs
            .write()
            .map_err(|_| JobRepositoryError::LockPoisoned)?;
        let mut next_jobs = jobs.clone();
        let job = next_jobs
            .iter_mut()
            .find(|job| job.id == id)
            .ok_or(JobRepositoryError::NotFound(id))?;
        job.state = job.state.transition(next_state)?;
        job.updated_at = chrono::Utc::now();
        if next_state == JobState::Succeeded {
            job.progress_percent = 100;
            job.status_message = None;
            job.error_message = None;
        }
        let updated = job.clone();

        self.persist(&next_jobs)?;
        *jobs = next_jobs;
        Ok(updated)
    }

    pub fn lease_for_device(
        &self,
        device_id: Uuid,
        duration: chrono::Duration,
    ) -> Result<Option<DeploymentJob>, JobRepositoryError> {
        self.lease_for_device_if(device_id, duration, |_| true)
    }

    pub fn lease_for_device_if(
        &self,
        device_id: Uuid,
        duration: chrono::Duration,
        mut is_eligible: impl FnMut(&DeploymentJob) -> bool,
    ) -> Result<Option<DeploymentJob>, JobRepositoryError> {
        let mut jobs = self
            .jobs
            .write()
            .map_err(|_| JobRepositoryError::LockPoisoned)?;
        let mut next_jobs = jobs.clone();
        let now = chrono::Utc::now();
        let Some(job) = next_jobs.iter_mut().find(|job| {
            (job.state == JobState::Waiting
                || (job.state == JobState::Running
                    && job
                        .lease_expires_at
                        .is_some_and(|expires_at| expires_at < now)))
                && job.targets.len() == 1
                && job.targets[0].device_id == device_id
                && is_eligible(job)
        }) else {
            return Ok(None);
        };
        if job.state == JobState::Waiting {
            job.state = job.state.transition(JobState::Running)?;
        }
        job.stage = Some(DeploymentStage::Preflight);
        job.progress_percent = 0;
        job.status_message = Some("Agent claimed deployment task".to_owned());
        job.error_message = None;
        job.lease_id = Some(Uuid::new_v4());
        job.lease_expires_at = Some(now + duration);
        job.updated_at = now;
        let leased = job.clone();
        self.persist(&next_jobs)?;
        *jobs = next_jobs;
        Ok(Some(leased))
    }

    pub fn report_progress(
        &self,
        job_id: Uuid,
        device_id: Uuid,
        lease_duration: chrono::Duration,
        progress: AgentJobProgress,
    ) -> Result<DeploymentJob, JobRepositoryError> {
        if progress.progress_percent > 100 {
            return Err(JobRepositoryError::InvalidProgress);
        }
        let renewed_until = chrono::Utc::now() + lease_duration;
        self.update_leased_job(job_id, device_id, progress.lease_id, |job| {
            job.stage = Some(progress.stage);
            job.progress_percent = progress.progress_percent;
            job.status_message = progress
                .message
                .map(|value| value.trim().chars().take(512).collect())
                .filter(|value: &String| !value.is_empty());
            job.lease_expires_at = Some(
                job.lease_expires_at
                    .map_or(renewed_until, |current| current.max(renewed_until)),
            );
        })
    }

    pub fn authorized_lease(
        &self,
        job_id: Uuid,
        device_id: Uuid,
        lease_id: Uuid,
    ) -> Result<DeploymentJob, JobRepositoryError> {
        let jobs = self
            .jobs
            .read()
            .map_err(|_| JobRepositoryError::LockPoisoned)?;
        let job = jobs
            .iter()
            .find(|job| job.id == job_id)
            .ok_or(JobRepositoryError::NotFound(job_id))?;
        if !job
            .targets
            .iter()
            .any(|target| target.device_id == device_id)
        {
            return Err(JobRepositoryError::WrongDevice(device_id));
        }
        if !matches!(job.state, JobState::Running | JobState::Paused)
            || job.lease_id != Some(lease_id)
            || job
                .lease_expires_at
                .is_none_or(|expires_at| expires_at < chrono::Utc::now())
        {
            return Err(JobRepositoryError::InvalidLease);
        }
        Ok(job.clone())
    }

    pub fn complete(
        &self,
        job_id: Uuid,
        device_id: Uuid,
        lease_id: Uuid,
        succeeded: bool,
        error_message: Option<String>,
    ) -> Result<DeploymentJob, JobRepositoryError> {
        self.update_leased_job(job_id, device_id, lease_id, |job| {
            let next = if succeeded {
                JobState::Succeeded
            } else {
                JobState::Failed
            };
            job.state = job
                .state
                .transition(next)
                .expect("leased jobs must be running");
            if succeeded {
                job.progress_percent = 100;
                job.status_message = Some("Deployment completed".to_owned());
                job.error_message = None;
            } else {
                job.status_message = None;
                job.error_message = error_message
                    .map(|value| value.trim().chars().take(2048).collect())
                    .filter(|value: &String| !value.is_empty())
                    .or_else(|| Some("Agent reported deployment failure".to_owned()));
            }
            job.lease_id = None;
            job.lease_expires_at = None;
        })
    }

    pub fn renew_control_lease(
        &self,
        job_id: Uuid,
        device_id: Uuid,
        lease_id: Uuid,
        duration: chrono::Duration,
    ) -> Result<JobState, JobRepositoryError> {
        let mut jobs = self
            .jobs
            .write()
            .map_err(|_| JobRepositoryError::LockPoisoned)?;
        let mut next_jobs = jobs.clone();
        let job = next_jobs
            .iter_mut()
            .find(|job| job.id == job_id)
            .ok_or(JobRepositoryError::NotFound(job_id))?;
        if !job
            .targets
            .iter()
            .any(|target| target.device_id == device_id)
        {
            return Err(JobRepositoryError::WrongDevice(device_id));
        }
        if !matches!(job.state, JobState::Running | JobState::Paused)
            || job.lease_id != Some(lease_id)
        {
            return Err(JobRepositoryError::InvalidLease);
        }
        let now = chrono::Utc::now();
        if job
            .lease_expires_at
            .is_some_and(|expires_at| expires_at >= now + duration / 2)
        {
            return Ok(job.state);
        }
        job.lease_expires_at = Some(now + duration);
        let state = job.state;
        self.persist(&next_jobs)?;
        *jobs = next_jobs;
        Ok(state)
    }

    fn update_leased_job(
        &self,
        job_id: Uuid,
        device_id: Uuid,
        lease_id: Uuid,
        update: impl FnOnce(&mut DeploymentJob),
    ) -> Result<DeploymentJob, JobRepositoryError> {
        let mut jobs = self
            .jobs
            .write()
            .map_err(|_| JobRepositoryError::LockPoisoned)?;
        let mut next_jobs = jobs.clone();
        let job = next_jobs
            .iter_mut()
            .find(|job| job.id == job_id)
            .ok_or(JobRepositoryError::NotFound(job_id))?;
        if !job
            .targets
            .iter()
            .any(|target| target.device_id == device_id)
        {
            return Err(JobRepositoryError::WrongDevice(device_id));
        }
        if !matches!(job.state, JobState::Running | JobState::Paused)
            || job.lease_id != Some(lease_id)
            || job
                .lease_expires_at
                .is_none_or(|expires_at| expires_at < chrono::Utc::now())
        {
            return Err(JobRepositoryError::InvalidLease);
        }
        update(job);
        job.updated_at = chrono::Utc::now();
        let updated = job.clone();
        self.persist(&next_jobs)?;
        *jobs = next_jobs;
        Ok(updated)
    }

    fn persist(&self, jobs: &[DeploymentJob]) -> Result<(), JobRepositoryError> {
        let manifest = JobManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            jobs: jobs.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&manifest)?;
        fs::write(&self.manifest_path, bytes).map_err(|source| JobRepositoryError::Write {
            path: self.manifest_path.display().to_string(),
            source,
        })
    }
}

fn validate_request(request: &CreateDeploymentJob) -> Result<(), JobRepositoryError> {
    if request.name.trim().is_empty() {
        return Err(JobRepositoryError::MissingName);
    }
    if request.targets.is_empty() {
        return Err(JobRepositoryError::MissingTargets);
    }
    if request.options.image_index == 0 {
        return Err(JobRepositoryError::InvalidImageIndex);
    }
    request.options.partition_plan.validate()?;
    if let Some(target) = request.targets.iter().find(|target| {
        target.target_disk_id.trim().is_empty()
            || target.target_disk_model.trim().is_empty()
            || target.target_disk_size_bytes == 0
    }) {
        return Err(JobRepositoryError::IncompleteDiskFingerprint(
            target.target_disk_id.clone(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use easydeploymesh_core::{DeploymentOptions, DeploymentTarget, Operation};

    fn request() -> CreateDeploymentJob {
        CreateDeploymentJob {
            name: "Lab row A".to_owned(),
            operation: Operation::DeployGho,
            image_id: Uuid::new_v4(),
            targets: vec![DeploymentTarget {
                device_id: Uuid::new_v4(),
                target_disk_id: "disk-0".to_owned(),
                target_disk_model: "Test disk".to_owned(),
                target_disk_serial: Some("SERIAL-0".to_owned()),
                target_disk_size_bytes: 64 * 1024 * 1024 * 1024,
            }],
            options: DeploymentOptions::default(),
        }
    }

    #[test]
    fn creates_transitions_and_reloads_a_job() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");
        let created = repository.create(request()).expect("job should be created");
        assert_eq!(created.state, JobState::Draft);

        repository
            .transition(created.id, JobState::Waiting)
            .expect("draft job should queue");
        assert_eq!(repository.queued_count().expect("count should work"), 1);

        let reloaded = JobRepository::open(temp.path()).expect("repository should reload");
        assert_eq!(
            reloaded.list().expect("jobs should list")[0].state,
            JobState::Waiting
        );
    }

    #[test]
    fn enqueues_a_waiting_job_without_a_persisted_draft_step() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");

        let queued = repository.enqueue(request()).expect("job should enqueue");

        assert_eq!(queued.state, JobState::Waiting);
        assert_eq!(repository.queued_count().expect("count should work"), 1);
        assert_eq!(
            repository.list().expect("jobs should list")[0].state,
            JobState::Waiting
        );
    }

    #[test]
    fn rejects_a_job_without_targets() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");
        let mut invalid = request();
        invalid.targets.clear();

        assert!(matches!(
            repository.create(invalid),
            Err(JobRepositoryError::MissingTargets)
        ));
    }

    #[test]
    fn rejects_a_manual_job_with_multiple_targets() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");
        let mut invalid = request();
        invalid.targets.push(invalid.targets[0].clone());

        assert!(matches!(
            repository.create(invalid),
            Err(JobRepositoryError::RequiresSingleTarget)
        ));
    }

    #[test]
    fn concurrent_jobs_for_the_same_device_are_rejected_atomically() {
        use std::sync::{Arc, Barrier};

        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository =
            Arc::new(JobRepository::open(temp.path()).expect("repository should open"));
        let request = request();
        let device_id = request.targets[0].device_id;
        let barrier = Arc::new(Barrier::new(3));
        let attempts = (0..2)
            .map(|_| {
                let repository = Arc::clone(&repository);
                let request = request.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    repository.enqueue(request)
                })
            })
            .collect::<Vec<_>>();

        barrier.wait();
        let results = attempts
            .into_iter()
            .map(|attempt| attempt.join().expect("create thread should finish"))
            .collect::<Vec<_>>();

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(JobRepositoryError::TargetAlreadyHasJob(conflict)) if *conflict == device_id
                ))
                .count(),
            1
        );
        assert_eq!(repository.list().expect("jobs should list").len(), 1);
    }

    #[test]
    fn a_terminal_job_releases_its_device_for_a_new_manual_job() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");
        let request = request();
        let first = repository
            .create(request.clone())
            .expect("first job should create");

        assert!(matches!(
            repository.create(request.clone()),
            Err(JobRepositoryError::TargetAlreadyHasJob(_))
        ));
        repository
            .transition(first.id, JobState::Cancelled)
            .expect("draft job should cancel");
        repository
            .create(request)
            .expect("terminal job should release the target device");
    }

    #[test]
    fn removes_only_terminal_jobs_and_persists_the_removal() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");
        let active = repository.create(request()).expect("job should create");
        assert!(matches!(
            repository.remove(active.id),
            Err(JobRepositoryError::NotTerminal)
        ));

        repository
            .transition(active.id, JobState::Cancelled)
            .expect("draft job should cancel");
        assert!(
            repository
                .remove(active.id)
                .expect("terminal job should delete")
        );
        assert!(
            !repository
                .remove(active.id)
                .expect("missing job is unchanged")
        );

        let reopened = JobRepository::open(temp.path()).expect("repository should reopen");
        assert!(reopened.list().expect("jobs should list").is_empty());
    }

    #[test]
    fn completed_jobs_do_not_prevent_removing_their_target_device() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");
        let request = request();
        let device_id = request.targets[0].device_id;
        let created = repository
            .create(request.clone())
            .expect("job should create");

        assert!(
            repository
                .references_device(device_id)
                .expect("draft job reference should be checked")
        );

        repository
            .transition(created.id, JobState::Cancelled)
            .expect("draft job should cancel");

        assert!(
            !repository
                .references_device(device_id)
                .expect("completed job reference should be ignored")
        );
    }

    #[test]
    fn retryable_failed_jobs_still_protect_their_target_device() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");
        let request = request();
        let device_id = request.targets[0].device_id;
        let created = repository
            .create(request.clone())
            .expect("job should create");

        repository
            .transition(created.id, JobState::Waiting)
            .expect("draft job should queue");
        repository
            .transition(created.id, JobState::Running)
            .expect("waiting job should start");
        repository
            .transition(created.id, JobState::Failed)
            .expect("running job should fail");

        assert!(
            repository
                .references_device(device_id)
                .expect("retryable job reference should be checked")
        );
        assert!(matches!(
            repository.create(request),
            Err(JobRepositoryError::TargetAlreadyHasJob(conflict)) if conflict == device_id
        ));
    }

    #[test]
    fn agent_lease_is_bound_to_the_target_device_and_reports_progress() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");
        let request = request();
        let device_id = request.targets[0].device_id;
        let created = repository.create(request).expect("job should be created");
        repository
            .transition(created.id, JobState::Waiting)
            .expect("job should queue");

        let leased = repository
            .lease_for_device(device_id, chrono::Duration::minutes(30))
            .expect("lease should succeed")
            .expect("job should be available");
        let lease_id = leased.lease_id.expect("lease id should exist");
        assert_eq!(leased.state, JobState::Running);
        assert_eq!(leased.stage, Some(DeploymentStage::Preflight));

        let progress = repository
            .report_progress(
                created.id,
                device_id,
                chrono::Duration::minutes(30),
                AgentJobProgress {
                    lease_id,
                    stage: DeploymentStage::ApplyingImage,
                    progress_percent: 65,
                    message: Some("Applying Windows image".to_owned()),
                },
            )
            .expect("progress should be accepted");
        assert_eq!(progress.progress_percent, 65);
        assert_eq!(progress.stage, Some(DeploymentStage::ApplyingImage));

        assert!(matches!(
            repository.report_progress(
                created.id,
                Uuid::new_v4(),
                chrono::Duration::minutes(30),
                AgentJobProgress {
                    lease_id,
                    stage: DeploymentStage::ApplyingImage,
                    progress_percent: 70,
                    message: None,
                },
            ),
            Err(JobRepositoryError::WrongDevice(_))
        ));
    }

    #[test]
    fn expired_running_job_can_be_reclaimed_by_its_target_device() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");
        let request = request();
        let device_id = request.targets[0].device_id;
        let created = repository.create(request).expect("job should be created");
        repository
            .transition(created.id, JobState::Waiting)
            .expect("job should queue");

        let expired = repository
            .lease_for_device(device_id, chrono::Duration::seconds(-1))
            .expect("initial lease should succeed")
            .expect("queued job should be available");
        let expired_lease_id = expired.lease_id.expect("initial lease id should exist");

        let reclaimed = repository
            .lease_for_device_if(device_id, chrono::Duration::minutes(30), |_| true)
            .expect("reclaim should succeed")
            .expect("expired running job should be available");
        let reclaimed_lease_id = reclaimed.lease_id.expect("fresh lease id should exist");

        assert_eq!(reclaimed.id, created.id);
        assert_eq!(reclaimed.state, JobState::Running);
        assert_ne!(reclaimed_lease_id, expired_lease_id);
        assert!(matches!(
            repository.authorized_lease(created.id, device_id, expired_lease_id),
            Err(JobRepositoryError::InvalidLease)
        ));
        repository
            .authorized_lease(created.id, device_id, reclaimed_lease_id)
            .expect("fresh lease should be authorized");
    }

    #[test]
    fn unexpired_running_job_cannot_be_reclaimed() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");
        let request = request();
        let device_id = request.targets[0].device_id;
        let created = repository.create(request).expect("job should be created");
        repository
            .transition(created.id, JobState::Waiting)
            .expect("job should queue");

        let active = repository
            .lease_for_device(device_id, chrono::Duration::minutes(30))
            .expect("initial lease should succeed")
            .expect("queued job should be available");
        let active_lease_id = active.lease_id.expect("active lease id should exist");

        assert!(
            repository
                .lease_for_device(device_id, chrono::Duration::minutes(30))
                .expect("second claim should be handled")
                .is_none()
        );
        repository
            .authorized_lease(created.id, device_id, active_lease_id)
            .expect("original lease should remain authorized");
    }

    #[test]
    fn expired_running_job_can_be_reclaimed_after_repository_reopens() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");
        let request = request();
        let device_id = request.targets[0].device_id;
        let created = repository.create(request).expect("job should be created");
        repository
            .transition(created.id, JobState::Waiting)
            .expect("job should queue");
        let expired = repository
            .lease_for_device(device_id, chrono::Duration::seconds(-1))
            .expect("initial lease should succeed")
            .expect("queued job should be available");
        let expired_lease_id = expired.lease_id.expect("initial lease id should exist");
        drop(repository);

        let reopened = JobRepository::open(temp.path()).expect("repository should reopen");
        let reclaimed = reopened
            .lease_for_device(device_id, chrono::Duration::minutes(30))
            .expect("reclaim should succeed")
            .expect("persisted expired job should be available");

        assert_eq!(reclaimed.id, created.id);
        assert_ne!(reclaimed.lease_id, Some(expired_lease_id));
    }

    #[test]
    fn expired_running_job_is_not_reclaimed_when_no_longer_eligible() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");
        let request = request();
        let device_id = request.targets[0].device_id;
        let created = repository.create(request).expect("job should be created");
        repository
            .transition(created.id, JobState::Waiting)
            .expect("job should queue");
        let expired = repository
            .lease_for_device(device_id, chrono::Duration::seconds(-1))
            .expect("initial lease should succeed")
            .expect("queued job should be available");

        assert!(
            repository
                .lease_for_device_if(device_id, chrono::Duration::minutes(30), |_| false)
                .expect("ineligible claim should be handled")
                .is_none()
        );
        assert_eq!(
            repository.list().expect("jobs should list")[0].lease_id,
            expired.lease_id
        );
    }

    #[test]
    fn progress_report_extends_the_active_lease() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");
        let request = request();
        let device_id = request.targets[0].device_id;
        let created = repository.create(request).expect("job should be created");
        repository
            .transition(created.id, JobState::Waiting)
            .expect("job should queue");
        let leased = repository
            .lease_for_device(device_id, chrono::Duration::minutes(1))
            .expect("initial lease should succeed")
            .expect("queued job should be available");
        let lease_id = leased.lease_id.expect("lease id should exist");
        let initial_expiry = leased.lease_expires_at.expect("lease expiry should exist");

        let updated = repository
            .report_progress(
                created.id,
                device_id,
                chrono::Duration::minutes(30),
                AgentJobProgress {
                    lease_id,
                    stage: DeploymentStage::ApplyingImage,
                    progress_percent: 65,
                    message: Some("Applying Windows image".to_owned()),
                },
            )
            .expect("progress should renew the lease");
        let renewed_expiry = updated
            .lease_expires_at
            .expect("renewed lease expiry should exist");

        assert!(renewed_expiry > initial_expiry + chrono::Duration::minutes(20));
    }

    #[test]
    fn paused_job_keeps_its_lease_and_can_resume() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let repository = JobRepository::open(temp.path()).expect("repository should open");
        let request = request();
        let device_id = request.targets[0].device_id;
        let created = repository.create(request).expect("job should create");
        repository
            .transition(created.id, JobState::Waiting)
            .expect("job should queue");
        let leased = repository
            .lease_for_device(device_id, chrono::Duration::minutes(30))
            .expect("lease should work")
            .expect("job should be available");
        let lease_id = leased.lease_id.expect("lease id should exist");

        repository
            .transition(created.id, JobState::Paused)
            .expect("running job should pause");
        assert_eq!(
            repository
                .renew_control_lease(
                    created.id,
                    device_id,
                    lease_id,
                    chrono::Duration::minutes(30),
                )
                .expect("paused lease should remain authorized"),
            JobState::Paused
        );
        repository
            .transition(created.id, JobState::Running)
            .expect("paused job should resume");
        repository
            .authorized_lease(created.id, device_id, lease_id)
            .expect("resumed lease should remain authorized");
    }
}
