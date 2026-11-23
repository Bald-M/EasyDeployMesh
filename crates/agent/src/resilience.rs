//! Retry rules for control, progress, and download requests made after the
//! target disk may already have been modified. A single network blip must not
//! fail the job, but nothing here may let the Agent continue past an explicit
//! control decision or after the lease could have expired.

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use easydeploymesh_core::JobState;
use reqwest::Url;
use std::{
    error::Error,
    fmt, io,
    io::{Read, Write},
    time::{Duration, Instant},
};

const MAX_DELAY: Duration = Duration::from_secs(15);
const PAUSE_POLL: Duration = Duration::from_secs(1);
const MIN_LEASE_WINDOW: Duration = Duration::from_secs(60);
const MAX_LEASE_WINDOW: Duration = Duration::from_secs(2 * 60 * 60);
/// Progress reports are informational; give up on one after this many tries.
const PROGRESS_ATTEMPTS: u32 = 3;
/// Consecutive download failures without any new bytes before giving up.
const DOWNLOAD_ATTEMPTS: u32 = 10;

type BoxError = Box<dyn Error>;

/// True when a request error is worth retrying: no HTTP status (connection,
/// timeout, body decode) or a server-side/throttling status.
pub fn is_transient_request_error(error: &reqwest::Error) -> bool {
    error
        .status()
        .is_none_or(|status| status.is_server_error() || matches!(status.as_u16(), 408 | 425 | 429))
}

fn is_transient(error: &(dyn Error + 'static)) -> bool {
    error
        .downcast_ref::<reqwest::Error>()
        .is_some_and(is_transient_request_error)
        || error.downcast_ref::<TransientIo>().is_some()
}

fn retry_delay(attempt: u32) -> Duration {
    Duration::from_secs(1_u64 << attempt.min(4)).min(MAX_DELAY)
}

/// The control service was unreachable for longer than the lease lasts.
#[derive(Debug)]
struct ControlUnreachable(Duration, String);

impl fmt::Display for ControlUnreachable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "control service unreachable for {} second(s), past the job lease: {}",
            self.0.as_secs(),
            self.1
        )
    }
}

impl Error for ControlUnreachable {}

/// A body read failure; marked so it is retried like a request error.
#[derive(Debug)]
struct TransientIo(io::Error);

impl fmt::Display for TransientIo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "image download interrupted: {}", self.0)
    }
}

impl Error for TransientIo {}

/// How long the Agent may go without control contact: the time left on the
/// lease it was issued, clamped so clock skew between host and target cannot
/// make it zero or unbounded. The server renews the lease on every contact.
pub fn lease_window(expires_at_unix: i64, now_unix: i64) -> Duration {
    let remaining = u64::try_from(expires_at_unix.saturating_sub(now_unix)).unwrap_or(0);
    Duration::from_secs(remaining).clamp(MIN_LEASE_WINDOW, MAX_LEASE_WINDOW)
}

