---
name: sluice dashboard
description: Synthesis. The National Park Service Unigrid in the logo's Americana palette: a heavy navy title band with the page's name huge, cream paper below on a visible module grid, the channel's blue for what runs, coral only for a question to the owner.
colors:
  paper: "oklch(0.98 0.017 88)"
  paper-2: "oklch(0.955 0.022 86)"
  paper-raised: "oklch(0.993 0.008 88)"
  ink: "oklch(0.22 0.07 262)"
  ink-muted: "oklch(0.46 0.05 262)"
  navy: "oklch(0.308 0.112 262)"
  navy-deep: "oklch(0.22 0.09 262)"
  on-navy: "oklch(0.98 0.017 88)"
  rule: "oklch(0.308 0.112 262 / 22%)"
  rule-faint: "oklch(0.308 0.112 262 / 11%)"
  band: "oklch(0.308 0.112 262)"
  band-ink: "oklch(0.98 0.017 88)"
  band-muted: "oklch(0.98 0.017 88 / 74%)"
  band-rule: "oklch(0.98 0.017 88 / 30%)"
  band-accent: "oklch(0.82 0.09 232)"
  run: "oklch(0.5 0.19 258)"
  on-run: "oklch(0.98 0.017 88)"
  run-ink: "oklch(0.42 0.17 258)"
  focus: "oklch(0.5 0.19 258)"
  sky: "oklch(0.94 0.03 232)"
  sky-ink: "oklch(0.42 0.09 240)"
  sweep: "oklch(0.82 0.09 232)"
  sand: "oklch(0.85 0.09 80)"
  on-sand: "oklch(0.22 0.07 262)"
  sand-pale: "oklch(0.95 0.04 84)"
  sand-ink: "oklch(0.47 0.1 62)"
  sand-mark: "oklch(0.47 0.1 62)"
  coral: "oklch(0.69 0.2 30)"
  on-coral: "oklch(0.2 0.08 262)"
  idle: "oklch(0.52 0.03 262)"
  logo-blue: "oklch(0.617 0.2 257)"
  paper-dark: "oklch(0.235 0.068 263)"
  paper-2-dark: "oklch(0.27 0.072 263)"
  paper-raised-dark: "oklch(0.285 0.075 263)"
  ink-dark: "oklch(0.955 0.022 88)"
  ink-muted-dark: "oklch(0.8 0.035 88)"
  navy-dark: "oklch(0.955 0.022 88)"
  on-navy-dark: "oklch(0.235 0.068 263)"
  band-dark: "oklch(0.17 0.055 264)"
  run-ink-dark: "oklch(0.76 0.12 252)"
  focus-dark: "oklch(0.72 0.15 250)"
  sky-dark: "oklch(0.34 0.06 240)"
  sky-ink-dark: "oklch(0.84 0.07 232)"
  sand-pale-dark: "oklch(0.3 0.045 72)"
  sand-ink-dark: "oklch(0.84 0.11 80)"
  idle-dark: "oklch(0.7 0.03 262)"
typography:
  display:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, system-ui, sans-serif"
    fontSize: "150px"
    fontWeight: 900
    lineHeight: "0.8"
    letterSpacing: "-0.04em"
  display-phone:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "88px"
    fontWeight: 900
    lineHeight: "0.8"
    letterSpacing: "-0.04em"
  section:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "30px"
    fontWeight: 850
    lineHeight: "1"
    letterSpacing: "-0.03em"
  question:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "23px"
    fontWeight: 800
    lineHeight: "1.18"
    letterSpacing: "-0.022em"
  summary:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "19px"
    fontWeight: 650
    lineHeight: "1.3"
    letterSpacing: "-0.01em"
  module-title:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "16px"
    fontWeight: 750
    lineHeight: "1.25"
  body:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "15px"
    fontWeight: 400
    lineHeight: "22px"
  meta:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: "18px"
  cell-time:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "20px"
    fontWeight: 800
    lineHeight: "1.05"
  data:
    fontFamily: "JetBrains Mono Variable, JetBrains Mono, ui-monospace, monospace"
    fontSize: "12.5px"
    fontWeight: 400
    lineHeight: "18px"
  data-large:
    fontFamily: "JetBrains Mono Variable, JetBrains Mono, ui-monospace, monospace"
    fontSize: "22px"
    fontWeight: 600
    lineHeight: "1.05"
    letterSpacing: "-0.02em"
