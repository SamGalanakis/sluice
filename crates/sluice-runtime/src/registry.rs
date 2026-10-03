//! The fn registry (SPEC §2, §4): which fn a step runs, in scope order —
//! compiled builtins, then `<home>/fns/`, then each configured `fn_dirs` entry
//! in order, then the project's own `projects/<id>/fns/`.
//!
//! A scope scans into entries (one per fn dir, loaded or not); a [`Registry`]
//! combines the scopes a project sees in lookup order and records every
//! problem: a bad fn.json, a name that does not match its directory, or a name
//! that collides with an earlier scope's fn (or a same-scope one). The earlier
//! fn wins and stays resolvable; only the project's own errors block it.
//!
//! `fn_save` writes the loose fn dir and publishes the whole scope as one
//! atomic artifact generation (p2-05's `stage_generation`/`execute`), so an
//! admitted run can pin a complete immutable bundle — the fn plus its sibling
//! helper files — instead of reading live files mid-run. `watch` invalidates
//! on notify events; every access still re-fingerprints, so a missed event can
//! never leave a stale registry behind.

use indexmap::IndexMap;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{Map, Value, json};
use sluice_model::{
    error::PublicError,
    ids::{InvocationId, ProjectId, ProjectSelector, RunId},
    types::{PathError, Type},
};
use sluice_store::{
    StoreError, Writer,
    artifacts::{self, ArtifactJob, Bundle, BundleScope},
    projects,
    writer::{RetrySafety, WriteTransaction},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::builtins::descriptor::{self, BuiltinDescriptor};

/// The largest image icon (bytes) and longest text icon (SPEC §4).
pub const ICON_MAX: u64 = 256 * 1024;
pub const ICON_TEXT_MAX: usize = 16;
/// The icon files a fn dir may hold; a file icon wins over a text `icon`.
const ICON_FILES: [(&str, &str, &str); 3] = [
    ("icon.svg", "svg", "image/svg+xml"),
    ("icon.png", "png", "image/png"),
    ("icon.webp", "webp", "image/webp"),
];
const MANIFEST_KEYS: [&str; 7] = [
    "name", "doc", "inputs", "outputs", "open", "submits", "icon",
];
/// Configured `fn_dirs` travel inside the home generation under this root, so
/// a run pinned to it sees them complete. `@` never starts a fn dir name.
const FN_DIRS_PREFIX: &str = "@fn_dirs";

// ---- scope, icon, fn --------------------------------------------------------

/// Where a fn comes from, in lookup order: compiled, then global (the home's
/// and the configured `fn_dirs`), then the project's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Builtin,
    Global,
    Project,
}
impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}
impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A fn's icon: an image read from its dir (content type, bytes and their
/// sha256, the cache identity), or a short text from fn.json.
#[derive(Debug, Clone, PartialEq)]
pub enum FnIcon {
    Image {
        media_type: String,
        bytes: Vec<u8>,
        hash: String,
    },
    Text(String),
}
impl FnIcon {
    /// fn_list's icon summary: `{kind: "image", type}` or `{kind: "text", text}`.
    pub fn summary(&self) -> Value {
        match self {
            Self::Image { media_type, .. } => {
                json!({"kind": "image", "type": media_type})
            }
            Self::Text(text) => json!({"kind": "text", "text": text}),
        }
    }
}

/// One resolved function: its parsed fn.json (kept verbatim in `raw`), the dir
/// it lives in (`None` for a builtin) and its scope.
#[derive(Debug, Clone)]
pub struct Fn {
    pub name: String,
    pub doc: String,
    pub inputs: IndexMap<String, Type>,
    pub outputs: IndexMap<String, Type>,
    /// The fn.json it was parsed from (a builtin's synthesized manifest).
    pub raw: Value,
    pub dir: Option<PathBuf>,
    pub scope: Scope,
    /// The owning project of a project-scope fn, for generation scoping.
    pub project: Option<ProjectId>,
    /// A step running it may bind extra inputs and declare outputs (§5).
    pub open: bool,
    /// An open fn's outputs its agent submits (step_submit): every step
    /// running it declares them.
    pub submits: IndexMap<String, Type>,
    pub submit_docs: IndexMap<String, String>,
    pub icon: Option<FnIcon>,
    /// The compiled descriptor for `scope == Scope::Builtin`.
    pub builtin: Option<&'static BuiltinDescriptor>,
}
impl Fn {
    /// The catalog entry as a registry fn: builtin scope, no dir.
    pub fn from_descriptor(descriptor: &'static BuiltinDescriptor) -> Self {
        Self {
            name: descriptor.name.to_string(),
            doc: descriptor.doc.to_string(),
            inputs: descriptor
                .inputs
                .iter()
                .map(|(name, ty)| (name.to_string(), ty.clone()))
                .collect(),
            outputs: descriptor
                .outputs
                .iter()
                .map(|(name, ty)| (name.to_string(), ty.clone()))
                .collect(),
            raw: descriptor.manifest(),
            dir: None,
            scope: Scope::Builtin,
            project: None,
            open: descriptor.open,
            submits: descriptor
                .submits
                .iter()
                .map(|(name, decl)| (name.to_string(), decl.ty.clone()))
                .collect(),
            submit_docs: descriptor
                .submits
                .iter()
                .filter_map(|(name, decl)| decl.doc.map(|doc| (name.to_string(), doc.to_string())))
                .collect(),
            icon: descriptor.icon.map(|icon| FnIcon::Image {
                media_type: icon.media_type.to_string(),
                bytes: icon.bytes.to_vec(),
                hash: artifacts::fingerprint(icon.bytes),
            }),
            builtin: Some(descriptor),
        }
    }
    /// Work done outside sluice: its step waits to be settled by hand.
    pub fn external(&self) -> bool {
        self.builtin.is_some_and(|d| d.name == "core.external")
    }
    /// fn_get's reply: the raw manifest plus `scope` and `path` (null for a
    /// builtin, which has no directory).
    pub fn detail(&self) -> Value {
        let mut out = self.raw.clone();
        if let Value::Object(map) = &mut out {
            map.insert("scope".into(), json!(self.scope.label()));
            map.insert(
                "path".into(),
                self.dir
                    .as_ref()
                    .map(|dir| json!(dir.display().to_string()))
                    .unwrap_or(Value::Null),
            );
        }
        out
    }
    /// fn_list's per-fn summary (plus `open`, `submits`, `icon` when present).
    pub fn summary(&self) -> Value {
        let mut out = Map::new();
        out.insert("name".into(), json!(self.name));
        out.insert("doc".into(), json!(self.doc));
        out.insert(
            "inputs".into(),
            self.raw.get("inputs").cloned().unwrap_or(Value::Null),
        );
        out.insert(
            "outputs".into(),
            self.raw.get("outputs").cloned().unwrap_or(Value::Null),
        );
        out.insert("scope".into(), json!(self.scope.label()));
        if !self.submits.is_empty()
            && let Some(submits) = self.raw.get("submits")
        {
            out.insert("submits".into(), submits.clone());
        }
        if let Some(icon) = &self.icon {
            out.insert("icon".into(), icon.summary());
        }
        if self.open {
            out.insert("open".into(), json!(true));
        }
        Value::Object(out)
    }
}

