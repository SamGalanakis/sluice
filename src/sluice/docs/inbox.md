# The inbox: asking a person

When you need a person (a decision, an approval, a value only they know), post an item to the
project's inbox. The dashboard's Inbox (`/inbox`) shows every open item with a red count in its
nav; the person answers there (or anyone calls `inbox_answer`), and you read the answer.

## Posting and waiting
- `inbox_post(project, title, body?, ui?, input?, from?)` → `{id}` (e.g. `"i3"`). `title` is
  the question in one line, `body` any context as markdown, `ui` an OpenUI Lang program with
  buttons or a form (below; without it the person gets a text box), `from` who is asking.
- Wait with `log_wait(project, since_seq, kinds=["inbox"])`: every post, answer and close is a
  log record (`inbox.post {item, title, from?, input?}`, `inbox.answer {item, answer, by}`,
  `inbox.close {item, reason?, by}`). Or read `inbox_list(project, status="answered")`.
- An answer is `{action, params?, values?, text?}`: a Button sends its `action` and `params`
  plus the `values` of its form's fields; the text box sends `{"action": "answer", "text"}`.
- `inbox_close(project, id, reason?)` withdraws an item you no longer need.
- Only an open item can be answered or closed. A second answer, or one after a close, is
  refused (`conflict` with the item's `status`), so a stale button can never answer twice.
- Items live in the project's inbox, not in the log, so they outlast log trimming.

## Setting a plan input
With `input` (a declared plan input; anything else is refused at post), the answer sets that
input, exactly as `plan_set_input` would (same type check, a `plan.input` record whose reason
names the item), so the steps waiting on it start. The value is the first of `values.value` (a
field named `value`), `params.value` (a button's value) and `text`. A value that does not fit
the input's type refuses the answer and the item stays open. Without a `body`, the item shows
the input's `doc` (`docs("plans")`), so a well-documented input needs only a title.

## In a plan: `inbox.ask`
`inbox.ask` `{title, body?, ui?}` → `{answer}` posts an item (`from` = the step) and waits for
it, so a human decision is a plain step: read `ask/answer.action`, `ask/answer.values`,
`ask/answer.text`. If the item is closed instead, the step fails.

```json
{"inputs": {},
 "outputs": {"decision": {"source": "ask/answer.action"}},
 "steps": {
   "ask": {"run": "inbox.ask", "in": {"title": {"default": "Ship v2?"},
           "body": {"default": "All **412** tests pass."},
           "ui": {"default": "root = Stack([Button(\"Ship\", \"ship\"), Button(\"Hold\", \"hold\")], \"row\")"}}}}}
```

## The ui: OpenUI Lang
One statement per line, `name = Component(arg, ...)`; the first statement (conventionally
`root`) is drawn. Arguments are positional, in the order of the signatures below (pass `null`
to skip an optional one). Values: `"strings"`, numbers, `true`/`false`, `[arrays]`, `{key:
value}` objects, and names of other statements. A line with an unknown component, a missing
required argument or bad syntax is dropped, and the item says how many lines were dropped; the
text box is always there as a fallback. Put prose in `body`; the ui is for the answer.

Components (the whole vocabulary):

<!-- vocabulary: from src/sluice/static/openui.json, which the renderer uses; a test keeps them in step -->
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
- `Button(label: string, action?: string, params?: Record<string, any>, variant?: "primary" | "secondary")` — Answers the item with {action, params, values}: action defaults to "submit", params to {}, values are the fields of its Form (or every field outside a form). A primary button (the default) checks its fields' rules first.
<!-- end vocabulary -->

## Examples

Approve or reject; posted with `input: "approved"` (a `boolean` plan input), the button's
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
