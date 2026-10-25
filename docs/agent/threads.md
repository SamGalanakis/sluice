# Messages, threads and the log

Every project has one log: a record for each plan edit, manual value, step status change, call
and message, in order, each with a `seq`. Seqs increase across the whole home, so one log's
seqs have gaps: never count on the next one being yours plus one. The log is history (the
oldest records are dropped past a cap); `status` and `plan_get` are the current truth.
Messages also live as rows in the project's `messages` table, which is never trimmed with the
log — a question and its answer outlive the records that announced them.

## Reading the log
- `log_read(project, since_seq?, kinds?, threads?, statuses?, recipients?, limit=200)` →
  `{records, last_seq}`. Without
  `since_seq` you get the latest records; with it, the ones after it. Pass `last_seq` back as
  `since_seq` next time. Without `project` it reads the home's own log (calls run without a
  project). A `since_seq` older than the log keeps, or newer than any seq the home has issued,
  is `cursor_expired`: read again without it.
- `log_wait(project, since_seq, kinds?, threads?, statuses?, recipients?, timeout=300,
  wake="any")` is the same, but waits until at least one matching record exists (or `timeout`
  seconds pass, at most 3600: then `records` is empty). Records that do not match never wake
  it. Call it in a loop to follow a project.
- `kinds`: `plan.edit`, `plan.input`, `step.output`, `step.retry`, `step.cancel`,
  `step.submit`, `step.status`, `step.lease`, `step.queued`, `call`, `message`,
  `project.pause`, `project.archive`, `project.update`, `project.board`, `project.rename`,
  `project.delete`, `project.capacity`, `project.notify`, `run.adopt`, `run.orphan`, `run.completion_action`,
  `run.completion_action.register`, `unit.settled`; the groups `plan`, `step`, `project`,
  `run` and `unit` match every kind under them. `threads`: only messages on these threads
  (alone, it means messages only).
- `statuses`: only `step.status` records whose `to` is one of these statuses. `recipients`:
  only messages whose `to` is one of these (step ids, `@session` names, `owner`). Each narrows
  its own kind and lets every other kind through, so pair them with `kinds`. One standing wait
  that wakes only on failures, stale steps and messages to you, session `@lead`:
  `log_wait(project, since_seq, kinds=["step.status", "message"], statuses=["failed",
  "stale"], recipients=["@lead"])`.

A record is `{"seq", "at", "project", "kind", ...}`. A `step.status` record adds `"step",
"from", "to", "error", "run_ids", "needs"` (`error` is a structured error object or null),
so `log_wait(project, since_seq, kinds=["step.status"])` wakes you when a step finishes, fails
or turns stale.

## Waiting on steps
`step_wait(project, steps? | tags?, until, timeout=300)` blocks until every selected step
meets `until`, and returns `{met, steps: {id: status}, seq}`:

- `until`: `"succeeded"`; `"settled"`: nothing more happens to the step without an edit, a
  retry, an input or an output set by hand (it succeeded, failed, turned stale or was skipped,
  or it is pending and held: paused, a `core.external` step waiting for its outputs, or
  waiting only on such steps or on a plan input with no value); or `{"any_of": ["failed",
  "stale"]}`, each step in one of those statuses.
- Give `steps` (ids) or `tags` (`unit:<name>` selects a unit), not both. A unit whose steps
  are all settled is the settled unit `next` reports and `unit_get` marks `settled`.
- It answers at once when the condition already holds; otherwise when a status change makes
  it hold, or with `met: false` after `timeout` seconds (at most 3600; 0 checks once).
- `seq` is the project log's last seq at that answer: `log_read(project, since_seq=seq)`
  shows what happened after it. A step id the plan lacks, a tag no step carries or a bad
  `until` is `invalid`, each named in `errors`.

```
step_wait(project="myproj", tags=["unit:lane-3"], until="settled", timeout=1800)
→ {"met": true, "steps": {"lane-3-work": "succeeded", "lane-3-land": "failed"}, "seq": 812}
```

## Wait, don't poll
Never sleep and re-check `status`, `plan_get` or `log_read` in a loop, and never write your
own watcher: hold one wait instead. To wait for steps (a sibling lane, a merge to main, a
unit), call `step_wait`; to wait for events (failures, messages to you), call `log_wait` with
`kinds`, `statuses` and `recipients`; to wait for everything an orchestrator acts on, call
`next`. Each answers the moment its condition holds and costs nothing while it waits; on a
timeout, call it again with what it returned.

