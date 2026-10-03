use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}
pub struct SystemClock {
    origin: Instant,
}
impl Default for SystemClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}
impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}
#[derive(Default)]
pub struct ManualClock {
    milliseconds: AtomicU64,
}
impl ManualClock {
    pub fn advance(&self, duration: Duration) -> Result<(), &'static str> {
        let delta = u64::try_from(duration.as_millis()).map_err(|_| "clock overflow")?;
        self.milliseconds
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
                old.checked_add(delta)
            })
            .map_err(|_| "clock overflow")?;
        Ok(())
    }
}
impl Clock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.milliseconds.load(Ordering::SeqCst))
    }
}
