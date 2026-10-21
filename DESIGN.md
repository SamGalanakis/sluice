---
name: sluice dashboard
description: A ruled sheet. cenote's paper UI in two stocks, Paper and Carbon (the default); a projects sidebar, a slim top bar of the page's sections, each page led by one sentence; rows between hairlines and space instead of boxes; Inter for words and JetBrains Mono for ids, stages and times; one accent per meaning (teal the owner, blue an agent, green running, red a failure, ochre a look) and nothing at rest casting a shadow.
colors:
  sheet: "light-dark(#f5f3ec, #0a1520)"
  paper: "light-dark(#ece9e0, #07101a)"
  field: "light-dark(#fbfaf6, #0f1f2e)"
  inset: "light-dark(#efede5, #132739)"
  wash: "light-dark(rgb(14 27 38 / .05), rgb(220 240 255 / .055))"
  active: "light-dark(rgb(14 27 38 / .075), rgb(220 240 255 / .09))"
  line: "light-dark(rgb(14 27 38 / .13), rgb(214 230 242 / .13))"
  line-2: "light-dark(rgb(14 27 38 / .24), rgb(214 230 242 / .24))"
  ink: "light-dark(#0e1b26, #e4edf3)"
  muted: "light-dark(#43505c, #afbfcc)"
  faint: "light-dark(#5b6772, #8c9dad)"
  pen: "light-dark(#1a2129, #dce6ee)"
  human: "#10c9cf"
  on-human: "#071827"
  accent: "light-dark(#066c73, #4fdce0)"
  accent-tint: "light-dark(rgb(16 201 207 / .16), rgb(16 201 207 / .18))"
  agent-ink: "light-dark(#1d56e0, #86a9ff)"
  agent-tint: "light-dark(rgb(33 98 255 / .08), rgb(33 98 255 / .16))"
  running: "light-dark(#0a6b49, #7be0b5)"
  running-tint: "light-dark(rgb(18 160 110 / .08), rgb(40 200 140 / .09))"
  danger: "light-dark(#a8261f, #ff9c94)"
  danger-tint: "light-dark(rgb(168 38 31 / .08), rgb(255 156 148 / .12))"
  overlap: "light-dark(#8f5f00, #f2c45a)"
  overlap-tint: "light-dark(rgb(143 95 0 / .12), rgb(242 196 90 / .16))"
  focus: "light-dark(#0a8f95, #10c9cf)"
typography:
  ui:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "13.5px"
    fontWeight: 400
    lineHeight: 1.5
  lead:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "15px"
    fontWeight: 400
  title:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "22px"
    fontWeight: 600
    letterSpacing: "-0.01em"
  section:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "12px"
    fontWeight: 500
  mono:
    fontFamily: "JetBrains Mono Variable, JetBrains Mono, ui-monospace, monospace"
    fontSize: "11.5px"
    fontWeight: 400
rounded:
  xs: "4px"
  sm: "6px"
  md: "8px"
  lg: "12px"
  pill: "999px"
spacing:
  row: "30px"
  topbar: "52px"
  sidebar: "248px"
  rail: "300px"
  column: "1240px"
  gutter: "32px (16px under 860px)"
motion:
  ease: "cubic-bezier(.16, 1, .3, 1)"
  fast: "150ms"
  base: "180ms"
  slow: "200ms"
  flash: "1.4s"
  hover-card: "320ms show, 160ms hide"
components:
  button-primary:
    backgroundColor: "{colors.human}"
    textColor: "{colors.on-human}"
    rounded: "{rounded.sm}"
  button:
    backgroundColor: "transparent"
    textColor: "{colors.ink}"
    border: "1px {colors.line-2}"
    rounded: "{rounded.sm}"
  badge-running:
    backgroundColor: "{colors.running-tint}"
    textColor: "{colors.running}"
    rounded: "{rounded.pill}"
  badge-agent:
    backgroundColor: "{colors.agent-tint}"
    textColor: "{colors.agent-ink}"
    rounded: "{rounded.pill}"
  float:
    backgroundColor: "{colors.field}"
    border: "1px {colors.line}"
    rounded: "{rounded.lg}"
    shadow: "--shadow-float"
---

# Design System: sluice dashboard

## Overview

**A ruled sheet.** The dashboard is a briefing for work that runs while the owner is elsewhere.
They open it between other work, often on a second screen or a phone, to answer one question
within seconds: does anything need me, and is the work moving? The look is cenote's paper UI
(its tokens, base, components and `paper-ui.md`), taken whole: a page is one sheet ruled by
hairlines, its sections found by their rules and their content rather than by boxes, every
colour a token with a meaning, and nothing at rest lifted off the page.

- **Two stocks.** Paper (light, a warm stock) and Carbon (dark, three steps of navy), each
  token written once as `light-dark()`. Carbon is the default; a browser that asks for light
  gets Paper until the owner picks one.
- **Two families.** Inter for words; JetBrains Mono for what is data: ids, stages, step names,
  functions, times and figures in a table of them.
- **One accent per meaning.** Teal is the owner (a question for them, the page's one primary
  move, links); blue is an agent and nothing else; green is running; red a failure; ochre
  needs a look (quiet, paused). Done is no colour: muted ink.
- **No resting shadow.** Only what floats casts one: a menu, a popover, the hover card, the
  tooltip, a drawer, the dialog.
- **One ease** for every transition, and reduced motion stops all of it.

A plan answers in its order, a briefing over a roster: one sentence; **Needs you** (questions,
stops and long silences, a hairline row each, a question answered in place); **Watching** (the
long runs, their latest result and their last runs); **At work** (a row a unit in creation
order, its stage track, its latest message, its pulse strip); the line of what waits and what
is done; **Finished since you looked**. From 1600px Watching and Finished stand in a rail at
the right. The design rules that keep this from eroding are tested (Rules, below).

The look before this one (cream paper under a navy bar, a raised Needs-you well, Finished
cards, the step table in every opened row, the Live badge, the legend sentence and nine extra
themes) is gone, with its tokens, templates, script and tests. Nothing is kept behind a flag.

## The generic rule

The dashboard knows only sluice's own concepts: project, recipe, unit, step, status (the status
table, `sluice_model::shown`), run, wait, message (owner, sessions, steps), progress fields,
outputs and the project's board document. Nothing in view code assumes a project's vocabulary.

- **Stages** come from a unit's recipe's own steps, in recipe order. A project with several
  recipes gets a group a recipe in At work; a unit's row draws its stages as a track.
- **Finished** has one component and one name: "Finished since you looked", or "Finished
  lately" while no look is kept. It is the units (or steps of no unit) whose run ended most
  recently, newest first.
- **A long run** (a step of no recipe running six hours or more, or one that reports
  `step_progress`) is watched, not at work: it is a line in Watching. Its progress fields show
  as it reported them, and its last results' red or green is said generically from them
  (Watching, below): sluice never names them.
- **A chip** says what a unit's steps run on: an engine's own name. A fn's row leaves the
  slot empty.
- **A reference** in prose is drawn as one only when its shape says so: a ticket by the generic
  pattern `ABC-123`, a sha by forty or seven-plus hex digits in a word of its own, a step id the
  plan knows. Each opens a hover card (Hover cards, below). Titles never carry them: a title is
  one link.
- **A shared cause** is said once for the stops it holds, from the failure kind alone; a fn's
  own failure is always its own.
- **The summary sentence** is built only from status counts, quiet runs and open questions.
- **Project-specific words** come only from what the project supplies: recipe definitions,
  step progress and outputs, its board document.
- A guard test (`tests/vocabulary.rs`) fails on one project's words anywhere in `src/`,
  `templates/` or `assets/` outside a test module; the `/_ui` gallery's sample data is an
  invented project, `almanac`, a team writing a field guide. The neutral fixture
  (`tests/neutral`, also served by `examples/dashboard_fixture.rs`) seeds `almanac`, `chores`,
  `atlas` (a plan at scale) and a project with a board, and every page renders them.

