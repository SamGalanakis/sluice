---
name: sluice dashboard
description: A calm board for one person supervising many agents: what is running, what each block produced, and an inbox for what needs you.
colors:
  canvas: "oklch(0.975 0.008 190)"
  ink: "oklch(0.23 0.018 220)"
  card: "oklch(1 0 0)"
  quiet-fill: "oklch(0.955 0.01 190)"
  secondary-fill: "oklch(0.945 0.012 190)"
  muted-ink: "oklch(0.45 0.035 205)"
  primary: "oklch(0.48 0.12 158)"
  primary-ink: "oklch(0.985 0.006 175)"
  hairline: "oklch(0.205 0.02 286 / 10%)"
  hairline-strong: "oklch(0.205 0.02 286 / 22%)"
  ring: "oklch(0.54 0.12 158)"
  edge: "oklch(0.49 0.035 205 / 45%)"
  status-active: "oklch(0.52 0.12 221)"
  status-idle: "oklch(0.55 0 0)"
  status-success: "oklch(0.52 0.17 149)"
  status-attention: "oklch(0.5 0.14 65)"
  badge: "oklch(0.55 0.22 27)"
  canvas-dark: "oklch(0.17 0.012 220)"
  ink-dark: "oklch(0.94 0.01 165)"
  card-dark: "oklch(0.215 0.014 220)"
  muted-ink-dark: "oklch(0.79 0.022 175)"
  primary-dark: "oklch(0.79 0.105 158)"
typography:
  title:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "26px"
    fontWeight: 650
    lineHeight: "32px"
    letterSpacing: "-0.01em"
  section:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "17px"
    fontWeight: 600
    lineHeight: "24px"
  body:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "15px"
    fontWeight: 400
    lineHeight: "22px"
  small:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: "20px"
  meta:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "13px"
    fontWeight: 500
    lineHeight: "18px"
  drawer-title:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "20px"
    fontWeight: 600
    lineHeight: "26px"
  title-phone:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "22px"
    fontWeight: 650
    lineHeight: "28px"
  data:
    fontFamily: "ui-monospace, SFMono-Regular, Cascadia Code, Liberation Mono, Menlo, monospace"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: "18px"
rounded:
  sm: "4px"
  md: "8px"
  lg: "0.625rem"
  pill: "999px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "24px"
components:
  card:
    backgroundColor: "{colors.card}"
    textColor: "{colors.ink}"
    rounded: "{rounded.lg}"
    padding: "10px 12px"
    width: "248px"
    height: "112px"
  chip:
    textColor: "{colors.muted-ink}"
    typography: "{typography.small}"
    rounded: "{rounded.lg}"
    height: "34px"
    width: "188px"
  button-primary:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.primary-ink}"
    rounded: "{rounded.md}"
    padding: "5px 12px"
  button:
    backgroundColor: "{colors.card}"
    textColor: "{colors.ink}"
    rounded: "{rounded.md}"
    padding: "5px 12px"
  badge:
    backgroundColor: "{colors.badge}"
    textColor: "#ffffff"
    typography: "{typography.meta}"
    rounded: "9px"
    height: "18px"
  nav-link-current:
    textColor: "{colors.ink}"
    padding: "0 10px"
    height: "48px"
  project-switcher:
    backgroundColor: "{colors.card}"
    textColor: "{colors.ink}"
    rounded: "8px"
    padding: "0 10px"
    height: "34px"
  status-filter-current:
    backgroundColor: "{colors.secondary-fill}"
    textColor: "{colors.ink}"
    rounded: "{rounded.sm}"
    padding: "2px 10px"
---

# Design System: sluice dashboard

## Overview

The dashboard is an Operate surface for one person, Sam, who supervises many agents from a
second screen or a phone. It is kin to hirsel, his own product: slate neutrals with a teal cast,
a near-white canvas and a charcoal field, white cards one step above the canvas, 10% ink
hairlines, one green primary, a named status ramp, Inter over the system sans. The plan is the
page: a left-to-right board of the blocks of work, each card saying one thing per slot. Ids,
fn names, bindings and run history live in the step drawer, never on the board. Nothing is
repeated: plan inputs and outputs are board nodes, not tables; history is the log filtered.

