---
name: sluice dashboard
description: A calm board for one person supervising many agents, in the logo's world: navy ink on cream, the logo's blue for what runs, coral only for what needs you.
colors:
  canvas: "oklch(0.98 0.017 88)"
  ink: "oklch(0.308 0.112 262)"
  card: "oklch(0.995 0.007 88)"
  quiet-fill: "oklch(0.95 0.022 88)"
  secondary-fill: "oklch(0.925 0.028 88)"
  muted-ink: "oklch(0.46 0.06 262)"
  accent: "oklch(0.617 0.2 257)"
  accent-ink: "oklch(0.52 0.19 257)"
  primary: "oklch(0.55 0.2 257)"
  primary-ink: "oklch(0.995 0.007 88)"
  hairline: "oklch(0.308 0.112 262 / 11%)"
  hairline-strong: "oklch(0.308 0.112 262 / 24%)"
  ring: "oklch(0.617 0.2 257)"
  edge: "oklch(0.46 0.07 258 / 45%)"
  box: "oklch(0.945 0.024 240)"
  nav: "oklch(0.308 0.112 262)"
  nav-ink: "oklch(0.975 0.017 88)"
  nav-muted: "oklch(0.84 0.045 245)"
  stripe-1: "oklch(0.617 0.2 257)"
  stripe-2: "oklch(0.82 0.09 232)"
  stripe-3: "oklch(0.85 0.09 80)"
  heading-accent: "oklch(0.48 0.14 258)"
  status-active: "oklch(0.617 0.2 257)"
  status-idle: "oklch(0.6 0.025 262)"
  status-success: "oklch(0.53 0.14 150)"
  status-attention: "oklch(0.53 0.12 78)"
  status-paused: "oklch(0.5 0.13 330)"
  badge: "oklch(0.69 0.2 30)"
  badge-ink: "oklch(0.2 0.06 262)"
  canvas-dark: "oklch(0.2 0.06 263)"
  ink-dark: "oklch(0.955 0.022 88)"
  card-dark: "oklch(0.27 0.07 263)"
  box-dark: "oklch(0.24 0.075 257)"
  quiet-fill-dark: "oklch(0.235 0.066 263)"
  muted-ink-dark: "oklch(0.8 0.035 88)"
  accent-dark: "oklch(0.64 0.195 256)"
  accent-ink-dark: "oklch(0.76 0.12 252)"
  status-idle-dark: "oklch(0.66 0.03 262)"
  status-success-dark: "oklch(0.76 0.16 150)"
  status-attention-dark: "oklch(0.83 0.14 85)"
  status-paused-dark: "oklch(0.76 0.11 330)"
  heading-accent-dark: "oklch(0.82 0.08 235)"
typography:
  title:
    fontFamily: "Archivo Variable, Archivo, ui-sans-serif, system-ui, sans-serif"
    fontSize: "28px"
    fontWeight: 800
    lineHeight: "34px"
    letterSpacing: "-0.015em"
  title-phone:
    fontFamily: "Archivo Variable, Archivo, ui-sans-serif, system-ui, sans-serif"
    fontSize: "24px"
    fontWeight: 800
    lineHeight: "30px"
  wordmark:
    fontFamily: "Archivo Variable, Archivo, ui-sans-serif, system-ui, sans-serif"
    fontSize: "22px"
    fontWeight: 800
    lineHeight: "22px"
    letterSpacing: "-0.025em"
  drawer-title:
    fontFamily: "Archivo Variable, Archivo, ui-sans-serif, system-ui, sans-serif"
    fontSize: "22px"
    fontWeight: 800
    lineHeight: "26px"
  section:
    fontFamily: "Archivo Variable, Archivo, ui-sans-serif, system-ui, sans-serif"
    fontSize: "18px"
    fontWeight: 750
    lineHeight: "24px"
  item-title:
    fontFamily: "Archivo Variable, Archivo, ui-sans-serif, system-ui, sans-serif"
    fontSize: "17px"
    fontWeight: 750
    lineHeight: "23px"
  step-id:
    fontFamily: "Archivo Variable, Archivo, ui-sans-serif, system-ui, sans-serif"
    fontSize: "14.5px"
    fontWeight: 600
    lineHeight: "20px"
  body:
    fontFamily: "Public Sans Variable, Public Sans, ui-sans-serif, system-ui, sans-serif"
    fontSize: "15px"
    fontWeight: 400
    lineHeight: "22px"
  small:
    fontFamily: "Public Sans Variable, Public Sans, ui-sans-serif, system-ui, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: "20px"
  meta:
    fontFamily: "Public Sans Variable, Public Sans, ui-sans-serif, system-ui, sans-serif"
    fontSize: "13px"
    fontWeight: 500
    lineHeight: "18px"
  data:
    fontFamily: "ui-monospace, SFMono-Regular, Cascadia Code, Liberation Mono, Menlo, monospace"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: "18px"
