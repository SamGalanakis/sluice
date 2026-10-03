//! git.* builtins: worktree, worktree_rm, head, merge, rebase and push, ported from
//! packs/git. This file also carries the builtin plumbing gh.rs reuses — a minimal
//! BuiltinDescriptor, BuiltinCtx and FnFailure plus bounded argv-only command execution —
//! until P4.02 unifies the builtin contract.
//!
//! Commands run through tokio Command with argv only, never a shell, in the run dir, with
//! the run environment after host-PATH restoration (sluice.fn.child_env). Each stream is
//! captured with a bound. A cancelled or dropped future kills the in-flight child
//! (kill_on_drop), which is how cancellation reaches the external tool. No git builtin
//! reports a command failure as transient: a push whose result is unknown must never be
//! retried.

use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::{
    rpc::{JsonMap, JsonValue, decode_json},
    types::Type,
};
use std::{
    ffi::OsString,
    fmt,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};

/// Upper bound on each captured stream; Python captured unbounded text.
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
/// The stderr tail carried by a command failure, like sluice.fn's `_TAIL`.
const TAIL: usize = 2000;
/// What `uv run` and sluice change for a fn's interpreter, and the runner's
/// SLUICE_HOST_* records restore for the tools a fn starts (sluice.fn.HOST_VARS).
const HOST_VARS: [&str; 3] = ["PATH", "PYTHONPATH", "VIRTUAL_ENV"];

/// A compiled fn's manifest, equal to its fn.json plus the same-run retry budget the
/// pack's `run(main, retries=.., backoff=..)` declared.
pub struct BuiltinDescriptor {
    pub name: &'static str,
    pub doc: &'static str,
    pub inputs: Vec<(&'static str, Type)>,
    pub outputs: Vec<(&'static str, Type)>,
    /// Extra attempts after the first a Transient failure earns (run's `retries`).
    pub retries: u32,
    /// The wait between same-run retries (run's `backoff`; SLUICE_BACKOFF overrides).
    pub backoff: Duration,
}

/// What a builtin sees of its run: the run's environment and its run directory. The run
/// dir is the fn's working directory (the Python runner spawns fns with `cwd=run_dir`),
/// the base for relative path inputs and the scratch space merge's worktree lives in.
#[derive(Debug, Default, Clone)]
pub struct BuiltinCtx {
    pub env: IndexMap<String, String>,
    pub run_dir: PathBuf,
}
impl BuiltinCtx {
    pub fn new(
        env: impl IntoIterator<Item = (String, String)>,
        run_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            env: env.into_iter().collect(),
            run_dir: run_dir.into(),
        }
    }
}

/// How a fn run ended. Transient is retryable within the run under the spec's same-run
/// retry rule (the Python helper's `Transient`); Terminal fails the step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FnFailure {
    Transient(String),
    Terminal(String),
}
impl FnFailure {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
    pub(crate) fn terminal(message: impl Into<String>) -> Self {
        Self::Terminal(message.into())
    }
}
impl fmt::Display for FnFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transient(m) | Self::Terminal(m) => f.write_str(m),
        }
    }
}
impl std::error::Error for FnFailure {}

/// The fn.json type grammar these fns declare: names, `?` optional and `[]` list suffixes.
pub(crate) fn ty(form: &str) -> Type {
    if let Some(inner) = form.strip_suffix('?') {
        return Type::Optional(Box::new(ty(inner)));
    }
    if let Some(inner) = form.strip_suffix("[]") {
        return Type::List(Box::new(ty(inner)));
    }
    match form {
        "string" => Type::String,
        "int" => Type::Int,
        "float" => Type::Float,
        "boolean" => Type::Boolean,
        "Any" => Type::Any,
        other => panic!("unknown fn type {other:?}"),
    }
}
pub(crate) fn ports(forms: &[(&'static str, &'static str)]) -> Vec<(&'static str, Type)> {
    forms.iter().map(|(name, form)| (*name, ty(form))).collect()
}

