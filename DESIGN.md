---
name: sluice dashboard
description: Synthesis. The National Park Service Unigrid in the logo's palette: a slim navy nav bar over cream paper, the page's name at a reading size and its sentence under it, a twelve-column module grid, what identifies an item behind its ⋯, the channel's blue for what runs, coral only for a question to the owner.
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
  band-mark: "oklch(0.617 0.2 257)"
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
    fontSize: "clamp(88px, 10.9cqi, 240px)"
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
  gutter: "clamp(16px, calc(10px + 1.5vw), 64px)"
  col-gap: "clamp(8px, calc(4px + 0.8vw), 28px)"
grid:
  frame: "fluid"
  columns: 12
  columns-narrow: 6
  columns-wide: 24
  sheet-narrow-below: "640px"
  sheet-regular-from: "1200px"
  sheet-wide-from: "2000px"
  phone-below: "760px"
components:
  band-nav:
    backgroundColor: "{colors.band}"
    textColor: "{colors.band-ink}"
    height: "52px"
  page-head:
    textColor: "{colors.ink}"
    typography: "30px/34px 850"
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
Synthesis keeps the grid and the type and sets them in the logo's palette, with the band cut to
a slim navy nav bar so the work starts near the top of the screen: the paper is the logo's
cream, the channel's blue is the one active colour, sky is done, sand needs a look, and coral
is spent on one thing only, an open question to the owner.

The owner opens the dashboard between other work, often on a second screen or a phone, to
answer one question within seconds: does anything need me, and is the work moving? The page
answers in its order. The page's head says it in one sentence; under it the paper lists what needs the
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
- **"Finished last"** is the units (or steps of no unit) whose run ended most recently, by
  title, with when and how long they took.
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

Roles, not hues. Each is a token (`assets/style.css`), `light-dark()` of its light and dark
value, so a theme's scheme (its id's `-light` or `-dark`) picks the side, and with no theme
chosen the system's. These are Sluice's (Sluice Light and Sluice Dark); every other theme
(below, Themes) maps the same roles.

- **Paper** (`paper`, cream): the page. `paper-2` is a sunk well and a hover; `paper-raised` a
  menu or the drawer.
- **Ink** (`ink`) for text, `ink-muted` for meta (4.5:1 on paper and on the wells).
- **Navy** (`navy`): the heavy rules (a band section's 3px head rule, a stopped module's 2px),
  the filled button, the selected ring. In the dark it turns cream, so a rule stays the
  heaviest thing on the paper.
- **The nav bar** (`band`, `band-ink`, `band-muted`, `band-rule`, `band-accent`): the logo's navy
  in the light, a deeper navy than the paper in the dark, cream text, sky for what the band
  links to and how long things took.
- **The band's mark** (`band-mark`): the logo's blue on the band, the current place's underline
  and the live square.
- **Run** (`run`): the channel's blue, the one active colour: a running cell's fill (cream on
  it), a running mark (edged in `run-ink`). `run-ink` is the blue as text, `focus` the focus
  ring.
- **Sky** (`sky`): done. A done cell's fill and a done mark's (edged in `sky-ink`); `sky-ink`
  its glyph; `sweep` the bright sky of the sweep along a running cell's foot.
- **Sand** (`sand`, `sand-pale`, `sand-ink`, `sand-mark`): needs a look: failed, cancelled,
  stale, quiet, an overrun. A look cell and the overrun chip are sand with navy on them; a
  stopped module sits on `sand-pale`.
- **Coral** (`coral`, `on-coral`): only an open question to the owner (the Inbox count, the
  question module's rule, "Awaiting your reply", the summary's link to it) and the logo. A
  failure is never coral.

**The One Coral Rule.** Coral marks a question waiting on the owner, nothing else.
**The One Blue Rule.** Blue fills only what is running now.

Sluice Dark is navy paper (`paper-dark`), never black; the nav bar goes deeper than the paper
so it still reads as the heaviest thing on the page. Text holds 4.5:1 in both, large text
3:1.

