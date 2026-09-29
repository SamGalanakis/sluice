# First-party packs

Optional function packs for sluice. They are **not loaded by default** — install the ones a
home or project actually needs.

| Pack | Functions |
|---|---|
| `agents/` | `agent.devin`, `agent.codex`, `agent.claude`, `agent.run`, `agent.review`, `decide.llm` — run Devin/Codex/Claude agents and LLM decisions |
| `git/` | `git.worktree`, `git.worktree_rm`, `git.head`, `git.merge`, `git.rebase`, `git.push`, `gh.pr`, `gh.pr_wait`, `gh.run_latest`, `gh.run_cancel` — worktrees, merges, rebases, pushes, pull requests, workflow runs |
| `jev/` | `jev.ask`, `jev.choice`, `jev.score`, `jev.noul` — TypeSafe's System One model (Jev); needs `TYPESAFE_API_KEY` |

Each `fn.json` is the reference for that function's typed inputs and outputs.

## Agent functions and live sessions

The agent functions (`agent.claude`, `agent.codex`, `agent.devin`, `agent.run`,
`agent.review`) are **open**: a plan step running one may bind extra inputs and declare
outputs (`docs("plans")`). As a plan step, each appends to its task text an `## Inputs`
section (every extra input with its type and value), an `## Outputs you must submit` section
(every declared output with its type and doc, and the exact `sluice tool step_submit` command
to submit them) and the step-thread note (`listen: false` drops only the note). The step fails
if the agent finishes without submitting a required output.

Claude always runs Opus. Codex takes `model` `sol` (default) or `astra`, and `effort`
(`minimal`, `low`, `medium`, `high`, `xhigh`, `max`); left out, effort is `high`. Devin
takes `model` `swe-2-high` (default, alias `high`) or `fusion` — Fusion (Claude Opus 5.5
High + SWE-2 Medium), whose full id `fusion-claude-opus-5-5-high-sidekick-swe-2-medium`
is also accepted. `effort` stays codex-only. The allowlist is deliberate: each accepted
model is a cost the owner opted into.
Projects choose which engine runs each kind of work.

Every agent function takes `session?: string` and returns `session: string` (the engine's
session or thread id; empty when there is none). Run in a git worktree, each also returns
`git`: `{head_before, head_after, commits, dirty}`. `head_before` is HEAD when the run
started, read once per run and kept in `<run_dir>/native.json`, so a Transient retry does not
reset it; `commits` counts `head_before..head_after`, i.e. how far HEAD moved (a lane that
pulls before it pushes counts the upstream commits it pulled); `dirty` says tracked changes
were left uncommitted. Outside a git worktree `git` is absent. `agent.review`'s `sha` and
`commits` are `head_after` and `commits`.
A follow-up to a particular agent is another step with `session` bound to the earlier step's
`session` output, or a `fn_call` with that session.

The agents pack is a starting point: which engine runs which kind of work is up to each
project. To route, copy (or wrap) `agent.run` into the project's own `fns/` under a name of
its own and pick the engines there.

Claude, Codex and Devin run as real interactive sessions on your own logins through `_agents/native/`.
`agent.claude`, `agent.review`, and `agent.run` with engine `claude` use the Claude adapter.
`agent.codex` and `agent.run` with engine `codex` use the Codex adapter.
`agent.devin` and `agent.run` with engine `devin` use the Devin adapter. Per run:

- A private tmux server on `<run_dir>/tmux.sock` runs the session. The step's stderr starts
  with an `attach:` line (`cd <run_dir> && tmux -S tmux.sock attach`): run it to watch or
  steer the worker live.
- A long task goes to `<run_dir>/task.md` and the session gets one line pointing at it. Claude
  and Devin receive that line in their composers; Codex receives it through app-server JSON-RPC.
