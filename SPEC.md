# sluice: specification (v0, minimal)

sluice runs a plan: a graph of typed function calls. Orchestrators (agents or humans) edit the
plan through typed tools; a runner executes it. Deliberately small: features get added when real
use asks for them. The document shapes borrow from CWL (Common Workflow Language) where that
helps: `inputs`/`outputs`/`steps`, `run`, `source`/`default`, `scatter`, CWL type spellings. We do
not aim for CWL compliance. When the code and this file disagree, fix one of them in the same
change.

## 1. Concepts

- **Function (fn):** a reusable unit with a name, an optional `doc`, typed named `inputs`, typed
  named `outputs`, and a Python implementation (`main.py`, run with `uv`).
- **Project:** a name and an optional description, nothing else (no code directory: put whatever
  context matters in the description). Each project has exactly one plan, its own functions and
  its own `.env`. Every call names the project it acts on.
- **Plan:** typed plan `inputs`, named plan `outputs`, and `steps`. Each step runs one fn; each of
  its inputs comes from a plan input, other steps' outputs, or a literal. Edited only through
  typed edits, every edit logged.
- **Log:** each project has one append-only log of what happened (edits, manual values, step
  status changes, calls, thread messages). It is history; `plan.json` and `state.json` are the
  current truth.
- **Runner:** starts a step once everything it reads is available, records its outputs or its
  failure. A failed step shows up in `status`; an orchestrator decides what next.
- **Inbox:** each project's items waiting on a person (a question, optionally with an OpenUI
  form, optionally setting a plan input). A person answers in the dashboard; agents post, wait
  on the log and read the answer (§8a).

## 2. Workspace layout

`SLUICE_HOME` (default `~/.sluice`):

```
config.json                 {"fn_dirs": [], "http": {"host": "127.0.0.1", "port": 7420}, "max_parallel": 8,
                             "log_max": 10000}
runner.lock                 flock held by the one runner of this home (a second one refuses to start)
.env                        global secrets (KEY=value lines)
fns/                        global user functions
log.jsonl                   the home log: fn_call runs without a project (§6b)
runs/<call_id>/             input.json, output.json, stderr.log of those calls
.lock                       flock target for appends to the home log
projects/<name>/
  project.json              {"name", "description"}
  plan.json                 the project's plan (current truth)
  state.json                runner-owned: plan input values, step status and outputs (current truth)
  log.jsonl                 the project's log (§6b): edits, manual values, step status changes,
                            calls, thread messages, inbox changes
  inbox.json                {"items": [...]}: the project's inbox items (§8a), current truth
  fns/                      project-local functions
  .env                      project secrets (override global ones)
  runs/<run_id>/            input.json, output.json, stderr.log for one fn execution (a step run,
                            or a call: then run_id is the call id)
  .lock                     flock target for read-modify-write in this project and log appends
```

**Function scopes:** built-in (shipped in the package, `src/sluice/fns/`), global
(`SLUICE_HOME/fns/` and every dir in `config.fn_dirs`), and project (`projects/<name>/fns/`). A
project sees built-in + global + its own functions. **Names never collide:** a global function may
not reuse a built-in name, and a project function may not reuse a built-in or global name (or
another name in the same scope). A collision is an error of the later scope: the offending
project (or global dir) reports it from `verify` and `fn_list` (an entry with `error`), and
`fn_save` refuses to create one. Lookup still resolves a colliding name to the earlier scope's
function. Two projects may each have a function of the same name. A fn dir is any immediate
subdirectory containing `fn.json` whose `name` matches the directory; anything else (e.g.
`_lib/`) is ignored.

**Only project problems block.** While one of the project's own functions has a problem (a
collision, or a fn.json that fails the §6a checks), that project refuses plan edits, manual values,
`fn_call` and new runs (`invalid`, listing the problems); steps already running finish. Problems
in the global or built-in scope never block anything: the broken or colliding function is left
out of lookup and reported by `verify` and `fn_list`, and a plan step that uses it fails
validation like any unknown function. Reads (`status`, `plan_get`, `plan_history`, views,
`fn_list`, `verify`) always work. Functions are rescanned when a `fn.json` or `main.py` changes,
so a fix (or `fn_save`) needs no restart.

Writes are atomic (`<file>.tmp` then `os.replace`); read-modify-write holds `fcntl.flock`.

## 3. Types

Written inline, compared structurally, no shared registry. CWL spellings:

| Form | Meaning |
|---|---|
| `"string"`, `"int"`, `"float"`, `"boolean"`, `"Any"` | primitives; `Any` accepts anything |
| `"T?"` e.g. `"string?"`, `"string[]?"` | optional: T or null; an optional input may be left unbound |
| `["null", T]` | optional form for any T, e.g. an optional enum |
| `"T[]"` e.g. `"string[]"` | array of T (shorthand) |
| `{"type": "array", "items": T}` | array of T |
| `{"type": "enum", "symbols": ["a", "b"]}` | enum of strings |
| `{"type": "record", "fields": {"f": T, ...}}` | record |

