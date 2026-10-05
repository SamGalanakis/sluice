# The board: a project's live instrument panel

Each project can have a **board**: a small page you write, drawn on the dashboard beside the
plan (on a wide screen a column to its right, 320px wide or more, which the owner can widen or
show alone across the page; on a phone its own section behind a Plan · Board switch), so lay it
out to read at any of those widths. It is for what the owner should see at a glance about *this* project: a lane
overview, a few numbers, a chart, the step that matters, a button to ask you for something.
Its live parts are filled from the project each time the page draws and again whenever the
project changes, so you write it once and it stays current. A level-1 `Heading` that opens the
root `Stack` is the board's own title: the dashboard draws it as the column's head (else the
head says "Board"). Under the head the dashboard says when you last set the board and whether
the plan has changed since, so the board needs no "updated at" line of its own.

## Setting it
- `board_set(project, program, expected_rev?, reason?)` → `{rev}`. `program` is an OpenUI Lang
  program (below); `null` clears the board. Setting the program it already has changes nothing
  and returns the same `rev`.
- `expected_rev` is the `board_rev` you read (`status`, `projects_list` or `board_get`): with a
  different current rev the call is refused (`conflict`, with `current_rev`) and nothing
  changes. Pass it when you edit a board someone else may have changed (the owner can edit it in
  the project's settings).
- A program that does not check is refused (`invalid`) with every problem as `line N: …`: a
  line that is not a statement, an unknown component, a missing, extra or mistyped argument, a
  name used but never defined (or defined twice, or referring to itself), a statement nothing
  uses. Nothing is stored until it checks.
- `board_get(project)` → `{project, rev, program}` (`rev` 0 and `program` null before any
  board). `status` and `projects_list` carry `board_rev`.
- Each change is a `project.board` log record: `{rev, cleared, reason, author}` (the program
  itself is not recorded).
- A step's run may set its own project's board (`sluice tool board_set` in the run, or the
  MCP tool), as the orchestrator can.

## The language
One statement per line, `name = Component(arg, ...)`; the first statement (conventionally
`root`) is drawn. Arguments are positional, in the order of the signatures below; pass `null`
to skip an optional one. Values: `"strings"` (or `'strings'`), numbers, `true`/`false`,
`[arrays]`, `{key: value}` objects, and names of other statements. A statement may span lines
while its brackets are open. Lines starting `//` or `#` are comments.

Everything a question's `ui` uses (`docs("inbox")`) works here, plus the data components.

Layout and text:

- `Stack(children: Component[], direction?: "col" | "row")` — a column of parts, or a row.
- `Heading(text: string, level?: number)` — a heading, level 1 to 3.
- `Text(text: string, tone?: "default" | "muted")` — one paragraph of plain text.
- `Callout(text: string, variant?: "info" | "success" | "warning", title?: string)` — a short highlighted notice.
- `Table(columns: string[], rows: string[][], caption?: string)` — a fixed table; cells may be numbers.
- `Separator()` — a rule between groups.

Fields and buttons:

- `Form(name: string, fields: Component[], buttons: Component[])` — fields with the buttons that send them.
- `Input(name: string, label?: string, placeholder?: string, type?: "text" | "number" | "email" | "url", value?: string, rules?: string[])`
- `Textarea(name: string, label?: string, placeholder?: string, value?: string, rules?: string[], rows?: number)`
- `Select(name: string, options: string[], label?: string, value?: string, rules?: string[])`
- `Radio(name: string, options: string[], label?: string, value?: string, rules?: string[])`
- `Checkbox(name: string, label: string, checked?: boolean)`
- `Button(label: string, action?: string, params?: Record<string, any>, variant?: "primary" | "secondary")`

Data, filled from the project when the page draws:

- `Units(state?: ("running" | "failed" | "settled" | "blocked" | "queued" | "pending")[])` — the units view's rows (`status(project, view="units")`): each unit (a link to its page) with its state, its steps' marks and what holds it. Without `state`, done units are left out and counted; name `settled` to see them.
- `StepStatus(step: string)` — the step's card: its status glyph and word (or "blocked", "queued", "outside"), and why it waits or what failed. A link to the step.
- `Output(step: string, field: string)` — the step's current output `field`, cut short; "Not set yet." until it has one.
- `Metric(label: string, query: string)` — one number (the first column of the first row) under its label.
- `Query(query: string, caption?: string)` — the result as a table (the first 50 rows).
- `Chart(kind: "bar" | "line", query: string, caption?: string)` — a small chart of a two-column result: a label, then a number (the first 60 rows). `bar` draws a bar per row; `line` joins them in order.

A query is one read-only `SELECT` (or `WITH`) run exactly as the `query` tool runs it (the same
tables and views, the 2 s deadline and size limits). Every `?` (or `?1`) in it is bound to the
project's id, so `WHERE project_id = ?` keeps it to this project; named parameters are refused.
A board runs at most 16 queries.

A part that cannot be drawn (a query that fails, a step not in the plan, a chart whose second
column is not a number) is drawn as a small error box naming the component, its line and the
reason; the rest of the board and the page still draw. Check a new board once on the page, or
in the settings' preview.

## Buttons
A Button sends you, the orchestrator, a `say` from the owner on the `owner` thread: its body is
`Board: <label>` and its `data` is `{board_rev, action, params, values}` — `action` and
`params` as the Button names them (`action` defaults to `"submit"`), `values` the fields of its
Form (or every field outside a Form), by name: text as strings, a `number` Input as a number, a
Checkbox as a boolean. A primary Button (the default) checks its fields' `rules` first; a
secondary one does not. You receive it as any note (`next`, `log_wait`, `messages`); act on it
as you see fit.

The board never changes the plan by itself. If the board has changed since the page was drawn
(`board_rev` differs), the press is refused on the page ("The board changed since this page was
drawn…") and nothing is sent, so an action always refers to the board the owner saw.

## Examples

A lane overview: the units still in play, two numbers and the steps by status.

```openui
root = Stack([title, lanes, numbers, chart])
title = Heading("Lanes", 1)
lanes = Units(["running", "failed", "blocked", "queued"])
numbers = Stack([running, failed], "row")
running = Metric("Running", "SELECT count(*) FROM steps WHERE project_id = ? AND status = 'running'")
failed = Metric("Failed", "SELECT count(*) FROM steps WHERE project_id = ? AND status = 'failed'")
chart = Chart("bar", "SELECT status, count(*) FROM steps WHERE project_id = ? GROUP BY status ORDER BY 2 DESC", "Steps by status")
```

A release at a glance, with a button asking you to ship (`data` arrives as
`{"board_rev": 3, "action": "ship", "params": {"channel": "stable"}, "values": {"notes": "..."}}`):

```openui
root = Stack([gate, version, notes, ask])
gate = StepStatus("release-gate")
version = Output("bump", "version")
notes = Query("SELECT step_id, json_extract(outputs, '$.summary') AS summary FROM steps WHERE project_id = ? AND status = 'succeeded' ORDER BY position", "What is done")
ask = Form("ship", [note], [ship])
note = Textarea("notes", "Release notes", null, null, ["required", "minLength:10"])
ship = Button("Ship it", "ship", {channel: "stable"})
```

Messages per day, as a line:

```openui
root = Chart("line", "SELECT substr(at, 1, 10) AS day, count(*) FROM messages WHERE project_id = ? GROUP BY day ORDER BY day", "Messages a day")
```
