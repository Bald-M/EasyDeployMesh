//! Claim-time image revalidation that never blocks an Agent request for long.
//!
//! Revalidating a deployment image means hashing it and, for GHO, decoding
//! the whole partition stream. A 10 GB Ghost image takes minutes, which is far
//! longer than the Agent's 15-second claim timeout. A claim therefore starts
//! the revalidation in the background, waits only briefly, and the job is
//! leased on a later claim once a revalidation has passed.
//!
//! A passed revalidation is used to issue a lease only while:
//! - it started after the job was created;
//! - it finished less than [`FRESHNESS`] ago;
//! - every image file had the same size and modification time before and
//!   after the revalidation, and still has them now.
//!
//! Anything else starts a new revalidation, so the claim fails closed. A
//! failed revalidation is retried after [`FAILURE_RETRY`].
//!
//! Size and modification time cannot detect a change that preserves both.
//! The Agent's SHA-256 check of the downloaded bytes against the lease is the
//! backstop for that.

use crate::{ImageLibrary, ImageLibraryError, PreparedGhoDeployment};
use chrono::{DateTime, Utc};
use easydeploymesh_core::{DeploymentJob, Operation};
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime},
};
use thiserror::Error;
use tokio::sync::watch;
use uuid::Uuid;

/// How long after it finished a passed revalidation may issue a lease.
pub(crate) const FRESHNESS: Duration = Duration::from_secs(30 * 60);
/// How long a failed revalidation is reported before it is retried.
pub(crate) const FAILURE_RETRY: Duration = Duration::from_secs(2 * 60);
/// How long a claim waits for revalidation before answering. Well below the
/// Agent's 15-second claim timeout.
pub(crate) const CLAIM_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub(crate) enum RevalidationError {
    #[error("deployment image {0} was not found")]
    ImageNotFound(Uuid),
    #[error("image file {name} is unavailable: {source}")]
    FileUnavailable { name: String, source: io::Error },
    #[error("image file {name} changed while it was being revalidated")]
    ChangedDuringRevalidation { name: String },
    #[error("{0:?} jobs are not leased to the Windows Agent")]
    UnsupportedOperation(Operation),
    #[error("image revalidation stopped unexpectedly")]
    Interrupted,
    #[error(transparent)]
    Image(#[from] ImageLibraryError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    image_id: Uuid,
    image_index: u32,
    operation: Operation,
}

impl Key {
    fn for_job(job: &DeploymentJob) -> Self {
        Self {
            image_id: job.image_id,
            image_index: job.options.image_index,
            operation: job.operation,
        }
    }
}

/// What a passed revalidation hands to the lease and the download.
#[derive(Debug)]
pub(crate) enum Validated {
    Wim,
    Gho(Box<PreparedGhoDeployment>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileStamp {
    path: PathBuf,
    len: u64,
    modified: SystemTime,
}

type Outcome = Result<(Validated, Vec<FileStamp>), RevalidationError>;

#[derive(Debug)]
struct Entry {
    started_at: DateTime<Utc>,
    finished: Instant,
    stamps: Vec<FileStamp>,
    outcome: Result<Arc<Validated>, Arc<RevalidationError>>,
}

#[derive(Default)]
struct Slot {
    done: Option<Arc<Entry>>,
    in_flight: Option<watch::Receiver<bool>>,
    /// Jobs whose claim already reported that `done` failed.
    reported_failures: HashSet<Uuid>,
}

/// A job's image revalidation as seen by a claim.
#[derive(Debug, Clone)]
pub(crate) enum Status {
    Passed(Arc<Validated>),
    /// A revalidation is running; `started` is true when this call began it.
    Running {
        started: bool,
    },
    Failed(Arc<RevalidationError>),
}

#[derive(Default)]
pub(crate) struct DeploymentValidationCache {
    slots: Mutex<HashMap<Key, Slot>>,
}

impl DeploymentValidationCache {
    /// Returns the job's revalidation status. Starts a revalidation in the
    /// background when no usable result exists, and waits until `deadline`
    /// for a running one to finish.
    pub(crate) async fn status_for_claim(
        self: &Arc<Self>,
        images: &Arc<ImageLibrary>,
        job: &DeploymentJob,
        deadline: tokio::time::Instant,
    ) -> Status {
        let key = Key::for_job(job);
        let images = Arc::clone(images);
        self.status_with(job, deadline, move || revalidate(&images, key))
            .await
    }

    async fn status_with<F>(
        self: &Arc<Self>,
        job: &DeploymentJob,
        deadline: tokio::time::Instant,
        check: F,
    ) -> Status
    where
        F: FnOnce() -> Outcome + Send + 'static,
    {
        let key = Key::for_job(job);
        if let Some(done) = self.done(key).filter(|entry| entry.usable_for_claim(job)) {
            return done.status();
        }
        let (started, mut receiver) = {
            let mut slots = self.lock();
            let slot = slots.entry(key).or_default();
            match slot.in_flight.clone() {
                Some(receiver) => (false, receiver),
                None => (true, self.start(key, slot, check)),
            }
        };
        let running = Status::Running { started };
        if tokio::time::timeout_at(deadline, receiver.wait_for(|finished| *finished))
            .await
            .is_err()
        {
            return running;
        }
        match self.done(key).filter(|entry| entry.usable_for_claim(job)) {
            Some(done) => done.status(),
            None => running,
        }
    }

    /// Returns the passed revalidation for the job's current lease, if it
    /// started after the job was created and its files are unchanged. The
    /// image download uses this. [`FRESHNESS`] is deliberately not applied:
    /// the lease was issued from a fresh result and lasts longer than that.
    /// A newer result for the same image replaces the one the lease came
    /// from; a failed one refuses the download, and a passed one describes
    /// the same unchanged files.
    pub(crate) fn passed_for_lease(&self, job: &DeploymentJob) -> Option<Arc<Validated>> {
        let entry = self.done(Key::for_job(job))?;
        if entry.started_at < job.created_at || !entry.files_unchanged() {
            return None;
        }
        entry.outcome.as_ref().ok().cloned()
    }

    /// Returns true the first time it is called for a job after its current
    /// revalidation failed.
    pub(crate) fn report_failure_once(&self, job: &DeploymentJob) -> bool {
        self.lock()
            .get_mut(&Key::for_job(job))
            .is_some_and(|slot| slot.reported_failures.insert(job.id))
    }

    fn done(&self, key: Key) -> Option<Arc<Entry>> {
        self.lock().get(&key).and_then(|slot| slot.done.clone())
    }

    fn start<F>(self: &Arc<Self>, key: Key, slot: &mut Slot, check: F) -> watch::Receiver<bool>
    where
        F: FnOnce() -> Outcome + Send + 'static,
    {
        let (sender, receiver) = watch::channel(false);
        slot.in_flight = Some(receiver.clone());
        let completion = Completion {
            cache: Arc::clone(self),
            key,
            sender: Some(sender),
            started_at: Utc::now(),
            outcome: None,
        };
        tokio::task::spawn_blocking(move || {
            let mut completion = completion;
            completion.outcome = Some(check());
        });
        receiver
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<Key, Slot>> {
        self.slots.lock().unwrap_or_else(|error| error.into_inner())
    }
}

/// Stores a background revalidation's result when it is dropped, including
/// when the revalidation panicked, so a slot never stays in flight.
struct Completion {
    cache: Arc<DeploymentValidationCache>,
    key: Key,
    sender: Option<watch::Sender<bool>>,
    started_at: DateTime<Utc>,
    outcome: Option<Outcome>,
}

impl Drop for Completion {
    fn drop(&mut self) {
        let (stamps, outcome) = match self.outcome.take() {
            Some(Ok((validated, stamps))) => (stamps, Ok(Arc::new(validated))),
            Some(Err(error)) => (Vec::new(), Err(Arc::new(error))),
            None => (Vec::new(), Err(Arc::new(RevalidationError::Interrupted))),
        };
        let entry = Entry {
            started_at: self.started_at,
            finished: Instant::now(),
            stamps,
            outcome,
        };
        {
            let mut slots = self.cache.lock();
            let slot = slots.entry(self.key).or_default();
            slot.done = Some(Arc::new(entry));
            slot.in_flight = None;
            slot.reported_failures.clear();
        }
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(true);
        }
    }
}

impl Entry {
    fn usable_for_claim(&self, job: &DeploymentJob) -> bool {
        if self.started_at < job.created_at {
            return false;
        }
        let age = self.finished.elapsed();
        match self.outcome {
            Ok(_) => age <= FRESHNESS && self.files_unchanged(),
            Err(_) => age <= FAILURE_RETRY,
        }
    }

    fn files_unchanged(&self) -> bool {
        self.stamps
            .iter()
            .all(|stamp| stamp_file(&stamp.path).is_ok_and(|current| current == *stamp))
    }

    fn status(&self) -> Status {
        match &self.outcome {
            Ok(validated) => Status::Passed(Arc::clone(validated)),
            Err(error) => Status::Failed(Arc::clone(error)),
        }
    }
}

/// Revalidates the job's image and records its files, refusing the result if
/// any file changed while it was being revalidated.
fn revalidate(images: &ImageLibrary, key: Key) -> Outcome {
    let artifact = images
        .get(key.image_id)?
        .ok_or(RevalidationError::ImageNotFound(key.image_id))?;
    let paths = std::iter::once(&artifact.source_path)
        .chain(artifact.spans.iter())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let before = stamp_files(&paths)?;
    let validated = match key.operation {
        Operation::DeployWim => {
            images.revalidate_for_deployment(key.image_id, key.image_index)?;
            Validated::Wim
        }
        Operation::DeployGho => Validated::Gho(Box::new(
            images.prepare_gho_partition_deployment(key.image_id, key.image_index)?,
        )),
        Operation::CaptureGho | Operation::InstallLinux => {
            return Err(RevalidationError::UnsupportedOperation(key.operation));
        }
    };
    let after = stamp_files(&paths)?;
    if let Some(changed) = before
        .iter()
        .zip(&after)
        .find(|(before, after)| before != after)
    {
        return Err(RevalidationError::ChangedDuringRevalidation {
            name: file_name(&changed.0.path),
        });
    }
    Ok((validated, after))
}

fn stamp_files(paths: &[PathBuf]) -> Result<Vec<FileStamp>, RevalidationError> {
    paths
        .iter()
        .map(|path| {
            stamp_file(path).map_err(|source| RevalidationError::FileUnavailable {
                name: file_name(path),
                source,
            })
        })
        .collect()
}

fn stamp_file(path: &PathBuf) -> io::Result<FileStamp> {
    let metadata = fs::metadata(path)?;
    Ok(FileStamp {
        path: path.clone(),
        len: metadata.len(),
        modified: metadata.modified()?,
    })
}

/// The file name only, so activity logs do not carry managed library paths.
fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::JobRepository;
    use easydeploymesh_core::{
        CreateDeploymentJob, DeploymentOptions, DeploymentTarget, PartitionPlan,
    };

    fn job(jobs: &JobRepository, image_id: Uuid) -> DeploymentJob {
        jobs.create(CreateDeploymentJob {
            name: "Ghost deployment".to_owned(),
            operation: Operation::DeployGho,
            image_id,
            targets: vec![DeploymentTarget {
                device_id: Uuid::new_v4(),
                target_disk_id: r"\\.\PhysicalDrive0".to_owned(),
                target_disk_model: "Test disk".to_owned(),
                target_disk_serial: None,
                target_disk_size_bytes: 64 * 1024 * 1024 * 1024,
            }],
            options: DeploymentOptions {
                image_index: 1,
                partition_plan: PartitionPlan::uefi_gpt(),
                linux_install: None,
            },
        })
        .expect("GHO job should create")
    }

    fn soon() -> tokio::time::Instant {
        tokio::time::Instant::now() + Duration::from_secs(30)
    }

    fn now() -> tokio::time::Instant {
        tokio::time::Instant::now()
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        jobs: JobRepository,
        images: Arc<ImageLibrary>,
        image_id: Uuid,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let jobs = JobRepository::open(temp.path().join("jobs")).expect("jobs should open");
        let images =
            Arc::new(ImageLibrary::open(temp.path().join("images")).expect("images should open"));
        let source = temp.path().join("disk.gho");
        fs::write(&source, easydeploymesh_gho::fixtures::image()).unwrap();
        let image_id = images.import(&source).expect("GHO should import").id;
        Fixture {
            _temp: temp,
            jobs,
            images,
            image_id,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_claim_answers_while_revalidation_runs_and_a_later_claim_passes() {
        let fixture = fixture();
        let job = job(&fixture.jobs, fixture.image_id);
        let cache = Arc::new(DeploymentValidationCache::default());
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let images = Arc::clone(&fixture.images);
        let key = Key::for_job(&job);

        // The revalidation is held until released, as a long GHO decode is.
        let status = cache
            .status_with(&job, now(), move || {
                gate.recv().unwrap();
                revalidate(&images, key)
            })
            .await;
        assert!(matches!(status, Status::Running { started: true }));
        assert!(cache.passed_for_lease(&job).is_none());
        assert!(matches!(
            cache
                .status_with(&job, now(), || panic!("a second run must not start"))
                .await,
            Status::Running { started: false }
        ));

        release.send(()).unwrap();
        let Status::Passed(validated) = cache.status_for_claim(&fixture.images, &job, soon()).await
        else {
            panic!("revalidation should pass");
        };
        assert!(matches!(validated.as_ref(), Validated::Gho(_)));
        assert!(cache.passed_for_lease(&job).is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_result_is_not_reused_for_a_newer_job_or_after_files_change() {
        let fixture = fixture();
        let first = job(&fixture.jobs, fixture.image_id);
        let cache = Arc::new(DeploymentValidationCache::default());
        let Status::Passed(validated) = cache
            .status_for_claim(&fixture.images, &first, soon())
            .await
        else {
            panic!("revalidation should pass");
        };
        let Validated::Gho(prepared) = validated.as_ref() else {
            panic!("a GHO job should get a GHO revalidation");
        };
        let managed = prepared.image.primary.canonical_path.clone();

        // A revalidation that started before a job was created is not reused.
        let newer = job(&fixture.jobs, fixture.image_id);
        assert!(matches!(
            cache.status_for_claim(&fixture.images, &newer, now()).await,
            Status::Running { started: true }
        ));
        assert!(matches!(
            cache
                .status_for_claim(&fixture.images, &newer, soon())
                .await,
            Status::Passed(_)
        ));

        // Changing the managed image refuses the download, and the new
        // revalidation fails because the content no longer matches.
        let mut bytes = fs::read(&managed).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&managed, bytes).unwrap();
        fs::File::options()
            .write(true)
            .open(&managed)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000))
            .unwrap();
        assert!(cache.passed_for_lease(&newer).is_none());
        assert!(matches!(
            cache
                .status_for_claim(&fixture.images, &newer, soon())
                .await,
            Status::Failed(_)
        ));
        assert!(cache.report_failure_once(&newer));
        assert!(!cache.report_failure_once(&newer));
        assert!(cache.passed_for_lease(&newer).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_panicking_revalidation_is_reported_as_a_failure() {
        let fixture = fixture();
        let job = job(&fixture.jobs, fixture.image_id);
        let cache = Arc::new(DeploymentValidationCache::default());
        let status = cache
            .status_with(&job, soon(), || panic!("decoder bug"))
            .await;
        let Status::Failed(error) = status else {
            panic!("a panic should be reported as a failure, got {status:?}");
        };
        assert!(matches!(error.as_ref(), RevalidationError::Interrupted));
        assert!(
            cache
                .lock()
                .get(&Key::for_job(&job))
                .unwrap()
                .in_flight
                .is_none()
        );
    }

    #[test]
    fn passes_expire_after_freshness_and_failures_after_the_retry_interval() {
        let fixture = fixture();
        let job = job(&fixture.jobs, fixture.image_id);
        let entry = |age: Duration, outcome| Entry {
            started_at: Utc::now(),
            finished: Instant::now().checked_sub(age).unwrap(),
            stamps: Vec::new(),
            outcome,
        };
        let passed = || Ok(Arc::new(Validated::Wim));
        let failed = || Err(Arc::new(RevalidationError::Interrupted));
        let minute = Duration::from_secs(60);

        assert!(entry(FRESHNESS - minute, passed()).usable_for_claim(&job));
        assert!(!entry(FRESHNESS + minute, passed()).usable_for_claim(&job));
        assert!(entry(FAILURE_RETRY - minute, failed()).usable_for_claim(&job));
        assert!(!entry(FAILURE_RETRY + minute, failed()).usable_for_claim(&job));

        let mut before_job = entry(Duration::ZERO, passed());
        before_job.started_at = job.created_at - chrono::Duration::seconds(1);
        assert!(!before_job.usable_for_claim(&job));
    }

    #[test]
    fn revalidation_accepts_files_with_a_future_modification_time() {
        let fixture = fixture();
        let job = job(&fixture.jobs, fixture.image_id);
        let artifact = fixture.images.get(fixture.image_id).unwrap().unwrap();
        fs::File::options()
            .write(true)
            .open(&artifact.source_path)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(24 * 60 * 60))
            .unwrap();
        let (_, stamps) = revalidate(&fixture.images, Key::for_job(&job))
            .expect("a future timestamp alone must not fail revalidation");
        assert_eq!(stamps.len(), 1);
    }
}
