# First-party packs

Optional function packs for sluice. They are **not loaded by default** — install the ones a
home or project actually needs.

| Pack | Functions |
|---|---|
| `agents/` | `agent.devin`, `agent.codex`, `agent.claude`, `agent.run`, `agent.review`, `decide.llm` — run Devin/Codex/Claude agents and LLM decisions |
| `git/` | `git.worktree`, `git.worktree_rm`, `git.head`, `git.merge`, `git.rebase`, `git.push`, `gh.pr` — worktrees, merges, rebases, pushes, pull requests |
| `jev/` | `jev.ask`, `jev.choice`, `jev.score`, `jev.noul` — TypeSafe's System One model (Jev); needs `TYPESAFE_API_KEY` |

Each `fn.json` is the reference for that function's typed inputs and outputs.

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
