# sluice

sluice runs typed plans of functions for AI-agent work. An orchestrator, usually an agent
talking MCP, edits a plan: a small graph of steps, each a function with typed inputs and
outputs. A runner starts each step once what it reads is ready (and, for a step that needs a
share of its project's resources, once they have room), records what it produced, marks
results stale when their inputs change, and keeps runs alive across its own restarts. A local
dashboard shows the plan as a live board, with an inbox for the decisions that need a person.

> Unchecked slop: use at your own risk.