rounded:
  none: "0"
spacing:
  hair: "4px"
  xs: "6px"
  sm: "8px"
  md: "12px"
  gutter: "16px"
  lg: "22px"
  xl: "30px"
  frame-margin: "32px"
  frame-margin-phone: "16px"
grid:
  frame: "1440px"
  columns: 12
  columns-phone: 6
  column-gap: "16px"
  column-gap-phone: "8px"
  phone-below: "760px"
components:
  band:
    backgroundColor: "{colors.band}"
    textColor: "{colors.band-ink}"
    padding: "0 32px"
  band-nav:
    textColor: "{colors.band-ink}"
    height: "54px"
  inbox-count:
    backgroundColor: "{colors.coral}"
    textColor: "{colors.on-coral}"
    rounded: "{rounded.none}"
    height: "20px"
  section-head:
    typography: "{typography.section}"
    textColor: "{colors.ink}"
    padding: "0 0 10px"
  module:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    rounded: "{rounded.none}"
  module-ask:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    rounded: "{rounded.none}"
    padding: "16px 0 0"
  module-look:
    backgroundColor: "{colors.sand-pale}"
    textColor: "{colors.ink}"
    rounded: "{rounded.none}"
    padding: "14px 16px 16px"
  module-margin:
    backgroundColor: "{colors.sky}"
    textColor: "{colors.ink}"
    rounded: "{rounded.none}"
    padding: "16px 14px 20px"
  cell-done:
    backgroundColor: "{colors.sky}"
    textColor: "{colors.ink}"
    height: "56px"
  cell-run:
    backgroundColor: "{colors.run}"
    textColor: "{colors.on-run}"
    height: "56px"
  cell-look:
    backgroundColor: "{colors.sand}"
    textColor: "{colors.on-sand}"
    height: "56px"
  overrun-chip:
    backgroundColor: "{colors.sand}"
    textColor: "{colors.on-sand}"
    rounded: "{rounded.none}"
    padding: "1px 6px"
  button-primary:
    backgroundColor: "{colors.navy}"
    textColor: "{colors.on-navy}"
    rounded: "{rounded.none}"
    height: "44px"
    padding: "0 18px"
  button:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    rounded: "{rounded.none}"
    height: "44px"
    padding: "0 18px"
  ask-tag:
    backgroundColor: "{colors.coral}"
    textColor: "{colors.on-coral}"
    rounded: "{rounded.none}"
    height: "20px"
---

# Design System: sluice dashboard

## Overview

**Synthesis.** The dashboard is a park map for work. Its parent is the National Park Service
Unigrid (Massimo Vignelli, 1977): a heavy black title band carrying the place's name huge, a
module grid everything sits on, and information set in one grotesque at a few decisive sizes.
Synthesis keeps that structure and sets it in the logo's Americana palette: the band is the
logo's navy, the paper its cream, the channel's blue is the one active colour, sky is done,
sand needs a look, and coral is spent on one thing only, an open question to the owner.

The owner opens the dashboard between other work, often on a second screen or a phone, to
answer one question within seconds: does anything need me, and is the work moving? The page
answers in its order. The band says it in one sentence; under it the paper lists what needs the
owner first and what is done last: **For you** (a question to the owner) → **Stopped**
(failed, cancelled, stale) → **Running** (quiet first, then the longest past its usual time) →
**Waiting** → **Done** (a dense index, folded). The module that needs the owner swells. A
running step shows its elapsed time against its stage's usual time and says when it is over
("2.1× usual"). Selecting a unit traces it: it opens in place, its upstream and downstream
chain lights with a rail in the margin, a sentence states the chain, and the rest fades.