At a shell, `sluice say`, `sluice ask` and `sluice reply` are aliases of
`sluice tool say`, `sluice tool ask` and `sluice tool reply`. They accept the same JSON,
flags, `--body-file` and run defaults; for example, `sluice say --body "blocked on schema"`
speaks to your unit's owner from a run.

## Who you are
Every session names itself before it speaks: `@<name>`, a lowercase letter, then lowercase
letters, digits or `-` (at most 40), for example `@lash-f4`. Pass `me="@lash-f4"` on your first
MCP call: that binds your session and every later call defaults to it (a different `me` on
the same session is refused). For HTTP (`POST /api/tools/<name>`), give `me` on every request
that needs your name. Each call is independent: omitting `me` leaves it unnamed, and no call
can bind another caller's identity. From a shell, set `SLUICE_ME=@lash-f4` (or pass `--me`). The
name is your identity, not your connection: a successor that reuses it carries on with your
units and your messages.

- Without a name you may read anything, but `ask`, `say` and `reply` are refused: "name this
  session: pass me=@name or set SLUICE_ME".
- `owner` is the person at the dashboard; only the dashboard speaks as it. `orchestrator` is
  no longer a name or an address: messages from before sessions named themselves keep it.
- A worker run speaks as its step; the coordinator's own alerts come from `sluice`.
- Your writes default to your name as author, so the plan's history says which session did
  what. An unnamed HTTP edit uses the ordinary author default, without inheriting a name.

**Owners.** Every unit has an owner, who answers its workers: an `@session`, `owner` (the
person), or nobody. A unit you add is yours: `step_add`, `unit_add` and `plan_edit` with your
`me` make you its owner (`owner="@peer"` or `owner="owner"` names another; see
`docs("plans")`). A step added to an existing unit keeps that unit's owner. The project's
`fallback` answers unowned units: `project_update(project, fallback="@lash-f4")`; with no
fallback, a worker of an unowned unit asks the person.

**Starting.** Name yourself and set the fallback of a project you drive if it has none
(`project_update(project, fallback="@lash-f4")`). Create one durable subscription, then wait
on its name. From a shell:

```sh
export SLUICE_ME=@lash-f4
sluice tool subscription_set --name lanes --project lash \
  --wake ask,reply,failed,stale,settled,limit,quota,say-to-me --digest 30m
sluice next --subscription lanes --timeout 3600
```

Call `next --subscription lanes` again after acting on each batch; after a compaction or
client restart, use that same name. Its projects, identity, filter and cursor are stored by
the coordinator; no grep, cursor file or watcher rebuild is needed. With MCP, use
`subscription_set(name="lanes", projects=["lash"], me="@lash-f4", wake=["ask", "reply",
"failed", "stale", "settled", "limit", "quota", "say-to-me"], digest="30m")`, then
`next(subscription="lanes", timeout=3600)`.

The reader's `mine` scope applies: its units' failures and settlements, questions and replies
to it, and selected notes to it. `say_from_steps="asks-only"` (the default) suppresses
unsolicited step notes, including queued and still-waiting updates. To request an update,
ask the step a question: its says are eligible while that question remains open.
`"none"` suppresses every step say; `"all"` allows them. Ask and reply wake kinds always
work independently. `limit` selects execution-limit notices and resource exhaustion;
`quota` selects quota-exhausted step failures.

A matching event wakes at once and returns every pending match, one line each, followed by a
digest counting suppressed events by kind. `--json` gives record lines plus
`{digest, seq, timed_out}`. Optional `digest="30m"` returns a digest-only batch after 30 minutes
of suppressed events; asks never wait for that timer. Without a digest interval, counts ride
with the next matching event or timeout. `subscription_get(name)`, `subscription_list()` and
`subscription_remove(name)` inspect and remove subscriptions. Setting an existing name
updates its filter without rewinding its cursor; only creation starts from now. Use one
reader per name. If the bounded log was trimmed while it was away, `cursor_expired` asks you
to remove and recreate the subscription to start from now.

**Other waits.** Unstored `next` still supports `scope="all"`, `units=["pe-*"]` and
`tags=["arc:x"]`, and `log_wait` takes the same `me`, `scope`, `units` and `tags`. Each named
session's unstored `next` keeps its own position; passing `since_seq` or CLI `--cursor FILE`
reads from there instead. Use a stored subscription for the standing orchestrator watcher.

**Handing off and taking over.** Ownership moves only by these two calls, each one write, all
or none, and neither is plan work (no `rev`; allowed during a drain):