rounded:
  sm: "5px"
  md: "10px"
  lg: "14px"
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
    padding: "16px 18px"
  step-bubble:
    backgroundColor: "{colors.card}"
    textColor: "{colors.ink}"
    typography: "{typography.step-id}"
    rounded: "{rounded.pill}"
    padding: "7px 14px 7px 10px"
  board-box:
    backgroundColor: "{colors.box}"
    rounded: "{rounded.lg}"
    padding: "16px 18px"
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
    textColor: "{colors.badge-ink}"
    typography: "{typography.meta}"
    rounded: "{rounded.pill}"
    height: "20px"
  nav-bar:
    backgroundColor: "{colors.nav}"
    textColor: "{colors.nav-ink}"
    height: "52px"
  nav-link-current:
    textColor: "{colors.nav-ink}"
    padding: "0 10px"
    height: "52px"
  project-switcher:
    backgroundColor: "{colors.nav}"
    textColor: "{colors.nav-ink}"
    rounded: "{rounded.md}"
    padding: "0 10px"
    height: "34px"
  progress-bar:
    backgroundColor: "{colors.secondary-fill}"
    rounded: "{rounded.pill}"
    height: "8px"
    width: "200px"
  board-column:
    backgroundColor: "{colors.card}"
    rounded: "{rounded.lg}"
    padding: "14px 16px 16px"
    width: "400px"
  view-switch-current:
    backgroundColor: "{colors.secondary-fill}"
    textColor: "{colors.ink}"
    rounded: "{rounded.sm}"
    padding: "2px 12px"
  status-filter-current:
    backgroundColor: "{colors.secondary-fill}"
    textColor: "{colors.ink}"
    rounded: "{rounded.sm}"
    padding: "2px 10px"
---

# Design System: sluice dashboard

## Overview

The dashboard is an Operate surface for one person, the owner, who supervises many agents from
a second screen or a phone. Its world is the logo's: a rounded square in 1970s American
supergraphics, coral corners, two navy channel walls and a bright blue channel of water running
down the middle. The page takes the logo's own colours: navy ink on a cream canvas (cream on
deep navy in the dark), the nav a band of the logo's navy with three stripes under it (the
logo's water, the sky, the sand), the board's unit boxes pale water, cards a warmer white one
step above both, navy hairlines, the logo's blue as the one accent and the colour of running,
coral for the open-question count alone. The display voice is Archivo, heavy and a touch wide
like the wordmark; text is Public Sans, a plain American grotesque; corners are the mark's
generous rounding at a smaller scale. The rest is quiet on purpose: the brand lives in the
mark, the nav's band and stripes, the heavy titles, the flat colour and the corners, never in
decoration over the work.

The plan is the page: a board of the plan's units, each card a step with its status glyph and
id. Inputs, outputs, errors and runs live on the step's page (the drawer, with script), never on
the board.

The pages are server-rendered by `crates/sluice-web` (askama templates in `templates/`, views in
`src/views/`), styled by `assets/style.css` and `assets/settings.css`, and kept live by Datastar
streams; `assets/sluice.js` adds the drawer, edge drawing, tracing and live times, `nav.js` the
menus, `inbox.js` and `openui.js` the answer forms, `board.js` the project board's Plan · Board
switch and its buttons. Every page works without JavaScript.

## Colors