The old world (rounded cards on cream, three racing stripes under the nav, Archivo and Public
Sans, seven themes, a green for success and a plum for paused) is gone; nothing of its look is
kept.

## The generic rule

The dashboard knows only sluice's own concepts: project, recipe, unit, step, status (the status
table, `sluice_model::shown`), run, wait, message (owner, orchestrator, steps), progress fields,
outputs and the project's board document. Nothing in view code assumes a project's vocabulary.

- **Stage columns** come from a unit's recipe's own steps, in recipe order. A project with
  several recipes gets a block per recipe, each with its own columns. A unit of no recipe draws
  its own small graph; a one-step unit is one cell.
- **"Recently finished"** is the units (or steps of no unit) whose run ended most recently,
  with when and how long they took.
- **A long run** (a step running far past any usual time, or reporting `step_progress`) shows
  its progress fields in the margin module, as it reported them: sluice never reads or names
  them.
- **The summary sentence** is built only from status counts, overruns, quiet runs and open
  questions, naming recipes by their own names.
- **Project-specific words** come only from what the project supplies: recipe definitions
  (step names, title template, `view`), step progress and outputs, its board document.
- A guard test (`tests/vocabulary.rs`) fails on one project's words (its ticket prefix, its
  name, its tools, its stage words) anywhere in `src/`, `templates/` or `assets/` outside a
  test module; the `/_ui` gallery's sample data is an invented project, `almanac`, a team
  writing a field guide. The neutral fixture (`tests/neutral`, also served by
  `examples/dashboard_fixture.rs`) seeds `almanac` and `chores`, and every page renders them.

## Palette

Roles, not hues. Each is a token on `:root` (`assets/style.css`), `light-dark()` of its light
and dark value; with no theme chosen a page follows the system, and the display preferences
offer Light and Dark.

- **Paper** (`paper`, cream): the page. `paper-2` is a sunk well and a hover; `paper-raised` a
  menu or the drawer.
- **Ink** (`ink`) for text, `ink-muted` for meta (4.5:1 on paper and on the wells).
- **Navy** (`navy`): the heavy rules (a band section's 3px head rule, a stopped module's 2px),
  the filled button, the selected ring. In the dark it turns cream, so a rule stays the
  heaviest thing on the paper.
- **The band** (`band`, `band-ink`, `band-muted`, `band-rule`, `band-accent`): the logo's navy
  in the light, a deeper navy than the paper in the dark, cream text, sky for what the band
  links to and how long things took.
- **Run** (`run`): the channel's blue, the one active colour: a running cell's fill (cream on
  it), the overlay's tag, the grid switch pressed. `run-ink` is the blue as text, `focus` the
  focus ring.
- **Sky** (`sky`): done. A done cell's fill; `sky-ink` its glyph; `sweep` the bright sky of the
  sweep along a running cell's foot and a done mark.
- **Sand** (`sand`, `sand-pale`, `sand-ink`, `sand-mark`): needs a look: failed, cancelled,
  stale, quiet, an overrun. A look cell and the overrun chip are sand with navy on them; a
  stopped module sits on `sand-pale`.
- **Coral** (`coral`, `on-coral`): only an open question to the owner (the Inbox count, the
  question module's rule, "Awaiting your reply", the summary's link to it) and the logo. A
  failure is never coral.

