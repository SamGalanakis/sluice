//! The store's cost counters (`docs/design/plan-rows.md` §11): process-wide, because the writer
//! runs on its own thread; a test that measures resets them, does its work and reads them.

use std::sync::atomic::{AtomicU64, Ordering};

/// What the store counted since the last `reset`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Step, input or output declarations parsed from SQL text.
    pub declarations_decoded: u64,
    /// Declarations written.
    pub declarations_written: u64,
    /// Authored and index rows an edit inserted, updated or deleted (rows a trigger or a
    /// cascade removes are not counted).
    pub rows_written: u64,
    /// `export_plan` and `read_plan_rows` calls.
    pub full_exports: u64,
    /// Existing rows an edit (or a replayed revision) moved to another position.
    pub positions_renumbered: u64,
    /// Step, input, lease and competitor rows `read_scoped_state` read.
    pub state_rows_read: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Counter {
    DeclarationsDecoded,
    DeclarationsWritten,
    RowsWritten,
    FullExports,
    PositionsRenumbered,
    StateRowsRead,
}

static COUNTERS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

pub(crate) fn add(counter: Counter, amount: u64) {
    COUNTERS[counter as usize].fetch_add(amount, Ordering::Relaxed);
}

/// Zero every counter.
pub fn reset() {
    for counter in &COUNTERS {
        counter.store(0, Ordering::Relaxed);
    }
}

/// The counts since the last `reset`.
pub fn read() -> Counters {
    let get = |counter: Counter| COUNTERS[counter as usize].load(Ordering::Relaxed);
    Counters {
        declarations_decoded: get(Counter::DeclarationsDecoded),
        declarations_written: get(Counter::DeclarationsWritten),
        rows_written: get(Counter::RowsWritten),
        full_exports: get(Counter::FullExports),
        positions_renumbered: get(Counter::PositionsRenumbered),
        state_rows_read: get(Counter::StateRowsRead),
    }
}
