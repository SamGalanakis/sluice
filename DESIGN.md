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
  status-filter-current:
    backgroundColor: "{colors.secondary-fill}"
    textColor: "{colors.ink}"
    rounded: "{rounded.sm}"
    padding: "2px 10px"
---

# Design System: sluice dashboard

## Overview

The dashboard is an Operate surface for one person, Sam, who supervises many agents from a
second screen or a phone. Its world is the logo's: a rounded square in 1970s American
supergraphics, coral corners, two navy channel walls and a bright blue channel of water running
down the middle. The page takes the logo's own colours: navy ink on a cream canvas (cream on
deep navy in the dark), the nav a band of the logo's navy with three stripes under it (the
logo's water, the sky, the sand), the board's boxes pale water, cards a warmer white one step
above both, navy hairlines, the logo's blue as the one accent and the colour of running, coral
for the inbox count alone. The display voice
is Archivo, heavy and a touch wide like the wordmark; text is Public Sans, a plain American
grotesque; corners are the mark's generous rounding at a smaller scale. The rest is quiet on
purpose: the brand lives in the mark, the nav's band and stripes, the heavy titles, the flat
colour and the corners, never in decoration over the work. The plan is the
page: a left-to-right board of the blocks of work, each card saying one thing per slot. Ids,
fn names, bindings and run history live in the step drawer, never on the board. Nothing is
repeated: plan inputs and outputs are board nodes, not tables; history is the log filtered
(to the history kinds, every edit back to rev 1 with the manual values the log still has).

## Colors

Tokens in `src/sluice/static/dashboard.css` are the only source of colour (the Mermaid
classes in `views.py` repeat the Sluice Light set as hex for agents). What follows is the
house pair, Sluice Light and Sluice Dark; the other presets are under Themes, below. The
pair's block holds each token as `light-dark(light, dark)`, so an unpicked page is
`color-scheme` alone: `light dark` (the OS's, by `prefers-color-scheme`) when `<html>` has no
`data-theme`, and `data-theme="light"` or `"dark"` fixes it. The logo's colours are the source
of
truth: coral `#ff5c49`→`#ff6551`, blue `#258aff`→`#1878f5`, navy `#0b285f`→`#102e70`; the
flat tokens take the midpoints. Light: cream canvas `#fdf8ec`, navy ink `#0d2b67`. Dark: deep
navy canvas `#071431`, cream ink `#f6f0e0`. Two colours are mixed in sRGB, never oklch: in
oklch navy and cream meet by way of teal.

### Accent
- **The logo's blue** (`accent`, `#1f81fa`; `#288aff` in the dark): the one accent. The focus
  ring, text selection, a link's underline, checkboxes, the caret, the open step's ring, the
  orchestrator's rule in a thread, the "n new" pill; and running (below). Where it is text it
  darkens to `accent-ink` (`#0063d3` on cream, `#77b5fb` on navy), and the answer button fills
  with `primary`, the blue deep enough for cream text (`#006be2`; in the dark the logo's blue
  itself under navy text). A link keeps its text's colour with the blue underline, so a failed
  step's id in the stuck sentence stays ink.

### Neutral
- **Canvas / box / card / quiet fill / secondary fill**: cream under everything, the box tone
  (pale water, `box`) behind the board's pieces of work, cards a warmer white one step above
  both, the quiet fill (a deeper cream) behind prompts, facts and code, the secondary fill for
  hover, the current filter and the progress track. In the dark: deep navy, a bluer navy for
  the boxes, a navy step up for cards, the quiet fill between canvas and card.
- **The nav band and its stripes**: the nav is a band of the logo's navy (`nav-bg`, in both
  schemes) with cream text (`nav-ink`; the sections at rest `nav-muted`, a pale blue), and
  under it three 4px stripes (3px on a phone), top down the logo's water (`stripe-1`), the
  sky (`stripe-2`) and the sand of the shore (`stripe-3`): the 1970s racing stripe, the
  theme's signature. Section heads (the drawer's, those under the board, a page's `h2`) take
  `heading-accent`, a deep blue (a pale sky in the dark).
- **Ink / muted ink**: navy text (cream in the dark); muted ink, a greyed navy (a warm grey
  cream in the dark), for meta, summaries, labels. Hairlines are ink at 11% (24% when
  stronger).

