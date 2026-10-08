# The inbox: asking a person

When you need a person (a decision, an approval, a value only they know), ask them. A
question to them is `ask(project, to="owner", ...)`. The dashboard's Inbox (`/inbox`) shows
every open question to the owner with a red count in its nav; the person answers there, and
you read the answering reply. When the home's config sets a `notify` command, each new open
question to `owner` also runs it once (a note never does). A note to the owner (a `say`) waits under "Unread
notes" until the owner marks it read there or on its thread; opening the thread does not, so
a note is never read for having been shown.

## Asking and waiting
- `ask(project, to="owner", body, title=..., ui=..., input=..., data=...)` → its receipt
  `{id, to, thread, delivery}` (`docs("threads")`). `title` is the question in one line,
  `body` any context as markdown, `ui` an OpenUI Lang program with buttons or a form (below;
  without it the person gets a text box). You ask as the orchestrator, on the thread `owner`;
  a step's run asking (with its `run`) asks as its step, on its own thread.
- Wait for the answer with `log_wait(project, since_seq, wake="questions")`, or let `next`
  return it: a `message` record is written for the question and for the reply. Or read the
  `messages(project, view)` views: `"inbox"` (your open questions, then your unread notes and
  replies; `owner: true` for the owner's), `"questions"` (every open question in the project,
  whoever it is addressed to), `"history"` (threads you took part in), `"thread"` (one thread
  in full). Each question shows its `state`: `open`, `answered` (with `answered_by`) or
  `closed`.
- An **answer** is a `reply(project, to_message=<question id>, body, answer={action,
  params?, values?})`. From the dashboard, a Button sends its `action` and `params` plus the
  `values` of its form's fields; the text box sends `{"action": "answer"}` with the text as
  the reply's body. The first reply to an open question answers it, atomically.
- A reply with an `answer` to a question that is no longer open is refused (`conflict`:
  "question is no longer open"), so a stale button can never answer twice; a later plain
  reply is just a message. A reply whose `answer.action` is `close` closes the question
  without answering it.
- Message rows outlive the log: they are never trimmed with it, and are deleted with their
  project.

