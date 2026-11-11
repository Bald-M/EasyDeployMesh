//! Claim-time image revalidation that never blocks an Agent request for long.
//!
//! Revalidating a deployment image means hashing it and, for GHO, decoding
//! the whole partition stream. A 10 GB Ghost image takes minutes, which is far
//! longer than the Agent's 15-second claim timeout. A claim therefore starts
//! the revalidation in the background, waits only briefly, and the job is
//! leased on a later claim once a revalidation has passed.
//!
//! A result is used to issue a lease only while it is still a claim-time
//! result:
//! - the revalidation started after the job was created and less than
//!   [`FRESHNESS`] ago;
//! - every validated file still has the size and modification time it had
//!   when the revalidation finished, and none was modified after the
//!   revalidation started.
//!
//! Anything else starts a new revalidation, so the claim fails closed.

use crate::{ImageLibrary, ImageLibraryError, PreparedGhoDeployment};
use chrono::{DateTime, Utc};
use easydeploymesh_core::{DeploymentJob, Operation};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime},
};
use tokio::sync::watch;
use uuid::Uuid;

/// How long a passed revalidation may be used to issue a lease.
pub(crate) const FRESHNESS: chrono::Duration = chrono::Duration::minutes(30);
/// How long a claim waits for a running revalidation before answering. Well
/// below the Agent's 15-second claim timeout.
pub(crate) const CLAIM_WAIT: Duration = Duration::from_secs(5);

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
    Gho(PreparedGhoDeployment),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    path: PathBuf,
    len: u64,
    modified: SystemTime,
}

#[derive(Debug)]
struct Entry {
    started_at: DateTime<Utc>,
    stamps: Vec<FileStamp>,
    outcome: Result<Arc<Validated>, String>,
}

#[derive(Default)]
struct Slot {
    done: Option<Arc<Entry>>,
    in_flight: Option<watch::Receiver<bool>>,
    /// Jobs already told that `done` failed, so the failure is logged once.
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
    Failed(String),
}

#[derive(Default)]
pub(crate) struct DeploymentValidationCache {
    slots: Mutex<HashMap<Key, Slot>>,
}

impl DeploymentValidationCache {
    /// Returns the job's revalidation status. Starts a revalidation in the
    /// background when no usable result exists, and waits up to `wait` for a
    /// running one to finish.
    pub(crate) async fn status_for_claim(
        self: &Arc<Self>,
        images: &Arc<ImageLibrary>,
        job: &DeploymentJob,
        wait: Duration,
    ) -> Status {
        let key = Key::for_job(job);
        let (status, mut receiver) = {
            let mut slots = self.lock();
            let slot = slots.entry(key).or_default();
            if let Some(done) = usable(slot, job.created_at) {
                return done.status();
            }
            match slot.in_flight.clone() {
                Some(receiver) => (Status::Running { started: false }, receiver),
                None => {
                    let receiver = self.start(key, Arc::clone(images), slot);
                    (Status::Running { started: true }, receiver)
                }
            }
        };
        if wait.is_zero()
            || tokio::time::timeout(wait, receiver.wait_for(|finished| *finished))
                .await
                .is_err()
        {
            return status;
        }
        let mut slots = self.lock();
        match usable(slots.entry(key).or_default(), job.created_at) {
            Some(done) => done.status(),
            None => status,
        }
    }

    /// Returns the passed revalidation the job's lease was issued from, if
    /// its files are unchanged. The image download uses this.
    pub(crate) fn passed_for_lease(&self, job: &DeploymentJob) -> Option<Arc<Validated>> {
        let slots = self.lock();
        let entry = slots.get(&Key::for_job(job))?.done.as_ref()?;
        if entry.started_at < job.created_at || !entry.files_unchanged() {
            return None;
        }
        entry.outcome.as_ref().ok().cloned()
    }

    /// Returns true the first time it is called for a job after its current
    /// revalidation failed.
    pub(crate) fn report_failure_once(&self, job: &DeploymentJob) -> bool {
        let mut slots = self.lock();
        slots
            .get_mut(&Key::for_job(job))
            .is_some_and(|slot| slot.reported_failures.insert(job.id))
    }