/// Wraps a control request so transient failures stay transient until no
/// successful contact has happened for `window`; then they become fatal.
pub fn lease_bounded(
    mut control_state: impl FnMut() -> Result<JobState, BoxError>,
    window: Duration,
    now: impl Fn() -> Instant,
) -> impl FnMut() -> Result<JobState, BoxError> {
    let mut last_contact = now();
    move || match control_state() {
        Ok(state) => {
            last_contact = now();
            Ok(state)
        }
        Err(error) if is_transient(&*error) => {
            let silent = now().saturating_duration_since(last_contact);
            if silent >= window {
                Err(Box::new(ControlUnreachable(silent, error.to_string())))
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

/// Blocks until the job is Running. Paused, or a transient control failure,
/// keeps waiting; any other state or a fatal error stops the job.
pub fn await_running_with(
    control_state: &mut impl FnMut() -> Result<JobState, BoxError>,
    sleep: &mut impl FnMut(Duration),
) -> Result<(), BoxError> {
    let mut failures = 0;
    loop {
        match control_state() {
            Ok(JobState::Running) => return Ok(()),
            Ok(JobState::Paused) => {
                failures = 0;
                sleep(PAUSE_POLL);
            }
            Ok(state) => {
                return Err(
                    format!("deployment control entered unexpected state: {state:?}").into(),
                );
            }
            Err(error) if is_transient(&*error) => {
                eprintln!("Deployment control request failed: {error}; retrying");
                sleep(retry_delay(failures));
                failures = failures.saturating_add(1);
            }
            Err(error) => return Err(error),
        }
    }
}

/// Sends a progress report, retrying transient failures a few times and then
/// dropping it. A non-transient rejection (for example a revoked lease) stops.
pub fn report_best_effort(
    mut send: impl FnMut() -> Result<(), BoxError>,
    sleep: &mut impl FnMut(Duration),
) -> Result<(), BoxError> {
    for attempt in 0..PROGRESS_ATTEMPTS {
        match send() {
            Ok(()) => return Ok(()),
            Err(error) if is_transient(&*error) => {
                if attempt + 1 == PROGRESS_ATTEMPTS {
                    eprintln!(
                        "Progress report dropped after {PROGRESS_ATTEMPTS} attempts: {error}"
                    );
                } else {
                    sleep(retry_delay(attempt));
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Resolves the lease's download URL against the control server, refusing an
/// absolute URL on another origin so the device token never leaves it.
pub fn resolve_download_url(server: &str, download_url: &str) -> Result<String, BoxError> {
    let base = Url::parse(server)?;
    let resolved = base.join(download_url)?;
    if resolved.origin() != base.origin() {
        return Err(format!(
            "deployment image URL {resolved} is not on the control server {}",
            base.origin().ascii_serialization()
        )
        .into());
    }
    Ok(resolved.into())
}

/// Streams `total` bytes into `output`. `open(offset)` must return a reader
/// that starts at `offset`; after a transient failure the download resumes
/// from the bytes already written instead of starting over.
pub fn download_resumable(
    mut open: impl FnMut(u64) -> Result<Box<dyn Read>, BoxError>,
    output: &mut impl Write,
    total: u64,
    control_state: &mut impl FnMut() -> Result<JobState, BoxError>,
    sleep: &mut impl FnMut(Duration),
    mut on_progress: impl FnMut(u64),
) -> Result<u64, BoxError> {
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut done = 0_u64;
    let mut failures = 0_u32;
    loop {
        await_running_with(control_state, sleep)?;
        let error: BoxError = match open(done) {
            Ok(mut reader) => loop {
                await_running_with(control_state, sleep)?;
                match reader.read(&mut buffer) {
                    Ok(0) if done >= total => {
                        output.flush()?;
                        return Ok(done);
                    }
                    Ok(0) => {
                        break Box::new(TransientIo(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            format!("connection closed after {done} of {total} bytes"),
                        )));
                    }
                    Ok(read) => {
                        if done.saturating_add(read as u64) > total {
                            return Err("deployment image is larger than the lease declares".into());
                        }
                        output.write_all(&buffer[..read])?;
                        done += read as u64;
                        failures = 0;
                        on_progress(done);
                    }
                    Err(error) => break Box::new(TransientIo(error)),
                }
            },
            Err(error) => error,
        };
        if !is_transient(&*error) {
            return Err(error);
        }
        failures = failures.saturating_add(1);
        if failures >= DOWNLOAD_ATTEMPTS {
            return Err(format!(
                "image download failed {failures} times without progress: {error}"
            )
            .into());
        }
        eprintln!("Image download interrupted at {done} bytes: {error}; resuming");
        sleep(retry_delay(failures - 1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, collections::VecDeque, rc::Rc};

    fn transient() -> BoxError {
        Box::new(TransientIo(io::Error::other("network down")))
    }

    fn scripted(
        steps: Vec<Result<JobState, BoxError>>,
    ) -> impl FnMut() -> Result<JobState, BoxError> {
        let mut steps = VecDeque::from(steps);
        move || steps.pop_front().expect("control script exhausted")
    }

    #[test]
    fn await_running_survives_transient_failures_and_pauses() {
        let mut control = scripted(vec![
            Err(transient()),
            Ok(JobState::Paused),
            Err(transient()),
            Ok(JobState::Running),
        ]);
        let mut slept = Vec::new();
        await_running_with(&mut control, &mut |delay| slept.push(delay))
            .expect("transient failures must not stop the job");
        assert_eq!(slept.len(), 3);
    }

    #[test]
    fn await_running_stops_on_explicit_state_and_fatal_errors() {
        let mut cancelled = scripted(vec![Err(transient()), Ok(JobState::Cancelled)]);
        assert!(await_running_with(&mut cancelled, &mut |_| {}).is_err());
        let mut fatal = scripted(vec![Err("lease revoked".into())]);
        assert!(await_running_with(&mut fatal, &mut |_| {}).is_err());
    }

    #[test]
    fn lease_bound_turns_a_long_outage_fatal() {
        let start = Instant::now();
        let clock = Rc::new(Cell::new(start));
        let now = {
            let clock = clock.clone();
            move || clock.get()
        };
        let mut control = lease_bounded(|| Err(transient()), Duration::from_secs(60), now);
        let mut sleep = |delay: Duration| clock.set(clock.get() + delay);
        let error = await_running_with(&mut control, &mut sleep)
            .expect_err("an outage past the lease must stop the job");
        assert!(error.to_string().contains("past the job lease"));
        assert!(clock.get().duration_since(start) >= Duration::from_secs(60));
    }

    #[test]
    fn lease_window_is_clamped_against_clock_skew() {
        assert_eq!(lease_window(1_000, 2_000), MIN_LEASE_WINDOW);
        assert_eq!(lease_window(1_000 + 600, 1_000), Duration::from_secs(600));
        assert_eq!(lease_window(i64::MAX, 0), MAX_LEASE_WINDOW);
    }

    #[test]
    fn progress_is_best_effort_for_transient_failures_only() {
        let attempts = Cell::new(0);
        report_best_effort(
            || {
                attempts.set(attempts.get() + 1);
                Err(transient())
            },
            &mut |_| {},
        )
        .expect("dropped progress must not fail the job");
        assert_eq!(attempts.get(), PROGRESS_ATTEMPTS);
        assert!(report_best_effort(|| Err("forbidden".into()), &mut |_| {}).is_err());
    }

    struct FailAfter {
        data: Vec<u8>,
        limit: usize,
    }

    impl Read for FailAfter {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.data.is_empty() {
                return Ok(0);
            }
            if self.limit == 0 {
                return Err(io::Error::from(io::ErrorKind::ConnectionReset));
            }
            let count = buffer.len().min(self.data.len()).min(self.limit);
            buffer[..count].copy_from_slice(&self.data[..count]);
            self.data.drain(..count);
            self.limit -= count;
            Ok(count)
        }
    }

    #[test]
    fn download_resumes_from_the_written_offset() {
        let image: Vec<u8> = (0..=255).cycle().take(10_000).collect();
        let mut offsets = Vec::new();
        let mut output = Vec::new();
        let mut control = || Ok(JobState::Running);
        let written = download_resumable(
            |offset| {
                offsets.push(offset);
                if offsets.len() == 2 {
                    return Err(transient());
                }
                Ok(Box::new(FailAfter {
                    data: image[offset as usize..].to_vec(),
                    limit: 4_000,
                }))
            },
            &mut output,
            image.len() as u64,
            &mut control,
            &mut |_| {},
            |_| {},
        )
        .expect("interrupted download must resume");
        assert_eq!(written, image.len() as u64);
        assert_eq!(output, image);
        assert_eq!(offsets, vec![0, 4_000, 4_000, 8_000]);
    }

    #[test]
    fn download_treats_early_eof_as_resumable_and_gives_up_without_progress() {
        let mut output = Vec::new();
        let mut control = || Ok(JobState::Running);
        let error = download_resumable(
            |_| Ok(Box::new(io::empty())),
            &mut output,
            10,
            &mut control,
            &mut |_| {},
            |_| {},
        )
        .expect_err("a stalled download must eventually fail");
        assert!(error.to_string().contains("without progress"));
    }

    #[test]
    fn download_stops_when_the_job_is_cancelled() {
        let mut output = Vec::new();
        let mut control = scripted(vec![Ok(JobState::Running), Ok(JobState::Cancelled)]);
        let result = download_resumable(
            |_| Ok(Box::new(io::repeat(1))),
            &mut output,
            10,
            &mut control,
            &mut |_| {},
            |_| {},
        );
        assert!(result.is_err());
        assert!(output.is_empty());
    }

    #[test]
    fn download_url_must_stay_on_the_control_server() {
        let server = "http://192.168.1.10:8080";
        assert_eq!(
            resolve_download_url(server, "/api/v1/images/a").unwrap(),
            "http://192.168.1.10:8080/api/v1/images/a"
        );
        assert!(resolve_download_url(server, "http://192.168.1.10:8080/x").is_ok());
        for foreign in [
            "http://evil.example/x",
            "https://192.168.1.10:8080/x",
            "http://192.168.1.10:9090/x",
            "//evil.example/x",
        ] {
            assert!(resolve_download_url(server, foreign).is_err(), "{foreign}");
        }
    }
}