## Setting a plan input
With `input` (a declared plan input; anything else is refused when asked), the answering reply
sets that input, exactly as `plan_set_input` would (same type check, a `plan.input` record),
so the steps waiting on it start. The value is the first of `answer.values.value` (a field
named `value`), `answer.params.value` (a button's value) and the reply's body text. A value
that does not fit the input's type refuses the reply and the question stays open. The
dashboard shows which input a question sets.

## In a plan: `message.ask` with `wait`
`message.ask` is a builtin fn taking `ask`'s arguments: a step running it asks and, with
`wait: true`, blocks until the first answering reply, which it returns as `reply` (with the
question's `id` and `receipt`) — a human decision is a plain step. If the question is closed
instead, the step fails `question closed`.

```json
{"inputs": {},
 "outputs": {"decision": {"source": "ask/reply.action"}},
 "steps": {
   "ask": {"run": "message.ask",
           "in": {"to": {"default": "owner"}, "title": {"default": "Ship v2?"},
                  "body": {"default": "All **412** tests pass."},
                  "ui": {"default": "root = Stack([Button(\"Ship\", \"ship\"), Button(\"Hold\", \"hold\")], \"row\")"},
                  "wait": {"default": true}}}}}
```

While the step's run waits, its question is `waiting`; once the run stops (failed, cancelled,
finished some other way) the dashboard folds it under "Nobody is waiting" with the reason,
where the owner may close them all at once. The
question stays open: retrying the step takes it up again — a run of the same step asking the
same title reuses its earlier open question, and an answer given while nobody was waiting is
delivered to it. Answer or close a question you do not mean to ask again (a reply with
`answer={"action": "close"}`). Plans written before the verbs may still name `message.post`;
it runs as `message.ask`, `message.say` or `message.reply` would, and is retired.

## The ui: OpenUI Lang
One statement per line, `name = Component(arg, ...)`; the first statement (conventionally
`root`) is drawn. Arguments are positional, in the order of the signatures below (pass `null`
to skip an optional one). Values: `"strings"`, numbers, `true`/`false`, `[arrays]`, `{key:
value}` objects, and names of other statements. A line with an unknown component, a missing
required argument or bad syntax is dropped, and the message says how many lines were dropped;
the text box is always there as a fallback. Put prose in `body`; the ui is for the answer.

Components (the whole vocabulary):

- `Stack(children: Component[], direction?: "col" | "row")` — Layout container and the usual root: a column of parts, or a row (e.g. of buttons).
- `Heading(text: string, level?: number)` — A heading, level 1 to 3.
- `Text(text: string, tone?: "default" | "muted")` — One paragraph of plain text. Longer prose belongs in the item's markdown body.
- `Callout(text: string, variant?: "info" | "success" | "warning", title?: string)` — A short highlighted notice.
- `Table(columns: string[], rows: string[][], caption?: string)` — A table: one array of cell strings per row, in column order.
- `Separator()` — A horizontal rule between groups.
- `Form(name: string, fields: Component[], buttons: Component[])` — Groups fields with the buttons that submit them: a button in a form sends that form's fields as `values`.
- `Input(name: string, label?: string, placeholder?: string, type?: "text" | "number" | "email" | "url", value?: string, rules?: string[])` — A one-line field. type number sends a number. rules: "required", "email", "url", "numeric", "min:N", "max:N", "minLength:N", "maxLength:N".
- `Textarea(name: string, label?: string, placeholder?: string, value?: string, rules?: string[], rows?: number)` — A multi-line field for longer answers.
- `Select(name: string, options: string[], label?: string, value?: string, rules?: string[])` — One choice from a drop-down list.
- `Radio(name: string, options: string[], label?: string, value?: string, rules?: string[])` — One choice, all options shown. Prefer it to Select for a handful of options.
- `Checkbox(name: string, label: string, checked?: boolean)` — One yes/no answer (a boolean in values).
- `Button(label: string, action?: string, params?: Record<string, any>, variant?: "primary" | "secondary")` — Answers the question with {action, params, values}: action defaults to "submit", params to {}, values are the fields of its Form (or every field outside a form). A primary button (the default) checks its fields' rules first.

## Examples

Approve or reject; asked with `input: "approved"` (a `boolean` plan input), the button's
`params.value` sets it. Approve answers `{"action": "approve", "params": {"value": true},
"values": {}}`:

```openui
root = Stack([summary, buttons])
summary = Table(["change", "tests"], [["billing: retry failed charges", "412 passed"]])
buttons = Stack([approve, reject], "row")
approve = Button("Approve", "approve", {value: true})
reject = Button("Reject", "reject", {value: false}, "secondary")
```

Pick one of N; with `input`, the radio named `value` sets it. Choosing sqlite answers
`{"action": "choose", "params": {}, "values": {"value": "sqlite"}}`:

```openui
root = Form("pick", [choice], [go])
choice = Radio("value", ["postgres", "sqlite", "duckdb"], "Which database?", null, ["required"])
go = Button("Choose", "choose")
```

A short form. Ship answers `{"action": "ship", "params": {}, "values": {"version": "1.4.0",
"notes": "...", "notify": true}}`; Hold skips the rules:

```openui
root = Stack([intro, release])
intro = Callout("Nothing ships until you say so.", "info", "Release 1.4")
release = Form("release", [version, notes, notify], [ship, hold])
version = Input("version", "Version", null, null, "1.4.0", ["required"])
notes = Textarea("notes", "Release notes", null, null, ["required", "minLength:10"])
notify = Checkbox("notify", "Tell the team", true)
ship = Button("Ship", "ship")
hold = Button("Hold", "hold", {reason: "not yet"}, "secondary")
```