/// Descriptors equal to packs/git/git.*/fn.json, in the same field order. Every git fn
/// runs `run(main)` in Python: no retries, 30s backoff.
pub fn descriptors() -> [BuiltinDescriptor; 6] {
    let d = |name, doc, inputs, outputs| BuiltinDescriptor {
        name,
        doc,
        inputs,
        outputs,
        retries: 0,
        backoff: Duration::from_secs(30),
    };
    [
        d(
            "git.worktree",
            "Add a git worktree on a new or existing branch.",
            ports(&[
                ("repo", "string"),
                ("base", "string"),
                ("branch", "string"),
                ("path", "string?"),
            ]),
            ports(&[("path", "string"), ("branch", "string"), ("sha", "string")]),
        ),
        d(
            "git.worktree_rm",
            "Remove a git worktree; reports whether the path was a worktree.",
            ports(&[
                ("repo", "string"),
                ("path", "string"),
                ("force", "boolean?"),
            ]),
            ports(&[("removed", "boolean")]),
        ),
        d(
            "git.head",
            "Report the current branch and commit of a worktree.",
            ports(&[("path", "string")]),
            ports(&[("branch", "string"), ("sha", "string")]),
        ),
        d(
            "git.merge",
            "Merge source into target in a temporary worktree; conflicts are data, not \
failures.",
            ports(&[
                ("repo", "string"),
                ("source", "string"),
                ("target", "string"),
                ("message", "string?"),
                ("push", "boolean?"),
            ]),
            ports(&[
                ("merged", "boolean"),
                ("sha", "string?"),
                ("conflicts", "string[]"),
            ]),
        ),
        d(
            "git.rebase",
            "Rebase a worktree onto a ref; conflicts abort and are returned as data.",
            ports(&[("path", "string"), ("onto", "string")]),
            ports(&[
                ("ok", "boolean"),
                ("sha", "string"),
                ("conflicts", "string[]"),
            ]),
        ),
        d(
            "git.push",
            "Push HEAD to a branch on a remote, optionally with --force-with-lease.",
            ports(&[
                ("path", "string"),
                ("branch", "string"),
                ("remote", "string?"),
                ("force_with_lease", "boolean?"),
            ]),
            ports(&[("sha", "string")]),
        ),
    ]
}

pub async fn dispatch(
    name: &str,
    inputs: &JsonMap,
    ctx: &BuiltinCtx,
) -> Result<JsonMap, FnFailure> {
    match name {
        "git.worktree" => worktree(inputs, ctx).await,
        "git.worktree_rm" => worktree_rm(inputs, ctx).await,
        "git.head" => head(inputs, ctx).await,
        "git.merge" => merge(inputs, ctx).await,
        "git.rebase" => rebase(inputs, ctx).await,
        "git.push" => push(inputs, ctx).await,
        _ => Err(FnFailure::terminal(format!("unknown git builtin: {name}"))),
    }
}

// ---- sluice.fn ports --------------------------------------------------------

