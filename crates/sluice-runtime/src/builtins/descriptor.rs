//! The shared builtin contract: every compiled fn's descriptor, equal to its
//! fn.json in name, doc, ordered typed ports and icon, plus the same-run retry
//! budget the Python pack declared through `run(main, retries=N, backoff=S)`.
//!
//! `catalog()` lists all 31 builtins. jev's four live in `builtins::jev` with
//! their implementation; the rest are declared here until their units land the
//! dispatch bodies (agents in P5, git/gh in P4.04, core/messages in P4.03).
//! Until then [`dispatch`](crate::builtins::dispatch) returns
//! `FnFailure::NotBuilt` for them.

use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use sluice_model::types::Type;
use std::{fmt, sync::LazyLock, time::Duration};

/// The helper's default same-run retry policy: `run(main)` retries nothing and
/// `backoff` defaults to 30 seconds (SPEC §7, rust-port.md §6.1).
pub const DEFAULT_RETRY: RetryBudget = RetryBudget {
    retries: 0,
    backoff: Duration::from_secs(30),
};

/// A builtin's same-run retry budget: how many extra calls a `Transient` buys
/// and the fixed wait between them, inside one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryBudget {
    pub retries: u32,
    pub backoff: Duration,
}

/// A submitted output of an open fn's step (fn.json `submits` entries: a bare
/// type or `{"type", "doc"}`).
#[derive(Debug, Clone, PartialEq)]
pub struct SubmitDecl {
    pub ty: Type,
    pub doc: Option<&'static str>,
}

/// A compiled icon. Builtin icons are single-colour 16x16 SVGs in the status
/// glyphs' style (SPEC §10), embedded from `assets/icons/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinIcon {
    pub media_type: &'static str,
    pub bytes: &'static [u8],
}

/// A compiled fn's manifest, equal to its fn.json (name, doc, typed inputs and
/// outputs, open, submits, icon) plus its same-run retry budget.
#[derive(Debug, Clone)]
pub struct BuiltinDescriptor {
    pub name: &'static str,
    pub doc: &'static str,
    pub inputs: Vec<(&'static str, Type)>,
    pub outputs: Vec<(&'static str, Type)>,
    pub open: bool,
    pub submits: Vec<(&'static str, SubmitDecl)>,
    pub icon: Option<BuiltinIcon>,
    pub retry: RetryBudget,
}
impl BuiltinDescriptor {
    /// The descriptor as a fn.json document (`raw`), the shape `fn_get` returns.
    pub fn manifest(&self) -> Value {
        let ports = |ports: &[(&'static str, Type)]| {
            ports
                .iter()
                .map(|(name, ty)| (name.to_string(), ty.form()))
                .collect::<Map<_, _>>()
        };
        let mut raw = Map::new();
        raw.insert("name".into(), json!(self.name));
        raw.insert("doc".into(), json!(self.doc));
        if self.open {
            raw.insert("open".into(), json!(true));
        }
        if !self.submits.is_empty() {
            raw.insert(
                "submits".into(),
                Value::Object(
                    self.submits
                        .iter()
                        .map(|(name, decl)| {
                            (
                                name.to_string(),
                                match decl.doc {
                                    Some(doc) => {
                                        json!({"type": decl.ty.form(), "doc": doc})
                                    }
                                    None => decl.ty.form(),
                                },
                            )
                        })
                        .collect(),
                ),
            );
        }
        raw.insert("inputs".into(), Value::Object(ports(&self.inputs)));
        raw.insert("outputs".into(), Value::Object(ports(&self.outputs)));
        Value::Object(raw)
    }
}

/// What a builtin sees of its run: the environment holding secrets and
/// provider configuration.
#[derive(Debug, Default, Clone)]
pub struct BuiltinCtx {
    pub env: IndexMap<String, String>,
}
impl BuiltinCtx {
    pub fn new(env: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            env: env.into_iter().collect(),
        }
    }
    /// One variable's value, for secrets and provider configuration.
    pub fn env(&self, name: &str) -> Option<&str> {
        self.env.get(name).map(String::as_str)
    }
}

/// How a fn run ended. Transient is retryable within the run under the
/// descriptor's retry budget (the Python helper's `Transient`); Terminal fails
/// the step; NotBuilt names a descriptor whose dispatch lands in a later unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FnFailure {
    Transient(String),
    Terminal(String),
    NotBuilt(String),
}
impl FnFailure {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
    pub fn terminal(message: impl Into<String>) -> Self {
        Self::Terminal(message.into())
    }
    pub fn not_built(name: impl Into<String>) -> Self {
        Self::NotBuilt(name.into())
    }
}
impl fmt::Display for FnFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transient(m) | Self::Terminal(m) => f.write_str(m),
            Self::NotBuilt(name) => {
                write!(
                    f,
                    "builtin {name} is registered but not built in this build"
                )
            }
        }
    }
}
impl std::error::Error for FnFailure {}

