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
  `project.pause`, `project.archive`, `project.update`, `project.rename`, `project.delete`,
  `project.capacity`, `project.notify`, `run.adopt`, `run.orphan`, `run.completion_action`,
  `run.completion_action.register`, `unit.settled`; the groups `plan`, `step`, `project`,
  `run` and `unit` match every kind under them. `threads`: only messages on these threads
  (alone, it means messages only).

A record is `{"seq", "at", "project", "kind", ...}`. A `step.status` record adds `"step",
"from", "to", "error", "run_ids", "needs"` (`error` is a structured error object or null),
so `log_wait(project, since_seq, kinds=["step.status"])` wakes you when a step finishes, fails
or turns stale.

## Messages and threads
A message is a row `{id, thread, from, to, title, body, needs_reply, reply_to, answer, ui,
input, data, run, at, claimed_by}` plus its `message` log record, which carries the same fields
with `at` renamed `posted_at` (the record's own `at` is when the record was written). Ids come from the same sequence
as log seqs. A thread is a named conversation in the project; names use lowercase letters,
digits, `-` and `_`.

- Post: `message_post(project="myproj", thread="questions", body="Which DB?", to="lead")` →
  `{id}`. `from` defaults to who you are (your MCP client's name, or `step:<id>` inside a
  step); `to` is a step id, `orchestrator`, `owner`, or absent (anyone). `needs_reply` (true
  on a new thread) makes it a question; set it false for a note, a heads-up or a decision
  already made. `data` is an optional structured payload. A post with no `thread` and no
  `reply_to` goes on `step-<step>` when it comes from a step's run, else starts a thread named
  `m<id>`.
- Reply: `message_post(project, reply_to=<id>, body=...)` joins the parent's thread, `to`
  defaults to the parent's `from`, and `needs_reply` defaults to false. `reply_to` and `body`
  are all it needs. See `docs("inbox")` for questions to the person, `ui` and `answer`, and
  the `input` that sets a plan input.
- Read or wait: `log_wait("myproj", since_seq=42, threads=["questions"])`. A message record is
  `{"seq", "at", "project", "kind": "message", "id", "thread", "from", "to", "body",
  "needs_reply", ..., "posted_at"}`; `messages(project, "thread", thread="questions")` reads a
  thread's rows directly (`{project, messages, last_id}`).
- Wake only on questions: `log_wait(..., wake="questions")` (and `message.wait`'s `wake`
  input) does not return for a note; notes come back with the next question or record that
  does wake it, or when `timeout` passes, so nothing is lost and nothing wakes you early.
- `next(projects, since_seq)` waits on your behalf across projects (all live projects when
  `projects` is empty): it wakes on a question addressed to you or to nobody, on an answering
  reply you did not write, on a step that failed, turned stale or was skipped, on a unit that
  settled, and on a project paused or archived by someone else. Notes ride along in `notes`.
  It returns `{records, notes, last_seq, timed_out}`. Pass `me` your name (the same `from` you
  post with, default `orchestrator`); `since_seq` defaults to 0, so pass the `last_seq` you
  hold.

In a plan, `message.wait` blocks a step until a message arrives (`to` keeps only messages
addressed to it or to nobody; `timeout` defaults to 300 s, then `messages` is empty):

```json
{"inputs": {"question": "string"},
 "outputs": {"answer": {"source": "answer/messages"}},
 "steps": {
   "ask":    {"run": "message.post",
              "in": {"thread": {"default": "questions"}, "from": {"default": "plan"},
                     "to": {"default": "lead"}, "body": {"source": "question"}}},
   "answer": {"run": "message.wait",
              "in": {"thread": {"default": "questions"}, "since": {"source": "ask/id"},
                     "to": {"default": "plan"}, "timeout": {"default": 3600}}}}}
```

## Talking to a running agent step

The agent fns (`agent.devin`, `agent.codex`, `agent.claude`, `agent.review`, and so
`agent.run`) give every agent running as a plan step its own thread, `step-<id>`, and tell it
in the spec to check that thread at natural checkpoints and to post questions to
`orchestrator` there, with notes (decisions already made) marked `needs_reply: false` and no
progress reports (pass `listen: false` to a step to leave the section out). A person is never
asked through a thread: when the orchestrator needs one, it posts a question `to="owner"`
(`docs("inbox")`).

- To steer a running step, post on its thread with `to` set to the step id:
  `message_post(project="myproj", thread="step-work", to="work", from="orchestrator",
  body="skip the Windows build")`. A post on `step-<id>` without `to` (not a reply) is
  addressed to that step when it is in the current plan. Messages to a step are delivered to its run durably: each
  step keeps a delivery cursor, a run is assigned every message after it when it is reserved,
  and a retried step picks up where the last attempt left off.
- To read what the step asks back, watch the same thread:
  `log_wait(project="myproj", since_seq=<last>, threads=["step-work"], wake="questions")`,
  or wait on everything you act on with `next(projects, since_seq)`: questions and notes come
  first and whole in every batch, and a settled unit's long outputs are named, not printed
  (`settles="full"` for all of them).
- To send a step back to fix something: `step_retry(project, steps=["work"],
  message="what to fix")` posts the message and runs the step again; an agent fn that can
  resume its session sees the message first.

## Watching from a shell
`sluice watch -p myproj [--kinds k1,k2] [--threads a,b] [--since-seq N] [--wake any|questions]`
follows the log from now (or after `--since-seq`) and prints each matching record as one JSON
line; it never exits. `sluice next` is the same wait as the `next` tool, printed one line per
event and ending with `seq N`. In Claude Code, run it under the Monitor tool so every record arrives as an
event:

    Monitor("sluice watch -p myproj --kinds step.status,message")

`--threads questions` narrows the messages to that thread.