/// A finished process with bounded captures, like subprocess.CompletedProcess.
pub(crate) struct Completed {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// sluice.fn.sh: run argv (never a shell) in `cwd` — relative to the run dir — with the
/// child_env environment, capture bounded stdout/stderr, and raise on a non-zero exit
/// when `check`. `argv` entries are passed through verbatim; `argv[0]` resolves on the
/// child's PATH.
pub(crate) async fn sh(
    ctx: &BuiltinCtx,
    argv: &[OsString],
    cwd: Option<&Path>,
    check: bool,
) -> Result<Completed, FnFailure> {
    tracing::debug!(command = %display_argv(argv), dir = %cwd.unwrap_or(&ctx.run_dir).display(), "sh");
    let program = argv[0].to_string_lossy().into_owned();
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(match cwd {
            // A relative cwd lands under the run dir, like the fn process's cwd.
            Some(dir) => ctx.run_dir.join(dir),
            None => ctx.run_dir.clone(),
        })
        .env_clear()
        .envs(child_env(ctx))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| FnFailure::terminal(format!("{program}: {e}")))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (out, err, status) = tokio::join!(capture(stdout), capture(stderr), child.wait());
    let status = status.map_err(|e| FnFailure::terminal(format!("{program}: wait: {e}")))?;
    let (out, out_over) = out?;
    let (err, err_over) = err?;
    if out_over || err_over {
        return Err(FnFailure::terminal(format!(
            "{program} wrote more than {OUTPUT_LIMIT} bytes of output"
        )));
    }
    let done = Completed {
        code: exit_code(&status),
        stdout: String::from_utf8_lossy(&out).into_owned(),
        stderr: String::from_utf8_lossy(&err).into_owned(),
    };
    if check && done.code != 0 {
        return Err(sh_error(argv, &done));
    }
    Ok(done)
}

/// sluice.fn.ShError's message: `{argv0} exited {code}: {stderr tail}`.
pub(crate) fn sh_error(argv: &[OsString], p: &Completed) -> FnFailure {
    let stripped = p.stderr.trim();
    let tail: String = stripped
        .chars()
        .skip(stripped.chars().count().saturating_sub(TAIL))
        .collect();
    FnFailure::terminal(format!(
        "{} exited {}: {tail}",
        argv[0].to_string_lossy(),
        p.code
    ))
}

/// Drain a stream, keeping up to OUTPUT_LIMIT bytes and flagging overflow.
async fn capture<R: AsyncReadExt + Unpin>(mut reader: R) -> Result<(Vec<u8>, bool), FnFailure> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let mut overflow = false;
    loop {
        let n = reader
            .read(&mut chunk)
            .await
            .map_err(|e| FnFailure::terminal(format!("read pipe: {e}")))?;
        if n == 0 {
            break;
        }
        if overflow || buf.len() + n > OUTPUT_LIMIT {
            overflow = true;
        } else {
            buf.extend_from_slice(&chunk[..n]);
        }
    }
    Ok((buf, overflow))
}

/// Python's returncode: the exit code, or `-signal` when the child died on one.
fn exit_code(status: &std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status
            .code()
            .unwrap_or_else(|| -status.signal().unwrap_or_default())
    }
    #[cfg(not(unix))]
    {
        status.code().unwrap_or(-1)
    }
}

/// sluice.fn.child_env: the run env with PATH, PYTHONPATH and VIRTUAL_ENV put back the
/// way the host had them (SLUICE_HOST_* records), so the tools a fn starts see the
/// host's interpreter paths instead of the fn's own setup.
fn child_env(ctx: &BuiltinCtx) -> IndexMap<String, String> {
    let mut env = ctx.env.clone();
    if HOST_VARS
        .iter()
        .all(|k| env.contains_key(&format!("SLUICE_HOST_{k}")))
    {
        for k in HOST_VARS {
            let host = env
                .get(&format!("SLUICE_HOST_{k}"))
                .cloned()
                .unwrap_or_default();
            if host.is_empty() {
                env.shift_remove(k);
            } else {
                env.insert(k.to_string(), host);
            }
        }
    } else if let Some(venv) = env.get("VIRTUAL_ENV").cloned() {
        // Python compares VIRTUAL_ENV to the running interpreter's sys.prefix; the ctx
        // env *is* this fn's environment, so a venv in it is the fn's own and its bin
        // leaves PATH, like the elif branch.
        env.shift_remove("VIRTUAL_ENV");
        let venv_bin = Path::new(&venv).join("bin");
        if let Some(path) = env.get("PATH").cloned() {
            let kept: Vec<PathBuf> = std::env::split_paths(&path)
                .filter(|entry| entry.as_path() != venv_bin)
                .collect();
            if let Ok(joined) = std::env::join_paths(&kept) {
                env.insert("PATH".into(), joined.to_string_lossy().into_owned());
            }
        }
    }
    env
}