- The wrapper, not the model's end of turn, decides when the step is done. When a turn ends:
  every required declared output submitted → done; the session still waits on its own
  background work (below) → it keeps waiting; otherwise it is nudged ("Your turn ended but
  these outputs are not submitted: …"), up to `SLUICE_AGENT_NUDGES` (3) times, then the step
  fails naming the missing outputs and the agent's last message. A step that declares no
  required outputs is done once the session is idle with nothing pending, after the grace
  period for an engine that cannot report pending work.
- While the session is busy, the supervisor samples the git worktree every few minutes (HEAD,
  `git status`, and the size and mtime of each changed or untracked file). After
  `SLUICE_AGENT_QUIET_MIN` (45) minutes busy with no change it posts one note to the
  orchestrator on the step's thread (`needs_reply: false`): "fix-x: busy 47 min with no change
  to the worktree (HEAD abc1234, no diff)", and another only after a further quiet period. It
  never steers, stops or restarts the session over it. Outside a git worktree it does nothing.
- Messages addressed to the step on its thread (`thread.post` to `step-<id>`, to the step
  or to nobody) are delivered to the live session as they arrive. Messages addressed to
  another recipient are not forwarded. The step-thread note no
  longer asks the agent to poll `log_read`. `listen: false` turns this off.
- Caps: `SLUICE_AGENT_MAX_MIN` (600) minutes of wall clock; `SLUICE_AGENT_STALL_MIN` (30)
  minutes in any non-idle state with no transcript growth. A delivered message must start
  a turn within `SLUICE_AGENT_TURN_START_S` (60) seconds; it is delivered once more, then
  fails if the turn still does not start. A Claude dialog open for 60 seconds is dismissed
  and nudged; an idle session waiting on background work is nudged after
  `SLUICE_AGENT_WAIT_MIN` (90) minutes. `SLUICE_AGENT_SETTLE_S` (10) seconds of idle before
  a nudge. `SLUICE_AGENT_GRACE_MIN` (10) minutes of idle before the first nudge, for an engine
  with no waiting signal (Codex and Devin have none; Claude has one).
  `SLUICE_AGENT_POLL_S` (0.5) sets the state-check interval.
- `session` resumes the session, and only from the directory it was started in; another cwd
  fails the step before anything starts. Codex keeps its private `CODEX_HOME` per thread and
  a registry at `<SLUICE_HOME>/codex-native-sessions/`, with homes under
  `<SLUICE_HOME>/codex-native-homes/<thread>`. A rate limit or
  capacity error raises `Transient`; the retry resumes the session and tells it to continue.
- The engine's environment has `GIT_TERMINAL_PROMPT=0`, `GIT_EDITOR=true` and
  `GIT_MERGE_AUTOEDIT=no`: nobody can answer a git prompt in a supervised session.
- `step_cancel` (SIGTERM) ends the tmux server, the engine and every process it started,
  background shells included. The run dir records the tmux server, pane engine and app-server
  pids with `/proc` start times; the runner finds their children and reaps them if the fn is
  SIGKILLed before its cleanup runs.

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

### Codex: idle is not done

The Codex adapter starts `codex app-server` on a private Unix WebSocket, then attaches a Codex
TUI in the run's tmux server with `--remote`. It sends the task, nudges and thread messages as
JSON-RPC `turn/start` or `turn/steer` calls. The app-server's `item/completed` and
`turn/completed` notifications supply progress, the final message and the turn boundary.
Resume uses `thread/resume` and the same private thread home. The private config pins the
selected model and effort, disables each configured MCP server, and retains the owner's login.
`SLUICE_CODEX_SEARCH=1` enables live web search. A fork's `env.sh` supplies its Cargo target.

Observed on Codex CLI 0.158.0: `exec_command` ran `sleep 20 && echo finished >
background-3.txt` with a running session id; the model ended its turn at 7.7 s. The
app-server reported `turn/completed` and stayed idle with no pending-work field. The file
appeared at 25.2 s, but there was no new turn or notification by 42 s. A separate `nohup`
background command also ended in idle and did not complete after its shell returned. Codex
therefore has no reliable pending-work signal or autonomous wakeup for this case. The
supervisor waits `SLUICE_AGENT_GRACE_MIN` (10 minutes by default) before its first nudge; the
worker should wait for background work within its turn when it needs the result. Subsequent
nudges use `SLUICE_AGENT_SETTLE_S`.

### Devin: idle is not done

The Devin adapter runs `devin` in the private tmux server with `--model` naming the
chosen model (`swe-2-high` unless the step's `model` input picks `fusion`),
`--permission-mode dangerous`, `--respect-workspace-trust false`, `--export` and a per-run
`--config`. That config retains the owner's settings and hooks, pins `agent.model` to the
chosen model, then adds lifecycle hooks
which append to `<run_dir>/hooks.jsonl`. `SessionStart` supplies the session id;
`UserPromptSubmit` marks a busy turn; `PreToolUse` and `PostToolUse` supply progress;
`Stop` supplies its final message and turn end. The export is saved as `<log>.json`, with
`<log>.final` and `<log>.session` beside it. The pack does not write Devin config into the
repository. `--resume` continues a session in its original cwd.

Observed on Devin CLI 3000.11.3: a turn started `sleep 25 && echo finished >
idle-marker.txt` as a background shell and replied `LAUNCHED`. `Stop` arrived at 11.3 s,
and the session stayed idle. The file appeared at 30.7 s. Through 65 s there was no new
hook event or autonomous turn. The `Stop` payload had no pending-work field. Devin therefore
has no reliable waiting signal for this case. The supervisor waits
`SLUICE_AGENT_GRACE_MIN` (10 minutes by default) after the first idle before its first
nudge or completion when no required output is due. Workers should wait within their turn
when they need a background result. Later nudges use `SLUICE_AGENT_SETTLE_S`.

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
