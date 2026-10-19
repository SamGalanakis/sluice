//! Bounded retries for observations which must never decide a payload's lifetime.
use std::{future::Future, io, time::Duration};

#[derive(Debug, Clone, Copy)]
pub struct ProbeTiming {
    pub timeout: Duration,
    pub backoff: Duration,
}
impl Default for ProbeTiming {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            backoff: Duration::from_millis(100),
        }
    }
}
tokio::task_local! { static TIMING: ProbeTiming; }
impl ProbeTiming {
    pub fn current() -> Self {
        TIMING.try_with(|t| *t).unwrap_or_default()
    }
    /// Scope observation timing directly, without changing home or process settings.
    pub async fn scope<T>(self, future: impl Future<Output = T>) -> T {
        TIMING.scope(self, future).await
    }
}
pub async fn retry<T, F: Future<Output = io::Result<T>>>(
    mut observe: impl FnMut() -> F,
) -> io::Result<T> {
    let timing = ProbeTiming::current();
    if timing.timeout.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "observation has no time budget",
        ));
    }
    let mut delay = timing.backoff;
    for attempt in 0..3 {
        let result = tokio::time::timeout(timing.timeout, observe())
            .await
            .unwrap_or_else(|_| {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "observation timed out",
                ))
            });
        match result {
            Ok(value) => return Ok(value),
            Err(error) if attempt == 2 => return Err(error),
            Err(_) => tokio::time::sleep(delay).await,
        }
        delay = delay.saturating_mul(2);
    }
    unreachable!()
}

/// Re-run a read-only command; mutations must handle ambiguous acceptance themselves.
pub async fn output(command: &mut tokio::process::Command) -> io::Result<std::process::Output> {
    let timing = ProbeTiming::current();
    if timing.timeout.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "observation has no time budget",
        ));
    }
    command
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut delay = timing.backoff;
    for attempt in 0..3 {
        let result = tokio::time::timeout(timing.timeout, command.output())
            .await
            .unwrap_or_else(|_| {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "observation timed out",
                ))
            });
        match result {
            Ok(output) => return Ok(output),
            Err(error) if attempt == 2 => return Err(error),
            Err(_) => tokio::time::sleep(delay).await,
        }
        delay = delay.saturating_mul(2);
    }
    unreachable!()
}