The derived tokens (`rule`, `rule-faint`, `line` from `navy`; `band-muted`, `band-rule`,
`band-hover` from `band-ink`; `lift`, `scrim` from `band`) are worked out in one block, again
on any element that names a theme, so a part of the gallery in another theme draws itself
whole. Page CSS uses the tokens only, never a literal colour.

## Themes

There is no light or dark setting: there is one list of themes, and a theme is a whole look in
one scheme. Sluice Light and Sluice Dark are the brand; beside them the display preferences
offer twelve more made from well-loved colour schemes, each adapted to sluice's roles: Solarized
Light and Dark, Nord Light and Nord, Gruvbox Light and Dark, Catppuccin Latte and Mocha, Rosé
Pine Dawn and Rosé Pine, Flexoki Light and Dark. A theme's id is `<family>-light` or
`<family>-dark` (`views::THEMES`): `data-theme` on `<html>` and the `sluice_theme` cookie,
server-drawn, so a page without script is drawn in it; with script the picker applies at once
(`sluice-toggle`). The picker shows each theme's swatch in its own tokens: its band over its
paper, a square of its running colour and one of its question colour.

**The first open.** With no cookie the page names no theme and the stylesheet draws Sluice
Light or Sluice Dark by the system's scheme. With script, that first page keeps what it drew as
the choice (`nav.js` posts it to `/settings` once); from then on only a pick in the picker
changes the theme, whatever the system's scheme does. Without script nothing is kept, and the
system's scheme decides until a theme is picked.

**Mapping by role, not hue.** A family's block (`[data-theme^="nord-"]`) maps every role as
`light-dark()` of its light and dark theme, and the theme's suffix sets its `color-scheme`. The
band is the theme's deepest surface (for a light pair it
borrows the scheme's darkest base, as Unigrid's band is black); paper and paper-2 its base and
its next surface; ink its text, ink-muted its comment or subtle text; navy, the heavy rules and
the filled button, its strongest text colour; run its blue (or its nearest); sky a pale wash
of its cyan or green, its glyph the full hue; sand its yellow; and the question its one warm or
vivid colour no other role uses. Status never rests on colour alone in any theme: every state
keeps its glyph and its word.

**Held by a test** (`tests/themes.rs`), which reads each of the fourteen themes' tokens from
the stylesheet, in its own scheme:
text at 4.5:1 (ink and ink-muted on paper, paper-2, paper-raised, sand-pale and sky; run-ink
and sand-ink on paper and paper-2; band-ink, band-muted and band-accent on the band; the text on
running, sand, coral and navy fills), 3:1 for the focus ring on paper and paper-2 and for status
marks (sky-ink on paper and sky, sand-ink on sand-pale, sand-mark on sand, idle on paper,
band-mark and the coral count on the band), the question colour at least 0.1 apart in Oklab
from every other role's, running apart from done, and the band no lighter than the paper.

