//! sluice-store: durable SQLite ownership.

pub mod artifacts;
pub mod attempts;
pub mod backup;
pub mod messages;
pub mod plans;
pub mod projects;
pub mod query;
pub mod reads;
pub mod records;
pub mod resources;
pub mod schema;
pub mod writer;

pub use reads::{DurableCursor, ReadPool, Subscription};
pub use schema::{Result, StoreError};
pub use writer::{
    ChangeKey, ChangeNotification, RetrySafety, RowMark, WriteTransaction, Writer, WriterOptions,
};
