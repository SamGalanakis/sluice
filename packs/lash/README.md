# lash pack

Functions for driving lash work with sluice: kiln forks, the L0–L3 check ladder with test
receipts, lane-rules briefs, the task and arc-landing composites, a known-red ledger, and Linear.
Generic agent and git work binds to `packs/agents` and `packs/git` (SPEC §11).

External binaries come from env overrides so tests can fake them: `SLUICE_KILN_BIN` (`kiln`),
`SLUICE_JUST_BIN` (`just`), `SLUICE_PYTHON_BIN` (`python3`), `SLUICE_LINEAR_URL`
(`https://api.linear.app/graphql`) with `LINEAR_API_KEY`. Shared code lives in `_lib/lashkit`
(stdlib only); each `main.py` puts `_lib` on `sys.path`.

## Functions

| fn | in → out | does |
|---|---|---|
| `lash.fork` | `name, base?, branch?, detach?` → `path, branch, sha, base` | `kiln fork lash <name>` (the last stdout line that is a directory is the fork), then `git switch -c <branch or name> <base>`. `base` defaults to `origin/main`; a bare branch name resolves to `origin/<name>` when that exists. `detach` stays detached (branch `""`). |
| `lash.fork_rm` | `name` → `removed` | `kiln rm lash <name>` (never `rm -rf`). |
| `check.l0` | `path, base?` → `ok, summary, log, failures` | `kiln fmt -- --check`; `git diff --check` since the merge-base with `base` (else uncommitted changes). |
| `check.l1` | `path, base?, labels?, args?, allow_no_tests?` → `ok, summary, log, tests_run, failures` | L0, then `kiln test <labels> <args>`, then `python3 scripts/dev-test.py [--base base]`. It stops at the first red stage. The result is judged by receipts (below). |
| `check.l2` | `path, gates, known_reds?` → `ok, results, new_reds, known_reds, stale, summary, log` | Runs the named gates and checks every failure against the known-red ledger. `ok` means no **new** reds. |
| `check.l3` | `path, extra?, known_reds?, allow_known_reds?` → same as L2 | `kiln test //:workspace_tests`, then `extra` gates. Known reds fail the release gate unless `allow_known_reds`. |
| `lash.brief` | `task, may_edit, gates?, report_path?, path?, branch?, context?, ticket?` → `brief` | Effect-free. The brief opens with "Read /workspace/notes/lash/lane-rules.md and /workspace/code/lash/AGENTS.md first; both bind you." It covers scope, gates, the attribution rule and the report. |
| `lash.standards` | → `path` | The absolute path of `standards/coding-standards.md` (a composite cannot compute a path). |
| `lash.gate` | attempt context + L1 result → `ok, attempt, next?, summary` | The retry and escalation gate (below). |
| `lash.card` | `door, blast, what, checks, known_reds?, part_of?, refs?` → `body, draft` | Effect-free. Renders the merge-danger PR body. A one-way door gives `draft: true`. |
| `linear.update` | `issue, state?, comment?` → `issue_id, identifier, state?, comment_id?` | Looks up the issue by identifier, sets its state by name within the issue's team, and adds a comment. HTTP 429 and 5xx responses count as transient. |
| `lash.task` | composite, see below | The unit of work. |
| `lash.attempt` | composite | One attempt in an existing fork. `lash.gate` spawns it for retries. |
| `lash.finish` | composite | review → L1 → merge into arc → fork rm. |
| `lash.arc_land` | composite | L2 → PR from the arc branch to `main`. |

Gate spec (L2/L3): `{"kind": "just"|"kiln-test"|"script", "target": ..., "args"?: [...],
"min_tests"?: int}`, with gate id `<kind>:<target>`. The kinds run as follows:
- `just <target>`
- `kiln test <target>`
- `script`: `python3` for `.py`, `bash` for `.sh`, otherwise run directly

### Receipts: a check that ran zero tests is a lie, not a pass

