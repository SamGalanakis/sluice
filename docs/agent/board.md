# The board: a project's live instrument panel

Each project can have a **board**: one page for the owner, drawn on the dashboard beside the
plan (on a wide screen a column to its right, 320px wide or more, which the owner can widen or
show alone across the page; on a phone its own section behind a Plan · Board switch), so lay it
out to read at any of those widths. It is one human-facing document about *this* project: what
it is doing, where it stands, what needs the owner, with a few live numbers, a chart, the step
that matters and a button to ask you for something.

It has two parts. The **program** (`board_set`) is its layout and its live widgets, filled from
the project each time the page draws and again whenever the project changes, so you write it
once and it stays current. The **document** (`board_doc_write`, `board_doc_edit`) is the
hand-written part, plain prose for the owner, drawn where the program puts `Doc()`. A level-1
`Heading` that opens the root `Stack` is the board's own title: the dashboard draws it as the
column's head (else the head says "Board"). The dashboard says when you last set the program
or edited the document and whether the plan has changed since (under the document, "Edited
12m ago by orchestrator", once it is written; else under the head), so the board needs no
"updated at" line of its own.

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
- `board_set` returns `{rev, warnings}`. Each warning is a step the program names that the
  plan cannot give it: `line N: StepStatus names step `x`, which is not in the plan`, or
  `selects `tag:x`, which no plan step carries` (or several do); or a query that counts the
  owner's cancels as failures: `line N: Metric counts status 'failed', which includes the steps
  the owner cancelled; …`. A warning never refuses the program; fix the name, or name the step
  by its tag (below), or count with `Count`.
- `board_get(project)` → `{project, rev, program}` (`rev` 0 and `program` null before any
  board). `status` and `projects_list` carry `board_rev`.
- Each change is a `project.board` log record: `{rev, cleared, reason, author}` (the program
  itself is not recorded).
- A step's run may set its own project's board (`sluice tool board_set` in the run, or the
  MCP tool), as the orchestrator can.
- A plan edit (`plan_patch`, `step_remove`, the unit tools, any edit) that takes away a step
  the board names is made all the same, and its result carries `board_warnings` in the same
  form, so a rename never leaves a widget silently showing a step that is gone.

## The document
The board is one document, so keep it as one: the program fixes its layout and live parts;
`Doc()` is the hand-written part, in plain prose for the owner. Say what the project is doing,
where it stands and what needs the owner. Tickets and runs appear only as citations (links),
never as the content itself. Whenever anything the document says changes, update the whole of
it: read it and edit the lines that changed, or rewrite it. A document that is right in one
paragraph and a day stale in the next is the failure it exists to prevent.

- `board_doc_read(project)` → `{rev, updated_at, author, markdown, numbered}`: its revision
  (0, with `markdown` "", before the first write), when and by whom it was last edited, its
  markdown, and `numbered`, the same text with right-aligned line numbers and a tab before
  each line, as `cat -n` prints it.
- `board_doc_write(project, markdown, expected_rev?, reason?)` → `{rev, changed}` replaces
  the whole document. With `expected_rev`, a document at another rev is refused (`conflict`,
  with `current_rev`) and nothing changes. The same text again changes nothing (`changed`
  false, the same `rev`).
- `board_doc_edit(project, expected_rev, edits, reason?)` → `{rev, changed}` edits lines of
  the rev you read; `expected_rev` is required, since the line numbers come from it, and a
  mismatch is refused (`conflict`, with `current_rev`). Each edit `{start, end, text}`
  replaces lines `start` to `end` (1-based, inclusive) with `text`, which may be several lines
  or `""` to delete them; `end = start - 1` inserts before `start`, and `start = lines + 1`
  appends. Edits must not overlap, and all apply (bottom-up, so every number is from the rev
  you read) or none; a bad range or an overlap is refused (`invalid`), naming the edit.
- The document is markdown: headings (`##` to `####`), paragraphs, lists, links, **bold**,
  *italics*, `code` and quotes, at most 64 KiB. It draws through the dashboard's renderer, as
  message bodies do: everything escaped, an unsafe link drawn without its target.
- Each change is one `project.update` record whose `fields` is `["board_doc"]`, with its
  `reason` and `author`, so `log_read(kinds=["project.update"])` shows who changed it when. A
  document edit never moves the board's `rev`, so a button the owner pressed stays valid.
- The tools refuse (`invalid`, "this board's program has no Doc()") until the board's
  program has a `Doc()`; a program has at most one. In a run, `project` defaults to the run's
  own; another project's document is outside its authority.
- Project settings shows the document under the program, read-only, with its rev and last
  edit.

```openui
root = Stack([title, doc, lanes, red])
title = Heading("Release 2.0", 1)
doc = Doc("Nothing written yet.")
lanes = Units(["running", "failed", "blocked"])
red = Metric("Red targets", "SELECT json_extract(progress, '$.red') FROM steps WHERE project_id = ? AND EXISTS (SELECT 1 FROM json_each(declaration, '$.tags') WHERE value = 'main-tests')")
```

Read, then edit the lines that changed:

```
board_doc_read(project="release")
→ {"rev": 4, "updated_at": "2026-10-07T09:12:03Z", "author": "orchestrator",
   "markdown": "## Where it stands\nMaking main green: 11 red targets left.\n\n## Needs you\n- Approve the paid S35 rows\n- Decide [#53](https://github.com/example/release/issues/53)\n",
   "numbered": "     1\t## Where it stands\n     2\tMaking main green: 11 red targets left.\n     3\t\n     4\t## Needs you\n     5\t- Approve the paid S35 rows\n     6\t- Decide [#53](https://github.com/example/release/issues/53)\n"}

board_doc_edit(project="release", expected_rev=4, reason="S35 approved; 3 red left", edits=[
  {"start": 2, "end": 2, "text": "Making main green: 3 red targets left ([run 412](https://ci.example/412))."},
  {"start": 5, "end": 5, "text": ""}])
→ {"rev": 5, "changed": true}
```

## Naming steps
`StepStatus(step)`, `Output(step, field)` and `LatestMessage(from)` take a step id, or
`"tag:<tag>"` for the one plan step carrying that tag when the board draws. A step id never has
a colon, so the two cannot be confused. Prefer a tag: tag the step you mean (`main-tests`) and
name it as `tag:main-tests`, and a rename of the step changes nothing on the board. A tag that
no step carries, or that several carry, draws the widget's error box, naming the tag and how
many steps carry it.

In a query, select steps by tag with what the `query` tool exposes: a step's declaration, in
the `steps` table's `declaration` column, holds its `tags`, so
`EXISTS (SELECT 1 FROM json_each(declaration, '$.tags') WHERE value = 'main-tests')` keeps the
steps tagged `main-tests` (and `unit` holds a step's unit). Rows of other tables join `steps` on
`project_id` and `step_id`; a removed step's rows (`outcomes`) have no tags to select by.

The checks behind the warnings read the program as text, so they catch what is written down:
- StepStatus's and Output's step, and a `tag:` selector anywhere: always.
- LatestMessage's `from`, unless `owner` or `orchestrator`: when it is not a plan step but
  was one (a step result, a message from one of its runs or a status record names it).
- A Metric's, Query's or Chart's SQL: each string compared with a column that holds step ids
  by `=`, `==` or `IN (...)`, on either side, bare or qualified (`s.step_id`): `step_id`
  always; a message's `from`, `to` or `thread` when the value was once a step (and never a
  status word, `owner` or `orchestrator`).
- Not caught: `NOT IN`, `<>`, `LIKE` and `GLOB`, a value built by an expression or held in
  JSON, an author such as `step:x`, and a step id in a Markdown or Text.
- A Metric's, Query's or Chart's SQL that compares `status` with `'failed'` and never reads
  `error`: it counts the steps the owner cancelled as failures, which the dashboard does not.
  The warning names `Count(label, "failed")`, which counts as the dashboard does.

On the page, a Metric, Query, Chart or LatestMessage whose step is not in the plan draws what it
has under a line in the attention colour: "Names step `tests-main`, which is not in the plan;
this shows its last data." StepStatus and Output draw their error box instead.

## A recipe's unit view
The board is one panel for the whole project. To show each unit of a recipe the same way, give
the recipe a `view` instead (`docs("plans")`, Recipes): a program in this same language, bound
to one unit, which the dashboard draws in the unit's row on the plan and whole on the unit's page, inside a
frame sluice draws (the unit's title, id, status and a cell per stage). Its vocabulary is smaller and per unit: `Stack(children, direction?)`, `Text(text,
tone?)`, `Markdown(text)`, `Link(label, href)`, `Param(name)`, `Output(stage, field)`,
`StepStatus(stage)` and `LastMessage(chars?)`, where a stage is the recipe step's id without
`{unit}-` and `{param}` fills in any string. No queries, charts or buttons: those belong on
the board.

The dashboard names steps by their titles (a step's doc's first line, its spec's heading, or
its recipe's `title`; `docs("plans")`, Titles), so a Text that repeats a step's id adds little.

A recipe's block on the plan lists its live units; its name links to every unit it made, done ones too
(`/projects/id/<p>?recipe=<name>&show=all`). A step's chain is `?root=<step>` (`&up=1` what it
comes after, `&down=1` what comes after it, `&depth=N`), a link worth sending the owner instead
of a list of ids. Each unit's page draws its runs on a timeline, and the project's Stats page
(`/projects/id/<p>/stats`) says how long each recipe's stage usually takes (median, p90 and
longest), how its runs end and how many units finish a day, so none of it belongs on the
board.

## The language
One statement per line, `name = Component(arg, ...)`; the first statement (conventionally
`root`) is drawn. Arguments are positional, in the order of the signatures below; pass `null`
to skip an optional one. Values: `"strings"` (or `'strings'`), numbers, `true`/`false`,
`[arrays]`, `{key: value}` objects, and names of other statements. A statement may span lines
while its brackets are open. Lines starting `//` or `#` are comments.

Everything a question's `ui` uses (`docs("inbox")`) works here, plus the data components.

Layout and text:

- `Stack(children: Component[], direction?: "col" | "row")` — a column of parts, or a row.
- `Heading(text: string, level?: number)` — a heading, level 1 to 3 (the column's head is its title; the levels nest under it, and markdown's headings nest under the heading before them).
- `Text(text: string, tone?: "default" | "muted")` — one paragraph of plain text (markdown is not read: use `Markdown`).
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

- `Units(state?: ("running" | "failed" | "settled" | "blocked" | "queued" | "pending")[])` — the units view's rows (`status(project, view="units")`): each unit (a link to its page) with how it reads, its steps' marks and what holds it. `state` only chooses the rows, in the units view's words: `failed` a unit with a failed or stale step (a cancel too, and one that also has a running step), `blocked` a held unit (paused, outside work, a plan input with no value, or behind a failure). Each row is drawn as the plan draws it, from the dashboard's one status table: a stale unit reads "stale", a cancel "cancelled" (■), a paused one "paused" (‖), outside work "outside" (↗), one behind a failure "blocked" (⊖), a quiet run "quiet" (◔). Without `state`, done units are left out and counted; name `settled` to see them.
- `Doc(fallback?: string)` — the board's document (above) as markdown, with "Edited 12m ago by orchestrator" under it; until it is written, `fallback` (markdown, muted) or "Not written yet.". At most one per program.
- `StepStatus(step: string)` — the step's card: its state's glyph and word (as its card on the plan reads: "quiet", "stopping", "paused", "blocked", "queued", "outside", …), and why it waits ("Paused by the owner: …", "after up (cancelled)") or what failed. A link to the step.
- `Output(step: string, field: string)` — the step's freshest value of `field`, cut short; "Not set yet." until it has one. While the step's progress (`step_progress`, `docs("fns")`) is newer than its outputs, that is the value, marked "live" while the step runs ("progress" after its run ended) with when it was set; once the step finishes with outputs, the output.
- `Count(label: string, of: "failed" | "cancelled" | "stale" | "quiet" | "blocked" | "stopping" | "finishing" | "running" | "external" | "paused" | "held" | "queued" | "pending" | "manual" | "succeeded" | "skipped" | "steps")` — one number under its label, counted as the dashboard's own summary line counts it, each step once under the one state it reads as: a step the owner cancelled is `cancelled`, never `failed`; `quiet` is a running step that has written nothing past its threshold (two hours, or its `cadence:` tag), `stopping` one whose cancel was asked for, `finishing` one whose run has submitted, and `running` counts none of those; `external` is ready work outside sluice; `paused` is a step held by its own pause or its project's, `blocked` one behind a failure, `held` one reading a plan input with no value, `queued` one short of a resource, and `pending` never includes any of them; `manual` is a step set by hand, never `succeeded`; `steps` is every step. Prefer it to a Metric for these: a Metric's SQL sees a cancel as `status = 'failed'`.
- `Metric(label: string, query: string)` — one number (the first column of the first row) under its label.
- `Query(query: string, caption?: string)` — the result as a table (the first 50 rows).
- `Chart(kind: "bar" | "line", query: string, caption?: string)` — a small chart of a two-column result: a label, then a number (the first 60 rows). `bar` draws a bar per row; `line` joins them in order.
- `LatestMessage(from: string, chars?: number)` — the newest message in the project whose sender is `from` (a step id, or a name such as `orchestrator` or `owner`, as `messages` reports `from`), with its time: its body as markdown, cut to `chars` characters (default 280, at most 4000) with an ellipsis and a link to the whole message in its thread. With none, "No message from <from> yet.".

Markdown:

- `Markdown(text: string)` — `text` as markdown: links, **bold**, *italics*, `code`, short lists. Raw HTML is shown as text, and a link to anything but `http`, `https`, `mailto` or a relative path is dropped.

A query is one read-only `SELECT` (or `WITH`) run exactly as the `query` tool runs it (the same
tables and views, the 2 s deadline and size limits). Every `?` (or `?1`) in it is bound to the
project's id, so `WHERE project_id = ?` keeps it to this project; named parameters are refused.
A board runs at most 16 queries.

Doc, Markdown and LatestMessage draw markdown on the server the way message bodies draw:
everything is escaped, and an unsafe link is drawn without its target.

A part that cannot be drawn (a query that fails, a step not in the plan, a tag that does not
select one step, a chart whose second column is not a number) is drawn as a small error box naming the component, its line and the
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
numbers = Stack([running, quiet, failed], "row")
running = Count("Running", "running")
quiet = Count("Quiet", "quiet")
failed = Count("Failed", "failed")
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

What a step last reported, cut short, with the whole message a click away:

```openui
root = Stack([head, latest, note])
head = Heading("Main: the latest full test run", 2)
latest = LatestMessage("tag:main-tests", 200)
note = Markdown("Red targets are tracked in [the census](https://ci.example/census); **lanes** fix them one by one.")
```

A rolling step that never finishes (tagged `main-tests`, publishing `ctx.progress(red=…, head=…)`
after each run of the suite): its latest numbers, live.

```openui
root = Stack([title, now, history])
title = Heading("Main", 1)
now = Stack([red, head], "row")
red = Output("tag:main-tests", "red")
head = Output("tag:main-tests", "head")
history = Query("SELECT step_id, json_extract(progress, '$.red') AS red, progress_at AS at FROM steps WHERE project_id = ? AND EXISTS (SELECT 1 FROM json_each(declaration, '$.tags') WHERE value = 'main-tests')", "Latest run")
```

Messages per day, as a line:

```openui
root = Chart("line", "SELECT substr(at, 1, 10) AS day, count(*) FROM messages WHERE project_id = ? GROUP BY day ORDER BY day", "Messages a day")
```
