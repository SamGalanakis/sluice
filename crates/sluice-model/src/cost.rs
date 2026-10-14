//! The plan-rows cost counters and the preparation probe (`docs/design/plan-rows.md` §11).
//!
//! The counters are process-wide, because the writer runs on its own thread: the store counts
//! what it decodes, writes, exports and reads (`declarations_decoded`, `declarations_written`,
//! `rows_written`, `full_exports`, `positions_renumbered`, `state_rows_read`), the model counts
//! its whole compiles (`full_compiles`), and the runtime counts its edit preparations by
//! outcome and those that ran on the writer's thread (`writer_preparations`, always 0). A
//! test that measures holds a [`Measurement`] for the whole measurement, so two measuring
//! tests in one process never reset or read each other's counts.
//!
//! The preparation probe is the barrier §11's contention test holds an edit at: the runtime
//! calls [`preparation_point`] once per preparation, after it has read its snapshot and
//! before it hands the result to the writer, on the thread that prepared it (never the
//! writer's). The commit probe is where a committed edit has not yet reached the plan cache
//! ([`commit_point`]). With no hook installed each costs one atomic load.

use crate::ids::{ProjectId, Revision};
use std::sync::{
    Arc, Mutex, MutexGuard, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

/// One counter of §11.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Counter {
    /// Step, input or output declarations parsed from SQL text (store).
    DeclarationsDecoded,
    /// Declarations written (store).
    DeclarationsWritten,
    /// Authored and index rows an edit inserted, updated or deleted (store).
    RowsWritten,
    /// `export_plan` and `read_plan_rows` calls (store).
    FullExports,
    /// `compile_rows` calls (model).
    FullCompiles,
    /// Rows whose position changed other than by their own put (store).
    PositionsRenumbered,
    /// Step, input, lease and competitor rows `read_scoped_state` read (store).
    StateRowsRead,
    /// Edit preparations that ended committed (runtime).
    PreparationsCommitted,
    /// Edit preparations that ended stale and were prepared again (runtime).
    PreparationsStale,
    /// Edits refused `busy` after their last stale preparation (runtime).
    PreparationsContended,
    /// Edit preparations that ended as a dry run (runtime).
    PreparationsDryRun,
    /// Preparations run on the writer's thread: always 0 (runtime).
    WriterPreparations,
}
const COUNTERS: usize = 12;
impl Counter {
    pub const ALL: [Counter; COUNTERS] = [
        Self::DeclarationsDecoded,
        Self::DeclarationsWritten,
        Self::RowsWritten,
        Self::FullExports,
        Self::FullCompiles,
        Self::PositionsRenumbered,
        Self::StateRowsRead,
        Self::PreparationsCommitted,
        Self::PreparationsStale,
        Self::PreparationsContended,
        Self::PreparationsDryRun,
        Self::WriterPreparations,
    ];
    /// The name §11 gives it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::DeclarationsDecoded => "declarations_decoded",
            Self::DeclarationsWritten => "declarations_written",
            Self::RowsWritten => "rows_written",
            Self::FullExports => "full_exports",
            Self::FullCompiles => "full_compiles",
            Self::PositionsRenumbered => "positions_renumbered",
            Self::StateRowsRead => "state_rows_read",
            Self::PreparationsCommitted => "preparations.committed",
            Self::PreparationsStale => "preparations.stale",
            Self::PreparationsContended => "preparations.contended",
            Self::PreparationsDryRun => "preparations.dry_run",
            Self::WriterPreparations => "writer_preparations",
        }
    }
}

static COUNTS: [AtomicU64; COUNTERS] = [const { AtomicU64::new(0) }; COUNTERS];

/// Add `n` to `counter`.
pub fn add(counter: Counter, n: u64) {
    COUNTS[counter as usize].fetch_add(n, Ordering::Relaxed);
}
/// Add one to `counter`.
pub fn count(counter: Counter) {
    add(counter, 1);
}

/// Every counter at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Costs {
    pub declarations_decoded: u64,
    pub declarations_written: u64,
    pub rows_written: u64,
    pub full_exports: u64,
    pub full_compiles: u64,
    pub positions_renumbered: u64,
    pub state_rows_read: u64,
    pub preparations: Preparations,
    pub writer_preparations: u64,
}
/// Edit preparations by outcome.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Preparations {
    pub committed: u64,
    pub stale: u64,
    pub contended: u64,
    pub dry_run: u64,
}
impl Costs {
    pub fn get(&self, counter: Counter) -> u64 {
        match counter {
            Counter::DeclarationsDecoded => self.declarations_decoded,
            Counter::DeclarationsWritten => self.declarations_written,
            Counter::RowsWritten => self.rows_written,
            Counter::FullExports => self.full_exports,
            Counter::FullCompiles => self.full_compiles,
            Counter::PositionsRenumbered => self.positions_renumbered,
            Counter::StateRowsRead => self.state_rows_read,
            Counter::PreparationsCommitted => self.preparations.committed,
            Counter::PreparationsStale => self.preparations.stale,
            Counter::PreparationsContended => self.preparations.contended,
            Counter::PreparationsDryRun => self.preparations.dry_run,
            Counter::WriterPreparations => self.writer_preparations,
        }
    }
    /// What happened between `earlier` and `self`.
    pub fn since(&self, earlier: &Costs) -> Costs {
        let mut delta = Costs::default();
        for counter in Counter::ALL {
            delta.set(
                counter,
                self.get(counter).saturating_sub(earlier.get(counter)),
            );
        }
        delta
    }
    fn set(&mut self, counter: Counter, value: u64) {
        let slot = match counter {
            Counter::DeclarationsDecoded => &mut self.declarations_decoded,
            Counter::DeclarationsWritten => &mut self.declarations_written,
            Counter::RowsWritten => &mut self.rows_written,
            Counter::FullExports => &mut self.full_exports,
            Counter::FullCompiles => &mut self.full_compiles,
            Counter::PositionsRenumbered => &mut self.positions_renumbered,
            Counter::StateRowsRead => &mut self.state_rows_read,
            Counter::PreparationsCommitted => &mut self.preparations.committed,
            Counter::PreparationsStale => &mut self.preparations.stale,
            Counter::PreparationsContended => &mut self.preparations.contended,
            Counter::PreparationsDryRun => &mut self.preparations.dry_run,
            Counter::WriterPreparations => &mut self.writer_preparations,
        };
        *slot = value;
    }
}

