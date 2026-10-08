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
  edge: "oklch(0.46 0.07 258 / 74%)"
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
  status-attention: "oklch(0.5 0.115 78)"
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
    width: "clamp(200px, 32%, 480px)"
  board-column:
    backgroundColor: "{colors.card}"
    rounded: "{rounded.lg}"
    padding: "14px 16px 16px"
    width: "clamp(400px, 36%, 1040px)"
  splitter:
    textColor: "{colors.muted-ink}"
    width: "32px"
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
streams; `assets/sluice.js` adds the drawer, edge drawing and tracing, `nav.js` the menus and
every page's live times, `kit.js` the kit's tabs, deep links, message box and confirmation dialog, `inbox.js` the message pages' read marks and closes (loading
`openui.js`, the answer forms, only on a page with an answer), `board.js` the project page's view switch,
splitter, description fold, board tools and the board's buttons. Every page works without JavaScript.

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
  phone): the water (`stripe-1`), the sky (`stripe-2`) and the sand (`stripe-3`). The board's
  title and its icon take `heading-accent`, a deep blue (a pale sky in the dark).
- **Ink / muted ink**: navy text (cream in the dark); muted ink for meta, captions and labels.
  Hairlines are ink at 11% (24% when stronger).

### Status ramp
One table names every state a step reads as (`sluice_model::shown::Shown`): the six stored
statuses, each named more closely from the facts beside it. Each state has one row: its glyph,
its word, its lane mark, its tone and its band, and whether it needs attention. Every surface
reads that row (the card, its caption and title, the matrix pill and dot, the lane strings,
the board's Units, StepStatus and Count, the summary line and its bar, the unit and step pages,
the index, the tab title and the screen-reader announcer); no page names a state by hand, and a
state added to the table and not drawn fails the build. The order of the rows is the one
priority: the state that most needs someone first. A unit, a project, a matrix row, the
index's order, Live first's band and every count read in it; what stands for several steps is
the first state among them.

| state | glyph (Lucide) | word | lane | tone | band | attention |
|---|---|---|---|---|---|---|
| failed | `circle-x`, solid | failed | ✗ | ink | Stopped | yes |
| cancelled | `circle-stop` | cancelled | ■ | muted ink | Stopped | yes |
| stale | `rotate-cw` | stale | ~ | gold | Stopped | yes |
| quiet | `hourglass` | quiet | ◔ | gold | Running | yes |
| blocked | `circle-minus` | blocked | ⊖ | ink | Stopped | no |
| stopping | `circle-stop`, turning | stopping | □ | muted ink | Running | no |
| finishing | `loader-circle`, turning | finishing | ▷ | blue | Running | no |
| running | `loader-circle`, turning | running | ▶ | blue | Running | no |
| external | `square-arrow-out-up-right` | outside | ↗ | blue | Running | no |
| paused | `circle-pause` | paused | ‖ | plum | Waiting | no |
| held | `circle-dot-dashed` | held | ∅ | grey navy | Waiting | no |
| queued | `circle-ellipsis` | queued | ≡ | grey navy | Waiting | no |
| pending | `circle-dashed` | pending | · | grey navy | Waiting | no |
| manual | `circle-dot` | set by hand | ⊙ | green | Done | no |
| succeeded | `circle-check`, solid | succeeded | ✓ | green | Done | no |
| skipped | `circle-slash` | skipped | – | grey navy | Done | no |

- **Failed**: ink, never coral, and the loudest state on the board: the cross in a disc; a
  card is a solid ink pill, its id, caption ("failed", 700) and timer in the card colour, its
  glyph a card-coloured disc cut in ink (`--glyph-cut`). The open step's ring stands 2px apart
  from it, so it never reads as chosen. A unit's label (the box head) adds "· 1 failed" at 600
  in ink, so a phone reads the failure before reaching the card at the bottom of its stack.
- **Cancelled** (a failed step the owner cancelled, read from its stored error by
  `shown::is_cancel`): a stop on purpose, not a fault, so muted ink: a ring with a square in it,
  the word "cancelled" (the card's caption too, its border the strong hairline), counted apart
  ("n cancelled", after any failure and quieter than it), and Retry a plain button, never the
  primary. A step waiting on it says so ("after up (cancelled)", "step up is cancelled").
- **Stale**: harvest gold, the attention voice (0.5 lightness in light, so its words hold
  4.5:1 on every surface); a gold-tinted border. It has its own tag on the summary line and its
  own bar segment.
- **Quiet** (a running step whose run has written nothing past its cadence): gold. Its glyph is
  the hourglass in place of the turning ring; its card wears a gold border and the caption
  "quiet" (gold, 600) with how long its run has written nothing, ticking, in place of its run's
  time.
- **Blocked** (a pending step behind a step that failed, was cancelled or went stale): a ring
  with a bar, in ink; a dashed card and the caption "blocked". Never a pause: that is paused.
- **Stopping** (a running step whose cancel was asked for, its run not ended yet): the
  cancelled sign, still turning, in muted ink; a dashed muted card and the caption "stopping".
  Cancel is not offered again.
- **Finishing** (a running step whose run has submitted): running's turning ring and blue,
  told apart by the caption "finishing" and the drawer's "finishing · running for" badge.
- **Running**: the logo's blue (a turning ring).
- **Outside** (`external`, a ready `core.external` step): live work outside sluice, so
  running's blue, told apart by its glyph (an arrow leaving a box).
- **Paused** (held by its own pause or its project's): plum, a hold someone chose; a dashed
  plum card.
- **Held** (pending on a plan input with no value): a dashed ring with a dot, grey navy, the
  caption "held".
- **Queued** (ready, short of a resource): a ring of three dots, grey navy, the caption
  "queued".
- **Pending** and **Skipped**: idle, a grey navy.
- **Succeeded** and **Set by hand** (`manual`, a ring and dot): kelly green; succeeded, the
  common case, steps back (its check at 78% opacity).

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
from the canvas in hue; cards are a lighter step off both; the board's title takes
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
  `badge` at least 4.5:1; status glyphs, the ring, the edges (the plan's wait-lines, on the page, a card and a unit box) and their arrowheads at least 3:1.
- **The mark** keeps its own colours in every theme: it is an `<img>`, never recoloured.

**Adding a theme**: an entry in `THEMES` in `src/views.rs` (its place there is its place in the
menu) and a block `[data-theme="<id>"] { color-scheme: light|dark; … }` in `style.css` that sets
every token above. Then measure it.

**The Shape Carries It Rule.** Every state has its own glyph, a Lucide icon at 16px (the
status ramp's table), named for a screen reader by its word; colour only repeats what the shape
says. Three pairs share a shape and part by motion and word: stopping is cancelled's sign still
turning, finishing is running's ring with the caption "finishing", and a turning glyph is work
in motion. Succeeded and failed fill Lucide's ring with the status colour and cut the mark in
the card colour.

## Typography

Two faces from cdn.jsdelivr.net (Fontsource), the system sans without them. **Archivo
Variable** (weight and width axes) is the display voice: the wordmark (800, 22px), page titles
(800, 28/34, 24/30 on a phone), a step's or unit's title on its page (800, 22/28), the board's title (750,
18/24), a project's name on the index, a question's title (750, 17/23) and every step id on the
board (600, 14.5/20, normal width). **Public Sans Variable** is the text face: 15/22 body, 14/20
small, 13/18 meta and labels. Every section head on every page is one style: Public Sans at
16/22, 650, in ink, 28px above and 10px below; a count after it (`.n`) is 14px at 500 in muted
ink, tabular, and left out at zero (the empty line under the head says it). Prose holds one
measure, `--measure` (64ch of the body face, about 72 characters a line): descriptions, docs,
failures, help lines, notes, message bodies and values. The system monospace is for data only: errors, fn names, values,
types, run ids, the log's seq and kind, and a step's or unit's id beside its title. Numerals are tabular.

A step is named by its title, not its id (`views::ui::StepRef`): the title in the text face at
500 in ink, its stage before it in muted ink ("land ·"), its id after it in data mono at 12.5px,
muted and never broken. A step with no title of its own shows its id alone, in the title's
place and weight. On its own page and its unit's the title is the `h1` (the stage muted inside
it) and the id is a line under it in data mono; the board's cards stay ids (a card is a step's
handle), with a solo unit's title over its card in 13px meta (two lines at most) and a unit's
title in its box label, 600 14/19, two lines at most, over its id.

### Named Rules
**The Meta Voice Rule.** What a run says about itself (times, counts, captions, labels) is 13px
meta in muted ink, sentence case. Nothing is uppercase; no kickers or eyebrows.

## Layout

### Named Rules
**The One Column Rule.** Every page sits on one centred 960px column (`--column`, with at least
a 24px gutter, 16px at 720px and below). The top nav's content aligns to the same edges (the
mark on the left edge, the display preferences icon ending on the right), so nav, titles, lists
and cards share one left edge at every width. Nothing makes the page scroll sideways. The one
widening: above a phone's width (from 721px) a project page with a board takes the window's
width up to 2400px (`--page-max`, with a 32px gutter), nav included, so its edges hold when the
board joins the plan at 1280px; from there the nav's edges are the plan's left and the board's
right: the plan (at least 560px, `--plan-min`), the splitter's 32px track (`--board-gap`) and
the board (`--board-w`). With the step drawer open beside the page (from 1200px) the gutter is
24px again and the drawer takes the side.