The custom properties in `crates/sluice-web/assets/style.css` are the only source of colour.
What follows is the house pair, Sluice Light and Sluice Dark; the other presets are under
Themes. The pair's block holds each token as `light-dark(light, dark)`, so an unpicked page
follows `prefers-color-scheme`; `data-theme` on `<html>` fixes a theme. The logo's colours are
the source of truth: coral `#ff5c49`→`#ff6551`, blue `#258aff`→`#1878f5`, navy
`#0b285f`→`#102e70`; the flat tokens take the midpoints. Light: cream canvas `#fdf8ec`, navy ink
`#0d2b67`. Dark: deep navy canvas `#071431`, cream ink `#f6f0e0`. Mixes are in sRGB, never
oklch: in oklch navy and cream meet by way of teal.

### Accent
- **The logo's blue** (`accent`, `#1f81fa`; `#288aff` in the dark): the one accent. The focus
  ring, text selection, a link's underline, checkboxes, the caret, the open step's ring; and
  running. Where it is text it darkens to `accent-ink`; primary buttons fill with `primary`, the
  blue deep enough for cream text (in the dark the logo's blue under navy text).

### Neutral
- **Canvas / box / card / muted / secondary**: cream under everything, the box tone (pale water)
  behind each unit on the board, cards a warmer white one step above both, the muted fill (a
  deeper cream) behind code, errors and runs, the secondary fill for hover, the current choice
  and the progress track.
- **The nav band and its stripes**: `nav-bg` (the logo's navy in both schemes) with `nav-ink`
  text (the sections at rest `nav-muted`, a pale blue), and under it three 4px stripes (3px on a
  phone): the water (`stripe-1`), the sky (`stripe-2`) and the sand (`stripe-3`). Section heads
  take `heading-accent`, a deep blue (a pale sky in the dark).
- **Ink / muted ink**: navy text (cream in the dark); muted ink for meta, captions and labels.
  Hairlines are ink at 11% (24% when stronger).

### Status ramp
Each status keeps its drawn glyph; colour repeats it.
- **Running**: the logo's blue (a spinning ring).
- **External** (a ready `core.external` step): live work outside sluice, so running's blue,
  told apart by its glyph (an arrow leaving a box) and the caption "outside".
- **Succeeded** and set by hand (`manual`, a ring and dot): kelly green.
- **Stale**, a question awaiting a reply and a running step gone quiet: harvest gold, the
  attention voice.
- **Paused**: plum, a hold someone chose.
- **Pending** and skipped: idle, a grey navy.
- **Failed**: ink, never coral: the cross in a disc and a full-ink border.
- **Blocked**: a pending step behind a failed or stale one: a dashed border and the caption
  "blocked".

### Named Rules
**The One Coral Rule.** Coral (`badge`) is the nav's count of open questions to the owner and
nothing else, besides the logo itself; its number is deep navy (`badge-ink`). A failed step
reads through its cross glyph and border, not coral and not red. In another theme `badge` is
that theme's one alert hue, under the same rule.

### Themes
The display preferences menu offers seven presets (`THEMES` in `src/views.rs`); until one is
picked the page follows the OS between the house pair. Each preset is a full set of the colour
tokens under `[data-theme="<id>"]` in `style.css`, with its `color-scheme`: `background`,
`foreground`, `card`, `box`, `secondary`, `muted`, `muted-foreground`, `accent`,
`accent-ink`, `primary`, `primary-foreground`, `border`, `border-strong`, `input`, `edge`,
`edge-head`, `status-idle`, `status-success`, `status-attention`, `status-paused`, `badge`,
`badge-ink`, `nav-bg`, `nav-ink`, `nav-muted`, `stripe-1`, `stripe-2`, `stripe-3`,
`heading-accent`, `lift` and `scrim`. `ring` and `status-active` are the accent in every theme.
The selectors are bare attributes, so a swatch carrying a theme's `data-theme` draws itself in
that theme's tokens.

**The Two-Tone Rule.** A theme is two or three hues with a job each, never one hue washed over
the page. Its deepest colour is the nav band; three stripes under the band are its signature,
stepping from the band towards the canvas; the board's boxes take a tone of their own, apart
from the canvas in hue; cards are a lighter step off both; section heads take
`heading-accent`. No stripe, pattern or gradient inside a box, behind text or on a card.

| Theme (id) | Scheme | The idea | Canvas · ink · accent · badge | Band · stripes (top down) · boxes · heads |
|---|---|---|---|---|
| Sluice Light (`light`) | light | the logo: navy on cream | cream · navy · the logo's blue · coral | the logo's navy · its water, sky, sand · pale water · deep blue |
| Sluice Dark (`dark`) | dark | the logo at night: cream on deep navy | deep navy · cream · the logo's blue · coral | the logo's navy · water, sky, sand · a bluer navy · pale sky |
| Canyon (`canyon`) | light | desert sandstone, a cliff's shadow, turquoise water; signature: the sunset | sandstone `oklch(0.945 0.028 62)` · canyon brown `oklch(0.3 0.07 40)` · turquoise `oklch(0.56 0.1 205)` · poppy `oklch(0.63 0.2 28)` | cliff brown `oklch(0.33 0.07 40)` · terracotta, sundown orange, the last gold · desert sage `oklch(0.925 0.03 150)` · terracotta |
| Ranger (`ranger`) | light | a park service's parchment, pine and lake, ochre signs; signature: the forest band with ochre | parchment `oklch(0.955 0.03 95)` · pine `oklch(0.3 0.07 158)` · lake blue `oklch(0.55 0.12 240)` · fire-danger orange `oklch(0.66 0.18 45)` | pine `oklch(0.33 0.07 158)` · ochre, bark, moss · meadow `oklch(0.928 0.04 135)` · bark brown |
| Diner (`diner`) | light | chrome, mint walls, charcoal lettering, a jukebox; signature: the turquoise sign trimmed in chrome | chrome white `oklch(0.97 0.006 210)` · charcoal `oklch(0.27 0.025 230)` · jukebox blue `oklch(0.56 0.15 255)` · cherry `oklch(0.56 0.2 22)` | turquoise `oklch(0.47 0.085 193)` · chrome, charcoal, mint · mint `oklch(0.925 0.045 172)` · deep turquoise |
| Night Sky (`night-sky`) | dark | a desert night: indigo, starlight, moonlit cyan; signature: indigo with a star-white stripe | indigo `oklch(0.2 0.06 285)` · starlight `oklch(0.945 0.02 90)` · cyan `oklch(0.78 0.11 210)` · sunset coral `oklch(0.72 0.17 35)` | deepest night `oklch(0.155 0.05 285)` · violet, periwinkle, starlight · violet dusk `oklch(0.245 0.08 298)` · pale periwinkle |
| Wood Panel (`wood-panel`) | dark | a 1970s den: walnut, harvest gold, avocado, the TV's glow; signature: walnut with harvest gold | walnut `oklch(0.22 0.03 55)` · cream `oklch(0.935 0.03 85)` · TV blue `oklch(0.74 0.1 225)` · burnt orange `oklch(0.7 0.17 45)` | dark walnut `oklch(0.17 0.025 50)` · harvest gold, avocado, teak · avocado shade `oklch(0.275 0.04 108)` · pale teak |

Every preset keeps these, measured when it is added:
- **One alert colour**: `badge` is used by the open-question count alone; no stripe, band or
  head takes its hue. On the band the count wears a rim of `nav-ink`.
- **Distinct states**: running, succeeded, stale, paused, pending, failed and the badge are apart
  from each other, and the glyphs still carry every state.
- **Contrast**: text at least 4.5:1 on the canvas, the card and the box; `nav-ink` and
  `nav-muted` at least 4.5:1 on the band; `primary-foreground` on `primary` and `badge-ink` on
  `badge` at least 4.5:1; status glyphs, the ring and edge arrowheads at least 3:1.
- **The mark** keeps its own colours in every theme: it is an `<img>`, never recoloured.

**Adding a theme**: an entry in `THEMES` in `src/views.rs` (its place there is its place in the
menu) and a block `[data-theme="<id>"] { color-scheme: light|dark; … }` in `style.css` that sets
every token above. Then measure it.

**The Shape Carries It Rule.** Every status has its own glyph, a Lucide icon at 16px (dashed
ring `circle-dashed` pending, the turning arc `loader-circle` running, a check in a filled disc
`circle-check` succeeded, ring and dot `circle-dot` set by hand, circular arrow `rotate-cw`
stale, a cross in a filled disc `circle-x` failed, ring with two bars `circle-pause` paused,
ring with a slash `circle-slash` skipped, an arrow leaving a box `square-arrow-out-up-right`
external) plus a visually hidden word; colour only repeats what the shape says. Succeeded and
failed fill Lucide's ring with the status colour and cut the mark in the card colour.

## Typography

Two faces from cdn.jsdelivr.net (Fontsource), the system sans without them. **Archivo
Variable** (weight and width axes) is the display voice: the wordmark (800, 22px), page titles
(800, 28/34, 24/30 on a phone), the step's id on its page (800, 22/26), section heads (750,
18/24), a project's name on the index, a question's title (750, 17/23) and every step id on the
board (600, 14.5/20, normal width). **Public Sans Variable** is the text face: 15/22 body, 14/20
small, 13/18 meta and labels. The system monospace is for data only: errors, fn names, values,
types, run ids, the log's seq and kind. Numerals are tabular.

### Named Rules
**The Meta Voice Rule.** What a run says about itself (times, counts, captions, labels) is 13px
meta in muted ink, sentence case. Nothing is uppercase; no kickers or eyebrows.

## Layout

### Named Rules
**The One Column Rule.** Every page sits on one centred 960px column (`--column`, with at least
a 24px gutter, 16px at 720px and below). The top nav's content aligns to the same edges (the
mark on the left edge, the display preferences icon ending on the right), so nav, titles, lists
and cards share one left edge at every width. Nothing makes the page scroll sideways. The one
widening: from 1280px a plan page with a board grows the column by the board's width and gap
(`--board-w` 400px, 480px from 2200px; `--board-gap` 32px), nav included, so the plan keeps its
960px and the board's right edge is the nav's.