// ---- input, path and ref helpers --------------------------------------------

pub(crate) fn opt<'a>(inputs: &'a JsonMap, name: &str) -> Option<&'a Value> {
    inputs.0.get(name).map(JsonValue::as_value)
}
pub(crate) fn req<'a>(inputs: &'a JsonMap, name: &str) -> Result<&'a Value, FnFailure> {
    opt(inputs, name).ok_or_else(|| FnFailure::terminal(format!("missing required input {name:?}")))
}
/// A required string input; Python fails deep in Path()/argv construction otherwise.
pub(crate) fn arg(inputs: &JsonMap, name: &str) -> Result<String, FnFailure> {
    req(inputs, name)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| FnFailure::terminal(format!("{name} must be a string")))
}
pub(crate) fn output<const N: usize>(pairs: [(&str, Value); N]) -> Result<JsonMap, FnFailure> {
    let mut map = IndexMap::with_capacity(N);
    for (name, value) in pairs {
        map.insert(
            name.to_string(),
            JsonValue::try_from(value).map_err(|e| FnFailure::terminal(e.to_string()))?,
        );
    }
    Ok(JsonMap(map))
}

/// Python truthiness for a JSON value, for `x or default` and `if inp.get(x)` checks.
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python str() of a scalar, for `str(run_id)` and ref checks; containers have no place
/// in argv and fail like the interpreter's TypeError.
pub(crate) fn py_str(value: &Value) -> Result<String, FnFailure> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(if *b { "True" } else { "False" }.to_string()),
        Value::Null => Ok("None".into()),
        other => Err(FnFailure::terminal(format!(
            "expected a string, got {}",
            py_type(other)
        ))),
    }
}

/// Numeric coercion matching Python comparisons: bool counts as 0/1.
pub(crate) fn num(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}
pub(crate) fn py_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// _git.refs.ref: a plan-supplied ref, branch or remote may not start with '-' —
/// positionally it would be read as an option (`git push --all`, `gh pr view
/// --repo=other/x`). Returns the value rendered as it would appear in argv.
pub(crate) fn ref_(name: &str, value: &Value) -> Result<String, FnFailure> {
    let s = py_str(value)?;
    if s.starts_with('-') {
        return Err(FnFailure::terminal(format!(
            "{name} may not start with '-': {s}"
        )));
    }
    Ok(s)
}

/// Path.resolve(strict=False): an absolute path with symlinks collapsed in the parts
/// that exist; `base` stands in for the fn's working directory (the run dir).
pub(crate) fn resolve(base: &Path, path: &Path) -> Result<PathBuf, FnFailure> {
    let abs = normalize(&base.join(path));
    let mut probe = abs.as_path();
    let mut tail: Vec<OsString> = Vec::new();
    loop {
        match std::fs::canonicalize(probe) {
            Ok(mut canon) => {
                for part in tail.iter().rev() {
                    canon.push(part);
                }
                return Ok(canon);
            }
            Err(e) if e.kind() == ErrorKind::NotFound => match probe.file_name() {
                Some(name) => {
                    tail.push(name.to_os_string());
                    probe = probe.parent().unwrap_or(probe);
                }
                None => {
                    return Err(FnFailure::terminal(format!(
                        "resolve {}: {e}",
                        abs.display()
                    )));
                }
            },
            Err(e) => {
                return Err(FnFailure::terminal(format!(
                    "resolve {}: {e}",
                    abs.display()
                )));
            }
        }
    }
}
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Decode a command's stdout as strict JSON, like json.loads except stricter (the
/// workspace decoder refuses duplicate keys, overflowing ints and non-finite floats).
pub(crate) fn decode(stdout: &str) -> Result<Value, FnFailure> {
    decode_json::<Value>(stdout.as_bytes()).map_err(|e| FnFailure::terminal(e.to_string()))
}

