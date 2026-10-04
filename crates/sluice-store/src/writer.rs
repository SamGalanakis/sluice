//! Single writable connection on a dedicated OS thread.
//!
//! Store modules submit short synchronous closures. No filesystem/process work,
//! waits or async execution belongs in a closure. Once enqueued, a command runs
//! even if its awaiting caller disappears. `RetrySafety` classifies errors; it
//! does not implement deduplication. Owners must check their durable request key.

use crate::schema::{self, Result, StoreError};
use rusqlite::{
    Connection, Transaction, TransactionBehavior,
    hooks::{AuthAction, AuthContext, Authorization},
};
use sluice_model::{
    error::PublicError,
    events::{Event, Record},
    ids::{MessageId, ProjectId, RecordSeq},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrySafety {
    NonIdempotent,
    Idempotent,
}
impl RetrySafety {
    pub fn is_idempotent(self) -> bool {
        self == Self::Idempotent
    }
}

/// NULL project means home scope. View strings are stable module-owned names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChangeKey {
    pub project: Option<ProjectId>,
    pub view: String,
}
impl ChangeKey {
    pub fn new(project: Option<ProjectId>, view: impl Into<String>) -> Self {
        Self {
            project,
            view: view.into(),
        }
    }
    pub fn scope(&self) -> String {
        self.project
            .map_or_else(|| "home".into(), |id| id.to_string())
    }
}

/// Latest committed dirty set, coalesced by watch. Slow consumers may skip sets;
/// durable versions for all their interests must be rechecked on any wake/timer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeNotification {
    pub commits: u64,
    pub changed: BTreeSet<ChangeKey>,
}

#[derive(Debug, Clone)]
pub struct WriterOptions {
    pub queue_capacity: usize,
    pub busy_timeout: Duration,
}
impl Default for WriterOptions {
    fn default() -> Self {
        Self {
            queue_capacity: 64,
            busy_timeout: Duration::from_secs(5),
        }
    }
}

/// Actor-owned transaction. SQL access is for trusted store modules only.
/// Do not replace hooks or connection settings. Transaction/DDL/ATTACH SQL is
/// denied during a request. Every mutation must call `changed` or append a record.
pub struct WriteTransaction<'connection> {
    transaction: Transaction<'connection>,
    changed: BTreeSet<ChangeKey>,
}
impl WriteTransaction<'_> {
    pub fn sql(&self) -> &Connection {
        &self.transaction
    }
    pub fn changed(&mut self, project: Option<ProjectId>, view: impl Into<String>) {
        self.changed.insert(ChangeKey::new(project, view));
    }
    /// Append in this state-change transaction. Returns the allocated typed record.
    /// Message payload ids are replaced with the allocated seq before commit.
    pub fn append_record(
        &mut self,
        project: Option<ProjectId>,
        mut event: Event,
    ) -> Result<Record> {
        let at = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|e| StoreError::InvalidDatabase(e.to_string()))?;
        let mut payload = serde_json::to_value(&event)?;
        let kind = payload
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| StoreError::InvalidDatabase("event kind missing".into()))?
            .to_owned();
        let field = |name: &str| {
            payload
                .get(name)
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        };
        self.sql().execute(
            "INSERT INTO records(project_id,at,kind,payload_version,payload,step_id,call_id,thread,run_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![project.map(|id| id.to_string()), at, kind,
                schema::RECORD_PAYLOAD_VERSION, serde_json::to_string(&payload)?,
                field("step"), field("call"), field("thread"), field("run")],
        )?;
        let seq = RecordSeq(self.sql().last_insert_rowid());
        if let Event::Message(message) = &mut event {
            message.id = MessageId(seq.0);
            payload = serde_json::to_value(&event)?;
            self.sql().execute(
                "UPDATE records SET payload=?1 WHERE seq=?2",
                (serde_json::to_string(&payload)?, seq.0),
            )?;
        }
        self.changed(project, "log");
        self.changed(None, "log");
        Ok(Record {
            seq,
            at,
            project,
            event,
        })
    }
}

