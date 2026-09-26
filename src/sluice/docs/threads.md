# Threads, the log and watching

Every project has one log: a record for each plan edit, manual value, step status change, call
and thread message, in order, each with a `seq`. It is history (the oldest records are dropped
past a cap); `status` and `plan_get` are the current truth.

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
- Post: `fn_call("thread.post", {"thread": "questions", "from": "worker-1", "body": "Which DB?",
  "to": "lead"}, project="myproj", wait=10)` → `{"seq": 42}`. `to` and `data` (any JSON) are
  optional.
- Read or wait: `log_wait("myproj", since_seq=42, threads=["questions"])`. A message is
  `{"seq", "at", "kind": "message", "thread", "from", "to"?, "body", "data"?}`.
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

## Watching from a shell
`sluice watch -p myproj [--kinds k1,k2] [--threads a,b] [--since-seq N]` follows the log from now
(or after `--since-seq`) and prints each matching record as one JSON line; it never exits and
needs no server. In Claude Code, run it under the Monitor tool so every record arrives as an
event:

    Monitor("sluice watch -p myproj --kinds step.status,message")

`--threads questions` narrows the messages to that thread.