- `unit_claim(project, unit | units | tags | owned_by, expected_owner?, force?, reason?)` makes
  you the owner of every unit selected (`owned_by="@x"` keeps the units `@x` holds now; alone,
  all of them). A unit is taken when it is unowned or its owner has not waited (`next` or
  `log_wait`) in the last 10 minutes. A live owner's unit is a `conflict` naming it and its last
  heartbeat: agree with it first, then take it with `expected_owner="@it"`, a compare-and-set
  that refuses if the owner changed meanwhile. A unit the person holds needs `force`.
- `unit_release(project, unit | units | tags | owned_by, to?, reason?)` hands units you own to
  `to` (an `@session` registered in the project, or `owner`), or, without `to`, leaves them
  unowned, so the fallback answers them.

Either way the units' open questions to whoever answered them before (and those sent to
`orchestrator` before sessions named themselves) are re-addressed to whoever answers them now,
listed in the reply's `questions`, and `sluice` leaves a note saying who moved what to the
party that lost (on a claim) or gained (on a release) the units, its `data.unit_owner`
`{by, reason, units: [{unit, from, to}], questions}`. Later questions route to the new owner.

To take over a session that stopped (its units, its questions), select by who holds them, which
is also the compare-and-set (a unit it no longer holds is left alone):

```
unit_claim(project="lash", owned_by="@lash-f4", reason="lash-f4 stopped")
→ {"units": [{"unit": "pe-a", "from": "@lash-f4", "to": "@lash-f5"}, ...],
   "questions": [{"id": 312458, "from": "pe-a-work", "to": "@lash-f5", ...}], ...}
```

A successor that reuses the stopped session's name needs no claim: it is that session.

## Messages and threads
Three verbs; the thread and the sender are derived, never given, and the recipient is checked
and stored as resolved:

- `ask(project, to, body, title?, ui?, input?, data?)`: a question that needs a reply.
- `say(project, to, body, data?)`: a note; no reply is expected.
- `reply(project, to_message?, to?, body, answer?)`: a reply to that message, sent to its `from`
  on its thread. A reply to an open question answers it (with `answer {"action": "close"}`
  it closes it instead). A reply to a question already answered or closed is just a message;
  one carrying an `answer` is refused (`conflict`). See `docs("inbox")` for `ui`, `answer`
  and the `input` an answer sets.
  Give exactly one of `to_message` or `to`. With `to="work"`, answer `work`'s single open
  question addressed to you. No match is a `conflict` saying so; several matches are a
  `conflict` listing their ids and first body lines, cut to 200 characters. Choose an id to
  disambiguate. In a run, "you" is that run's step; otherwise it is your session (questions
  sent to `orchestrator` before sessions named themselves count as yours too).

From a shell, `sluice tool` takes the same arguments as one JSON object or as flags, a
message's body from a file or stdin so it needs no quoting (`sluice tool reply --help` lists
the fields). Inside a step's run, `project` and `run` default to the run's own:

```sh
export SLUICE_ME=@lash-f4
sluice tool ask --project myproj --to work --body 'Which schema version?'
sluice tool reply --project myproj --to-message 24771 --body-file - <<'END'
Use v3; it's on the "parser" branch.
END
sluice tool say --body-file note.md      # in a step's run: to its unit's owner
```

`to` is a step of the project's current plan, a session (`@name`) registered in the project,
or `owner`. A session registers in a project with its first message, edit or `next` there, so
a misspelt `@name` is refused rather than lost. You must name your recipient; anything else
(left out, `orchestrator`, an unknown or removed step, an unknown session, yourself) is
`invalid` and nothing is stored. A run may leave `to` out: its question or note goes to its
unit's owner, else the project's fallback, else the person; a run of an older release that
still says `to: "orchestrator"` reaches the same owner.

A message to or from a step lives on that step's thread, `step-<step>`; when a step's run
speaks, the thread is its own step's. A session and the owner talk on the session's thread,
named as the session (`@lash-f4`); a message to another session lives on that session's
thread. You speak as your session; with `run` (a run id, `SLUICE_RUN_ID` inside a step) you
speak as that run's step; the dashboard speaks as the owner.

Each of the three returns its receipt, `{id, to, thread, delivery, run?, note?}` (`to` as
resolved):

- `delivered`: a live run of the step that listens (its fn takes `listen`, as every agent
  fn does and a fn that runs one should, and the step does not bind `listen: false`) was
  handed it on its live feed (`run` names it); to a session or `owner` it is in their
  inbox. A session that has not waited (`next`) in the last 10 minutes still gets it, and the
  receipt says so: `note: "queued; @peer has no live reader"`.
- `queued`: the step will run (it is pending, or its run has not started yet) and its next
  run is assigned it.