Bazel's summary counts targets. A filter that matches nothing, such as a bare name in a
nested-module target, still reports PASSED. So receipts count **cases**:
- For every target in the console summary, the libtest `test result: … N passed; M failed` lines
  (and pytest summaries) come from `<fork>/bazel-testlogs/<pkg>/<name>/{test.log,*_of_*/test.log}`.
  If a target has no log there, the parser falls back to its `Test output for` console section.
- Failures are `<label> <case>` per failing case. A target that failed with no known case (a
  timeout or build failure) is listed as `<label>`.
- `check.l1`: every label passed in `labels` must itself execute at least one case. If dev-test
  planned a `kiln test` (its `+ kiln test …` line), at least one case must have run. If nothing
  was requested at all (a docs-only diff), the check is red unless `allow_no_tests` is set.
- L2/L3: a `kiln-test` gate needs `min_tests` cases (default 1). `just` and `script` gates
  default to 0 but can set their own minimum.

## The task shape

```
lash.task (name, task, arc, engine, may_edit, labels?, ladder?, ...)
  fork ── lash.fork(name, base=arc)                      kiln fork, branch <name>
  brief ─ lash.brief(task, may_edit, path, branch, ...)
  attempt ─ lash.attempt(attempt=1, spec=brief)
     work ── agent.run(engine, cwd=path, spec, resume?)
     check ─ check.l1(path, base, labels)       after work
     gate ── lash.gate(check.*, work.session, ...)
     finish ─ lash.finish                        when gate.ok == true
        review ─ agent.review(cwd, base, standards=lash.standards.path)
        recheck ─ check.l1                      after review
        red_after_review ─ core.fail            when recheck.ok == false
        merge ─ git.merge(source=branch, target=arc, push=true)  when recheck.ok == true
        conflict ─ core.fail                    when merge.merged == false
        rm ──── lash.fork_rm(name)              when merge.merged == true
```

Implementers get process rules through the brief (lane-rules.md, AGENTS.md). The reviewer gets
design standards (`standards/coding-standards.md`, derived from lane-rules' Design rules and
AGENTS.md) and works with fresh context. It commits fixes; it does not comment. A red L1 after
review and a merge conflict each fail a `core.fail` node, so the runner opens an inbox item and
the fork is kept. Workers commit only: the plan merges task branches into the arc branch, and
PRs exist only from arc to `main`.

## Retry and escalation: `lash.gate`

A plan is a DAG, so the loop "retry the warm worker with the failing output, then escalate the
engine" is unrolled at run time. Each attempt ends in a gate:

```
attempt 1 ── L1 red ──> gate(1) ──spawn──> <name>-a2: lash.attempt ── L1 red ──> gate(2) ──spawn──> <name>-a3 ...
                │ green                                        │ green
                v                                              v
            finish (review, L1, merge, rm)                   finish
                                                  last rung red: gate fails -> inbox
```

- `ladder` lists the engine for each attempt; its length is the attempt budget. The default is
  `[engine, engine, claude]`: try, retry warm, escalate to Opus. During the Codex period, pass
  for example `["codex", "codex", "claude"]` or `["devin", "devin", "codex"]`.
- Green: `ok: true`, and the static `finish` runs.
- Red with attempts left: `ok: false`, so this attempt's `finish` is skipped. The gate also
  spawns (SPEC §4.1 `_spawn`) one top-level `lash.attempt` node named `<name>-a<n+1>`, in the
  same fork and on the same branch:
  - **Same engine as the last attempt, with a session:** `resume` is that session, and `spec`
    holds only the failing summary, the failures and the log tail. The warm worker already has
    the brief.
  - **New engine:** a fresh session whose `spec` is the original brief plus a "Previous attempt"
    section with the failure. `model` is dropped.
- The spawned node inherits the `claims` of the task's top-level plan node, read from
  `plan.json`, so a hot-file claim like `pg-schema` still covers retries. The claim is released
  between the gate finishing and the spawned node starting, so another claimant can slip in.
- Red on the last rung: the gate raises. The node fails, the runner opens a `failure` inbox
  item, and the fork is kept on its branch for whoever picks it up.

