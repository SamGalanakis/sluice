//! The dashboard's wall clock, shared by request rendering, snapshot work and SSE loaders.
//! A directly constructed test dashboard can tie it to Tokio's controllable clock. Each
//! future poll installs its clock only for that poll; unrelated requests keep their own.
use std::{cell::Cell, future::Future, time::SystemTime};

#[derive(Clone, Copy, Debug, Default)]
pub enum Clock {
    #[default]
    System,
    /// Wall time at `origin`, advancing with Tokio time rather than the host clock.
    Tokio {
        wall: SystemTime,
        origin: tokio::time::Instant,
    },
    /// Snapshot work uses the time captured before it entered the blocking pool.
    Fixed(SystemTime),
}
thread_local! {
    static CURRENT: Cell<Clock> = const { Cell::new(Clock::System) };
}
impl Clock {
    /// Anchor a test dashboard's wall time to the current Tokio time. Use with a paused
    /// current-thread runtime and `tokio::time::advance`, or with a normal runtime.
    pub fn tokio(wall: SystemTime) -> Self {
        Self::Tokio {
            wall,
            origin: tokio::time::Instant::now(),
        }
    }
    pub fn now(self) -> SystemTime {
        match self {
            Self::System => SystemTime::now(),
            Self::Tokio { wall, origin } => wall + origin.elapsed(),
            Self::Fixed(wall) => wall,
        }
    }
    pub(crate) fn current() -> Self {
        CURRENT.with(Cell::get)
    }
    fn with<T>(self, operation: impl FnOnce() -> T) -> T {
        struct Restore(Clock);
        impl Drop for Restore {
            fn drop(&mut self) {
                CURRENT.with(|current| current.set(self.0));
            }
        }
        let _restore = Restore(CURRENT.with(|current| current.replace(self)));
        operation()
    }
    /// Keep this clock for every poll, restoring the caller's clock on return or panic.
    pub async fn scope<F: Future>(self, future: F) -> F::Output {
        let mut future = Box::pin(future);
        std::future::poll_fn(move |cx| self.with(|| future.as_mut().poll(cx))).await
    }
}
pub(crate) fn now() -> SystemTime {
    Clock::current().now()
}
/// Carry the rendering clock across ReadPool's blocking-thread boundary without changing
/// the store's clock, writer timestamps or snapshot/lease behavior.
pub(crate) trait ClockRead {
    async fn clock_snapshot<T, F>(&self, operation: F) -> sluice_store::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> sluice_store::Result<T> + Send + 'static;
}
impl ClockRead for sluice_store::ReadPool {
    async fn clock_snapshot<T, F>(&self, operation: F) -> sluice_store::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> sluice_store::Result<T> + Send + 'static,
    {
        let clock = match Clock::current() {
            Clock::System => Clock::System,
            clock => Clock::Fixed(clock.now()),
        };
        self.snapshot(move |connection| clock.with(|| operation(connection)))
            .await
    }
}