**The One Coral Rule.** Coral marks a question waiting on the owner, nothing else.
**The One Blue Rule.** Blue fills only what is running now (and the grid switch while it is
pressed, which is the construction's own colour).

Dark is navy paper (`paper-dark`), never black; the band goes deeper than the paper so it still
reads as the heaviest thing on the page. Text holds 4.5:1 in both, large text 3:1.

## Type

One family: **Schibsted Grotesk** (variable, 400 to 900) for display and text, with
**JetBrains Mono** for ids, durations, clock times and data only. Both load from
cdn.jsdelivr.net (`@fontsource-variable/schibsted-grotesk@5.3.0`,
`@fontsource-variable/jetbrains-mono@5.3.0`), the only third-party requests a page makes.

- The band's name: 150px, 900, line-height 0.8, -0.04em (88px on a phone); a longer name
  steps down (`long` 104px from 7 characters, `longer` 76px across the band from 13,
  `longest` 52px from 21) and wraps anywhere rather than run off.
- A band section's head: 30px, 850, -0.03em. A question's title in its module: 23px, 800. The
  summary sentence: 19px, 650. A module's title: 16px, 750, two lines at most.
- Body 15/22; meta 13/18 in `ink-muted`; a running cell's time 20px 800.
- Data: JetBrains Mono 12.5px; a clock time in the band's strip 22px 600.
- Tabular figures only on numeric data (`time`, durations, counts, `.num`), never page-wide:
  Schibsted's tabular figures widen its punctuation.
- Long tokens (ids, paths) wrap (`overflow-wrap: anywhere`), so a page never scrolls sideways.
- No uppercase labels, no kickers above a heading.

## The grid

A frame 1440px wide at most, centred, with a 32px margin (16px on a phone): its columns are
`--column` (1376px) at most. Every page's content and the band's content share those edges.

- **Twelve columns**, 16px apart (six columns, 8px apart, below 760px). Modules span whole
  columns (`ui::module_open(span, …)`); on a phone every module spans all six.
- **Show grid** (`ui::grid_toggle`, in the page's row under the band) draws the real
  construction over the sheet (`ui::grid_open`, `sluice-grid`): each column tinted with the
  logo's blue and numbered above in data mono, each module's span named in its corner ("6
  col"). It is kept in the browser until switched off. It is a way of looking, so it needs
  script and is not there without it.
- A plan's stages sit on fixed columns: a band's strip head (`ui::strip_head`) names each
  stage over its column, and every unit's strip under it uses the same columns.

## The band

The page header is the navy title band (`templates/layout.html`), across the window:

- **Its top row** (`nav#top-nav`, 54px): the mark and "sluice", "Projects" (on a project's
  pages), the project switcher (the project's name, or "Projects" on the index), Inbox with its
  coral count, then at the right the live line (a blue square and "Live" while the page's
  stream is, from `html[data-stream]`) and display preferences. The current place carries a
  3px underline in the logo's blue. On a phone the row wraps: the live line and display
  preferences take a second row at the right.
- **Its head** (`Frame::head`, drawn with `render_framed`): the page's name huge at the left
  on the grid (five columns), its description in `band-muted` and its summary sentence beside
  it (`ui::band_head`); then a strip under a `band-rule`: "Recently finished"
  (`ui::recent_strip`), up to five units, newest first, each its clock time big, its name, its
  title and how long it took. On a phone the name stands over its words, everything one column,
  and the strip is a list of rows.
- **Under it on the paper**, the page's own row (`.subnav`): its sections as tabs (Plan,
  Messages, Log, Functions, Settings; the current one under a 2px ink bar), a count line
  (`Frame::meta`), and at the right its tools (`Frame::tools`: a find field, the grid switch).
  A hairline closes it.

A page without its own head draws the band's top row alone and its heading on the paper.

## Modules

A module is a unit, a question or a step on the grid: an `article` spanning whole columns,
square, with a rule over it. Its parts: `.mod-k` (its kind's line: a glyph, a word, who and
when), `.mod-t` (its title), `.mod-meta` (its id in ink and its facts), a stage strip,
`.mod-body` (prose at the measure), `.mod-actions` (44px buttons).

- **The swell**: the module that needs the owner grows. A question to the owner
  (`Swell::Ask`) takes six columns under a 6px coral rule, its title at 23px, its question
  whole with Answer first. A stopped unit (`Swell::Look`) sits on `sand-pale` under a 2px navy
  rule, with Retry with feedback first. A long run (`Swell::Margin`) is the margin module on
  sky under a 3px navy rule. Anything else (`Swell::Plain`) is a row under a hairline.
- **A band section's head** (`ui::section_head`): For you, Stopped, Running, Waiting, Done,
  each under a 3px navy rule, its name at 30px at the left and its count line at the right
  ("1 failed · 1 cancelled").
- **The margin module** (`ui::margin_module`): two columns at the sheet's right; a long run's
  name and words, its running time on the blue with the sweep, then each progress field as it
  reported it (the first one large), when it last reported and a link to its page.

## Status presentation

Status words, glyphs and ranks come from the status table (`sluice_model::shown`) only, and
status is never colour alone: every state has its Lucide glyph and its word.

**The stage strip** (`ui::stage_strip`): a unit's stages in recipe order, one 56px cell each
(48px on a phone), the cell's kind read from the status table (`ui::Cell::of`):

- **Not reached**: an outline in `rule` with the stage's name; a waiting state that says
  itself (held, queued, paused, blocked) writes its glyph and word in it.
- **Done** (the Done band): sky, its glyph (check, set by hand, skipped) and its name, how long
  it took under it.
- **Running** (the rest of the Running band: running, finishing, stopping, outside): the blue,
  its name small, its elapsed time big and ticking, a sweep along its foot; past its stage's
  usual time a sand corner says "2.1×".
- **Needs a look** (a state that wants attention: failed, cancelled, stale, quiet): sand, its
  glyph, its name and its word, its time; a quiet run keeps running's blue edge.
- A note may span the cells not reached ("then review and publish").

Each cell's state is said in words to a screen reader, and a cell with a page links to it.
`ui::stage_marks` draws the same strip as 12px squares in a line of words; `ui::overrun` is
the sand chip with the timer, "2.1× usual".

### Status ramp

Each state, in the table's priority order, with its glyph's tone (`.g-<key>`) and the cell it
draws:

- **Failed**: Lucide `circle-x`, solid, in ink; a look cell on sand. Never coral.
- **Cancelled**: `circle-stop` in muted ink, a stop on purpose; a look cell.
- **Stale**: `rotate-cw` in `sand-ink`; a look cell.
- **Quiet**: `hourglass` in `sand-ink`; a look cell with running's blue edge.
- **Blocked**: `circle-minus` in ink; not reached, saying "blocked".
- **Stopping**: `circle-stop`, turning, in muted ink; a running cell.
- **Finishing**: `loader-circle`, turning, in `run-ink`; a running cell.
- **Running**: `loader-circle`, turning, in `run-ink`; a running cell.
- **Outside** (`external`): `square-arrow-out-up-right` in `run-ink`; a running cell.
- **Paused**: `circle-pause` in muted ink; not reached, saying "paused".
- **Held**: `circle-dot-dashed` in `idle`; not reached, saying "held".
- **Queued**: `circle-ellipsis` in `idle`; not reached, saying "queued".
- **Pending**: `circle-dashed` in `idle`; not reached.
- **Set by hand** (`manual`): `circle-dot` in `sky-ink`; a done cell.
- **Succeeded**: `circle-check`, solid, in `sky-ink`; a done cell.
- **Skipped**: `circle-slash` in `idle`; a done cell.

### Named Rules

- **The Shape Carries It Rule.** Every state has its glyph and its word; colour comes second.
- **The One Coral Rule.** Coral is an open question to the owner and the logo, nothing else.
- **The One Blue Rule.** Blue fills only what runs now.

**The summary sentence** (`ui::summary_sentence`): "1 question for you. 1 failed, 1
cancelled. 2 article and 1 scan at work: s-3 quiet for 53m, a-12 at 2.1× its usual time. 2
waiting. 3 of 10 units done; the last finished 57m ago." Each part is left out when it has
nothing to say; the question links to where it is answered, under a coral underline.

## Select to trace

`sluice-trace` (`ui::trace_open`) around the units it traces, its line first: "Select any unit
to trace what it waits for and what waits on it." Each unit carries its whole chain in its
markup (`ui::Trace::attrs`: `data-up`, `data-down`, `data-chain`), so the trace fetches
nothing. Selecting a unit (its `[data-trace-pick]` button):

- opens its `[data-trace-more]` in place and rings it in navy;
- marks every unit selected, upstream or downstream (a navy chip says which,
  `ui::trace_role`), and fades the rest to 25%;
- draws the rail in the margin at the units' left, from the first lit unit to the last, a
  square at each (filled for the selected), through the section heads between;
- puts the chain's sentence in the line ("Tracing a-13. It waits for a-12 and s-3. a-14 waits
  on it."), politely, with Clear trace beside it.

Selecting it again, Clear trace or Escape clears and gives the focus back to its button; the
arrows move between the units' buttons. The choice is the host's `selected`, kept through a
stream patch and drawn again after one. Without script the units link to their pages.

## Motion

One moment, the work moving: the sweep along a running cell's foot (3.2s, ease-out, the
bright sky crossing the blue). The trace's fades (0.35s ease-out) and the rail drawing down
(0.45s) answer a selection. Everything else is still. Under reduced motion the sweep is a
static line and nothing draws or fades.

## Components

**The rule: Rust renders, Rocket behaves.** Every shared part is server-drawn from one place:
`views::ui` (status, time, count, the module grid and every Synthesis part in `ui::grid`, tabs,
fold, empty state, confirmation, and every component's host in `ui::rocket`),
`templates/kit.html` (the field row) and `views::threads::Conversation` (messages);
`style.css`'s kit sections give them their look, on this file's tokens. A part with behaviour
is a component: a custom element (`sluice-*`) whose host the server draws around the part, and
a Rocket component of the same name (Datastar's custom-element API, bundled in
`datastar-rocket-1.0.4.js`) defined in `assets/components.js` (the board and the drawer in
`assets/sluice.js`), in light DOM, with typed props (its host's attributes) and a manifest (its
slots and events). A component only enhances what the server drew, in its `setup`: listeners,
observers, keyboard handling, and state kept in its host's own attributes, which a stream patch
leaves as they are (`data-preserve-attr`). It never draws content: a page reads whole before
its script runs and without it, a patch morphs server HTML into server HTML, and a screen
reader hears what the server wrote; the only words a component writes are chrome with no
meaning without script ("Copied", the trace's sentence, which the server carries in
`data-chain`). A control that needs script (`.needs-js`) is hidden until its component is
defined. A host around a part gives it no box (`display: contents`); a host that is its part
takes the part's. A page makes a host only through `ui::rocket` and `ui::grid`, and Datastar
attributes stay outside a host.

**`/_ui`** ("States and parts") is the kit's gallery and its documentation: each part led by
what the owner sees it as and how it is built, every part and component in every state with
the invented project's data, light and dark side by side (stacked under 900px, and stacked at
every width for a part that needs the grid's room), and a table of every component, its props
and events (a Chromium test holds each to its script's manifest). A new part goes there first.
The display preferences end with the Keys, "What each state and part means" (the gallery) and
"Agent docs" (`/docs`).

The components:

- **`sluice-grid`**: the module grid's construction under its show-grid switch (above).
- **`sluice-trace`**: select to trace (above).
- **`sluice-tabs`**: an ARIA tablist over its panels (arrows wrap, Home and End); the choice in
  `$$tab`, its `current`, and with `url` in `?tab=`; without script every panel stands stacked.
- **`sluice-fold`**: Show all and Show less over a long text, said only when it is cut; or a
  More fold kept open per project or opened on a wide screen.
- **`sluice-confirm`**: the confirmation dialog (below).
- **`sluice-conversation`**, **`sluice-composer`**, **`sluice-answer`**: a conversation (Jump
  to latest, Mark read), the message box (sends as JSON, keeps its text through a patch) and a
  question's Answer and Close.
- **`sluice-menu`**: the project switcher, display preferences and the plan's More: a click
  elsewhere or Escape closes it; the arrows, Home and End move through it.
- **`sluice-copy`**: an id or a SHA in data mono with a copy button.
- **`sluice-toggle`**: a display setting applied at once and kept by `/settings`.
- **`sluice-search`**: the board's search, the functions' find, a form of filters.
- **`sluice-banner`**: the stream line ("Updates paused. Reconnecting…", "Updates stopped at
  14:02." with Reconnect) and the build line ("sluice was updated · Reload"); the stream's
  phase is also `html[data-stream]`, which the band's live line reads.
- **`sluice-keys`**: `/` finds, `[` and `]` move the drawer, `g` then a letter goes to a page,
  `?` lists them; never while typing or under a dialog.
- **`sluice-splitter`**, **`sluice-board`**, **`sluice-drawer`**: the board's splitter, the
  plan's lines and the step drawer.

The page's clock is no component: `nav.js` ticks every `<time>` (`data-since` a two-unit
duration, `data-ago` "12m ago", `data-clock` a time of day on the reader's own clock), the
server writing each as it read the clock, and its text is left out of the stream's version,
so a quiet board sends nothing.

The parts:

- **Buttons**: square, 44px on a phone. The next move is filled navy with cream (`.primary`);
  others carry a 1.5px navy line; a delete is the deeper navy (`.danger`); a quiet action is
  muted words. Hover deepens; the focus ring is 2px in `focus`, 2px off.
- **Tags** (`ui::tag`): 20px, square, a hairline; `attn` is a sand chip, `live` the blue,
  `ask` coral ("Awaiting your reply", the only coral tag).
- **Icons**: Lucide (lucide-static 1.52.0, ISC), the published SVG unmodified in
  `assets/icons/`, each a `<symbol>` in the page's one sprite and drawn as a `<use>` of it in
  `currentColor`, `aria-hidden`. A new icon is fetched from Lucide at that version, never
  drawn; the mark and favicon are the owner's SVGs, served as they are.
- **Confirmation dialogs**: Cancel, a succeeded step's Retry, Delete project (a single
  confirm), Clear board, Remove a resource and Close all use one native `<dialog>`, drawn by
  `ui::Confirm` in a `sluice-confirm`: square, on `paper-raised` with the lift and the scrim,
  its title ending in its question mark with the id under it in data mono. Focus starts on the
  keep button, stays in the dialog, Escape closes it and focus returns to the opener. Without
  script a details fold shows the same form inline.
- **Live updates**: each page's stream patches only the regions that changed, and a quiet
  board sends nothing. A patch never resets what the owner holds.

## Pages

The pages compose these parts; each lane that redraws one writes its section here.

- **A project's plan** (`/projects/id/<p>`): the band with the project's name, its summary
  sentence and Recently finished; the row with Plan, Messages, Log, Settings, the count line,
  Find and Show grid; the trace line; then on the sheet For you (six columns) beside Stopped
  (four), the margin module in the last two, Running and Waiting as strip tables per recipe,
  Done folded to an index. The matrix and the plan list live units only.
- **A step, its thread, the inbox**: Unigrid's step board and phone question, on the same
  band and grid.
- **Home** (`/`): the projects as a cover and an index; the log, functions and settings on the
  same frame.

## Accessibility

Keyboard everywhere with a visible focus ring; 4.5:1 text in both themes; 44px targets on a
phone; usable at 390px with nothing scrolling sideways; status never colour alone; every
region that updates says so politely; reduced motion honoured.

## Do's and Don'ts

### Do:
- Do set the page's name huge in the band and let the grid carry the order.
- Do put what needs the owner first, and let that one module swell.
- Do show state with the glyph's shape and its word first, colour second.
- Do keep every page a server-rendered picture that works without JavaScript.

### Don't:
- Don't spend coral on anything but an open question waiting on the owner (and the logo).
- Don't fill anything blue that is not running.
- Don't round a corner, draw a card inside a card or add a stripe.
- Don't name a project's own words in view code; take them from what the project supplies.
- Don't restyle or redraw the mark; it is the owner's SVG.
- Don't add a legend; each state says itself.