| Theme | Source and licence | Light | Dark |
|---|---|---|---|
| **Sluice** | the logo's own palette | cream paper, the logo's navy band | navy paper, a deeper navy band |
| **Solarized** | Ethan Schoonover, [ethanschoonover.com/solarized](https://ethanschoonover.com/solarized), MIT | base3 paper, base03 band | base03 paper, a deeper base03 band |
| **Nord** | Arctic Ice Studio, [nordtheme.com](https://www.nordtheme.com), MIT | Snow Storm paper (nord6), Polar Night band (nord0) | Polar Night paper (nord0), a deeper Polar Night band |
| **Gruvbox** | Pavel Pertsev, [github.com/morhetz/gruvbox](https://github.com/morhetz/gruvbox), MIT/X11 | light0 paper, dark0_hard band | dark0 paper, dark0_hard band |
| **Catppuccin** | Catppuccin, [catppuccin.com](https://catppuccin.com), MIT | Latte base, Mocha base band | Mocha base, Mocha crust band |
| **Rosé Pine** | Rosé Pine, [rosepinetheme.com](https://rosepinetheme.com), MIT | Dawn base, Main base band | Main base, a deeper Main band |
| **Flexoki** | Steph Ango, [stephango.com/flexoki](https://stephango.com/flexoki), MIT | paper, black band | base-950 paper, black band |

The roles in each (light / dark; one value when both are the same):

| Role | Solarized | Nord | Gruvbox | Catppuccin | Rosé Pine | Flexoki |
|---|---|---|---|---|---|---|
| paper | base3 / base03 | nord6 / nord0 | light0 / dark0 | Latte base / Mocha base | Dawn base / Main base | paper / base-950 |
| paper-2 | base2 / base02 | nord5 / nord1 | light0_soft / dark0_soft | mantle / mantle | overlay / surface | base-50 / base-900 |
| ink | base02 / base2 | nord0 / nord6 | dark1 / light1 | text / text | text / text | black / base-200 |
| ink-muted | base01* / base1 | nord3 / nord4–3 mix* | dark3 / light3 | subtext1 / subtext0 | subtle* / subtle* | base-600* / base-400 |
| navy (rules, filled button) | base03 / base1 | nord1 / nord4 | dark0 / light2 | text / subtext1 | text / text | black / base-200 |
| band | base03 / deeper base03* | nord0 / deeper nord0* | dark0_hard | Mocha base / crust | Main base / deeper base* | black |
| band-accent, band-mark | cyan, blue | nord8, nord8 | bright_aqua, bright_yellow | sky, blue | foam, rose | cyan-400, blue-400 |
| run (on-run) | blue* (base3 / base03) | nord10* / nord9 (nord6 / nord0) | faded_blue / bright_blue | blue* / blue (base / crust) | pine / Moon pine | blue-600 / blue-400 |
| sky, sky-ink | cyan wash, cyan* | nord8 wash, nord7* | aqua wash, faded / bright aqua | sky wash, sapphire* / sky | foam wash, foam* | cyan-50 / cyan-900*, cyan-600 / cyan-400 |
| sand (on-sand) | yellow (base03) | nord13 (nord0) | neutral / bright yellow (dark0_hard) | yellow (crust) | gold (base) | yellow-400 (black) |
| sand-ink | yellow* | nord13* / nord13 | faded_yellow* / bright_yellow | yellow* | gold* | yellow-700* / yellow-400 |
| question (coral, on-coral) | magenta* (base03) | nord12* (nord0) | bright_purple (dark0_hard) | Latte pink / Mocha red (crust) | love* (base) | orange-600 / orange-400 |

\* Adjusted from the published value for contrast, or a surface the scheme lacks designed in its
spirit. Solarized: ink-muted base01 darkened to #4f646b (4.39:1 on base2 otherwise); running
blue darkened to #1f6fa8 for base3 text (#268bd2 gives 3.6:1), its text #1c6aa3 and #4fa6e0;
sky-ink #1d7a73; sand-ink #7d5f00 and #c99a12 (yellow #b58900 is 3.0:1 as text on base3);
magenta lifted to #e2639f with base03 on it (base3 on #d33682 is 4.2:1); the dark band
#00212b and the dark sky #083a45 designed deeper than base03. Nord: the light running fill
#4c6f99 and text #4a6b94 (nord10 #5e81ac is 3.9:1 under nord6); muted in the dark #c0c8d6
between nord4 and nord3; sky-ink #3f7a87; sand-ink #7d5f1a; the question nord12 lifted to
#d6907a (nord0 on #d08770 is 4.4:1); the dark band #242933. Gruvbox: sand-ink #8f5a0f (faded
yellow #b57614 is 3.3:1); the question is bright_purple #d3869b in both, since its orange sits
too near its yellow. Catppuccin: Latte blue darkened to #1a5ce0 (base on #1e66f5 is 4.3:1),
text #1e5fe0; sky-ink #13788c; sand-ink #8f5a0c; the dark question is Mocha red #f38ba8, as
Mocha's pink sits too near its text. Rosé Pine: Dawn's subtle darkened to #625d80 and Main's
lifted to #a29eba (4.2:1 on sky and sand-pale otherwise); sky-ink #3d7a85; sand-ink #8f5d17;
Dawn's love lifted to #c26d85 with base on it (4.2:1); the dark band #12101b. Flexoki:
base-600 darkened to #64635e (4.47:1 on base-50); yellow-700 to #7a5c01 as text on base-50;
the dark sky cyan-900 #122F2C (cyan-850 gives the muted text 4.3:1); the dark running text
blue-300 #66A0C8 (blue-400 is 4.4:1 on base-950); the light paper-2 base-50, the dark paper
base-950 so the black band stays the deepest. The washes (sky, sand-pale) are the theme's hue
mixed into its paper. Every value is in `assets/style.css`, one block a theme.

## Type

One family: **Schibsted Grotesk** (variable, 400 to 900) for display and text, with
**JetBrains Mono** for ids, durations, clock times and data only. Both load from
cdn.jsdelivr.net (`@fontsource-variable/schibsted-grotesk@5.3.0`,
`@fontsource-variable/jetbrains-mono@5.3.0`), the only third-party requests a page makes.

- The page's name (`ui::page_head`): 30px/34px, 850, -0.025em (26px on a phone), balanced
  and wrapping anywhere rather than running off; a step's name 28px/34px (23px on a phone,
  24px in the drawer, three lines at most there). The summary sentence under it: 17px/24px, 500
  (15px on a phone), one line where it fits. A page's note in its place: 14px in `ink-muted`.
- A band section's head: 30px, 850, -0.03em. A question's title in its module: 23px, 800. A
  module's title: 16px, 750, two lines at most.
- Body 15/22; meta 13/18 in `ink-muted`; a running cell's time 20px 800.
- Data: JetBrains Mono 12.5px.
- Tabular figures only on numeric data (`time`, durations, counts, `.num`), never page-wide:
  Schibsted's tabular figures widen its punctuation.
- Long tokens (ids, paths) wrap (`overflow-wrap: anywhere`), so a page never scrolls sideways.
- No uppercase labels, no kickers above a heading.

## The grid

**Fluid at every width, 320px to 3840px.** There is no frame cap: the column is the window less
a gutter each side (less the step drawer when it stands beside the page), and the nav bar, the
page's head, its row and the page share its edges. The gutter grows with the window,
`clamp(16px, 10px + 1.5vw, 64px)` (16px on a phone, 32 at 1440, 48 at 2560, 64 at 3840), and
so does the gap between columns, `clamp(8px, 4px + 0.8vw, 28px)`. A wide screen gets more
modules a row, never longer lines: prose keeps `--measure` (68ch) at every width.

**Lay out by the box, not the window.** `main` is the `frame` container and every sheet the
`sheet` container, so what sits in them follows the room it has, the drawer and a board beside
the plan counted. Media queries are for the frame outside `main` (the nav bar, the head and the
row), and
for input (pointer, hover, motion, colour scheme).

**The sheet** (`ui::grid_open`, a plain `div.sheet`) has four modes, by its own width:

| sheet | columns | a module of span N (twelfths) |
|---|---|---|
| under 640px (narrow) | 6 | across the whole sheet |
| 640 to 1199px (medium) | 12 | half (6) when N is 6 or less, else whole |
| 1200 to 1999px (regular) | 12 | N |
| 2000px and over (wide) | 24 | 2N, its fraction kept; N when it halves |

- Underneath, the sheet is always twice its columns in tracks (24 for twelve), and the lines
  of 12 or 6 columns are a subset of them, so nothing jumps between modes.
- **A module's wide behaviour.** `ui::module_open(span, swell, label)` and
  `ui::column_open(span)` keep their fraction on a wide sheet. `ui::module_open_wide(span,
  swell, label, ui::Wide::Halve)` (or `ui::column_open_wide`, or `data-wide="halve"` beside
  `style="--span:N"`) halves it instead, so two whole-width modules stand side by side from
  2000px. A child with no span takes the whole sheet.
- Nothing is drawn over the sheet: the grid is the layout, not a picture of it.
- **Strip tables.** A plan's stages sit on fixed columns: a band's strip head
  (`ui::strip_head(…, stages)`) names each stage over its column, and each row under it (a
  plan's unit, or `ui::strip_row_open(n)`: its lead, then its `ui::stage_strip`) shares one
  track list: the lead takes what the cells leave, the cells `--cell` wide each
  (`clamp(68px, 6cqi, 104px)`) close beside it, then the row's "⋯". So each cell stands under
  its name at every width. Where the sheet is under 640px the lead stands over the cells, the
  head's names give way and each row's cells share its width.
- The trace rail sits in the gutter and narrows with it (28px from 1200px, the gutter less a
  pixel below).
- **Narrow end.** At 320 and 390 nothing clips and the page never scrolls sideways; every
  control is 44px tall on a phone.

## The frame

The page's frame (`templates/layout.html`, `render_framed`) is a slim nav bar and the page's
head on the paper under it.

- **The nav bar** (`nav#top-nav`, 52px; 48px on a phone): the mark and "sluice", "Projects",
  the project switcher (its button the current project's icon and name in a box of the band's
  rule, the menu every project), Inbox with its coral count, then at the right the live line (a
  square in `band-mark` and "Live" while the page's stream is, from `html[data-stream]`) and
  display preferences (the theme, value types). The current place carries a 3px underline in
  `band-mark`. On a phone the wordmark and Projects give way to the switcher (whose menu has
  them), its name shortens before anything wraps, and the live line is its square.
- **The page's head** (`Frame::head`, `ui::page_head` and `ui::page_head_with`): on the paper,
  its crumbs when it is one thing in a tab (a step's way back to its plan and its unit), its name
  at 30px with its Details' "⋯" beside it, and under it on a plan and on home only the summary
  sentence (`ui::summary_sentence`), one line of large body text where it fits; a page that says
  what it shows has a muted note in its place (`ui::page_head_note`: the Log's records, the
  Day's date and clock, the Functions' counts). No page carries a description blurb.
- **The row** (`.subnav`): its sections as tabs (Plan, Day, Messages, Log, Functions, Settings;
  the current one under a 2px ink bar), a count line (`Frame::meta`), and at the right its tools
  (`Frame::tools`: a find field and the view switch on a plan, the messages' switch on For you,
  Questions and History). A hairline closes it. On a phone the tabs wrap and the tools take a
  line of their own.
- **One thing inside a tab** (a step, a unit, a thread: `Frame::inner`): the row comes first
  and the head under it, so the row stays where the plan has it.

The budget: at 1440px the plan's first section starts within 200px of the top (a test holds
it within 220), about 260px on a phone.

**The view switch** (Plan · Both · Board on a project with a board, Plan · Board below 1280px)
is a segmented bar of links in the row: the current view filled navy with cream
(`aria-current`), the others muted words. A link asks the server for its view (`?view=`, kept
in the project's `sluice_view_<id>` cookie), so it works without script; with script
`board.js` shows the view at once and keeps it per project. The messages' switch (For you,
Questions, History) and the log's presets are the same bar.

## Details

What identifies an item rather than explains it (its step, unit and run ids, its run number
and earlier runs, its fn, engine and model, its tags, its recipe, its params, a message's id, a
hash or a path) is kept behind one "⋯" at the item's end: `ui::Details`, drawn by its `menu`.
It is a `<details>` (so it opens without script) inside a `sluice-menu` (Escape and a click
elsewhere close it and give the focus back to its button, which says `aria-expanded`), 44px on
a phone, where it opens as a sheet at the bottom of the window. Its panel lists each value with
its name; an id is whole in data mono with a copy button. Every row, module, head and message
has one; ids stay addressable (the find takes them, URLs carry them, and a step's Inputs,
Outputs and Runs tabs stay whole).

**A value by its shape** (`ui::ValueSet`), never its name: a number, a boolean or a short word
shows; a hash, an id, a path, a URL or a long token goes to the Details; prose shows clamped to
two lines, whole in the Details. A message line on a row names its sender in words (the
sending step's stage, "orchestrator" or "you") and keeps to one line.

## Modules

A module is a unit, a question or a step on the grid: an `article` spanning whole columns,
square, with a rule over it. Its parts: `.mod-k` (its kind's line: a glyph, a word, who and
when), `.mod-t` (its title), `.mod-meta` (its facts; what identifies it in its Details), a stage strip,
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
  title (linking its page) with its Details, its words, its running time on the blue with the
  sweep, then the progress fields that read at a glance, when it last reported. A value is set
  by its shape alone (`ui::ValueSet`): the first short one at display size, prose clamped to
  two lines; an id, a hash, a path or a list only in its Details.

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
the sand chip with the timer, "2.1× usual". Every overrun on every page is said by one rule,
`ui::ratio_text`: one decimal below ten ("5×" when whole), a whole number from ten, rounded
down.

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
cancelled. 2 article units and 1 scan unit at work: 1 quiet for 53m, 1 at 2.1× its usual
time. 2 waiting. 3 of 10 units done; the last finished 57m ago." It names no unit: quiet runs
and overruns are counted, the furthest said ("8 past their usual time, the furthest at 9.8×"),
and the rows under it name them. Each recipe is named by its
own name with the unit after it ("1 unit without a recipe" for one of no recipe, as the
plan's head over them says it too), each part is left out
when it has nothing to say, and the question links to where it is answered, under a coral
underline. A quiet run's time ticks on the page.

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

- **`sluice-trace`**: select to trace (above).
- **`sluice-tabs`**: an ARIA tablist over its panels (arrows wrap, Home and End); the choice in
  `$$tab`, its `current`, and with `url` in `?tab=`; without script every panel stands stacked.
- **`sluice-fold`**: Show all and Show less over a long text, said only when it is cut; or a
  More fold kept open per project or opened on a wide screen.
- **`sluice-confirm`**: the confirmation dialog (below).
- **`sluice-conversation`**, **`sluice-composer`**, **`sluice-answer`**: a conversation (Jump
  to latest, Mark read), the message box (sends as JSON, keeps its text through a patch) and a
  question's Answer and Close.
- **`sluice-menu`**: the project switcher, display preferences, the plan's More and every
  Details "⋯": a click elsewhere or Escape closes it and gives the focus back, its button says
  `aria-expanded`; the arrows, Home and End move through it.
- **`sluice-copy`**: an id or a SHA in data mono with a copy button.
- **`sluice-toggle`**: a display setting (the theme, value types) applied at once and kept by
  `/settings`.
- **`sluice-search`**: the board's search, the functions' find, a form of filters.
- **`sluice-banner`**: the stream line ("Updates paused. Reconnecting…", "Updates stopped at
  14:02." with Reconnect) and the build line ("sluice was updated · Reload"); the stream's
  phase is also `html[data-stream]`, which the nav bar's live line reads.
- **`sluice-keys`**: `/` finds, `[` and `]` move the drawer, `g` then a letter goes to a page,
  `?` lists them; never while typing or under a dialog.
- **`sluice-splitter`**, **`sluice-drawer`**: the board's splitter and the step drawer.

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

- **A project's plan** (`/projects/id/<p>`, `views::plan`): the head with the project's name,
  its Details (its id, its description, its settings) and its summary sentence; the row with
  its sections, the count line ("103 units · 475 steps"), Find, the Plan · Both · Board switch
  when the project has a board and a quiet menu; the trace line (said to a screen reader, seen
  only while tracing); then the sheet. Each live unit is drawn once, in the first band that
  holds it:
  - **For you**: each open question to the owner a swelled module (the coral rule, the
    question as its title, who asked in words and when, the unit's title and how long it has
    run, its stage strip the first time a unit asks, the question's body, Answer primary, Open
    step, Message, Close question, its Details). Beside it, **Stopped**: each failed, cancelled
    or stale unit a module on the sand (its glyph and word, its failure's kind in a word, its
    stage marks, the failure's sentence and what to try next, Retry with feedback first when a
    bare Retry would fail again, Retry, Open step, Unit page, its Details). Failures before
    cancels, in plan order.
  - **Running**, quiet first then the longest running, and **Waiting**, in plan order, as rows:
    a block a recipe (its name, linking to every unit it made, its count by state, its stages
    as the columns over its rows), then the units of no recipe and the loose steps. A row (72 to
    88px at 1440) is the unit's trace button: its glyph and word, how long it has run against
    its usual time ("1h 51m, usually 21m", how long silent when quiet) and the overrun chip past
    twice it, then its title, two lines at most; under it one line, what holds a waiting unit
    (each source by title, linked, and what it is doing) or the recipe's `view` (else its last
    message, its sender in words); its cells close beside it, sized to them, on the recipe's
    columns (a unit of no recipe its own small graph, a loose step one cell); its Details at
    the end. Selecting a row opens its steps, last message and actions in place.
  - **The margin**: a long run alone (no recipe, reporting progress, or past four times the
    longest usual time) as the sky module in the sheet's last two columns, down beside the
    bands; its title opens the step in the drawer.
  - **Done**: its count and when the last finished; what finished last, the latest four by
    title with their clock time and how long they took (`ui::latest_list`); then the index
    folded behind "Show the n done units" (open for Show: Done, a find or a recipe's every
    unit), a dense list newest first in columns: finish time, glyph, title, how long it took.
  The sheet's modes: on a medium sheet (under 1200px, a board or the drawer beside it) a row's
  cells narrow to 68px each, the modules sit two to a band's row and the long
  runs go side by side under Running; under 640px everything stacks; from 2000px the bands of
  modules set twice as many to a row and Running and Waiting stand side by side. Every band
  is a patch region and every unit one inside it, so a change patches the bands a unit moved
  between and nothing else; a step whose state moves on is said once to a screen reader. The
  step drawer opens beside the plan from any cell, Open or margin title, on paper under the
  heavy navy rule a band's head wears, a hairline at its edge, lifted off the plan.
- **A step's page** (`/projects/id/<p>/steps/<s>`): under the row, its head (the gallery's
  Step head, `StepView::band`): its way back to its plan and its unit, its stage muted before
  its title at 28px with its Details (its id, unit, recipe, fn, run and tags), its state and how
  long it has run against its usual time, its words, then its unit's stage strip (its own
  stage ringed) and its actions, the next move filled. Its tabs follow. Overview is modules on the sheet: the question
  to the owner swells first on the coral rule, answered in place; then why it failed on the
  sand, or Now (its live turn: last words, its calls by tool as tiles, named exactly as the
  engine's transcript names them, and its latest calls); beside them, as titled modules, what
  it waits on (or when it starts), what it comes after, its progress and its key output. The
  columns stand one over the other on a narrow sheet (the drawer, a phone, to 900px of sheet),
  at seven and five twelfths to 2000px, as halves from there with the side's modules two to a
  row, and at a third and two thirds from 3000px: more modules a row, never longer lines.
  Activity is its turns as rows numbered in data mono, the running one on the run rule;
  Messages the conversation; Inputs and Outputs field tables with where each came from; Runs
  its unit's timeline over its runs. In the drawer the same head tops the step a size down,
  three lines at most.
- **A unit's page** (`/projects/id/<p>/units/<u>`): under the row, its head: its way back,
  its glyph and title with its Details (its id, recipe and params), how its steps stand, its
  stage strip. On the sheet its steps are modules (two a row from 640px of sheet, three from
  1200, four from 2000, six from 3000), each headed by its stage name with its Details, its own
  title only when it differs from the unit's (which the head already says), the one asking the
  owner on the
  coral rule, a stop on the sand, a running one
  on the run rule; then its timeline and its last message.
- **The inbox, Questions and a thread**: the head names the page (a thread its step's title,
  its way back over it and its Details); the messages' switch is in the row. Questions to the
  owner
  come first as swelled modules on the coral rule, answered in place (Answer and Close 44px; on
  a phone a whole-width card), one a row to 900px of sheet, two to 2000, three from there and
  four from 3000; then those between agents, the ones nobody waits on and the unread notes.
  A conversation's messages are rows apart by hairlines, each with its Details (its id, its
  thread, what it replies to), an open question to the owner on the coral rule with its reply
  under it, the message box under the navy rule.
- **Home** (`/`, `views::home`): the projects as a cover and an index. The head: "All
  projects" and one sentence across them (the questions first, in coral, then what runs and
  where, then what stopped where), the runner stopped on the sand when it is; what each project
  finished last is on its own module. On the paper, For you (eight
  columns: each open question to the owner swollen under the coral rule, its title at 23px,
  its words, Answer and Close in place, the way to its step; an answered one the line that
  says so) beside Today (four, on the sky: a project's last day in a line, the way to the
  timetable); with no question For you is one line and Today a row across. Then Projects: a
  module a project, as many to a row as fit 460px each, what needs the owner first (a
  question: the coral swell; something stopped: the sand swell): its name at 46px, its
  state's glyph and size, its words, its summary sentence, a 9px square a unit (coral asks,
  sand needs a look, blue runs, an outline waits, sky done), its rows (questions, stopped
  steps, running steps by title, "running for 1h 10m" against "usually 30m" or the overrun
  chip), what it finished last by title and the way to its plan and its day. Then the Index, a
  table of every project's units by band, archived ones muted, its counts 10ch each so on a
  wide screen they stand together at the right.
- **Day** (`/day`, `/projects/id/<p>/day`, `views::day`): the day as a timetable. The head:
  "Day", its note the date and the reader's clock. The day line under the day's sentence: a
  row a project, its name in the first 132px, its runs as 5px
  bars on 7px tracks (sky done, blue running, sand-ink needing a look), the hours along the top
  in data mono, the rule at now in the run blue; quick successes are counted, not drawn. The
  timetable: a 64px hour column (the hour 22px, its day where it changes, its runs counted),
  then a column a project; each run a line: its minute in mono, its unit's title in ink and its
  stage muted (its step and run in the link's title), then at the right its glyph and duration
  (sky-ink check), its word on
  the sand chip when it needs a look, or "running 43m" on the blue; a line is at most 480px, so
  its outcome stays by its name on a wide screen. A busy hour (more than six in a cell) lists
  only its notable runs (not a success, an hour or longer, or past twice its stage's usual time
  that day) and folds the successes behind a muted "+23 more succeeded, 1m to 12m" that opens
  in place; a long list flows into columns as wide as the screen allows; four projects or more stack under each hour below
  1100px of content, every project below 640px. The current hour ends on the now rule and
  says what runs. `nav.js` keeps the reader's zone in a cookie and moves the rule and the
  running bars with the clock; the page's version is its runs, so it patches only when they
  change.
- **Log** (`views::log`): the head says "Log" and, as its note, what the page shows ("42
  records on this page; the newest …"). The log is the records whole, so it keeps each record's
  seq and names its step by title with its id after it; under the head the presets as one
  view-switch bar beside the filters, the note on what is left out, then Records under a
  band section's head, the table under a 2px navy rule with seq, time and kind in data mono.
- **Functions** (`/fns`): the head says "Functions" and counts them in its note; the picker and
  the
  find on one bar, the groups' index, then each group under a band section's head, its
  entries on the grid as many to a row as fit 400px, each under a hairline (a broken one under
  the navy rule), its name in data mono.
- **Settings** (`/projects/id/<p>/settings`): the head says "Settings"; how the project stands
  (active, paused or archived; its resources; when done units retire) heads its sections. The
  sections' index stands sticky in a quarter of the width, at most 300px (a scrolling row of
  tabs on a phone), each section beside it under a band section's head, each field a row (its
  name, then its form at 78ch); pause and archive are rows with a square switch; Delete sits on
  the sand under its rule. (`settings.css` is folded into `style.css`.)
- **History** (`/history`): the third of the message views, its head its name as For you
  and Questions; the threads with the owner, counted in their section's head, a module each on
  the grid, as many to a row as fit
  360px; one with an open question under the coral rule.
- **A page that cannot be drawn** (`views::missing`): the head says what is missing and, as
  its note, why; the paper lists the ways on, each a heavy link with its arrow.
- **Agent docs** (`/docs`): the head names them; the index a row of topic modules; a topic
  its markdown at the measure in eight columns, every topic listed sticky beside it.

## Accessibility

Keyboard everywhere with a visible focus ring; 4.5:1 text in every theme; 44px targets on a
phone; usable at 390px with nothing scrolling sideways; status never colour alone; every
region that updates says so politely; reduced motion honoured.

## Do's and Don'ts

### Do:
- Do keep the head slim (the name, one sentence) and let the grid carry the order.
- Do put what identifies an item behind its "⋯"; show what explains it.
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
