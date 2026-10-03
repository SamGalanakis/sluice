//! Durable, complete filesystem generations. The manifest is the recovery input;
//! only `done` jobs are published. A single worker lock serializes filesystem work.
//! Linux directory descriptors anchor traversal beneath a coordinator-owned home.
//! Mount administrators and other malicious same-uid writers are outside this boundary.

use crate::{Result, RetrySafety, StoreError, WriteTransaction, Writer};
use fs4::FileExt;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use sluice_model::{
    error::PublicError,
    hash::ExecutionProvenance,
    ids::{InvocationId, ProjectId, RunId},
};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Component, Path, PathBuf},
};

const DIRECTORY_NOFOLLOW: i32 = 0x10000 | 0x20000;
const NOFOLLOW: i32 = 0x20000;
const BUNDLE_MAX: usize = 16 * 1024 * 1024;

pub(crate) fn invalid(message: impl Into<String>) -> StoreError {
    PublicError::BadRequest {
        message: message.into(),
    }
    .into()
}
pub(crate) fn now() -> Result<String> {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| invalid(e.to_string()))
}
pub fn fingerprint(bytes: &[u8]) -> String {
    ExecutionProvenance::fingerprint(bytes).to_string()
}

/// Immutable owned bytes, not source paths. Nested sibling helpers travel together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    files: BTreeMap<String, Vec<u8>>,
}
impl Bundle {
    pub fn new(files: BTreeMap<String, Vec<u8>>) -> Result<Self> {
        if files.len() > 1024 || files.values().map(Vec::len).sum::<usize>() > BUNDLE_MAX {
            return Err(invalid("bundle exceeds 1024 files or 16 MiB"));
        }
        for name in files.keys() {
            relative(name)?;
        }
        // A file cannot also be an ancestor of another file.
        for name in files.keys() {
            for ancestor in Path::new(name).ancestors().skip(1) {
                if ancestor.to_str().is_some_and(|a| files.contains_key(a)) {
                    return Err(invalid("bundle file is also a directory"));
                }
            }
        }
        Ok(Self { files })
    }
    pub fn files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }
    fn manifest(&self) -> Value {
        let files: BTreeMap<_, _> = self
            .files
            .iter()
            .map(|(p, b)| (p.clone(), hex(b)))
            .collect();
        let digest = fingerprint(&serde_json::to_vec(&files).expect("string map serializes"));
        json!({"version":1,"files":files,"fingerprint":digest})
    }
    fn from_manifest(manifest: &Value) -> Result<Self> {
        if manifest["version"] != 1 {
            return Err(invalid("unsupported artifact manifest"));
        }
        let files = manifest["files"]
            .as_object()
            .ok_or_else(|| invalid("missing artifact files"))?;
        let files = files
            .iter()
            .map(|(p, v)| {
                Ok((
                    p.clone(),
                    unhex(
                        v.as_str()
                            .ok_or_else(|| invalid("invalid artifact bytes"))?,
                    )?,
                ))
            })
            .collect::<Result<_>>()?;
        let bundle = Self::new(files)?;
        if bundle.manifest()["fingerprint"] != manifest["fingerprint"] {
            return Err(invalid("artifact fingerprint mismatch"));
        }
        Ok(bundle)
    }
}
pub(crate) fn manifest(bundle: Bundle) -> Value {
    bundle.manifest()
}
pub(crate) fn decode_manifest(value: &Value) -> Result<Bundle> {
    Bundle::from_manifest(value)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(value: &str) -> Result<Vec<u8>> {
    if value.len() > BUNDLE_MAX * 2
        || !value.len().is_multiple_of(2)
        || !value.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(invalid("invalid artifact hex"));
    }
    (0..value.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&value[i..i + 2], 16).map_err(|_| invalid("invalid artifact hex"))
        })
        .collect()
}
fn relative(path: &str) -> Result<&Path> {
    let parsed = Path::new(path);
    if path.len() > 512
        || path.split('/').count() > 32
        || path.is_empty()
        || path.contains('\0')
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        || !parsed
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(invalid(
            "artifact path must be a nonempty relative path without dot components",
        ));
    }
    Ok(parsed)
}

