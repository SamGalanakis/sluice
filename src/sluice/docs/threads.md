# Threads, the log and watching

Every project has one log: a record for each plan edit, manual value, step status change, call
and thread message, in order, each with a `seq`. Seqs increase across the whole home, so one
log's seqs have gaps: never count on the next one being yours plus one. It is history (the
oldest records are dropped past a cap); `status` and `plan_get` are the current truth.

## Reading the log
- `log_read(project, since_seq?, kinds?, threads?, limit?)` → `{records, last_seq}`. Without
  `since_seq` you get the latest records; with it, the ones after it. Pass `last_seq` back as
  `since_seq` next time.
- `log_wait(project, since_seq, kinds?, threads?, timeout?)` is the same, but waits until at least
  one matching record exists (or `timeout` seconds pass: then `records` is empty). Call it in a
  loop to follow a project.
- `kinds`: `plan.edit`, `plan.input`, `step.output`, `step.retry`, `step.status`, `call`,
  `message`; `step` and `plan` match every kind under them. `threads`: only messages on these
  threads (alone, it means messages only).

A `step.status` record is `{"seq", "at", "kind": "step.status", "step", "from", "to", "error"?}`,
so `log_wait(project, since_seq, kinds=["step.status"])` wakes you when a step finishes, fails
or turns stale.

## Threads
A thread is a named conversation in the project's log: its `message` records. Names use
lowercase letters, digits, `-` and `_`.
- Post: `thread_post(project="myproj", thread="questions", body="Which DB?", to="lead")` →
  `{"seq": 42}`, the message's seq in the log, so it is delivered. `from` defaults to who you
  are (your MCP client's name, or `step:<id>` inside a step); `to` and `data` (any JSON) are
  optional. `needs_reply` (default true) says whether it asks something: set it false for a
  note, a heads-up or a decision already made. The dashboard marks an unanswered question
  "Awaiting reply". A plan step posts with the `thread.post` fn, which writes the same
  record.
- Read or wait: `log_wait("myproj", since_seq=42, threads=["questions"])`. A message is
  `{"seq", "at", "kind": "message", "thread", "from", "to"?, "body", "needs_reply", "data"?}`.
- Wake only on questions: `log_wait(..., wake="questions")` (and `thread.wait`'s `wake`
  input) does not return for a note; notes come back with the next question or record that
  does wake it, or when `timeout` passes, so nothing is lost and nothing wakes you early.
- Thread functions need a project: `fn_call` them with `project`, or use them as plan steps.

In a plan, `thread.wait` blocks a step until a message arrives (`to` keeps only messages
addressed to it or to nobody; `timeout` defaults to 300 s, then `messages` is empty):

```json
{"inputs": {"question": "string"},
 "outputs": {"answer": {"source": "answer/messages"}},
 "steps": {
   "ask":    {"run": "thread.post", "in": {"thread": {"default": "questions"},
              "from": {"default": "plan"}, "to": {"default": "lead"},
              "body": {"source": "question"}}},
   "answer": {"run": "thread.wait", "in": {"thread": {"default": "questions"},
              "since_seq": {"source": "ask/seq"}, "to": {"default": "plan"},
              "timeout": {"default": 3600}}}}}
```

## Talking to a running agent step

The agents pack (`agent.devin`, `agent.codex`, `agent.claude`, `agent.review`, and so
`agent.run`) gives every agent running as a plan step its own thread, `step-<id>`, and
tells it in the spec to check that thread at natural checkpoints and to post questions to
`orchestrator` there, with notes (decisions already made) marked `needs_reply: false` and no
progress reports (pass `listen: false` to a step to leave the section out). A person is never
asked through a thread: when the orchestrator needs one, it posts to the inbox
(`docs("inbox")`).

- To steer a running step, post on its thread with `to` set to the step id:
  `thread_post(project="myproj", thread="step-work", to="work", from="orchestrator",
  body="skip the Windows build")`.
- To read what the step asks back, watch the same thread:
  `log_wait(project="myproj", since_seq=<last>, threads=["step-work"], wake="questions")`,
  or wait on everything you act on with `next(projects, since_seq)`: questions and notes come
  first and whole in every batch, and a settled unit's long outputs are named, not printed
  (`settles="full"` for all of them).
- For every unit at a glance — state, age, engine, each step's mark, what a blocked unit
  waits on, its last message — `status(project, view="units")`: one row per unit with a
  `line` of at most 80 characters (`state="blocked"` or `tags=[...]` to narrow it).

## Watching from a shell
`sluice watch -p myproj [--kinds k1,k2] [--threads a,b] [--since-seq N]` follows the log from now
(or after `--since-seq`) and prints each matching record as one JSON line; it never exits and
needs no server. In Claude Code, run it under the Monitor tool so every record arrives as an
event:

    Monitor("sluice watch -p myproj --kinds step.status,message")

`--threads questions` narrows the messages to that thread.