**The One Click Rule.** The board is names and states. Everything else (inputs, outputs,
errors, runs) is one click away on the step's page, or under the board for the plan as a
whole. Two things are said before that click, each in a few words: why a stopped step stopped
(its one sentence under its card or its matrix row, as a wait is said in words) and, on a
retry, its run's number with how the runs before it ended.

### Navigation
- **One bar** (the band, `nav-bg`, full width with its content on the column, its three
  stripes under it, a `<header>` on every page, its `nav` named "Main"): the brand (the owner's mark, `/static/logo.svg` at 27×26, and the wordmark
  "sluice" as live text; one link to the index, named "sluice: all projects"); the **project
  switcher**; in a project, the **project settings** gear; the sections; **Inbox**; and the
  **display preferences** sliders at the right edge. Every page links `/static/favicon.svg`, the
  same mark.
- **Project switcher** (its projects in the index's order: by the state each reads as, the
  status ramp's order, then those with no steps): a `<details>` whose summary is the chosen project's name ("All
  projects" when none) and a chevron, its label clipping at what the band leaves after the
  rest (120 to 260px), so a long name gives way before a section does; its menu lists All projects, then every live project with
  its status glyph and icon, then the archived ones under "Archived". `nav.js` closes it on a
  click elsewhere or Escape.
- **Sections**: in a project Plan, Messages, Log, Functions; without one Log and Functions (the
  index is the switcher's "All projects"). Messages is the project's inbox, questions and
  history behind one section (its tabs on the page): the one Inbox in the nav is the tray. The current one is `nav-ink` with
  a bar on the band's bottom edge and `aria-current`; the rest `nav-muted`.
- **Inbox**: the tray (Lucide `inbox`), the word "Inbox" and the coral badge with the number of open
  questions to the owner across every project (none when nothing waits). It always leads to the
  home-wide inbox, is current there (and on Questions and History), and its name says the count
  ("Inbox, 3 open questions, 2 unread notes") when the word gives way to the tray alone. With
  no question open and unread notes waiting, an 8px dot in the band's ink sits on the tray's
  corner (coral stays the questions' alone).
- **Project settings**: the gear (Lucide `settings`) linking `/projects/id/<id>/settings`,
  shown only in a project. Its hover and its current state (on the settings page) are the same
  36px rounded fill as display preferences', its focus ring drawn on that fill.
- **Display preferences**: horizontal sliders (Lucide `sliders-horizontal`, so it never reads
  as a second settings gear) opening a `<details>` menu: "Theme", a radio list of the seven
  themes (each its name and a swatch: "Aa" on the theme's canvas cut by its band and stripes,
  a tick on the chosen one), led by "Match system" (the house pair's two swatches; chosen until
  a theme is picked, and the way back to following the OS), then "Show value types". It is a form posting to `/settings`
  (cookies `sluice_theme`, `sluice_types`); without script its Save button sends it.
- **A narrow band** (on a phone, or beside the open drawer): the band is a size container
  (`nav`), so it keys off its own width, never the window's. Under 880px of content it is
  compact: the wordmark gives way, the mark stays as the way home (its label clips at 140px; the switcher's menu
  leads to All projects), Inbox shows only its tray and badge, the sections close up (14px,
  6px apart). Under 640px the sections take a second row of the band (44px tall, the current
  one's bar on the band's bottom edge, from the column's left edge), so none hides behind a
  sideways scroll; the first row is the switcher, the gear, Inbox and display preferences.

### Projects (`/`)
One list of the live projects, most urgent first: by the state each reads as, its steps' first
in the status ramp's order (a failure, a cancel, a stale step, a quiet run, then running, then
waiting; by name within each). Each is a row: its status glyph, icon and name (a link), when it
last changed; for one
with failures or cancels a list of its stopped steps, a row each: the step's glyph (failed, or
cancelled in muted ink), the step named as every page names it (title, then its id in data
mono; a link that opens it in the drawer) and its failure's headline cut with an ellipsis in
meta, a link to the failure's own record on the log when the log keeps it, the failures first and at most four rows, then "and n more" (a link to Show: Attention);
its description's opening; a progress bar (a segment a state, as the board's) with "n of m"
and "· n paused" in plum when some are; then either its running steps (the glyph of how each
reads: running, quiet, finishing, stopping, with a muted "stopping" or "finishing" tag; the step named as every page names it,
its stage, title and id, the title cut at 96 characters, its live time and a gold "quiet" tag once a run has written
nothing for its cadence (its plan's `cadence:` tag, else 2 hours; one threshold for every page):
"quiet 2h 42m", or "quiet" alone when it has written nothing since it started; on a narrow row
the time and tag take the next line, never squeezing the title) or one line on what
stops it ("Paused.", "Stopped: nothing is running."). Projects with no steps fold under "No
steps yet (n)", the archived ones under "Archived (n)". While nothing holds the
scheduler lease a box heads the list and outweighs every tag under it: a hairline of the
attention gold at 55% all round on a 9% gold wash, the 14px corner, Lucide `triangle-alert` in
gold, "**Runner stopped.** No step starts until `sluice loop` runs; running steps carry on." at
15/22.

### Board (`/projects/id/<p>`)
Top down:
0. **Title**: the project's status glyph (22px), icon and name, the page's `h1` (28/34).
1. **Summary line**: two rows, the switch beside both at the right (on a phone, under them):
   the progress bar (a segment a state: the done states first, then the rest in the status
   ramp's order; a segment only for a count above zero; it grows with the line, 200 to 480px,
   so a few failures among hundreds still read; named for a screen reader by every count),
   "N steps" then every state that needs no attention, counted in the bar's order ("· 1077
   succeeded · 14 running · 6 paused · 40 pending"; each step counted once, under its state:
   one name and one count on every surface, the summary line, the home row, a matrix's head,
   the board's Count and Units, a unit's sum), then, only when there are some, weighted tags
   for the states that need attention, each leading to Show: Attention with its title saying
   what it means: "n failed" (the failed glyph, ink border, 600), "n cancelled" (the cancelled
   glyph, muted), "n stale" and "n quiet" (gold), on one baseline, kept together as a group so
   a phone wraps them as one line under the counts; and a Paused tag.
   A plan with no steps has no bar and no counts, only "The plan has no steps yet." The title stays over
   the board when the board is shown alone, on a phone too; then "Paused: no step starts." or "Archived: listed
   apart from other projects." when so; then, 12px below, the project's description as
   markdown: its first block (a heading takes the block after it too) and the rest folded in a
   `<details>` under a quiet "More" (a chevron; "Less" when open), closed by default and
   remembered per project. Without script the fold still works: it is native.
2. **Board tools**, 24px below the description: a GET form (`role="search"`): a search field
   ("Find a step", Lucide `search`, a clear `x` while it holds text), Order (Live first, Plan
   order) and Show (All, Active, Attention, Done) selects, Apply, and at the row's end a quiet
   "more" menu (Lucide `ellipsis`) holding "The plan as Mermaid text" (`plan_view`'s
   `flowchart TD`, a subgraph per unit): not a main control. With script each applies as it changes and the search as one
   types (200ms), without reloading: the address follows (`history.replaceState`) and the
   page's stream restarts under the new query; Apply is for a page without script only. Escape
   or the `x` clears the search. With a search a meta line under the tools says how many steps
   match ("12 steps match “land”", "No step matches “x”.", a polite status). Live first draws
   the stopped units, then the running, then the waiting, each band laid out by dependency
   depth; Plan order draws every unit not done in one band by depth, in the plan's order where
   depth ties. Either way every done unit is on one shelf at the end. The search keeps the
   steps whose id, title, doc or unit id holds every word of it (any case, any order); a unit with
   none hides, and an empty board says "Clear the search to see every unit." A plan with no
   steps has no tools: it says "The plan has no steps yet."
   A focused board says so in one meta line under the tools, at the measure, ending in "Show
   everything" (muted, underlined; the board as a whole): a step's chain (`?root=`), "Showing
   the chain of <the step named as every page names it>" ("Showing what … comes after", "…, 2
   steps each way"), or a recipe's units (`?recipe=`), "Every unit of recipe `lane`: 189 units ·
   4 running · 1 paused · 7 waiting · 177 done." The tools' form keeps the focus in hidden
   fields, so Order and Show apply within it.
3. **Lane matrices**, inside the bands: a recipe with a view (SPEC §13) draws its units that
   are not done as tables, one per band its units fall in, each first in its band (so a failed
   lane is under "Stopped", above the running lanes' matrix, never below a table of healthy
   rows), on the box fill with the 14px corner, headed like a band label with the recipe's
   name in ink at 600 (a link, underlined on hover, to every unit the recipe made, done ones too:
   the board under `?recipe=`, its shelf open) and its units counted, each row once under the
   state it reads as ("3 units · 1 failed · 1 running · 1 pending"). A row a unit: the first column its status glyph, its title (600, ink, a link to
   its unit page, two lines at most), its id in data mono and any alarm under it; then the
   view's summary in 13/18 (its parts in a line: a param at 500 in ink, an output in data mono,
   a message clamped to two lines), its column headed by what the view shows ("Ticket · last
   message"); then a column per stage, headed by the stage in 12.5px meta and, when its
   column holds something that needs a look, what in ink at 600 ("land · 2 failed · 1 quiet",
   the attention states counted in the ramp's order), each cell that
   stage's card as a compact pill (28px, glyph, caption and timer, no id: the column says it),
   centred; a stage nothing has reached yet (pending, paused, blocked or held) is a 24px mark
   instead of a pill (pending a small dot, the others their glyph at 14px), so a row's live
   work stands out from its tail. Rows go in the status ramp's order of the state each unit
   reads as, the plan's order where they tie; rows part by a hairline. The head row sticks to
   the window's top while a tall matrix's rows pass under it (its wrap clips rather than
   scrolls, so nothing between it and the page scrolls). A row with a failed or cancelled
   stage says why under its id: that stage's one sentence, muted, a link to the step. A matrix is a table and draws no
   lines: what a row waits for is said in words under its title ("Waits for land · Stop the
   parser leak `l2-land` (failed)"), and no line from elsewhere crosses its cells (a line to or from a row would have
   to). A view that does not check is one line above the table in the attention colour with
   `triangle-alert`, and the table draws without the summary column. The matrix keys off the
   `plan` container, never the window: at 720px of pane and narrower a row is a block: the
   glyph and title, the id, then the stages as a lane string in 13px (`fork✓ work▶ land·`, each
   a 44px-tall link to its step), then the summary; the stage columns and head are gone. Between
   720px and 1100px of pane (a laptop with the drawer open) the summary column gives way and the
   title column narrows, so the stage columns are never clipped and the page never scrolls
   sideways.
4. **The board**: the plan as a graph that reads top down with no key. Every mark explains
   itself: a card is a step (its glyph and id), a box is a unit of several steps, a line with
   an arrowhead is "this, then that", a quiet label names each band.
   - **Bands**: under Live first, each unit under its first state's band (the status ramp):
     "Stopped" (failed, cancelled, stale or blocked first: a unit with a failure and running
     work is stopped), then "Running" (quiet, stopping, finishing, running or outside), then
     "Waiting" (the rest not done), each band its recipes' matrices first and its graph under them, each label an `h2` under the page's `h1`, set in meta (13px, 500, muted ink) at the
     column's left edge, 32px after
     the band before. Under Plan order there is one band and no label. Show and the search
     only leave units out; the bands keep their order.
   - **Layers**: a band lays its units in layers by the longest chain of waits among them, so
     every line from a unit to one that waits for it runs down to a later layer. A layer is a
     centred row that wraps (16px between rows, 32px between units, so a line passes between
     two units), 40px from the next layer for the lines; its units are ordered after where the
     units they wait for were placed, so lines stay short and seldom cross, ties in the view's
     order. Units no line joins to any other unit (nothing they wait for, nothing waiting for
     them) are not layered: they follow the band's layers in one row packed from its start,
     tallest first, so no hole under a short box implies a wait that is not there; centring is
     kept for the layers lines join.
   - **Units**: a unit of one step is its card alone, no box and no label: drawn once. When
     the step's id does not hold the unit's name, the card names the unit first in muted ink
     ("build / compile"). A unit of several steps is a box (the theme's box tone, 14px radius,
     16 by 18px padding, 12px on a phone, no border) labelled with its id in meta, its cards
     in rows by dependency depth.
   - **The done shelf**: every done unit (every step succeeded or skipped), under either order,
     on one `<details class="done-shelf">` after the bands: the success glyph, "44 done units ·
     44 steps" (14px, 500) on the box tone with the 14px corner, and a chevron; closed by
     default and remembered per tab. Open, the latest 20 done units, finished newest first,
     in one column of rows on the box tone, each with when it finished at its line's end ("2h
     ago", 12.5px muted): a one-step unit a row, a link to its step: the glyph, the unit's id
     first in muted ink when the step's id does not hold it, the step named as every page names
     it (its title, its id after it in data mono, the title giving way to an ellipsis before the
     id does), how long it took, and when; any other one line, a `<details>`: the success glyph,
     its title and id as a step's are named, its steps as the board's lane strings write them (`fork✓ work✓ land✓ rm–`, data
     mono 12px in muted ink, the step's id without the unit's prefix and its mark, a step named
     as its unit its mark alone; each mark in ink at 700 and 13px so a ✓ never passes for the
     pending dot; a lane too long for its line gives way at its start, so its last steps (land,
     close, rm), which say how it ended, stay in view; whole in its title; a screen reader hears "6 steps done") and a
     chevron, in a grid of wider cells (`minmax(360px, 1fr)`); the line opens to its cards. A
     search that matches in a done unit, or Show: Done, draws the shelf and the matching units
     open (under ids of their own); opening a step on the shelf in the drawer opens the shelf
     and its unit. Under the 20, "The latest 20, newest first. Show all n" in meta, its link
     Show: Done, which draws every one; the page sends the lines' data only for what it draws.
   - **Empty**: a board with no units to show says why, for the view: "Nothing needs
     attention." (Attention), "Every unit is done." (Active), "No unit is done yet." (Done),
     else "No units match this view.", at the column's left edge; under a Show that left units
     out it says how many and offers them back: "Nothing needs attention. 12 units hidden by
     Show: Attention. Show all", the link the same view with Show: All.
   The plan pane is a size container named `plan`: what lays out the plan keys off the pane's
   width, which the splitter changes, never the window's.
5. **Lines** (the `<sluice-board>` element draws them in an SVG over the measured cards, from
   the relations the server marks `line`): within a unit's box every relation; between units
   each wait, from a source that has not yet succeeded or been skipped to a step the view
   shows in a unit not done. A satisfied wait is history, so it has no line and no words; the
   step's page lists every gate. One path a pair of cards, however many relations join them.
   A path whose order a longer path already gives (its source reaches its dependent through
   two lines or more) is not drawn, a value passed along it too: the board shows what comes
   after what; a condition and an `after?` stay, each saying more than the order. Every line
   looks alike: 1.5px in the edge colour (3:1 or more on the page, a card and a unit box, still
   quieter than the arrowhead), an arrowhead on the card it enters. It leaves the bottom of its
   source's card (a unit gate, the bottom of its box) and enters the top of the card that
   waits, spread along each in the order the lines head off and come in (a bypass on the right
   leaves and enters on the right, so hooks never cross); between units it passes every card
   and every other unit in between through the gaps of each row, never over a unit, 12px
   further from a unit's box than from a card so it never runs along a box's border, and past
   a box's label as past a card. What a line carries beyond order is in words, shown by its far end while
   a card is traced and in its `<title>`: "text → data" for a value passed, "if ok" and "if
   not ok" for a condition, "even if skipped" for an `after?`; plain order needs none. The
   lines are drawn on the client and kept across the stream's patches (`data-ignore-morph`),
   redrawn when the cards move, a box opens or the board resizes.
   - **Waits in words**: under a card that waits on another unit, one sentence in meta (13px,
     muted ink), "Waits for land · Ship the docs `l-a1` (running) and unit Kit `kit`": each
     source named as every page names a step (its title, its stage before it, its id after it
     in data mono; a unit "unit" and its title and id), a link in ink to its step
     (the drawer with script), what it is doing in brackets when it is not merely waiting
     itself (its state's word: running, failed, cancelled, stale, quiet, paused, outside, …);
     past four sources the first three, then
     "and 79 more", a link to the waiting step, whose page lists them all. A name wraps with the
     sentence (its comma kept with it), and these words and a stopped card's sentence sit over
     the lines on their own fill, so a line leaving the card above passes behind them, as behind
     a label. Each link is a 24px
     target. The sentence shows where no line says it: on a phone, on a page without script,
     and where the view leaves a source out ("…, not in this view"). Hovering or focusing a
     name traces its source.
   No line runs up the page: a wait whose source is drawn below the step that waits (another
   band, or a later layer) is said in words under the card instead ("Waits for alpha-review"),
   and so is a wait into or out of a matrix's row or one that would cross a matrix between its
   ends. Such a sentence shows at every width, not only where lines are off.
   On a phone the page opens on the plan while something needs the owner (a state that needs
   attention, or a paused step), else on the board when the project has one.
   Below 720px there are no lines: the layers stack in one column, a one-step unit's card at
   the left with its waits under it (under its glyph's column), a box across the width
   stacking its lanes, each reading straight down.
6. **Under the board**: Result (each plan output's value, or "No value yet.") and Plan inputs
   (value or "No value yet.", and the input's doc).

### The project's board (beside the plan)
A project may carry a board (`docs("board")`): an OpenUI program the server draws with the
project's live data. It is the owner's instrument for that project, never a second plan.
- **Wide (1280px and up):** the page spans the window (see the One Column Rule): the plan, a
  splitter, and the board as a right-hand column (`board-pane`) top-aligned with the summary
  line: the card colour with a hairline and the 14px corner, 14 by 16px padding, sticky 16px
  from the top and at most the window's height, scrolling on its own: while more of it lies
  past an edge, a soft 12px shade (ink at 12-14%) sits along that edge inside the box, gone at
  its ends, so a board cut off mid-chart says it scrolls. Its head (section voice,
  `heading-accent`, Lucide `layout-dashboard`) is the program's own title when a level-1
  Heading leads it (drawn there once, not again under it), else "Board"; under it in meta,
  "Updated 2h ago" (the last change to its words: the program's `project.board` record or a
  document edit's `project.update`; the time in UTC without script), and "; the plan has changed since"
  when a plan edit came after it. A board that draws a written document says it once, on the
  document's own "Edited … by …" line, and the head leaves it out; every such time reads "2m
  ago" on every page with script, the same at every width. The "plan has changed" note goes
  where the time is, since the
  board's own words may then be behind while its live parts are not. Its width is
  `clamp(400px, 36%, 1040px)` until the splitter sets one (about 495px at 1440, 864px at
  2560), so its Units table keeps a lane's marks on one line beside the unit and its state. A small segmented control,
  "Plan" (Lucide `workflow`), "Both" (`columns-2`, the default) and "Board"
  (`layout-dashboard`), shows the plan alone, both, or the board alone across the page (its
  parts then take the pane's width up to 1680px, every table as wide as the rest so they share
  one right edge; prose keeps its 72ch); the choice is remembered per project. It is part of
  the summary line, at its right end, as wide as its words; with the board alone, the summary
  line keeps only it, at the right above the board, under the project's title. An address that
  asks for a search, a Show or an Order opens on the plan whatever was picked. With the
  step drawer open the drawer takes the side: the plan shows, the board and the switch step
  away until it closes.
- **The splitter** (script only): a 32px track between plan and board, a `role="separator"`
  (`aria-orientation="vertical"`, `aria-controls="board-pane"`, its values the board's width
  in px) in the tab order. At rest a Lucide `grip-vertical` in muted ink at the board's middle;
  on hover a 2px hairline (`border-strong`) the board's height and the grip in ink; focused or
  dragged the line is the ring's blue and the grip ringed. Drag (pointer capture, one layout
  per frame; the plan's edges hide and redraw once at the end), ←/→ 16px (64px with Shift),
  Home/End to the least (320px) and greatest (the lesser of 65% of the page and what leaves the
  plan 560px), double-click to reset. The width is remembered per project
  (`sluice.boardw.<project>`); the page's grid keeps a remembered width within bounds.
- **Narrow (below 1280px):** the same switch in the same place, "Plan" and "Board" only, the
  current one in the secondary fill, then one section at a time (the head says "Board" only
  when the program has no title, the switch naming it already); on a phone it follows the
  summary line's bar and counts, on a line of its own and 44px tall, and the board is shown
  until a choice is made (its Units table is the quick check); between a phone and 1280px the
  plan is. The choice is remembered per project, apart from the wide one. Without script there is no switch and no
  splitter: from 1280px both show side by side at the default width; below, the board follows
  the plan, flat on the canvas under its head.
- **No board:** nothing at all: no column, no switch; the plan keeps the whole column.
- **Its parts** keep the question forms' look (`ou-*`): headings in Archivo, text at 15/22,
  callouts, tables at 13px with hairline rows; a word with a hyphen inside it (a step or
  unit id, `FIG-5004`) never breaks, in markdown too, so a narrow board breaks between ids.
  Headings nest under the column's h2 without skipping a level: a titled board's level-2
  Heading is an h3 (an untitled one's level 1 is), and markdown's headings start one level
  under the heading drawn last; a level-1 Heading is 17/23, the others 15/21. The board pane is a
  size container named `board`. Units is a table of unit (a link), state (the status glyph and
  word, then its age in meta), the steps' marks in data mono (on one line; the table scrolls
  sideways in its own wrap before a lane breaks) and what it waits on in muted ink (breaking
  anywhere, so a long path never widens the table); under it a one-line key to the marks it shows,
  in the status ramp's order. The state, its glyph and every mark are the plan's own reading of
  the unit and its steps (a stale unit "stale", a cancel ■, a quiet run ◔, a project pause ‖),
  never the units filter's word: the filter (`Units(["failed"])`) only chooses the rows. In a board under 600px each row is a block: the unit, its state and its marks on
  one line where they fit (the marks take the next line whole when they do not, breaking
  between steps only past the board's width), then what it waits on. StepStatus is the step's own card (pill, glyph, id, caption) with the reason
  under it in meta. Output is its name in meta over the value; a value from the step's progress
  (`step_progress`, fresher than its outputs) has a line under it: a small badge, "live" led by
  the running glyph (its blue spinning ring, the badge's border a blue hairline) while the step
  runs, or a muted "progress" once its run has ended, then when it was set in meta ("2m ago",
  the time in UTC without script). An output never carries the badge, so a value that is not
  final never passes for one. Metric is a number in Archivo
  800 at 28/34 over its label, on the box tone; metrics in a row share it. Chart is an inline
  SVG at most 520px wide: bars and the line in the accent, labels in ink and values in muted
  ink at 12px, its caption in meta under it. Doc, Markdown and LatestMessage are markdown at
  the text's 15/22 and the measure, drawn as message bodies draw (escaped, unsafe links
  dropped). Doc is the board's document, the hand-written part that reads as one piece of
  prose. Its first section shows (up to its second heading); the rest folds under "Read more"
  (a chevron; open, "Read less" sits at the document's end, where the reader is, folds it and
  brings its head back into view; without script the summary stays the toggle), open where the board has the room (1280px and up) and
  closed below, so on a phone the live parts after it (the Units table, the quick check) stay
  near the top while the author's order is kept; a toggle by hand is kept: its first rank of headings in the board's heading voice (Archivo 750 at 17/23), the
  next at 15/21 and any under that at 14/20 in muted ink, 18px above a heading and 4px under
  it; quotes a 1px strong
  hairline at the left in muted ink; under it "Edited 12m ago by orchestrator" in meta (the time
  a `data-ago`, UTC without script; "; the plan has changed since" when a plan edit came
  after it), the board's one time, so the owner sees how fresh it is and who wrote it without
  the board saying so twice; before it says anything, its fallback in muted ink, or "Not
  written yet.". A Metric, Query, Chart or LatestMessage whose step is no longer in the plan
  keeps drawing what it has, under one line in the attention gold at 13/18: Lucide
  `triangle-alert` at 14px, then "Names step `x`, which is not in the plan; this shows its last
  data.", the id in data mono in ink, so a frozen value never passes for a live one.
  LatestMessage leads with a meta
  line, the sender in ink at 600 and its time as a link to the message in its thread, then the
  body, and when it is cut, an ellipsis and "The whole message" in meta under it. Buttons are the dashboard's buttons; what a press
  did (or why it was refused) is a status line under the board.
- **A part that cannot be drawn** is a small box in its place: the muted fill, a hairline of
  the attention gold at 45% all round and the 10px corner, Lucide `triangle-alert` in gold;
  the component and line in 600, the reason in data mono. Never coral, never red, never a
  side tab.

### Step (`/projects/id/<p>/steps/<s>`, and the drawer)
On the board, opening a card with script loads the step into a right-hand drawer (beside the
page from 1200px, `min(680px, 45vw)`; over the page on a scrim below that; full width on a
phone) and puts the card's ring on; without script the card's link opens the step's own page.
The step reads top down:
- on its own page only, a way back above it, in 14px muted ink: "← <project> plan / unit <u>"
  (Lucide `arrow-left`; the unit named by its title, cut at 64 characters, its whole title on hover); there its title is the page's `h1` and each section's head an `h2`, in
  the drawer its id a 22px `h2` and each head an `h3` (one look either way: 15/22 at 600), so
  no level is skipped; the tab reads "<stage> · <title> · <project> · sluice" (the title cut at
48 characters);
- its id with badges: the state's glyph and word, one tag ("blocked", "set by hand"); while
  it runs "running for 2h 14m" in that tag, led by its state when it is more than running
  ("quiet · running for 2h 14m", "stopping · …", gold when it needs a look), no second line
  repeating it; "usually 24m" in meta after the tags while pending or running (how long its
  stage usually takes; " · usually 24m" after "took …" once ended); "done/total runs" when scattered; its doc; "Ended 3h ago · took 10h 0m" once it has ended; a meta line of the fn in mono and its tags as badges (a `unit:` tag a link to its
  unit's page);
- the actions, one POST form carrying the plan revision: Pause or Unpause (where pausing acts:
  pending, stale, or any paused step; never a failed or cancelled one, which starts only when
  retried), Retry with a folded "Feedback for retry" textarea
  (succeeded, failed, stale; on a failed step Retry is the primary button and comes first; on
  a cancelled one it stays a plain button),
  Cancel (running, not already stopping; or pending work outside sluice), 14px apart; it opens the shared confirmation dialog, titled by the step's title with its id in data mono ("Cancel Fix the parser `l1-work`?"), names the run and its duration, says Retry starts it over, and offers an optional reason, "Cancel the run" and "Keep running"; then, while it has no messages, a link
  "Thread · no messages yet" to its thread's page (where the owner can write to it), "Log" (the
  log of its records, filtered to the step) and a gold "n awaiting reply" tag leading to its
  Thread tab, on the same line, the feedback's fold on a line of its own under them;
  a failure that says how to resume ("To resume it, bind the step's session input…") under
  them in meta at the measure, its tool call as code; a meta line under the description says
  when it ended ("Ended 3h ago · took 2h 14m", "Failed 8d ago" from its result when no run is
  kept);
- under the header, **tabs** (the kit's, below), each only when it has something to show and
  with a count where one helps: **Overview** (always, and where every page opens), **Activity n**
  (its turns), **Thread n** (its messages), **Inputs n**, **Outputs n** (or "3/7" while some are
  not set; a screen reader hears "3 of 7 set") and **Runs n**. `?tab=` opens another; an anchor
  in a tab (`#run-3`, a failing call's, `#message-12`, `#activity`, `#tp-outputs`) opens its tab
  on it. In the drawer the tab follows the owner from step to step and a link inside it never
  touches the address (its hash is the open step's).
- **Overview**, in need order: a failed step's failure first ("Why it failed", or "Cancelled":
  below), its latest own message under it ("Its last message"); while it runs **Now**: on a
  retry one meta line for the run before ("Run 2 failed 2h ago: Its engine hit a usage cap.", a
  cancel's reason after its colon, "Run 2" a link to that run in Runs); when it is quiet,
  "Nothing written for 6h 18m." in gold at 600 with the hourglass; then its latest exchange as
  messages (the conversation's look, below): its own latest (to anyone) and the latest to it
  since, each its first 360 characters as inline markdown with "Read it in the thread" (its
  Thread tab, on that message) when cut, or "It has sent no message yet."; its live progress
  (before the messages when it is newer); and its activity in one meta line, "Run 3 · 9 turns ·
  33 tool calls · Its activity", the link its Activity tab. Then the facts ("Waits on", "After"),
  Queued, Skipped, Finishing, Outside sluice, progress kept from an ended run, **Its output**
  (the first set of `summary`, `result`, `report`, `final`, `answer` or `verdict`, as Outputs
  draws it, "All outputs" beside its head), a finished step's last message, and "Show its
  chain on the plan" last. With none of these, the state's own sentence from the status table
  in an empty state.
- facts: "Waits on" (each reason; a pause in words with the paused glyph in plum, "Paused by
  the owner: <reason>. 3h ago" from the plan edit that paused it, "Paused." when no edit names
  who, "Its project is paused."; "after X" left out when After lists X) and "After": four entries or fewer inline; more as a sentence
  ("98 steps, all done", "6 steps: 5 done, 1 running") folded over the entries, sorted, each a
  link with its glyph, one per line-break unit (`nowrap`);
- Overview's sections under small heads (an `h3` on its page, an `h4` in the drawer): Finishing
  (when it submitted, as a relative time, the record's seq and the release in mono, then a meta
  note that `step_settle` settles a run that lingers), "Why it failed" (or "Cancelled"): one plain
  sentence from its failure kind at 500 ("Stopped at its wall-clock cap after 10h 0m.", one
  wording for an agent's cap and a fn's; for a fn its traceback's last exception, else "Its fn
  failed: <its first line>", a trailing colon giving way to the full stop; for a cancel its
  reason), then "Its log record 4521" in meta, a link to the failure's own record on the log
  (the step's records up to it, it first and marked), what it said under it (a captured tail
  that starts mid-sentence led by "…") (one line in 13px data; several as prose in the body's
  font, lines kept, in the muted box), "Its last failed call: Bash `cargo test …`" (a link that
  opens the Activity tab on that call), the traceback folded under "Traceback", the pane its
  agent left folded under "Pane at failure" (a chevron; the rows in 12px data, scrolling in
  their own box), and "Run files" linking each file the run has (`file-text` icons) as plain
  text.
- **Thread**: the step's conversation (below): its own thread and every message it sent or was
  sent on another (a question it put to a sibling, the sibling's reply), its latest 60 with "n
  earlier messages not shown. Read the whole thread", the message box to it at the end.
- Activity (an agent run's, read from its transcript when the page draws): a meta line "Run 2 ·
  8 turns · 34 tool calls · 2 failed · read from its Claude session transcript" (and a link to
  the raw transcript when the run's directory holds one), then one row per turn, numbered in the
  attempts' muted column (gone on a phone, where the turn takes the column's width): a card (the
  hairline, `--radius-md`, the card colour) whose summary is what sluice sent ("Task", "Message"
  or "Sent" in 13px muted, then its first words at 500, two lines at most), "Said" and the
  agent's last words (three lines), and a meta line "12 tool calls · Bash 8 · Edit 3 · Read 1 ·
  took 4m" with "· 1 failed" in ink at 600 behind the failed glyph; the live run's open turn
  leads its meta with the running glyph and "running for 6m", ticking, and takes running's blue
  border, a turn with a failure the strong hairline. A chevron at the summary's right turns as
  it opens. Open, its calls hang from a hairline tree (a 1px rail, an elbow to each row, the
  last one rounded; borders only), one 32px row each (44px on a phone): the call's kind as a
  16px Lucide icon in muted ink (`terminal` a command, `file-pen` an edit, `file-text` a read,
  `search` a search, `folder-open` a listing, `bot` a subagent, `globe` the web,
  `message-square` a sluice tool, `wrench` anything else), the tool at 600, its key argument
  in 12.5px data mono, muted and cut with an ellipsis on one line (two lines, wrapping, under
  the tool on a phone), then at the row's end how long it took in tabular meta and, for a
  failed call, the failed glyph (its key argument in ink); a call its run ended before it had a
  result says "no result". Two or more reads, searches or listings in a row fold into one row,
  "3 reads, 2 searches" with the search icon. A call opens to its arguments as name and value
  rows (12.5px, the value in data mono, wrapping, a long one scrolling in its row) and its
  result under "Result" (or "Error"), "12,408 characters; its start and end are kept" when
  cut: in the mono box, a long one faded behind "Show all", a failed one's in the error box
  scrolled to its end. Earlier turns with no failure fold to one line, "2 earlier turns not
  shown, none with a failed call. Show earlier turns"; a failed call and its turn never fold. On
  a failed step the last call that failed is open, "Why it failed" ends with "Its last failed
  call: Bash `cargo test …`", a link that opens the Activity tab on it (the page opens on Overview,
  the failure first).
  No colour but the status ramp's: failure is ink and its glyph, never red.
- Progress (a step's `step_progress` values while they are fresher than its outputs): the
  small head "Progress" with the same badge as the board's Output ("live" with the running
  glyph while the step runs, else a muted "progress") on its centre line, a meta line "Set 2m
  ago by its current run. Not final: no step reads it." (or "by its last run, which ended
  without making it outputs. Kept until the next run starts."), then its fields as the
  Outputs draw theirs.
- Outputs and Inputs are their tabs, field lists (the kit's field rows): the name (with its type after it, shown by the Types
  switch at the Outputs head or the display preference) in a narrow column of meta, its doc
  and "From <source>" (linking to the source step) under the name at 12.5px, the value beside
  it, so every value starts at one x; an input says where its value came from under its name
  ("Default", "From <step> · <output>" with the step a link, "Plan input <name>", "A file, read
  when the run starts"); an output what set it when that is not the run the rest came from
  ("Run 1", "Set by hand"), the rest said once at the tab's top ("From run 2 · 1h ago"), and a
  long value equal to an earlier one's says "Same as summary" (a link to it) instead of
  repeating it; a value reads by its kind (text, a tabular number, a
  `true`/`false` pill, a muted "none", a file reference's path in code, a small flat object as
  names and values on one line, "type normal · model sol"); a long value folds, its name, doc
  and source across the row above it: long text as markdown in the body's font at 14.5/22 and
  the measure, its headings nested under the section's, JSON and code in the mono box; its first
  lines show, faded, with "Show all" under them (the toggle alone is the disclosure; the value
  is never inside it). The name column is one width on every page (34% up to 11rem; 44% up to
  16rem with types), so Outputs and Inputs line up. Inputs that are on/off switches the fn
  does not document (`listen`, `queued`) are one meta line under the fields: "Switches
  cenote.worker reads (it does not document them): listen on". A type that is a JSON schema reads as its type
  ("object"). Where a value comes from shows only when it says something (a source step, a
  file, a plan input, "Submitted so far"); "As its last run received them." or "A value not
  otherwise set is its default." says the rest once under the Inputs head. A running step's
  Outputs say whose they are under the head, "From run 2 · 1h ago": its last finished run's. The outputs not set yet are named on one line after the set ones, "7 outputs
  not set yet: summary, final, …" (each name's doc its title); an unset input reads "No value
  yet."
- Runs: first, its unit's timeline folded under the head ("Its unit's timeline", "Timeline" for a
  unit of one step; the fold's look, closed by default), the step's own row marked; then one row
  per run, numbered (its ordinal; its id in data mono among its facts as "Run
  id"), with its own outcome's glyph and word (Running, Succeeded,
  Failed, Cancelled) and "ended 3h ago · took 2h 14m" (or "started 5m ago"), its failure's
  sentence (not on a failed step's last run, whose sentence leads the page), then its result as
  labelled facts in 13px (Kind in plain words, "wall-clock cap", "fn failure"; Said, left out
  on a failed step's last run whose words lead the page; Outputs by name, On completion, Engine, Session,
  Run, Files), never escaped JSON; an agent run's calls by tool on a meta line under its
  outcome ("Bash 42 · Edit 9 · Read 17 · 2 failed"); the current run last on the
  secondary fill, inside the column (no bleed past its edges).

### Unit (`/projects/id/<p>/units/<u>`)
A way back ("← <project> plan"), the unit's title as the page's `h1` (with the success glyph when
done), its id in data mono under it with "· from recipe `lane`" in meta when a recipe made it,
then its recipe's view drawn whole in a card-filled block at the measure (its parts in a
row, a column stack as a grid; labelled "Summary" for a screen reader; each param, output
and stage status named before its value, "engine opus"), and a sum in meta ("6 steps · 1
running · 4 pending · 1 succeeded · Log", the link its log filtered to the unit); its box as
the board draws it, with the lines inside it, without its label (the h1 names it), its
layers from the column's left edge, a done unit open to its cards; each card that waits on
another unit says it in words ("Waits for l-a1 (running), not in this view"); "Last message":
who sent it and when, then its body as markdown at the measure, and "Read the thread". Between
the box and the last message, once something in it has run, **Timeline** (a section head): see
Components.

### Messages (`/inbox`, `/questions`, `/history`, `/projects/id/<p>/…`)
A page title and a small segmented control (Inbox · Questions · History, the current one in the
secondary fill). The questions someone waits on come in two groups, each an `h2` with its
count: "For you" ("Questions for you" on the inbox; what the nav's Inbox counts) and "Between
agents" (one agent's question to another, under a meta line that its addressee answers it).
Each is a card: its title (an `h3`, Archivo 17px; without one, its body's first line, cut at a
word with an ellipsis, never "Question"; when that line is the whole title the body under it
starts after it, so the opening is never said twice), a meta line (its state's tag, "from X" and "to Y"
when not the owner, each named as the conversation names them, when, its id in data mono,
"project · thread" linking to the thread, "sets <input>"), the body as markdown, then
a row of two buttons that keep their places: Answer (primary; "Answer as owner", plain, between
agents) and Close question; Answer opens the answer box under the row (open from the start
for a question with a `ui`). A question between agents is quieter: its title at 600 in the
body's font, its body in muted ink. A question with a `ui` draws its OpenUI program (its
fields and buttons) above a folded "Answer in words instead". The questions nobody is waiting
on (their askers stopped) follow under "Nobody is waiting n" with "Close all n" at its right,
which asks once ("Close these 6 questions? Closed, they leave this list and no one can answer
them." with "Close them" and "Keep them" in the shared confirmation dialog) before it closes them:
one list on the card colour, a line each (a chevron, the title at 600, "project · why" in
meta) that opens to the body, and Close at its end. The inbox then lists "Unread notes n" with
"Mark all read", a help line "A note stays here until you mark it read." (a read mark that does not go through is one quiet meta line under the help,
"A note was not marked read: sluice did not take it. Try again", never the browser's words or
a line under every note; every request a page makes says a failure in sluice's words): each
thread a card named for what it is about (its notes drawn as the conversation draws them, flat
in the card: no card in a card) ("Step k2-owner", "Notes to
you", else its first message's title or first line), with "Mark read" at its head's right. A
note is marked read only when the owner asks (its card's Mark read, Mark all read, or Mark n
notes read on its thread page): never on render, never for having been on screen, never for
opening its thread. The notes read in the last 24 hours fold under "Read today n" after the
unread ones, drawn by the server from the stored read marks, so the fold survives a reload;
with none unread the section says "No unread notes." above it. History lists the threads with
the owner by the same names (an `h2` at 600 in the body's font, a link without an underline
until hovered), each with its project, message count and "last 3h ago", and a preview cut at
a word with an ellipsis. "Unread notes n" counts notes, not threads. A thread page
(`thread?thread=<name>`) has no segmented control: a way back, "← <project> plan / <step title>"
(its id and whole title on hover; or "/ History", an empty thread too), then the thread's name
as its title, the project, thread id and message count in meta under it with "Jump to latest"
past two messages and "Mark n notes read" while some are unread, then its conversation (below) with the message box at its end: to the thread's step while
it is in the plan ("Message to <its title>"), else the orchestrator, with "Ask a question that
needs a reply" (an ask; unchecked, a note) and Send. Arriving without an anchor the page
scrolls to the newest message and the box. The tab reads "Thread · <step title>".

### Log (`/log`, `/projects/id/<p>/log`)
Presets as a segmented control (All · Steps · Runs · Messages · Errors; Errors: steps that
failed, failed calls and orphaned runs), then the filters: a Step field ("any step": its records
and its thread's messages; a unit's link from its page filters to the unit, said in a meta line
with "every unit" to drop it), a Thread field ("any thread"), the kind checkboxes folded behind a
bordered "Kinds: all" (open only when a custom set is chosen) and Apply; a meta line says that
fn calls that succeeded are left out (they are noise: a capacity fn runs every few seconds)
unless the `call` kind is chosen, and that lease records (`step.lease`, resources held and
released) are left out of All unless their kind is chosen. The tab reads "Log · <project>". Then a table of seq, time ("12m ago"), kind (12px data) and a
one-line sentence, its step, unit and thread linked to their pages and the project's name
linked on the global log ("fig-5294-rm running → succeeded", "w lane held 1 land", "Unit fig-5294
settled, 6 steps", a failure as its step page says it, "harness: Cancelled while it ran.",
"The board's document changed by cli: standup refresh"; never JSON; message bodies read as
Now reads them), with a small "JSON" toggle at the row's right end (its name "Record 12345 as
JSON") that opens the record's JSON under the sentence; records in a row that say the same are
one row with "×10"; Errors leaves out the owner's cancels; the record a link names (`#r4521`,
a failure's "Its log record") is on the secondary fill, its seq in ink at 700; each row at least 24px tall, 50 records a page with "« newest", "‹ newer" and
"older ›", in a focusable region named "Log records". A status record that changes nothing (a
restart, running → running) is left out unless `step.status` is chosen. The global log holds
every project's records, each sentence led by its project's name at 600 in muted ink, and says
so in the meta line. On a phone each record is a block (seq, time and kind on a
line, what it was under them), so the log never scrolls sideways.

### Functions (`/fns`)
The title "Functions"; "As seen by" a project select (or "no project") with Show, and with
script a "Find a function" field that keeps the functions whose name or doc holds every word
(a polite count under it); an index of the groups with their counts (Built-in, Global,
Project), each a link to its group, an empty group a plain "Global none"; then the groups, each fn its name in mono, its doc as
markdown (its `code` drawn as code) at the 72ch measure, a problem in ink when it has one, and
Inputs and Outputs as `name type` runs (one column on a phone).

### A page that cannot be drawn
A browser that follows a dead link gets a page in the layout, in the calm voice: the title
("No such step", "No such unit", "No such project" (also for an id that is no project id at
all, never "Invalid URL"), "Nothing here"), one line on what is missing ("lash has no step nope now."), one on why it may be gone (a plan edit, or the project
retiring done units n h after their last step finished), the ways back as links ("Back to the
lash plan", "Search the log for nope") and "HTTP 404" in meta.

### Project settings (`/projects/id/<p>/settings`)
The project's name as title, "Project settings", and "Each Apply commits one change right away"
with a link to the authored changes, under an index of the sections (Details · Resources ·
Board · Retiring · Activity · Delete; its landmark "Settings sections") that sticks to the top
as the page scrolls (14px muted links over a hairline, wrapping on a phone). Help lines keep the
measure. Details: Name (with its rules; links and running work keep
the project id), Description (with a Preview; how it reads folded under "Show how it reads"), Icon (a text icon up to 16 characters, or an
image up to 256 KiB; the file input's button in the page's button look). Resources: a line per
resource (static capacity or capacity fn, in use, waiting, the step queue, or "steps queued:
not known until the plan's fns have loaded") and an editor each: a choice of "A number" (a whole-number field, 0
or more, the browser refusing a negative) or "From a fn" (a select of the fns that return a
capacity), Apply capacity, and a quiet "Remove <name>" link-button; then a new resource's name
and the same choice.
Board: the program in a monospace textarea (with its rev), Save board and Clear board, and under
them a live preview folded under "Show the preview", drawn as the board column would draw it
(its width), as one types (a save's
warnings, each "Warning: line N: …", follow "Saved (rev N)" in its status); then Document (a 15px
heading): a help line, a status line ("Rev 3 · edited <time> by orch", or "Not written yet.",
and "The program has no `Doc()`, so the board does not show it." when it has none), and the
document read-only, drawn as markdown in the preview's box at the 72ch measure. Retiring: a help line, then a status line (an "On" tag and "Done units retire 6h
after their last step finished", with how many keep patterns, or a muted "Off" tag and "Done
units stay until an edit removes them", then "Last retired <time>: N steps (rev R)" or "Nothing
retired yet."), then two rows: "Retire done units after", a 9rem field (placeholder "Off") with
"hours" beside it, empty meaning off; and "Never retire", the keep patterns in a monospace
field. Activity: Pause project and Archive project switches. Delete project, in a bordered danger card:
one Delete project button opens the shared confirmation dialog, naming the project and what goes, with "Delete <name>" in ink and "Keep it". Confirmation archives first if needed, then deletes with the known name and current revision. Live work disables the button in muted ink and fill, with a reason and a link to the plan.

## Elevation & Depth

Flat by default, as supergraphics are: fields of flat colour, cards separated by hairline, not
shadow. One lift (`--lift`) on what floats over the page: the drawer below 1200px, the
switcher's and the preferences' menus and the focused skip link. The board column is flat: a
card with a hairline, no shadow.

## Shapes

The mark's generous corner, scaled down. 14px (`--radius`) for regions: unit boxes, the index's
list, question cards, the danger card; 10px (`--radius-md`) for controls, the switcher and its
menu, and code blocks; 5px (`--radius-sm`) for inline code, tags and menu items. A pill (999px)
is only for the step cards and the plan's input and output chips, the progress bar, the inbox
badge, the Types switch's track and a boolean value: a card is a token of work, not a panel. On
the graph every pill is one family: plan input and output chips are dashed cards, since they
are ends, not work. The done shelf's lines and a done unit's line are rows of a box
(`--radius`), not pills.

## Components

**The kit.** Every shared part is server-drawn from one place: `views::ui` (status, time,
count, head, tag, tabs, panel, fold, empty state, confirmation), `templates/kit.html` (the field
row) and `views::threads::Conversation` (messages); `assets/kit.js` gives them their behaviour
and `style.css`'s kit section their look, on this file's tokens. **`/_ui`** is the kit's
gallery and its documentation: every part in every state with real-looking data, the house
pair's light and dark side by side (stacked under 900px). A new part goes there first. The
ideas and CSS are adapted from knadh/oat and hunvreus/basecoat, the tabs' keyboard model from
github/tab-container-element (all MIT; `assets/README.md`): no runtime, no shadow DOM, no
build step.

- **Tabs** (`ui::tabs_open`, `panel_open`, `panel_close`, `tabs_close`): a bar of words under a
  hairline, the chosen one in ink at 650 over a 2px ink bar, a count after a word in muted
  tabular figures; 42px tall (44px on a phone). An ARIA tablist: the arrow keys move along it
  and wrap, Home and End go to its ends, each move chooses; a panel is a tab stop of its own.
  The bar sticks to the top of what scrolls (in the drawer under its close band) and scrolls
  sideways inside itself on a narrow screen, fading at its end while more tabs wait there, so the
  page never scrolls sideways. The choice is the set's `data-current` and the page's `tab` signal
  (the step's stream reads it as it connects), mirrored into `?tab=` on a step's own page
  (replaceState, no history entry); a stream patch never resets it (`data-preserve-attr`).
  Without script the bar is gone and every panel stands stacked under its own head, apart by a
  hairline (`@media (scripting: none)`); with it the head is kept for a screen reader.
- **Conversation** (`threads::Conversation`, `templates/conversation.html`): one column, as a
  transcript reads (a long technical message keeps the full measure, its code unsqueezed;
  sides would halve it). A run of messages from one sender is a group under who sent it and to
  whom, "Orchestrator → This step" in 13px: a step by its title with its id after it in data
  mono (a link to it; the drawer opens it on the board), the step the page is about "This
  step", the orchestrator "Orchestrator", the owner "You". A message is a card (the hairline,
  the 14px corner): from the step the page is about on the card colour, to it on the muted
  fill, the owner's tinted with 7% of the primary blue; its line in 12.5px meta: when
  (relative, the absolute time in its title), its id in 11.5px data mono, its state's tag, "to
  Y" when its group's head does not say it. A question leads its line with the
  `message-circle-question` glyph and "Question" at 650, a strong hairline, and "Awaiting your
  reply" in coral (only a question waiting on the owner), "Awaiting reply" in gold, else
  "Answered" or "Closed", muted; its reply sits in its card directly under it on the muted fill
  (`corner-down-right`, "<who> replied", when, its id), never a distant link. A line across the
  column at each day's start ("Thu 8 Oct 2026", muted) and at the first note to the owner not
  read yet ("New", in ink at 650). A body past 700 characters or 14 lines folds behind Show all.
  "n messages · Jump to latest" over it on a page of its own; the message box at its end (a card
  with a strong hairline: the recipient, the text, "Ask a question that needs a reply" and Send;
  it keeps what is typed through a patch). Inside a card (an inbox note) messages are flat.
- **Field row** (`kit.html`'s `field`): see the step's Outputs and Inputs.
- **Empty state** (`ui::empty`, `ui::empty_with`): one sentence in muted ink in a dashed
  region at the measure, a way on under it.
- **Card** (`.card`): the card colour, the hairline and the region's corner; never a card in a
  card.
- **Confirmation** (`ui::Confirm`): see Confirmation dialogs.

- **Step card**: a pill with the status glyph, the step id (14.5px, 600) and, in 12px meta, a
  caption: its state's word where the status ramp says a card says it ("cancelled",
  "quiet", "blocked", "stopping", "finishing", "outside", "held", "queued") or `done/total`,
  each with a title from the same table that says what it means ("Waits on a step that
  failed, was cancelled or went stale"), so the board needs no legend. A failed card's caption
  is its failure's kind in a word, so "retry it" reads apart from "read it" before it is
  opened: "cap" (a wall-clock cap), "stalled", "quota", "auth", "engine" (its engine would not
  start, take its input or keep its session), "lost" (its process), "no submit", "invalid",
  "rejected", "transient", else "failed" for the work's own failure; its title, and a
  cancel's, is the failure's one sentence. The pill stays the ink pill whatever the kind. After the caption, a
  timer in the board's two-unit durations ("45s", "12m", "2h 14m", "1d 3h"), tabular figures:
  a running step's says how long its current run (the latest, a retry's own) has gone, in ink,
  ticking, and holds the width of "2h 14m" with its figures at the pill's end so a tick never
  moves the card or its edges; a succeeded or failed step's says how long its last run took,
  in muted ink and still. A pending, blocked, queued, paused, stale or skipped step, and a value
  set by hand, has none. A running or failed step on its second run or later says so after
  the timer, in muted 12px tabular figures: a 12px mark for how each of its last three earlier
  runs ended (oldest first, "+2" before them for more) and "run 3"; its title "Run 3, after 2
  failed", a screen reader hears ", after 2 failed" after it; a quiet run keeps it (the quiet
  time replaces the timer, never the count), and the matrix pill and the drawer's badges say
  it too, the phone's lane string as "land✗ (run 3)". Under a stopped card (failed or
  cancelled) its one sentence in meta (13px, muted, at most 40ch), a link to the step. The
  timer's title counts the runs ("3 runs; this one took 2h 14m"); a screen
  reader hears "running fig-5240-work for 2 hours 14 minutes" or "… took 12 minutes" (no
comma: the card's parts are flex items, which the name already parts with a space). On a
  phone the card keeps one line: the id gives way before the timer. Running cards take a blue border,
  failed a solid ink pill, quiet a gold border, stale a gold one, paused plum; blocked and
  stopping a dashed border; a step next in
  line (ready) keeps a strong hairline. Every card's boundary (`--card-edge`, a pending card's
  too) holds 1.5:1 or more against the page and a unit box, in both themes. A solo card's title
  above it wraps balanced (`text-wrap: balance`). Inline `core.*` steps are chips: dashed and muted. Its
  accessible description carries the doc, what it waits on and why it stopped, each a
  sentence (never the stored error's JSON). The card in the drawer
  wears the blue ring 2px outside its border; the keyboard's focus is a 2px ink ring on the
  pill itself, no gap, so a card the drawer hands the focus back to never reads as running or
  open. "Next" (a strong hairline) is only ever a pending step whose gates are met.
- **Shared components** (`views::ui`): every page draws a status (`status`: glyph and word),
  a time (`ago`, `since`, `at`), a duration (`duration`: the short form, its words for a
  screen reader), a count (`count`, `tally`: "6 steps · 1 running", a zero part left out), a
  section head (`head`), a facts row (`field`), a step's name (`StepRef`), a tag (`tag`,
  `tag_link`), text cut at a word (`cut`) and a type in words (`type_words`: a JSON schema as
  its type) through one function each, and every state from one table (the status ramp:
  `glyph`, `mark` for a glyph beside its own word, `status`, `states` and `bar` over a
  `Tally`), so a change to one reads everywhere. A template never
  hand-builds these.
- **Timeline** (`views::timeline`, `templates/timeline.html`): a unit's runs on one axis, no
  legend. A grid: a 76px column of stage names (13px, muted; the current step's in ink at 650,
  `aria-current`), then the track, 8px in from the column's edges so a glyph centred on a run's
  end stays inside. Over the rows the axis in 12px muted tabular figures ("start", "waited 6h
  12m" centred on its break, "+7h 20m" or "now" at the end, never nearer than 72px). Rows part by
  a hairline; each a 24px track and its words under it in 13px meta ("3 runs · 52m · waited 6h
  12m", the running time a ticking `<time data-since>`). A bar is 10px tall with the 3px corner
  (never a pill): succeeded the success green at 62%, failed solid ink, cancelled muted ink at
  45%, running the accent, a run with no recorded result the idle grey; each ends in its status
  glyph on the canvas, a run too short to draw is the glyph alone. A run still going runs to
  "now", its last 48px dashed (a mask, 4px on 3px off) under its turning glyph: its end open. A
  long wait is a 48px break across the rows: the muted fill between two dashed strong hairlines.
  Under 490px of its own width (a container), each row is one 44px line of words: the outcome
  glyph and stage in ink at 600, "·", then its words. The spinner is the only motion, and it
  stops under reduced motion.
- **Usual time on a running card**: a card whose stage usually takes a known time draws, 3px
  above its foot and 12px in (8px on a matrix pill), a 2px line in running's blue at 50% as long
  as the run is along that time (full once past it), drawn by `nav.js` from the timer's
  `data-usually`; never gold, which is quiet's. Its timer's title adds "; its stage usually
  takes 24m".
- **Tag** (`.tag`): a small fact set apart: 12px text at 500, a strong hairline and the 5px
  corner; gold (`attn`) for "quiet" and "n awaiting reply", muted for a closed or answered
  state.
- **Icons**: every icon is Lucide (lucide-static 1.52.0, ISC), the published SVG unmodified in
  `crates/sluice-web/assets/icons/`, each a `<symbol>` in the one sprite a page carries
  (`views::icons::sprite`), drawn by `views::icons::icon` as a `<use>` of it (the solid
  success and failure glyphs are symbols of their own, since a rule cannot reach into a
  `<use>`): Lucide's 24-unit grid
  and 2-unit round stroke, in `currentColor` so it takes its control's ink, hover and focus, and
  `aria-hidden` (the control carries the name). 20px in the nav (the inbox tray, project
  settings, display preferences) and for the drawer's close (`x`), 16px for status glyphs, chevrons, the theme tick, the board's
  head and switch and its error boxes, the splitter's grip and the search field's magnifier and
  clear. A new icon is fetched from Lucide at that version, never
  drawn; the mark and favicon are the owner's, and a project's own icon is the user's.
- **Tracing**: hovering or focusing a card lights its lines to and from it, in ink, with their
  words; the other cards lose their border and fill and their text turns muted ink, the other
  lines fade. A name in a card's waits traces its source; a card folded away in a done
  unit is stood in for by that unit's line, ringed in ink, and a unit on the closed done shelf by
  the shelf's line, ringed the same way. Tracing follows the
  keyboard's focus, never a focus given back after a click or by the drawer's close, so when
  the pointer leaves with nothing focused from the keyboard, nothing stays lit.
- **Buttons**: primary is the deep blue under cream; others are card-coloured with an input
  hairline. The progress bar is an 8px pill, 200px wide (120 on a phone); on a project page
  it grows with its summary line, up to 480px. It draws a segment only for a count above
  zero, so an empty plan's bar is its bare track and no zero leaves a sliver.
- **Splitter**: see The project's board; the one control that resizes, a grip on a hairline
  that lights only when used.
- **Live updates**: each page's stream patches only the regions that changed (a unit's box, a
  matrix row, the summary line; the plan's lines only when its shape changes). While it
  reconnects a gold line says "Updates paused. Reconnecting…" with a Reconnect button; once
  Datastar gives up it says "Updates stopped at 14:02." with the same button, and the stream opens
  again on its own when the tab shows, the window is focused or the machine is back online. A
  page drawn by an older build says "sluice was updated · Reload" in muted ink, news and not
  trouble. A step or unit that left the plan says so where it was drawn, in muted ink, with a link
  to its log. A patch never resets what the owner holds: a confirmation stays the server's
  `<details>` (its summary opens the dialog), the plan's More menu stays open, a board field's
  refusal stays until the field is edited, and a settings field another author changed says
  "Changed by <author> since you opened this · Reload this field" in the attention colour. Times have one vocabulary
  on every page, and `nav.js` (on every page) owns them: the server writes "2026-10-07 20:47
  UTC" (its title keeps it), the script reads `data-since` as a two-unit duration ("45s",
  "12m", "2h 14m", "3d 12h") and `data-ago` as "just now", "12m ago", "3d 12h ago", and reads
  a time again at once when a stream patch writes the server's text back; a running step's
  quiet tag appears after 15 minutes without a write.

### Touch
At 720px and below every control is at least 44px tall: the board tools (the search field and
its clear too), the description's More, the segmented
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

### Confirmation dialogs

Cancel, Delete project and Questions' Close all use one native `<dialog>` (Cancel and Close all are drawn by `ui::Confirm`; `kit.js` opens it). It is centred, at most 480px wide with 16px beside it on a phone, on the card colour with the region corner, strong hairline, lift and scrim. Its title is Public Sans 18/24 at 650, its copy the body voice. Button rows wrap and each target is at least 44px high. Focus starts on the keep button, stays in the dialog, Escape closes it, and closing returns focus to its opener. Without script, a details fold shows the same confirmation form inline. Cancel offers a reason field; Delete uses an ink-filled danger button, leaving coral to questions. Running card borders hold at least 3:1 in every theme; wait-lines hold at least 3.4:1 on a unit box.