/// Shared helper for module functions that already have a transaction.
pub fn append_record(
    tx: &mut WriteTransaction<'_>,
    project: Option<ProjectId>,
    event: Event,
) -> Result<Record> {
    tx.append_record(project, event)
}

type Operation<T> = Box<dyn FnOnce(&mut WriteTransaction<'_>) -> Result<T> + Send>;
trait Job: Send {
    fn run(
        self: Box<Self>,
        connection: &mut Connection,
        changes: &watch::Sender<ChangeNotification>,
    );
}
struct Request<T> {
    operation: Operation<T>,
    reply: oneshot::Sender<std::result::Result<T, PublicError>>,
    safety: RetrySafety,
}
impl<T: Send + 'static> Job for Request<T> {
    fn run(
        self: Box<Self>,
        connection: &mut Connection,
        changes: &watch::Sender<ChangeNotification>,
    ) {
        let result = transact(connection, self.operation, changes)
            .map_err(|error| error.into_public(self.safety.is_idempotent()));
        let _ = self.reply.send(result);
    }
}

enum Command {
    Write(Box<dyn Job>),
    Shutdown(oneshot::Sender<()>),
}
struct Inner {
    sender: mpsc::Sender<Command>,
    admitted: std::sync::atomic::AtomicU64,
    changes: watch::Receiver<ChangeNotification>,
    thread: Mutex<Option<JoinHandle<()>>>,
    home: PathBuf,
}