## Tokens

Every colour, size, radius, layer and duration is a token on `:root` in `assets/style.css`.
A colour is written once, `light-dark(paper, carbon)`; `color-scheme` on `<html>` picks the
side (`data-theme="paper"` light, `"carbon"` dark, none: dark unless the system asks for
light). The brand teal (`--human`, `--on-human`) is the same on both stocks.

**Surfaces.**

| Token | Paper | Carbon | Holds |
|---|---|---|---|
| `--sheet` | `#f5f3ec` | `#0a1520` | the page |
| `--paper` | `#ece9e0` | `#07101a` | the sidebar, a shade off the sheet |
| `--field` | `#fbfaf6` | `#0f1f2e` | inputs and everything that floats |
| `--inset` | `#efede5` | `#132739` | code chips |
| `--wash` | ink at 5% | chalk at 5.5% | hover |
| `--active` | ink at 7.5% | chalk at 9% | the current item, an open row |
| `--line` | ink at 13% | chalk at 13% | a hairline |
| `--line-2` | ink at 24% | chalk at 24% | a control's edge |

**Ink**, four levels: `--ink`, `--muted`, `--faint` (labels, metadata, the pulse marks) and
`--pen` (the sketches and the person's avatar).

**Meanings.** Each accent has an ink and a tint, and is read only by its own roles (Rule 11):

| Meaning | Ink / tint | Where |
|---|---|---|
| The owner | `--accent`, `--accent-tint`, `--human` | a question's mark, "1 question for you", the Inbox count, a sidebar unread dot, Answer (the one primary button), links, focus, a row's flash |
| An agent | `--agent-ink`, `--agent-tint` | the engine chip, an agent's avatar and name, a message's mark on a pulse strip |
| Running | `--running`, `--running-tint` | the stage at work and its time, the live dot, a green result |
| A failure | `--danger`, `--danger-tint` | a failed item's mark, a red result, a Danger button |
| A look | `--overlap`, `--overlap-tint` | quiet, paused, a stale or set-aside mark, "Updates paused" |

Done is never coloured: a check in muted ink.

**Type.** `--text-cap` 12, `--text-mono` 11.5, `--text-meta` 12, `--text-sm` 13, `--text-ui`
13.5 (the base), `--text-body` and `--text-lead` 15, `--text-subhead` 18, `--text-title` 22,
`--text-figure` 28. Weights 400, 500 (a section's label, a name) and 600 (a title, a row's
title). A section's label is 12px/500 in faint ink, sentence case: the lead sentence carries the
page, and the sections are found by their rules. Mono is 11.5px and only in its roles.

**Size and layer.** `--row` 30px (a menu row, a sidebar row), `--topbar` 52px, `--sidebar`
248px, `--drawer` 264px, `--rail` 300px, `--column` 1240px, `--gutter` 32px (16px under 860px),
`--measure` 68ch. Radii `--radius-xs` 4, `-sm` 6, `-md` 8, `-lg` 12 and `--pill`. Layers
`--z-sticky` through `--z-skip`, a name each.

**Motion.** `--ease` is the one easing; `--fast` 150ms, `--base` 180ms, `--slow` 200ms for
colour and folds, `--flash` 1.4s for a row's flash, `--spin` and `--breathe` the two linear
loops, `--delay-card` 320ms and `--delay-card-hide` 160ms for the hover card.

**Floats.** `--shadow-float` (a menu, a popover, the hover card, the tooltip) and
`--shadow-sheet` (the phone's drawer, the step drawer over the plan, the dialog), each over the
`--field` stock with a hairline and a 12px radius.

**Themes.** Two, Paper and Carbon, picked in display preferences and kept in the `theme`
signal and its cookie. A theme is the stock, never the components. The old theme catalogue
(Solarized, Nord, Gruvbox and the rest) is retired; a stored id from it reads as Carbon or
Paper by its scheme.

## Rules

`tests/web/design_rules.rs` reads `style.css` and fails on each break, naming its line. The
regression it catches: a hard-coded colour or a resting shadow that looks right in the theme it
was written in and wrong in the other (a `#fff` card on Carbon, a token that exists only in
Paper). Nothing else catches that short of screenshotting every page in both themes.

1. **Colour literals only in the token block.** Everywhere else a colour is `var(--…)`,
   `transparent` or `currentColor`.
2. **Every colour token has both stocks**, as `light-dark()` (the brand's teal and its ink
   aside).
3. **No undefined token.** Every `var(--x)` names one defined somewhere (or has a fallback).
4. **Sizes from the scale.** `font-size` a `--text-*` (or `em`, `inherit`), `border-radius` a
   `--radius-*` (or `50%`, `0`), `z-index` a `--z-*` (or 0, ±1).
5. **No shadow at rest.** A shadow is a ring (`0 0 0 Npx`), inset, or the float's token on a
   float.
6. **Lines are hairlines.** 1px; never dashed; dotted only on the pulse strip's tail.
7. **No glass.** No `backdrop-filter`; no filter or blend on text.
8. **No decorative gradient.** A gradient only in a mask (the tabs' fade, a strip's faded edge).
9. **Two families, mono by role.** `font-family` only from the two tokens; mono only in its
   roles (ids, stages, step names, fns, times, code and the figures of a table).
10. **Sentence case.** No `text-transform` to capitals; `letter-spacing` only from its token.
11. **One accent per meaning.** Each meaning's tokens are read only by its roles' selectors;
    the lists live in the test, so a new use of blue argues for itself in review.
12. **One ease.** Transitions and animations use `--ease` (or `linear` for a loop) and the
    duration tokens; no literal duration.
13. **Reduced motion is honoured.** A `prefers-reduced-motion: reduce` rule on `*` stops every
    transition and animation.

The kit's own guard (`tests/web/kit.rs`) holds the markup side: every heading, button, pill and
section head comes from its `views::ui` helper.

## The shell

The page's frame (`templates/layout.html`, `render_framed`) is the projects sidebar beside a
column of the top bar and the page.

- **The sidebar** (`templates/sidebar.html`, `side_nav.html`; 248px on `--paper`, a hairline
  at its right, sticky to the window): its head the current project (its mark and name) with
  sluice's mark small at the right (it goes home); the find, a field that reads "Find steps"
  with its `/` at the right (the plan's find, `q`, on a project's page; elsewhere it goes to
  the plan's find); All projects; "Projects", a row each (30px, `--wash` on hover, `--active`
  current): its mark (its emoji, its image, else its initial on a hairline tile, teal on the
  current project), its name, how many of its units are at work, and a teal dot while it needs
  the owner. Its foot: the owner's avatar and "You", then the "…" of display preferences.
- **Display preferences** (`views::prefs`, a `sluice-menu`): Theme (Paper and Carbon in place,
  applied at once), Show value types (a checkbox), Keyboard shortcuts (its `?` and a "›", a
  sub-panel in the same popover), then "States and parts" and "Agent docs". A pick posts to
  `/settings` (Writes); there is no Save.
- **The top bar** (`templates/bar.html`, `top_nav.html`; 52px on the sheet, a hairline at its
  foot, sticky): the page's sections as words (Plan, Day, Stats, Messages, Log, Functions,
  Settings; on a page of no project Day, Log, Functions), the current one on `--active`; Inbox
  at the right, its teal count of open questions (a faint dot for unread notes and no
  question). Nothing says the stream is live while it is. While it reconnects the bar carries
  one ochre pill, "Updates paused · Reconnect" (`html[data-stream]`), whose Reconnect reopens
  the stream at once. Once it has stopped, `sluice-banner` says so under the bar.
- **The column**: the bar's contents and the page share one centred column, up to `--column`
  (1240px) less the gutters, so on a wide screen the page reads as one column under the
  controls that act on it. Prose keeps `--measure`.
- **The rail** (from 1600px, on a plan the board does not stand beside): a 300px column behind
  a hairline at the right of the page's column, sticky under the bar, holding Watching, the
  waiting and done line and Finished ("Finished · since 21:30" there, the long label shortened).
  Narrower, the same blocks follow At work in the column.
- **A phone** (under 860px): the sidebar is a drawer (`--drawer`) over a scrim, cast with
  `--shadow-sheet`, opened by the menu button that leads the bar ("Open projects"), closed by
  its ×, the scrim, Escape or a link, the focus going in and coming back (`nav.js`). The bar is
  the menu button, the sections and Inbox's icon. Under 620px the sections are Plan, Day,
  Stats, Messages and "More ▾" (Log, Functions, Settings in its menu); a fade at their right
  edge shows only while they overflow (`data-tabs-fade`, `nav.js`). Every control is 44px.
- **The page's head** (`Frame::head`): on a page the bar names (a plan, home, Day, Stats,
  Messages, Log, Functions, Settings) its name is an `h1` for a screen reader alone and its lead
  line comes first, at 15px: the summary sentence on a plan and home, what the page shows
  elsewhere, with what stands beside it at its right (the plan's view switch and Details' "⋯",
  Stats' window switch). A page the bar does not name keeps its heading (`ui::page_head`):
  its crumbs, its name at 22px/600, its Details' "⋯" at the head's right edge, its status line
  under it. No page carries a description blurb.
- `main` takes the focus when the skip link sends it there and draws no ring.

**The view switch** (Plan · Both · Board on a project with a board, Plan · Board below
1280px): three ghost icons beside the plan's sentence, the current one on `--active`
(`aria-current`). The server is the view's one source: a link asks it for its view (`?view=`,
kept in the project's `sluice_view_<id>` cookie). With none chosen the stylesheet shows the
width's default: Both from 1280px, the plan between, and on a phone the board unless the plan
comes first (`data-plan-first`: something needs the owner, or the address asks the plan
something).

**Narrow end.** At 320 and 390 nothing clips and the page never scrolls sideways.

## Surfaces

There are no boxes at rest. A page is the sheet; its regions are rows between hairlines.

- **A hairline list** (`.hl`, Needs you, Watching, At work, Finished, the Done index, a step's
  calls): rows divided by `--line`, the section's first line its top.
- **A row** lights `--wash` on hover and stands on `--active` while it is open or current.
- **A module** (`ui::module_open`, `.mod`, on the grid): a part of a step or unit page (Now,
  Progress, a stop, what it waits on), its kind's line (`.mod-k`, small and faint), its title,
  its body at the measure, its actions. One that needs a look says so in ochre in its kind's
  line; nothing swells.
- **A card** (`.card`) only where a thing is a region of its own on a page of them: a project on
  home, a message, a fn, a docs topic. A hairline edge, `--radius-lg`, no shadow; never a card
  in a card.
- **What floats** (a menu, a popover, the hover card, the tooltip, the pulse popover): the
  field stock, a hairline, `--radius-lg`, `--shadow-float`.

A section's head (`ui::SectionHead`) is its label (12px/500, faint), its count after it, its
one quiet action at the right ("Set all aside", "Mark as seen"); no rule over it beyond the
list's own.

## Details

What identifies an item rather than explains it (its step, unit and run ids, its run number
and earlier runs, its fn, engine and model, its tags, its recipe, its params, a message's id, a
hash or a path) is kept behind one "⋯" at the item's end: `ui::Details`, drawn by its `menu`,
a `<details>` inside a `sluice-menu` (Escape and a click elsewhere close it and give the focus
back to its button, which says `aria-expanded`), 44px on a phone, where it opens as a sheet at
the bottom of the window. Wider, its panel hangs from its button's right edge and is kept inside
the window. Its panel lists each value with its name; an id is whole in mono with a copy button,
a description is markdown.

**A value by its shape** (`ui::ValueSet`), never its name: a number, a boolean or a short word
shows; a hash, an id, a path, a URL or a long token goes to the Details; prose shows clamped to
two lines, whole in the Details.

**Who.** Every sender and recipient is named as the identity it is (`ui::Party`, a step by its
`StepRef`), with its printed avatar (`ui::avatar`): an agent a rounded square in agent ink (two
families of rings beating into a moiré, seeded from its id); a session the same square over a
diamond with an agent-ink ring; the owner a circle with a pen-ink line-screen bust, named "you"
("You" leading a sentence). sluice is "sluice". "orchestrator", the address messages were
stored under before sessions named themselves, is shown as stored ("orchestrator, a session"
to a screen reader), never rewritten. A unit's owner is its `@name` small in muted ink; an
unowned unit says nothing. A fn step has no avatar.

## Status presentation

Status words, glyphs and ranks come from the status table (`sluice_model::shown`) only, and
status is never colour alone: every state has its Lucide glyph and its word.

**The glyph's tone** (`.g-<key>`): running, finishing and outside in `--running`, turning;
failed in `--danger`; quiet, stale and paused in `--overlap`; a question in `--accent`;
succeeded, set by hand and skipped in muted ink; pending, held, queued and blocked in faint
ink; cancelled, stopping and parked in muted ink. A parked run says "parked 12m · <what it waits
for>" where its row says when it was heard, stays in At work, and draws no quiet warning while
it waits.

**A badge** (`ui::pill`, `ui::status_pill`): a small fact on its tint, a pill: `running`
(green), `fail` (red), `look` (ochre), `agent` (blue), `ask` (teal), `done` and `settled` (the
active wash, muted), `muted` (a hairline ring). A state said as a badge is its glyph and its
words ("quiet 3h", "running 43m"); a loose step says its state once.

**The stage track** (`ui::stage_track`): a unit's stages in recipe order on one mono line,
each its name and a small mark: done a check, the one at work green with its time
("work ● 2h 44m"), the one asking the owner teal ("land ● asks you"), a stop red, a quiet one
ochre, one held, paused or bypassed its word ("fork ‖ paused", "bypass · @lead"), the rest a
faint ring. Separators are faint dashes. On a phone it shrinks to the stage at work and "2/6".
It replaces the stage strip and the step table on the plan.

**The summary sentence** (`ui::summary_sentence`): "1 question for you. 1 failed, 1 cancelled.
3 units at work: 1 quiet for 53m. 2 waiting. 3 of 10 units done; the last finished 57m ago."
It names no unit; each part is left out when it has nothing to say; the question links to where
it is answered under a teal underline. "Nothing needs you" leads it when nothing does.

## The plan

`views::plan` draws a project's plan as one ruled sheet (`templates/project.html` around it,
`sluice-board` its host). Each section is a stream region and each item and row one inside it,
so a change patches what it moved and nothing else.