## Colors

Tokens in `src/sluice/static/dashboard.css` are the only source of colour; `:root` holds the
light values and `@media (prefers-color-scheme: dark)` the dark ones (the frontmatter lists the
light set plus the dark canvas, ink, card, muted ink and primary).

### Primary
- **Primary green** (`primary`): the answer button and the focus ring (`ring`); never decoration.

### Neutral
- **Canvas / card / quiet fill / secondary fill**: canvas under everything, cards one step above
  it, the quiet fill behind the board, prompts and code, the secondary fill for the current nav
  item and the progress track.
- **Ink / muted ink**: text; muted ink for meta, summaries, labels. Hairlines are ink at 10%
  (22% when stronger).

### Status ramp
- **Active blue** running, **success green** succeeded and set by hand, **attention amber**
  stale and a message awaiting a reply, **idle grey** pending and skipped, **ink** failed and
  the pending steps a failure blocks.

### Named Rules
**The One Red Rule.** Red (`badge`) is the open-inbox count in the nav and nothing else. A
failed step reads through its cross glyph, full-ink border and bold error line, not red.

**The Shape Carries It Rule.** Every status has its own drawn glyph (dashed ring, spinning
ring, check, ring and dot, circular arrow, cross, ring with two bars for paused, dashed ring
with a slash for skipped) plus a
visually hidden word; colour only repeats what the shape says. A paused bubble also has a
dashed amber border.

## Typography

One family, Inter Variable (the system sans without it), on a fixed ramp for a glance from a
second screen: 26/32 page title (22/28 on a phone), 20/26 the drawer's step id, 17/24
section, 15/22 body, 14/20 card text, 13/18 meta, 12/18 data. Monospace is for data only:
stderr, fn names in the functions list, values, types, the log's seq, time and kind. All
numerals are tabular.

### Named Rules
**The Meta Voice Rule.** What a run says about itself (times, costs, counts, engines, labels)
is 13px meta in muted ink, sentence case. Nothing is uppercase; no kickers or eyebrows.

## Layout

The plan reads top to bottom, inside the column. Each independent piece of work (the steps any
edge joins, handoff or `after`) is a quiet box when there are several (the muted fill,
0.625rem radius, 16px by 18px padding, 12px on a phone; 14px apart, 10px on a phone; no border:
a region, not a card, since the bubbles carry the borders), so what belongs together reads
without the edges; a plan of one piece has no box. A box is rows by dependency depth with 40px
between rows for the edges (10px on a phone, which draws none), from its first step; in a
row the cards stand
lane by lane (a lane: the steps joined by handoffs), the next lane's first card 22px apart,
and a row too wide wraps within itself. On a phone a box stacks its lanes one after another,
each reading straight down, a lane after the first 14px apart. The server lays the board out;
the `<sluice-board>` component draws the edges between measured cards (bottom to top, spread
when several share a side, an arrowhead at the end; dashed for an `after` edge, which orders
two steps without passing data), threading an edge that passes rows through their gaps so it
never hides behind a card. A quiet legend under the board names the two lines. A box of
several steps that have all succeeded (or been skipped, beside at least one success) folds to
one 44px line: the success glyph, its first step's id, then in meta "… its last step · n steps
· all succeeded"; a native `<details>` that opens to its cards (open through live updates and,
per tab, a reload). No edge crosses between boxes, so a folded one hides only its own edges.
The plan's order stays; nothing is sorted by attention. The head of a
project page says first whether the work moves (progress bar, counts, Pause and Archive), then
what the project is (its description, folded to its opening); what the plan took and produced
follows the board. When the runner is down (its heartbeat stale), the index and that
line say so first, in the attention amber. A project with failed steps leads its page and its
index row with one sentence in ink, weight 500 (not red: the failure is the orchestrator's to
retry, and only the inbox asks the person): "Stopped: a and b failed, blocking 4 steps · 11
paused", each failed step a link (to its drawer), "Stopped:" only while nothing runs. The
counts line and the bar's label count the blocked and paused steps too; the index row leads
with the project's status glyph, and the tab title with "n failed ·". Text blocks hold a
68-75ch measure.

