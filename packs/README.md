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

Claude always runs Opus. Codex takes `model` `sol` (default) or `astra`, and `effort`
(`minimal`, `low`, `medium`, `high`, `xhigh`, `max`); left out, effort is `high`. Long,
grinding work goes to Devin, not to a bigger codex effort.

Every agent function takes `session?: string` and returns `session: string` (Claude's session
id, or the id the Devin or Codex harness writes to `<log>.session`; empty when there is none).
A follow-up to a particular agent is another step with `session` bound to the earlier step's
`session` output, or a `fn_call` with that session.

The agents pack is a starting point: which engine runs which kind of work is up to each
project. To route, copy (or wrap) `agent.run` into the project's own `fns/` under a name of
its own and pick the engines there.

## Live sessions

Claude runs as a real interactive session (`claude` in its TUI, on your own login), not a
one-shot `claude -p`: `agent.claude`, `agent.review` and `agent.run` with engine `claude` go
through `_agents/native/`. (Codex and Devin still run through their harness scripts.) Per run:

- A private tmux server on `<run_dir>/tmux.sock` runs the session. The step's stderr starts
  with an `attach:` line (`cd <run_dir> && tmux -S tmux.sock attach`): run it to watch or
  steer the worker live.
- The task goes to `<run_dir>/task.md` and the session gets one typed line pointing at it.
  (A TUI collapses a long or multi-line paste into a placeholder the model reads as pasted
  content rather than a request; only one line of at most 500 characters is typed as it is.)
- The wrapper, not the model's end of turn, decides when the step is done. When a turn ends:
  every required declared output submitted → done; the session still waits on its own
  background work (below) → it keeps waiting; otherwise it is nudged ("Your turn ended but
  these outputs are not submitted: …"), up to `SLUICE_AGENT_NUDGES` (3) times, then the step
  fails naming the missing outputs and the agent's last message. A step that declares no
  required outputs is done once the session is idle with nothing pending.
- Messages addressed to the step on its thread (`thread.post` to `step-<id>`, to the step
  or to nobody) are typed into the live session as they arrive, so the step-thread note no
  longer asks the agent to poll `log_read`. `listen: false` turns this off.
- Caps: `SLUICE_AGENT_MAX_MIN` (600) minutes of wall clock; `SLUICE_AGENT_STALL_MIN` (30)
  minutes busy with no transcript growth. `SLUICE_AGENT_SETTLE_S` (10) seconds of idle before
  a nudge. `SLUICE_AGENT_GRACE_MIN` (10) minutes of idle before the first nudge, for an engine
  with no waiting signal (Claude has one).
- `session` resumes the session, and only from the directory it was started in (Claude files
  a session under its cwd); another cwd fails the step before anything starts. A rate limit or
  capacity error raises `Transient`; the retry resumes the session and tells it to continue.
- `step_cancel` (SIGTERM) ends the tmux server, the engine and every process it started,
  background shells included.

### Claude: idle is not done

Observed on Claude Code 2.1.283 (`--settings <run_dir>/claude-settings.json` adds
`SessionStart`, `Stop`, `StopFailure` and `UserPromptSubmit` hooks that append their payload
to `<run_dir>/hooks.jsonl`; the status file is `~/.claude/sessions/<pane pid>.json`):

- A background Bash task (`run_in_background`), then the turn ends: the status file goes
  `busy` → `shell` and stays `shell` while the task runs; the `Stop` payload lists it,
  `"background_tasks": [{"type": "shell", "status": "running", "command": "sleep 60 && …"}]`.
  When it finishes the model comes back by itself: a `UserPromptSubmit` whose prompt is a
  `<task-notification>` (transcript `origin.kind` `task-notification`), status `busy`, then
  `idle`, and a new `Stop`.
- The Monitor tool looks the same: its watch command is a `shell` background task.
- `ScheduleWakeup` (60 s), then the turn ends: the status file says plain `idle`, so it cannot
  tell. The `Stop` payload has `"session_crons": [{"recurring": false, "prompt": …}]` and the
  transcript's tool result has `toolUseResult.scheduledFor` (epoch ms). When it fires: a
  `system` entry `scheduled_task_fire`, a `UserPromptSubmit` with the wakeup's prompt, a new
  turn. Claude Code then re-arms a loop wakeup of its own (about 20 min later, same prompt,
  no tool call in the transcript), which lingers in `session_crons`.
- `CronCreate` (one-shot): the status file says `idle`; the job is in `session_crons` with the
  id the tool result gave, and leaves the list once it has fired.
- `StopFailure` ends a turn on an API error (`error`, `last_assistant_message`).

The rule: an idle session is waiting, not done, while the status file says `shell`, or the
latest `Stop` lists an unfinished background task that is not a shell (a background agent;
shells are trusted to the status file), or the model's own latest ScheduleWakeup has not fired
and its time is less than 2 minutes past, or a one-shot job the model made with CronCreate is
still in `session_crons`. The loop's re-armed wakeup and recurring jobs do not count. The
status is read before the hooks, and an idle status only counts once the prompt's `Stop` is
in (an interrupted turn, which has no `Stop`, counts after 5 s).

A new directory's workspace-trust dialog is answered yes (its default is "No, exit"), as
`claude -p` never asked. The session's environment drops the markers a parent Claude Code
session sets (`CLAUDECODE`, `CLAUDE_CODE_CHILD_SESSION`, …): inherited, they turn off the
child's transcript and status file. `cost_usd` is the `lastCost` Claude Code records for the
directory at exit (for a resumed session it includes the earlier turns).

## Installing

Copy a pack's contents into a functions directory — the whole pack, including any `_`-prefixed
helper dirs (e.g. `jev/_jev/`, `agents/_agents/`):

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
