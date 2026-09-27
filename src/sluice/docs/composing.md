# Composing plans

A plan exists so that the routine runs by itself and your attention goes to judgment: a step
that failed, a question an agent asks, a decision only a person can make. Keep it light, so
it stays easy to read and to change.

## Principles
- **A step is a unit of work you would hand to a person.** Almost always an agent block: an
  agent function (`agent.claude`, `agent.codex`, `agent.devin`, `agent.run`, `agent.review`)
  with a prompt, the typed inputs it needs and the typed outputs it must hand back
  (`docs("plans")`, agent blocks). Say what to achieve, not how to type the commands.
- **An edge is a real handoff**: an interface, a branch, a finding, a decision. If nothing
  meaningful passes between two steps, they need no edge, unless they must not overlap (both
  change the same file): then `"after": ["<step>"]` orders them without passing anything.
- **Draft, then release.** Steps you add come in paused; add the next handoffs, read them
  over, then `step_pause(steps=[...], subtree=true, paused=false)` to start them. Pause by
  tag to back off when the machine is busy.
- **Agents do their own mechanics.** Worktrees, branches, merges, rebases, commit messages,
  formatting a prompt, reading a sha: all of it happens inside the agent that needs it. Tell an
  agent that works next to others to use a worktree and branch of its own.
- **Declare the outputs someone downstream needs**, with a `doc` when the name alone is not
  enough (`"interface": {"type": "string", "doc": "Path of the interface file"}`). The agent
  submits them; the next step gets them as inputs. A step with nothing to hand on declares
  nothing.
- **People decide in the inbox.** When a choice is theirs, an `inbox.ask` step (or
  `inbox_post` from you) waits for it (`docs("inbox")`).
- **Finish with a check.** A last block verifies the result as a whole (tests pass, the
  feature works, the brief answers the questions) and says what is still open.
- **The plan is malleable.** Plan the part you understand, run it, read what comes back, then
  add, change or drop steps. A follow-up to an agent is one more step with its `session`.

## What to avoid
- Plumbing nodes: a `git.worktree`, `git.merge` or `git.push` step between two agents, a
  `core.format` step to build a prompt, a step to read a sha or write a commit message. Pass
  values as extra inputs; let the agent do the rest.
- Guessing every small step up front. A plan written before the work is understood has to be
  rewritten anyway; plan the next handoffs and extend it.
- Splitting one person's job over several steps so each can be tiny, or hiding several jobs in
  one prompt.

## A shared interface, then parallel work
`design` fixes the interface; `logic` and `ui` build on it in parallel; `integrate` brings
both together; `review` is optional.

```json
{"inputs": {"repo": {"type": "string", "doc": "Absolute path of the checkout"},
            "goal": {"type": "string", "doc": "The feature, in a few sentences"}},
 "outputs": {"branch": {"source": "integrate/branch"},
             "summary": {"source": "integrate/summary"}},
 "steps": {
   "design": {"run": "agent.claude", "doc": "Fix the interface the logic and the UI share",
     "in": {"cwd": {"source": "repo"}, "goal": {"source": "goal"},
            "prompt": {"default": "Design the interface between the logic and the UI for the goal under Inputs. Write it as a file, commit it on a new branch, and stop there."}},
     "outputs": {"interface": {"type": "string", "doc": "Path of the interface file in the repo"},
                 "branch": {"type": "string", "doc": "The branch holding it"}}},
   "logic": {"run": "agent.codex", "doc": "The logic behind the interface",
     "in": {"cwd": {"source": "repo"}, "interface": {"source": "design/interface"},
            "branch": {"source": "design/branch"},
            "spec": {"default": "Implement the logic behind the interface, with tests. Work in a worktree of your own on a new branch started from the given one, and commit there."}},
     "outputs": {"branch": "string"}},
   "ui": {"run": "agent.claude", "doc": "The UI on top of the interface",
     "in": {"cwd": {"source": "repo"}, "interface": {"source": "design/interface"},
            "branch": {"source": "design/branch"},
            "prompt": {"default": "Build the UI against the interface, with tests. Work in a worktree of your own on a new branch started from the given one, and commit there."}},
     "outputs": {"branch": "string"}},
   "integrate": {"run": "agent.claude", "doc": "One branch with both, tests passing",
     "in": {"cwd": {"source": "repo"}, "logic": {"source": "logic/branch"},
            "ui": {"source": "ui/branch"}, "interface": {"source": "design/interface"},
            "prompt": {"default": "Merge the logic and UI branches into a new branch and make the whole test suite pass. Where they disagree, the interface decides."}},
     "outputs": {"branch": "string",
                 "summary": {"type": "string", "doc": "What changed, and anything left open"}}},
   "review": {"run": "agent.review", "doc": "Optional: a standards pass on the result",
     "in": {"cwd": {"source": "repo"}, "base": {"default": "main"},
            "standards": {"default": "STANDARDS.md"}, "branch": {"source": "integrate/branch"},
            "notes": {"default": "Check out the branch under Inputs before you review."}}}}}
```