### Status ramp
Each status keeps its drawn glyph; colour repeats it and stays clear of the accent's other uses
only by shape, and of coral always.
- **Running**: the logo's blue (a spinning ring; the bubble's border at 65%).
- **External** (a ready `core.external` step, its work going on outside sluice): live work,
  so running's blue and the running bubble's border, told apart by its glyph (an arrow
  leaving a box) and "outside · 2h 5m" where its time would be. No colour of its own.
- **Succeeded** and set by hand: a kelly green (`#11813c`; `#58cd78` in the dark).
- **Stale** and a message awaiting a reply, a running step gone quiet, and an open inbox item
  nobody is waiting for any more ("Nobody is waiting — build is failed", a 500 line under its
  meta, the item listed after the live ones): harvest gold (`#916100`; `#f1bf4e` in the dark),
  the attention voice.
- **Paused**: plum (`#8b4486`; `#d997d2` in the dark) and a dashed plum border, a hold someone
  chose, apart from attention.
- **Pending** and skipped: idle, a grey navy (`#788190`; `#8893a5` in the dark).
- **Failed**: ink, never coral: the cross in a disc, a full-ink border over a 6% ink fill, a
  600 error line.
- **Blocked**: a pending step a failure holds up, a dashed ink border at 45% and "blocked" in
  ink.

### Named Rules
**The One Coral Rule.** Coral (`badge`, the logo's `#ff604d`) is the open-inbox count in the nav
and nothing else, besides the logo itself; its number is deep navy (`badge-ink`, 6.0:1). A
failed step reads through its cross glyph, full-ink border and bold error line, not coral and
not red. In another theme `badge` is that theme's one alert hue (a poppy, fire-danger orange,
the booths' cherry), under the same rule.

### Themes
The settings menu offers seven presets; until one is picked the page follows the OS between
the house pair (their shared block is also the fallback, `:root:not([data-theme])`). Each
preset is a full set of
the colour tokens under `[data-theme="<id>"]` in `dashboard.css`, with its `color-scheme`:
`background`, `foreground`, `card`, `box`, `secondary`, `muted`, `muted-foreground`,
`accent`, `accent-ink`, `primary`, `primary-foreground`, `border`, `border-strong`, `input`,
`edge`, `edge-head`, `status-idle`, `status-success`, `status-attention`, `status-paused`,
`badge`, `badge-ink`, `nav-bg`, `nav-ink`, `nav-muted`, `stripe-1`, `stripe-2`, `stripe-3`,
`heading-accent`, `lift` and `scrim`. `ring` and `status-active` are the accent in every
theme, set once on `:root`. The selectors are bare attributes, not `:root`'s, so a swatch that
carries a theme's `data-theme` draws itself in that theme's tokens.

**The Two-Tone Rule.** A theme is two or three hues with a job each, never one hue washed over
the page. Its deepest colour is the nav band; three stripes under the band are its signature,
in analogous tones that step from the band towards the canvas; the board's boxes take a tone
of their own, apart from the canvas in hue, not only in lightness; cards are a lighter step off
both; section heads take `heading-accent`. The graph itself stays calm: no stripe, pattern or
gradient inside a box, behind text or on a card.

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
- **One alert colour**: `badge` is used by the inbox count alone; no stripe, band or head
  takes its hue (each stripe at least 0.13 from it in OKLab). On the band the count wears a
  1.5px rim of `nav-ink`, so it stands off a band whatever its colour.
- **Distinct states**: running (the accent), succeeded, stale and quiet (the attention gold),
  paused, pending, failed (ink) and the badge are apart from each other (the closest pair at
  least 0.11 in OKLab), and the glyphs still carry every state.
- **Contrast**: text (`foreground`, `muted-foreground`, `accent-ink`, `status-attention`,
  `heading-accent`, a far-off pending id at 72% ink) at least 4.5:1 on the canvas, the card
  and the box, and on a pending card over the box; `nav-ink` and `nav-muted` at least 4.5:1 on
  the band; `primary-foreground` on `primary` and `badge-ink` on `badge` at least 4.5:1; the
  status glyphs, the ring and the edges' arrowheads at least 3:1 on the canvas, the card and
  the box (a focus ring on the band is `nav-ink`).
- **The mark** keeps its own colours in every theme: it is an `<img>`, never recoloured.
- **The structure**: canvas under everything, the box tone behind the board's pieces, cards one
  step off both, the secondary fill for hover and the current choice, hairlines as ink at
  11-12% and 24-26%.

**Adding a theme**: a line `"<id>": "<Name>"` in `THEMES` in `views.py` (its place there is its
place in the menu), and a block `[data-theme="<id>"] { color-scheme: light|dark; … }` in
`dashboard.css` that sets every token above. The route, the picker and its swatch read the
list; a test fails if a block is missing a token or a theme has no block. Then measure it.

**The Shape Carries It Rule.** Every status has its own drawn glyph (dashed ring, spinning
ring, check, ring and dot, circular arrow, cross, ring with two bars for paused, dashed ring
with a slash for skipped, an arrow leaving a box for external) plus a
visually hidden word; colour only repeats what the shape says. A paused bubble also has a
dashed plum border.

## Typography

Two faces from cdn.jsdelivr.net (Fontsource), the system sans without them. **Archivo
Variable** (weight and width axes) is the display voice, a heavy grotesque of the late 19th
century American kind that the 1970s set big: the wordmark (800, 22px, 112% wide, -0.025em),
the page title (800, 28/34, 24/30 on a phone), the drawer's step id (800, 22/26), section
heads (750, 18/24), a project's name on the index (750, 18/24), an inbox item's title
(750, 17/23), the switcher's label (700, 15/20) and every step id on the board (600,
14.5/20, normal width, so a long id stays compact). Display text runs 105-112% wide.
**Public Sans Variable** is the text face: 15/22 body, 14/20 small, 13/18 meta, labels. The
system monospace is for data only: stderr, fn names in the functions list, values, types, the
log's seq, time and kind. All numerals are tabular.