/// The counters now.
pub fn snapshot() -> Costs {
    let mut costs = Costs::default();
    for counter in Counter::ALL {
        costs.set(counter, COUNTS[counter as usize].load(Ordering::Relaxed));
    }
    costs
}

static MEASURING: Mutex<()> = Mutex::new(());

/// Exclusive use of the counters for one measurement: holding it, no other measurement in
/// this process resets or reads them. Created reset.
pub struct Measurement {
    _exclusive: MutexGuard<'static, ()>,
}
impl Measurement {
    pub fn start() -> Self {
        let exclusive = MEASURING.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        Self {
            _exclusive: exclusive,
        }
    }
    /// Zero every counter.
    pub fn reset(&self) {
        reset();
    }
    /// The counters since the last reset.
    pub fn costs(&self) -> Costs {
        snapshot()
    }
}
fn reset() {
    for count in &COUNTS {
        count.store(0, Ordering::Relaxed);
    }
}

/// Where a preparation stands when it reaches the probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparationPoint {
    pub project: ProjectId,
    /// 1 for an edit's first preparation, 2 and 3 for its re-preparations.
    pub attempt: usize,
}
/// Where a plan edit stands when it reaches the commit probe: committed at `rev`, and not yet
/// installed in the runtime's plan cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPoint {
    pub project: ProjectId,
    pub rev: Revision,
}

type Hook<P> = Arc<dyn Fn(&P) + Send + Sync>;
/// One probe: a hook a test installs to stop the runtime at a point, costing one atomic load
/// while none is installed.
struct Probe<P: 'static> {
    hooked: AtomicBool,
    hook: RwLock<Option<Hook<P>>>,
    installed: Mutex<()>,
}
impl<P: 'static> Probe<P> {
    const fn new() -> Self {
        Self {
            hooked: AtomicBool::new(false),
            hook: RwLock::new(None),
            installed: Mutex::new(()),
        }
    }
    fn reach(&self, point: &P) {
        if !self.hooked.load(Ordering::Acquire) {
            return;
        }
        let hook = self.hook.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(hook) = hook {
            hook(point);
        }
    }
    fn install(&'static self, hook: impl Fn(&P) + Send + Sync + 'static) -> ProbeHook<P> {
        let exclusive = self.installed.lock().unwrap_or_else(|e| e.into_inner());
        *self.hook.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(hook));
        self.hooked.store(true, Ordering::Release);
        ProbeHook {
            probe: self,
            _exclusive: exclusive,
        }
    }
}
/// Removes a probe's hook when dropped.
pub struct ProbeHook<P: 'static> {
    probe: &'static Probe<P>,
    _exclusive: MutexGuard<'static, ()>,
}
impl<P: 'static> Drop for ProbeHook<P> {
    fn drop(&mut self) {
        self.probe.hooked.store(false, Ordering::Release);
        *self.probe.hook.write().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
/// The preparation hook's guard.
pub type PreparationHook = ProbeHook<PreparationPoint>;
/// The commit hook's guard.
pub type CommitHook = ProbeHook<CommitPoint>;

static PREPARATIONS: Probe<PreparationPoint> = Probe::new();
static COMMITS: Probe<CommitPoint> = Probe::new();

/// The runtime calls this once per preparation, after reading its snapshot and before the
/// writer sees the result. A hook may block (a test's barrier); it runs on the preparing
/// thread, never the writer's.
pub fn preparation_point(point: &PreparationPoint) {
    PREPARATIONS.reach(point);
}

/// Install the preparation hook until the returned guard drops. One at a time per process:
/// a second install waits for the first guard.
pub fn hook_preparations(
    hook: impl Fn(&PreparationPoint) + Send + Sync + 'static,
) -> PreparationHook {
    PREPARATIONS.install(hook)
}

/// The runtime calls this once per committed plan edit, after its transaction has committed
/// and before the plan cache holds the edit's plan: the window a reader of the new revision
/// can fall into. A hook may block; it runs on the writer's thread, so while it blocks
/// nothing else is written.
pub fn commit_point(point: &CommitPoint) {
    COMMITS.reach(point);
}

/// Install the commit hook until the returned guard drops. One at a time per process.
pub fn hook_commits(hook: impl Fn(&CommitPoint) + Send + Sync + 'static) -> CommitHook {
    COMMITS.install(hook)
}
