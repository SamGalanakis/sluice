# Messages, threads and the log

Every project has one log: a record for each plan edit, manual value, step status change, call
and message, in order, each with a `seq`. Seqs increase across the whole home, so one log's
seqs have gaps: never count on the next one being yours plus one. The log is history (the
oldest records are dropped past a cap); `status` and `plan_get` are the current truth.
Messages also live as rows in the project's `messages` table, which is never trimmed with the
log — a question and its answer outlive the records that announced them.

## Reading the log
- `log_read(project, since_seq?, kinds?, threads?, limit=200)` → `{records, last_seq}`. Without
  `since_seq` you get the latest records; with it, the ones after it. Pass `last_seq` back as
  `since_seq` next time. Without `project` it reads the home's own log (calls run without a
  project). A `since_seq` older than the log keeps, or newer than any seq the home has issued,
  is `cursor_expired`: read again without it.
- `log_wait(project, since_seq, kinds?, threads?, timeout=300, wake="any")` is the same, but
  waits until at least one matching record exists (or `timeout` seconds pass, at most 3600:
  then `records` is empty). Call it in a loop to follow a project.
- `kinds`: `plan.edit`, `plan.input`, `step.output`, `step.retry`, `step.cancel`,
  `step.submit`, `step.status`, `step.lease`, `step.queued`, `call`, `message`,
  `project.pause`, `project.archive`, `project.update`, `project.board`, `project.rename`,
  `project.delete`, `project.capacity`, `project.notify`, `run.adopt`, `run.orphan`, `run.completion_action`,
  `run.completion_action.register`, `unit.settled`; the groups `plan`, `step`, `project`,
  `run` and `unit` match every kind under them. `threads`: only messages on these threads
  (alone, it means messages only).

A record is `{"seq", "at", "project", "kind", ...}`. A `step.status` record adds `"step",
"from", "to", "error", "run_ids", "needs"` (`error` is a structured error object or null),
so `log_wait(project, since_seq, kinds=["step.status"])` wakes you when a step finishes, fails
or turns stale.

## Messages and threads
Three verbs, each with a required recipient; the thread and the sender are derived, never
given:

- `ask(project, to, body, title?, ui?, input?, data?)`: a question that needs a reply.
- `say(project, to, body, data?)`: a note; no reply is expected.
- `reply(project, to_message, body, answer?)`: a reply to that message, sent to its `from`
  on its thread. A reply to an open question answers it (with `answer {"action": "close"}`
  it closes it instead). A reply to a question already answered or closed is just a message;
  one carrying an `answer` is refused (`conflict`). See `docs("inbox")` for `ui`, `answer`
  and the `input` an answer sets.

`to` is a step of the project's current plan, `orchestrator` or `owner`. Anything else (left
out, an unknown or removed step, any other name, yourself) is `invalid` and nothing is
stored. A message to or from a step lives on that step's thread, `step-<step>`; when a
step's run speaks, the thread is its own step's. The orchestrator and the owner talk on the
thread `owner`. You speak as the orchestrator; with `run` (a run id, `SLUICE_RUN_ID` inside
a step) you speak as that run's step; the dashboard speaks as the owner.

Each of the three returns its receipt, `{id, to, thread, delivery, run?}`:

- `delivered`: a live run of the step that listens (its fn takes `listen`, as every agent
  fn does and a fn that runs one should, and the step does not bind `listen: false`) was
  handed it on its live feed (`run` names it); to `orchestrator` or `owner` it is in their
  inbox.
- `queued`: the step will run (it is pending, or its run has not started yet) and its next
  run is assigned it.
- `no_live_run`: the step has no live or upcoming run (it is paused, or its live run does
  not listen). The message is kept and given to the step's next run, if one is ever started.

