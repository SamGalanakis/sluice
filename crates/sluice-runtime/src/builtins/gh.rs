//! gh.* builtins: pr, pr_wait, run_cancel and run_latest, ported from packs/git. Each is
//! a bounded argv-only `gh` invocation with JSON output decoded strictly; the plumbing
//! (BuiltinCtx, FnFailure, sh, ref validation) lives in builtins::git until P4.02 unifies
//! the builtin contract. Only a failed `gh pr view` inside pr_wait is Transient — the one
//! call the Python declares worth a retry; every other failure is Terminal.

use super::git::{
    BuiltinCtx, BuiltinDescriptor, FnFailure, arg, decode, num, opt, output, ports, py_str,
    py_type, ref_, req, sh, sh_error, truthy, ty,
};
use serde_json::{Value, json};
use sluice_model::{rpc::JsonMap, types::Type};
use std::{ffi::OsString, path::Path, time::Duration};
use tokio::time::Instant;

const PR_FIELDS: &str = "state,mergeable,headRefOid,url,statusCheckRollup";
/// CheckRun conclusions that mark a check failed (gh.pr_wait).
const FAILED_CONCLUSIONS: [&str; 5] = [
    "FAILURE",
    "CANCELLED",
    "TIMED_OUT",
    "ACTION_REQUIRED",
    "STARTUP_FAILURE",
];
/// StatusContext states that mark a context failed (gh.pr_wait).
const FAILED_STATES: [&str; 2] = ["FAILURE", "ERROR"];
/// `gh run` conclusions worth listing under failed_jobs (gh.run_latest).
const FAILED_JOBS: [&str; 3] = ["failure", "cancelled", "timed_out"];

/// Descriptors equal to packs/git/gh.*/fn.json, in the same field order. pr_wait declares
/// `run(main, retries=3)`; the others run `run(main)`: no retries, 30s backoff.
pub fn descriptors() -> [BuiltinDescriptor; 4] {
    let d = |name, doc, inputs, outputs, retries| BuiltinDescriptor {
        name,
        doc,
        inputs,
        outputs,
        retries,
        backoff: Duration::from_secs(30),
    };
    [
        d(
            "gh.pr",
            "Create a GitHub PR for head into base, or update the existing open PR.",
            ports(&[
                ("path", "string"),
                ("base", "string"),
                ("head", "string"),
                ("title", "string"),
                ("body", "string"),
                ("draft", "boolean?"),
            ]),
            ports(&[("number", "int"), ("url", "string")]),
            0,
        ),
        d(
            "gh.pr_wait",
            "Poll a GitHub PR until its checks settle, it merges/closes/conflicts, or a \
timeout passes.",
            vec![
                ("path", Type::String),
                ("pr", Type::String),
                ("until", Type::Enum(vec!["checks".into(), "merged".into()])),
                ("interval", ty("int?")),
                ("timeout", ty("int?")),
            ],
            vec![
                (
                    "state",
                    Type::Enum(
                        ["green", "red", "conflicting", "merged", "closed", "timeout"]
                            .map(str::to_string)
                            .into(),
                    ),
                ),
                ("sha", Type::String),
                ("url", Type::String),
                ("failed", ty("string[]")),
            ],
            3,
        ),
        d(
            "gh.run_cancel",
            "Cancel a GitHub Actions run; a run that already completed is not an error.",
            ports(&[("path", "string"), ("run_id", "int")]),
            ports(&[("cancelled", "boolean")]),
            0,
        ),
        d(
            "gh.run_latest",
            "The latest GitHub Actions run on a branch (optionally of one workflow), with \
its failed jobs.",
            ports(&[
                ("path", "string"),
                ("branch", "string?"),
                ("workflow", "string?"),
            ]),
            ports(&[
                ("run_id", "int"),
                ("sha", "string"),
                ("status", "string"),
                ("conclusion", "string?"),
                ("url", "string"),
                ("workflow", "string"),
                ("failed_jobs", "string[]"),
            ]),
            0,
        ),
    ]
}

pub async fn dispatch(
    name: &str,
    inputs: &JsonMap,
    ctx: &BuiltinCtx,
) -> Result<JsonMap, FnFailure> {
    match name {
        "gh.pr" => pr(inputs, ctx).await,
        "gh.pr_wait" => pr_wait(inputs, ctx).await,
        "gh.run_cancel" => run_cancel(inputs, ctx).await,
        "gh.run_latest" => run_latest(inputs, ctx).await,
        _ => Err(FnFailure::terminal(format!("unknown gh builtin: {name}"))),
    }
}

fn gh(rest: &[&str]) -> Vec<OsString> {
    let mut argv = vec![OsString::from("gh")];
    argv.extend(rest.iter().map(OsString::from));
    argv
}