/// One fn dir of a scope: its fn when it loaded and does not collide, else its
/// errors. An entry can carry both — a valid fn that loses a collision.
#[derive(Debug, Clone)]
pub struct FnEntry {
    /// The fn.json name when it is a string, else the directory name.
    pub name: String,
    pub scope: Scope,
    pub dir: Option<PathBuf>,
    pub function: Option<Fn>,
    pub errors: Vec<String>,
}
impl FnEntry {
    /// fn_list's entry: the fn's summary, or the failure shape with `error`.
    pub fn summary(&self) -> Value {
        if let Some(f) = &self.function
            && self.errors.is_empty()
        {
            return f.summary();
        }
        let mut out = Map::new();
        out.insert("name".into(), json!(self.name));
        out.insert("scope".into(), json!(self.scope.label()));
        if let Some(f) = &self.function
            && let Value::Object(summary) = f.summary()
        {
            out.extend(summary);
        }
        out.insert("error".into(), json!(self.errors.join("; ")));
        Value::Object(out)
    }
}

/// A problem the registry found: `{where, message}` like the Python verify
/// output. `where` is the fn.json path for entry errors, the dir itself for
/// scope-level problems.
#[derive(Debug, Clone, PartialEq)]
pub struct FnProblem {
    pub location: String,
    pub message: String,
}
impl FnProblem {
    fn summary(&self) -> Value {
        json!({"where": self.location, "message": self.message})
    }
}

/// The functions one project (or, without one, the global context) sees, in
/// lookup order. Constructed by [`FnRegistry::registry`]; never panics on bad
/// input — problems are data.
#[derive(Debug)]
pub struct Registry {
    entries: Vec<FnEntry>,
    problems: Vec<FnProblem>,
    /// Only the project's own fns can block it (SPEC §2); a broken or
    /// colliding global fn is left out and a plan using it fails validation.
    blocking: Vec<FnProblem>,
    /// Resolved winners: the first error-free entry of each name.
    fns: IndexMap<String, usize>,
}
impl Registry {
    fn assemble(entries: Vec<FnEntry>, dir_problems: Vec<FnProblem>) -> Self {
        // name -> (winning entry's index, scope, dir) for collision messages
        let mut fns: IndexMap<String, (usize, Scope, Option<PathBuf>)> = IndexMap::new();
        let mut problems = dir_problems;
        let mut blocking = Vec::new();
        let mut entries = entries;
        for (index, entry) in entries.iter_mut().enumerate() {
            if entry.function.is_some() && entry.errors.is_empty() {
                match fns.get(&entry.name) {
                    None => {
                        fns.insert(entry.name.clone(), (index, entry.scope, entry.dir.clone()));
                        continue;
                    }
                    Some((_, scope, dir)) => {
                        let at = dir
                            .as_ref()
                            .map(|d| format!(" at {}", d.display()))
                            .unwrap_or_default();
                        entry.errors.push(format!(
                            "fn {} collides with the {scope} fn{at}",
                            entry.name
                        ));
                    }
                }
            }
            let location = entry
                .dir
                .as_ref()
                .map(|dir| dir.join("fn.json").display().to_string())
                .unwrap_or_else(|| "the builtin catalog".into());
            for message in entry.errors.clone() {
                let problem = FnProblem {
                    location: location.clone(),
                    message,
                };
                problems.push(problem.clone());
                if entry.scope == Scope::Project {
                    blocking.push(problem);
                }
            }
        }
        Self {
            entries,
            problems,
            blocking,
            fns: fns
                .into_iter()
                .map(|(name, (index, _, _))| (name, index))
                .collect(),
        }
    }
    /// The winning fn for a name, if any error-free entry claims it.
    pub fn get(&self, name: &str) -> Option<&Fn> {
        self.fns
            .get(name)
            .and_then(|index| self.entries[*index].function.as_ref())
    }
    /// Every fn dir in lookup order; ones with problems carry `error`.
    pub fn entries(&self) -> &[FnEntry] {
        &self.entries
    }
    /// fn_list: every fn dir in lookup order.
    pub fn listing(&self) -> Vec<Value> {
        self.entries.iter().map(FnEntry::summary).collect()
    }
    /// Resolved names, sorted.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.fns.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }
    /// Every problem found, in scan order (dir problems first).
    pub fn problems(&self) -> &[FnProblem] {
        &self.problems
    }
    /// The problems that block the project (its own scope's errors).
    pub fn blocking(&self) -> &[FnProblem] {
        &self.blocking
    }
    /// verify's view of fn problems, `{where, message}` objects.
    pub fn problems_summary(&self) -> Vec<Value> {
        self.problems.iter().map(FnProblem::summary).collect()
    }
}

// ---- manifest validation (parse_fn) -----------------------------------------