#[derive(Debug, Clone, Copy)]
pub enum BundleScope {
    Home,
    Project(ProjectId),
}
impl BundleScope {
    fn base(self) -> String {
        match self {
            Self::Home => "fns/generations".into(),
            Self::Project(id) => format!("projects/{id}/generations"),
        }
    }
    fn project(self) -> Option<ProjectId> {
        match self {
            Self::Home => None,
            Self::Project(id) => Some(id),
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactJob {
    pub job_id: InvocationId,
    pub project_id: Option<ProjectId>,
    pub run_id: Option<RunId>,
    pub kind: String,
    pub generation: i64,
    pub path: String,
    pub manifest: Value,
    pub state: String,
}
fn parse_id<T: std::str::FromStr>(s: String) -> Result<T> {
    s.parse()
        .map_err(|_| invalid("invalid persisted artifact id"))
}
pub fn job(connection: &Connection, id: InvocationId) -> Result<ArtifactJob> {
    let raw = connection.query_row("SELECT project_id,run_id,kind,generation,path,manifest,state FROM artifact_jobs WHERE job_id=?1",[id.to_string()],|r| Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?))).optional()?.ok_or_else(|| StoreError::from(PublicError::NotFound{message:"artifact job not found".into()}))?;
    Ok(ArtifactJob {
        job_id: id,
        project_id: raw.0.map(parse_id).transpose()?,
        run_id: raw.1.map(parse_id).transpose()?,
        kind: raw.2,
        generation: raw.3,
        path: raw.4,
        manifest: serde_json::from_str(&raw.5)?,
        state: raw.6,
    })
}
pub(crate) fn enqueue(
    tx: &mut WriteTransaction<'_>,
    project: Option<ProjectId>,
    run: Option<RunId>,
    kind: &str,
    generation: i64,
    path: String,
    manifest: Value,
) -> Result<InvocationId> {
    relative(&path)?;
    let id = InvocationId::new();
    tx.sql().execute("INSERT INTO artifact_jobs(job_id,project_id,run_id,kind,generation,path,manifest,created_at,state) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'pending')", rusqlite::params![id.to_string(), project.map(|p|p.to_string()),run.map(|r|r.to_string()),kind,generation,path,manifest.to_string(),now()?])?;
    tx.changed(project, "artifacts");
    Ok(id)
}
/// Queue a full immutable bundle. Registry readers select published manifests only.
pub fn stage_generation(
    tx: &mut WriteTransaction<'_>,
    scope: BundleScope,
    bundle: Bundle,
) -> Result<InvocationId> {
    if let Some(id) = scope.project() {
        crate::projects::resolve(tx.sql(), &sluice_model::ids::ProjectSelector::Id(id))?;
    }
    let base = scope.base();
    let generation: i64 = tx.sql().query_row("SELECT coalesce(max(generation),0)+1 FROM artifact_jobs WHERE kind='generation' AND (path LIKE ?1)", [format!("{base}/%")], |r|r.get(0))?;
    enqueue(
        tx,
        scope.project(),
        None,
        "generation",
        generation,
        format!("{base}/{generation}"),
        bundle.manifest(),
    )
}
/// A pin is durable and may be made before execution. The run must exist and match scope.
pub fn pin_generation(
    tx: &mut WriteTransaction<'_>,
    generation_job: InvocationId,
    run: RunId,
) -> Result<()> {
    let target = job(tx.sql(), generation_job)?;
    if target.kind != "generation" || target.state != "done" {
        return Err(invalid("only published generations can be pinned"));
    }
    let owner: Option<String> = tx
        .sql()
        .query_row(
            "SELECT project_id FROM runs WHERE run_id=?1 AND finished_at IS NULL",
            [run.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| invalid("pin requires a live run"))?;
    if target.project_id.is_some() && owner != target.project_id.map(|p| p.to_string()) {
        return Err(invalid("generation and run scopes differ"));
    }
    tx.sql().execute("INSERT INTO artifact_jobs(job_id,project_id,run_id,kind,generation,path,manifest,state,created_at,finished_at) VALUES (?1,?2,?3,'pin',?4,?5,?6,'done',?7,?7) ON CONFLICT(path,generation,kind) DO NOTHING",rusqlite::params![InvocationId::new().to_string(),owner,run.to_string(),target.generation,format!("pins/{run}/{generation_job}"),json!({"target":generation_job.to_string()}).to_string(),now()?])?;
    tx.changed(target.project_id, "artifacts");
    Ok(())
}
fn pinned(c: &Connection, path: &str) -> Result<bool> {
    Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM artifact_jobs p JOIN artifact_jobs g ON json_extract(p.manifest,'$.target')=g.job_id JOIN runs r ON r.run_id=p.run_id WHERE p.kind='pin' AND p.state='done' AND g.path=?1 AND (r.finished_at IS NULL OR EXISTS(SELECT 1 FROM runs n WHERE n.prev_run=r.run_id AND n.finished_at IS NULL)))",[path],|r|r.get(0))?)
}
/// Retire an unpinned, superseded bundle. New admission cannot pin a retiring job.
pub fn retire_generation(tx: &mut WriteTransaction<'_>, id: InvocationId) -> Result<InvocationId> {
    let target = job(tx.sql(), id)?;
    if target.kind != "generation" || target.state != "done" || pinned(tx.sql(), &target.path)? {
        return Err(invalid("generation is not published or is pinned"));
    }
    let newer: bool = tx.sql().query_row("SELECT EXISTS(SELECT 1 FROM artifact_jobs WHERE kind='generation' AND state='done' AND project_id IS ?1 AND generation>?2)",rusqlite::params![target.project_id.map(|p|p.to_string()),target.generation],|r|r.get(0))?;
    if !newer {
        return Err(invalid("cannot retire the current generation"));
    }
    tx.sql().execute(
        "UPDATE artifact_jobs SET state='failed',error=?2 WHERE job_id=?1",
        [id.to_string(), json!({"retired":true}).to_string()],
    )?;
    enqueue(
        tx,
        target.project_id,
        None,
        "cleanup",
        target.generation,
        target.path,
        json!({"version":1}),
    )
}
pub fn published_generation(c: &Connection, scope: BundleScope) -> Result<Option<ArtifactJob>> {
    let id: Option<String> = c.query_row("SELECT job_id FROM artifact_jobs WHERE kind='generation' AND state='done' AND project_id IS ?1 ORDER BY generation DESC LIMIT 1",[scope.project().map(|p|p.to_string())],|r|r.get(0)).optional()?;
    id.map(parse_id)
        .transpose()?
        .map(|id| job(c, id))
        .transpose()
}

/// Descriptor-relative walker. `/proc/self/fd` names only already-open directories;
/// each untrusted component is opened with O_DIRECTORY|O_NOFOLLOW.
struct Directory(File);
impl Directory {
    fn open(path: &Path) -> Result<Self> {
        Ok(Self(
            OpenOptions::new()
                .read(true)
                .custom_flags(DIRECTORY_NOFOLLOW)
                .open(path)?,
        ))
    }
    fn path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.0.as_raw_fd()))
    }
    fn child(&self, name: &str, create: bool) -> Result<Self> {
        relative(name)?;
        if name.contains('/') {
            return Err(invalid("expected one directory component"));
        }
        let path = self.path().join(name);
        if create {
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => self.0.sync_all()?,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.into()),
            }
        }
        Self::open(&path)
    }
    fn walk(&self, path: &str, create: bool) -> Result<Self> {
        relative(path)?;
        let mut dir = Self(self.0.try_clone()?);
        for part in path.split('/') {
            dir = dir.child(part, create)?;
        }
        Ok(dir)
    }
    fn parent(&self, path: &str, create: bool) -> Result<(Self, String)> {
        relative(path)?;
        match path.rsplit_once('/') {
            Some((parent, leaf)) => Ok((self.walk(parent, create)?, leaf.into())),
            None => Ok((Self(self.0.try_clone()?), path.into())),
        }
    }
    fn remove(&self, leaf: &str) -> Result<()> {
        let path = self.path().join(leaf);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => fs::remove_dir_all(&path)?,
            Ok(_) => fs::remove_file(&path)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        self.0.sync_all()?;
        Ok(())
    }
}
/// All cleanup destinations have a closed, id-based grammar, never arbitrary paths.
pub fn validate_cleanup_path(path: &str) -> Result<()> {
    relative(path)?;
    let parts: Vec<_> = path.split('/').collect();
    let valid = match parts.as_slice() {
        ["projects", id] => id.parse::<ProjectId>().is_ok(),
        ["runs", id] => id.parse::<RunId>().is_ok(),
        ["engine-homes", id] => id.parse::<InvocationId>().is_ok(),
        ["projects", id, "generations" | "icons", generation] => {
            id.parse::<ProjectId>().is_ok() && positive(generation)
        }
        ["fns", "generations", generation] => positive(generation),
        _ => false,
    };
    if !valid {
        return Err(invalid("cleanup path is not an owned id path"));
    }
    Ok(())
}
fn positive(s: &str) -> bool {
    s.parse::<i64>().is_ok_and(|n| n > 0 && n.to_string() == s)
}
fn missing(error: &StoreError) -> bool {
    matches!(error,StoreError::Io(e) if e.kind()==std::io::ErrorKind::NotFound)
}