### Named Rules
**The One Column Rule.** Every page sits on one centred 960px column (`--column`, with at
least a 24px gutter, 16px below 720px): `main` is a three-track grid (gutter, column, gutter)
and everything goes in the middle track, the board included. The top nav's content aligns to
the same edges (the mark on the left edge, Inbox ending on the right), so nav, title,
lists, tables and cards share one left edge at every width. Nothing makes the page scroll
sideways. While the step drawer is open from 1200px, the page makes room for it: nav and
`main` take its width as right padding and the column keeps to the drawer's side (40px from
it), so the column's left edge and the nav's still meet, and at 2560 the board stands beside
the drawer instead of centred far from it.

**The One Click Rule.** The board is names and states: compact bubbles you can take in at a
glance. Everything else (outputs, prompts, costs, shas) is one click away in the drawer, or
under the board for the plan as a whole.

Below 720px the board stacks one card per line without edges (and without their legend), and
the step drawer becomes a full-screen sheet over the scrim.

## Elevation & Depth

Flat by default: cards separate by hairline, not shadow. One lift (`--lift`), on what floats
over the page: the step drawer below 1200px (beside the page from there, it has only its
hairline), the project switcher's menu and the focused skip link.

## Shapes

Radius 0.625rem for cards, chips, the needs list, inbox items and board boxes; 8px for
controls and code blocks; 4px for inline code, tags, menu items, the segmented filter's
current item and the nav's current bar. A pill (999px) is deliberate, and only for the step
bubbles, the Types switch and the badge-like status pills: a bubble is a token of work, not a
panel. Plan input and output nodes are dashed, since they are ends, not work.

## Components

### Cards / Containers
- **Step bubble**: a pill with the status glyph, the step id (14.5px, 550) and, in 12px meta,
  its time (and `done/total` when scattered). Nothing else: the doc and what it says now are
  its tooltip (a failed step's is its error's last line, the exception), and everything it
  took and produced is in the drawer. Running bubbles take an active-blue border, failed a
  full-ink one over a 6% ink fill, stale an amber one. A pending step a failure holds up
  (directly or through other pending steps) is blocked: a dashed ink border at 45% and
  "blocked" in ink where its time would be (not red; a paused one keeps the paused look and
  counts as paused). A pending step next in line (its unfinished upstream all running) keeps
  a strong hairline and ink id; pending steps further off lose their border and dim, so what
  starts next stands out. Glue steps (`core.*`) are dashed and muted. The step in the drawer
  wears the ring 2px outside its border (canvas, then ring), so it never reads as a status.
  A bubble's accessible name is "failed, id, 1h 14m" (visually hidden commas).
- **Tracing**: hovering or keyboard-focusing a bubble lights its edges and names; the other
  bubbles lose their border and fill and their text turns muted ink, so they stay readable
  (at least 3:1, measured 6.4:1). Opening the drawer clears any tracing; a focus given back
  after a click does not trace.
- **Under the board**: the Result (`name value` rows; long text folds to 132px under a fade with
  "Show all") and the plan inputs (name, value, doc). The counts line (with the bar, Pause and
  Archive) heads the page instead.

### Buttons
Primary is green on its own ink; others are card-coloured with an input hairline.

### Navigation
- **One bar** (52px, card fill, on the column), the only navigation: the mark (an ink rounded
  square with a gate over water, cut in the card colour; it leads to All projects), the
  **project switcher**, then the sections; Inbox, with the one red badge, sits at the right
  edge. There is no second row: a page does not repeat the project's name or its sections.
- **Project switcher**: a bordered button whose label is the chosen project's name (or "All
  projects") and a chevron; it opens a menu of All projects, then every project with its status
  glyph, the archived ones last under a label. A `<details>`, so it works without script;
  `nav.js` closes it on a click elsewhere or Escape.