fn name_ok(name: &str) -> bool {
    let mut parts = name.split('.');
    let valid_part = |part: &str| {
        !part.is_empty()
            && part.starts_with(|c: char| c.is_ascii_lowercase())
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    parts.next().is_some_and(valid_part) && parts.all(valid_part) && name.contains('.')
}

/// A Python `repr` for the fn.json values that can appear as `name`.
fn py_repr(value: &Value) -> String {
    match value {
        Value::String(s) => format!("'{s}'"),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Null => "None".into(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// The fn.json type grammar's declaration form: a bare type, or
/// `{"type": T, "doc": "..."}` (CWL's object form). Returns (type, doc).
fn parse_decl(form: &Value, path: &str, errs: &mut Vec<String>) -> (Option<Type>, String) {
    let mut doc = String::new();
    let (form, path) = match form {
        Value::Object(object) if object.keys().all(|k| k == "type" || k == "doc") => {
            if !object.contains_key("type") {
                errs.push(format!("{path}.type: required"));
                return (None, doc);
            }
            match object.get("doc") {
                Some(Value::String(s)) => doc = s.clone(),
                Some(_) => errs.push(format!("{path}.doc: expected a string")),
                None => (),
            }
            (&object["type"], format!("{path}.type"))
        }
        _ => (form, path.to_string()),
    };
    (parse_type(form, &path, errs), doc)
}

/// `Type::parse` with its `type`-rooted error paths rewritten under `path`,
/// the way Python's `T.parse(form, f"{key}.{port}")` names them.
fn parse_type(form: &Value, path: &str, errs: &mut Vec<String>) -> Option<Type> {
    match Type::parse(form) {
        Ok(ty) => Some(ty),
        Err(error) => {
            errs.push(repath(error, path));
            None
        }
    }
}
fn repath(error: PathError, path: &str) -> String {
    let full = if error.path == "type" {
        path.to_string()
    } else if let Some(rest) = error.path.strip_prefix("type.") {
        format!("{path}.{rest}")
    } else {
        error.path
    };
    format!("{full}: {}", error.message)
}

/// Validate one fn.json (SPEC §4, §6a). `dir` is the fn's directory when the
/// manifest already lives there — it enables the dir-name check, the icon
/// files and the `main.py` requirement; pass `None` for a manifest being saved
/// (fn_save) or a compiled descriptor. Returns (fn or None, every problem).
pub fn parse_manifest(
    raw: &Value,
    dir: Option<&Path>,
    scope: Scope,
    project: Option<ProjectId>,
) -> (Option<Fn>, Vec<String>) {
    let Value::Object(object) = raw else {
        return (
            None,
            vec!["expected an object {name, doc?, inputs, outputs, open?, submits?, icon?}".into()],
        );
    };
    let mut errs: Vec<String> = object
        .keys()
        .filter(|key| !MANIFEST_KEYS.contains(&key.as_str()))
        .map(|key| format!("unknown key '{key}'"))
        .collect();
    let name = object.get("name");
    let name_valid = match name {
        Some(Value::String(name)) if name_ok(name) => {
            if let Some(dir) = dir
                && dir.file_name().is_some_and(|d| d != name.as_str())
            {
                errs.push(format!(
                    "name {name} does not match its directory {}",
                    dir.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            true
        }
        _ => {
            errs.push(format!(
                "name must be dotted lowercase like 'git.head', got {}",
                name.map(py_repr).unwrap_or_else(|| "None".into())
            ));
            false
        }
    };
    if !matches!(object.get("doc"), None | Some(Value::String(_))) {
        errs.push("doc must be a string".into());
    }
    if !matches!(object.get("open"), None | Some(Value::Bool(_))) {
        errs.push("open must be a boolean".into());
    }
    let mut ports: IndexMap<&str, IndexMap<String, Type>> = IndexMap::new();
    for key in ["inputs", "outputs"] {
        match object.get(key) {
            Some(Value::Object(spec)) => {
                let parsed = spec
                    .iter()
                    .filter_map(|(port, form)| {
                        parse_type(form, &format!("{key}.{port}"), &mut errs)
                            .map(|ty| (port.clone(), ty))
                    })
                    .collect();
                ports.insert(key, parsed);
            }
            _ => errs.push(format!("{key} is required, an object of name -> type")),
        }
    }
    let empty = IndexMap::new();
    let (submits, submit_docs) =
        parse_submits(object, ports.get("outputs").unwrap_or(&empty), &mut errs);
    let icon = parse_icon(object, dir, &mut errs);
    if dir.is_some()
        && !(scope == Scope::Builtin)
        && !dir.is_some_and(|d| d.join("main.py").is_file())
    {
        errs.push("main.py is missing".into());
    }
    if !errs.is_empty() || !name_valid {
        return (None, errs);
    }
    let name = name.and_then(Value::as_str).unwrap_or_default().to_string();
    (
        Some(Fn {
            doc: object
                .get("doc")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            inputs: ports.swap_remove("inputs").unwrap_or_default(),
            outputs: ports.swap_remove("outputs").unwrap_or_default(),
            raw: raw.clone(),
            dir: dir.map(Path::to_path_buf),
            scope,
            project,
            open: object.get("open") == Some(&Value::Bool(true)),
            submits,
            submit_docs,
            icon,
            builtin: None,
            name,
        }),
        vec![],
    )
}

/// An open fn's `submits`: {name: type or {"type", "doc"}}, outputs its agent
/// submits with step_submit, as if every step running it declared them (§5).
fn parse_submits(
    object: &Map<String, Value>,
    outputs: &IndexMap<String, Type>,
    errs: &mut Vec<String>,
) -> (IndexMap<String, Type>, IndexMap<String, String>) {
    let mut types = IndexMap::new();
    let mut docs = IndexMap::new();
    let Some(spec) = object.get("submits") else {
        return (types, docs);
    };
    let Value::Object(spec) = spec else {
        errs.push("submits must be an object of name -> type".into());
        return (types, docs);
    };
    if object.get("open") != Some(&Value::Bool(true)) {
        errs.push("submits needs open: true (an open fn's agent submits outputs)".into());
    }
    for (port, form) in spec {
        if outputs.contains_key(port) {
            errs.push(format!("submits.{port}: already an output of the fn"));
            continue;
        }
        let (ty, doc) = parse_decl(form, &format!("submits.{port}"), errs);
        if let Some(ty) = ty {
            types.insert(port.clone(), ty);
        }
        if !doc.is_empty() {
            docs.insert(port.clone(), doc);
        }
    }
    (types, docs)
}

// ---- icons ------------------------------------------------------------------

/// Why a (stripped) text is no text icon, mirroring util.text_icon_problem.
fn text_icon_problem(text: &str) -> Option<&'static str> {
    if text.chars().count() > ICON_TEXT_MAX {
        return Some("a text icon is at most 16 characters");
    }
    if text.chars().any(|c| c.is_control()) {
        return Some("a text icon may not contain control characters");
    }
    None
}

/// Which icon type `data` is: an SVG parses as XML with an `<svg>` root, the
/// others match magic bytes. The SVG check skips prologs, comments and the
/// doctype and accepts a namespace prefix, as ElementTree's parse does.
fn sniff_image(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("png");
    }
    if data.starts_with(b"\xff\xd8\xff") {
        return Some("jpg");
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some("gif");
    }
    if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return Some("webp");
    }
    if is_svg(data) { Some("svg") } else { None }
}

/// The document's root element is `<svg>` (any namespace). Skips the BOM,
/// whitespace, `<?...?>`, `<!--...-->` and `<!DOCTYPE ...>` trivia.
fn is_svg(data: &[u8]) -> bool {
    let text = match std::str::from_utf8(data) {
        Ok(text) => text.trim_start_matches('\u{feff}').trim_start(),
        Err(_) => return false,
    };
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        if let Some(end) = rest
            .strip_prefix("<?")
            .and_then(|r| r.find("?>").map(|i| &r[i + 2..]))
        {
            rest = end;
        } else if let Some(end) = rest
            .strip_prefix("<!--")
            .and_then(|r| r.find("-->").map(|i| &r[i + 3..]))
        {
            rest = end;
        } else if let Some(end) = rest
            .strip_prefix("<!")
            .and_then(|r| r.find('>').map(|i| &r[i + 1..]))
        {
            rest = end;
        } else {
            break;
        }
    }
    let Some(tag) = rest.strip_prefix('<') else {
        return false;
    };
    let name: String = tag
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '>' | '/'))
        .collect();
    name.rsplit(':').next() == Some("svg") && !name.is_empty()
}

/// A fn's icon: `icon.svg`, `icon.png` or `icon.webp` in its dir (at most 256
/// KB, its content of the type its name says), else a text `icon` in fn.json
/// (at most 16 characters, no control characters). Both may be there; the file
/// wins. `dir` is `None` for a manifest not in its dir yet (fn_save) — then
/// only the text icon applies.
fn parse_icon(
    object: &Map<String, Value>,
    dir: Option<&Path>,
    errs: &mut Vec<String>,
) -> Option<FnIcon> {
    let text = object.get("icon").and_then(|raw| match raw {
        Value::String(text) if !text.trim().is_empty() => match text_icon_problem(text.trim()) {
            None => Some(text.trim().to_string()),
            Some(problem) => {
                errs.push(format!("icon: {problem}"));
                None
            }
        },
        _ => {
            errs.push("icon must be a short text, e.g. an emoji".into());
            None
        }
    });
    let files: Vec<(&'static str, &'static str, &'static str, PathBuf)> = match dir {
        Some(dir) => ICON_FILES
            .iter()
            .map(|(file, ext, media)| (*file, *ext, *media, dir.join(file)))
            .filter(|(.., path)| path.is_file())
            .collect(),
        None => vec![],
    };
    if files.len() > 1 {
        let names = files
            .iter()
            .map(|(name, ..)| *name)
            .collect::<Vec<_>>()
            .join(" and ");
        errs.push(format!("icon: {names} are both there; keep one"));
        return None;
    }
    let Some((name, ext, media_type, path)) = files.into_iter().next() else {
        return text.map(FnIcon::Text);
    };
    let meta = match path.metadata() {
        Ok(meta) => meta,
        Err(error) => {
            errs.push(format!("icon: {name} is not readable: {error}"));
            return None;
        }
    };
    let data = match meta.len() <= ICON_MAX {
        true => match fs::read(&path) {
            Ok(data) => data,
            Err(error) => {
                errs.push(format!("icon: {name} is not readable: {error}"));
                return None;
            }
        },
        false => Vec::new(),
    };
    if meta.len() > ICON_MAX {
        errs.push(format!("icon: {name} is over {} KB", ICON_MAX / 1024));
    } else if sniff_image(&data) != Some(ext) {
        let kind = match ext {
            "svg" => "an SVG",
            "png" => "a PNG",
            _ => "a WebP",
        };
        errs.push(format!("icon: {name} is not {kind} image"));
    } else {
        return Some(FnIcon::Image {
            media_type: media_type.to_string(),
            hash: artifacts::fingerprint(&data),
            bytes: data,
        });
    }
    None
}

// ---- scans and fingerprints --------------------------------------------------

/// What a scan depends on: every fn dir's fn.json, main.py and icon file with
/// its mtime and size, plus which scope dirs exist.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FingerprintItem {
    Dir(PathBuf, bool),
    File(PathBuf, u128, u64),
}
type Fingerprint = Vec<FingerprintItem>;