### Named Rules
**The Meta Voice Rule.** What a run says about itself (times, costs, counts, engines, labels)
is 13px meta in muted ink, sentence case. Nothing is uppercase; no kickers or eyebrows. A small
label stays in the text face even on an `h2`.

## Layout

The plan reads top to bottom, inside the column. Each independent piece of work (the steps any
edge joins, handoff or `after`) is a quiet box when there are several (the theme's box tone,
0.625rem radius, 16px by 18px padding, 12px on a phone; 14px apart, 10px on a phone; no border:
a region, not a card, since the bubbles carry the borders), so what belongs together reads
without the edges; a plan of one piece has no box. A box is rows by dependency depth with 40px
between rows for the edges (10px on a phone, which draws none), from its first step; in a
row the cards stand
lane by lane (a lane: the steps joined by handoffs), the next lane's first card 22px apart,
and a row too wide wraps within itself. A lane keeps its cards together: one that would
crowd a row it shares past the box's width (it hangs from another only by an `after`)
starts below the lanes before it instead of wrapping in among their rows, and a lane keeps
its side of the box from row to row (one starting takes the place left free), so it does
not jump across when another ends beside it. On a phone a box stacks its lanes one after another,
each reading straight down, a lane after the first 14px apart. The server lays the board out;
the `<sluice-board>` component draws the edges between measured cards (bottom to top, spread
when several share a side, an arrowhead at the end; dashed for an `after` edge, which orders
two steps without passing data). Relation kinds determine lanes, dashed lines and the legend;
port names are labels, so a handoff named `after` stays a solid line, and a pair with both a
handoff and ordering stays solid. The component threads an edge that passes rows through gaps so it
never hides behind a card. A quiet legend under the board names the two lines. A box of
several steps that have all succeeded (or been skipped, beside at least one success) folds to
one 44px line: the success glyph, its first step's id, then in meta "… its last step · n steps"
(the glyph says they succeeded; "3 succeeded, 1 skipped" only when some were skipped; on a
phone the first id in full, wrapping, and "n steps" under it; the last id gives way); a native `<details>` that opens to its cards (open through live updates and,
per tab, a reload). No edge crosses between boxes, so a folded one hides only its own edges.
With several boxes a toolbar (below) orders them live first by default (what needs
attention, then what runs, what is ready, what is held, what is done; the plan's order within
each) or in the plan's order, and filters them; the cards inside a box keep the plan's
layout. By default the board leaves out the steps that can't run (behind a failure, a stale
step, a paused step or a plan input with no value, and every skipped step): the step a person acts on stays,
with "+12 behind" in its small muted line, and the box lays out again without the rest, so a
long chain waiting on one failure reads as that failure. The head of a
project page says first whether the work moves (progress bar, counts, Pause and Archive), then
what the project is (its description, folded to its opening); what the plan took and produced
follows the board. When the runner is down (its heartbeat stale), the index and that
line say so first, in the attention gold. A project with failed steps leads its page and its
index row with one sentence in ink, weight 500 (not coral: the failure is the orchestrator's to
retry, and only the inbox asks the person): "Stopped: a and b failed, blocking 4 steps · 11
paused", each failed step a link (to its drawer), "Stopped:" only while nothing runs; it
takes the column's width, balanced when it wraps, and never breaks inside a step's id (on a
phone a long one may). The bar's label counts the failed, blocked and paused steps too; the
counts line leaves them to that sentence when it leads the page, so nothing is said twice.
The index row leads with the project's status glyph, and a running step there wears the
"quiet 40m" badge its card does. The tab title leads with "n failed · n quiet ·" (the
quiet count grows as the page ages). Text blocks hold a 68-75ch measure.

