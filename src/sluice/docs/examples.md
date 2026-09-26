# Examples

## One call
`fn_call("git.head", {"path": "/repo"}, wait=30)` →
`{"call": "20260926-120000-a1b2c3", "status": "succeeded", "outputs": {"branch": "main", "sha": "..."}}`.
For long functions use `wait=0` and poll `call_status(call)`. Pass `project` to run it with that
project's functions and `.env`.

## Parallel work, then a summary (fan-out, fan-in)
`project_create("health", "Add health checks to the API and UI")`, then `plan_patch("health",
1, ...)` replacing `/inputs`, `/outputs` and `/steps` to reach:
```json
{"inputs": {"repo": "string"},
 "outputs": {"summary": {"source": "summary/final"}},
 "steps": {
   "api": {"run": "agent.run", "in": {"engine": {"default": "devin"}, "cwd": {"source": "repo"},
            "spec": {"default": "Add a /health endpoint"}}},
   "ui":  {"run": "agent.run", "in": {"engine": {"default": "devin"}, "cwd": {"source": "repo"},
            "spec": {"default": "Show health status in the footer"}}},
   "prompt": {"run": "core.format", "in": {
            "template": {"default": "Summarise these two changes for the changelog:\n\n{0}\n\n---\n\n{1}"},
            "values": {"source": ["api/final", "ui/final"]}}},
   "summary": {"run": "agent.run", "in": {"engine": {"default": "claude"}, "cwd": {"source": "repo"},
            "spec": {"source": "prompt/text"}}}}}
```
`api` and `ui` run in parallel once `plan_set_input("health", "repo", "/repo")` is set; `prompt`
waits for both (fan-in) and builds the text that `summary` receives.

## One step per item (scatter)
```json
{"inputs": {"repo": "string", "issues": "string[]"},
 "outputs": {},
 "steps": {"fix": {"run": "agent.run", "scatter": "spec",
   "in": {"engine": {"default": "devin"}, "cwd": {"source": "repo"}, "spec": {"source": "issues"}}}}}
```
`plan_set_input("fixes", "repo", "/repo")` and `plan_set_input("fixes", "issues", [...])` start it.

## A project function
`fn_save({"name": "text.upper", "inputs": {"text": "string"}, "outputs": {"text": "string"}},
main_py, project="fixes")`, then use `"run": "text.upper"` in that project's plan.

## A step that failed
1. `status("fixes")` shows `fix` failed with its error and stderr tail.
2. Either fix the cause (e.g. `plan_patch` to change an input, with the current `rev`) and
   `step_retry("fixes", "fix")`, or record the result yourself with
   `step_set_output("fixes", "fix", {...})`.

## A human or orchestrator step
Declare a plan input (e.g. `"approved": "boolean"`) and have later steps read it. They wait until
someone calls `plan_set_input(project, "approved", true)`.

## Something is off
`verify("fixes")` → `{"ok": false, "problems": [{"where": "projects/fixes/fns/git.head/fn.json",
"message": "fn git.head collides with the builtin fn at ..."}]}`: rename or remove that function.