A step that is settled (succeeded, failed, stale or skipped) takes no messages: an `ask` or
`say` to it, or a reply to a question it asked before it settled, is refused (`conflict`,
saying the step is settled) and nothing is stored. Closing such a question still works. To
give a settled step more work, retry it with a message (below).

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
  thread's rows directly (`{project, messages, last_id}`). `messages(project, "inbox")` is
  your inbox: your open questions, then the notes and replies to you not yet read (pass
  `owner: true` to read the owner's).
- Wake only on questions: `log_wait(..., wake="questions")` (and `message.wait`'s `wake`
  input) does not return for a note or a reply; they come back with the next question or
  record that does wake it, or when `timeout` passes, so nothing is lost and nothing wakes
  you early.
- `next(projects, since_seq)` waits on your behalf across projects (all live projects when
  `projects` is empty): it wakes on a question to you, on a reply that answers a question,
  not written by you, on a step that failed, turned stale or was skipped, on a unit that
  settled, and on a project paused or archived by someone else. Notes ride along in `notes`.
  It returns `{records, notes, last_seq, timed_out}`. Pass `me` your name (default
  `orchestrator`); `since_seq` defaults to 0, so pass the `last_seq` you hold.

In a plan, the fns `message.ask`, `message.say` and `message.reply` take the tools'
arguments and return `{id, receipt}` (`message.ask` also `reply`, below), speaking as the
step. `message.wait` blocks a step until a message lands on a thread (`to` keeps only
messages addressed to it; `timeout` defaults to 300 s, then `messages` is empty):

```json
{"inputs": {"question": "string"},
 "outputs": {"answer": {"source": "answer/messages"}},
 "steps": {
   "ask":    {"run": "message.ask",
              "in": {"to": {"default": "orchestrator"}, "body": {"source": "question"}}},
   "answer": {"run": "message.wait",
              "in": {"thread": {"default": "step-ask"}, "since": {"source": "ask/id"},
                     "to": {"default": "ask"}, "timeout": {"default": 3600}}}}}
```

`message.ask` with `wait: true` blocks until its question is answered instead
(`docs("inbox")`).

## Talking to a running agent step

The agent fns (`agent.devin`, `agent.codex`, `agent.claude`, `agent.review`, and so
`agent.run`) give every agent running as a plan step its own thread, `step-<id>`, and tell it
in the spec how to `ask` the orchestrator a question it cannot settle, `say` something that
needs no answer and `reply` to a question it is asked, each with its run, and to send no
progress reports. Messages to the step reach it live unless the step binds `listen: false`.
A person is never asked through a step's thread: when the orchestrator needs one, it asks
`to="owner"` (`docs("inbox")`).

- To steer a running step, tell it: `say(project="myproj", to="work", body="skip the
  Windows build")`; the receipt says whether its live run got it (`delivered`). Messages to a
  step are delivered to its runs durably: each step keeps a delivery cursor, a run is
  assigned every message after it when it is reserved, and a retried step picks up where the
  last attempt left off.
- To answer what it asks, `reply(project, to_message=<its id>, body=...)`.
- To read what the step asks back, watch its thread:
  `log_wait(project="myproj", since_seq=<last>, threads=["step-work"], wake="questions")`,
  or wait on everything you act on with `next(projects, since_seq)`: questions and notes come
  first and whole in every batch, and a settled unit's long outputs are named, not printed
  (`settles="full"` for all of them).
- To send a step back to fix something: `step_retry(project, steps=["work"],
  message="what to fix")` says the message to the step and runs it again; an agent fn that
  can resume its session sees the message first.

## Watching from a shell
`sluice watch -p myproj [--kinds k1,k2] [--threads a,b] [--since-seq N] [--wake any|questions]`
follows the log from now (or after `--since-seq`) and prints each matching record as one JSON
line; it never exits. `sluice next` is the same wait as the `next` tool, printed one line per
event and ending with `seq N`. In Claude Code, run it under the Monitor tool so every record arrives as an
event:

    Monitor("sluice watch -p myproj --kinds step.status,message")

`--threads step-work` narrows the messages to that thread.