`fits(out, inp)`: `Any` on either side fits; optional into non-optional does not; same primitive,
or `int` into `float`; enum subset, or enum into `string`; arrays covariant; records: every
required field of `inp` exists in `out` and fits, extra fields in `out` are fine.

`check_value(type, value) -> list[str]` validates a runtime value, returning path-bearing errors
(`report.outcome: expected one of [done, blocked], got "ok"`).

## 4. Functions

`<fn_dir>/fn.json` plus `<fn_dir>/main.py`:

```json
{
  "name": "git.head",
  "doc": "Current branch and commit of a working tree.",
  "inputs":  {"path": "string"},
  "outputs": {"branch": "string", "sha": "string"}
}
```

`name` (dotted lowercase), `inputs` and `outputs` are required; `doc` is optional.

**Process contract.** The runner runs `uv run --quiet --script <fn_dir>/main.py` with stdin = an
object keyed by input name (unbound optional inputs are `null`); env `SLUICE_HOME`,
`SLUICE_PROJECT`, `SLUICE_STEP`, `SLUICE_RUN_ID`, `SLUICE_RUN_DIR`, `SLUICE_FN_DIR`, and
`PYTHONPATH` containing sluice's `src` dir, plus every `KEY=value` line of `SLUICE_HOME/.env` and
then the project's `.env` (project values win). Secrets live there, never in plans. cwd = the run
dir. `SLUICE_PROJECT` names the project (empty for a call without one); for a call,
`SLUICE_STEP` is empty and `SLUICE_RUN_ID` is the call id. The fn writes one JSON object keyed
by output name to stdout (logs go to stderr) and exits 0. Any other exit code, or outputs that
fail `check_value`, is a failure. Retries and timeouts, if a fn needs them, happen inside the fn
(`run(main, retries=N)`, §7). Each fn process starts its own session; when the runner stops a
run (runner shutdown, a removed step, a failed sibling scatter run) it sends the whole process
group SIGTERM, then SIGKILL to whatever is left after 5 s, so nothing started under `uv run`
outlives it (SIGTERM first lets an agent CLI stop tool processes it started in sessions of
their own). `sluice serve` and `sluice loop` stop their fns this way on SIGINT, SIGTERM and
SIGHUP (a closed terminal or `tmux kill-session`), then exit 0.

## 5. Plans

```json
{
  "inputs":  {"repo": "string", "tasks": "string[]"},
  "outputs": {"notes": {"source": "notes/final"}},
  "steps": {
    "work":  {"run": "agent.run", "scatter": "spec",
              "in": {"engine": {"default": "devin"}, "cwd": {"source": "repo"},
                     "spec": {"source": "tasks"}}},
    "check": {"run": "agent.run",
              "in": {"engine": {"default": "claude"}, "cwd": {"source": "repo"},
                     "spec": {"default": "Run the test suite and summarise failures"}}},
    "gate":  {"run": "core.collect", "in": {"items": {"source": ["work/final", "check/final"]}}},
    "notes": {"run": "agent.run",
              "in": {"engine": {"default": "claude"}, "cwd": {"source": "repo"},
                     "spec": {"source": "gate/items.0"}}}
  }
}
```

A new project starts with the empty plan `{"inputs": {}, "outputs": {}, "steps": {}}`.

- Project names, step ids, plan input and output names match `^[a-z0-9][a-z0-9_-]*$`. The plan's
  `rev` is store-maintained and returned by `plan_get`/`status`.
- **Docs.** A plan input is declared by its type, or, as in CWL, by `{"type": <type>, "doc":
  "..."}` (both keys only; no type form has just these keys, so the two never clash). A step
  may carry `"doc": "..."` next to `run`, `in` and `scatter`. Docs are optional strings that say
  what a value or a step is for; `status` returns them (`input_docs`, a step's `doc`), the
  Mermaid view puts a step's doc on a second line of its label, the dashboard shows both, and an
  inbox item posted for an input without a body takes that input's doc as its body (§8a).
- **Step inputs** (`in`): `{"default": <json>}` a literal; `{"source": "<ref>"}` one value;
  `{"source": ["<ref>", ...]}` fan-in: an array of the values, in order. A ref is a plan input
  name (`repo`) or `<step>/<output>`, optionally followed by `.<field or index>...` to reach
  inside a value. Optional fn inputs may be omitted.
- **Fan-out:** several steps read the same output. **Fan-in:** a list `source`, or several inputs
  from different steps (`core.collect` gathers into one array).
- **Scatter** (dynamic fan-out): `"scatter": "<input name>"`. That input must receive an array
  whose items fit the fn's input type; the step runs once per item (the other inputs are the same
  for every run) and each of its outputs becomes an array, in item order. The step succeeds when
  every run succeeds and fails if any fails.