## Research: one agent per question, one synthesis
`scatter` runs `research` once per question; its outputs become arrays in question order.

```json
{"inputs": {"questions": {"type": "string[]", "doc": "The questions to answer"},
            "notes": {"type": "string", "doc": "A directory the agents may write notes in"}},
 "outputs": {"brief": {"source": "synthesize/brief"}},
 "steps": {
   "research": {"run": "agent.claude", "scatter": "question",
     "in": {"cwd": {"source": "notes"}, "question": {"source": "questions"},
            "prompt": {"default": "Answer the question under Inputs from primary sources. Say how sure you are."}},
     "outputs": {"answer": "string",
                 "sources": {"type": "string[]", "doc": "URLs or paths backing the answer"}}},
   "synthesize": {"run": "agent.claude", "doc": "One brief from every answer",
     "in": {"cwd": {"source": "notes"}, "questions": {"source": "questions"},
            "answers": {"source": "research/answer"}, "sources": {"source": "research/sources"},
            "prompt": {"default": "Write one brief from these answers: where they agree, where they conflict, and what is still unknown."}},
     "outputs": {"brief": {"type": "string", "doc": "The brief, as markdown"}}}}}
```

## A follow-up to the same agent
`fix` does the work and `check` looks at it with fresh eyes. When the check finds a problem,
add a step that continues `fix`'s own session (it keeps its context), bound to what `check`
found. The plan as it stands after that edit:

```json
{"inputs": {"repo": "string"},
 "outputs": {"branch": {"source": "follow-up/branch"}},
 "steps": {
   "fix": {"run": "agent.claude",
     "in": {"cwd": {"source": "repo"},
            "prompt": {"default": "Find why the login test is flaky and fix the cause. Work on a new branch and commit there."}},
     "outputs": {"branch": "string"}},
   "check": {"run": "agent.codex", "doc": "Fresh eyes on the fix",
     "in": {"cwd": {"source": "repo"}, "branch": {"source": "fix/branch"},
            "spec": {"default": "Check out the branch under Inputs. Run the test suite repeatedly and judge whether the login test is still flaky and whether the fix addresses the cause."}},
     "outputs": {"ok": "boolean", "findings": "string"}},
   "follow-up": {"run": "agent.claude", "doc": "Back to the agent that wrote the fix",
     "in": {"cwd": {"source": "repo"}, "session": {"source": "fix/session"},
            "findings": {"source": "check/findings"},
            "prompt": {"default": "A reviewer checked your fix; the findings are under Inputs. Address them on the same branch."}},
     "outputs": {"branch": "string"}}}}
```

Outside the plan, the same follow-up is one call: `fn_call("agent.claude", {"cwd": ...,
"prompt": ..., "session": <fix's session>}, project, wait=0)`.