/// The fn.json type grammar's string shorthand: names and the `?`/`[]` suffixes.
fn ty(form: &str) -> Type {
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
fn ports(forms: &[(&'static str, &'static str)]) -> Vec<(&'static str, Type)> {
    forms.iter().map(|(name, form)| (*name, ty(form))).collect()
}
fn optional(inner: Type) -> Type {
    Type::Optional(Box::new(inner))
}
fn list(inner: Type) -> Type {
    Type::List(Box::new(inner))
}
fn enumerated(symbols: &[&str]) -> Type {
    Type::Enum(symbols.iter().map(|s| s.to_string()).collect())
}
fn record(fields: &[(&'static str, &'static str)]) -> Type {
    Type::Record(
        fields
            .iter()
            .map(|(name, form)| (name.to_string(), ty(form)))
            .collect::<IndexMap<_, _>>(),
    )
}
/// The optional `git` record every agent fn returns inside a git worktree.
fn git_facts() -> Type {
    optional(record(&[
        ("head_before", "string"),
        ("head_after", "string"),
        ("commits", "int"),
        ("dirty", "boolean"),
    ]))
}

const SVG: &str = "image/svg+xml";
const SPARK: BuiltinIcon = BuiltinIcon {
    media_type: SVG,
    bytes: include_bytes!("../../assets/icons/spark.svg"),
};
const REVIEW: BuiltinIcon = BuiltinIcon {
    media_type: SVG,
    bytes: include_bytes!("../../assets/icons/review.svg"),
};
const FORK: BuiltinIcon = BuiltinIcon {
    media_type: SVG,
    bytes: include_bytes!("../../assets/icons/fork.svg"),
};
const BRANCH: BuiltinIcon = BuiltinIcon {
    media_type: SVG,
    bytes: include_bytes!("../../assets/icons/branch.svg"),
};
const PR: BuiltinIcon = BuiltinIcon {
    media_type: SVG,
    bytes: include_bytes!("../../assets/icons/pr.svg"),
};
const EXTERNAL: BuiltinIcon = BuiltinIcon {
    media_type: SVG,
    bytes: include_bytes!("../../assets/icons/external.svg"),
};

const AGENT_RETRY: RetryBudget = RetryBudget {
    retries: 3,
    backoff: Duration::from_secs(600),
};

/// Every compiled builtin, in the catalog order SPEC §10 presents them: the
/// four core fns, the two inline fns, the five message fns, six agent fns, six
/// git fns, four gh fns and the four jev fns.
pub fn catalog() -> &'static [BuiltinDescriptor] {
    static CATALOG: LazyLock<Vec<BuiltinDescriptor>> = LazyLock::new(|| {
        let mut all = vec![
            // ---- core: four fns, three inline and one never executed ----
            BuiltinDescriptor {
                name: "core.echo",
                doc: "Pass the value through. Built in: runs inline, no process.",
                inputs: ports(&[("value", "Any")]),
                outputs: ports(&[("value", "Any")]),
                open: false,
                submits: vec![],
                icon: None,
                retry: DEFAULT_RETRY,
            },
            BuiltinDescriptor {
                name: "core.collect",
                doc: "The fan-in join: gather values into one array. Built in: runs \
inline, no process.",
                inputs: ports(&[("items", "Any[]")]),
                outputs: ports(&[("items", "Any[]")]),
                open: false,
                submits: vec![],
                icon: None,
                retry: DEFAULT_RETRY,
            },
            BuiltinDescriptor {
                name: "core.format",
                doc: "Fill a template: {name} from a record, {0}, {1}... from an array ({0} from a \
single value); {{ and }} are literal braces. Non-strings are rendered as JSON. Attribute or \
item access, conversions and format specifications are refused. Built in: runs inline, no \
process.",
                inputs: ports(&[("template", "string"), ("values", "Any")]),
                outputs: ports(&[("text", "string")]),
                open: false,
                submits: vec![],
                icon: None,
                retry: DEFAULT_RETRY,
            },
            BuiltinDescriptor {
                name: "core.external",
                doc: "Work done outside sluice. The runner never starts it: once ready it \
waits until its outputs are set by hand with step_set_output, or it is cancelled. \
Declare the outputs it will get; bind extra inputs to order it after them.",
                inputs: vec![],
                outputs: vec![],
                open: true,
                submits: vec![],
                icon: Some(EXTERNAL),
                retry: DEFAULT_RETRY,
            },
            // ---- inline ----
            BuiltinDescriptor {
                name: "inline.bash",
                doc: "Run bash code given as a string (`code`, the same name as for \
inline.python), with errexit and pipefail. Each extra input the step binds is an \
environment variable of its name (- becomes _; strings as they are, anything else \
as JSON). A step that declares outputs gets them from a JSON object the script \
writes to the file $OUT. Fails on a non-zero exit unless check is false.",
                inputs: ports(&[
                    ("code", "string"),
                    ("cwd", "string?"),
                    ("check", "boolean?"),
                ]),
                outputs: ports(&[("stdout", "string"), ("stderr", "string"), ("code", "int")]),
                open: true,
                submits: vec![],
                icon: None,
                retry: DEFAULT_RETRY,
            },
            BuiltinDescriptor {
                name: "inline.python",
                doc: "Run Python code given as a string (standard library only). The code \
sees `inp` (every input) and each extra input the step binds as a variable of its \
name (- becomes _); it may set `out`. `value` is `out`; a step that declares outputs \
gets them from `out`, a dict. `stdout` is what it printed.",
                inputs: ports(&[("code", "string"), ("cwd", "string?")]),
                outputs: ports(&[("value", "Any?"), ("stdout", "string")]),
                open: true,
                submits: vec![],
                icon: None,
                retry: DEFAULT_RETRY,
            },
        ];
        // ---- messages: the ask, say and reply verbs, wait, and the retired post ----
        all.extend(crate::builtins::messages::descriptors());
        all.extend([
            // ---- agents ----
            BuiltinDescriptor {
                name: "agent.claude",
                doc: "Run a supervised interactive Claude session on a prompt in a \
working directory. Submit declared step outputs before the session ends; pass \
session to resume in the same directory. Runs Opus: model is a JSON object, by \
default {\"type\":\"normal\",\"model\":\"opus\",\"effort\":\"high\"}: \
{\"type\":\"normal\",\"model\":M,\"effort\":E?} with M opus or claude-opus-5-5 and E \
low, medium, high, xhigh or max. The result's model names what ran (docs(\"plans\"), \
Choosing a model).",
                inputs: ports(&[
                    ("cwd", "string"),
                    ("prompt", "string"),
                    ("model", "Any?"),
                    ("session", "string?"),
                    ("listen", "boolean?"),
                ]),
                outputs: vec![
                    ("result", ty("string")),
                    ("model", ty("string")),
                    ("session", ty("string")),
                    ("git", git_facts()),
                ],
                open: true,
                submits: vec![],
                icon: Some(SPARK),
                retry: AGENT_RETRY,
            },
            BuiltinDescriptor {
                name: "agent.codex",
                doc: "Run a supervised interactive Codex session on a spec in a working \
directory. Submit declared step outputs before the session ends; pass session to \
resume in the same directory. model is a JSON object, by default \
{\"type\":\"normal\",\"model\":\"sol\",\"effort\":\"high\"}: \
{\"type\":\"normal\",\"model\":M,\"effort\":E?}, a model `codex debug models` lists \
(sol and astra name gpt-6.1-sol and gpt-6-astra) at an effort it supports. The result's \
model names what ran (docs(\"plans\"), Choosing a model). log is the path to the \
readable run record, copied to the log input when given, or null if no log exists.",
                inputs: vec![
                    ("cwd", ty("string")),
                    ("spec", ty("string")),
                    ("model", ty("Any?")),
                    ("log", ty("string?")),
                    ("session", ty("string?")),
                    ("report_path", ty("string?")),
                    ("listen", ty("boolean?")),
                ],
                outputs: vec![
                    ("log", ty("string?")),
                    ("final", ty("string")),
                    ("model", ty("string")),
                    ("report", ty("string?")),
                    ("session", ty("string")),
                    ("git", git_facts()),
                ],
                open: true,
                submits: vec![],
                icon: Some(SPARK),
                retry: AGENT_RETRY,
            },
            BuiltinDescriptor {
                name: "agent.devin",
                doc: "Run a supervised interactive Devin session on a spec in a working \
directory. Submit declared step outputs before the session ends; pass session to \
resume in the same directory. model is a JSON object, by default \
{\"type\":\"normal\",\"model\":\"swe-2\",\"effort\":\"high\"}: normal \
{\"type\":\"normal\",\"model\":M,\"effort\":E?,\"fast\":bool?} runs M[-E][-fast]; \
fusion {\"type\":\"fusion\",\"main\":{\"model\":M,\"effort\":E?,\"fast\":bool?},\
\"sidekick\":{\"model\":S,\"effort\":F?,\"priority\":bool?}} runs \
fusion-M[-E][-fast]-sidekick-S[-F][-priority]. The id must be one `devin models list` \
shows. The result's model names what ran (docs(\"plans\"), Choosing a model). log is the path to the \
readable run record, copied to the log input when given, or null if no log exists.",
                inputs: ports(&[
                    ("cwd", "string"),
                    ("spec", "string"),
                    ("model", "Any?"),
                    ("log", "string?"),
                    ("session", "string?"),
                    ("report_path", "string?"),
                    ("listen", "boolean?"),
                ]),
                outputs: vec![
                    ("log", ty("string?")),
                    ("final", ty("string")),
                    ("model", ty("string")),
                    ("report", ty("string?")),
                    ("session", ty("string")),
                    ("git", git_facts()),
                ],
                open: true,
                submits: vec![],
                icon: Some(SPARK),
                retry: AGENT_RETRY,
            },
            BuiltinDescriptor {
                name: "agent.review",
                doc: "Review and fix a branch diff against project standards in a \
supervised interactive Claude session. Pass session to resume in the same \
directory.",
                inputs: ports(&[
                    ("cwd", "string"),
                    ("base", "string"),
                    ("standards", "string"),
                    ("notes", "string?"),
                    ("session", "string?"),
                    ("listen", "boolean?"),
                ]),
                outputs: vec![
                    ("summary", ty("string")),
                    ("sha", ty("string")),
                    ("commits", ty("int")),
                    ("session", ty("string")),
                    ("git", git_facts()),
                ],
                open: true,
                submits: vec![],
                icon: Some(REVIEW),
                retry: AGENT_RETRY,
            },
            BuiltinDescriptor {
                name: "agent.run",
                doc: "Run a spec in a supervised interactive session on the named \
engine. Submit declared step outputs before the session ends; pass session to \
resume in the same directory. model is a JSON object: \
{\"type\":\"normal\",\"model\":M,\"effort\":E?,\"fast\":bool?} (defaults: devin swe-2 \
high, codex sol high, claude opus high; fast only on devin), or on devin \
{\"type\":\"fusion\",\"main\":{\"model\":M,\"effort\":E?,\"fast\":bool?},\
\"sidekick\":{\"model\":S,\"effort\":F?,\"priority\":bool?}}. It is checked against \
the engine's models before the session starts; the result's model names what ran \
(docs(\"plans\"), Choosing a model).",
                inputs: vec![
                    ("engine", enumerated(&["devin", "codex", "claude"])),
                    ("cwd", ty("string")),
                    ("spec", ty("string")),
                    ("model", ty("Any?")),
                    ("session", ty("string?")),
                    ("report_path", ty("string?")),
                    ("listen", ty("boolean?")),
                ],
                outputs: vec![
                    ("final", ty("string")),
                    ("model", ty("string")),
                    ("report", ty("string?")),
                    ("session", ty("string")),
                    ("git", git_facts()),
                ],
                open: true,
                submits: vec![],
                icon: Some(SPARK),
                retry: AGENT_RETRY,
            },
            BuiltinDescriptor {
                name: "decide.llm",
                doc: "Pick one of the given options by asking a small claude model.",
                inputs: ports(&[
                    ("question", "string"),
                    ("context", "Any?"),
                    ("options", "string[]"),
                    ("threshold", "float?"),
                ]),
                outputs: ports(&[
                    ("choice", "string"),
                    ("p", "float"),
                    ("confident", "boolean"),
                ]),
                open: false,
                submits: vec![],
                icon: Some(FORK),
                retry: RetryBudget {
                    retries: 2,
                    backoff: Duration::from_secs(30),
                },
            },
            // ---- git ----
            BuiltinDescriptor {
                name: "git.head",
                doc: "Report the current branch and commit of a worktree.",
                inputs: ports(&[("path", "string")]),
                outputs: ports(&[("branch", "string"), ("sha", "string")]),
                open: false,
                submits: vec![],
                icon: Some(BRANCH),
                retry: DEFAULT_RETRY,
            },
            BuiltinDescriptor {
                name: "git.merge",
                doc: "Merge source into target in a temporary worktree; conflicts are \
data, not failures.",
                inputs: ports(&[
                    ("repo", "string"),
                    ("source", "string"),
                    ("target", "string"),
                    ("message", "string?"),
                    ("push", "boolean?"),
                ]),
                outputs: ports(&[
                    ("merged", "boolean"),
                    ("sha", "string?"),
                    ("conflicts", "string[]"),
                ]),
                open: false,
                submits: vec![],
                icon: Some(BRANCH),
                retry: DEFAULT_RETRY,
            },
            BuiltinDescriptor {
                name: "git.push",
                doc: "Push HEAD to a branch on a remote, optionally with \
--force-with-lease.",
                inputs: ports(&[
                    ("path", "string"),
                    ("branch", "string"),
                    ("remote", "string?"),
                    ("force_with_lease", "boolean?"),
                ]),
                outputs: ports(&[("sha", "string")]),
                open: false,
                submits: vec![],
                icon: Some(BRANCH),
                retry: DEFAULT_RETRY,
            },
            BuiltinDescriptor {
                name: "git.rebase",
                doc: "Rebase a worktree onto a ref; conflicts abort and are returned as \
data.",
                inputs: ports(&[("path", "string"), ("onto", "string")]),
                outputs: ports(&[
                    ("ok", "boolean"),
                    ("sha", "string"),
                    ("conflicts", "string[]"),
                ]),
                open: false,
                submits: vec![],
                icon: Some(BRANCH),
                retry: DEFAULT_RETRY,
            },
            BuiltinDescriptor {
                name: "git.worktree",
                doc: "Add a git worktree on a new or existing branch.",
                inputs: ports(&[
                    ("repo", "string"),
                    ("base", "string"),
                    ("branch", "string"),
                    ("path", "string?"),
                ]),
                outputs: ports(&[("path", "string"), ("branch", "string"), ("sha", "string")]),
                open: false,
                submits: vec![],
                icon: Some(BRANCH),
                retry: DEFAULT_RETRY,
            },
            BuiltinDescriptor {
                name: "git.worktree_rm",
                doc: "Remove a git worktree; reports whether the path was a worktree.",
                inputs: ports(&[
                    ("repo", "string"),
                    ("path", "string"),
                    ("force", "boolean?"),
                ]),
                outputs: ports(&[("removed", "boolean")]),
                open: false,
                submits: vec![],
                icon: Some(BRANCH),
                retry: DEFAULT_RETRY,
            },
            // ---- gh ----
            BuiltinDescriptor {
                name: "gh.pr",
                doc: "Create a GitHub PR for head into base, or update the existing \
open PR.",
                inputs: ports(&[
                    ("path", "string"),
                    ("base", "string"),
                    ("head", "string"),
                    ("title", "string"),
                    ("body", "string"),
                    ("draft", "boolean?"),
                ]),
                outputs: ports(&[("number", "int"), ("url", "string")]),
                open: false,
                submits: vec![],
                icon: Some(PR),
                retry: DEFAULT_RETRY,
            },
            BuiltinDescriptor {
                name: "gh.pr_wait",
                doc: "Poll a GitHub PR until its checks settle, it \
merges/closes/conflicts, or a timeout passes.",
                inputs: vec![
                    ("path", ty("string")),
                    ("pr", ty("string")),
                    ("until", enumerated(&["checks", "merged"])),
                    ("interval", ty("int?")),
                    ("timeout", ty("int?")),
                ],
                outputs: vec![
                    (
                        "state",
                        enumerated(&["green", "red", "conflicting", "merged", "closed", "timeout"]),
                    ),
                    ("sha", ty("string")),
                    ("url", ty("string")),
                    ("failed", list(ty("string"))),
                ],
                open: false,
                submits: vec![],
                icon: Some(PR),
                retry: RetryBudget {
                    retries: 3,
                    backoff: Duration::from_secs(30),
                },
            },
            BuiltinDescriptor {
                name: "gh.run_cancel",
                doc: "Cancel a GitHub Actions run; a run that already completed is not \
an error.",
                inputs: ports(&[("path", "string"), ("run_id", "int")]),
                outputs: ports(&[("cancelled", "boolean")]),
                open: false,
                submits: vec![],
                icon: Some(PR),
                retry: DEFAULT_RETRY,
            },
            BuiltinDescriptor {
                name: "gh.run_latest",
                doc: "The latest GitHub Actions run on a branch (optionally of one \
workflow), with its failed jobs.",
                inputs: ports(&[
                    ("path", "string"),
                    ("branch", "string?"),
                    ("workflow", "string?"),
                ]),
                outputs: ports(&[
                    ("run_id", "int"),
                    ("sha", "string"),
                    ("status", "string"),
                    ("conclusion", "string?"),
                    ("url", "string"),
                    ("workflow", "string"),
                    ("failed_jobs", "string[]"),
                ]),
                open: false,
                submits: vec![],
                icon: Some(PR),
                retry: DEFAULT_RETRY,
            },
        ]);
        all.extend(crate::builtins::jev::descriptors());
        all
    });
    &CATALOG
}

/// One catalog entry by name, for dispatch and builtin registration.
pub fn find(name: &str) -> Option<&'static BuiltinDescriptor> {
    catalog().iter().find(|d| d.name == name)
}