### Named Rules
**The One Column Rule.** Every page sits on one centred 960px column (`--column`, with at
least a 24px gutter, 16px below 720px): `main` is a three-track grid (gutter, column, gutter)
and everything goes in the middle track, the board included. The top nav's content aligns to
the same edges (the mark on the left edge, the settings cog ending on the right), so nav, title,
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

Flat by default, as supergraphics are: fields of flat colour, cards separated by hairline, not
shadow. One lift (`--lift`, a navy-tinted shadow), on what floats over the page: the step drawer below 1200px (beside the page from there, it has only its
hairline), the project switcher's and the settings' menus and the focused skip link.

## Shapes

The mark's generous corner, scaled down. 14px (`--radius`) for regions: board boxes, the
index's list, threads, inbox items, prompts; 10px (`--radius-md`) for
controls, the switcher and its menu, and code blocks; 5px (`--radius-sm`) for inline code,
badges, menu items, the segmented filter's current item, the focus ring and the nav's current
bar. A pill (999px) is deliberate, and only for the step bubbles, the progress bar, the inbox
badge, the Types switch's track, a boolean value and the "n new" pill: a bubble is a token of work, not a panel. Plan input and output nodes are dashed, since they are ends, not work.

## Components

### Cards / Containers
- **Step bubble**: a pill with the status glyph, the step id (14.5px, 550), its fn's icon
  when the fn has one, and, in 12px meta, its time (and `done/total` when scattered), and once a running step has gone quiet the
  "quiet 42m" badge after it. Nothing else: the doc and what it says now (for a quiet one
  too, its last line) are its tooltip (a failed step's is its error's last line, the exception), and everything it
  took and produced is in the drawer. A failed step's line is said in sluice's words: no
  exception class, the home directory as `~`, and an exit code a signal caused explained
  ("exited 143 (terminated: SIGTERM)"); the error as raised stays in the drawer. Running bubbles take a blue border, failed a
  full-ink one over a 6% ink fill, stale a gold one. A pending step a failure holds up
  (directly or through other pending steps) is blocked: a dashed ink border at 45% and
  "blocked" in ink where its time would be (not red; a paused one keeps the paused look and
  counts as paused). A pending step next in line (its unfinished upstream all running) keeps
  a strong hairline and ink id; pending steps further off lose their border and dim (the id
  at 72% ink, 5.0:1 on the box), so what
  starts next stands out. Glue steps (the inline `core.*`) are dashed and muted. A ready
  `core.external` step reads as live work outside sluice: the external glyph in the
  running blue, the running border, and "outside · 2h 5m" (the time since it became ready,
  live; "outside" alone when it waits on nothing); its drawer leads with its doc (who is
  doing it, where) on the quiet fill, one muted line on how to settle it, and its declared
  outputs as fields, "not set yet". The step in the drawer
  wears the ring 2px outside its border (canvas, then the blue ring: it is the selection), so
  it never reads as the card's own border.
