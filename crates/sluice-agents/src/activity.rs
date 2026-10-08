//! A run's activity outline: what its agent did, turn by turn and call by call, read when a page
//! asks from the transcript its engine already keeps. Claude's is its session JSONL (under its
//! config home's `projects/`), Codex's its session rollout (under the run's private Codex home in
//! `codex-native-homes/`), Devin's the hook journal sluice's hook writes in each invocation
//! (`devin-hooks.jsonl`). Nothing is stored: a cache follows each transcript by its size and
//! modification time and reads only what was appended since its last read.
//!
//! A turn is one thing sluice sent the agent (its task, a message, a nudge) and everything the
//! agent did until the next: its tool calls, each with its key argument, its arguments, its
//! result and whether it failed, and its last words. Every text kept is masked
//! (`account::redact`) and capped, so a transcript's secrets and huge outputs never reach a page
//! whole.
use crate::engines::account::redact;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
    time::SystemTime,
};

/// A transcript record longer than this is skipped (counted in `Outline::skipped`).
pub const MAX_RECORD: usize = 8 * 1024 * 1024;
/// What sluice sent and the agent's last words keep this many characters.
const TEXT_MOST: usize = 1200;
/// An argument's value keeps this many characters.
const ARG_MOST: usize = 600;
/// A call keeps this many of its arguments.
const ARGS_MOST: usize = 8;
/// A result keeps its first `RESULT_HEAD` and last `RESULT_TAIL` characters: a command's
/// failure is usually at its end.
const RESULT_HEAD: usize = 400;
const RESULT_TAIL: usize = 1200;
/// A call's key argument keeps one line of this many characters.
const KEY_MOST: usize = 300;
/// Transcripts the cache follows at once.
const CACHED: usize = 48;

/// The engine a transcript is from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Claude,
    Codex,
    Devin,
}
impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Devin => "Devin",
        }
    }
    /// What its outline is read from, in words.
    pub fn source(self) -> &'static str {
        match self {
            Self::Claude => "its Claude session transcript",
            Self::Codex => "its Codex session rollout",
            Self::Devin => "its Devin hook journal",
        }
    }
}

/// What a call does, for its icon and the quiet fold: reads, searches and listings are quiet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Shell,
    Edit,
    Read,
    Search,
    List,
    Agent,
    Web,
    Message,
    Other,
}
impl Kind {
    /// A call that only looks: a read, a search, a listing. A run's outline may fold these.
    pub fn quiet(self) -> bool {
        matches!(self, Self::Read | Self::Search | Self::List)
    }
}

/// How a call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// No result yet (or none ever, when its run ended first).
    Running,
    Done,
    Failed,
}

/// One tool call.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Call {
    pub id: String,
    /// The tool's name as its engine calls it ("Bash", "exec", "shell"), an MCP tool's as its
    /// server and tool ("sluice say").
    pub tool: String,
    pub kind: Kind,
    /// Its key argument, one line: the command, the file path, the pattern, the query.
    pub key: String,
    /// Its arguments, by name, each capped.
    pub args: Vec<(String, String)>,
    /// What it returned (its error when it failed), capped at its head and tail.
    pub result: String,
    /// How many characters the whole result had (more than `result` shows when it was cut).
    pub result_chars: usize,
    pub outcome: Outcome,
    /// When it started and ended, Unix milliseconds, when its transcript says.
    pub started_ms: Option<u64>,
    pub ended_ms: Option<u64>,
}

/// What sluice sent to open a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SentKind {
    /// Its task (`task.md`).
    Task,
    /// A message on its thread, handed over as a file.
    Message,
    /// Anything typed in whole: a short message, a nudge.
    Text,
    /// Nothing in this run: the agent carried on in a session an earlier run left.
    Carried,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Sent {
    pub kind: SentKind,
    /// Its words: the file's for a task or a handed-over message (a task's from its "## Task"
    /// section), else as sent. Masked and capped.
    pub text: String,
}

/// One turn: what sluice sent, then what the agent did until the next thing it was sent.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Turn {
    pub sent: Sent,
    pub started_ms: Option<u64>,
    /// Its latest record's time.
    pub ended_ms: Option<u64>,
    /// The engine said the turn ended (or the next one began).
    pub closed: bool,
    /// The agent's last words in it.
    pub said: String,
    pub calls: Vec<Call>,
}
impl Turn {
    pub fn failed(&self) -> usize {
        self.calls
            .iter()
            .filter(|c| c.outcome == Outcome::Failed)
            .count()
    }
    /// Its calls counted by tool, most first.
    pub fn profile(&self) -> Vec<(String, usize)> {
        profile(self.calls.iter())
    }
}