**The One Click Rule.** The board is names and states. Everything else (inputs, outputs,
errors, runs) is one click away on the step's page, or under the board for the plan as a
whole.

### Navigation
- **One bar** (the band, `nav-bg`, full width with its content on the column, its three
  stripes under it): the brand (the owner's mark, `/static/logo.svg` at 27×26, and the wordmark
  "sluice" as live text; one link to the index, named "sluice: all projects"); the **project
  switcher**; in a project, the **project settings** gear; the sections; **Inbox**; and the
  **display preferences** sliders at the right edge. Every page links `/static/favicon.svg`, the
  same mark.
- **Project switcher**: a `<details>` whose summary is the chosen project's name ("All
  projects" when none) and a chevron; its menu lists All projects, then every live project with
  its status glyph and icon, then the archived ones under "Archived". `nav.js` closes it on a
  click elsewhere or Escape.
- **Sections**: in a project Plan, Inbox, Questions, Log, History, Functions; without one Log
  and Functions (the index is the switcher's "All projects"). The current one is `nav-ink` with
  a bar on the band's bottom edge and `aria-current`; the rest `nav-muted`.
- **Inbox**: the tray (Lucide `inbox`), the word "Inbox" and the coral badge with the number of open
  questions to the owner across every project (none when nothing waits). It always leads to the
  home-wide inbox.
- **Project settings**: the gear (Lucide `settings`) linking `/projects/id/<id>/settings`,
  shown only in a project. Its hover and its current state (on the settings page) are the same
  36px rounded fill as display preferences', its focus ring drawn on that fill.
- **Display preferences**: horizontal sliders (Lucide `sliders-horizontal`, so it never reads
  as a second settings gear) opening a `<details>` menu: "Theme", a radio list of the seven
  themes (each its name and a swatch: "Aa" on the theme's canvas cut by its band and stripes,
  a tick on the chosen one), then "Show value types". It is a form posting to `/settings`
  (cookies `sluice_theme`, `sluice_types`); without script its Save button sends it.
- **Phone** (720px and below): the brand gives way to the switcher (its label clips at 120px),
  the sections scroll sideways inside themselves, and Inbox shows only its tray and badge.

### Projects (`/`)
One list of the live projects, each a row: its status glyph, icon and name (a link), when it
last changed; for one with failures a sentence in ink, "Stopped: a, b failed · n paused" (each
step a link; "Stopped:" only while nothing runs); its description's opening; a progress bar with
"n of m"; then either its running steps (glyph, title, live time, a gold "quiet" tag once a run
has written nothing for 15 minutes) or one line on what stops it ("Paused.", "Stopped: nothing
is running."). The archived projects fold under "Archived (n)". While nothing holds the
scheduler lease, one attention line heads the list: "Runner stopped · nothing new starts until
`sluice loop` runs".

### Board (`/projects/id/<p>`)
Top down:
1. **Summary line**: the progress bar (succeeded, running, failed, the rest), "N steps · n
   succeeded · n running · n failed", and a Paused tag; then "Paused: no step starts." or
   "Archived: listed apart from other projects." when so; then the project's description as
   markdown.
2. **Board tools**: a GET form, Order (Live first, Plan order) and Show (All, Active,
   Attention, Done) selects with an Apply button, and a Mermaid link (the plan as `plan_view`
   draws it: a Mermaid `flowchart TD` with a subgraph per unit). Live first orders units by attention (a failed or
   stale step), running, ready, held, done; Plan order keeps the plan's.
3. **The board**: the plan inputs as dashed chips, one box per unit, the plan outputs as dashed
   chips. A box (the theme's box tone, 14px radius, 16 by 18px padding, 12px on a phone, no
   border) is labelled with the unit's id in meta and lays its cards in rows by dependency depth.
   A done unit folds to one line, a `<details>`: the success glyph, the unit id, "n steps ·
   done" and a chevron; it opens to its cards. A unit with no visible steps reads "No units match
   this view."
4. **Edges** (the `<sluice-board>` element draws them in an SVG over the measured cards, from the
   server's typed relations): a handoff a solid line, an `after` step entry a dashed ordering
   line, a ref entry a condition line labelled with the output (`not <output>` for `!`), a `?`
   entry dashed, a `unit:` entry drawn to the unit's box. The legend under the board reads
   "Handoff · after · condition · not · dashed ? · unit". Below 720px there are no edges and no
   legend; each box stacks its lanes, each reading straight down.
5. **Under the board**: Result (each plan output's value, or "No value yet.") and Plan inputs
   (value or "No value yet.", and the input's doc).

### The project's board (beside the plan)
A project may carry a board (`docs("board")`): an OpenUI program the server draws with the
project's live data. It is the owner's instrument for that project, never a second plan.
- **Wide (1280px and up):** a right-hand column (`board-pane`) beside the plan, top-aligned
  with the summary line: the card colour with a hairline and the 14px corner, 14 by 16px
  padding, sticky 16px from the top and at most the window's height, scrolling on its own. Its
  head is "Board" (section voice, `heading-accent`, Lucide `layout-dashboard`). With the step
  drawer open the drawer takes the side and the board steps away until it closes.
- **Narrow (below 1280px):** a small segmented control first, "Plan" (Lucide `workflow`) and
  "Board" (`layout-dashboard`), the current one in the secondary fill (44px tall at 720px and
  below), then one section at a time; the choice is remembered per project. Without script
  there is no switch and the board follows the plan, flat on the canvas under its head.
- **No board:** nothing at all: no column, no switch; the plan keeps the whole column.
- **Its parts** keep the question forms' look (`ou-*`): headings in Archivo, text at 15/22,
  callouts, tables at 13px with hairline rows. Units is a table of unit (a link), state (the
  status glyph and word, then its age in meta), the steps' marks in data mono and what it waits
  on in muted ink. StepStatus is the step's own card (pill, glyph, id, caption) with the reason
  under it in meta. Output is its name in meta over the value. Metric is a number in Archivo
  800 at 28/34 over its label, on the box tone; metrics in a row share it. Chart is an inline
  SVG at most 520px wide: bars and the line in the accent, labels in ink and values in muted
  ink at 12px, its caption in meta under it. Buttons are the dashboard's buttons; what a press
  did (or why it was refused) is a status line under the board.
- **A part that cannot be drawn** is a small box in its place: the muted fill, a strong
  hairline, a 3px left rule in the attention gold and Lucide `triangle-alert` in gold; the
  component and line in 600, the reason in data mono. Never coral, never red.

### Step (`/projects/id/<p>/steps/<s>`, and the drawer)
On the board, opening a card with script loads the step into a right-hand drawer (beside the
page from 1200px, `min(680px, 45vw)`; over the page on a scrim below that; full width on a
phone) and puts the card's ring on; without script the card's link opens the step's own page.
The step reads top down:
- its id (22px) with badges: the status glyph and word, "blocked", "quiet", "done/total runs"
  when scattered; its doc; "Running <time>" or "Ended <time ago>"; a meta line of the fn in mono
  and its tags as badges;
- the actions, one POST form carrying the plan revision: Pause or Unpause (where pausing acts:
  pending, failed, stale, or any paused step), Retry with a folded "Feedback for retry" textarea
  (succeeded, failed, stale), Cancel (pending or running); then a link "Thread · n messages" and
  a gold "n awaiting reply" tag;
- facts: "Waits on" (each reason) and "After" (its gate entries);
- sections under small heads, in need order: Queued, Skipped, Error (the error in a mono box),
  Outside sluice (an external step's doc and how to settle it), Outputs, Inputs, Runs.
- Outputs and Inputs are field lists: the name (with its type after it, shown by the Types
  switch at the Outputs head or the display preference) in a narrow column of meta, the value
  beside it, "From <source>" linking to the source step; a value reads by its kind (text, a
  tabular number, a `true`/`false` pill, a muted "none"); a long value folds. An unset output
  reads "Not set yet.", an unset input "No value yet."
- Runs: one row per run, numbered, with the step's glyph, the run id in mono, "Started … ·
  ended …", the engine and session, and the run's result; the current run last on the muted
  fill.

### Messages (`/inbox`, `/questions`, `/history`, `/projects/id/<p>/…`)
A page title and a small segmented control (Inbox · Questions · History, the current one in the
secondary fill). Each question is a card: its title (Archivo 17px), a meta line ("project ·
thread" linking to the thread, "from X to Y", when, "sets <input>"), "Nobody is waiting: <why>"
in gold when its asker has stopped, the body as markdown, then the answer form. A question with a
`ui` draws its OpenUI program (its fields and buttons, and Close) above a folded "Answer in
words instead"; without one, a text box with Answer and Close question. The inbox
then lists "Unread notes". History lists the threads with the owner, each with its message
count and a preview. A thread page (`thread?thread=<name>`) shows every message, each a head
(from → to, when, an "Awaiting reply" or state tag, "Reply to n"), an optional title, the body,
"Answer as sent" folded, and an open question's answer form; under them a "Message to
<recipient>" box (the thread's step while it is in the plan, else the orchestrator) with "Ask a
question that needs a reply" (an ask; unchecked, a note) and Send. Messages shown are marked
read.

### Log (`/log`, `/projects/id/<p>/log`)
A "Kinds" fieldset of checkboxes (every kind and group), a Threads field ("any") and Apply; then
a table of seq, time, kind (12px data) and a one-line summary that opens to the record's JSON,
50 records a page with "« newest", "‹ newer" and "older ›". The home log shows records without
a project; the message fns' call noise (`message.ask`, `message.say`, `message.reply`,
`message.post`) is left out. On a phone the kinds fold behind "Filter: all
kinds" and the time column hides.

### Functions (`/fns`)
"As seen by" a project select (or "no project") with Show; then Built-in, Global and Project
groups, each fn its name in mono, its doc, a problem in ink when it has one, and Inputs and
Outputs columns of `name: type` (one column on a phone).

### Project settings (`/projects/id/<p>/settings`)
The project's name as title, "Project settings", and "Each Apply commits one change right away"
with a link to the authored changes. Details: Name (with its rules; links and running work keep
the project id), Description (with a Preview), Icon (a text icon up to 16 characters, or an
image up to 256 KiB). Resources: a line per resource (static capacity or capacity fn, in use,
waiting, the step queue) and a capacity field each, then a new resource's name and capacity.
Board: the program in a monospace textarea (with its rev), Save board and Clear board, and under
them a live preview, drawn as the board column would draw it (its width), as one types.
Activity: Pause project and Archive project switches. Delete project, in a bordered danger card:
it explains what goes, refuses until the project is archived, and needs the current name typed.

## Elevation & Depth

Flat by default, as supergraphics are: fields of flat colour, cards separated by hairline, not
shadow. One lift (`--lift`) on what floats over the page: the drawer below 1200px, the
switcher's and the preferences' menus and the focused skip link. The board column is flat: a
card with a hairline, no shadow.

## Shapes

The mark's generous corner, scaled down. 14px (`--radius`) for regions: unit boxes, the index's
list, question cards, the danger card; 10px (`--radius-md`) for controls, the switcher and its
menu, and code blocks; 5px (`--radius-sm`) for inline code, tags and menu items. A pill (999px)
is only for the step cards, the progress bar, the inbox badge, the Types switch's track and a
boolean value: a card is a token of work, not a panel. Plan input and output chips are dashed,
since they are ends, not work.

## Components

- **Step card**: a pill with the status glyph, the step id (14.5px, 600) and, in 12px meta, a
  caption: "blocked", "queued", "outside" or `done/total`. Running cards take a blue border,
  failed a full-ink one, stale a gold one, paused plum; blocked a dashed border; a step next in
  line (ready) keeps a strong hairline. Inline `core.*` steps are chips: dashed and muted. Its
  accessible description carries the doc, what it waits on and its error. The card in the drawer
  wears the blue ring 2px outside its border.
- **Tag** (`.tag`): a small fact set apart: 12px text at 500, a strong hairline and the 5px
  corner; gold (`attn`) for "quiet" and "n awaiting reply", muted for a closed or answered
  state.
- **Icons**: every icon is Lucide (lucide-static 1.52.0, ISC), the published SVG unmodified in
  `crates/sluice-web/assets/icons/` and inlined by `views::icons::icon`: Lucide's 24-unit grid
  and 2-unit round stroke, in `currentColor` so it takes its control's ink, hover and focus, and
  `aria-hidden` (the control carries the name). 20px in the nav (the inbox tray, project
  settings, display preferences), 16px for status glyphs, chevrons, the theme tick, the board's
  head and switch and its error boxes. A new icon is fetched from Lucide at that version, never
  drawn; the mark and favicon are the owner's, and a project's own icon is the user's.
- **Tracing**: hovering or focusing a card lights its edges; the other cards lose their border
  and fill and their text turns muted ink.
- **Buttons**: primary is the deep blue under cream; others are card-coloured with an input
  hairline. The progress bar is an 8px pill, 200px wide (120 on a phone).
- **Live updates**: each page's stream patches only what changed; while it reconnects a gold
  line says "Updates paused. Reconnecting…" with a Reconnect button. Times tick live (`data-since`,
  `data-ago`), and a running step's quiet tag appears after 15 minutes without a write.

### Touch
At 720px and below every control is at least 44px tall: the board tools, the segmented
controls, the log's filter labels and pager links, the menu rows, and in the drawer the links of
its facts and its thread link; the Types switch keeps its size on a 44px target.

## Do's and Don'ts

### Do:
- Do set titles, ids and the wordmark in Archivo and let the flat colour carry the era; keep
  everything else plain.
- Do say one thing per slot, and keep ids, values and runs on the step's page.
- Do show state with the glyph's shape first and colour second.
- Do keep every page a server-rendered picture that works without JavaScript; script adds the
  drawer, edges, tracing and live times.

### Don't:
- Don't spend coral on anything but the open-question count (and the logo); never draw failure
  in coral or red.
- Don't restyle or redraw the mark; it is the owner's SVG, served as is.
- Don't draw an icon by hand; take it from Lucide (see Icons).
- Don't repeat on the page what another part of it already says.
- Don't use uppercase labels, kickers or decorative motion; motion is the drawer's slide, the
  running spinner and short fades.
