//! Read-only connections leased to bounded blocking work, with snapshot reads.

use crate::{
    schema::{self, Result, StoreError},
    writer::{ChangeKey, ChangeNotification, Writer},
};
use rusqlite::{
    Connection, OptionalExtension, TransactionBehavior,
    hooks::{AuthAction, AuthContext, Authorization},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Semaphore, watch};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DurableCursor {
    pub versions: BTreeMap<ChangeKey, i64>,
}
struct Pool {
    connections: Mutex<Vec<Connection>>,
    permits: Arc<Semaphore>,
    home: PathBuf,
    snapshots: AtomicU64,
}
#[derive(Clone)]
pub struct ReadPool {
    inner: Arc<Pool>,
}

struct Lease {
    connection: Option<Connection>,
    pool: Arc<Pool>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            // Poison cannot be produced by a user closure: it never owns this lock.
            self.pool
                .connections
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(connection);
        }
    }
}

impl ReadPool {
    /// Synchronous startup only. Database must already have been initialized by
    /// Writer. Connections never have a writable main database handle.
    pub fn open(home: impl AsRef<Path>, size: usize) -> Result<Self> {
        if size == 0 {
            return Err(sluice_model::error::PublicError::BadRequest {
                message: "read pool size must be positive".into(),
            }
            .into());
        }
        let home = std::fs::canonicalize(home)?;
        let connections = (0..size)
            .map(|_| schema::open_reader(&home, Duration::from_secs(5)))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            inner: Arc::new(Pool {
                connections: Mutex::new(connections),
                permits: Arc::new(Semaphore::new(size)),
                home,
                snapshots: AtomicU64::new(0),
            }),
        })
    }
    pub fn home(&self) -> &Path {
        &self.inner.home
    }
    /// How many snapshots this pool has begun: the cost of a waiter or poller
    /// in database reads, for diagnostics and tests.
    pub fn snapshots(&self) -> u64 {
        self.inner.snapshots.load(Ordering::Relaxed)
    }

    /// All queries in the closure see one DEFERRED read transaction. A snapshot
    /// begins at its first SELECT and is released on success, error or panic.
    /// Admit before spawn_blocking, so waiting readers do not occupy OS threads.
    /// Closures are trusted store code; do not replace hooks/connection settings.
    pub async fn snapshot<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let permit = self
            .inner
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| StoreError::Closed)?;
        self.inner.snapshots.fetch_add(1, Ordering::Relaxed);
        let pool = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let connection = pool
                .connections
                .lock()
                .map_err(|_| StoreError::RequestPanicked)?
                .pop()
                .ok_or(StoreError::Closed)?;
            let mut lease = Lease {
                connection: Some(connection),
                pool,
            };
            let connection = lease.connection.as_mut().ok_or(StoreError::Closed)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            transaction.authorizer(Some(snapshot_authorizer))?;
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(&transaction)));
            transaction.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
            let value = result.map_err(|_| StoreError::RequestPanicked)??;
            transaction.commit()?;
            Ok(value)
        })
        .await
        .map_err(|_| StoreError::RequestPanicked)?
    }

    pub async fn cursor(&self, keys: Vec<ChangeKey>) -> Result<DurableCursor> {
        self.snapshot(move |connection| {
            let mut versions = BTreeMap::new();
            for key in keys {
                let version = connection
                    .query_row(
                        "SELECT version FROM change_versions WHERE scope=?1 AND view=?2",
                        (key.scope(), &key.view),
                        |row| row.get(0),
                    )
                    .optional()?
                    .unwrap_or(0);
                versions.insert(key, version);
            }
            Ok(DurableCursor { versions })
        })
        .await
    }

    /// Snapshot cursor, subscribe, recheck. A commit in either gap is returned
    /// by wait immediately, even if watch considered it seen at subscription.
    pub async fn subscribe(&self, writer: &Writer, keys: Vec<ChangeKey>) -> Result<Subscription> {
        let cursor = self.cursor(keys).await?;
        self.subscribe_after_cursor(writer, cursor).await
    }

    /// Also supports callers whose durable cursor came from a larger snapshot.
    pub async fn subscribe_after_cursor(
        &self,
        writer: &Writer,
        cursor: DurableCursor,
    ) -> Result<Subscription> {
        if std::fs::canonicalize(writer.home())? != self.inner.home {
            return Err(sluice_model::error::PublicError::BadRequest {
                message: "subscription writer and reader must use the same home".into(),
            }
            .into());
        }
        let receiver = writer.subscribe();
        let seen = receiver.borrow().commits;
        let keys = cursor.versions.keys().cloned().collect::<Vec<_>>();
        let latest = self.cursor(keys.clone()).await?;
        Ok(Subscription {
            pool: self.clone(),
            keys,
            receiver,
            pending: (latest != cursor).then_some(latest),
            cursor,
            seen,
        })
    }
}

pub struct Subscription {
    pool: ReadPool,
    keys: Vec<ChangeKey>,
    receiver: watch::Receiver<ChangeNotification>,
    cursor: DurableCursor,
    pending: Option<DurableCursor>,
    /// The notification commit count last examined.
    seen: u64,
}
impl Subscription {
    pub fn cursor(&self) -> &DurableCursor {
        &self.cursor
    }
    /// Rechecks all interested versions when a commit may have touched one of
    /// them and at least every 30s. A notification that is exactly the next
    /// commit and names none of the keys is skipped without a read; a gap in
    /// the commit count means coalesced sets, so it rechecks. Irrelevant wakes
    /// do not produce a change. The cursor advances on return.
    pub async fn wait(&mut self) -> Result<DurableCursor> {
        if let Some(cursor) = self.pending.take() {
            self.cursor = cursor;
            return Ok(self.cursor.clone());
        }
        loop {
            self.seen = self.receiver.borrow_and_update().commits;
            let latest = self.pool.cursor(self.keys.clone()).await?;
            if latest != self.cursor {
                self.cursor = latest;
                return Ok(self.cursor.clone());
            }
            loop {
                match tokio::time::timeout(Duration::from_secs(30), self.receiver.changed()).await {
                    Err(_) => break,
                    Ok(Ok(())) => {
                        let notification = self.receiver.borrow_and_update();
                        let gap = notification.commits.wrapping_sub(self.seen) != 1;
                        self.seen = notification.commits;
                        if gap || notification.changed.iter().any(|k| self.keys.contains(k)) {
                            break;
                        }
                    }
                    Ok(Err(_)) => {
                        // One last durable read when the writer shuts down.
                        let latest = self.pool.cursor(self.keys.clone()).await?;
                        if latest != self.cursor {
                            self.cursor = latest;
                            return Ok(self.cursor.clone());
                        }
                        return Err(StoreError::Closed);
                    }
                }
            }
        }
    }
}
fn snapshot_authorizer(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Select
        | AuthAction::Read { .. }
        | AuthAction::Function { .. }
        | AuthAction::Recursive => Authorization::Allow,
        _ => Authorization::Deny,
    }
}