/// A run's outline.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Outline {
    pub engine: Engine,
    pub turns: Vec<Turn>,
    /// Records skipped as too long to read (over `MAX_RECORD`).
    pub skipped: usize,
}
impl Outline {
    pub fn calls(&self) -> impl Iterator<Item = &Call> {
        self.turns.iter().flat_map(|t| &t.calls)
    }
    pub fn failed(&self) -> usize {
        self.turns.iter().map(Turn::failed).sum()
    }
    /// Its calls counted by tool, most first ("Bash 42 · Edit 9 · Read 17").
    pub fn profile(&self) -> Vec<(String, usize)> {
        profile(self.calls())
    }
}
fn profile<'a>(calls: impl Iterator<Item = &'a Call>) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = vec![];
    for call in calls {
        match counts.iter_mut().find(|(t, _)| *t == call.tool) {
            Some((_, n)) => *n += 1,
            None => counts.push((call.tool.clone(), 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

/// Where transcripts live: sluice's home (its runs and Codex homes) and Claude's config home.
#[derive(Clone, Debug)]
pub struct Homes {
    pub sluice: PathBuf,
    pub claude: Option<PathBuf>,
}
impl Homes {
    /// Claude's config home as Claude itself finds it: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
    pub fn claude_from_env() -> Option<PathBuf> {
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude")))
    }
}

/// The part of a transcript that is one run's: a resumed session's transcript holds the earlier
/// runs' records too. Unix milliseconds; a record with no time (Devin's journal, which is per
/// invocation) is always the run's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Window {
    pub since_ms: u64,
    pub until_ms: Option<u64>,
}
impl Window {
    fn holds(&self, at: Option<u64>) -> bool {
        at.is_none_or(|at| at >= self.since_ms && self.until_ms.is_none_or(|u| at < u))
    }
}

/// A run's outline, read from its engine's transcripts; `None` when it has none (no agent ran,
/// or its engine kept nothing sluice can read). `run` is a run id: anything else names nothing.
pub fn outline(homes: &Homes, run: &str, window: Window) -> Option<Arc<Outline>> {
    let sources = locate(homes, run)?;
    let mut outlines: Vec<Arc<Outline>> =
        sources.iter().filter_map(|s| follow(s, window)).collect();
    match outlines.len() {
        0 => None,
        1 => outlines.pop(),
        _ => {
            let mut whole = Outline {
                engine: outlines[0].engine,
                turns: vec![],
                skipped: 0,
            };
            for part in outlines {
                whole.turns.extend(part.turns.iter().cloned());
                whole.skipped += part.skipped;
            }
            Some(Arc::new(whole))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Source {
    engine: Engine,
    path: PathBuf,
    /// The run's directory: a handed-over message is read only from under it.
    run_dir: PathBuf,
}

fn plain_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}
fn plain_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir())
}
fn read_json(path: &Path) -> Option<Value> {
    if !plain_file(path) {
        return None;
    }
    let mut bytes = vec![];
    fs::File::open(path)
        .ok()?
        .take(1024 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}
/// A session id safe to put in a file name.
fn session_id(value: &Value) -> Option<String> {
    let s = value.get("session")?.as_str()?;
    (!s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then(|| s.to_owned())
}

/// The run's transcripts, in the order its invocations ran.
fn locate(homes: &Homes, run: &str) -> Option<Vec<Source>> {
    run.parse::<sluice_model::ids::RunId>().ok()?;
    let run_dir = homes.sluice.join("runs").join(run);
    if !plain_dir(&run_dir) {
        return None;
    }
    let native = read_json(&run_dir.join("native.json"))?;
    let engine = match native.get("engine")?.as_str()? {
        "claude" => Engine::Claude,
        "codex" => Engine::Codex,
        "devin" => Engine::Devin,
        _ => return None,
    };
    let mut invocations: Vec<PathBuf> = fs::read_dir(run_dir.join("invocations"))
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| plain_dir(p))
                .collect()
        })
        .unwrap_or_default();
    invocations.sort();
    let mut sessions: Vec<(String, Option<PathBuf>)> = vec![];
    for invocation in &invocations {
        if let Some(session) = read_json(&invocation.join("native.json"))
            .as_ref()
            .and_then(session_id)
            && !sessions.iter().any(|(s, _)| *s == session)
        {
            sessions.push((session, Some(invocation.clone())));
        }
    }
    if let Some(session) = session_id(&native)
        && !sessions.iter().any(|(s, _)| *s == session)
    {
        sessions.push((session, None));
    }
    let source = |path: PathBuf| Source {
        engine,
        path,
        run_dir: run_dir.clone(),
    };
    let sources = match engine {
        Engine::Devin => invocations
            .iter()
            .map(|i| i.join("devin-hooks.jsonl"))
            .filter(|p| plain_file(p))
            .map(source)
            .collect(),
        Engine::Claude => {
            let projects = homes.claude.as_ref()?.join("projects");
            let root = fs::canonicalize(&projects).ok()?;
            let dirs: Vec<PathBuf> = fs::read_dir(&projects)
                .ok()?
                .flatten()
                .map(|e| e.path())
                .filter(|p| plain_dir(p))
                .collect();
            sessions
                .iter()
                .filter_map(|(session, _)| {
                    dirs.iter()
                        .map(|d| d.join(format!("{session}.jsonl")))
                        .find(|p| {
                            plain_file(p) && fs::canonicalize(p).is_ok_and(|c| c.starts_with(&root))
                        })
                })
                .map(source)
                .collect()
        }
        Engine::Codex => {
            let homes_dir = homes.sluice.join("codex-native-homes");
            let root = fs::canonicalize(&homes_dir).ok()?;
            sessions
                .iter()
                .filter_map(|(session, invocation)| {
                    codex_rollout(
                        &homes.sluice,
                        &root,
                        session,
                        invocation.as_deref(),
                        &invocations,
                        &run_dir,
                    )
                })
                .map(source)
                .collect()
        }
    };
    Some(sources)
}

/// A Codex session's rollout: under the home its registry names, its own home, or the home its
/// first invocation ran in (`pending-<invocation>`), each inside `codex-native-homes`.
fn codex_rollout(
    sluice: &Path,
    root: &Path,
    session: &str,
    invocation: Option<&Path>,
    invocations: &[PathBuf],
    run_dir: &Path,
) -> Option<PathBuf> {
    let mut candidates = vec![];
    if let Some(saved) = read_json(
        &sluice
            .join("codex-native-sessions")
            .join(format!("{session}.json")),
    ) && let Some(home) = saved.get("home").and_then(Value::as_str)
    {
        candidates.push(PathBuf::from(home));
    }
    candidates.push(root.join(session));
    // a run's first Codex home is named for the directory it ran in: its invocation's, or the
    // run's own
    for i in invocation
        .into_iter()
        .chain(invocations.iter().map(PathBuf::as_path))
        .chain(std::iter::once(run_dir))
    {
        if let Some(name) = i.file_name().and_then(|n| n.to_str()) {
            candidates.push(root.join(format!("pending-{name}")));
        }
    }
    let suffix = format!("-{session}.jsonl");
    for home in candidates {
        let Ok(home) = fs::canonicalize(&home) else {
            continue;
        };
        if !home.starts_with(root) {
            continue;
        }
        // sessions/YYYY/MM/DD/rollout-…-<session>.jsonl
        let mut dirs = vec![(home.join("sessions"), 0)];
        while let Some((dir, depth)) = dirs.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() && depth < 3 {
                    dirs.push((path, depth + 1));
                } else if kind.is_file()
                    && entry
                        .file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(&suffix))
                {
                    return Some(path);
                }
            }
        }
    }
    None
}