- **The lead sentence**: the summary sentence, then the steps counted ("1 question for you, 2
  failed, 1 quiet. 10 at work; 73 of 92 done · 412 steps."); "Nothing needs you. 9 at work, all
  heard from in the last 24m. …" when nothing does. What the owner set aside still counts
  (setting aside is attention, never status). Under a find it says what the find matched.
- **Needs you** (`ui::need_line`): a hairline row an item, in a grid of a mark column, the
  sentence and the actions. The mark: a teal dot a question, a red square a failure, an ochre
  ring a silence, stale or a set-aside item (each said in words to a screen reader). The line:
  the unit's title (one link), its state ("failed after 30m", "asks you"), then who and when,
  faint (its owner, its step id when the title is not already it, how long ago). Under it the
  body: a question's words with its asker's avatar; a failure's headline, what it last said and
  the advice, muted. The actions: a question's Answer in place (below) and its ×; a failure's
  Retry and Retry with feedback (its field folded under the item until asked for; a failure that
  repeated opens it and offers no bare Retry); a cancel's Retry and "Settle as done"; a silence's
  Message it. The × sets an item aside, posting in place, the focus going to its Undo line.
  Questions first, then stops (a shared cause said once, its units folded, Retry all), then
  silences past the host's threshold. The head counts them and holds "Set all aside" (stops and
  silences only). Nothing to say: the section stays, drawing the empty-sheet sketch over
  "Nothing needs you" and one muted sentence ("4 items set aside: each comes back when it
  changes, or with its Undo below." when the owner set them aside). A drawn state reads as
  checked and clear, where a missing section reads as not loaded.
- **A question answered in place**: under its words the owner's avatar, a one-line field
  ("Answer <title>…") and Answer, the page's one primary (teal) button, drawn at rest. On focus
  the field grows to about four lines and says "⌘↵ answer · Esc cancel"; ⌘↵ (Ctrl+↵) sends,
  Escape empties and leaves it. A question with an OpenUI form opens its form from Answer
  (`sluice-answer`). Answered, the item gives way to the line that says so.
- **Watching** (`watching_html`): a long run a line: its title, its latest result as short
  words coloured by its verdict (a green result, a red one, else ink; a small square of the
  colour before the words, so colour is never alone), its last twenty results as 6px squares,
  oldest first, the latest taller (each titled with its time, the strip labelled with its red
  and green counts), and "last result 2h ago" (or "running 11d"). The verdict comes from the
  progress fields alone (`step::Verdict::of`): a count of failures (`failed`, `red`, `errors`
  and their like) above zero, or a status word of failure, is red; a pass count or an `ok` with
  no failure is green; else neither. Its history is the
  step's last twenty published results (`steps.progress_runs`, SPEC §6.10). A long run comes to
  Needs you only on a failure, a question or a silence past `long_run_quiet_h` (24 hours by
  default).
- **At work**: its head one line: "At work", its counts ("8 units · 1 waiting"), the order menu
  ("Order: Created ▾": Created, Plan order, Recent activity, kept per project in
  `sluice_order_<p>`), the ⓘ (its tooltip draws each glyph of a strip beside its meaning: the
  legend lives there and nowhere else), and over the strip column only, "1h ago" and "now". A
  group a recipe (a quiet fold, its name and counts; the units of no recipe after, "Other
  units"); in a big roster a group shows its remarkable rows and folds the rest to one line
  ("+7 running · each heard from in the last 11m"). Every fold preserves `open` and the
  project's session memory restores it.
- **A row** (`roster_row`, a `<details>`): a grid of the engine chip (a blue agent badge, the
  engine's name; the column kept, empty, for a fn), then the title (600, two lines at most) with
  its owner, the stage track and "heard 12m ago" on one mono line, and its latest message with
  its sender's avatar (one line); the pulse strip; the chevron. A waiting row says what it
  waits on in place of its message. The row never moves when its state changes: it flashes
  (`--accent-tint` fading over 1.4s, `data-flash`, `board.js` on a change of its `data-said`)
  where it stands; reduced motion skips it.
- **The pulse strip** (`sluice-pulse`, `unit_steps::Pulse`): the last hour, now at the right,
  built from what its agent actually did. A faint 1.5px mark each minute it acted; a message a
  blue mark, twice as tall; a dotted tail from its last action to now, faint while recent and
  ochre once quiet; a start notch where a run began inside the hour; a run that began earlier
  fades at the left edge, which says when it began when pointed at. Pointing, a tap or the
  arrows pick a minute and open the popover of what happened then (`/projects/id/<p>/pulse`).
  Under 760px the strip gives way to a 7px dot on the track's line: green while heard within
  five minutes, faint after, ochre once quiet.
- **A row open**: no box inside it; the row's `--active` runs on under it. Its conversation as
  plain text under hairlines (each message its avatar, its sender and time, its words indented
  to the name, oldest first), then its foot: "Last heard 22:11 UTC (10m ago)" and Message · Unit
  page as quiet links, Open step its one outlined move. No step table: the unit's page keeps its
  step view.
- **The line under At work**: "4 waiting on earlier work · 25 done, the last 2h ago", the done
  count linking the Done index.
- **Finished since you looked**: a hairline row each: the title (one link), its result in one
  line (its summary output, its last message, else up to two short outputs, "ready true · files
  3"; a sha as code), and at the right its time and "took 2h 19m" (under the title on a phone).
  Units with nothing to say share one muted line after. The head holds "since 14:02" and "Mark as
  seen". Seen through with nothing new: the ruled-lines sketch over "Nothing new since you
  looked" and "Seen through 22:41 · Undo".
- **Done**, asked for (Show: Done, a find, a recipe's units): the index, a dense list newest
  first in columns.
- **The board column** (a project with a board, Both from 1280px): the board's document and its
  program beside the plan behind the splitter (a hairline that lights while dragged), on the
  same sheet and hairlines, its metric tiles and tables the kit's, no gradients or fills of its
  own.
- **The glance**: the plan's tab title is "(1) ⚠4 · almanac · sluice" and its favicon wears a
  teal dot while a question waits, an ochre square while a step needs a look.

**Hover cards** (`ui::hover_card`, `components.js`): a ticket, a sha or a step id in prose is a
`.ref` with a quiet underline (a sha or an id a code chip). Pointing at one opens its card after
320ms; it hides 160ms after the pointer leaves the ref and the card; focus opens it at once and
Escape or a scroll closes it. The card (`#hover-card`, one a page, outside every patched
region) is the floating look: the mono key and a state badge, a one-line title, a meta line
(a step: its fn, its unit and how its run stands; a sha: the steps whose outputs name it; a
ticket: the units whose titles name it; each linked with its state, or a line saying none
does). Its words are asked for (`/projects/id/<p>/ref?r=step:…|sha:…|t:…`), cached a
minute.

**Empty states** (`ui::state` with a `ui::Sketch`): one of cenote's pen sketches (the empty sheet, the
ruled lines), drawn on stroke by stroke along the pen's clock and shown finished under reduced
motion, over its one line and a muted sentence.

## The step view

`views::unit_steps` (`View`) in a `sluice-steps`: a unit drawn step by step, on the unit's page
and a step's Runs tab (its own row ringed). Rows are steps in graph order, each: the rail, the
name (its stage, linking the step; a condition it runs on muted before it; a wait on another
unit under it), **On** (only when its steps run on more than one thing), **State** (glyph and
word, "bypassed", a "bypass · <who>" tag while one waits, the run number), **Progress** (only
when one reports any), the **timeline** and **Span** (mono, a running one's ticking in
`--running`).

- **The rail**: the gates between the unit's steps in `--line`; a node a step, filled muted when
  done, ringed faint when not reached, an arc in `--running` turning while it runs, ochre for a
  look, red for a failure. At most five lanes.
- **The time axis**: one axis for every row, from the unit's first start to now (rounded up so
  the server redraws it once a step, never with the clock), else to its last end. Linear; no run
  drawn under 3px; a long stretch where nothing ran collapses to a break labelled with its length.
  "now" is a `--running` line `sluice-steps` moves on the clock's tick. A bar: done muted, running
  green, a look ochre, a failure red; earlier runs at 35%.
- **Selecting**: a row is the step's link (in the drawer on the plan, its page elsewhere); the
  arrows, Home and End move between rows; the selected row stands on `--active` under a focus
  ring.
- **Narrow** (under 560px of its own width): one column, the runs a 4px bar; nothing scrolls
  sideways.

## Motion

One ease, `--ease`, for colour, background and a fold's chevron (150–200ms). A running glyph
turns (`--spin`), the live dot breathes (`--breathe`) while its unit was heard from in the last
five minutes, a changed row flashes once (`--flash`), a sketch draws on, the hover card fades in.
A step view's now line moves with the clock in steps of five seconds, never animated.
Everything else is still, and reduced motion stops all of it.

## Components

### The practices

The dashboard is built the way Datastar recommends: [the Tao of
Datastar](https://data-star.dev/guide/the_tao_of_datastar) and its references
([attributes](https://data-star.dev/reference/attributes),
[actions](https://data-star.dev/reference/actions),
[SSE events](https://data-star.dev/reference/sse_events),
[Rocket](https://data-star.dev/reference/rocket)). There are no rules of our own beside them:
each decision below names the page of the docs it follows. Script beyond Datastar is written
only where Datastar can't do the job, as a Rocket component, and the component table says why
for each.

- **The backend is the source of truth** ([Tao](https://data-star.dev/guide/the_tao_of_datastar)).
  Every page is HTML from Rust (Askama templates and `views::ui`), whole as it arrives; state
  lives in the store and the cookies; disclosure choices alone may live in project-scoped
  session memory so re-created folds keep the viewer's choice. A component never draws content.
- **Patch elements, fat morph** ([Tao: Patch Elements](https://data-star.dev/guide/the_tao_of_datastar),
  [SSE events: datastar-patch-elements](https://data-star.dev/reference/sse_events)).
  A page's stream (`streams::page_events`) renders the page again and sends each region that
  changed (`#id`, mode `outer`, the default), so the morph keeps focus, scroll and open folds;
  it never sends a fragment built for the patch. Plan group, quiet and unit folds preserve
  `open`, with project-scoped session memory restoring re-created folds; a group's initial
  default applies only before the viewer has chosen. Counts have their own regions, so count
  or activity changes alone leave the roster skeleton unchanged. A quiet page sends nothing.
- **CQRS: one read, short writes** ([Tao: CQRS](https://data-star.dev/guide/the_tao_of_datastar)).
  The read is `data-init="@get(stream, …)"` on `main`
  ([Attributes: data-init](https://data-star.dev/reference/attributes)). Every write is
  a form posted by Datastar (Writes, below), and what it changed comes back on the read.
- **No optimistic updates; say what is in flight**
  ([Tao: Loading Indicators](https://data-star.dev/guide/the_tao_of_datastar),
  [Attributes: data-indicator](https://data-star.dev/reference/attributes)).
  While a form's request runs its indicator signal is true: the form is `aria-busy`, its
  pressed button shows a quiet ring and the pointer waits. The page shows the result only
  when the stream patches it.
- **Signals sparingly** ([Tao: Signals](https://data-star.dev/guide/the_tao_of_datastar),
  [Attributes: data-signals](https://data-star.dev/reference/attributes)). The page's
  signals are only what its server needs back or what a binding shows: `ver` and `stale` (the
  version the page holds), `theme` and `types` (display preferences), `tab` (the chosen tab),
  `q` (the plan's find), `seen` (the plan's read cursor), the drawer's `step` and `sver`, each
  form's busy indicator (`_busy…`, local: Datastar sends no signal whose name starts with `_`)
  and the icon field's chosen file name (`_iconfile`). Inside a Rocket host a page-wide signal is
  bound with `__root` (`data-bind:theme__root`, `data-indicator__root`), which Rocket leaves
  unscoped ([Rocket](https://data-star.dev/reference/rocket)).
- **Stream addresses are fixed; signals carry the query**
  ([Actions: @get](https://data-star.dev/reference/actions), which sends the page's
  signals with every request). The plan's stream is one address (`stream_href`, its `?view=` and
  `show=` as drawn); the find (`data-bind:q__root`, `data-on:input__debounce.200ms`) and Mark as
  seen change their signal and open the stream again, the open one ended by Datastar's
  `requestCancellation` ('auto': a newer request to the same address from the page cancels the
  older). The page's own stream asks 'cleanup' too, so it ends with its element.
- **History is navigation only.** Datastar has no router and leaves the address to the page. The
  one thing in it beyond the server's own query is the drawer's step, `#step:<id>`: an open step
  is something to link, reload and go Back from, so it is the address's fragment, set by plain
  links (`<a data-step>`), and closing the drawer is going back to the plan's address. A tab is a
  signal (`tab`), a find is a signal (`q`); neither writes history. The drawer's stream is its
  own element (`#drawer-stream`): a new step's binding replaces the old, whose stream Datastar
  ends ('cleanup'), because `requestCancellation` 'auto' only cancels a request to the same
  address, and each step's stream has its own.
- **Keep what the owner holds through a morph**
  ([Attributes: data-preserve-attr, data-ignore-morph](https://data-star.dev/reference/attributes)).
  An open `<details>`, a host's chosen tab or width, a composer's text: `data-preserve-attr`
  on the element that holds it (`Host::keep`); the confirmation dialog `data-ignore-morph`.
  Every Rocket host keeps `data-rocket-host` (Rocket's own mark, by which it finds the host an
  element's actions run in); a morph would otherwise take it away.
- **Reconnect, and rest while hidden** ([Actions: @get options](https://data-star.dev/reference/actions)).
  Every stream asks `retry: 'always'` (ten tries, at most 30 s apart); a hidden tab's stream
  closes (Datastar's default, `openWhenHidden: false`) and reopens on return; meanwhile
  `nav.js` asks `/title` for the tab's title and favicon alone.
- **Rocket components in light DOM** ([Rocket](https://data-star.dev/reference/rocket)). A part
  with behaviour Datastar's attributes can't give is a custom element whose host the server
  draws; `rocket(tag, {mode: "light", renderOnPropChange: false, props, manifest, setup})`
  defines it. It has no `render`: `setup` adds listeners (each removed by its `cleanup`, through
  one `AbortController`), observers (disconnected) and frames (cancelled), and reads its typed
  props from the host's attributes. So a host can leave the page and come back (a morph that
  moves it) with exactly one of each. This is a deliberate departure from Rocket's own pattern,
  where a component renders its insides from its props: here the server draws every host and
  its insides, and a component only sets up behaviour. The reason is that the server is the
  page's one source. A stream's patch morphs the same HTML the first load drew, the page reads
  whole without a script, and the content is never drawn twice, once in Rust and once in
  JavaScript.

### Writes

Every write on the dashboard is a form, drawn by `ui::post_form` (`views/ui/form.rs`) and
answered by `views::write`:

```html
<form id="<id>" method="post" action="<route>"
  data-on:submit="evt.submitter?.setAttribute('data-pressed', '');
    @post(el.getAttribute('action'), {contentType: 'form',
    headers: {'sluice-form': el.id}, retry: 'never', retryMaxCount: 0})
    .catch(() => { …its notice says sluice did not answer… })
    .finally(() => evt.submitter?.removeAttribute('data-pressed'))"
  data-indicator__root="_busy<hash>" data-attr:aria-busy="$_busy<hash> && 'true'">
  …its fields, its submit…
  <p id="<id>-said" class="form-said" role="status"></p>
</form>
```

- `@post` with `contentType: 'form'` sends the form's own fields as the browser would, and
  Datastar prevents the page load
  ([Actions: @post, contentType](https://data-star.dev/reference/actions)); a form with a
  file is sent as multipart (`enctype`). `retry: 'never'` because a write is not repeated on its
  own. A write that could not be sent rejects the action (after Datastar's `retries-failed`
  event, which draws nothing), and its `.catch` says so in the form's notice as an alert:
  "Sluice did not answer: is it running? Try again in a moment." Nothing is swallowed.
- `data-indicator` sets the busy signal while the request runs, and `data-attr:aria-busy`
  draws it ([Attributes: data-indicator, data-attr](https://data-star.dev/reference/attributes)).
  The submit event's `submitter` is marked `data-pressed` until the action settles: the
  stylesheet turns a quiet ring on that button and dims the form's others, and none takes a
  second press while the form is busy.
- The answer is SSE ([SSE events](https://data-star.dev/reference/sse_events)), and only to the
  form that asked (its `sluice-form` header). It patches elements and signals, never script: a
  write taken empties the form's notice, and `sluice-dialog` closes the confirmation it was sent
  from on that answer (a `datastar-fetch` `finished` with no alert in its notice); one that says
  something ("Saved.", "Sent to @lead.") puts it in the notice as a status, and a message sent
  also empties its box by patching the box's signals (`ui::draft_signals`, bound to its words
  and its "ask" box); a refusal puts sluice's words there as an alert, what was typed kept. The
  one exception is a deleted project's page, which the answer sends home with the Datastar
  SDK's redirect (an `ExecuteScript` of `window.location`, the documented way). A refusal to a form is a 200, because Datastar applies only a 200's events; any other
  client gets 204, or the JSON error at its status. A notice is put in whole (mode `replace`),
  because a confirmation's form sits in the dialog, which no morph enters.
- Display preferences are not a form: the theme radios and the types switch are bound
  (`data-bind:theme__root`, `data-bind:types__root`) and a change posts the signals to
  `/settings` (`@post('/settings')`, JSON), answered 204 with the cookies.


### Rocket components and `ui::` helpers

Every shared part is one of two things:

- **A `ui::` helper**: a Rust function or type in `views::ui` (or `views/ui/*.rs`) that returns
  HTML, with its look in `style.css`'s kit sections, on the tokens. It has no script.
- **A Rocket component**: drawn by Rust (its host from `ui::rocket::Host`, its insides from
  helpers), enhanced by Rocket, listed in `ui::rocket::COMPONENTS` (its tag, its script, its
  props, its slots and events, which a Chromium test holds to the script's manifest), shown in
  `/_ui` in every state, and safe through a morph (its state in preserved attributes, its
  listeners set up once). A host around a part gives it no box (`display: contents`); a host that
  is its part takes the part's. Datastar attributes inside a host are Rocket's to scope: a
  signal there is the component's own unless its attribute says `__root`, and an action there
  runs in the host.

The helpers (`crates/sluice-web/src/views/`):

| Part | File | What it draws |
|---|---|---|
| Page head | `ui/head.rs` (`lead_head`, `page_head`, `page_head_with`, `page_head_html`, `page_head_note`) | on a page the bar names, its lead line with what stands beside it and its name for a screen reader; elsewhere a page's crumbs, its name at 22px/600, its Details' ⋯, its sentence or muted note |
| Status line | `ui/head.rs` (`Dot`, `status_line`) | how a thing stands: its state's glyph (the teal dot when it asks the owner) and one muted sentence |
| Section head | `ui/section.rs` (`SectionHead`, `section_head`) | a section's label (12px/500, faint, sentence case), its count, a note, one quiet action |
| Tabs | `ui/tabs.rs` (`Tab`, `tabs_open`/`close`, `panel_open`/`close`, `switch`) | tabs as words over panels, the chosen on `--active`; the messages' switch and Stats' window as links |
| Buttons | `ui/button.rs` (`Btn`, `submit`, `link`) | a ring by default (secondary), Primary the one teal move a place (Answer, a form's Save), Quiet (ghost), Danger (red ink), Icon; 30px, 44px on a phone |
| Badges | `ui/pill.rs` (`Tone`, `pill`, `pill_link`, `ask_link`) | a small fact on its meaning's tint: running, fail, look, agent, ask, done, muted; `ask_link` the way to an open question, its teal dot |
| Needs you | `ui/need.rs` (`Mark`, `NeedLine`, `need_line`, `need_said`) | an item a hairline row: its mark, its title and state, who and when, its body or a question's words with its asker's avatar, its actions and ×, a folded form |
| Question | `ui/question.rs` (`QuestionCard`, `answer_link`, `way_on`); `threads::MessageItem::question_card`; `templates/answer.html` | a question to the owner, one way everywhere: as a line it is `need_line` with `Mark::Ask` and the answer in place; as a card, its teal dot and "Question for you", who asked and when, its words whole, Answer and Close question (behind the page's confirmation) |
| Message | `ui/card.rs` (`message_card`) | a message: who to whom with their avatars, when, its words, the way to the whole |
| Avatars | `ui/avatar.rs` (`avatar`, `AvSize`) | an identity's printed mark: an agent's moiré square, a session's square on a diamond, the owner's bust |
| References | `ui/refs.rs` (`refs`, `step_ref`, `sha_ref`, `is_sha`); `ui.rs` (`hover_card`) | tickets, shas and step ids in prose as `.ref`s; the page's one hover card |
| Sketches | `ui/sketch.rs` (`Sketch`, `state`) | cenote's pen sketches over a drawn empty state |
| Tool calls | `ui/calls.rs` (`Call`, `calls`) | a turn's calls in a quiet `<details>`: the tool, its key argument on one line, how long or that it failed |
| Menus and Details | `ui/menu.rs` (`Details`); `ui.rs` (`fold`) | the ⋯ with what identifies a thing, then its actions; a plain fold |
| Empty and notice | `ui/empty.rs` (`empty`, `empty_with`, `notice`) | one line where a blank would look broken; a notice of what was not done |
| The grid | `ui/grid.rs` (`grid_open`, `module_open*`, `column_open`, `stage_track`, `summary_sentence`, `ValueSet`, `latest_list`, `clock`) | the sheet, modules, the stage track, the summary sentence, a value by its shape |
| Status and time | `ui.rs` (`status`, `glyph`, `mark`, `word`, `tries`, `duration`, `ago`, `since`, `at`, `count`, `tally`, `progress`, `bar`) | a state's glyph and word, durations and clock times as `<time>` the clock ticks |
| Who | `ui.rs` (`Party`, `StepRef`, `party_html`) | a sender or recipient as the identity it is |
| Fields | `ui.rs` (`head`, `field`, `field_start`, `field_end`); `templates/kit.html` | a form's row: its label, its control, its error |
| Confirmation | `ui.rs` (`Confirm`) in a `sluice-confirm` | the one native `<dialog>` |
| Conversation | `threads::Conversation` (`cards`, rows, the composer) | a thread's messages and its box |
| The plan's rows | `views/plan.rs` (`RowParts`, `roster_row`, `finished_row`) | At work's row and Finished's row, which the gallery draws as the plan does |

The Rocket components (`ui::rocket::COMPONENTS`; the hosts' helpers are in `ui/rocket.rs`), and
why each is script rather than Datastar attributes:

| Tag | Script | What it does | Why Datastar can't |
|---|---|---|---|
| `sluice-tabs` | `components.js` | an ARIA tablist over its panels: arrows wrap, Home and End; the choice in `$$tab` and the page's `tab` signal, its `current` kept through a patch; an anchor in a panel opens its tab | a tablist's roving focus and an anchor's tab are imperative focus and hash work across many elements |
| `sluice-fold` | `components.js` | Show all and Show less over a long text, said only when it is cut; or a More fold kept open per project or opened on a wide screen | whether a text is cut is a layout measurement, and the fold's memory is this browser's |
| `sluice-confirm` | `components.js` | its summary opens the page's one dialog with its form; disabled while its summary is `aria-disabled` | moving a form into a modal `<dialog>` and back, and `showModal`, are DOM calls |
| `sluice-dialog` | `components.js` | the page's one confirmation dialog: keep closes it, Tab stays in it, a write taken from its form closes it, closing puts the form back and the focus on its opener; a form in it runs its `@post` in its host | a focus trap and focus return are imperative; closing a modal `<dialog>` is a method call no patch makes |
| `sluice-conversation` | `components.js` | Jump to latest, and a thread's page opening at its end | scrolling to and focusing a message is imperative |
| `sluice-answer` | `components.js` | a question's Answer opens its box, drawn by `openui.js` from the question's OpenUI program; once answered the question gives way at once to the line that says so | the OpenUI program is parsed and drawn in the browser, and the answered line takes the focus |
| `sluice-menu` | `components.js` | display preferences, More, the order menu and every ⋯ and ⓘ: a click elsewhere or Escape closes it and gives the focus back, its button says `aria-expanded`, the arrows, Home and End move through it, a sub-menu opens one level at a time, the panel kept inside the window | a click elsewhere, focus return, a menu's keys and fitting the panel to the window are imperative |
| `sluice-copy` | `components.js` | an id or a sha in mono with a copy button ("Copied") | the clipboard is a browser API |
| `sluice-search` | `components.js` | the board's search and the functions' find: what does not match hides as one types, a status counts what is left | filtering hundreds of items as one types, with no request, would put an expression on every item |
| `sluice-banner` | `components.js` | the stream line once it has stopped ("Updates stopped at 14:02." with Reconnect) and the build line ("sluice was updated · Reload"); the stream's phase as `html[data-stream]`, which shows the bar's "Updates paused" pill | Datastar reports its stream's life as `datastar-fetch` events and draws nothing from them |
| `sluice-splitter` | `components.js` | the board's splitter: drag, arrows, its width kept per project | a pointer drag and a width kept in this browser |
| `sluice-keys` | `components.js` | `/` finds, `[` and `]` move the drawer, `g` then a letter goes to a page, `?` opens Keyboard shortcuts; never while typing or under a dialog | a two-key sequence and its exceptions over the whole page |
| `sluice-steps` | `components.js` | the step view: a row's click opens its step, the arrows move between rows, the selected row in `selected`; its now line and running bars move on the clock's tick | roving focus, and marks placed by time on each tick |
| `sluice-day` | `components.js` | the day line: its now rule and running bars move on the clock's tick | marks placed by time on each tick, measured against the window |
| `sluice-pulse` | `components.js` | a pulse strip: pointing, a tap or the arrows pick a minute (or the faded edge), anchor the page's popover to it and ask for what happened (`sluice-pulse`, the holder's debounced `@get`) | hit-testing a minute-wide target around a thin mark and the strip's keys are not an attribute's; the words are the server's |
| `sluice-drawer` | `sluice.js` | the step drawer: `#step:<id>` opens it beside the plan (over it below 1200px, a modal dialog; a sheet on a phone) and binds the step's stream; it follows the browser's own `hashchange` | `inert` behind a modal, its focus trap, `[` and `]` and the stream binding it swaps per step |
| `sluice-board` | `board.js` | a project's page: session memory for every Plan fold, the board column's height and its "More below" cue, a board Button's say sent in place, a row's flash on a change of its state (`data-flash`), a question's ⌘↵ and Escape, Retry with feedback's fold, a step's new state said once in `#announce`, and the focus after a form with `data-focus-after` is taken | restoring a fold after its element is re-created, a flash keyed to what changed, keys in a field, announcing changes and moving the focus |

Beside the components, two pieces of script belong to every page rather than a part:

- **`nav.js`**: the clock and the title (below), the phone's projects drawer (`data-nav-open`,
  `data-nav-close`: `html.nav-open`, Escape, a link closes it, the focus in and back), and the
  tabs' fade (`data-tabs-fade`).
- **The hover card** (`components.js`, not a component: one card a page, bound to every
  `[data-ref]` by delegation): its timing, its fetch and cache, Escape and scroll.

A write sluice never answered is said by its own form's `.catch` (Writes).

**The clock is infrastructure, not a component.** `nav.js` ticks every `<time>` (`data-since`
a two-unit duration, `data-ago` "12m ago", `data-clock` a time of day on the reader's own clock,
which the server writes in UTC and names, "17:49 UTC", so it reads right before the clock first
ticks) every five seconds and after a patch, then dispatches `sluice-tick` on `window` with the
time; a component that draws against now moves its own marks on that event. On the same tick
it keeps the pulse strips' marks (`data-a`, `data-b`, sliding left as they age) and each heard
line's and live dot's state (`data-heard`: `is-live` its first five minutes, `is-aging` from
15, `is-old` from 30). The server writes each as it read the clock, and the stream's version
leaves all of it out (SPEC §13), so a quiet page sends nothing as the clock moves. `nav.js` also
keeps the tab's title and favicon (the plan's glance) and a first open's theme.

**`/_ui`** ("States and parts") is the gallery and its documentation: each part led by what the
owner sees it as and how it is built, every helper and component in every state with the
invented project's data, Paper and Carbon side by side (stacked under 900px), and a table of
every component, its props and events. A new part goes there first. The two page components,
`sluice-board` and `sluice-drawer`, are one a page and bind the page's address and its streams,
so the gallery draws their parts rather than a second live instance; their live states are held
by the Chromium tests (`board_browser`, `live_path`).

The confirmation dialogs (Cancel, a succeeded step's Retry, Delete project, Clear board, Remove a
resource, Close all) use one native `<dialog>` (`ui::rocket::confirmation`, in a
`sluice-dialog`), opened by `ui::Confirm` in a `sluice-confirm`: the floating look with
`--shadow-sheet` and the scrim, its title naming what it acts on in words. Focus starts on the
keep button, stays in the dialog, Escape closes it and focus returns to the opener.

**Icons**: Lucide (lucide-static 1.52.0, ISC), the published SVG unmodified in `assets/icons/`,
each a `<symbol>` in the page's one sprite and drawn as a `<use>` of it in `currentColor`,
`aria-hidden`. A new icon is fetched from Lucide at that version, never drawn; the mark and
favicon are the owner's SVGs, served as they are. **Faces**: Inter and JetBrains Mono, Sluice's
own copies of the fontsource files in `assets/fonts/`, each a variable woff2 by unicode range.

## Pages

Every page is the frame (the sidebar, the top bar, display preferences, the banners:
`templates/layout.html`, `templates/sidebar.html`, `templates/bar.html`, `render_framed`)
around a page head and the page's own regions, each region a patch target of the page's stream.
A lane that redraws a page writes its notes here.

### The inventory

| Page | Route | Drawn by | Components and helpers |
|---|---|---|---|
| Home | `/` (its stream `/stream`; `/title` the tab title alone) | `views::home` | lead head (the summary sentence), section heads, For you as Needs-you lines, Today, a card a project |
| A project's plan | `/projects/id/<p>` (`?view=plan`, `both`, `board`; `/stream`; posts `/needs`, `/seen`; `/pulse`, `/ref`) | `views::board::project_page`, `views::plan`, `views::panel` (the board), `templates/project.html` | `sluice-board`, lead head, the view switch, Needs you rows, Watching, At work rows, Finished rows, the rail, `sluice-pulse`, `sluice-splitter`, `sluice-drawer`, `sluice-keys`, `sluice-menu`, `sluice-confirm`, the hover card |
| A step | `/projects/id/<p>/steps/<s>` (`/stream`; posts `/actions`), and in the drawer | `views::step`, `templates/step.html`, `step_band.html`, `activity.html` | the step head, `sluice-tabs`, Now with the tool calls fold, asking and stopped modules, message cards, `sluice-steps` (Runs), field tables, `sluice-fold`, `sluice-confirm`, the message box |
| A unit | `/projects/id/<p>/units/<u>` (`/stream`) | `views::unit_page`, `templates/unit_page.html` | page head, status line, `sluice-steps`, asking and stopped modules, `sluice-answer` |
| A run's file | `/projects/id/<p>/runs/<r>/files/<name>` | `views::step::run_file_page` | page head, the file in mono on the inset |
| Day | `/day`, `/projects/id/<p>/day` (`/stream`) | `views::day` | lead head, `sluice-day`, the timetable with its badges and folds |
| Stats | `/projects/id/<p>/stats` (`/stream`) | `views::stats` | lead head with the window switch beside it, section heads, tables, the throughput chart |
| For you, Questions, History | `/inbox`, `/questions`, `/history`, and each under `/projects/id/<p>/` (`/stream`; posts `/messages`, `/messages/read`, `/messages/close`, a reply) | `views::inbox`, `views::threads`, `templates/inbox.html`, `history.html`, `conversation.html`, `answer.html` | lead head over the messages' switch, question cards (`sluice-answer`), section heads, cards, `sluice-conversation`, `sluice-confirm` (Close all) |
| A thread | `/projects/id/<p>/thread?thread=<t>` | `views::inbox`, `templates/thread.html` | page head with Details, a conversation, `sluice-conversation`, the message box, `sluice-answer` |
| Log | `/log`, `/projects/id/<p>/log` (`/stream`) | `views::log`, `templates/log.html` | lead head, the presets, `sluice-search` filters, the records table, `sluice-copy` |
| Functions | `/fns` (`?project=`; `/fns/stream`) | `views::home`, `templates/functions.html` | lead head, `sluice-search`, section heads, a card a fn |
| Settings | `/projects/id/<p>/settings` (`/stream`; posts its fields, `/board`, `/icon`, `/delete`) | `crate::settings`, `views::project_settings`, `templates/settings.html` | the sections' index, section heads, field rows, switches, Delete, `sluice-confirm` |
| States and parts | `/_ui` | `views::gallery`, `templates/gallery.html` | every part and component, Paper and Carbon |
| Agent docs | `/docs`, `/docs/<topic>` | `views::gallery::agent_docs` | page head, a card a topic, the topic at the measure, the index sticky beside it |
| A page that cannot be drawn | any address a browser asks for that answers 4xx | `views::missing`, `templates/missing.html` | page head and note, the ways on |

### Per page

- **A project's plan**: The plan, above. The step drawer opens beside the plan from any step
  link (over it below 1200px, a sheet on a phone), on the field stock with `--shadow-sheet`.
- **A step's page**: its head (`StepView::band`, `templates/step_band.html`): its way back to
  its plan and its unit, its stage muted before its title (two lines at most), its one ⋯ (its
  Details, then its actions: Pause or Unpause, Cancel, a failure's Set it aside, its log,
  Message it), next to the drawer's ×. Under the title its status line ("Running 6h 34m · heard
  2m ago · run 2, after 1 failed"), then its words, its unit's stage track (its own stage
  marked) and its moves: Retry with feedback (its box folded under it, Retry beside it; on a
  failure that repeated the box stands open and the bare Retry is quiet), a cancel's Dismiss.
  Its tabs: Overview is modules on the sheet (the question first, answered in place; why it
  failed, or **Now**: the latest sentence the agent said at lead size, its calls in a quiet fold,
  its live progress, its messages; **Its brief** and **So far** under Now on a running step;
  beside them what it waits on, "Runs after", its progress and its key output). Activity is its
  turns as rows numbered in mono; Messages its conversation; Inputs and Outputs field tables;
  Runs its unit's step view over its runs, each run's facts with its Budget and Usage.
- **A unit's page**: its head (its way back, its glyph and title with its Details, how its steps
  stand, a short recipe view, its owner), its recipe's view whole when it says more than a line,
  then Steps: its step view across the sheet, and under it a module for each step that needs the
  owner.
- **The inbox, Questions and a thread**: the messages' switch under the lead. Questions to the
  owner come first as question cards, answered in place; then those between agents, the ones
  nobody waits on and the unread notes (Mark read after them). A conversation's messages are
  rows between hairlines, each with its avatar and Details, the message box under a hairline.
- **Home** (`/`): the lead sentence of counts alone ("3 questions for you, 2 failed, 1 quiet; 63
  running in 4 projects."); "Runner stopped" under it in ochre. For you is the Needs-you list, a
  line an open question (its teal mark, its title linking where it is answered, its project, who
  asked and when, Answer and the ×); Today a project's last day in a line. Then a card a project,
  ordered by what needs the owner: its mark and name, its state, its plan's sentence, At work's
  groups a line each (Watching its own line), "Finished lately:" and the last three, the way to
  its plan and its day. A muted line names the projects with no steps yet and another the
  archived ones.
- **Day**: the day as a timetable. The day line: a row a project, its runs as bars (done muted,
  running green, a look ochre, a failure red), the hours along the top in mono, the rule at now
  in green. The timetable: an hour column, a column a project; each run a line, its minute in
  mono, its unit's title and stage, its state as a badge at the right. A busy hour folds its
  successes; hours where nothing ran are one line ("06–17 · nothing ran").
- **Stats**: a project's numbers over a window (24 hours, 7 days, 30 days, All): Durations by
  stage (median, p90, longest in mono, the median a muted bar inside its p90), Outcomes (the
  failure rate in red when any failed), Throughput (a column a day in muted ink, the busiest in
  ink) and The slowest runs.
- **Log**: the records whole, seq, time and kind in mono; the presets and filters above.
- **Functions** (`/fns`): the picker and the find on one bar, the groups' index, each group a
  grid of fns, a broken one saying its problem first under a "cannot be loaded" look badge.
- **Settings**: its sections' index sticky at the left (a scrolling row on a phone), each section
  one form with one Save and its notice; Delete behind its confirmation; a Save never writes over
  another author.
- **History**: the threads with the owner, a module each; one with an open question says
  "Waiting on your answer" after the teal dot.
- **A page that cannot be drawn**: the head says what is missing and why; the ways on, each a
  44px link.
- **Agent docs** (`/docs`): the index a row of topic modules; a topic its markdown at the
  measure, every topic listed sticky beside it.

## Words

Historical wall-cap failures keep their cause and say “Agent runs now have no time limit.
Retry to use the current release.” (Retry all for a group); no time-limit setting is offered.
An operation timeout reads “An engine or host operation timed out.” and groups by that cause.

The UI explains nothing it shows: no captions on how to read a view, no standing hints, no
legend sentence (the ⓘ holds the strip's legend), no second sentences; state, labels and errors
only. A field's constraint is said by the error when it is broken, never under the field. One
short line where a blank would look broken, a banner that says the condition and what to do in
the fewest words, one advice line under a failure only when it says more than the Retry beside
it. Sentence case everywhere; engine names as typed. No hedging and no second clause on why.

## Accessibility

Keyboard everywhere with a visible focus ring (`--focus`, 2px, offset 2px); 4.5:1 text on every
surface in both stocks; 44px targets on a phone; usable at 390px with nothing scrolling
sideways; status never colour alone (a glyph and a word, a Watching result's square and words);
every region that updates says so politely, a step's new state once in `#announce`; every
avatar labelled with its kind; the hover card opens on focus; reduced motion honoured.

## Do's and Don'ts

### Do:
- Do let the lead sentence carry the page, and the hairlines its order.
- Do put what identifies an item behind its "⋯"; show what explains it.
- Do put what needs the owner first, and let the owner answer where they read the question.
- Do keep a row where it stands; say its change with the flash.
- Do show state with the glyph's shape and its word first, colour second.
- Do read every colour, size, radius, layer and duration from its token, and write a new colour
  token for both stocks at once.
- Do build the way Datastar recommends ([the Tao](https://data-star.dev/guide/the_tao_of_datastar),
  [attributes](https://data-star.dev/reference/attributes),
  [actions](https://data-star.dev/reference/actions), [Rocket](https://data-star.dev/reference/rocket)),
  naming the doc beside the decision; write script only where Datastar can't, and say why in
  the component table.
- Do build a part from `views::ui`; when it behaves, make it a Rocket component in
  `COMPONENTS`, shown in `/_ui` in every state, its state in preserved attributes.

### Don't (cenote's list, as sluice keeps it):
- Don't put a shadow on anything at rest, or a box where a hairline and space will do; never a
  card inside a card.
- Don't write a colour outside the token block, or a token for one stock only.
- Don't read an accent outside its meaning: no blue that is not an agent, no green that is not
  running, no red that is not a failure, no teal that is not the owner (a link, the focus and
  the flash aside).
- Don't use a gradient, glass, a blur on text, a dashed line or a heavy rule as decoration.
- Don't use a third family, mono for prose, capitals by transform or letter-spacing off its
  token.
- Don't animate on any curve but the one ease, or anything reduced motion would not stop.
- Don't add a legend, a caption or a badge that says the page is live.
- Don't draw content from script, or keep in the browser what the server should know.
- Don't name a project's own words in view code; take them from what the project supplies.
- Don't restyle or redraw the mark; it is the owner's SVG.
- Don't add grain or authorship marks: sluice has no authorship to show.