- `no_live_run`: the step has no live or upcoming run (it is paused, or its live run does
  not listen). The message is kept and given to the step's next run, if one is ever started.

A step that succeeded, failed, turned stale or was skipped, or whose runs have all
submitted (their agents' sessions are over), takes no messages: an `ask` or `say` to it, or a
reply to a question it asked earlier, is refused (`conflict`, saying why) and nothing is
stored. Closing such a question still works. To
give such a step more work, retry it with a message (below).

A message is a row `{id, verb, from, to, thread, body, title?, ui?, input?, data?, run?, at,
to_message?, answer?}`: `verb` is `ask`, `say` or `reply`; `to_message` and `answer` are a
reply's; fields with no value are left out. A question also carries `state`: `open`,
`answered` (with `answered_by`, the answering message's id) or `closed`; a question with an
empty body shows its plan input's doc. Its `message` log record has the same fields, with
`at` renamed `posted_at` (the record's own `at` is when the record was written) and never a
`state`. Ids come from the same sequence as log seqs. Messages stored before the verbs read
in this shape too, with their verb derived (a reply if they replied, else a question if they
needed a reply, else a note).

- Read or wait: `log_wait("myproj", since_seq=42, threads=["step-work"])`. A message record
  is `{"seq", "at", "project", "kind": "message", "id", "verb", "from", "to", "thread",
  "body", ..., "posted_at"}`; `messages(project, "thread", thread="step-work")` reads a
  thread's rows directly (`{project, messages, last_id}`). `messages(project)` defaults to
  `view="inbox"` and reads your inbox: your open questions (with those sent to `orchestrator`
  before sessions named themselves), then the notes and replies to you not yet read (pass
  `owner: true` to read the owner's).
- Wake only on questions: `log_wait(..., wake="questions")` (and `message.wait`'s `wake`
  input) does not return for a note or a reply; they come back with the next question or
  record that does wake it, or when `timeout` passes, so nothing is lost and nothing wakes
  you early.
- `next(projects, since_seq?)` waits on your behalf across projects (all live projects when
  `projects` is empty), in your scope (above): it wakes on a question to you, on a reply that
  answers a question, not written by you, on a step that failed, turned stale or was skipped,
  on a unit that settled, on a project paused or archived by someone else, on a new
  fallback, and on a non-routine execution-limit notice to you: a `say` from `sluice` whose
  `data.execution_limit` is `{run, step, limit, value, observed, outcome, since?}` (a run's
  failed forks, memory at its cap or killed, CPU throttling, or a step held until the host
  has memory free; `docs("plans")`, Execution limits). The step's own run gets the same note in its
  inbox. A unit settles when every one
  of its steps is settled as `step_wait`'s `"settled"` means: one whose next step waits on
  work still running, in it or in another unit, has not. It settles again each time it comes
  to rest differently (retried, its pause or a value acted on, then finished). Notes ride
  along in `notes`.
  It returns `{records, notes, last_seq, timed_out}`. With your name (`me`, or your session's
  binding, or `SLUICE_ME` for `sluice next`) it wakes on questions to you; without one, or with
  `scope="all"`, on a question to any session. Without `since_seq` it resumes after your last
  batch (unnamed: from now). Each wait (and each `log_wait`) also marks you as reading, which
  senders' receipts and `unit_claim` reflect.

In a plan, the fns `message.ask`, `message.say` and `message.reply` take the tools'
arguments and return `{id, receipt}` (`message.ask` also `reply`, below), speaking as the
step; left without `to`, they reach the unit's owner. The `message.reply` fn selects by `to_message`; sender selection is for the `reply` tool.
`message.wait` blocks a step until a message lands on a thread (`to` keeps only messages
addressed to it; `timeout` defaults to 300 s, then `messages` is empty):

```json
{"inputs": {"question": "string"},
 "outputs": {"answer": {"source": "answer/messages"}},
 "steps": {
   "ask":    {"run": "message.ask",
              "in": {"body": {"source": "question"}}},
   "answer": {"run": "message.wait",
              "in": {"thread": {"default": "step-ask"}, "since": {"source": "ask/id"},
                     "to": {"default": "ask"}, "timeout": {"default": 3600}}}}}
```

`message.ask` with `wait: true` blocks until its question is answered instead
(`docs("inbox")`).

## Talking to a running agent step

The agent fns (`agent.devin`, `agent.codex`, `agent.claude`, `agent.review`, and so
`agent.run`) give every agent running as a plan step its own thread, `step-<id>`, and tell it
in the spec how to `ask` a question it cannot settle, `say` something that needs no answer
(both to its unit's owner: you, for a unit you added) and `reply` to a question it is asked,
each with its run, and to send no progress reports. Messages to the step reach it live unless
the step binds `listen: false`. A person is never asked through a step's thread: when you need
one, ask `to="owner"` (`docs("inbox")`).

- To steer a running step, tell it: `say(project="myproj", to="work", body="skip the
  Windows build")`; `delivered` means the listening run has it on its live feed. The engine
  acknowledges it separately: Claude can consume busy-turn input through a queued-command
  attachment without starting another turn. A steered input left unacknowledged after that
  turn ends fails the idle run within 60 s as `TurnStartTimeout`, naming its input id; only verified
  nonacceptance permits one redelivery. Each step keeps a durable delivery cursor: a run is
  assigned every message after it when it is reserved, and a retried step picks up where the
  last attempt left off.
- To answer what it asks, `reply(project, to="work", body=...)`, or use its id with
  `to_message=<id>`. From a shell, `sluice tool reply --project myproj --to work --body 'Use the Rust parser'`.
- To read what the step asks back, watch its thread:
  `log_wait(project="myproj", since_seq=<last>, threads=["step-work"], wake="questions")`,
  or wait on everything you act on with `next(projects, since_seq)`: questions and notes come
  first and whole in every batch, and a settled unit's long outputs are named, not printed
  (`settles="full"` for all of them).
- To send a step back to fix something: `step_retry(project, steps=["work"],
  message="what to fix")` says the message to the step and runs it again; an agent fn that
  can resume its session sees the message first.

## Correcting a running step

`say(project, to="work", body="what to change")` reaches a listening agent live.
For a `{"file": path}` input, edit the file too: each start re-reads it.
To change a typed input, `step_cancel`, wait until the step fails, then `step_set_input`
and `step_retry(message="what to fix")`. Changing `in` mid-run would change the inputs
hash and make the corrected result stale.

## Watching from a shell
`sluice watch -p myproj [--kinds k1,k2] [--threads a,b] [--since-seq N] [--wake any|questions]`
follows the log from now (or after `--since-seq`) and prints each matching record as one JSON
line; it never exits. `sluice next` is the same wait as the `next` tool, printed one entry per
event and ending, after a blank line, with `seq N`. Message entries have the form
`ASK|SAY|REPLY #<message-id> <thread> <from> -> <to>: <title or body>`, the rest of the body on
the lines after, indented: the second token always names the message, so an ASK can be
answered directly with `--to-message <message-id>`. A body is printed whole up to 4,000
characters; a longer one shows its first paragraph and each paragraph that asks ("Question:",
"Q:", or ending in "?"), whole, and how to read the rest. A settled unit reads `UNIT <u> settled
· <outcome>: <step> <status> · …`, the outcome first (`succeeded`, `failed`, `cancelled`,
`stale`, or counts when mixed: `1 failed, 1 cancelled`), so `UNIT x settled · failed: work
failed · rm pending (held)` never reads as a success.
Reply with the `#N` on the ASK line; the batch's final `seq N` is the cursor for `--since-seq`.
`log_wait` and JSON `next` carry `id` too. The last-message summary in `status(view="units")`
begins with `#<message-id>`; the rendered line includes it when it shows that message.
In Claude Code, run it under the Monitor tool so every record arrives as an event:

    Monitor("sluice watch -p myproj --kinds step.status,message")

`--threads step-work` narrows the messages to that thread.

## Routine events in next

A session's `next` and stored subscriptions omit these events by default:

- Your own bypass confirmations and cancel requests.
- Capacity, memory and CPU hold/wait notices, and CPU throttling notices.
- Memory-cap warnings that killed nothing.
- Engine probe observations.
- Rebase-conflict handbacks and `awaiting slice` not-ready handbacks whose registered
  completion action actually retried the work step. An unresolved failure still comes through.

Other failures, questions, notes and replies to you, unit settlements and run check notes
keep coming. Notes addressed to your named session now wake ordinary `next` too. Check notes
also wake stored subscriptions even when their `wake` list omits `say-to-me`.

`include_routine=true` restores routine events under the usual scope and wake selection,
even with `all=true`. On a stored subscription, set it with `subscription_set` for its default,
or give it to `next(subscription="lanes", include_routine=true)` for one batch. At the shell,
use `sluice next --include-routine`. Filtered records advance the cursor; they remain in
`log_read`, `log_wait` and `watch`. A subscription counts them in its suppressed digest.
No grep wrapper is needed.