fn fingerprint(dirs: &[PathBuf]) -> Fingerprint {
    let mut out = Vec::new();
    for dir in dirs {
        out.push(FingerprintItem::Dir(dir.clone(), dir.is_dir()));
        let mut files: Vec<PathBuf> = Vec::new();
        if let Ok(children) = fs::read_dir(dir) {
            for child in children.flatten() {
                for file in ["fn.json", "main.py"] {
                    let path = child.path().join(file);
                    if path.is_file() {
                        files.push(path);
                    }
                }
                if let Ok(icons) = fs::read_dir(child.path()) {
                    files.extend(icons.flatten().map(|icon| icon.path()).filter(|path| {
                        path.is_file()
                            && path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(|n| n.starts_with("icon."))
                    }));
                }
            }
        }
        files.sort();
        for path in files {
            if let Ok(meta) = path.metadata() {
                out.push(FingerprintItem::File(
                    path,
                    meta.modified()
                        .map(|t| {
                            t.duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_nanos())
                                .unwrap_or(0)
                        })
                        .unwrap_or(0),
                    meta.len(),
                ));
            }
        }
    }
    out
}

/// One scope position's cached scan.
struct ScopeScan {
    fingerprint: Fingerprint,
    entries: Vec<FnEntry>,
    problems: Vec<FnProblem>,
}

