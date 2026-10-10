//! The cutover rehearsal harness (`docs/design/plan-rows.md` §10.10, lane H). Built against
//! the release a schema cutover replaces, it plays the cutover's deadline on a private copy of
//! a home with that release's own code (`rehearse`) through a test adoption host that cannot
//! reach production services (`host::RehearsalHost`), so the copy reaches zero blockers the
//! way the real cutover would, before the candidate converts it. It also replays every legacy
//! plan history with the old patch semantics (`legacy`), the oracle the converter's output is
//! checked against, and keeps legacy homes as SQL text (`dump`).

pub mod dump;
pub mod host;
pub mod legacy;
pub mod rehearse;
pub mod report;
pub mod scratch;