/// Execute/recover one job. Call outside the writer thread; I/O uses spawn_blocking.
/// Interrupted staging and post-rename/pre-commit windows replay from frozen bytes.
pub async fn execute(writer: &Writer, home: &Path, id: InvocationId) -> Result<()> {
    let worker = writer.clone();
    let home = home.to_path_buf();
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let root = Directory::open(&home)?;
        let database_path = handle.block_on(worker.write(RetrySafety::Idempotent, |tx| {
            tx.sql().path().map(PathBuf::from).ok_or_else(||invalid("writer has no database path"))
        })).map_err(StoreError::from)?;
        let expected = OpenOptions::new().read(true).custom_flags(NOFOLLOW).open(database_path)?.metadata()?;
        let actual = OpenOptions::new().read(true).custom_flags(NOFOLLOW).open(root.path().join(crate::schema::DATABASE_FILE))?.metadata()?;
        if (actual.dev(),actual.ino())!=(expected.dev(),expected.ino()) {
            return Err(invalid("artifact home does not match the writer home"));
        }
        let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false).custom_flags(NOFOLLOW).mode(0o600).open(root.path().join("artifact-worker.lock"))?;
        FileExt::lock(&lock)?;
        let current = handle.block_on(worker.write(RetrySafety::Idempotent,move |tx| {
            let current=job(tx.sql(),id)?;
            if current.state=="done" || current.state=="failed" { return Ok(current); }
            if current.kind=="cleanup" && pinned(tx.sql(),&current.path)? { return Err(invalid("cleanup destination is pinned")); }
            tx.sql().execute("UPDATE artifact_jobs SET state='running',error=NULL WHERE job_id=?1",[id.to_string()])?;
            tx.changed(current.project_id,"artifacts");
            Ok(current)
        })).map_err(StoreError::from)?;
        if current.state=="done" || current.state=="failed" { return Ok(()); }
        let result = apply(&root,&current);
        let failed = result.as_ref().err().map(|e|json!({"message":e.to_string()}).to_string());
        handle.block_on(worker.write(RetrySafety::Idempotent, move |tx| {
            // Deletion may have cancelled this job while it was writing. Cleanup is
            // serialized after this worker, so no late writer can resurrect a path.
            tx.sql().execute("UPDATE artifact_jobs SET state=?2,error=?3,finished_at=?4 WHERE job_id=?1 AND state='running'",rusqlite::params![id.to_string(),if failed.is_some(){"pending"}else{"done"},failed,if failed.is_some(){None}else{Some(now()?)}])?;
            tx.changed(current.project_id,"artifacts");
            Ok(())
        })).map_err(StoreError::from)?;
        result
    }).await.map_err(|e| invalid(e.to_string()))?
}
fn apply(root: &Directory, job: &ArtifactJob) -> Result<()> {
    match job.kind.as_str() {
        "project_dir" => {
            validate_cleanup_path(&job.path)?;
            root.walk(&job.path, true)?.child("fns", true)?;
            Ok(())
        }
        "generation" | "icon" => {
            validate_cleanup_path(&job.path)?;
            let bundle = Bundle::from_manifest(&job.manifest)?;
            let (parent, leaf) = root.parent(&job.path, true)?;
            match parent.child(&leaf, false) {
                Ok(existing) => verify(&existing, &bundle),
                Err(e) if missing(&e) => {
                    let stage = format!(".stage-{}", job.job_id);
                    parent.remove(&stage)?;
                    let staging = parent.child(&stage, true)?;
                    for (name, bytes) in bundle.files() {
                        let (dir, file) = staging.parent(name, true)?;
                        let mut output = OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(dir.path().join(file))?;
                        output.write_all(bytes)?;
                        output.sync_all()?;
                        dir.0.sync_all()?;
                    }
                    staging.0.sync_all()?;
                    fs::rename(parent.path().join(&stage), parent.path().join(&leaf))?;
                    parent.0.sync_all()?;
                    verify(&parent.child(&leaf, false)?, &bundle)
                }
                Err(e) => Err(e),
            }
        }
        "cleanup" => {
            validate_cleanup_path(&job.path)?;
            let trash = root.child("trash", true)?;
            let leaf = job.job_id.to_string();
            // A prior crash can leave the moved directory in trash.
            trash.remove(&leaf)?;
            match root.parent(&job.path, false) {
                Ok((parent, source)) => {
                    match fs::rename(parent.path().join(&source), trash.path().join(&leaf)) {
                        Ok(()) => {
                            parent.0.sync_all()?;
                            trash.0.sync_all()?;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                        Err(e) => return Err(e.into()),
                    }
                    trash.remove(&leaf)
                }
                Err(e) if missing(&e) => Ok(()),
                Err(e) => Err(e),
            }
        }
        _ => Err(invalid("unknown artifact job kind")),
    }
}
fn verify(dir: &Directory, bundle: &Bundle) -> Result<()> {
    let mut actual_paths = std::collections::BTreeSet::new();
    let mut pending = vec![(Directory(dir.0.try_clone()?), String::new())];
    let mut count = 0;
    while let Some((parent, prefix)) = pending.pop() {
        for entry in fs::read_dir(parent.path())? {
            count += 1;
            if count > 32768 {
                return Err(invalid("generation has too many entries"));
            }
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| invalid("non-UTF8 generation entry"))?;
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            relative(&path)?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push((parent.child(&name, false)?, path));
            } else if kind.is_file() {
                actual_paths.insert(path);
            } else {
                return Err(invalid("generation contains a link or special file"));
            }
        }
    }
    if actual_paths != bundle.files.keys().cloned().collect() {
        return Err(invalid("generation file set differs from manifest"));
    }
    for (name, bytes) in bundle.files() {
        let (parent, leaf) = dir.parent(name, false)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(NOFOLLOW)
            .open(parent.path().join(leaf))?;
        if !file.metadata()?.is_file() {
            return Err(invalid("artifact is not a file"));
        }
        let mut actual = Vec::new();
        file.take(bytes.len() as u64 + 1).read_to_end(&mut actual)?;
        if actual != *bytes {
            return Err(invalid("published generation has different bytes"));
        }
    }
    Ok(())
}
/// Boot recovery processes pending and interrupted jobs in creation order.
pub async fn recover(writer: &Writer, home: &Path) -> Result<usize> {
    let ids=writer.write(RetrySafety::Idempotent,|tx| {
        let mut query=tx.sql().prepare("SELECT job_id FROM artifact_jobs WHERE state IN ('pending','running') ORDER BY created_at,job_id")?;
        let ids=query.query_map([],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        ids.into_iter().map(parse_id).collect::<Result<Vec<InvocationId>>>()
    }).await.map_err(StoreError::from)?;
    let count = ids.len();
    let mut failure = None;
    for id in ids {
        if let Err(error) = execute(writer, home, id).await {
            failure.get_or_insert(error);
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(count),
    }
}