**Caveat (SPEC gap).** When a retry happens, the merge runs inside the spawned `<name>-a<k>`
node, not inside the original task node. The task node's `out` binds `attempt.merged/sha/review`,
which then never resolve, so nodes that bind the task's outputs never start. The gate's
spawn reason and `next` output name the node to rewire them to. Proposed SPEC change: let
`_spawn` name a `forward` node, a tail call. The spawning node would stay unfinished and take its
outputs from that node, so the task node could take its output from the spawned attempt.

## The arc shape

```
arc-fork (lash.fork base=origin/main) ─> arc-push (git.push) ─> arc-fork-rm
   ├─> task A (lash.task, claims pg-schema) ─┐
   ├─> task B (lash.task)                    ├─> land (lash.arc_land) ─> linear.update
   └─> task A ──> task C (context from A) ───┘
lash.arc_land: fork (detached at origin/<arc>) ─> check.l2 ─> card (when l2.ok) ─> gh.pr(head=arc, base=main, draft=card.draft) ─> fork rm
                                               └> core.fail (when not l2.ok)
```

Laying out an arc in a plan (see `examples/arc-plan.json`):

1. Create the arc branch once: `lash.fork` from `origin/main`, then `git.push`, then
   `lash.fork_rm`. The fork has to be removed before any task merges, because `git.merge`
   checks out the arc branch in its own worktree. Name the arc branch (and task branches) after
   the ticket, e.g. `fig-3842-send-batch`: lane-rules forbid arc ids in branch names.
2. Add one `lash.task` per unit, each `after` the arc setup. Parallel tasks need nothing more.
   A dependent task binds an upstream output (e.g. `context: {"from": "a.review"}`). That orders
   it after the upstream merge, so its fork is cut from the arc head that already contains that
   work.
3. Claim hot files on the task node (`"claims": ["pg-schema"]`, declared in `resources`).
4. Add `lash.arc_land` `after` every task, and give it the L2 gates the arc's risks name, e.g.
   the live-Restate E2E recipes for host wiring, or the PG leg for store changes. Set `door`
   and `blast` honestly: a one-way door opens a draft PR for a human.
5. Optionally, add `linear.update` after the landing to move the ticket and link the PR.

## Known-red ledger (`known-reds.json`)

```json
{"version": 1, "entries": [
  {"test": "//crates/lash-restate:workbench_e2e", "owner": "node:lash-1-0/s5b",
   "reason": "cron sync-cancel; fixed by S5b", "expires": "2026-10-03"}
]}
```

- `test` is an `fnmatch` pattern that matches a failing item in one of three forms:
  - a Bazel label (this also covers every case in that target)
  - `<label> <case>` (one case)
  - a gate id such as `just:agent-workbench-restate-e2e` (this covers everything in that gate)
- `owner` is the plan node (`node:<plan>/<node>`) or ticket (`FIG-NNNN`) that will fix it.
- `reason` is required.
- `expires` is an ISO date. The entry is live through that day.

Every field is required, and an invalid ledger fails the check node. A failure matched by a live
entry is `known_red`. Anything else is a new red, including a failure that only matches an
expired entry: the report says it expired and who owned it. A gate is `pass`, `known_red` (all
its failures excused) or `red`. L2 is `ok` when there are no new reds. L3 is `ok` only with no
reds at all, unless `allow_known_reds` is set. A live entry that matches something passing and
nothing failing is listed in `stale`, so it can be deleted.

## Tests

`uv run pytest packs/lash -q`. The fns run as processes (`tests/helpers.py`). kiln, just and
python3 are fakes (`tests/fakebin.py`), and `kiln fork` and `kiln rm` manage real git worktrees
of a throwaway repo. Linear is a local `http.server`. `test_lash_static.py` checks every
composite and the example plan against SPEC §4.3/§11 signatures and this pack's `fn.json` files:
refs, ports, required bindings, type fit, `when` values, claims and acyclicity. It stands in
until the core validator is merged.