    fn start(
        self: &Arc<Self>,
        key: Key,
        images: Arc<ImageLibrary>,
        slot: &mut Slot,
    ) -> watch::Receiver<bool> {
        let (sender, receiver) = watch::channel(false);
        slot.in_flight = Some(receiver.clone());
        let cache = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let entry = run(&images, key);
            {
                let mut slots = cache.lock();
                let slot = slots.entry(key).or_default();
                slot.done = Some(Arc::new(entry));
                slot.in_flight = None;
                slot.reported_failures.clear();
            }
            let _ = sender.send(true);
        });
        receiver
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<Key, Slot>> {
        self.slots.lock().unwrap_or_else(|error| error.into_inner())
    }
}

fn usable(slot: &Slot, job_created_at: DateTime<Utc>) -> Option<Arc<Entry>> {
    slot.done
        .as_ref()
        .filter(|entry| {
            entry.started_at >= job_created_at
                && Utc::now() - entry.started_at <= FRESHNESS
                && entry.files_unchanged()
        })
        .cloned()
}

impl Entry {
    fn files_unchanged(&self) -> bool {
        self.stamps.iter().all(|stamp| {
            fs::metadata(&stamp.path).is_ok_and(|metadata| {
                metadata.len() == stamp.len
                    && metadata
                        .modified()
                        .is_ok_and(|modified| modified == stamp.modified)
            })
        })
    }

    fn status(&self) -> Status {
        match &self.outcome {
            Ok(validated) => Status::Passed(Arc::clone(validated)),
            Err(reason) => Status::Failed(reason.clone()),
        }
    }
}

fn run(images: &ImageLibrary, key: Key) -> Entry {
    let started_at = Utc::now();
    let started = SystemTime::now();
    let outcome = validate(images, key)
        .map_err(|error| error.to_string())
        .and_then(|(validated, paths)| Ok((validated, stamps(&paths, started)?)));
    match outcome {
        Ok((validated, stamps)) => Entry {
            started_at,
            stamps,
            outcome: Ok(Arc::new(validated)),
        },
        Err(reason) => Entry {
            started_at,
            stamps: Vec::new(),
            outcome: Err(reason),
        },
    }
}

fn validate(
    images: &ImageLibrary,
    key: Key,
) -> Result<(Validated, Vec<PathBuf>), ImageLibraryError> {
    match key.operation {
        Operation::DeployWim => {
            let artifact = images.revalidate_for_deployment(key.image_id, key.image_index)?;
            Ok((Validated::Wim, vec![PathBuf::from(artifact.source_path)]))
        }
        Operation::DeployGho => {
            let prepared =
                images.prepare_gho_partition_deployment(key.image_id, key.image_index)?;
            let paths = std::iter::once(&prepared.image.primary)
                .chain(prepared.image.spans.iter())
                .map(|file| file.canonical_path.clone())
                .collect();
            Ok((Validated::Gho(prepared), paths))
        }
        Operation::CaptureGho | Operation::InstallLinux => {
            Err(ImageLibraryError::UnsupportedFormat(format!(
                "{:?} jobs are not leased to the Windows Agent",
                key.operation
            )))
        }
    }
}

/// Records each validated file's size and modification time, and refuses a
/// file that was modified after the revalidation started.
fn stamps(paths: &[PathBuf], started: SystemTime) -> Result<Vec<FileStamp>, String> {
    paths
        .iter()
        .map(|path| {
            let metadata = fs::metadata(path)
                .map_err(|error| format!("{} is unavailable: {error}", path.display()))?;
            let modified = metadata
                .modified()
                .map_err(|error| format!("{} has no modification time: {error}", path.display()))?;
            if modified >= started {
                return Err(format!(
                    "{} changed while it was being revalidated",
                    path.display()
                ));
            }
            Ok(FileStamp {
                path: path.clone(),
                len: metadata.len(),
                modified,
            })
        })
        .collect()
}