/// Clones share one actor. Explicit shutdown closes admission, drains accepted
/// commands and joins on a blocking thread. Last-handle drop closes the queue;
/// the thread drains it and releases its flock without blocking Drop.
#[derive(Clone)]
pub struct Writer {
    inner: Arc<Inner>,
}
impl Writer {
    pub fn open(home: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(home, WriterOptions::default())
    }
    /// Synchronous startup handshake. Call during coordinator startup or from a
    /// blocking task, not a Tokio worker handling live requests.
    pub fn open_with_options(home: impl AsRef<Path>, options: WriterOptions) -> Result<Self> {
        if options.queue_capacity == 0 {
            return Err(PublicError::BadRequest {
                message: "writer queue capacity must be positive".into(),
            }
            .into());
        }
        let home = home.as_ref().to_path_buf();
        let actor_home = home.clone();
        let (sender, mut receiver) = mpsc::channel(options.queue_capacity);
        let (changes, change_receiver) = watch::channel(ChangeNotification::default());
        let (started, startup) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("sluice-sqlite-writer".into())
            .spawn(move || {
                let opened = schema::lock_home(&actor_home).and_then(|lock| {
                    schema::open_writer(&actor_home, options.busy_timeout).map(|db| (lock, db))
                });
                let (lock, mut connection) = match opened {
                    Ok(opened) => {
                        if started.send(Ok(())).is_err() {
                            return;
                        }
                        opened
                    }
                    Err(error) => {
                        let _ = started.send(Err(error));
                        return;
                    }
                };
                let mut shutdown_replies = Vec::new();
                while let Some(command) = receiver.blocking_recv() {
                    match command {
                        Command::Write(job) => job.run(&mut connection, &changes),
                        Command::Shutdown(reply) => {
                            receiver.close();
                            shutdown_replies.push(reply);
                        }
                    }
                }
                drop(connection);
                drop(lock);
                for reply in shutdown_replies {
                    let _ = reply.send(());
                }
            })?;
        match startup.recv() {
            Ok(Ok(())) => Ok(Self {
                inner: Arc::new(Inner {
                    sender,
                    admitted: Default::default(),
                    changes: change_receiver,
                    thread: Mutex::new(Some(thread)),
                    home,
                }),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(StoreError::Closed)
            }
        }
    }
    pub fn home(&self) -> &Path {
        &self.inner.home
    }
    /// How many write requests this actor has admitted: what callers cost the
    /// one writer, for diagnostics and tests.
    pub fn transactions(&self) -> u64 {
        self.inner
            .admitted
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn subscribe(&self) -> watch::Receiver<ChangeNotification> {
        let mut receiver = self.inner.changes.clone();
        receiver.borrow_and_update();
        receiver
    }
    /// FIFO by queue admission. Awaiting cancellation before admission writes
    /// nothing; cancellation after admission does not cancel the transaction.
    pub async fn write<T, F>(
        &self,
        safety: RetrySafety,
        operation: F,
    ) -> std::result::Result<T, PublicError>
    where
        T: Send + 'static,
        F: FnOnce(&mut WriteTransaction<'_>) -> Result<T> + Send + 'static,
    {
        let permit = self
            .inner
            .sender
            .reserve()
            .await
            .map_err(|_| StoreError::Closed.into_public(false))?;
        let (reply, result) = oneshot::channel();
        self.inner
            .admitted
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        permit.send(Command::Write(Box::new(Request {
            operation: Box::new(operation),
            reply,
            safety,
        })));
        result
            .await
            .map_err(|_| StoreError::Closed.into_public(false))?
    }
    pub async fn shutdown(&self) -> Result<()> {
        let (reply, finished) = oneshot::channel();
        if self
            .inner
            .sender
            .send(Command::Shutdown(reply))
            .await
            .is_ok()
        {
            finished.await.map_err(|_| StoreError::Closed)?;
        }
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut thread_guard = inner
                .thread
                .lock()
                .map_err(|_| StoreError::RequestPanicked)?;
            if let Some(thread) = thread_guard.take() {
                thread.join().map_err(|_| StoreError::RequestPanicked)?;
            }
            Ok(())
        })
        .await
        .map_err(|_| StoreError::RequestPanicked)?
    }
}

fn request_authorizer(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Select
        | AuthAction::Read { .. }
        | AuthAction::Insert { .. }
        | AuthAction::Update { .. }
        | AuthAction::Delete { .. }
        | AuthAction::Function { .. }
        | AuthAction::Recursive => Authorization::Allow,
        AuthAction::Pragma {
            pragma_name: "foreign_keys" | "synchronous" | "busy_timeout" | "journal_mode",
            pragma_value: None,
        } => Authorization::Allow,
        AuthAction::Pragma {
            pragma_name: "defer_foreign_keys",
            ..
        } => Authorization::Allow,
        _ => Authorization::Deny,
    }
}

fn transact<T>(
    connection: &mut Connection,
    operation: Operation<T>,
    changes: &watch::Sender<ChangeNotification>,
) -> Result<T> {
    let before = connection.total_changes();
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.authorizer(Some(request_authorizer))?;
    let mut tx = WriteTransaction {
        transaction,
        changed: BTreeSet::new(),
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(&mut tx)));
    tx.sql()
        .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
    let value = result.map_err(|_| StoreError::RequestPanicked)??;
    if tx.sql().total_changes() != before && tx.changed.is_empty() {
        return Err(PublicError::Invalid {
            message: "a store mutation must declare its changed views".into(),
            errors: vec![],
        }
        .into());
    }
    for key in &tx.changed {
        tx.sql().execute(
            "INSERT INTO change_versions(scope,project_id,view,version) VALUES (?1,?2,?3,1)
             ON CONFLICT(scope,view) DO UPDATE SET version=version+1",
            (key.scope(), key.project.map(|id| id.to_string()), &key.view),
        )?;
    }
    tx.transaction.commit()?;
    if !tx.changed.is_empty() {
        changes.send_modify(|notification| {
            notification.commits = notification.commits.wrapping_add(1);
            notification.changed = tx.changed;
        });
    }
    Ok(value)
}
