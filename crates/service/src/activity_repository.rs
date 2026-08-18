use chrono::{DateTime, Duration, Utc};
use easydeploymesh_core::{ActivityEvent, ActivitySeverity, ActivitySource, ActivitySubject};
use serde_json::{Map, Value};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::RwLock,
};
use thiserror::Error;
use uuid::Uuid;

const RETENTION_DAYS: i64 = 30;
const MAX_EVENTS: usize = 10_000;

#[derive(Debug, Error)]
pub enum ActivityRepositoryError {
    #[error("activity repository lock was poisoned")]
    LockPoisoned,
    #[error("could not write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("could not encode activity events: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Default)]
pub struct ActivityQuery {
    pub sources: Vec<ActivitySource>,
    pub severities: Vec<ActivitySeverity>,
    pub before: Option<DateTime<Utc>>,
    pub after: Option<DateTime<Utc>>,
    pub limit: usize,
}

#[derive(Debug)]
pub struct ActivityRepository {
    path: PathBuf,
    events: RwLock<Vec<ActivityEvent>>,
}

impl ActivityRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ActivityRepositoryError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ActivityRepositoryError::Write {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let events = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<ActivityEvent>>(&bytes).ok())
            .unwrap_or_default();
        let repository = Self {
            path,
            events: RwLock::new(events),
        };
        repository.prune()?;
        Ok(repository)
    }

    pub fn record(
        &self,
        source: ActivitySource,
        kind: impl Into<String>,
        severity: ActivitySeverity,
        subject: Option<ActivitySubject>,
        details: Map<String, Value>,
        raw_message: Option<String>,
    ) -> Result<ActivityEvent, ActivityRepositoryError> {
        let event = ActivityEvent {
            id: Uuid::new_v4(),
            occurred_at: Utc::now(),
            source,
            kind: kind.into(),
            severity,
            subject,
            details,
            raw_message,
        };
        let mut events = self
            .events
            .write()
            .map_err(|_| ActivityRepositoryError::LockPoisoned)?;
        let mut next = events.clone();
        next.push(event.clone());
        prune_events(&mut next, Utc::now());
        self.persist(&next)?;
        *events = next;
        Ok(event)
    }

    pub fn query(
        &self,
        query: &ActivityQuery,
    ) -> Result<Vec<ActivityEvent>, ActivityRepositoryError> {
        let events = self
            .events
            .read()
            .map_err(|_| ActivityRepositoryError::LockPoisoned)?;
        let limit = query.limit.clamp(1, 500);
        Ok(events
            .iter()
            .rev()
            .filter(|event| {
                (query.sources.is_empty() || query.sources.contains(&event.source))
                    && (query.severities.is_empty() || query.severities.contains(&event.severity))
                    && query.before.is_none_or(|before| event.occurred_at < before)
                    && query.after.is_none_or(|after| event.occurred_at > after)
            })
            .take(limit)
            .cloned()
            .collect())
    }

    fn prune(&self) -> Result<(), ActivityRepositoryError> {
        let mut events = self
            .events
            .write()
            .map_err(|_| ActivityRepositoryError::LockPoisoned)?;
        let mut next = events.clone();
        prune_events(&mut next, Utc::now());
        if next.len() != events.len() {
            self.persist(&next)?;
            *events = next;
        }
        Ok(())
    }

    fn persist(&self, events: &[ActivityEvent]) -> Result<(), ActivityRepositoryError> {
        let bytes = serde_json::to_vec_pretty(events)?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, bytes).map_err(|source| ActivityRepositoryError::Write {
            path: temporary.display().to_string(),
            source,
        })?;
        fs::rename(&temporary, &self.path).map_err(|source| ActivityRepositoryError::Write {
            path: self.path.display().to_string(),
            source,
        })
    }
}

fn prune_events(events: &mut Vec<ActivityEvent>, now: DateTime<Utc>) {
    let cutoff = now - Duration::days(RETENTION_DAYS);
    events.retain(|event| event.occurred_at >= cutoff);
    if events.len() > MAX_EVENTS {
        events.drain(..events.len() - MAX_EVENTS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn persists_queries_and_recovers_from_invalid_json() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("activities.json");
        fs::write(&path, b"not json").unwrap();
        let repo = ActivityRepository::open(&path).unwrap();
        repo.record(
            ActivitySource::Service,
            "service_started",
            ActivitySeverity::Success,
            None,
            Map::new(),
            None,
        )
        .unwrap();
        let events = ActivityRepository::open(&path)
            .unwrap()
            .query(&ActivityQuery {
                limit: 200,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "service_started");
    }

    #[test]
    fn filters_by_source_severity_and_cursor() {
        let temp = tempdir().unwrap();
        let repo = ActivityRepository::open(temp.path().join("activities.json")).unwrap();
        repo.record(
            ActivitySource::Device,
            "device_connected",
            ActivitySeverity::Info,
            None,
            Map::new(),
            None,
        )
        .unwrap();
        let cursor = Utc::now() + Duration::seconds(1);
        repo.record(
            ActivitySource::Deployment,
            "job_failed",
            ActivitySeverity::Error,
            None,
            Map::new(),
            None,
        )
        .unwrap();
        let events = repo
            .query(&ActivityQuery {
                sources: vec![ActivitySource::Deployment],
                severities: vec![ActivitySeverity::Error],
                before: Some(cursor),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "job_failed");
    }

    fn fixture(at: DateTime<Utc>) -> ActivityEvent {
        ActivityEvent {
            id: Uuid::new_v4(),
            occurred_at: at,
            source: ActivitySource::Service,
            kind: "fixture".into(),
            severity: ActivitySeverity::Info,
            subject: None,
            details: Map::new(),
            raw_message: None,
        }
    }

    #[test]
    fn pruning_enforces_age_and_count_limits() {
        let now = Utc::now();
        let mut events = vec![fixture(now - Duration::days(31))];
        events.extend((0..=MAX_EVENTS).map(|_| fixture(now)));
        prune_events(&mut events, now);
        assert_eq!(events.len(), MAX_EVENTS);
        assert!(
            events
                .iter()
                .all(|event| event.occurred_at >= now - Duration::days(RETENTION_DAYS))
        );
    }
}
