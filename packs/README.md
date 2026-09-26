# First-party packs

Optional function packs for sluice. They are **not loaded by default** — install the ones a
home or project actually needs.

| Pack | Functions |
|---|---|
| `agents/` | `agent.devin`, `agent.codex`, `agent.claude`, `agent.run`, `agent.review`, `decide.llm` — run Devin/Codex/Claude agents and LLM decisions |
| `git/` | `git.worktree`, `git.worktree_rm`, `git.head`, `git.merge`, `git.rebase`, `git.push`, `gh.pr`, `gh.pr_wait`, `gh.run_latest`, `gh.run_cancel` — worktrees, merges, rebases, pushes, pull requests, workflow runs |
| `jev/` | `jev.ask`, `jev.choice`, `jev.score`, `jev.noul` — TypeSafe's System One model (Jev); needs `TYPESAFE_API_KEY` |

Each `fn.json` is the reference for that function's typed inputs and outputs.

## Agent blocks and sessions

The agent functions (`agent.claude`, `agent.codex`, `agent.devin`, `agent.run`,
`agent.review`) are **open**: a plan step running one may bind extra inputs and declare
outputs (`docs("plans")`). As a plan step, each appends to its task text an `## Inputs`
section (every extra input with its type and value), an `## Outputs you must submit` section
(every declared output with its type and doc, and the exact `sluice tool step_submit` command
to submit them) and the step-thread note (`listen: false` drops only the note). The step fails
if the agent finishes without submitting a required output.

Every agent function takes `session?: string` and returns `session: string` (Claude's session
id, or the id the Devin or Codex harness writes to `<log>.session`; empty when there is none).
A follow-up to a particular agent is another step with `session` bound to the earlier step's
`session` output, or a `fn_call` with that session.

The agents pack is a starting point: which engine runs which kind of work is up to each
project. To route, copy (or wrap) `agent.run` into the project's own `fns/` under a name of
its own and pick the engines there.

## Installing

Copy a pack's contents into a functions directory — the whole pack, including any `_`-prefixed
helper dirs (e.g. `jev/_jev/`):

```sh
cp -r packs/<pack>/* ~/.sluice/fns/                  # every project in this home
cp -r packs/<pack>/* ~/.sluice/projects/<p>/fns/     # one project only
```

Or leave the pack where it is and add its absolute path to `fn_dirs` in
`~/.sluice/config.json`:

```json
{"fn_dirs": ["/path/to/sluice/packs/agents"]}
```

`fn_list` then shows the pack's functions in the `global` (or `project`) scope. Secrets the
fns need (e.g. `TYPESAFE_API_KEY` for the jev pack) go in `~/.sluice/.env` — or the project's
`.env` — as `KEY=value` lines, never in plans.

Each pack's `tests/` holds its pytest suite, run from the repo root (`uv run pytest`).