- **Fn icon** (a fn's own mark, SPEC §4): it says what kind of work a step is (an agent, a
  git step, a question to the inbox, work outside sluice) without reading ids, and always
  second to the status. On a card it follows the id, 8px after it: 14px against the
  glyph's 16, in muted ink against the glyph's status colour, so the glyph stays the first and
  loudest mark and the icon reads as a caption; a PNG, WebP or text icon (an emoji keeps its
  own colours) sits at 70% opacity for the same reason. Tracing dims it with the glyph. A card
  of a fn without one is unchanged. A box folded to one line wears its main fn's icon (its
  first open fn: the agent in a lane) after its first id, as a card would. In the drawer the
  icon leads the fn's name in the meta line, 16px in muted ink; on the Functions page it
  leads each name at 20px in ink. An SVG icon is a mask over `currentColor`, so it takes
  every theme's ink as the glyphs do; the shipped ones are drawn like the glyphs (16×16, 1.5
  strokes, round caps and joins, no fill).
  A bubble's accessible name is "failed, id, 1h 14m" (visually hidden commas; ", quiet
  42m" after it when quiet).
- **Badge** (`.tag`): a small fact set apart, never a sentence: 20px tall (24 by the drawer's
  title), 12px text (13 there) at 500, a strong hairline and the 5px corner, ink on no fill.
  In the attention gold (text and a 45% gold border) for "quiet 42m" and "n awaiting
  reply"; muted for "note". The status badge leads with the glyph (its word is the badge's
  own, so the glyph is hidden from assistive technology). A quiet badge is rendered hidden on
  every running step and shown by the ticker once the run has written nothing for 15
  minutes, to the minute ("quiet 42m", then "quiet 1h 5m").
- **Tracing**: hovering or keyboard-focusing a bubble lights its edges and names; the other
  bubbles lose their border and fill and their text turns muted ink, so they stay readable
  (at least 3:1, measured 6.1:1 on the box). Opening the drawer clears any tracing; a focus given back
  after a click does not trace, and a card the reflow puts under a still pointer does not
  trace until the pointer moves.
- **Under the board**: the Result (`name value` rows; long text folds to 132px under a fade with
  "Show all") and the plan inputs (name, value, doc). The counts line (with the bar, Pause and
  Archive) heads the page instead.