- **Sections** follow the switcher: in a project, Plan, Threads, Log, History, Functions; with
  none chosen, Log, Functions (the switcher's "All projects" is the index). The current one is ink with a 2px ink bar on the bar's
  bottom hairline and `aria-current` (`page`, or `true` on a page inside it: a step is inside
  Plan); the rest are muted ink, with a quiet fill on hover.
- **Phone** (below 720px): the mark gives way to the switcher (whose menu leads to All
  projects), the switcher's label clips at 120px, and Inbox is a tray icon with its badge; the
  sections scroll sideways inside themselves if they ever do not fit.
- **Status filter** (inbox): a small segmented control, the current status in the secondary
  fill with a strong hairline; a filter, so it does not look like the sections.

### Threads
The Threads tab, one bordered card per thread, the latest first: the step's glyph, id and
doc (or the thread name; "no longer in the plan" when its step has gone), "n messages ·
when", an "awaiting reply" tag in amber when a question waits on a step still to finish (or on
a thread of no step), and a muted line of the last message; open, a hairline under the summary and
the messages, all but the last three folded under an "n earlier messages" link. A small blue
"n new" pill counts what arrived since this browser last opened the thread, and a blue dot
marks those messages while it is open. A message is a 2px left rule and a small head (sender in ink, → recipient and
when in muted ink, a "note" or "Awaiting reply" tag), then its body at reading size. A step's
messages sit on the left with a strong-hairline rule; the orchestrator's are indented 28px
(14px on a phone) with a blue rule, so a conversation reads at a glance. Nothing there asks
the person for anything: that is the inbox.

### Step drawer
A right-hand panel, full height. From 1200px it sits beside the page (`min(680px, 45vw)`, its
hairline and no lift): nav and page make room for it in one reflow while the drawer slides in
(180ms ease-out, transform and opacity), the board redraws its edges, and the opened card
scrolls into view. From 721 to 1199px it is over
the page (680px) on a scrim that closes it. Below 720px it is a full-screen sheet over the
scrim and a modal dialog (the page behind it inert). Escape, the close button, the scrim or
a click on the page around the board close it, and focus goes back to the card. A "Skip to
plan" link is the page's first tab stop, and a polite live region says the statuses the live
board moves ("a failed").
It reads like a run history (Temporal's event view is the reference): the step id (20px) and
its doc, then a quiet grid of facts (status, "blocked" for a blocked step, function, started,
duration, cost, session; then, each a full row that wraps, what a pending step waits on, what
it runs after, its `when`, and what a failed step blocks: steps as links led by their
glyphs), the Pause switch (only where pausing acts: pending, failed, stale; Resume on any
paused step) and a muted link to its thread. Sections under small labels in need order:
Error (its last line in 600, then all of it in a box that opens scrolled to its end),
Progress, Outputs, Prompt, Inputs, Log output, Attempts (only past one). A value is a field:
its name in 600, a small `← source` link, its doc in meta, the value under it. Types are
noise until asked for: in the name's title, and beside every name with the one Types switch
(on the first section of values). Long values fold under a fade.

### Log
A table of seq, time, kind (12px data) and a one-line summary in plain words (a run the
runner adopted or stopped reads as a sentence; a step's message on its own thread does not
repeat the thread). On a phone the kind filter folds behind a 44px "Filter: all kinds" /
"Filter: 3 kinds" summary, so the records start near the top; its labels are 44px tall.

### Touch
At phone width every control is at least 44px tall: Pause and Archive, the nav's sections,
the log's filter labels and pager links, the inbox's status filter, a folded box's line;
the Types pill keeps its size with a clear border that makes its target 44px.

## Do's and Don'ts

### Do:
- Do say one thing per slot, and put ids and plumbing in the drawer.
- Do show state with the glyph's shape first and colour second.
- Do keep the board a server-drawn picture that works without JavaScript; the script only adds
  the drawer, tracing, live times and the frontier scroll.

### Don't:
- Don't spend red on anything but the open-inbox count.
- Don't repeat on the page what another part of it already says (no inputs table next to input
  nodes, no history table next to the log).
- Don't use uppercase labels, kickers, emoji icons or decorative motion; motion is the drawer's
  180ms slide in (the page makes room at once), the running
  spinner, the 220ms flip of a changed glyph and 150ms fades.
