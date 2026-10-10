# Examples

## One call
`fn_call("git.head", {"path": "/repo"}, wait=30)` →
`{"call": "019a2b3c-…", "project_id": null, "status": "succeeded", "inputs": {"path": "/repo"},
"outputs": {"branch": "main", "sha": "..."}, "error": null, "direct": false}`.
The call id is a UUID. For long functions use `wait=0` and poll `call_status(call)`. Pass
`project` to run it with that project's functions. (`git.head` and the `agent.*` functions below are built in.)

## Parallel work, then a summary (fan-out, fan-in)
`project_create("health", "Add health checks to the API and UI")`, then use one
`plan_edit(project="health", rev=1, reason="Build API and UI health checks", ops=[...])`.
Declare `repo` with `input.put`, add each step with `step.add {step, spec}`, and expose
`summary` with `output.put {name: "summary", source: "summary/result"}`.
The following JSON is the export format returned by `plan_get`, not an edit request:
```json
{"inputs": {"repo": "string"},
 "outputs": {"summary": {"source": "summary/result"}},
 "steps": {
   "api": {"run": "agent.devin", "outputs": {"branch": "string"},
           "in": {"cwd": {"source": "repo"},
                  "spec": {"default": "Add a /health endpoint on a new branch of your own"}}},
   "ui":  {"run": "agent.devin", "outputs": {"branch": "string"},
           "in": {"cwd": {"source": "repo"},
                  "spec": {"default": "Show health status in the footer, on a new branch of your own"}}},
   "summary": {"run": "agent.claude",
           "in": {"cwd": {"source": "repo"}, "api": {"source": "api/branch"},
                  "ui": {"source": "ui/branch"},
                  "prompt": {"default": "Summarise the changes on these two branches for the changelog"}}}}}
```
`api` and `ui` run in parallel once `plan_set_input("health", "repo", "/repo")` is set; each
submits the branch it worked on. `summary` waits for both (fan-in) and gets both branches as
inputs. More on shaping plans like this: `docs("composing")`.

## One step per item (scatter)
```json
{"inputs": {"repo": "string", "issues": "string[]"},
 "outputs": {},
 "steps": {"fix": {"run": "agent.run", "scatter": "spec",
   "in": {"engine": {"default": "devin"}, "cwd": {"source": "repo"}, "spec": {"source": "issues"}}}}}
```
`plan_set_input("fixes", "repo", "/repo")` and `plan_set_input("fixes", "issues", [...])` start it.

## A unit of steps that lands together
Tag the lane `unit:<name>` and its delivery step `exit`, then the next unit depends on the
whole lane:
```json
{"inputs": {},
 "outputs": {},
 "steps": {
   "a-fork":  {"run": "git.worktree", "tags": ["unit:a"],
               "in": {"repo": {"default": "/repo"}, "base": {"default": "origin/main"},
                      "branch": {"default": "work/a"}}},
   "a-work":  {"run": "agent.devin", "tags": ["unit:a", "exit"],
               "in": {"cwd": {"source": "a-fork/path"},
                      "spec": {"default": "Fix the flaky login test on your branch"}},
               "outputs": {"landed": "boolean"}},
   "a-clean": {"run": "git.worktree_rm", "tags": ["unit:a"], "after": ["a-work?"],
               "in": {"repo": {"default": "/repo"}, "path": {"source": "a-fork/path"}}},
   "b-fork":  {"run": "git.worktree", "tags": ["unit:b"], "after": ["unit:a"],
               "in": {"repo": {"default": "/repo"}, "base": {"default": "origin/main"},
                      "branch": {"default": "work/b"}}}}}
```
`b-fork` starts once unit `a`'s exit (`a-work`) succeeded; `a-clean` runs whether `a-work`
succeeded or was skipped.

## A step that failed
1. `status("fixes")` shows `fix` failed with its error (`{"error": <kind>, "message": ...}`).
2. Either fix the cause (e.g. `step_update` to change an input binding, with the revision read by `step_get`) and
   `step_retry("fixes", steps=["fix"])` — the retry also re-arms failed and stale steps
   blocked behind it — or record the result yourself with
   `step_set_output("fixes", "fix", {...})`.

## A step that went stale
You fixed an upstream by hand: `step_set_output("fixes", "fix", {...})` with a different result
than before. Everything computed from the old result turns `stale` (`status` shows it; its
readers wait). `step_retry` each stale step in order, or accept one as it is with
`step_set_output`.

## A human or orchestrator step
Declare a plan input (e.g. `"approved": "boolean"`) and have later steps read it. They wait until
someone calls `plan_set_input(project, "approved", true)` — or answers a question asked with
`ask(project, to="owner", input="approved", title=...)`, which sets it.

## Something is off
`verify("fixes")` → `[{"where": "projects/<id>/fns/message.ask/fn.json", "message": "fn
message.ask collides with the builtin fn ..."}]`, one entry per problem (empty when all is
well): rename or remove that function.