/// Every fn dir (an immediate subdirectory holding fn.json) of `dirs`, in
/// order; problems name dirs themselves (e.g. a configured dir that is
/// missing). Never fails: a bad fn.json is an entry with errors.
fn scan_dirs(
    scope: Scope,
    dirs: &[PathBuf],
    missing_ok: &[&Path],
    project: Option<ProjectId>,
) -> ScopeScan {
    let mut entries = Vec::new();
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for dir in dirs {
        if !seen.insert(dir) {
            continue;
        }
        if !dir.is_dir() {
            if !missing_ok.contains(&dir.as_path()) {
                problems.push(FnProblem {
                    location: dir.display().to_string(),
                    message: "fn directory does not exist".into(),
                });
            }
            continue;
        }
        let mut children: Vec<PathBuf> = fs::read_dir(dir)
            .map(|list| list.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        children.sort();
        for child in children {
            let manifest_path = child.join("fn.json");
            if !manifest_path.is_file() {
                continue;
            }
            let dir_name = child
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let raw: Result<Value, String> = fs::read(&manifest_path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()));
            let (entry_name, function, errors) = match raw {
                Err(error) => (dir_name.clone(), None, vec![format!("bad JSON: {error}")]),
                Ok(raw) => {
                    let (function, errors) = parse_manifest(&raw, Some(&child), scope, project);
                    let name = raw
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(&dir_name)
                        .to_string();
                    (name, function, errors)
                }
            };
            entries.push(FnEntry {
                name: entry_name,
                scope,
                dir: Some(child),
                function,
                errors,
            });
        }
    }
    ScopeScan {
        fingerprint: fingerprint(dirs),
        entries,
        problems,
    }
}

// ---- the registry façade -----------------------------------------------------

/// One scope position the registry watches and caches under.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    Home,
    FnDir(usize),
    Project(ProjectId),
}
impl Slot {
    fn dirs(&self, home: &Path, fn_dirs: &[PathBuf]) -> Vec<PathBuf> {
        match self {
            Self::Home => vec![home.join("fns")],
            Self::FnDir(index) => vec![fn_dirs[*index].clone()],
            Self::Project(id) => vec![home.join("projects").join(id.to_string()).join("fns")],
        }
    }
    fn scope(&self) -> Scope {
        match self {
            Self::Project(_) => Scope::Project,
            _ => Scope::Global,
        }
    }
    fn project(&self) -> Option<ProjectId> {
        match self {
            Self::Project(id) => Some(*id),
            _ => None,
        }
    }
    /// A missing scope dir is a problem for configured `fn_dirs`, expected
    /// for the home's and the project's fns.
    fn optional(&self) -> bool {
        !matches!(self, Self::FnDir(_))
    }
}