// ---- the cache -------------------------------------------------------------------------------

type Key = (Source, Window);
struct Follow {
    len: u64,
    modified: Option<SystemTime>,
    offset: u64,
    carry: Vec<u8>,
    /// Inside a record too long to read, until its line ends.
    skipping: bool,
    builder: Builder,
    snapshot: Arc<Outline>,
}
#[derive(Default)]
struct Cache {
    entries: HashMap<Key, (u64, Arc<Mutex<Follow>>)>,
    tick: u64,
}
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Mutex::default);

fn follow(source: &Source, window: Window) -> Option<Arc<Outline>> {
    let key = (source.clone(), window);
    let entry = {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.tick += 1;
        let tick = cache.tick;
        let entry = cache.entries.entry(key).or_insert_with(|| {
            (
                tick,
                Arc::new(Mutex::new(Follow::new(source.clone(), window))),
            )
        });
        entry.0 = tick;
        let entry = entry.1.clone();
        if cache.entries.len() > CACHED
            && let Some(oldest) = cache
                .entries
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(k, _)| k.clone())
        {
            cache.entries.remove(&oldest);
        }
        entry
    };
    let mut follow = entry.lock().unwrap_or_else(|e| e.into_inner());
    follow.refresh(&source.path)
}
impl Follow {
    fn new(source: Source, window: Window) -> Self {
        let builder = Builder::new(source.engine, source.run_dir, window);
        Self {
            len: 0,
            modified: None,
            offset: 0,
            carry: vec![],
            skipping: false,
            snapshot: Arc::new(builder.outline()),
            builder,
        }
    }
    fn reset(&mut self) {
        self.builder = Builder::new(
            self.builder.engine,
            self.builder.run_dir.clone(),
            self.builder.window,
        );
        self.offset = 0;
        self.carry.clear();
        self.skipping = false;
    }
    /// Read what was appended since the last read; the outline as it stands.
    fn refresh(&mut self, path: &Path) -> Option<Arc<Outline>> {
        let meta = fs::symlink_metadata(path).ok()?;
        if !meta.file_type().is_file() {
            return None;
        }
        let (len, modified) = (meta.len(), meta.modified().ok());
        if len < self.offset {
            self.reset();
        }
        if len == self.offset && modified == self.modified && self.len == len {
            return Some(self.snapshot.clone());
        }
        let mut file = fs::File::open(path).ok()?;
        file.seek(SeekFrom::Start(self.offset)).ok()?;
        let mut file = file.take(len - self.offset);
        let mut chunk = vec![0u8; 1024 * 1024];
        let mut fed = false;
        loop {
            let n = match file.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            self.offset += n as u64;
            self.lines(&chunk[..n]);
            fed = true;
        }
        self.len = len;
        self.modified = modified;
        if fed {
            self.snapshot = Arc::new(self.builder.outline());
        }
        Some(self.snapshot.clone())
    }
    fn lines(&mut self, mut bytes: &[u8]) {
        while let Some(end) = bytes.iter().position(|b| *b == b'\n') {
            let (line, rest) = (&bytes[..end], &bytes[end + 1..]);
            bytes = rest;
            if self.skipping {
                self.skipping = false;
                self.carry.clear();
                continue;
            }
            if self.carry.is_empty() {
                self.builder.line(line);
            } else {
                let mut whole = std::mem::take(&mut self.carry);
                whole.extend_from_slice(line);
                self.builder.line(&whole);
            }
        }
        if self.skipping {
            return;
        }
        self.carry.extend_from_slice(bytes);
        if self.carry.len() > MAX_RECORD {
            self.carry = vec![];
            self.skipping = true;
            self.builder.skipped += 1;
        }
    }
}