/// gh.pr: create or update the open PR for a head branch via the gh CLI.
pub async fn pr(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<JsonMap, FnFailure> {
    let path = arg(inputs, "path")?;
    let head = ref_("head", req(inputs, "head")?)?;
    let base = ref_("base", req(inputs, "base")?)?;
    let title = arg(inputs, "title")?;
    let body = arg(inputs, "body")?;
    let listed = sh(
        ctx,
        &gh(&["pr", "list", "--head", &head, "--json", "number,url"]),
        Some(Path::new(&path)),
        true,
    )
    .await?
    .stdout;
    let prs = decode(&listed)?;
    let existing = match &prs {
        Value::Array(items) if !items.is_empty() => Some(&items[0]),
        other if truthy(other) => {
            // `prs[0]` on a non-list: Python dies the interpreter's way.
            return Err(FnFailure::terminal(format!(
                "{} indices must be integers or slices",
                py_type(other)
            )));
        }
        _ => None,
    };
    if let Some(first) = existing {
        let number = py_str(required(first, "number")?)?;
        sh(
            ctx,
            &gh(&["pr", "edit", &number, "--title", &title, "--body", &body]),
            Some(Path::new(&path)),
            true,
        )
        .await?;
    } else {
        let mut argv = gh(&[
            "pr", "create", "--base", &base, "--head", &head, "--title", &title, "--body", &body,
        ]);
        if opt(inputs, "draft").is_some_and(truthy) {
            argv.push("--draft".into());
        }
        sh(ctx, &argv, Some(Path::new(&path)), true).await?;
    }
    let data = decode(
        &sh(
            ctx,
            &gh(&["pr", "view", &head, "--json", "number,url"]),
            Some(Path::new(&path)),
            true,
        )
        .await?
        .stdout,
    )?;
    output([
        ("number", required(&data, "number")?.clone()),
        ("url", required(&data, "url")?.clone()),
    ])
}

/// gh.pr_wait: poll a PR until checks settle, it merges/closes/conflicts, or timeout.
pub async fn pr_wait(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<JsonMap, FnFailure> {
    let until = req(inputs, "until")?.as_str();
    let interval = opt_num(inputs, "interval", 60.0)?;
    let timeout = opt_num(inputs, "timeout", 21600.0)?;
    let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
    let mut sha = String::new();
    let mut url = String::new();
    let (state, failed) = loop {
        let data = pr_data(inputs, ctx).await?;
        if let Some(v) = data.get("headRefOid").filter(|v| truthy(v)) {
            sha = py_str(v)?;
        }
        if let Some(v) = data.get("url").filter(|v| truthy(v)) {
            url = py_str(v)?;
        }
        match data.get("state").and_then(Value::as_str) {
            Some("MERGED") => break ("merged", Vec::new()),
            Some("CLOSED") => break ("closed", Vec::new()),
            _ => {}
        }
        if data.get("mergeable").and_then(Value::as_str) == Some("CONFLICTING") {
            break ("conflicting", Vec::new());
        }
        let (pending, failed) = checks(data.get("statusCheckRollup"));
        if !failed.is_empty() && !pending {
            break ("red", failed);
        }
        if until == Some("checks") && !pending {
            break ("green", Vec::new());
        }
        let now = Instant::now();
        if now >= deadline {
            break ("timeout", failed);
        }
        let nap = interval.min((deadline - now).as_secs_f64());
        if nap < 0.0 {
            return Err(FnFailure::terminal("sleep length must be non-negative"));
        }
        tokio::time::sleep(Duration::from_secs_f64(nap)).await;
    };
    output([
        ("state", json!(state)),
        ("sha", json!(sha)),
        ("url", json!(url)),
        ("failed", json!(failed)),
    ])
}

/// One `gh pr view` poll; a failing call is worth a retry.
async fn pr_data(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<Value, FnFailure> {
    let path = arg(inputs, "path")?;
    let pr_ref = ref_("pr", req(inputs, "pr")?)?;
    let polled = sh(
        ctx,
        &gh(&["pr", "view", &pr_ref, "--json", PR_FIELDS]),
        Some(Path::new(&path)),
        true,
    )
    .await
    .map_err(|e| FnFailure::Transient(format!("gh pr view failed: {e}")))?;
    decode(&polled.stdout)
}

/// statusCheckRollup -> (any still pending, names of the failed ones).
fn checks(rollup: Option<&Value>) -> (bool, Vec<Value>) {
    let (mut pending, mut failed) = (false, Vec::new());
    let Some(rollup) = rollup.filter(|v| truthy(v)) else {
        return (pending, failed);
    };
    let Some(items) = rollup.as_array() else {
        return (pending, failed);
    };
    for c in items {
        let Some(c) = c.as_object() else { continue };
        if c.contains_key("status") {
            // a CheckRun
            if c["status"].as_str() != Some("COMPLETED") {
                pending = true;
            } else if c
                .get("conclusion")
                .and_then(Value::as_str)
                .is_some_and(|s| FAILED_CONCLUSIONS.contains(&s))
            {
                failed.push(
                    c.get("name")
                        .filter(|v| truthy(v))
                        .or_else(|| c.get("workflowName").filter(|v| truthy(v)))
                        .cloned()
                        .unwrap_or_else(|| json!("check")),
                );
            }
        } else if c.contains_key("state") {
            // a StatusContext
            let state = c["state"].as_str();
            if state.is_some_and(|s| FAILED_STATES.contains(&s)) {
                failed.push(
                    c.get("context")
                        .filter(|v| truthy(v))
                        .cloned()
                        .unwrap_or_else(|| json!("context")),
                );
            } else if state != Some("SUCCESS") {
                pending = true;
            }
        }
    }
    (pending, failed)
}

/// gh.run_cancel: cancel a workflow run; already-completed reports cancelled=false.
pub async fn run_cancel(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<JsonMap, FnFailure> {
    let run_id = ref_("run_id", req(inputs, "run_id")?)?;
    let path = arg(inputs, "path")?;
    let argv = gh(&["run", "cancel", &run_id]);
    let p = sh(ctx, &argv, Some(Path::new(&path)), false).await?;
    if p.code != 0
        && !format!("{}{}", p.stderr, p.stdout)
            .to_lowercase()
            .contains("complet")
    {
        return Err(sh_error(&argv, &p));
    }
    output([("cancelled", json!(p.code == 0))])
}

/// gh.run_latest: the latest GitHub Actions run on a branch, with its failed jobs.
pub async fn run_latest(inputs: &JsonMap, ctx: &BuiltinCtx) -> Result<JsonMap, FnFailure> {
    let path = arg(inputs, "path")?;
    let branch = match opt(inputs, "branch").filter(|v| truthy(v)) {
        Some(value) => ref_("branch", value)?,
        None => "main".to_string(),
    };
    let mut argv = gh(&["run", "list", "--branch", &branch]);
    if let Some(workflow) = opt(inputs, "workflow").filter(|v| truthy(v)) {
        argv.push("--workflow".into());
        argv.push(py_str(workflow)?.into());
    }
    argv.extend(
        [
            "--limit",
            "1",
            "--json",
            "databaseId,headSha,status,conclusion,url,workflowName",
        ]
        .map(OsString::from),
    );
    let runs = decode(&sh(ctx, &argv, Some(Path::new(&path)), true).await?.stdout)?;
    let Value::Array(items) = &runs else {
        return Err(FnFailure::terminal("run list did not return a list"));
    };
    if items.is_empty() {
        return Err(FnFailure::terminal(format!("no runs on {branch}")));
    }
    let r = &items[0];
    let mut failed_jobs = Vec::new();
    if r.get("conclusion")
        .and_then(Value::as_str)
        .is_some_and(|c| FAILED_JOBS.contains(&c))
    {
        let run_id = py_str(required(r, "databaseId")?)?;
        let jobs = decode(
            &sh(
                ctx,
                &gh(&["run", "view", &run_id, "--json", "jobs"]),
                Some(Path::new(&path)),
                true,
            )
            .await?
            .stdout,
        )?;
        match jobs.get("jobs") {
            None => {}
            Some(Value::Array(list)) => {
                for j in list {
                    if j.get("conclusion")
                        .and_then(Value::as_str)
                        .is_some_and(|c| FAILED_JOBS.contains(&c))
                    {
                        failed_jobs.push(required(j, "name")?.clone());
                    }
                }
            }
            Some(other) => {
                return Err(FnFailure::terminal(format!(
                    "'{}' object is not iterable",
                    py_type(other)
                )));
            }
        }
    }
    output([
        ("run_id", required(r, "databaseId")?.clone()),
        ("sha", required(r, "headSha")?.clone()),
        ("status", required(r, "status")?.clone()),
        (
            "conclusion",
            r.get("conclusion").cloned().unwrap_or(Value::Null),
        ),
        ("url", required(r, "url")?.clone()),
        ("workflow", required(r, "workflowName")?.clone()),
        ("failed_jobs", json!(failed_jobs)),
    ])
}

fn required<'a>(value: &'a Value, name: &str) -> Result<&'a Value, FnFailure> {
    value
        .get(name)
        .ok_or_else(|| FnFailure::terminal(format!("'{name}'")))
}
fn opt_num(inputs: &JsonMap, name: &str, default: f64) -> Result<f64, FnFailure> {
    match opt(inputs, name) {
        Some(v) if !v.is_null() => {
            num(v).ok_or_else(|| FnFailure::terminal(format!("{name} must be a number")))
        }
        _ => Ok(default),
    }
}