### Buttons
Primary is the deep blue under cream (the logo's blue under navy in the dark); others are
card-coloured with an input hairline. The progress bar is an 8px pill, 200px wide (120 on a
phone), its segments 2px apart: green, blue, gold, ink, then the track.

### Navigation
- **One bar** (52px, the theme's band, `nav-bg`, the full width of the window with its
  content on the column, and its three stripes under it), the only navigation: the brand (the owner's
  mark, `static/logo.svg` at 27 by 26px, and the wordmark "sluice" beside it as live text;
  one link to All projects, named "sluice: all projects"), the **project switcher**, then the
  sections; Inbox, with the one coral badge, then the settings cog at the right edge. Every page links
  `static/favicon.svg`, the same mark (legible at 16px as it stands). There is no second row: a page does not repeat the project's name or its sections.
- **Project switcher**: a bordered button whose label is the chosen project's name (or "All
  projects") and a chevron; it opens a menu of All projects, then every project with its status
  glyph, the archived ones last under a label. A `<details>`, so it works without script;
  `nav.js` closes it on a click elsewhere or Escape.
- **Settings**: a drawn cog (20px, the tray's stroke) in the band's muted ink on a 44px
  target, its 36px fill the sections' hover (a stronger one while open), its edge on the column's;
  named "Settings". A `<details>` like the switcher (`nav.js` closes it on a click elsewhere
  or Escape, focus back on the cog) whose card, 256px, hangs under it flush with the column's
  right edge, with the lift: "Theme" (a label in meta) over a radio list of the themes, each
  a 36px row of its name and its swatch (a 58 by 26px chip of its canvas with "Aa" in its
  ink, 13px Archivo 800, and its signature cutting across the right corner at 118 degrees: its
  band's colour and its three stripes, drawn by its own tokens),
  the choice in the secondary fill with a strong hairline as the inbox's filter and a tick at
  the row's end (until one is picked the page follows the OS, and the menu marks the preset
  the OS chose); a hairline; then "Show value
  types", a checkbox (the Types switch's setting). It is a form posting to `/settings`: without script a Save button sends it and
  the page comes back in the chosen theme; with script a choice applies at once and Save is
  hidden. Its rows are 44px on a phone.
- **Sections** follow the switcher: in a project, Plan, Threads, Log, History, Functions; with
  none chosen, Log, Functions (the switcher's "All projects" is the index). The current one is `nav-ink` with a 3px bar of it on the
  band's bottom edge and `aria-current` (`page`, or `true` on a page inside it: a step is inside
  Plan); the rest are `nav-muted`, with a fill of `nav-ink` at 12% on hover. On the band the
  switcher is `nav-ink` on a 7% fill of it with a 32% border.
- **Phone** (below 720px): the brand gives way to the switcher (whose menu leads to All
  projects), the switcher's label clips at 120px, and Inbox is a tray icon with its badge, the
  cog beside it; the sections scroll sideways inside themselves when they do not fit (a
  project's last one or two, at 390px).
- **Status filter** (inbox): a small segmented control, the current status in the secondary
  fill with a strong hairline; a filter, so it does not look like the sections.
- **Board toolbar** (a project page with several boxes, or with steps that can't run; 12px
  above the board): one line of small quiet controls in 13px, what shows first: a segmented
  control of All · Active · Attention · Done, each but All with its box count in 12px muted
  numerals; a segmented control of Runnable · All steps (only when some step can't run; the
  one control on a board of one box); "Tag" in muted 500 before a select (only when the plan
  tags steps; "any" by default); a segmented control of Live first · Plan order. Segments as
  the status filter's (radios, the current one in the secondary fill with a strong hairline,
  the one ring around a focused segment), 30px tall. When a filter hides boxes or steps, "9
  done boxes hidden · show" or "14 steps that can't run hidden · show" (one sentence when
  both) in muted meta ends the line, its link showing them. A GET form: its Apply button is
  in a `<noscript>`. On a phone the show control takes the width, then the steps, the tag
  and the order wrap under it, every target 44px.

### Threads
The Threads tab, one bordered card per thread, the latest first: the step's glyph, id and
doc (or the thread name; "no longer in the plan" when its step has gone), "n messages ·
when", an "awaiting reply" tag in gold when a question waits on a step still to finish (or on
a thread of no step), and a muted line of the last message; open, a hairline under the summary and
the messages, all but the last three folded under an "n earlier messages" link. A small blue
"n new" pill counts what arrived since this browser last opened the thread, and a blue dot
marks those messages while it is open. A message is a 2px left rule and a small head (sender in ink, → recipient and
when in muted ink, a "note" or "Awaiting reply" badge), then its body at reading size. A step's
messages sit on the left with a strong-hairline rule; the orchestrator's are indented 28px
(14px on a phone) with a blue rule, so a conversation reads at a glance. Nothing there asks
the person for anything: that is the inbox.