/// The store-side façade over the fn scopes: a home, the configured
/// `fn_dirs`, cached scans invalidated by fingerprint and (optionally) a
/// notify watcher. `registry` re-derives the view on every call — it is cheap
/// (fingerprint stats only on cache hits) and never misses a change.
pub struct FnRegistry {
    home: PathBuf,
    fn_dirs: Vec<PathBuf>,
    scans: Mutex<BTreeMap<Slot, Arc<ScopeScan>>>,
    version: Arc<AtomicU64>,
}
impl FnRegistry {
    /// A registry over `home` and extra global fn dirs (already resolved).
    pub fn open(home: impl Into<PathBuf>, fn_dirs: Vec<PathBuf>) -> Self {
        Self {
            home: home.into(),
            fn_dirs,
            scans: Mutex::new(BTreeMap::new()),
            version: Arc::new(AtomicU64::new(0)),
        }
    }
    /// `fn_dirs` from the home's config.json: `[home/fns, *(home/d for d in
    /// config["fn_dirs"])]`; a missing or list-less config means `fns` alone.
    pub fn configured(home: impl Into<PathBuf>) -> Result<Self, PublicError> {
        let home = home.into();
        let config_path = home.join("config.json");
        let fn_dirs = match fs::read(&config_path) {
            Ok(bytes) => {
                let config: Value =
                    serde_json::from_slice(&bytes).map_err(|e| PublicError::BadRequest {
                        message: format!("{}: bad JSON: {e}", config_path.display()),
                    })?;
                match config.get("fn_dirs") {
                    None | Some(Value::Null) => Vec::new(),
                    Some(Value::Array(list)) => list
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|dir| home.join(dir))
                        .collect(),
                    Some(_) => {
                        return Err(PublicError::BadRequest {
                            message: "config.json fn_dirs must be a list of relative dirs".into(),
                        });
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return Err(PublicError::BadRequest {
                    message: format!("{}: {e}", config_path.display()),
                });
            }
        };
        Ok(Self::open(home, fn_dirs))
    }

    /// The home this registry reads.
    pub fn home(&self) -> &Path {
        &self.home
    }
    /// The configured extra fn dirs, in order.
    pub fn fn_dirs(&self) -> &[PathBuf] {
        &self.fn_dirs
    }
    /// Bumped whenever a notify event lands or a lazy fingerprint sees a
    /// change; consumers subscribe through [`FnWatcher`].
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// The functions `project` (or, without one, the global context) sees.
    pub fn registry(&self, project: Option<ProjectId>) -> Registry {
        let mut slots = vec![Slot::Home];
        slots.extend((0..self.fn_dirs.len()).map(Slot::FnDir));
        if let Some(id) = project {
            slots.push(Slot::Project(id));
        }
        let mut entries: Vec<FnEntry> = descriptor::catalog()
            .iter()
            .map(|d| FnEntry {
                name: d.name.to_string(),
                scope: Scope::Builtin,
                dir: None,
                function: Some(Fn::from_descriptor(d)),
                errors: vec![],
            })
            .collect();
        let mut problems = Vec::new();
        for slot in slots {
            let scan = self.scan(&slot);
            entries.extend(scan.entries.iter().cloned());
            problems.extend(scan.problems.iter().cloned());
        }
        Registry::assemble(entries, problems)
    }

    /// The registry, refusing when the project's own fns have problems (SPEC
    /// §2): plan edits, manual values, fn_call and runs wait until fixed.
    pub fn usable_registry(&self, project: Option<ProjectId>) -> Result<Registry, PublicError> {
        let registry = self.registry(project);
        if registry.blocking.is_empty() {
            return Ok(registry);
        }
        let who = match project {
            Some(id) => format!("project {id}"),
            None => "the global functions".into(),
        };
        Err(PublicError::Invalid {
            message: format!(
                "{who}: function problems block edits and runs until fixed (see verify)"
            ),
            errors: registry
                .blocking
                .iter()
                .map(|p| format!("{}: {}", p.location, p.message))
                .collect(),
        })
    }

    /// One resolved fn, or NotFound (`fn_get`/`fn_call`'s failure).
    pub fn get(&self, name: &str, project: Option<ProjectId>) -> Result<Fn, PublicError> {
        self.registry(project)
            .get(name)
            .cloned()
            .ok_or_else(|| PublicError::NotFound {
                message: format!(
                    "no fn '{name}'{}",
                    project
                        .map(|id| format!(" in project {id}"))
                        .unwrap_or_default()
                ),
            })
    }

    /// One slot's scan, cached by fingerprint. A changed fingerprint also
    /// bumps `version`, reconciling notify events the watcher may have missed.
    fn scan(&self, slot: &Slot) -> Arc<ScopeScan> {
        let dirs = slot.dirs(&self.home, &self.fn_dirs);
        let missing_ok: Vec<&Path> = if slot.optional() {
            dirs.iter().map(PathBuf::as_path).collect()
        } else {
            Vec::new()
        };
        let current = fingerprint(&dirs);
        let mut scans = self.scans.lock().unwrap_or_else(|e| e.into_inner());
        match scans.get(slot) {
            Some(scan) if scan.fingerprint == current => return scan.clone(),
            _ => (),
        }
        if scans.contains_key(slot) {
            self.version.fetch_add(1, Ordering::AcqRel);
        }
        let scan = Arc::new(scan_dirs(slot.scope(), &dirs, &missing_ok, slot.project()));
        scans.insert(slot.clone(), scan.clone());
        scan
    }

    /// Watch every fn scope for changes: `home/fns` and each `fn_dirs` dir
    /// recursively, `home/projects` non-recursively (to catch new project fns
    /// dirs), and every existing `projects/*/fns` recursively. Generations and
    /// run artifacts under `projects/` are not watched — registry state never
    /// depends on them.
    pub fn watch(&self) -> Result<FnWatcher, PublicError> {
        let (tx, rx) = tokio::sync::watch::channel(self.version());
        let version = self.version.clone();
        let watcher =
            notify::recommended_watcher(move |result: Result<notify::Event, notify::Error>| {
                if result.is_ok() {
                    let new = version.fetch_add(1, Ordering::AcqRel) + 1;
                    let _ = tx.send(new);
                }
            })
            .map_err(|e| PublicError::BadRequest {
                message: format!("fn watcher: {e}"),
            })?;
        let mut watcher = FnWatcher {
            watcher,
            home: self.home.clone(),
            fn_dirs: self.fn_dirs.clone(),
            rx,
        };
        watcher.arm();
        Ok(watcher)
    }
}

/// A live notify subscription to the fn scopes. `changed` resolves on the
/// next event or fingerprint reconciliation; callers then rebuild the view
/// with `registry(...)`, which is authoritative regardless. Because a scope
/// dir may appear after `watch` was called, every observed event re-arms the
/// watches so a new `fns/` or `projects/<id>/fns` is covered at once.
pub struct FnWatcher {
    watcher: RecommendedWatcher,
    /// The watched home, for diagnostics.
    pub home: PathBuf,
    fn_dirs: Vec<PathBuf>,
    rx: tokio::sync::watch::Receiver<u64>,
}
impl FnWatcher {
    /// Register watches on every scope dir that exists now: `fns` and each
    /// `fn_dirs` entry recursively, the home and `projects` non-recursively
    /// (so a new `fns` or a new project's `fns` is itself an event), and every
    /// existing `projects/*/fns` recursively. Generations and run artifacts
    /// are not watched — registry state never depends on them.
    fn arm(&mut self) {
        let watch = |watcher: &mut RecommendedWatcher, dir: &Path, mode: RecursiveMode| {
            if dir.is_dir() {
                let _ = watcher.watch(dir, mode);
            }
        };
        watch(&mut self.watcher, &self.home, RecursiveMode::NonRecursive);
        watch(
            &mut self.watcher,
            &self.home.join("fns"),
            RecursiveMode::Recursive,
        );
        for dir in &self.fn_dirs {
            watch(&mut self.watcher, dir, RecursiveMode::Recursive);
        }
        let projects = self.home.join("projects");
        watch(&mut self.watcher, &projects, RecursiveMode::NonRecursive);
        if let Ok(children) = fs::read_dir(&projects) {
            for fns in children
                .flatten()
                .map(|c| c.path().join("fns"))
                .filter(|d| d.is_dir())
            {
                watch(&mut self.watcher, &fns, RecursiveMode::Recursive);
            }
        }
    }
    /// The last seen version.
    pub fn version(&self) -> u64 {
        *self.rx.borrow()
    }
    /// Wait for the next observed change, re-arming watches afterward.
    pub async fn changed(&mut self) -> Result<u64, PublicError> {
        let version = self
            .rx
            .changed()
            .await
            .map(|_| *self.rx.borrow())
            .map_err(|_| PublicError::BadRequest {
                message: "fn watcher closed".into(),
            })?;
        self.arm();
        Ok(version)
    }
}

// ---- fn_save -----------------------------------------------------------------

/// What `fn_save` validated and wrote: the scope, the fn's name and dir, and
/// the generation the scope was published as.
#[derive(Debug, Clone)]
pub struct SavedFn {
    pub scope: Scope,
    pub name: String,
    pub path: PathBuf,
    pub generation: i64,
}

/// The scope a generation covers: the home's `fns` plus configured `fn_dirs`
/// under `@fn_dirs/<i>/`, or one project's own `fns`.
pub enum PublishScope<'a> {
    Home { fn_dirs: &'a [PathBuf] },
    Project(ProjectId),
}
impl PublishScope<'_> {
    fn bundle_scope(&self) -> BundleScope {
        match self {
            Self::Home { .. } => BundleScope::Home,
            Self::Project(id) => BundleScope::Project(*id),
        }
    }
}

/// The manifest fingerprint exactly as `artifacts::manifest` computes it, for
/// the "unchanged → keep the published generation" check.
fn bundle_digest(bundle: &Bundle) -> String {
    let files: BTreeMap<String, String> = bundle
        .files()
        .iter()
        .map(|(path, bytes)| {
            (
                path.clone(),
                bytes.iter().map(|b| format!("{b:02x}")).collect(),
            )
        })
        .collect();
    artifacts::fingerprint(&serde_json::to_vec(&files).expect("string map serializes"))
}