fn args(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}
fn display_argv(argv: &[OsString]) -> String {
    argv.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

// ---- the fns ------------------------------------------------------------------

/// git.worktree: add a worktree for a branch, creating the branch at base if needed.
pub async fn worktree(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<JsonMap, FnFailure> {
    let repo = resolve(&ctx.run_dir, Path::new(&arg(inputs, "repo")?))?;
    let branch = ref_("branch", req(inputs, "branch")?)?;
    let base = ref_("base", req(inputs, "base")?)?;
    let path = match opt(inputs, "path").filter(|v| truthy(v)) {
        Some(value) => resolve(&repo, &PathBuf::from(py_str(value)?))?,
        None => repo
            .parent()
            .unwrap_or(repo.as_path())
            .join(format!(
                "{}-wt",
                repo.file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            ))
            .join(&branch),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| FnFailure::terminal(format!("mkdir {}: {e}", parent.display())))?;
    }
    let exists = sh(
        ctx,
        &args(&[
            "git",
            "-C",
            &repo.to_string_lossy(),
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ]),
        None,
        false,
    )
    .await?
    .code
        == 0;
    if exists {
        sh(
            ctx,
            &args(&[
                "git",
                "-C",
                &repo.to_string_lossy(),
                "worktree",
                "add",
                &path.to_string_lossy(),
                &branch,
            ]),
            None,
            true,
        )
        .await?;
    } else {
        sh(
            ctx,
            &args(&[
                "git",
                "-C",
                &repo.to_string_lossy(),
                "worktree",
                "add",
                "-b",
                &branch,
                &path.to_string_lossy(),
                &base,
            ]),
            None,
            true,
        )
        .await?;
    }
    let sha = sh(
        ctx,
        &args(&["git", "-C", &path.to_string_lossy(), "rev-parse", "HEAD"]),
        None,
        true,
    )
    .await?
    .stdout;
    output([
        ("path", json!(path.to_string_lossy())),
        ("branch", json!(branch)),
        ("sha", json!(sha.trim())),
    ])
}

/// git.worktree_rm: remove a worktree; removed=false when the path is not one.
pub async fn worktree_rm(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<JsonMap, FnFailure> {
    let repo = arg(inputs, "repo")?;
    let raw = arg(inputs, "path")?;
    let target = resolve(&ctx.run_dir, Path::new(&raw))?;
    let listed = sh(
        ctx,
        &args(&["git", "-C", &repo, "worktree", "list", "--porcelain"]),
        None,
        true,
    )
    .await?
    .stdout;
    let mut is_worktree = false;
    for line in listed.lines().filter_map(|l| l.strip_prefix("worktree ")) {
        if resolve(&ctx.run_dir, Path::new(line))? == target {
            is_worktree = true;
            break;
        }
    }
    if !is_worktree {
        return output([("removed", json!(false))]);
    }
    let mut argv = args(&["git", "-C", &repo, "worktree", "remove"]);
    if opt(inputs, "force").is_some_and(truthy) {
        argv.push("--force".into());
    }
    argv.push((&raw).into());
    sh(ctx, &argv, None, true).await?;
    output([("removed", json!(true))])
}

/// git.head: current branch and sha of a worktree.
pub async fn head(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<JsonMap, FnFailure> {
    let path = arg(inputs, "path")?;
    let branch = sh(
        ctx,
        &args(&["git", "-C", &path, "rev-parse", "--abbrev-ref", "HEAD"]),
        None,
        true,
    )
    .await?
    .stdout;
    let sha = sh(
        ctx,
        &args(&["git", "-C", &path, "rev-parse", "HEAD"]),
        None,
        true,
    )
    .await?
    .stdout;
    output([("branch", json!(branch.trim())), ("sha", json!(sha.trim()))])
}

/// True when refs/heads/<branch> is checked out in any worktree of repo.
async fn checked_out(ctx: &BuiltinCtx, repo: &str, branch: &str) -> Result<bool, FnFailure> {
    let out = sh(
        ctx,
        &args(&["git", "-C", repo, "worktree", "list", "--porcelain"]),
        None,
        true,
    )
    .await?
    .stdout;
    let needle = format!("branch refs/heads/{branch}");
    Ok(out
        .trim()
        .split("\n\n")
        .any(|block| block.lines().any(|line| line == needle)))
}

/// git.merge: merge source into target inside a temporary worktree under the run dir.
pub async fn merge(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<JsonMap, FnFailure> {
    let repo = arg(inputs, "repo")?;
    let source = ref_("source", req(inputs, "source")?)?;
    let target = ref_("target", req(inputs, "target")?)?;
    let wt = ctx.run_dir.join("merge-wt");
    std::fs::create_dir_all(&ctx.run_dir)
        .map_err(|e| FnFailure::terminal(format!("mkdir {}: {e}", ctx.run_dir.display())))?;
    let old_sha = sh(
        ctx,
        &args(&["git", "-C", &repo, "rev-parse", &target]),
        None,
        true,
    )
    .await?
    .stdout
    .trim()
    .to_string();
    let detached = checked_out(ctx, &repo, &target).await?;
    // target checked out in another worktree: merge detached and move the branch ref
    // afterwards, guarded by its previous value.
    let mut argv = args(&["git", "-C", &repo, "worktree", "add"]);
    if detached {
        argv.push("--detach".into());
    }
    argv.push(wt.as_os_str().to_os_string());
    argv.push((&target).into());
    sh(ctx, &argv, None, true).await?;
    let job = MergeJob {
        repo: &repo,
        source: &source,
        target: &target,
        wt: &wt,
        detached,
        old_sha: &old_sha,
    };
    let result = merge_body(inputs, ctx, &job).await;
    let rm = sh(
        ctx,
        &[
            args(&["git", "-C", &repo, "worktree", "remove", "--force"]),
            vec![wt.as_os_str().to_os_string()],
        ]
        .concat(),
        None,
        false,
    )
    .await?;
    if rm.code != 0 {
        tracing::warn!(
            "warning: could not remove temporary worktree {}",
            wt.display()
        );
    }
    result
}

struct MergeJob<'a> {
    repo: &'a str,
    source: &'a str,
    target: &'a str,
    wt: &'a Path,
    detached: bool,
    old_sha: &'a str,
}

/// The merge itself and its guarded follow-up (ref move and optional push); cleanup of
/// the temporary worktree happens in `merge` whatever this returns.
async fn merge_body(
    inputs: &JsonMap,
    ctx: &BuiltinCtx,
    job: &MergeJob<'_>,
) -> Result<JsonMap, FnFailure> {
    let MergeJob {
        repo,
        source,
        target,
        wt,
        detached,
        old_sha,
    } = *job;
    // `git -C <wt> ...` with the worktree path verbatim.
    let gwt = |rest: &[&str]| {
        [
            args(&["git", "-C"]),
            vec![wt.as_os_str().to_os_string()],
            args(rest),
        ]
        .concat()
    };
    let message = match opt(inputs, "message").filter(|v| truthy(v)) {
        Some(value) => py_str(value)?,
        None => format!("merge {source} into {target}"),
    };
    let merge_argv = gwt(&["merge", "--no-ff", "-m", &message, source]);
    let merged = sh(ctx, &merge_argv, None, false).await?;
    if merged.code != 0 {
        let in_merge = sh(
            ctx,
            &gwt(&["rev-parse", "-q", "--verify", "MERGE_HEAD"]),
            None,
            false,
        )
        .await?
        .code
            == 0;
        if !in_merge {
            return Err(sh_error(&merge_argv, &merged));
        }
        let conflicts = sh(
            ctx,
            &gwt(&["diff", "--name-only", "--diff-filter=U"]),
            None,
            true,
        )
        .await?
        .stdout;
        sh(ctx, &gwt(&["merge", "--abort"]), None, false).await?;
        return output([
            ("merged", json!(false)),
            ("sha", Value::Null),
            (
                "conflicts",
                json!(conflicts.split_whitespace().collect::<Vec<_>>()),
            ),
        ]);
    }
    let sha = sh(ctx, &gwt(&["rev-parse", "HEAD"]), None, true)
        .await?
        .stdout;
    let sha = sha.trim().to_string();
    if detached {
        sh(
            ctx,
            &args(&[
                "git",
                "-C",
                repo,
                "update-ref",
                &format!("refs/heads/{target}"),
                &sha,
                old_sha,
            ]),
            None,
            true,
        )
        .await?;
    }
    if opt(inputs, "push").is_some_and(truthy) {
        sh(
            ctx,
            &args(&["git", "-C", repo, "push", "origin", target]),
            None,
            true,
        )
        .await?;
    }
    output([
        ("merged", json!(true)),
        ("sha", json!(sha)),
        ("conflicts", json!([])),
    ])
}

/// git.rebase: `git rebase <onto>` in a worktree; abort and report conflicts on clash.
pub async fn rebase(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<JsonMap, FnFailure> {
    let path = arg(inputs, "path")?;
    let onto = ref_("onto", req(inputs, "onto")?)?;
    let rebased = sh(
        ctx,
        &args(&["git", "-C", &path, "rebase", &onto]),
        None,
        false,
    )
    .await?;
    if rebased.code != 0 {
        let in_rebase = sh(
            ctx,
            &args(&[
                "git",
                "-C",
                &path,
                "rev-parse",
                "-q",
                "--verify",
                "REBASE_HEAD",
            ]),
            None,
            false,
        )
        .await?
        .code
            == 0;
        if !in_rebase {
            return Err(sh_error(
                &args(&["git", "-C", &path, "rebase", &onto]),
                &rebased,
            ));
        }
        let conflicts = sh(
            ctx,
            &args(&["git", "-C", &path, "diff", "--name-only", "--diff-filter=U"]),
            None,
            true,
        )
        .await?
        .stdout;
        sh(
            ctx,
            &args(&["git", "-C", &path, "rebase", "--abort"]),
            None,
            false,
        )
        .await?;
        return output([
            ("ok", json!(false)),
            ("sha", json!(rev_parse_head(ctx, &path).await?)),
            (
                "conflicts",
                json!(conflicts.split_whitespace().collect::<Vec<_>>()),
            ),
        ]);
    }
    output([
        ("ok", json!(true)),
        ("sha", json!(rev_parse_head(ctx, &path).await?)),
        ("conflicts", json!([])),
    ])
}

/// `git -C <path> rev-parse HEAD`, trimmed.
async fn rev_parse_head(ctx: &BuiltinCtx, path: &str) -> Result<String, FnFailure> {
    sh(
        ctx,
        &args(&["git", "-C", path, "rev-parse", "HEAD"]),
        None,
        true,
    )
    .await
    .map(|p| p.stdout.trim().to_string())
}

/// git.push: push HEAD:<branch> to a remote (default origin).
pub async fn push(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<JsonMap, FnFailure> {
    let path = arg(inputs, "path")?;
    let remote = match opt(inputs, "remote").filter(|v| truthy(v)) {
        Some(value) => ref_("remote", value)?,
        None => "origin".to_string(),
    };
    let branch = ref_("branch", req(inputs, "branch")?)?;
    let mut argv = args(&["git", "-C", &path, "push"]);
    if opt(inputs, "force_with_lease").is_some_and(truthy) {
        argv.push("--force-with-lease".into());
    }
    argv.push((&remote).into());
    argv.push(format!("HEAD:{branch}").into());
    sh(ctx, &argv, None, true).await?;
    output([("sha", json!(rev_parse_head(ctx, &path).await?))])
}