- **Plan outputs** name the plan's results: `{"source": "<ref>"}`. `fn_call` and `status`
  report them.
- A step is **ready** when every plan input and step it reads has a value / has `succeeded`.

**Validation** (every edit must pass; all errors returned with paths): ids valid; docs are
strings and an input's object form has a `type`; every `run`
exists (in the project's lookup order); every required fn input bound, no unknown inputs; every ref names a declared plan input
or an existing step and one of its fn's outputs (fields navigated through record types, anything
under `Any` allowed; a scattered step's outputs are arrays); `fits` holds for each source (for a
list source, the target must be an array or `Any` and each element must fit its item type; for
the scatter input, each item must fit the fn's input type); defaults and plan input values pass
`check_value`; the graph is acyclic.

**Edits.** `patch(rev, ops, reason, author)`: `ops` is RFC 6902 JSON Patch against the plan
without `rev`. A stale `rev` fails with `conflict` (and the current rev). A valid edit bumps
`rev`, rewrites `plan.json`, and appends a `plan.edit` record `{"rev", "author", "reason",
"ops"}` to the project's log (creation is rev 1, one `add` of the whole plan). Removing or
changing a running step is refused.

## 6. Runner and state

`state.json`:
`{"inputs": {"<name>": <value>}, "steps": {"<id>": {"status", "run_ids", "started", "finished",
"outputs", "error", "manual", "inputs_hash"}}}` with status `pending`, `running`, `succeeded`,
`failed`, `stale`. A
scattered step also records `done` and `total` runs. A step waiting for a process slot stays
`pending`; a scattered step whose runs fail stops its other runs and fails with `run <i>: ...`.

**Staleness.** A result is only valid for the inputs it was computed from. When a step starts
(and so when it succeeds) or is set by hand, its state records `inputs_hash`: a hash of the
canonical JSON of its resolved inputs (the object it runs with, unbound optional inputs null; for
a scattered step the whole array), or null for a step set by hand with `force` while what it
reads was not ready ("inputs unknown"). Each tick, in dependency order, a `succeeded` step becomes
`stale` when a step it reads is stale, or when its inputs are all available and hash differently
(an upstream re-ran with a different result, a plan input changed, its bindings were edited, or
a null hash once everything it reads is there). While an upstream is re-running the step keeps
its status: it turns stale only if the new result differs. Stale steps keep their outputs for
inspection but never re-run by themselves, and steps reading them wait (they are not
`succeeded`). A stale step whose inputs hash as recorded again (the upstream came back to the
same value) is `succeeded` again. `step_retry` re-runs a stale step; `step_set_output` accepts
its result by hand again. `status` and the views show stale steps distinctly. A succeeded step
with no `inputs_hash` at all (state written before hashes existed) adopts the current hash.

Loop (every ~1 s, and right after an in-process edit), over all projects:
1. New steps get `pending`. State entries of steps removed from the plan, and values of plan
   inputs removed from it, are dropped.
2. Finished processes: exit 0 with valid outputs → `succeeded` with `outputs`; otherwise
   `failed` with `error` (exit code or type errors, plus the stderr tail). A scattered step
   collects its runs as they finish.
3. Mark stale steps (above), then start ready `pending` steps, at most `max_parallel` processes
   across all projects. Built-in fns run inline; staleness is re-checked after each round of
   inline results, so nothing starts from a result that no longer holds.
4. If anything changed, write `state.json`, then append a `step.status` record per step whose
   status changed in this pass (§6b).
5. Calls: follow the log's `call` records; start `pending` calls within the same `max_parallel`
   budget and log each status change (§6b). `call_status` reads a call's latest record.

On startup, steps and calls left `running` by a previous runner are marked `failed` with
`error: "runner restarted"`. A `direct` call (§8 `fn_call`) is run by the process that made it,
never by the runner; if that process dies before logging the end, the runner logs the call
`failed` with `error: "the process running this direct call is gone"`.

**Manual values** (recorded in state and as log records with the current `rev`, `author` and
`reason`, so the history shows who set what; a manual status change also gets its `step.status`
record):
- `plan_set_input(name, value)`: sets a declared plan input (type-checked). Steps reading it
  become ready. Changing it later makes steps that already read it `stale`. Record `plan.input`
  `{name, value}`.
- `step_set_output(step, outputs, force?)`: marks a non-running step `succeeded` with the given
  outputs (type-checked against its fn's outputs, arrays for a scattered step), `manual: true`.
  For manual work, a failed step whose result is known, or a stand-in. Refused (`invalid`, its
  `errors` naming each: `step a is pending`, `plan input n has no value`) while any step it reads
  from has not succeeded or any plan input it reads has no value, unless `force: true`
  (deliberately bypassing a broken upstream; the step then records unknown inputs and turns
  `stale` once those values are all there). It is never run afterwards unless retried. Record
  `step.output` `{step, outputs, force?}`.
- `step_retry(step)`: sets a `failed`, `stale` or manual step back to `pending`. Its succeeded
  dependents turn `stale` when it produces a different result. Record `step.retry` `{step}`.
- Setting a step's input by hand is an edit: `step_set_input(step, input, value)` patches its
  binding to `{"default": value}`.

**Built-in fns** (in `src/sluice/fns/`, run inline):
- `core.echo`: inputs `{"value": "Any"}`, outputs `{"value": "Any"}`.
- `core.collect`: inputs `{"items": "Any[]"}`, outputs `{"items": "Any[]"}`. The fan-in join.
- `core.format`: inputs `{"template": "string", "values": "Any"}`, outputs `{"text": "string"}`.
  Python `str.format`: an array fills `{0}`, `{1}`...; a record fills `{name}`. Non-string values are
  rendered as JSON. Builds prompts from upstream outputs.

## 6a. Verify

`verify(project?)` checks everything and returns every problem it finds, each with a location
and a message; it changes nothing. `where` is a file path (relative to `SLUICE_HOME` when inside
it, e.g. `projects/p/fns/x.y/fn.json`), followed by `#<path in the document>` for JSON
(`projects/p/plan.json#steps.a.run`, `projects/p/state.json#inputs.n`) or `:<line>` for `.env`
files. Without a project it checks the built-in and global scopes and every directory under
`projects/`; with one, the built-in and global scopes and that project. It covers:
- every `fn.json`: shape (`name`, `inputs`, `outputs`, optional `doc`, nothing else), the name
  matching its directory, every type parsing, `main.py` present for non-built-ins;
- name collisions across scopes (see §2);
- `project.json` shape (`name` equal to its directory, optional string `description`), `.env`
  files parsing as `KEY=value` lines (blank lines, `#` comments and `export ` allowed);
- the plan: full validation (§5) against the project's functions;
- `state.json` agreeing with the plan (no state for unknown steps or undeclared plan inputs,
  valid statuses, outputs of succeeded steps passing their fn's output types, plan input values
  passing their types).

`{"ok": bool, "problems": [{"where", "message"}]}`. CLI `sluice verify [-p P]` prints them and exits
non-zero when there are any.

## 6b. The log

Each project has one append-only `log.jsonl`, and `SLUICE_HOME/log.jsonl` holds the calls made
without a project. Every record is `{"seq", "at", "kind", ...}`; `seq` counts up from 1 per log.
Appends hold the directory's `.lock` flock, so writers in any process (the store, the runner, a
fn process posting to a thread) get distinct, increasing seqs; the file is in seq order. Kinds:

| kind | fields | written by |
|---|---|---|
| `plan.edit` | `rev, author, reason, ops` | every accepted edit (§5) |
| `plan.input` | `rev, author, reason, name, value` | `plan_set_input` |
| `step.output` | `rev, author, reason, step, outputs, force?` | `step_set_output` |
| `step.retry` | `rev, author, reason, step` | `step_retry` |
| `step.status` | `step, from, to, error?, run_ids?` | every status change of a step: the runner, once per pass (`from` is the status before the pass, so a built-in finishing inline goes `pending` → `succeeded`; a new step's `from` is null), and the manual tools; `error` when it failed, `run_ids` when it finished |
| `call` | `call, fn, status, inputs?, outputs?, error?, direct?, pid?` | every status change of a `fn_call`; the pending record (a direct call's first) carries the `inputs` |
| `message` | `thread, from, to?, body, data?` | `thread.post` (§10) |
| `inbox.post` | `item, title, from?, input?` | `inbox_post`, `inbox.ask` (§8a) |
| `inbox.answer` | `item, answer, by` | `inbox_answer` and the dashboard's answer route |
| `inbox.close` | `item, reason?, by` | `inbox_close` |

The log is history, not the source of truth, so it is capped at `config.log_max` records
(default 10000): when an append takes it past the cap, the oldest records are dropped under the
lock, down to 90% of the cap (so a full log is not rewritten on every append), together with the
run dirs that only dropped records referred to (a `call` record refers to `runs/<call>`, a
`step.status` record to its `run_ids`; dirs `state.json` still lists are kept). The latest record
of a call that is still pending or running is never dropped. `plan_history` therefore reaches
back only as far as the log does.

Readers take no lock: a line not yet complete is left out until it is. `log_read` and `log_wait`
(§8), `thread.wait` and `sluice watch` share one filter: `kinds` (exact kinds, or a group name,
`step`, `plan` or `inbox`, for every kind under it) and `threads` (messages only on these threads; given
without `kinds`, only messages at all).

## 7. Helper library `sluice.fn` (stdlib only)

```python
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice.fn import run, sh, Transient

def main(inp, ctx):
    return {"sha": sh(["git", "rev-parse", "HEAD"], cwd=inp["path"]).stdout.strip()}

if __name__ == "__main__":
    run(main)            # run(main, retries=3, backoff=600) retries on Transient
```

`run(main, retries=0, backoff=30)` reads stdin, calls `main(inp, ctx)` (`ctx`: `project`, `step`,
`run_id`, `run_dir`, `home`, `fn_dir`, `attempt`, `log(msg)`) with stdout redirected to stderr,
prints the result as JSON. On `Transient` it sleeps `backoff` s (env `SLUICE_BACKOFF` overrides)
and calls `main` again, up to `retries` times; any other exception, or running out of retries,
prints the traceback and exits 1. `sh(argv, cwd=None, check=True, env=None, timeout=None,
input=None)` runs a command and raises `ShError` on a non-zero exit when `check`.

## 8. MCP server

`sluice serve` runs the runner and an MCP server (official `mcp` SDK, streamable HTTP) at
`http://<host>:<port>/mcp` in one process. With `--no-runner` it serves only, and a separate
`sluice loop` runs the steps: the two share nothing but the files (the runner polls about once
a second), so the server can restart without ending running steps. Stopping the runner ends
the fns it is running. Errors are tool errors whose message is JSON
`{"error": "not_found"|"conflict"|"invalid"|"bad_request", "message", ...}` (`conflict` carries
`current_rev` for a plan edit, or `status` for an inbox item that is no longer open; `invalid`
carries `errors`). `rev` is optional on the convenience tools (they apply
to the current revision under the lock) and required on `plan_patch`.

**Docs for agents.** The server sets MCP `instructions` from `src/sluice/docs/instructions.md`
(short: what sluice is, the workflow, where to read more). A `docs(topic?)` tool returns the index
(topic names with their first heading) without a topic, or `src/sluice/docs/<topic>.md`. The same
pages are MCP resources at `sluice://docs/<topic>`. Tool docstrings describe every argument.
Validation errors carry the path and what was expected.

**Views.** A dashboard that only reads, with one exception: answering an inbox item (§8a).
Server-rendered HTML with inline CSS (light and dark via `prefers-color-scheme`, usable at phone
width), a top nav (Projects · Functions · Log · Inbox), every value HTML-escaped (plans, logs
and inbox items are untrusted). The Inbox link carries the count of open items across all
projects as the dashboard's one red badge (none when nothing waits). The only external assets
come from cdn.jsdelivr.net: Datastar v1.0.4, mermaid, and, on inbox pages,
`@openuidev/lang-core@0.3.0/+esm` (jsDelivr's ESM build; it imports `zod@4.6.5` from the same
CDN). Markdown bodies are rendered on the server by `markdown-it-py` (CommonMark plus tables,
raw HTML escaped, unsafe link schemes refused).
- Mermaid (`flowchart LR`): plan inputs as rounded nodes, steps as boxes labelled
  `id / fn / status` (a scattered step shows `done/total`; a step's doc, one line of at most 60
  characters, below it), plan outputs as rounded nodes, an edge
  per source ref labelled with the output name, one colour class per status (pending grey,
  running blue, succeeded green, failed red, stale amber, manual outlined; a stale manual step
  shows as stale).
- `GET /`: every project with its description, step counts by status, plan rev and last change
  (the later of the last log record and the last state write), each linking to its page.
- `GET /projects/<name>`: step counts and links (log, functions, inbox), the Mermaid diagram,
  plan input values (with their docs) and output values, a steps table (fn, status, started,
  finished, first line of error; a step's doc under its id) whose
  rows expand (`<details>`) to the step's bindings, run inputs, outputs, full error and stderr
  tail, the last 20 history records (rev, time, author, what: kind plus step, input or op
  count, reason) and the last 10 log records.
- `GET /projects/<name>/log` (and `GET /log` for the home log): the log viewer. Newest first, 50
  records per page; `?before=<seq>` shows the 50 matching records below that seq, `?after=<seq>`
  the 50 above it, with newest / newer / older links. Filters are query parameters, so a URL is
  shareable: `kind` (repeated or comma-separated; exact kinds or the `step`/`plan` groups) and
  `thread` (comma-separated), the §6b filter `log_read` uses. A row shows seq, time, kind and a
  one-line summary (`s2 succeeded → stale`, `rev 7 by orch: reason (2 ops)`, `questions from
  e2e: body…`, `<call> <fn> <status>`) and expands to the full record as JSON. Unknown kinds or a
  bad seq are a 400 page.
- `GET /fns?project=<name>` (project optional): every function that context sees, grouped by
  scope, with doc and typed inputs and outputs (`string[]`, `enum(a|b)`, `{field: type}`,
  `T?`); a function with a problem (e.g. a collision) is shown in red with the verify message.

**Live updates.** Every page renders completely on first load and works without JavaScript
(the log filter is a plain GET form). The index, project and log pages then open one Datastar
SSE stream each (`GET /stream`, `/projects/<name>/stream`, `/projects/<name>/log/stream`,
`/log/stream`). The index and project pages carry a `ver` signal, a hash of the stats (mtime,
size) of the files they read (`project.json`, `plan.json`, `state.json`, `log.jsonl`). The
server polls those stats about once a second off the event loop (never blocking the runner or
the MCP tools); when they change it re-renders the page's parts (each an element with an id:
summary, diagram source, inputs, outputs, steps, history, recent log) and sends a
`datastar-patch-elements` event for each part that differs, then the new `ver`. An idle page
receives nothing; a client whose `ver` is not current (e.g. reconnecting) first gets every part.
Parts are morphed, so an expanded row stays open; the diagram's source element is replaced and
its `data-init` re-renders the Mermaid SVG into the view, keeping the old drawing until the new
one is ready. On the log page, changing the filter updates the `kinds`/`thread` signals and
reconnects the stream, which sends the new table and rewrites the address bar to the filter's
query string; on the newest page, new matching records are prepended as they are appended.
Streams end when the server shuts down; the client reconnects with backoff.

- `GET /inbox` (every project) and `GET /projects/<name>/inbox`: the items, filtered by
  `?status=open|answered|closed|all` (default open; open oldest first, the others newest first).
  An item shows its title, project, id, `from`, age, the input it sets, and its body as markdown.
  An open item has an answer box that works without JavaScript (a form POST of `text`, then a
  303 back); with it, `/static/inbox.js` draws the item's `ui` (§8a) above the box, and folds
  the box away when the program has buttons. An answered item shows its answer, a closed one
  its reason. Below the open items, a read-only "Waiting on a person" table lists the plan
  inputs that hold up a step: required, no value, read by a step that has not run, and no open
  item names them (project, input, type, doc, the steps waiting). A value for one comes through
  an inbox item or `plan_set_input`; the table has no write of its own. The page streams like
  the index (`/inbox/stream`, `/projects/<name>/inbox/stream`,
  parts: the items and the nav badge); each open item's answer area carries
  `data-ignore-morph`, so a patch never resets what a person is typing.
- `POST /projects/<name>/inbox/<id>/answer`: the only write. A JSON body is the answer object;
  a form body (`text`, `next`) becomes `{"action": "answer", "text"}` and redirects to `next` (a
  local path) on success. Both call the store's `inbox_answer`, the tool's own code path, with
  author `dashboard`. Refusals map to 404 (`not_found`), 409 (`conflict`: already answered or
  closed) and 400 (`invalid`, `bad_request`); JSON gets the error payload, a form an HTML page. A
  request whose `Origin` is not this host is refused (403).
- `GET /static/inbox.js`, `GET /static/openui.json`: the renderer and its vocabulary.

`plan_view(project, format)` returns the Mermaid text, or the project page as a standalone HTML
document (no nav, no links, no log, no stream), from the same renderer.

| Tool | Args | Returns |
|---|---|---|
| `docs` | `topic?` | the index, or one page as markdown |
| `projects_list` | – | `[{name, description, rev, counts}]` |
| `project_create` | `name, description?` | `{name}` (with an empty plan) |
| `project_update` | `name, description` | `{name}` |
| `fn_list` | `project?` | `[{name, doc, inputs, outputs, scope, error?}]` in lookup order (`scope`: builtin, global or project); `error` marks a function with a problem |
| `fn_get` | `name, project?` | the fn.json plus `scope` and `path` |
| `fn_save` | `fn, main_py, project?` | writes `fn.json` + `main.py` into the project's (or, without a project, the global) `fns/<name>/` after validating `fn`; `{scope, path}` |
| `fn_call` | `name, inputs, project?, wait?, direct?` | checks `inputs`, then queues one fn run outside the plan (a `call` record in the log, §6b) for the runner; `{call, status, outputs?, error?}`, waiting up to `wait` s. `direct: true` runs it in the calling process to the end instead (no runner needed) |
| `call_status` | `call, project?` | `{call, status, outputs?, error?, stderr_tail?}` from the call's latest record |
| `plan_get` | `project` | `{rev, plan}` |
| `plan_patch` | `project, rev, ops, reason, author?` | `{rev}` |
| `plan_history` | `project, since_rev?` | the `plan.edit`, `plan.input`, `step.output` and `step.retry` records still in the log (with `rev` > `since_rev`) |
| `plan_set_input` | `project, name, value, reason?` | `{ok}` |
| `step_set_input` | `project, step, input, value, reason?, rev?` | `{rev}` |
| `step_set_output` | `project, step, outputs, reason?, force?` | `{ok}` (§6: refused while what it reads is not ready, unless `force`) |
| `step_retry` | `project, step, reason?` | `{ok}` (a failed, stale or manual step) |
| `log_read` | `project?, since_seq?, kinds?, threads?, limit? = 200` | `{records, last_seq}`: matching records oldest first (§6b filter); after `since_seq` the first `limit` of them (`last_seq` is then the last one returned, else the log's last seq, so passing it back continues); without `since_seq` the last `limit`. No project: the home log |
| `log_wait` | `since_seq, project?, kinds?, threads?, timeout? = 300, limit? = 200` | like `log_read` after `since_seq`, but waits (polling the file, without blocking the server or the runner) until at least one matching record exists or `timeout` s pass (then `records` is empty) |
| `verify` | `project?` | `{ok, problems: [{where, message}]}` (§6a) |
| `plan_view` | `project, format: "mermaid"\|"html"` | the diagram or page as text |
| `status` | `project` | `{rev, inputs: {name: value or null}, input_docs?: {name: doc}, outputs: {name: value or null}, steps: [{id, run, status, started, finished, outputs?, error?, doc?, manual}]}` (status: pending, running, succeeded, failed or stale; `input_docs` only when some input has a doc) |
| `inbox_post` | `project, title, body?, ui?, input?, from?` | `{id}` (§8a); refused (`not_found`) when `input` is not a declared plan input |
| `inbox_list` | `project?, status? = "open"` | the items with that status (`open`, `answered`, `closed` or `all`), each with its `project`, oldest first; every project's without `project` |
| `inbox_answer` | `project, id, answer` | the answered item; `conflict` (with `status`) unless it is open; with `input`, `invalid` when the value does not fit (the item stays open) |
| `inbox_close` | `project, id, reason?` | the closed item; `conflict` unless it is open |

## 8a. The inbox

Each project has an inbox, `inbox.json`: `{"items": [...]}` in posting order, written atomically
under the project's flock (like `state.json`), and not trimmed with the log. An item is `{id,
title, body?, ui?, input?, from?, status, created, answer?, answered?, closed?, reason?}`: `id`
is `i<n>` (one more than the highest in the project), `body` markdown, `ui` an OpenUI Lang
program, `input` a plan input, `from` who asked (a step id, an agent), `status` `open`,
`answered` or `closed`, the times ISO UTC. Only an open item changes, once: answering or closing
anything else is refused (`conflict` with its `status`), which is what makes a stale button or a
second answer harmless. Every change appends one log record (§6b), so `log_wait(project,
since_seq, kinds=["inbox"])` and `sluice watch --kinds inbox` wake whoever waits on it.

An **answer** is `{action: string, params?: object, values?: object, text?: string}` (nothing
else). With `input`, answering sets that plan input through `plan_set_input`'s own path (type
check, `plan.input` record by the answering author, reason `inbox item <id>: <title>`) before
the item is marked answered; the value is the first present of `values.value`, `params.value`
and `text`. None present, or a value that does not fit, refuses the answer (`invalid`) and the
item stays open. `inbox_post` refuses an `input` the plan does not declare; without a `body`,
the item's body is that input's doc (§5), if it has one.

**The ui.** `ui` is OpenUI Lang (openui.com), drawn in the browser by a small vanilla-DOM
renderer (`src/sluice/static/inbox.js`, no build step) around lang-core's parser. The vocabulary
is closed and lives in one file, `src/sluice/static/openui.json` (each component's positional
props with their types, and a description): the renderer builds the parser's JSON Schema from
it and has one renderer per component; `docs("inbox")` lists the same signatures (a test
checks both). Components: Stack, Heading, Text, Callout, Table, Separator, Form, Input,
Textarea, Select, Radio, Checkbox, Button (prose goes in `body`, so there is no Markdown
component). Program text is only ever set as DOM text, never as HTML. A statement with an
unknown component or a bad prop, a reference to nothing, an unused statement or a line that is
not a statement is dropped, and the item shows `n lines dropped` with the reasons; if nothing is
left to draw, it says so. The text box always remains. A Button answers with `{action (default
"submit"), params (default {}), values}`, where `values` maps each field of its Form (or, outside
a form, each field outside any form) to its value (Input text, or a number for `type: "number"`;
Checkbox a boolean; Select and Radio the chosen option or ""); a primary button first checks the
fields' `rules` with lang-core's validators and shows what fails.

## 9. CLI

MCP is the interface; the CLI only starts it and reaches the same tools from a shell:

```
sluice serve [--host H] [--port P] [--no-runner]
                                      runner + MCP server + dashboard (with the inbox)
sluice loop                           runner only
sluice tool                           list the MCP tools with one-line descriptions
sluice tool <name> '<json args>'      call that tool in-process and print its result
sluice watch [-p P] [--kinds k1,k2] [--threads a,b] [--since-seq N]
                                      print new log records as JSON lines (§10)
```

Every command creates `SLUICE_HOME` with the default `config.json` on first use. `sluice tool`
builds the same MCP server object `serve` exposes and calls its tool (same argument validation,
same code path). It prints JSON results (text for `docs` and `plan_view`); a tool error goes to
stderr as the error JSON with exit 1, and a result with `"ok": false` (`verify` with problems)
also exits 1.

## 10. Built-in fns and first-party packs

`src/sluice/fns/` holds only what sluice itself needs: `core.*` (§6), `thread.*` and
`inbox.ask` (below), plus shared helper code for built-in fns in `src/sluice/fns/_lib/`. Their `fn.json` files are
the reference for their types.

Every other fn in this repo is a **first-party pack** under `packs/`, not loaded by default:

- `packs/agents/`: `agent.devin`, `agent.codex`, `agent.claude`, `agent.run`, `agent.review`,
  `decide.llm` (run Devin, Codex, Claude, the review agent, decisions)
- `packs/git/`: `git.worktree`, `git.worktree_rm`, `git.head`, `git.merge`, `git.rebase`,
  `git.push`, `gh.pr` (worktrees, merge, rebase, push, pull requests)
- `packs/jev/`: `jev.ask`, `jev.choice`, `jev.score`, `jev.noul` (Jev, TypeSafe's System One
  model; needs `TYPESAFE_API_KEY`), with its shared client in `packs/jev/_jev/`

A pack is installed by copying `packs/<pack>/*` into `SLUICE_HOME/fns/` (or a project's `fns/`),
or by adding the pack's absolute path to `config.fn_dirs`; its fns then load in the global (or
project) scope. Packs are self-contained: a fn finds its helpers relative to its own directory,
so a copy anywhere works (see `packs/README.md`).

**Threads** (`thread.*`, plain functions, no engine support): a thread is the project's log
filtered to `message` records with that `thread` name (the project comes from `SLUICE_PROJECT`;
without one they fail with a clear error; thread names use the id pattern). A message is
`{"seq", "at", "kind": "message", "thread", "from", "to"?, "body", "data"?}`; its `seq` is the
log's. Agents post through `fn_call` (any harness) and read with `log_read`/`log_wait`
(`threads: [name]`); plans use them as ordinary steps.
- `thread.post`: inputs `{thread: string, body: string, from: string, to: string?, data: Any?}`,
  outputs `{seq: int}`. Appends under the project's flock, so concurrent posters get distinct,
  increasing seqs.
- `thread.wait`: inputs `{thread: string, since_seq: int?, to: string?, timeout: int?}`, outputs
  `{messages: Any[], last_seq: int}`: the messages after `since_seq` (default 0: all) and, with
  `to`, only those addressed to it or to nobody; blocks (polling the log every 0.5 s) until there
  is at least one or `timeout` s (default 300) pass, then `messages` is empty. `last_seq` is the
  log's last seq, to pass back as `since_seq`.

**`inbox.ask`**: inputs `{title: string, body: string?, ui: string?}`, outputs `{answer: {action:
string, params: Any?, values: Any?, text: string?}}`. It posts an item to the project's inbox
(§8a) with `from` = the step (`call <run_id>` for a call) and polls `inbox.json` every 0.5 s
until the item is answered (the answer is the output) or closed (the step fails with `inbox item
<id> was closed without an answer: <reason>`). It waits as long as it takes. If this step
already has an open item with the same title, body and ui (a run killed by a runner restart,
then retried), it waits on that one instead of posting again. Like the thread fns, it needs a
project.

**Watching.** Agents watch through MCP: `log_wait` in a loop, passing back `last_seq`. For
harnesses with monitors (e.g. Claude Code's Monitor tool) the shell form is `sluice watch [-p P]
[--kinds k1,k2] [--threads a,b] [--since-seq N]`: it follows the project's log (the home log
without `-p`) with the same filter as `log_read`, from the end of the log (or after `--since-seq`),
printing each matching record as one JSON line (flushed) as it is appended, and never exits. It
reads the file only; it needs no runner or server.

## 11. Conventions

Python ≥ 3.12, `uv` for everything. `src/sluice/fn.py`, `src/sluice/__init__.py`,
`src/sluice/log.py`, `src/sluice/inbox.py` and `src/sluice/util.py` import only the stdlib (fns
import them). Tests in
`tests/`; external tools are faked in tests. Commits: plain sentences, no AI attribution of any
kind; stage exact paths.