/// The scope's complete file map: every regular file under each source dir,
/// at its bundle path (`fns` contents at top level, `fn_dirs[i]` under
/// `@fn_dirs/<i>/`). `fns/generations` — the scope's own publication point —
/// is not part of the bundle; symlinks and other non-files are skipped.
fn scope_bundle(home: &Path, scope: &PublishScope<'_>) -> Result<Bundle, PublicError> {
    let roots: Vec<(PathBuf, String)> = match scope {
        PublishScope::Home { fn_dirs } => {
            let mut roots = vec![(home.join("fns"), String::new())];
            for (index, dir) in fn_dirs.iter().enumerate() {
                roots.push((dir.clone(), format!("{FN_DIRS_PREFIX}/{index}/")));
            }
            roots
        }
        PublishScope::Project(id) => vec![(
            home.join("projects").join(id.to_string()).join("fns"),
            String::new(),
        )],
    };
    let mut files = BTreeMap::new();
    for (root, prefix) in roots {
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(children) = fs::read_dir(&dir) else {
                continue;
            };
            for child in children.flatten() {
                let path = child.path();
                let Ok(meta) = child.metadata() else { continue };
                if meta.is_dir() {
                    // the home fns' own publication point is not content
                    if dir == root
                        && root == home.join("fns")
                        && path.file_name().is_some_and(|n| n == "generations")
                    {
                        continue;
                    }
                    stack.push(path);
                } else if meta.is_file() {
                    let relative = path
                        .strip_prefix(&root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    let bytes = fs::read(&path).map_err(|e| PublicError::BadRequest {
                        message: format!("{}: {e}", path.display()),
                    })?;
                    files.insert(format!("{prefix}{relative}"), bytes);
                }
            }
        }
    }
    Bundle::new(files).map_err(|e| PublicError::BadRequest {
        message: e.to_string(),
    })
}

/// Publish the scope's loose files as one immutable artifact generation when
/// they differ from the published manifest; returns the published job. Stage
/// and check run inside the writer transaction; the file copy runs through
/// p2-05's `artifacts::execute`.
pub async fn publish(
    writer: &Writer,
    home: &Path,
    scope: PublishScope<'_>,
) -> Result<ArtifactJob, PublicError> {
    let bundle = scope_bundle(home, &scope)?;
    let digest = bundle_digest(&bundle);
    let bundle_scope = scope.bundle_scope();
    let job_id = writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            if let Some(current) = artifacts::published_generation(tx.sql(), bundle_scope)?
                && current.manifest["fingerprint"].as_str() == Some(digest.as_str())
            {
                return Ok(None);
            }
            artifacts::stage_generation(tx, bundle_scope, bundle).map(Some)
        })
        .await?;
    let job_id = match job_id {
        Some(id) => id,
        None => {
            return writer
                .write(RetrySafety::Idempotent, move |tx| {
                    artifacts::published_generation(tx.sql(), bundle_scope)?.ok_or_else(|| {
                        StoreError::from(PublicError::Storage {
                            message: "no published generation after publish".into(),
                        })
                    })
                })
                .await;
        }
    };
    artifacts::execute(writer, home, job_id)
        .await
        .map_err(|e| e.into_public(false))?;
    writer
        .write(RetrySafety::Idempotent, move |tx| {
            artifacts::job(tx.sql(), job_id)
        })
        .await
}

/// Write `<file>.tmp`, fsync, rename (SPEC §2); the parent dir is fsynced too.
fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let leaf = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!("{leaf}.tmp"));
    fs::write(&tmp, bytes)?;
    fs::File::open(&tmp)?.sync_all()?;
    fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// `fn_save` (SPEC §2): validate the manifest, refuse names that collide with