### Step drawer
A right-hand panel, full height. From 1200px it sits beside the page (`min(680px, 45vw)`, its
hairline and no lift): nav and page make room for it in one reflow while the drawer slides in
(180ms ease-out, transform and opacity), the board redraws its edges, and the opened card
scrolls into view. From 721 to 1199px it is over
the page (680px) on a scrim that closes it, and below 720px a full-screen sheet over the
scrim; below 1200px it is a modal dialog (the page behind it inert, focus kept in it). The
close button stays at the top on a band that turns opaque (card fill, a hairline) once the
drawer scrolls, so what passes under it is hidden whole. Escape, the close button, the scrim or
a click on the page around the board close it, and focus goes back to the card. A "Skip to
plan" link is the page's first tab stop (it moves focus to the plan), and a polite live region says the statuses the live
board moves ("a failed").
It reads like a run history (Temporal's event view is the reference): the step id (22px)
with its state beside it as badges, wrapping under a long id: the status glyph and word
("blocked" for a blocked step), `done/total runs` when scattered, how long it ran (live while
running; when it started and ended are its tooltip), the gold "quiet 42m" once quiet, and
for a finished step "ended 1h ago" in meta. Then its doc, one line of meta (the fn in mono,
the cost, "session" and its first 8 characters, its tags as badges), and, each a row that
wraps under a muted label, what a pending step waits on, what it runs after, its `when`, and
what a failed step blocks: steps as links led by their glyphs. No grid of facts: nothing
there is a setting. Then the Pause switch (only where pausing acts: pending, failed, stale;
Resume on any paused step) and a muted link to its thread with its gold "n awaiting reply"
badge. Sections under small labels in need order:
Error (its last line in 600, then all of it in a box that opens scrolled to its end),
Progress, Outputs, Prompt, Inputs, Log output, Attempts (only past one: oldest first, each
its number and outcome glyph in a column joined by a strong-hairline rail, the outcome word in
600 and in meta "started 1h ago · took 54m" (the current run: its live time alone, "42m so
far"), a failure's headline in ink at 500 with all of it
under a "Show error" disclosure; the current attempt, last, on the secondary fill with its word
in Archivo). Outputs and Inputs are field lists (a `<dl>`), one row per value between
hairlines: the name in a narrow column of 13px meta in muted ink (as wide as the longest
name, up to a third of the drawer; one line, ellipsized, whole in its title), the value
beside it at 14px, so five short values take five short rows. A value reads by its kind:
text as text, numbers tabular, a boolean a small pill (`true` in ink on a strong hairline,
`false` muted), null a muted "none", a short list commas; an identifier (a path, URL, sha,
session, ticket) in the data face at 12.5px, giving way in the middle so its end stays (a
path's last segment), with a quiet copy button (drawn, 24px; shown only with script, and
without it the text is there to select; a green tick once copied). Where a value comes from
is a small quiet chip at the row's end (the quiet fill, muted mono 12px, a drawn arrow,
linking to the step; under the value on a phone), for the prompt in its section's head; the
value gives way before its chip does, and both before either wraps. The doc is meta under
the value. A long value (multi-line text, markdown, JSON, prose past a line) takes the full
width below its name, its chip and doc on the name's row, and folds under a fade past six
lines. Types are noise until asked for: in the name's title, and after every name in muted
mono (wrapping under a long one) with the one Types switch, a small quiet switch (13px
"Types" and a 24 by 14px track, the accent's fill when on) at the end of the first values
section's head, which is the settings' "Show value types": either turns it for every page.
Its focus is the one ring.

### Log
A table of seq, time, kind (12px data) and a one-line summary in plain words (a run the
runner adopted or stopped reads as a sentence; a step's message on its own thread does not
repeat the thread). On a phone the kind filter folds behind a 44px "Filter: all kinds" /
"Filter: 3 kinds" summary, so the records start near the top; its labels are 44px tall.

### Touch
At phone width every control is at least 44px tall: Pause and Archive, the board toolbar's
segments, select and hidden line's link, the nav's sections,
the log's filter labels and pager links, the inbox's status filter, a folded box's line,
the settings cog and its menu's rows, and in the drawer the steps its facts link to, its
thread link and "Show n lines";
the Types switch and a value's copy button keep their size on a 44px target.

## Do's and Don'ts

### Do:
- Do set titles, ids and the wordmark in Archivo and let the flat colour carry the era; keep
  everything else plain.
- Do say one thing per slot, and put ids and plumbing in the drawer.
- Do show state with the glyph's shape first and colour second.
- Do keep the board a server-drawn picture that works without JavaScript; the script only adds
  the drawer, tracing, live times and the frontier scroll.

### Don't:
- Don't spend coral on anything but the open-inbox count (and the logo); never draw failure in
  coral or red.
- Don't restyle or redraw the mark; it is the owner's SVG, served as is.
- Don't repeat on the page what another part of it already says (no inputs table next to input
  nodes, no history table next to the log).
- Don't use uppercase labels, kickers, emoji icons or decorative motion (a fn's own text icon
  is its author's content, shown as a muted caption, never an icon of the dashboard's own); motion is the drawer's
  180ms slide in (the page makes room at once), the running
  spinner, the 220ms flip of a changed glyph and 150ms fades.