// ---- reading records -------------------------------------------------------------------------

struct Builder {
    engine: Engine,
    run_dir: PathBuf,
    window: Window,
    turns: Vec<Turn>,
    /// Each call still waiting for its result: its id and where it is.
    open: HashMap<String, (usize, usize)>,
    /// Devin's calls in the order they started, for its results (which carry no id).
    pending: Vec<(String, String, Value)>,
    seq: u64,
    skipped: usize,
}
impl Builder {
    fn new(engine: Engine, run_dir: PathBuf, window: Window) -> Self {
        Self {
            engine,
            run_dir,
            window,
            turns: vec![],
            open: HashMap::new(),
            pending: vec![],
            seq: 0,
            skipped: 0,
        }
    }
    fn outline(&self) -> Outline {
        Outline {
            engine: self.engine,
            turns: self.turns.clone(),
            skipped: self.skipped,
        }
    }
    fn line(&mut self, line: &[u8]) {
        if line.len() > MAX_RECORD {
            self.skipped += 1;
            return;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        match self.engine {
            Engine::Claude => self.claude(&record),
            Engine::Codex => self.codex(&record),
            Engine::Devin => self.devin(&record),
        }
    }

    // ---- the turn under construction ----

    fn current(&mut self, at: Option<u64>) -> &mut Turn {
        if self.turns.is_empty() {
            self.turns.push(Turn {
                sent: Sent {
                    kind: SentKind::Carried,
                    text: String::new(),
                },
                started_ms: at,
                ended_ms: at,
                closed: false,
                said: String::new(),
                calls: vec![],
            });
        }
        self.turns.last_mut().expect("a turn")
    }
    fn touch(&mut self, at: Option<u64>) {
        if let (Some(at), Some(turn)) = (at, self.turns.last_mut()) {
            turn.ended_ms = Some(turn.ended_ms.map_or(at, |e| e.max(at)));
        }
    }
    fn start(&mut self, text: &str, at: Option<u64>) {
        // a message queued while the agent worked is read when it is taken in: its turn starts
        // no earlier than the last one's last record
        let mut at = at;
        if let Some(last) = self.turns.last_mut() {
            last.closed = true;
            at = match (at, last.ended_ms) {
                (Some(a), Some(e)) => Some(a.max(e)),
                (a, e) => a.or(e),
            };
        }
        let sent = sent(text, &self.run_dir);
        self.turns.push(Turn {
            sent,
            started_ms: at,
            ended_ms: at,
            closed: false,
            said: String::new(),
            calls: vec![],
        });
    }
    fn say(&mut self, text: &str, at: Option<u64>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let said = keep(text, TEXT_MOST);
        let turn = self.current(at);
        turn.said = said;
        self.touch(at);
    }
    fn close(&mut self, at: Option<u64>) {
        self.touch(at);
        if let Some(turn) = self.turns.last_mut() {
            turn.closed = true;
        }
    }
    fn call(&mut self, id: &str, tool: &str, input: &Value, at: Option<u64>) {
        let (tool, kind, key) = describe(tool, input);
        self.call_keyed(id, tool, kind, key, input, at);
    }
    fn call_keyed(
        &mut self,
        id: &str,
        tool: String,
        kind: Kind,
        key: String,
        input: &Value,
        at: Option<u64>,
    ) {
        let call = Call {
            id: id.to_owned(),
            tool,
            kind,
            key,
            args: args(input),
            result: String::new(),
            result_chars: 0,
            outcome: Outcome::Running,
            started_ms: at,
            ended_ms: None,
        };
        let turn = self.current(at);
        turn.calls.push(call);
        let place = (
            self.turns.len() - 1,
            self.turns.last().map_or(0, |t| t.calls.len() - 1),
        );
        self.open.insert(id.to_owned(), place);
        self.touch(at);
    }
    fn finish(&mut self, id: &str, result: &str, failed: bool, at: Option<u64>) {
        let Some((t, c)) = self.open.remove(id) else {
            return;
        };
        let Some(call) = self.turns.get_mut(t).and_then(|t| t.calls.get_mut(c)) else {
            return;
        };
        call.result_chars = result.chars().count();
        call.result = head_tail(result);
        call.outcome = if failed {
            Outcome::Failed
        } else {
            Outcome::Done
        };
        call.ended_ms = at.or(call.started_ms);
        self.touch(at);
    }
    fn next_id(&mut self) -> String {
        self.seq += 1;
        format!("c{}", self.seq)
    }

    // ---- Claude: its session JSONL ----

    fn claude(&mut self, record: &Value) {
        if record["isSidechain"] == true {
            return;
        }
        let at = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(millis);
        if !self.window.holds(at) {
            return;
        }
        match record["type"].as_str() {
            Some("user") => {
                if record["isMeta"] == true || record["isCompactSummary"] == true {
                    return;
                }
                match &record["message"]["content"] {
                    Value::String(text) => self.claude_prompt(text, at),
                    Value::Array(blocks) => {
                        let mut words = vec![];
                        for block in blocks {
                            match block["type"].as_str() {
                                Some("tool_result") => {
                                    let id = block["tool_use_id"].as_str().unwrap_or_default();
                                    let text = content_text(&block["content"]);
                                    let failed = block["is_error"] == true;
                                    self.finish(id, &text, failed, at);
                                }
                                Some("text") => {
                                    words.push(block["text"].as_str().unwrap_or_default())
                                }
                                _ => {}
                            }
                        }
                        if !words.is_empty() {
                            self.claude_prompt(&words.join("\n"), at);
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                if let Some(blocks) = record["message"]["content"].as_array() {
                    for block in blocks {
                        match block["type"].as_str() {
                            Some("text") => {
                                self.say(block["text"].as_str().unwrap_or_default(), at)
                            }
                            Some("tool_use") => {
                                let id = block["id"].as_str().unwrap_or_default().to_owned();
                                let name = block["name"].as_str().unwrap_or("tool").to_owned();
                                self.call(&id, &name, &block["input"], at);
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some("attachment") => {
                if record["attachment"]["type"] == "queued_command"
                    && let Some(prompt) = record["attachment"]["prompt"].as_str()
                {
                    self.claude_prompt(prompt, at);
                } else {
                    self.touch(at);
                }
            }
            Some("system") => match record["subtype"].as_str() {
                Some("turn_duration" | "stop_hook_summary") => self.close(at),
                _ => self.touch(at),
            },
            _ => {}
        }
    }
    fn claude_prompt(&mut self, text: &str, at: Option<u64>) {
        let trimmed = text.trim();
        if trimmed.starts_with("[Request interrupted") {
            self.close(at);
            return;
        }
        // Claude Code's own notes (a command's echo, a background task's notification) are not
        // something sluice sent: sluice's words never open with a tag
        if trimmed.starts_with('<') {
            self.touch(at);
            return;
        }
        self.start(text, at);
    }

    // ---- Codex: its session rollout ----

    fn codex(&mut self, record: &Value) {
        let at = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(millis);
        if !self.window.holds(at) {
            return;
        }
        if record["type"] != "event_msg" {
            self.touch(at);
            return;
        }
        let payload = &record["payload"];
        match payload["type"].as_str() {
            Some("task_complete") => {
                if let Some(last) = payload["last_agent_message"].as_str() {
                    self.say(last, at);
                }
                self.close(at);
            }
            Some("turn_aborted") => self.close(at),
            Some("item_completed") => {
                let item = &payload["item"];
                let started = payload["started_at_ms"].as_u64().or(at);
                let ended = payload["completed_at_ms"].as_u64().or(at);
                self.codex_item(item, started, ended);
            }
            _ => self.touch(at),
        }
    }
    fn codex_item(&mut self, item: &Value, started: Option<u64>, ended: Option<u64>) {
        let id = item["id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| self.next_id());
        let failed_status = item["status"] == "failed";
        match item["type"].as_str() {
            Some("UserMessage") => {
                let text = content_text(&item["content"]);
                self.start(&text, started);
            }
            Some("AgentMessage") => self.say(&content_text(&item["content"]), ended),
            Some("CommandExecution") => {
                let command = match item["command"].as_array() {
                    Some(parts) => {
                        let parts: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
                        if parts.len() >= 3 && matches!(parts[1], "-lc" | "-c") {
                            parts[2..].join(" ")
                        } else {
                            parts.join(" ")
                        }
                    }
                    None => item["command"].as_str().unwrap_or_default().to_owned(),
                };
                let types: Vec<&str> = item["parsed_cmd"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|p| p["type"].as_str()).collect())
                    .unwrap_or_default();
                let tool = match types.first() {
                    Some(&first) if types.iter().all(|t| *t == first) => match first {
                        "read" => "read",
                        "search" => "search",
                        "list_files" => "list",
                        _ => "shell",
                    },
                    _ => "shell",
                };
                let exit = item["exit_code"].as_i64().or(item["exitCode"].as_i64());
                let mut input = serde_json::json!({"command": command});
                if let Some(cwd) = item["cwd"].as_str() {
                    input["cwd"] = cwd.trim_start_matches("file://").into();
                }
                if let Some(exit) = exit {
                    input["exit code"] = exit.into();
                }
                self.call(&id, tool, &input, started);
                let output = ["aggregated_output", "formatted_output", "stdout"]
                    .iter()
                    .find_map(|k| item[*k].as_str().filter(|s| !s.is_empty()))
                    .unwrap_or_default()
                    .to_owned();
                let output = match item["stderr"].as_str() {
                    Some(err) if !err.is_empty() && !output.contains(err) => {
                        format!("{output}{err}")
                    }
                    _ => output,
                };
                let failed = failed_status || exit.is_some_and(|e| e != 0);
                self.finish(&id, &output, failed, ended);
            }
            Some("FileChange") => {
                let mut input = serde_json::Map::new();
                let mut diff = String::new();
                if let Some(changes) = item["changes"].as_object() {
                    for (path, change) in changes {
                        let kind = change["type"].as_str().unwrap_or("change");
                        input.insert(path.clone(), kind.into());
                        if let Some(d) = change["unified_diff"].as_str() {
                            diff.push_str(&format!("{path}\n{d}\n"));
                        }
                    }
                }
                let key = match input.len() {
                    0 => String::new(),
                    1 => line(input.keys().next().map_or("", String::as_str)),
                    n => line(&format!(
                        "{} and {} more",
                        input.keys().next().map_or("", String::as_str),
                        n - 1
                    )),
                };
                let input = Value::Object(input);
                self.call_keyed(&id, "edit".into(), Kind::Edit, key, &input, started);
                let result = if diff.is_empty() {
                    item["stderr"].as_str().unwrap_or_default().to_owned()
                } else {
                    diff
                };
                self.finish(&id, &result, failed_status, ended);
            }
            Some("Extension") => {
                let kind = item["kind"].as_str().unwrap_or("extension");
                let tool = if kind == "web.search" {
                    "web search"
                } else {
                    kind
                };
                let mut input = serde_json::Map::new();
                if let Some(query) = item["query"].as_str() {
                    input.insert("query".into(), query.into());
                }
                self.call(&id, tool, &Value::Object(input), started);
                let result: Vec<String> = item["results"]
                    .as_array()
                    .map(|r| {
                        r.iter()
                            .filter_map(|x| x["title"].as_str())
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                self.finish(&id, &result.join("\n"), failed_status, ended);
            }
            Some("WebSearch") => {
                let input =
                    serde_json::json!({"query": item["query"].as_str().unwrap_or_default()});
                self.call(&id, "web search", &input, started);
                self.finish(&id, "", failed_status, ended);
            }
            Some("ImageView") => {
                let path = item["path"].as_str().unwrap_or_default();
                let input = serde_json::json!({"path": path.trim_start_matches("file://")});
                self.call(&id, "view image", &input, started);
                self.finish(&id, "", failed_status, ended);
            }
            Some("McpToolCall") => {
                let server = item["server"].as_str().unwrap_or("mcp");
                let tool = item["tool"].as_str().unwrap_or("tool");
                let name = format!("mcp__{server}__{tool}");
                self.call(&id, &name, &item["arguments"], started);
                let failed = failed_status || !item["error"].is_null();
                let result = if item["error"].is_null() {
                    content_text(&item["result"])
                } else {
                    content_text(&item["error"])
                };
                self.finish(&id, &result, failed, ended);
            }
            _ => self.touch(ended),
        }
    }

    // ---- Devin: the hook journal ----

    fn devin(&mut self, record: &Value) {
        let at = record.get("at").and_then(Value::as_u64);
        if !self.window.holds(at) {
            return;
        }
        let hook = &record["hook"];
        match hook["hook_event_name"].as_str() {
            Some("UserPromptSubmit") => {
                self.pending.clear();
                self.start(hook["prompt"].as_str().unwrap_or_default(), at);
            }
            Some("PreToolUse") => {
                let id = self.next_id();
                let tool = hook["tool_name"].as_str().unwrap_or("tool").to_owned();
                self.call(&id, &tool, &hook["tool_input"], at);
                self.pending.push((id, tool, hook["tool_input"].clone()));
            }
            Some("PostToolUse") => {
                let tool = hook["tool_name"].as_str().unwrap_or("tool");
                let input = &hook["tool_input"];
                let id = match self
                    .pending
                    .iter()
                    .position(|(_, t, i)| t == tool && i == input)
                {
                    Some(i) => self.pending.remove(i).0,
                    None => {
                        let id = self.next_id();
                        self.call(&id, tool, input, at);
                        id
                    }
                };
                let response = &hook["tool_response"];
                let output = response["output"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or(response["error"].as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| match response {
                        Value::Null => String::new(),
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    });
                let failed = response["success"] == false
                    || !response["error"].is_null() && response["success"] != true
                    || exit_code(&output).is_some_and(|c| c != 0);
                self.finish(&id, &output, failed, at);
            }
            Some("Stop") => {
                if let Some(text) = hook["last_assistant_message"].as_str() {
                    self.say(text, at);
                }
                self.close(at);
            }
            Some("SessionEnd") => self.close(at),
            _ => self.touch(at),
        }
    }
}

/// A shell's exit code as Devin's exec reports it: "Exit code: 1" at the end of its output.
fn exit_code(output: &str) -> Option<i64> {
    let at = output.rfind("Exit code: ")?;
    let digits: String = output[at + 11..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

/// Unix milliseconds as an RFC 3339 time in whole seconds, UTC ("2026-10-08T10:15:32Z").
pub fn rfc3339(ms: u64) -> String {
    i64::try_from(ms / 1000)
        .ok()
        .and_then(|s| time::OffsetDateTime::from_unix_timestamp(s).ok())
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_default()
}

/// RFC 3339 as Unix milliseconds.
fn millis(text: &str) -> Option<u64> {
    let at =
        time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()?;
    u64::try_from(at.unix_timestamp_nanos() / 1_000_000).ok()
}

/// A message's or a result's words: a string, or the text of each text block.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                Value::String(s) => Some(s.as_str()),
                b => b["text"].as_str(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        Value::Object(o) => o
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| content.to_string()),
        other => other.to_string(),
    }
}

/// Masking reads this many characters past a cut, so a token the cut runs through is still
/// recognised whole.
const MASK_MARGIN: usize = 256;
/// At most `most` characters, masked: masked a little past the cut and cut after, so a huge
/// text costs no more than its part and a token at the cut is still masked.
fn keep(text: &str, most: usize) -> String {
    match text.char_indices().nth(most + MASK_MARGIN) {
        None if text.chars().count() <= most => redact(text),
        found => {
            let part = found.map_or(text, |(at, _)| &text[..at]);
            let mut cut: String = redact(part).chars().take(most).collect();
            cut.push('…');
            cut
        }
    }
}
/// A result as kept: its head and tail when it is long, each masked as `keep` masks.
fn head_tail(text: &str) -> String {
    let chars = text.chars().count();
    if chars <= RESULT_HEAD + RESULT_TAIL {
        return redact(text);
    }
    let head: String = text.chars().take(RESULT_HEAD + MASK_MARGIN).collect();
    let head: String = redact(&head).chars().take(RESULT_HEAD).collect();
    let tail: String = text
        .chars()
        .skip(chars.saturating_sub(RESULT_TAIL + MASK_MARGIN))
        .collect();
    let tail = redact(&tail);
    let tail: String = tail
        .chars()
        .skip(tail.chars().count().saturating_sub(RESULT_TAIL))
        .collect();
    format!(
        "{head}\n… {} characters left out …\n{tail}",
        chars - RESULT_HEAD - RESULT_TAIL
    )
}

/// What sluice sent: a handed-over file (its task, a message) is read from under the run's
/// directory; anything else is its own words.
fn sent(text: &str, run_dir: &Path) -> Sent {
    const TAIL: &str = "; read it fully, then do it.";
    let trimmed = text.trim();
    if let Some(head) = trimmed.strip_suffix(TAIL)
        && let Some((what, path)) = head.split_once(" is in ")
    {
        let kind = if what.ends_with("task") {
            SentKind::Task
        } else {
            SentKind::Message
        };
        let file = Path::new(path);
        let words = handed_over(file, run_dir).map(|words| match kind {
            // a task's own words are under its "## Task" heading, after sluice's preamble
            SentKind::Task => words
                .split_once("\n## Task\n")
                .map(|(_, task)| task.trim().to_owned())
                .unwrap_or(words),
            _ => words,
        });
        // a file no longer kept still names the message it held
        let words = words.unwrap_or_else(|| match (kind, file.file_stem()) {
            (SentKind::Message, Some(id)) => format!("message {}", id.to_string_lossy()),
            _ => String::new(),
        });
        return Sent {
            kind,
            text: keep(&words, TEXT_MOST),
        };
    }
    Sent {
        kind: SentKind::Text,
        text: keep(trimmed, TEXT_MOST),
    }
}
fn handed_over(path: &Path, run_dir: &Path) -> Option<String> {
    let path = fs::canonicalize(path).ok()?;
    let root = fs::canonicalize(run_dir).ok()?;
    if !path.starts_with(&root) || path.extension().is_none_or(|e| e != "md") || !plain_file(&path)
    {
        return None;
    }
    let mut bytes = vec![];
    fs::File::open(&path)
        .ok()?
        .take(16 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Each argument by name, its value one string, capped and masked: at most `ARGS_MOST`.
fn args(input: &Value) -> Vec<(String, String)> {
    match input {
        Value::Object(map) => map
            .iter()
            .take(ARGS_MOST)
            .map(|(k, v)| {
                let v = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (k.clone(), keep(&v, ARG_MOST))
            })
            .collect(),
        Value::Null => vec![],
        Value::String(s) => vec![("input".into(), keep(s, ARG_MOST))],
        other => vec![("input".into(), keep(&other.to_string(), ARG_MOST))],
    }
}

/// A tool's name as shown, its kind and its key argument.
fn describe(tool: &str, input: &Value) -> (String, Kind, String) {
    let lower = tool.to_ascii_lowercase();
    let (shown, kind) = if let Some(rest) = tool.strip_prefix("mcp__") {
        let (server, name) = rest.split_once("__").unwrap_or((rest, ""));
        let kind = if server == "sluice" {
            Kind::Message
        } else {
            Kind::Other
        };
        (format!("{server} {name}").trim().to_owned(), kind)
    } else {
        let kind = match lower.as_str() {
            "bash" | "exec" | "shell" | "bashoutput" | "killshell" | "get_output"
            | "kill_shell" | "write_stdin" | "exec_command" => Kind::Shell,
            "edit" | "multiedit" | "multi_edit" | "write" | "notebookedit" | "str_replace"
            | "apply_patch" | "create_file" | "edit_file" | "write_file" => Kind::Edit,
            "read" | "read_file" | "view" | "notebookread" | "view image" | "view_image" => {
                Kind::Read
            }
            "grep" | "search" | "rg" | "codebase_search" => Kind::Search,
            "glob" | "ls" | "list" | "list_files" | "list_dir" | "find" => Kind::List,
            "task" | "agent" | "sidekick" | "run_subagent" | "read_subagent" | "subagent" => {
                Kind::Agent
            }
            "webfetch" | "websearch" | "web_search" | "web_fetch" | "fetch" | "web search" => {
                Kind::Web
            }
            _ => Kind::Other,
        };
        (tool.to_owned(), kind)
    };
    (shown, kind, key(input))
}
/// A call's key argument: its command, path, pattern or query; else its first text argument.
fn key(input: &Value) -> String {
    const KEYS: [&str; 15] = [
        "command",
        "cmd",
        "file_path",
        "notebook_path",
        "path",
        "pattern",
        "query",
        "url",
        "description",
        "prompt",
        "body",
        "skill",
        "shell_id",
        "name",
        "id",
    ];
    let Value::Object(map) = input else {
        return match input {
            Value::String(s) => line(s),
            _ => String::new(),
        };
    };
    let text = KEYS
        .iter()
        .find_map(|k| {
            map.get(*k)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| {
            map.values()
                .find_map(|v| v.as_str().filter(|s| !s.trim().is_empty()))
        });
    text.map(line).unwrap_or_default()
}
/// `text` on one line (its lines joined by a space, runs of space as one), masked and capped:
/// a multi-line command reads by all of it, not by its first line alone.
fn line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    keep(&flat, KEY_MOST)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_result_keeps_its_head_and_tail() {
        let text = format!("{}{}", "a".repeat(3000), "the error at the end");
        let kept = head_tail(&text);
        assert!(kept.starts_with(&"a".repeat(RESULT_HEAD)));
        assert!(kept.ends_with("the error at the end"));
        assert!(kept.contains("characters left out"));
    }
    #[test]
    fn a_token_the_cut_runs_through_is_still_masked() {
        let text = format!(
            "{}token=ghp_FAKEfixture0123456789abcdef and more",
            "x ".repeat(298)
        );
        let kept = keep(&text, 600);
        assert!(!kept.contains("ghp_"), "{kept}");
        assert!(kept.ends_with('…'));
        let long = format!(
            "{}secret ghp_FAKEfixture0123456789abcdefABCDEF tail",
            "y ".repeat(2000)
        );
        assert!(!head_tail(&long).contains("ghp_"));
    }
    #[test]
    fn devins_exit_code_is_read_from_its_output() {
        assert_eq!(exit_code("ok\n\nExit code: 0"), Some(0));
        assert_eq!(exit_code("bad\nExit code: 101"), Some(101));
        assert_eq!(exit_code("no code"), None);
    }
    #[test]
    fn the_key_argument_is_one_line() {
        assert_eq!(
            key(&serde_json::json!({"command": "cd x &&\n  cargo test --all", "description": "x"})),
            "cd x && cargo test --all"
        );
        assert_eq!(key(&serde_json::json!({"query": ""})), "");
    }
}