/// another scope, write `fns/<name>/{fn.json,main.py}` and publish the scope
/// as one atomic generation. `main_py` is required — a custom fn is Python.
pub async fn save(
    registry: &FnRegistry,
    writer: &Writer,
    raw: &Value,
    main_py: &str,
    project: Option<ProjectSelector>,
) -> Result<SavedFn, PublicError> {
    let scope = if project.is_some() {
        Scope::Project
    } else {
        Scope::Global
    };
    let (_, mut errs) = parse_manifest(raw, None, scope, None);
    if main_py.trim().is_empty() {
        errs.push("main_py: the fn's Python source is required".into());
    }
    if !errs.is_empty() {
        return Err(PublicError::Invalid {
            message: "not a valid fn".into(),
            errors: errs,
        });
    }
    let name = raw["name"].as_str().unwrap_or_default().to_string();

    let project_id = match &project {
        Some(selector) => {
            let selector = selector.clone();
            Some(
                writer
                    .write(RetrySafety::Idempotent, move |tx| {
                        projects::resolve(tx.sql(), &selector).map(|p| p.project_id)
                    })
                    .await?,
            )
        }
        None => None,
    };

    let root = match project_id {
        Some(id) => registry
            .home
            .join("projects")
            .join(id.to_string())
            .join("fns"),
        None => registry.home.join("fns"),
    };
    let target = root.join(&name);

    let mut clash: Vec<String> = Vec::new();
    match project_id {
        Some(_) => {
            // A project fn may not shadow a builtin or a good global fn.
            if let Some(other) = registry.registry(None).get(&name) {
                let at = other
                    .dir
                    .as_ref()
                    .map(|d| format!(" at {}", d.display()))
                    .unwrap_or_default();
                clash.push(format!("the {} fn{at}", other.scope));
            }
        }
        None => {
            // A global fn may not shadow any entry of another dir, nor any
            // project's own fn of the name.
            for entry in registry.registry(None).entries() {
                if entry.name == name && entry.dir.as_ref() != Some(&target) {
                    let at = entry
                        .dir
                        .as_ref()
                        .map(|d| format!(" at {}", d.display()))
                        .unwrap_or_default();
                    clash.push(format!("the {} fn{at}", entry.scope));
                }
            }
            let project_names: Vec<(String, String)> = writer
                .write(RetrySafety::Idempotent, |tx| {
                    let mut stmt = tx
                        .sql()
                        .prepare("SELECT project_id,name FROM projects WHERE deleted_at IS NULL")?;
                    let rows = stmt
                        .query_map([], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    Ok(rows)
                })
                .await?;
            for (id, pname) in project_names {
                if registry
                    .home
                    .join("projects")
                    .join(&id)
                    .join("fns")
                    .join(&name)
                    .join("fn.json")
                    .exists()
                {
                    clash.push(format!("the fn of project {pname}"));
                }
            }
        }
    }
    if !clash.is_empty() {
        return Err(PublicError::BadRequest {
            message: format!("fn {name} would collide with {}", clash.join(", ")),
        });
    }

    fs::create_dir_all(&target).map_err(|e| PublicError::BadRequest {
        message: format!("{}: {e}", target.display()),
    })?;
    // main.py first, then fn.json: a crash leaves a dir scan ignores.
    atomic_write(&target.join("main.py"), main_py.as_bytes()).map_err(|e| {
        PublicError::BadRequest {
            message: format!("{}: {e}", target.join("main.py").display()),
        }
    })?;
    let manifest = serde_json::to_string_pretty(raw).map_err(|e| PublicError::BadRequest {
        message: e.to_string(),
    })?;
    atomic_write(&target.join("fn.json"), format!("{manifest}\n").as_bytes()).map_err(|e| {
        PublicError::BadRequest {
            message: format!("{}: {e}", target.join("fn.json").display()),
        }
    })?;
    registry.invalidate();

    let publish_scope = match project_id {
        Some(id) => PublishScope::Project(id),
        None => PublishScope::Home {
            fn_dirs: &registry.fn_dirs,
        },
    };
    let job = publish(writer, &registry.home, publish_scope).await?;
    Ok(SavedFn {
        scope,
        name,
        path: target,
        generation: job.generation,
    })
}

// ---- pins and dispatch --------------------------------------------------------

/// The generation a run pins for its resolved fn: `None` for a builtin. The
/// scope is published first when loose files moved since the last generation.
pub async fn prepare_run_pin(
    writer: &Writer,
    registry: &FnRegistry,
    resolved: &Fn,
) -> Result<Option<ArtifactJob>, PublicError> {
    let scope = match resolved.scope {
        Scope::Builtin => return Ok(None),
        Scope::Global => PublishScope::Home {
            fn_dirs: &registry.fn_dirs,
        },
        Scope::Project => match resolved.project {
            Some(id) => PublishScope::Project(id),
            None => {
                return Err(PublicError::BadRequest {
                    message: format!("fn {} has no owning project", resolved.name),
                });
            }
        },
    };
    publish(writer, registry.home(), scope).await.map(Some)
}

/// Pin `job` to `run` in the admission transaction (p2-05's fencing keeps a
/// pinned generation alive); a small re-export so run admission reads as one
/// step.
pub fn pin_run(
    tx: &mut WriteTransaction<'_>,
    job: InvocationId,
    run: RunId,
) -> Result<(), sluice_store::StoreError> {
    artifacts::pin_generation(tx, job, run)
}

/// The published bundle a run executes from: the generation dir plus the fn's
/// relative position in it (`fns/<name>` at top level for home and project
/// scopes, `@fn_dirs/<i>/<name>` for a configured dir).
#[derive(Debug, Clone)]
pub struct PinnedBundle {
    pub job_id: InvocationId,
    pub generation: i64,
    /// The generation root, e.g. `<home>/fns/generations/3`.
    pub dir: PathBuf,
    /// The fn's dir inside the bundle — sibling helpers sit next to it.
    pub fn_dir: PathBuf,
    /// The generation manifest (`{version, files, fingerprint}`).
    pub manifest: Value,
}

/// Where the resolved fn's dir lands inside a published bundle, by matching
/// its parent against the scope roots.
fn bundle_path(registry: &FnRegistry, resolved: &Fn) -> Option<String> {
    let parent = resolved.dir.as_ref()?.parent()?.to_path_buf();
    if parent == registry.home.join("fns") || matches!(resolved.scope, Scope::Project) {
        return Some(resolved.name.clone());
    }
    for (index, dir) in registry.fn_dirs.iter().enumerate() {
        if &parent == dir {
            return Some(format!("{FN_DIRS_PREFIX}/{index}/{}", resolved.name));
        }
    }
    None
}

/// What a resolved fn dispatches to. `Python` carries the live dir for
/// unpinned contexts and the pinned bundle when the run pinned one.
#[derive(Debug, Clone)]
pub enum FnDispatch {
    Builtin(&'static BuiltinDescriptor),
    Python {
        name: String,
        dir: PathBuf,
        bundle: Option<PinnedBundle>,
    },
}
impl FnDispatch {
    /// The directory the fn's code is read from: the pinned bundle when
    /// present, else the live scope dir.
    pub fn fn_dir(&self) -> Option<PathBuf> {
        match self {
            Self::Builtin(_) => None,
            Self::Python { dir, bundle, .. } => Some(
                bundle
                    .as_ref()
                    .map(|b| b.fn_dir.clone())
                    .unwrap_or_else(|| dir.clone()),
            ),
        }
    }
}

/// A run's view of `resolved` given the generation it pinned.
pub fn dispatch(registry: &FnRegistry, resolved: &Fn, pinned: Option<ArtifactJob>) -> FnDispatch {
    if let Some(d) = resolved.builtin {
        return FnDispatch::Builtin(d);
    }
    let dir = resolved.dir.clone().unwrap_or_default();
    let bundle = pinned.and_then(|job| {
        let relative = bundle_path(registry, resolved)?;
        Some(PinnedBundle {
            job_id: job.job_id,
            generation: job.generation,
            dir: registry.home.join(&job.path),
            fn_dir: registry.home.join(&job.path).join(&relative),
            manifest: job.manifest,
        })
    });
    FnDispatch::Python {
        name: resolved.name.clone(),
        dir,
        bundle,
    }
}

impl FnRegistry {
    /// Drop every cached scan so the next `registry` call re-reads the scopes;
    /// `save` uses it after writing, watchers do not need it (fingerprints
    /// already cover them).
    fn invalidate(&self) {
        self.scans.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.version.fetch_add(1, Ordering::AcqRel);
    }
}
