---
name: sluice dashboard
description: Calm briefing. One set of components in the logo's palette: a slim navy nav bar over cream paper, each page's name in the title style with one sentence under it, cards, wells and rows on hairlines, two weights, what identifies an item behind its ⋯, the channel's blue for what runs, coral only for a question to the owner.
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
  title:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, system-ui, sans-serif"
    fontSize: "22px"
    fontWeight: 650
    lineHeight: "28px"
    letterSpacing: "-0.01em"
  title-phone:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "20px"
    fontWeight: 650
    lineHeight: "26px"
  section:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "18px"
    fontWeight: 600
    lineHeight: "24px"
  lead:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "17px"
    fontWeight: 400
    lineHeight: "24px"
  body:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "15px"
    fontWeight: 400
    lineHeight: "22px"
  strong:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "15px"
    fontWeight: 600
    lineHeight: "22px"
  small:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: "20px"
  meta:
    fontFamily: "Schibsted Grotesk Variable, Schibsted Grotesk, ui-sans-serif, sans-serif"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: "18px"
  data:
    fontFamily: "JetBrains Mono Variable, JetBrains Mono, ui-monospace, monospace"
    fontSize: "12.5px"
    fontWeight: 400
    lineHeight: "18px"
rounded:
  cell: "6px"
  row: "8px"
  module: "10px"
  surface: "14px"
  control: "999px"
  chip: "999px"
spacing:
  hair: "4px"
  xs: "6px"
  sm: "8px"
  md: "12px"
  lg: "22px"
  xl: "30px"
  gutter: "clamp(16px, calc(10px + 1.5vw), 64px)"
  col-gap: "clamp(8px, calc(4px + 0.8vw), 28px)"
grid:
  frame: "1440px, centred"
  columns: 12
  sheet-narrow-below: "640px"
  sheet-regular-from: "1200px"
  phone-below: "760px"
components:
  band-nav:
    backgroundColor: "{colors.band}"
    textColor: "{colors.band-ink}"
    height: "52px"
  page-head:
    textColor: "{colors.ink}"
    typography: "{typography.title}"
  section-head:
    typography: "{typography.section}"
    textColor: "{colors.ink}"
  tabs:
    textColor: "{colors.ink-muted}"
    rounded: "{rounded.control}"
    height: "36px"
  tab-current:
    backgroundColor: "{colors.paper-2}"
    textColor: "{colors.ink}"
    rounded: "{rounded.control}"
  card:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    rounded: "{rounded.module}"
    padding: "16px 18px"
  well:
    backgroundColor: "{colors.paper-2}"
    textColor: "{colors.ink}"
    rounded: "{rounded.surface}"
    padding: "2px 18px"
  question-card:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    rounded: "{rounded.module}"
    padding: "16px 18px"
  well-look:
    backgroundColor: "{colors.paper-2}"
    textColor: "{colors.ink}"
    rounded: "{rounded.surface}"
    padding: "16px 18px"
  row:
    textColor: "{colors.ink}"
    rounded: "{rounded.row}"
    padding: "10px 12px"
  message-card:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    rounded: "{rounded.module}"
    padding: "14px 16px"
  pill:
    backgroundColor: "{colors.paper-2}"
    textColor: "{colors.ink}"
    rounded: "{rounded.chip}"
    height: "20px"
  inbox-count:
    backgroundColor: "{colors.coral}"
    textColor: "{colors.on-coral}"
    rounded: "{rounded.chip}"
    height: "20px"
  button-primary:
    backgroundColor: "{colors.navy}"
    textColor: "{colors.on-navy}"
    rounded: "{rounded.control}"
    height: "32px"
    padding: "0 14px"
  button:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    rounded: "{rounded.control}"
    height: "32px"
    padding: "0 14px"
  cell-done:
    backgroundColor: "{colors.sky}"
    textColor: "{colors.ink}"
    rounded: "{rounded.cell}"
    height: "52px"
  cell-run:
    backgroundColor: "{colors.run}"
    textColor: "{colors.on-run}"
    rounded: "{rounded.cell}"
    height: "52px"
  cell-look:
    backgroundColor: "{colors.sand}"
    textColor: "{colors.on-sand}"
    rounded: "{rounded.cell}"
    height: "52px"
---

# Design System: sluice dashboard

## Overview

**Calm briefing.** The dashboard is a briefing for work that runs while the owner is elsewhere.
Every page is drawn from one set of components (Components, below): a page's head (its name in
the title style, one sentence or a muted note under it), soft pill tabs, cards and wells on
hairlines, rows that light on hover, pills for a state, buttons that are pills with the next
move filled, a thing's status line, message cards, a turn's tool calls in a quiet fold and the
stage strip. Two weights (400 and 600), a short scale of sizes and six radius tokens carry the
whole frontend. The palette is the logo's: cream paper under a slim navy nav bar, the
channel's blue the one active colour, sky done, sand needs a look, and coral spent on one thing
only, an open question to the owner. A theme changes the colours, the faces and the materials,
never the components.

The owner opens the dashboard between other work, often on a second screen or a phone, to
answer one question within seconds: does anything need me, and is the work moving? A plan
answers in its order, a briefing over a roster. Its head says it in one sentence; under it
**Needs you** (questions, stops and silences, each a sentence with one action and a ×) →
**At work** (a row a unit, the longest silent first, its last twelve hours as a track) →
**Finished since you looked** (what this browser has not seen finish), in that order at every
width: what is moving matters more than what is done. A running step shows how long
it has run; how long a stage usually takes is the project's Stats page's to say, never a
row's. A row opens in place to its steps.

The Unigrid look the dashboard had before (a heavy navy rule over every section, heads at 850,
square corners, counter tiles) is retired, and so are its tokens: `--display-tracking`, the
`--head-*` set, `--section-rule*`, `--look-rule` and `--ask-rule` are gone from Sluice and every
theme, with `build.rs`'s display-floor generation.

## The generic rule

The dashboard knows only sluice's own concepts: project, recipe, unit, step, status (the status
table, `sluice_model::shown`), run, wait, message (owner, sessions, steps), progress fields,
outputs and the project's board document. Nothing in view code assumes a project's vocabulary.

- **Stages** come from a unit's recipe's own steps, in recipe order. A project with several
  recipes gets a group a recipe in At work; a unit's row names its current stage, and its step
  view draws them all.
- **Finished** has one component and one name: "Finished since you looked", or "Finished
  lately" while no look is kept. It is the units (or steps of no unit) whose run ended most
  recently, newest first. The plan and each project's card on home draw it the same way
  (`plan::finished_list`).
- **A long run** (a step of no recipe running six hours or more, or reporting `step_progress`)
  goes with the long runs, last in At work; its progress fields show as it reported them:
  sluice never reads or names them.
- **A badge** says what a unit's steps run on: an engine's own name, else `fn`.
- **A shared cause** is said once for the stops it holds, from the failure kind alone (a
  wall-clock cap, a stall, an engine's usage cap, sign-in, exit or readiness, a lost process, a
  passing failure, exiting without submitting); a fn's own failure is always its own.
- **The summary sentence** is built only from status counts, quiet runs and open
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
- **Navy** (`navy`): the filled button and the selected ring. `rule` and `rule-faint` (navy at
  22% and 11%) are the hairlines that part everything else; no page draws a heavy rule. In the
  dark navy turns cream, so the filled button stays the strongest thing on the paper.
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
  stale, quiet. A look cell is sand with navy on it; a
  stopped module sits on `sand-pale`.
- **Coral** (`coral`, `on-coral`): only an open question to the owner (the Inbox count, the
  question's dot, the summary's link to it) and the logo. A
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
Pine Dawn and Rosé Pine, Flexoki Light and Dark; and deep themes from classics of American
design, which also set their faces and materials (the deep tokens, below): Empire Builder,
Williamsburg, Interstate, Ranger, Park Service, Kodachrome, Fiesta and Road Atlas, each Light and
Dark. A theme's id is `<family>-light` or
`<family>-dark` (`views::THEMES`): `data-theme` on `<html>` and the `sluice_theme` cookie,
server-drawn, so a page is drawn in it from its first paint; the picker (display preferences'
Theme panel, The frame) applies a pick at once: its radios are bound to the page's `theme`
signal (`data-bind:theme__root`), `<html>`'s `data-theme` follows it (`data-attr`), and a change
posts the signals to `/settings`, which keeps the cookie
([Attributes](https://data-star.dev/reference/attributes)). The picker shows each theme's swatch in its own tokens: its band over its
paper, a square of its running colour and one of its question colour.

**The picker at thirty themes** is still one flat list, in `THEMES`' order: Sluice, the six
editor families, then Empire Builder, Williamsburg, Interstate, Ranger, Park Service,
Kodachrome, Fiesta and Road Atlas, each Light then Dark. From 900px wide it lays the list out
in two columns, so a family's light and dark stand side by side and a row is a family; under
that it is one column. No name wraps: the Theme panel is as wide as its longest name needs
(never under 264px or past the window) and scrolls inside itself when it is taller than the
window. Its order, its arrow keys and
its tick are the list's, and its swatches carry no text, so they never set a theme's face and
opening the picker fetches no face.

**The first open.** With no cookie the page names no theme and the stylesheet draws Sluice
Light or Sluice Dark by the system's scheme, and keeps what it drew as the choice: `<html>`'s
`data-init` sets `theme` from the system's scheme and posts it to `/settings` once; from then on
only a pick in the picker changes the theme, whatever the system's scheme does.

**Mapping by role, not hue.** A family's block (`[data-theme^="nord-"]`) maps every role as
`light-dark()` of its light and dark theme, and the theme's suffix sets its `color-scheme`. The
band is the theme's deepest surface (for a light pair it
borrows the scheme's darkest base); paper and paper-2 its base and
its next surface; ink its text, ink-muted its comment or subtle text; navy, the filled button
and the hairlines' ink, its strongest text colour; run its blue (or its nearest); sky a pale wash
of its cyan or green, its glyph the full hue; sand its yellow; and the question its one warm or
vivid colour no other role uses. Status never rests on colour alone in any theme: every state
keeps its glyph and its word.

**The contrast pairs** every theme's values were chosen to, in its own scheme: text at 4.5:1
(ink and ink-muted on paper, paper-2, paper-raised, sand-pale and sky; run-ink and sand-ink on
paper and paper-2; band-ink, band-muted and band-accent on the band; the text on running, sand,
coral, navy and selection fills), 3:1 for the focus ring on paper, paper-2, sky and sand-pale and
for status marks (sky-ink on paper and sky, sand-ink on sand-pale, sand-mark on sand, idle on
paper, band-mark and the coral count on the band), the chart colours 3:1 on paper and paper-2,
the question colour at least 0.1 apart in Oklab from every other role's and every chart colour,
running apart from done, and the band no lighter than the paper. The adjusted values below say
where a published value moved to meet them.

**Held by tests** (`tests/web/themes.rs`, in Chromium): each theme drawn in place on `/_ui`, a
plan and home at 320 to 2560px with no sideways scroll, every 44px target still 44px on a phone,
no words in a display face under 19px and every stage cell's two lines whole; Williamsburg's
sweeps still and every other theme's moving; a page fetching only its own theme's faces; the
first open keeping the system's scheme once; and the picker in one column at 390px and two at
1440px with no name wrapped, its keyboard as The frame says.

| Theme | Source and licence | Light | Dark |
|---|---|---|---|
| **Sluice** | the logo's own palette | cream paper, the logo's navy band | navy paper, a deeper navy band |
| **Solarized** | Ethan Schoonover, [ethanschoonover.com/solarized](https://ethanschoonover.com/solarized), MIT | base3 paper, base03 band | base03 paper, a deeper base03 band |
| **Nord** | Arctic Ice Studio, [nordtheme.com](https://www.nordtheme.com), MIT | Snow Storm paper (nord6), Polar Night band (nord0) | Polar Night paper (nord0), a deeper Polar Night band |
| **Gruvbox** | Pavel Pertsev, [github.com/morhetz/gruvbox](https://github.com/morhetz/gruvbox), MIT/X11 | light0 paper, dark0_hard band | dark0 paper, dark0_hard band |
| **Catppuccin** | Catppuccin, [catppuccin.com](https://catppuccin.com), MIT | Latte base, Mocha base band | Mocha base, Mocha crust band |
| **Rosé Pine** | Rosé Pine, [rosepinetheme.com](https://rosepinetheme.com), MIT | Dawn base, Main base band | Main base, a deeper Main band |
| **Flexoki** | Steph Ango, [stephango.com/flexoki](https://stephango.com/flexoki), MIT | paper, black band | base-950 paper, black band |
| **Empire Builder** | Great Northern Railway livery, GNRHS Reference Sheet 28 | cream paper, Pullman Green band | Pullman Green paper, a deeper green band |
| **Williamsburg** | Benjamin Moore Williamsburg Collection | Capitol White paper, Washington Blue band | Washington Blue paper, a deeper blue band |
| **Interstate** | MUTCD sign colours, FHWA | white paper, guide-sign green band | dark green paper, a deeper green band |
| **Ranger** | WPA Ranger Naturalist posters, Library of Congress | geyser cream paper, night-sky band | night-sky paper, a deeper night band |
| **Park Service** | NPS UniGuide standards | warm white paper, UniGuide green band | dark green paper, a deeper green band |
| **Kodachrome** | Kodak trade dress | white print paper, the carton's black stripe as the band | near-black paper, a deeper black band |
| **Fiesta** | Fiesta ware glazes (unofficial matches) | Old Ivory paper, Cobalt band | Cobalt paper, a deeper Cobalt band |
| **Road Atlas** | Rand McNally road map, 1926 | buff paper, road-blue band | road-blue paper, a deeper road-blue band |

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
blue darkened to #1f6fa8 for base3 text (#268bd2 gives 3.6:1), its text #1c6aa3 and #4fa6e0, and
the light focus ring that blue too (#268bd2 is 2.97:1 on the light sky);
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
mixed into its paper. Sluice's values are in `assets/style.css`; every other family's are in its
own file, `assets/themes/<family>.css`.

**Empire Builder** (`assets/themes/empire.css`), the Great Northern's Empire Builder of 1947:
Omaha Orange over Pullman Green, parted by a gold stripe. Paper cream #F5F0E1 / #2A2516, ink
#2A2414 / #F1EAD6; navy, the rules and the filled button, Pullman Green #3B331C (the one
primitive, `--p-pullman-green`) / cream; the band a deeper green, #2E2815 / #1D190D, with the
livery's gold #E0B45A as its accent and a 3px gold pinstripe under it; done Glacier Green (sky
#D3EBE2 / #1F3F37, its ink #00775F / #5FD0B6), drawn as an outline on the paper, not a fill;
running Big Sky Blue; the look the gold; the question Omaha Orange. Its faces: Libre Franklin
black (900) for the titles and Courier Prime for data. A tighter rhythm (density 0.86,
46px cells), narrow columns (0.448: the study's 56% of Sluice's 0.8) on dotted leader rows in Stats, dotted 2px links, and the
sweep at a train's steady 2.6s linear. Adjusted from the source: the band deepened from Pullman
Green #3B331C to #2E2815 so Omaha Orange #BB5800 carries white text (4.66:1) and still reads on
the band (3.15:1); running Big Sky Blue darkened to #0070A1 for white text (5.48:1), its text
#006594; the look fill the gold #E0B45A with dark text; the dark question lifted to #E07A1F with
dark text (6.3:1), as white on it is 3.0:1; the dark sky-ink and run-ink lifted for the green paper.

**Williamsburg** (`assets/themes/williamsburg.css`), the Benjamin Moore paints matched to the
pigments of the 1928-34 restoration of Colonial Williamsburg. Paper CW-10 Capitol White #F5F2E5
/ #263440, ink #2A2C2E / #F5F2E5; navy and the band CW-630 Washington Blue #304656 (the one
primitive, `--p-washington-blue`), a deeper #1B2630 band in the dark, with a double hairline of
CW-405 Damask Gold under it; running CW-625 Prussian Blue #325A7D; done CW-585 Ewing Blue (sky
#D0E2E0), its mark CW-550 Geddy Verdigris; the look Damask Gold #DDB558, its text CW-265
Charlton Brown #693D2D; the question CW-300 Tucker Orange #DF745D / #E88A74 with dark text. Its
faces: Libre Caslon Display at its one weight (400) for the titles, Libre Caslon Text, its kind lines in italic, and Anonymous Pro for data, a
typewriter-cut mono whose zero is slashed, so 0 and O differ by shape at 12px (the study's
Cutive Mono told them apart by width alone). Laid
lines in the paper (the ink at 4% every 5px), an airier rhythm (density 1.06, 54px cells), cells
not reached dotted, Stats' columns at 0.56 (the study's 70% of 0.8) on ruled rows, a 3px focus
offset and 4px link offset, 1.75px glyphs, and no motion: `--sweep-animation: none`, the still
line along a running cell's foot. Adjusted from the source: the done mark Geddy Verdigris #2E8B7E
darkened to #2B7E72, so the mark keeps a margin on the Ewing Blue wash (3.61:1, against 3.06:1);
the dark paper #263440 and band #1B2630 designed deeper than Washington Blue so the band stays
the deepest surface. Libre Caslon Text has no old-style figures, so its prose figures stay
lining.

**Interstate** (`assets/themes/interstate.css`), the MUTCD's sign colours (FHWA; PMS by
pmscolorguide.com). The band the guide sign's Green 342 #006648 (`--p-guide-green`) / #08291D,
drawn as a sign panel: inset 6px from the window, its corners 10px, a white border inside the
green; paper #F6F6F2 / #1C2A24, ink #111614 / #F2F3EF, navy a deeper green #00402D / the ink;
running Blue 294 #002F6D / #3F7FD6; done a pale guide-green wash, its mark the green; the look
the warning sign's Yellow 122 #FFD241 with black text, its text Brown 469 #6B3F23; and the
question the incident-management fluorescent pink. Overpass, the open Highway Gothic, heavy
(800) and mixed case for the titles and the text (its middle dot, which Overpass draws with
no advance, comes from the fallback face), and Overpass Mono for data. Rounded corners (modules
8px, cells and buttons 6px, chips 4px), look cells in the
warning sign's 2px black border, cells not reached on a 1.5px line, Stats' columns on dashed
lane lines (square: the study's 4px would round every corner of the stretched SVG's columns), a
3px focus ring, 2px links, 2.5px glyphs and lane dashes running along a running cell's foot
(1.1s). Adjusted from the source: Pink 198 #E64461 lifted to #FF859A with black text (9.07:1) so
it reads on the green band (3.03:1), in character for a fluorescent; the dark pink #F0708A; the
dark band #08291D designed deeper than Green 342.

**Ranger** (`assets/themes/ranger.css`), the WPA Federal Art Project's screen-printed Ranger
Naturalist posters, 1938-41, sampled from the Library of Congress's scans (LCCN 2007676132-4):
aged paper and ink. Night band #1C2232 (`--p-night`, also ink and navy) / #151A26 over geyser
cream; running the poster sky; done pale aqua #AEC1B6 (sky #D4DFD6 / #34474A); the look Lassen
gold #DEC070, its text umber #6B563A; the question Lassen's rose. Its faces: Jost (a Futura)
bold for the titles and DM Mono for data. The screen print's misregistration: an offset aqua
line under the band; cells
not reached on a 1.5px line; no shadows; a 3px focus ring; 2px links at 4px; 2.25px glyphs and a
slow 4.8s ease-in-out sweep. Adjusted from the source: paper #F3EDDD from the sampled cream
#D7CCB2; running #3F6A82 (5.0:1 under cream) from the sky #48748D, its text #345A70; the rose
#9D6B6A lifted to #C9736D with night text (4.65:1, and 4.65:1 on the band) / #D98A7F.

**Park Service** (`assets/themes/parks.css`), the NPS UniGuide standards' PMS colours (UniGuide
ch. 2.4). Band PMS 5605 #22372C (`--p-uniguide-green`, also the light ink) / #141C18; navy, the
rules and the filled button, PMS 1615 brown #8D4820 (`--p-uniguide-brown`) / a lifted #E2A272;
paper #F1EEE7 / #1F2A24; idle PMS 405 #696158; running PMS 301 #004A88 / #4F8FCF; done PMS 5753
olive (sky-ink #5F6639 on #DDE2CC); the look PMS 131 #D08A00; the question PMS 485 red. Its
faces: Besley, a Clarendon, for the titles (800), Signika for text and Red Hat
Mono for data. A 3px brown sill under the band; an
open rhythm (density 1.08, 54px cells); buttons 3px round, chips 2px; and a 3.6s sweep. Adjusted
from the source: the red #E1251D lifted to #F33C30 with dark text (4.55:1; it reads on the band
at 3.33:1) / #F0574C; the look's text #7A5100 from PMS 131;
the sill starts a pixel under the nav, so its top row is the band's.

**Kodachrome** (`assets/themes/kodachrome.css`), Kodak's trade dress on the Kodachrome carton:
Kodak Yellow (PMS 1235C, #FFB700) and Kodak Red (PMS 485C, #ED0000) as Kodak publishes them,
over the carton's dark stripe #282C2F, sampled from a 1961 carton (the primitives
`--p-kodak-yellow`, `--p-kodak-red`, `--p-box-stripe`). Paper white #FBFAF6 / #1D2023, ink
#1B1D1F / #F3F1EA; navy and the band the stripe, the dark band #121416; the yellow is the look
and the band's accent and mark; the question the red. Its faces: Alfa Slab One for the titles
(400), IBM Plex Sans Condensed for the text, as on a data
sheet, and IBM Plex Mono for data. 2px corners as a perforation's, sprocket holes along the band's foot (a 12px
edge, the paper showing through 8px in every 24px), narrow columns (0.624: the study's 78% of
0.8), and the sweep
stepped, `steps(8)` over 2s, advancing a frame at a time. Adjusted from the source: Kodak
names no blue, so running is a designed blue #1F5FBF (white on it 6.09:1), its text #1D56AD; the
yellow, Kodak's identity, is the look colour and not the band, the theme's main compromise;
the red carries black text (4.6:1) and reads on the band at 3.08:1; the dark question lifted to
#FF4D3D. Done is a designed pale blue (#DDE8F6 / #24364C, its ink #2E5F8A / #9CC0F0). Contrast minima: text 4.60 / 5.61, marks 3.08 / 5.23.

**Fiesta** (`assets/themes/fiesta.css`), Homer Laughlin's Fiesta ware of 1936, Frederick
Rhead's concentric rings: Cobalt #13294B, Old Ivory #F2EFDE, Lapis #2B64A1, Turquoise #56B1BD,
Yellow #DAAC49 and Poppy #F24B21, a collectors' guide's matches from photographs, unofficial
(each a primitive, `--p-cobalt` and so on). Paper Old Ivory / Cobalt #16294A, ink Cobalt / Old
Ivory; the band Cobalt, #0D1B33 in the dark, with Turquoise its mark; running Lapis; done a
Turquoise wash; the look the Yellow; the question Poppy. Its faces: Righteous for the titles, Nunito for the text and DM Mono for data (declared once, in `ranger.css`, which
Ranger shares). 14px modules, 10px cells, buttons and chips at 14px, a roomy rhythm (density 1.1, 54px cells),
not-reached cells dashed, rounded columns (6px), a 3px focus ring 3px off, 2px links at 4px,
2.25 glyph strokes, and a 2.4s sweep with a slight overshoot. Adjusted from the source: Poppy
keeps its value and takes deep cobalt text #0B1A33 (4.8:1), and reads on the band at 4.0:1;
Turquoise darkens to Juniper #2C6672 as the done mark (#7FD0DA in the dark); Lapis text
#24578E; the dark question lifted to #F7703F. Contrast minima: text 4.80 / 5.41, marks
4.01 / 5.00.

**Road Atlas** (`assets/themes/atlas.css`), Rand McNally's Auto Road Map of Illinois of 1926,
the year the US numbered its highways, sampled from a public-domain scan of the aged sheet: road
blue #2C2C4B, map paper #DBC89B, boundary red #9F402E, map ink #30241E (the primitives
`--p-road-blue`, `--p-map-paper`, `--p-map-ink`). Paper buff #F4EAD2 / road blue #272638, ink the
map's / cream; navy, the rules and the filled button, the road blue; the band #1E1E36 /
#1A1A2B with the map paper as its accent; running a lifted road blue #34509A; the question the
boundary red. Its faces: Old Standard TT bold for the titles, as
a map's place names; Fira Sans Condensed for the text and Source
Code Pro for data. A dashed centre line under the band, a faint graticule on the paper (a line every 72px
each way, the road ink at 6%), a compact rhythm (density 0.92, 48px cells), not-reached cells
dotted as an unpaved road, and narrow columns (0.512: the study's 64% of 0.8) on a dotted grid
in Stats, with a 3.4s
ease-in-out sweep. Adjusted from the source: a two-ink map, so done (a grey-blue wash #DFE0DC
/ #323C4C, its ink #3D5B7A / #A5BFD8) and the look (#D9B25E) are designed in its spirit; the
paper lifted from the aged #DBC89B to #F4EAD2; the road blue lifted to #34509A for running
(text on it 6.75:1); the band deepened to #1E1E36 so the boundary red, lifted to #B24C38, reads
on it (3.08:1) and carries cream text (4.89:1); the dark question #E0745C. Adjusted from the
study: its text face, Barlow Semi Condensed, draws I and l, 0 and O alike at 12px, so
the text is Fira Sans Condensed, whose l has a tail and whose zero is narrower than its O.
Contrast minima: text 4.89 / 4.95, marks 3.08 / 5.20.

**Il1 and 0O at 12px.** Every mono face draws a slashed or dotted zero. The text faces tell 1
from I and l (1 is flagged everywhere), but Overpass (Interstate), Jost (Ranger) and Libre
Franklin (Empire Builder) draw I and l as plain bars a little apart in height; that is accepted
as it is (the owner's ruling), since a word in context reads and every id and figure is in the
mono face.

**The chart columns.** A family's `--chart-column-width` is the study's column share times
Sluice's own 0.8, so a "narrow" column is narrower than Sluice's: Empire Builder 0.448,
Williamsburg 0.56, Kodachrome 0.624, Road Atlas 0.512; the rest keep 0.8.

**One stylesheet.** A family's file is its block (`[data-theme^="<family>-"]`: its colour
roles, the deep tokens it changes, its faces' `@font-face` rules), plus a
`[data-theme="<family>-dark"]` rule for any non-colour value that differs by scheme
(`light-dark()` takes colours only). `build.rs` joins `style.css` and every file under
`assets/themes/`, by name (and versions the fonts' addresses), into the one stylesheet a page loads (`/static/style.css`,
`views::STYLESHEET`, what `tests/themes.rs` reads). A family's file comes after Sluice's
defaults, so what it declares wins and what it leaves out inherits. Primitives (`--p-…`, a
source's named colours) are readable only inside their own family's block; a lint in the test
fails on one anywhere else.

### The deep tokens

A theme is more than its colours: these tokens set its materials. Sluice's values are the
defaults (`:root, [data-theme]` in `style.css`), so a family that leaves one out draws it as
Sluice does, and the editor schemes are Sluice's shape in their own colours. Content, order, the
grid, sizes and the 44px targets are never a theme's: every theme shows the same page. Every
declared value must be in its allowed set (`tests/themes.rs`).

| Token | Sluice | Allowed | What it sets |
|---|---|---|---|
| `--font-display` | Schibsted Grotesk Variable | a stack ending in a generic family | the titles: a page's name, a step's, a project's on home, a docs topic's (20px and up) |
| `--font-text` | Schibsted Grotesk Variable | the same | body, modules, tables, controls |
| `--font-mono` | JetBrains Mono Variable | the same | ids, durations, clock times, data |
| `--display-weight` | 650 | 400 to 900 | the titles' weight |
| `--kind-style` | normal | normal, italic | a module's kind line (its glyph and word stay upright) |
| `--prose-numerals` | normal | normal, oldstyle-nums, lining-nums | figures in prose; data figures stay tabular |
| `--radius-module` | 10px | 0 to 14px | a card, a module, a message card, an empty state; the well (`--radius-surface`, 1.4 times it) and a row (`--radius-row`, 0.8 times it) follow it |
| `--radius-cell` | 6px | 0 to 14px | a stage cell, a notice, a run file, a field table, a confirmation's fold |
| `--radius-control` | 999px | 0 to 14px, or 999px (a pill) | buttons, pill tabs, the view switch, a find field |
| `--radius-chip` | 999px | the same | pills and the Inbox count |
| `--band-edge-height` | 0px | 0 to 12px | a strip under the band, below the nav, where text never sits |
| `--band-edge` | none | gradient layers | what the strip draws (a livery stripe, a sill, sprockets, hairlines) |
| `--band-frame` | none | none, or inset shadows | a sign panel's frame inside the band |
| `--band-radius` | 0px | 0 to 14px | the band's corners |
| `--band-margin` | 0px | 0 to 8px | the band's inset from the window (its content keeps the page's edges) |
| `--paper-texture` | none | none, or gradients (never an image) | the paper's grain, laid lines or graticule |
| `--paper-texture-alpha` | 0% | 0 to 6% | its ink at its darkest, which the contrast test lays over the paper |
| `--density` | 1 | 0.85 to 1.1 | the vertical rhythm: the page's and a module's padding, the gaps between modules, rows and sections (never a target's height) |
| `--cell-height` | 52px | 46 to 56px | a plan row's stage cell (a strip's is 4px more; a phone's never under 44px) |
| `--cell-done-fill` | `sky` | a colour, or transparent | a done cell's fill |
| `--cell-done-edge` | none | none, or an inset shadow | a done cell's edge (an outlined done cell) |
| `--cell-look-edge` | none | none, or an inset shadow | a look cell's border |
| `--cell-empty-edge` | inset 1px `rule` | none, or an inset shadow | a cell not reached, solid |
| `--cell-empty-outline` | none | none, or a 1 to 2px dashed or dotted outline | a cell not reached, dashed or dotted, just inside it |
| `--chart-ink` | `sky-ink` | a colour 3:1 on paper and paper-2, 0.1 from coral | Stats: the columns, a median, a p90's edge |
| `--chart-busiest` | `navy` | the same | Stats: the busiest column |
| `--chart-series` | `run-ink` | the same | the board's charts: bars, lines and dots |
| `--chart-range` | `sky` | a colour 0.1 from coral | Stats: a p90's fill |
| `--chart-grid` | none | none, or gradients | the rows behind the throughput's columns (leaders, lanes) |
| `--chart-column-width` | 0.8 | 0.3 to 1 | a column's share of its slot |
| `--chart-radius` | 0px | 0 to 14px | a column's and a duration bar's corners |
| `--focus-width` | 2px | 2 or 3px | the focus ring, always solid, 3:1 on paper, paper-2, sky and sand-pale |
| `--focus-offset` | 2px | 1 to 4px | its offset |
| `--selection`, `--on-selection` | `run`, `on-run` | a pair at 4.5:1 | selected text |
| `--link-thickness` | 1px | 1 to 3px | a link's underline |
| `--link-offset` | 3px | 2 to 6px | its offset |
| `--link-style` | solid | solid, dotted, dashed | its style (dotted as a timetable's leaders) |
| `--sweep-animation` | `tide 3.2s` ease-out | `tide` (a bar) or `lane` (dashes), at least 1s, any easing or `steps()`; or none, the still line | the sweep along a running cell's foot, the one motion a theme sets |
| `--sweep-fill` | `sweep` | a colour or a gradient | what sweeps |
| `--glyph-stroke` | 2 | 1.75 to 2.5 | every Lucide glyph's stroke (the glyphs stay Lucide's) |
| `--lift` | Sluice's two shadows | shadows, soft, or none | a menu's, the drawer's and a dialog's lift |

**The still line.** At rest a sweep's bar lies across the whole foot of a running cell at .6;
both keyframes (`tide`, `lane`) set the whole bar, so a moving sweep never shows the rest.
`--sweep-animation: none` (Williamsburg) and reduced motion, in every theme, draw that still
line; the drawer's slide is never a theme's.

**The titles' face.** Titles (a page's name, a step's, a project's on home, a docs topic's)
read `--font-display` at `--display-weight`, and nothing smaller does, so a theme's display face
never sets words under 19px (a Chromium test holds it in every theme). Coral is never a
chart's, a rule's but a question's, or a band edge's.

## Type

Sluice's: one family, **Schibsted Grotesk** (variable) for titles and text, with **JetBrains
Mono** for ids, durations, clock times and data only. Another theme sets its own faces (Themes,
the deep tokens); its sizes and weights stay these.

**Loading.** Every theme's faces are `@font-face` rules in the one stylesheet, pointing at Sluice's
own copies of pinned fontsource 5.3.0 files (Sluice's `@fontsource-variable/schibsted-grotesk` and
`@fontsource-variable/jetbrains-mono`, each subset fontsource ships; a family's own faces the latin
subset), vendored under `crates/sluice-web/assets/fonts/` with each family's licence and every
file's sha256, and served at `/static/fonts/<file>?v=<fingerprint>` to be kept for good. A page
asks no host but the dashboard for anything. A browser fetches a face only when it
sets text in it, so a page downloads its own theme's faces and no other, a switch needs no new
link, and a page's first paint asks for the cookie's theme's faces. A family's face has a fallback
face (a local Arial or Courier New with `size-adjust` and ascent and descent overrides measured
from the face) next in its stack, so the swap to it does not move the page. The picker's swatches
carry no text, so opening it fetches nothing; `/_ui`'s theme cards set each theme's faces.

**Two weights.** `--w-regular` (400) for words and `--w-strong` (600) for names: a title in a
row, a unit's name in a sentence, the current tab, a count that matters. Titles alone take
`--display-weight` (650 in Sluice). Nothing is 500, 700, 800 or 850; only the nav band's
wordmark keeps its own weight.

**The sizes** (tokens on `:root`, `--t-*`), each with its role:

| Token | Size | Role |
|---|---|---|
| `--t-title` | 26px/32px (22px/28px on a phone) | a page's name, a step's name (two lines at most, the whole in its tooltip), the title face |
| `--t-section` | 20px/26px, 600 | a section's head (`ui::SectionHead`), a settings section, a tab panel's head: a clear step under the page's name and over a card's title (18px) and the body |
| `--t-lead` | 17px/24px | the latest sentence that leads a step's Now |
| `--t-body` | 15px/22px | body, a sentence in Needs you, a project's summary on home |
| `--t-md` | 14px/20px | tabs, a module's body, a quiet link |
| `--t-sm` | 13px/18px | meta in `ink-muted`, a status line, a message card's head, buttons |
| `--t-xs` | 12px/16px | a table's column names, an axis |
| (data) | 12.5px/18px mono | ids, durations, clock times, a tool call's key argument |

- Tabular figures only on numeric data (`time`, durations, counts, `.num`), never page-wide:
  Schibsted's tabular figures widen its punctuation.
- Long tokens (ids, paths) wrap (`overflow-wrap: anywhere`), so a page never scrolls sideways.
- No uppercase labels, no kickers above a heading, no letter-spacing but the title's -0.01em.

## The grid

**Fluid to a centred frame, 320px to 3840px.** The column is the window less a gutter each side
(less the step drawer when it stands beside the page), up to `--frame` (1440px), centred; the
nav bar's contents, the page's head, its row and the page share its edges at every width, so
on a wide screen the page reads as one centred column under the controls that act on it
rather than a strip pinned left. The gutter grows with the window,
`clamp(16px, 10px + 1.5vw, 64px)` (16px on a phone, 32 at 1440, 48 at 2560, 64 at 3840), and
so does the gap between columns, `clamp(8px, 4px + 0.8vw, 28px)`. Prose keeps `--measure`
(68ch) at every width.

**Lay out by the box, not the window.** `main` is the `frame` container and every sheet the
`sheet` container, so what sits in them follows the room it has, the drawer and a board beside
the plan counted. Media queries are for the frame outside `main` (the nav bar, the head and the
row), and
for input (pointer, hover, motion, colour scheme).

**The sheet** (`ui::grid_open`, a plain `div.sheet`) has three modes, by its own width (the
frame keeps it to 1440px, so a wide screen gets the same sheet as a laptop, centred):

| sheet | columns | a module of span N (twelfths) |
|---|---|---|
| under 640px (narrow) | 1 | across the whole sheet |
| 640 to 1199px (medium) | 12 | half (6) when N is 6 or less, else whole |
| 1200px and over (regular) | 12 | N |

- `ui::module_open(span, swell, label)` and `ui::column_open(span)` take `span` twelfths; a
  child with no span takes the whole sheet.
- Nothing is drawn over the sheet: the grid is the layout, not a picture of it.
- **Narrow end.** At 320 and 390 nothing clips and the page never scrolls sideways; every
  control is 44px tall on a phone.

## The frame

The page's frame (`templates/layout.html`, `render_framed`) is a slim nav bar, the page's
row under it and the page's head on the paper under the row.

- **The nav bar** (`nav#top-nav`, 52px; 48px on a phone): the mark and "sluice", "Projects",
  the project switcher (its button the current project's icon and name in a box of the band's
  rule, the menu every project; its letter icon the band's mark deepened toward the band, so
  its ink reads at 5:1 or more), Inbox with its coral count, then at the right the live line (a
  square in `band-mark` and "Live" while the page's stream is, from `html[data-stream]`) and
  display preferences (below). The current place carries a 3px underline in
  `band-mark`. On a phone the wordmark and Projects give way to the switcher (whose menu has
  them), its name shortens before anything wraps, and the live line is its square.
- **Display preferences** (`views::prefs`, the sliders at the bar's right end) are a short
  menu, a row each on the raised paper: **Theme** (the current theme's name in muted ink and its
  swatch in the page's own tokens, then a "›"), **Show value types** (its checkbox, toggled in
  place) and **Keyboard shortcuts** (its `?` and a "›"), then under a faint rule "States and
  parts" (the gallery) and "Agent docs". Theme and Keyboard shortcuts are
  sub-menus (`details.sub`): a row opens its panel inside the same popover, headed by a back
  control ("‹ Theme") over a faint rule, with the top level out of sight; the panel scrolls
  inside itself when it is taller than the window. Theme's panel is the picker (Themes): a pick
  applies at once and the menu stays on the themes, so the next can be tried; Keyboard
  shortcuts' is the keys' list (`sluice-keys`). The keyboard follows `sluice-menu`: ArrowDown
  from the sliders goes in, the arrows, Home and End move through the rows, Enter, Space or →
  opens a row with the focus on its chosen theme or its first key, ← or Backspace returns to
  the row it came from (→ inside a panel moves nothing: the levels are across, the items down),
  and Escape closes the whole menu, the focus back on the sliders; it opens again at the top.
  Each row's summary says `aria-expanded`. The panel is as wide as its level needs (264px at
  least, never past the window), so it widens to the left for the two columns of themes. Rows
  and the back control are 40px, 44px on a phone. A pick applies at once, with no Save: the
  theme radios and the value types switch are bound to the page's signals and a change posts
  them to `/settings` (Writes).
- **The page's head** (`Frame::head`, `ui::page_head` and `ui::page_head_with`): on the paper,
  its crumbs when it is one thing in a tab (a step's way back to its plan and its unit, the unit
  by its title or name cut to 40, never "Its unit"), its name in the title style with its Details' "⋯"
  pinned to the head's right edge, and under it on a plan and on home only the summary
  sentence (`ui::summary_sentence`), one line of large body text where it fits; a page that says
  what it shows has a muted note in its place (`ui::page_head_note`: the Log's records, the
  Day's date and clock, the Functions' counts). No page carries a description blurb.
- **The row** (`.subnav`): its sections as pill tabs (`ui::tabs`: Plan, Day, Stats, Messages,
  Log, Functions, Settings; the current one on `paper-2` at weight 600), a count line (`Frame::meta`), and at the right its tools
  (`Frame::tools`: a find field and the view switch on a plan). A hairline closes it. A page
  has one row of tabs: For you, Questions and History carry their switch under the page's name,
  in the head, not in the row. On a phone the tabs stay one line (all seven fit
  at 390px; a narrower screen scrolls them sideways within themselves) and the tools take a
  line of their own.
- **Where they stand**: the row comes first on every page, right under the nav bar, and the
  head under it, so the row never moves with a head's height: a plan's, a step's, a unit's and
  a thread's tabs are in the same place.

The budget: at 1440px the plan's first section starts within 200px of the top (a test holds
it within 220), about 260px on a phone.

**The view switch** (Plan · Both · Board on a project with a board, Plan · Board below 1280px)
is a bar of pill links in the row (`ui::switch`): the current view on `paper-2` at weight 600
(`aria-current`), the others muted words. A link asks the server for its view (`?view=`, kept
in the project's `sluice_view_<id>` cookie); `sluice-board` shows the chosen view at once and
keeps it per project. The messages' switch (For you,
Questions, History, under the page's name) and the log's presets are the same bar.

## Details

What identifies an item rather than explains it (its step, unit and run ids, its run number
and earlier runs, its fn, engine and model, its tags, its recipe, its params, a message's id, a
hash or a path) is kept behind one "⋯" at the item's end: `ui::Details`, drawn by its `menu`.
It is a `<details>` inside a `sluice-menu` (Escape and a click
elsewhere close it and give the focus back to its button, which says `aria-expanded`), 44px on
a phone, where it opens as a sheet at the bottom of the window. Wider, its panel hangs from its
button's right edge, and `sluice-menu` moves it back inside the window when that would cut it
(8px from either edge), so every panel opens whole at every width (a Chromium test opens each
on the plan, a step, a unit, home and the inbox at 390, 1440 and 2560). Its panel lists each
value with its name; an id is whole in data mono with a copy button, a description is markdown. Every row, module, head and message
has one; ids stay addressable (the find takes them, URLs carry them, and a step's Inputs,
Outputs and Runs tabs stay whole).

**A value by its shape** (`ui::ValueSet`), never its name: a number, a boolean or a short word
shows; a hash, an id, a path, a URL or a long token goes to the Details; prose shows clamped to
two lines, whole in the Details. Every preview goes through it: a step's progress fields (Now,
its Progress module, a long run's row), Overview's outputs and a recipe's view on a unit's row and
head (a "land sha" is in the unit's Details); only the Inputs and Outputs tabs draw every value
whole. A message line on a row names its sender in words (the
sending step's stage, a session's `@name`, "orchestrator" for a message stored before sessions
named themselves, or "you") and keeps to one line.

**Who.** Every sender and recipient is named as the identity it is (`ui::Party`, a step by its
`StepRef`): a session its `@name`, the owner "you" ("You" leading a sentence or as a
conversation's chip), sluice "sluice". "orchestrator", the address messages were stored under
before sessions named themselves, is shown as stored, in `ink-muted` at weight 500 (`who-old`),
never "the orchestrator" and never rewritten. A unit's owner (`own`) is its `@name` (or "you")
small in `ink-muted`; an unowned unit says nothing.

## Surfaces

Four surfaces carry every page, each from its radius token and nothing heavier than a hairline:

- **A card** (`.card`, `--radius-module`): a region of its own on the paper, edged by a
  `rule` hairline: a project on home, a finished unit, a message, a fn, a docs topic.
- **A well** (`ui::well_open` / `well_close`, `--radius-surface`): the raised second paper
  (`paper-2`), where rows that belong together stand: Needs you, a step's Now. A well that
  needs a look stands on `sand-pale`. No surface carries a stripe at its edge.
- **A row** (`.row`, `--radius-row`): an item in a list, `paper-2` on hover and while open.
- **A module** (`ui::module_open`, `.mod` on the grid, `--span` twelfths): a step's or a
  unit's part on the sheet (Now, Progress, what it waits on, a stopped step, a question). Its
  parts: `.mod-k` (its kind's line: a glyph, a word, who and when), `.mod-t` (its title, 16px at
  600), `.mod-meta`, a stage strip, `.mod-body` (prose at the measure), `.mod-actions`. The one
  that needs a look swells (`Swell::Look`): a well on `sand-pale`, its next move first (Retry
  with feedback). Anything else (`Swell::Plain`) stands on the paper. A question to the owner
  is no module but the question card (Components). The plan draws no modules (The briefing).

A section's head (`ui::SectionHead`, `ui::section_head`) names a region at 18px/600, its count
muted after it, a note or its one action at the right; no rule over it. Pages never nest a card
in a card or lift anything but the drawer, a menu and a dialog (`--lift`).

## Status presentation

Status words, glyphs and ranks come from the status table (`sluice_model::shown`) only, and
status is never colour alone: every state has its Lucide glyph and its word.
The board, plan, drawer and step page use the same activity observer for quiet steps, including
every step in the drawer's unit.

**The stage strip** (`ui::stage_strip`): a unit's stages in recipe order, one 56px cell each
(48px on a phone), the cell's kind read from the status table (`ui::Cell::of`). Wherever a
strip is narrow (a phone, the drawer, a half-width module) one rule holds: three cells a
row, never under 72px, so a stage's name is never cut; where a cell says its state in a word,
the word goes under the name, never beside it.

- **Not reached**: an outline in `rule` with the stage's name; a waiting state that says
  itself (held, queued, paused, blocked) writes its glyph before the name and its word under
  them ("⊖ land", then "blocked").
- **Done**: sky, its glyph (check, set by hand, skipped) and its name, its word and how long
  it took under them ("succeeded 28m").
- **Running** (running, finishing, stopping, outside): the blue,
  its glyph (in `on-run`) and its name small, its word and its elapsed time big and ticking
  under them, a sweep along its foot. It is never set
  against a usual time: that is the Stats page's.
- **Needs a look** (a state that wants attention: failed, cancelled, stale, quiet): sand, its
  glyph, its name and its word, its time; a quiet run keeps running's blue edge.
- A note may span the cells not reached ("then review and publish").

Every cell has the same two lines: its glyph before its name (a cell not reached that says
nothing draws none), then its state's word, as the status table says it, and its time, big for
a run and a look, small for done. The word prints in every strip, the drawer's and a phone's
too; the time wraps under it when the cell is narrow. A state word never shares its line with
the glyph, and a word wider than its cell wraps within it, never clipped or out of sight. The
cell of the page's own stage is ringed inside its own edge, so the ring never reaches the cell
beside it.

Each cell's state is said in words to a screen reader, and a cell with a page links to it.
`ui::stage_marks` draws the same strip as 12px squares in a line of words.

### Status ramp

Each state, in the table's priority order, with its glyph's tone (`.g-<key>`) and the cell it
draws. A state said as a pill is `ui::status_pill` on every page (a plan row's stage, the
Day's runs, the Stats' slowest runs, a step's quiet, a broken fn): its glyph and its words in
its cell's tone, the blue running, the sand needing a look, the sky done, an outline waiting.
A loose step's pill says its state once ("quiet 3h", never "quiet · quiet 3h").

- **Failed**: Lucide `circle-x`, solid, in `sand-ink` everywhere (on the sand of a look cell or
  pill, `sand-mark`); a look cell on sand. Never coral.
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
- **Pending**: `circle-dashed` in `idle`; not reached. With a bypass waiting (`step_bypass`) it
  says "bypass · <who set it>" under its name, cut to one line.
- **Set by hand** (`manual`): `circle-dot` in `sky-ink`; a done cell. One its bypass settled
  says "bypassed" wherever set by hand is said of it.
- **Succeeded**: `circle-check`, solid, in `sky-ink`; a done cell.
- **Skipped**: `circle-slash` in `idle`; a done cell.

### Named Rules

- **The Shape Carries It Rule.** Every state has its glyph and its word; colour comes second.
- **The One Coral Rule.** Coral is an open question to the owner and the logo, nothing else.
- **The One Blue Rule.** Blue fills only what runs now.

**The summary sentence** (`ui::summary_sentence`), on home's project modules: "1 question
for you. 1 failed, 1 cancelled. 3 units at work: 1 quiet for 53m. 2 waiting. 3 of 10 units
done; the last finished 57m ago." It names no unit: quiet runs are counted, the longest said
("3 quiet, the longest for 1h 6m"). Each part is left out when it has nothing to say, and the
question links to where it is answered, under a coral underline. A quiet run's time ticks on
the page. A plan's head says it shorter, each state in its own word (The briefing).

## The step view

`views::unit_steps` (`View`, from a unit by `unit_steps::of_unit`) in a `sluice-steps`: a unit
drawn step by step, after the reference the owner chose (a pipeline's run view), in Synthesis.
It is the unit page's body, what a plan row opens in place, and a step's Runs tab's first part
(its own row ringed).

- **Rows are steps**, in graph order: the recipe's (else the plan's), each after every step of
  its unit it comes after. Each row: the rail, the name (its stage, linking the step; a
  condition it runs on muted before it, "if ok", "if not ok"; a wait on another unit under it,
  "waits for Terns"), **On** (what it runs on: an engine's name, a fn; muted, its own column
  from 560px, and only when the unit's steps run on more than one thing), **State** (`ui::status`: glyph and word, "bypassed" for a result its
  bypass preset settled, then a "bypass · <who set it>" tag while one waits, then `ui::tries`'
  "run 3"),
  **Progress** (a scatter's "12/40" or a short `step_progress` field, a 3px bar under an "a/b";
  "—" for none; a column only when one of its steps reports any), the **timeline** and **Span** (how long its current run took, in `ink-muted`
  data mono; a running one's ticking in `run-ink`).
- **The rail** (an inline SVG a row, out of flow so its lines run the row's whole height): the
  gates between the unit's steps (`after` entries and values passed) in `line`, 1.5px; a node a
  step, 5px: filled `sky-ink` done, ringed `idle` not reached, an arc in `run-ink` turning while
  it runs as the glyphs turn (the running glyph's arc turns over a faint ring of its own, so
  held still it reads as a ring, never as a letter C) (held still under reduced motion), `sand` edged in `sand-mark` for a
  look. A chain runs down lane 0, beside the names; a branch takes the lowest free lane to the
  left (never one freed on its own row), and an arrow marks where a branch or a join reaches a
  step. At most five lanes.
- **The time axis.** One axis for every row, from the unit's first start to now while it is not
  done (rounded up to a step of a tenth of the time drawn, 5 minutes at least, so the server
  redraws it once a step, never with the clock), else to its last end. Linear, because a reader
  compares lengths; but no run is drawn under 3px, so a two-second step beside a two-hour one is
  still a mark in its row, and a stretch where nothing ran that is a tenth or more of the time
  drawn to scale collapses to a break, 4% of the width on `paper-2` between dashed `rule` edges,
  labelled with its length ("+1d 2h"). A log axis was refused here: it would make every
  hour-long run look alike and lie about the gaps. The one exception is the step view in the
  plan's drilldown (In the plan, below), which takes its row's log scale so the run under the
  row reads exactly as the row's track does; a unit's whole history stays linear everywhere
  else. The labels (sans, 12px, as At work's axis) sit at a nice spacing
  of the scaled time, 17% apart at least, each with a hairline down the rows; "now" is a
  `run-ink` line with its word, which `sluice-steps` moves on the clock's tick (every five
  seconds and after every patch; `data-now`: the last stretch's start, place and rate), as it
  does a running bar's end. A bar: 8px, `--chart-radius`; done `sky-ink`, running `run`, a look
  `sand-ink`, no result `idle`; earlier runs at 35%.
- **The head**: the columns' names in `ink-muted` 12px, and after "Step" its unit's owner
  (`own`), which a screen reader hears alone; under 560px the head keeps only the owner. The
  plan row and unit page already say the owner, so their step views leave it out. The
  Runs tab keeps it in the step view's head.
- **The foot**: "Last message", how long ago, its sender in words, its first line linking its
  thread.
- **Selecting.** A row is the step's link: a click anywhere on it opens the step (in the drawer
  on the plan and in the drawer, its page elsewhere); the arrows, Home and End move between rows;
  the row in focus, clicked or open in the drawer is selected: `paper-2` under a 2px navy ring
  (the host's `selected`, kept through a patch). Hover is `paper-2` alone.
- **In the plan.** On the row's own time (`View::recent`): the last twelve hours to now on
  At work's log scale, so a run reads the same in the row and under it (a run that began
  earlier starts at the left edge, saying how long ago). A row of At work is a `<details>`:
  open, its step view stands under it on `paper-2`, its timeline exactly the row's track (as
  wide, ending at the same x: Took moves before it and the view ends where the row's chevron
  column begins; on a phone each run's bar starts where the row's track does), the current
  step's row tinted (`run` at 7%; `sand-pale` for a look, a full tint and no side stripe), the
  earlier runs' marks before "run 2" left out, then its latest message (with none, "Last heard
  22:51 (6m ago)", or for a quiet unit "Not heard from since 19:14 (3h ago)"), Message and Unit
  page as quiet links and Open step. A run clipped at the left edge says "from 3d ago" in data
  mono at 11.5px. It stays open
  through a patch (`data-preserve-attr="open"`), the unit moving between groups included.
- **Narrow** (under 560px of its own width: a phone, a narrow drawer): one column, the rail
  and name, then state, progress and span, then the runs as a 4px bar; the head and the axis's
  labels give way. Nothing scrolls sideways.
- Words: the column names, the states' words, durations. Nothing explains how to read it.

## Motion

One moment, the work moving: the sweep along a running cell's foot (Sluice's: 3.2s, ease-out,
the bright sky crossing the blue; a theme sets its duration, easing and shape, or none), and a
plan row's live dot breathing (2.4s) while its unit was heard from in the last five minutes. A
row's chevron turns as it opens (0.15s). A step view's now line
moves with the clock in steps of five seconds, never animated. Everything else is still. Under reduced motion the sweep is a
static line, the dot is still and nothing turns.

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
  lives in the store and the cookies, never in the browser. A component never draws content.
- **Patch elements, fat morph** ([Tao: Patch Elements](https://data-star.dev/guide/the_tao_of_datastar),
  [SSE events: datastar-patch-elements](https://data-star.dev/reference/sse_events)).
  A page's stream (`streams::page_events`) renders the page again and sends each region that
  changed (`#id`, mode `outer`, the default), so the morph keeps focus, scroll and open folds;
  it never sends a fragment built for the patch. A quiet page sends nothing.
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
  moves it) with exactly one of each.

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
| Page head | `ui/head.rs` (`page_head`, `page_head_with`, `page_head_html`, `page_head_note`) | a page's crumbs, name in the title style, its Details' ⋯, its sentence or muted note |
| Status line | `ui/head.rs` (`Dot`, `status_line`) | how a thing stands: its state's glyph (the coral dot when it asks the owner) and one muted sentence |
| Section head | `ui/section.rs` (`SectionHead`, `section_head`) | a region's name at 18px/600, its count, a note, one action |
| Tabs | `ui/tabs.rs` (`Tab`, `tabs_open`/`close`, `panel_open`/`close`, `switch`) | pill tabs over panels; the view switch and the messages' switch as pill links |
| Buttons | `ui/button.rs` (`Btn`, `submit`, `link`) | Primary (the next move, filled navy), Outlined, Quiet, Danger, Icon (44px, a plain glyph); one height, 32px (44px on a phone), a quiet text button its line's on a desktop; the filled one is the page's next move, so a group's "Retry all" is filled and "Set all aside" (one wording everywhere) outlined |
| Pills | `ui/pill.rs` (`Tone`, `pill`, `pill_link`, `ask_link`) | a fact set apart in its state's tone: plain, muted, run (soft blue), look, quiet (sand), done; `ask_link` the way to an open question, its coral dot and "Waiting on your answer" (never a coral pill) |
| Needs you | `ui/need.rs` (`Mark`, `need_line`) | an item a line: its mark (the coral dot for a question, else its state's glyph from the status table), its sentence, its one action, its × |
| Question | `ui/question.rs` (`QuestionCard`, `answer_link`, `way_on`); `threads::MessageItem::question_card`; `templates/answer.html` | a question to the owner, one way everywhere: as a line it is `need_line` with `Mark::Ask`; as a card, its coral dot and "Question for you", who asked and when, its title (when it has one of its own), its words whole, one filled Answer that unfolds its form and Close question behind the page's confirmation (`ui::close_form`, a `Confirm`: a close reaches the asker at once and the store cannot reopen one, so it is confirmed, never offered as an Undo) |
| Cards and wells | `ui/card.rs` (`well_open`, `well_close`, `message_card`) | a well (plain, look, ask); a message as a card: who to whom, when, its words, the way to the whole |
| Tool calls | `ui/calls.rs` (`Call`, `calls`) | a turn's calls in a quiet `<details>`: the tool, its key argument on one line, how long or that it failed |
| Menus and Details | `ui/menu.rs` (`Details`); `ui.rs` (`fold`) | the ⋯ with what identifies a thing, then its actions; a plain fold |
| Empty and notice | `ui/empty.rs` (`empty`, `empty_with`, `notice`) | one line where a blank would look broken; a notice of what was not done |
| The grid | `ui/grid.rs` (`grid_open`, `module_open*`, `column_open`, `Swell`, `stage_strip`, `summary_sentence`, `ValueSet`, `latest_list`, `clock`) | the sheet, modules, the stage strip, the summary sentence, a value by its shape |
| Status and time | `ui.rs` (`status`, `glyph`, `mark`, `word`, `tries`, `duration`, `ago`, `since`, `at`, `count`, `tally`, `progress`, `bar`) | a state's glyph and word, durations and clock times as `<time>` the clock ticks |
| Who | `ui.rs` (`Party`, `StepRef`, `party_html`) | a sender or recipient as the identity it is |
| Fields | `ui.rs` (`head`, `field`, `field_start`, `field_end`); `templates/kit.html` | a form's row: its label, its control, its error |
| Confirmation | `ui.rs` (`Confirm`) in a `sluice-confirm` | the one native `<dialog>` |
| Conversation | `threads::Conversation` (`cards`, rows, the composer) | a thread's messages and its box |

The Rocket components (`ui::rocket::COMPONENTS`; the hosts' helpers are in `ui/rocket.rs`), and
why each is script rather than Datastar attributes:

| Tag | Script | What it does | Why Datastar can't |
|---|---|---|---|
| `sluice-tabs` | `components.js` | an ARIA tablist over its panels: arrows wrap, Home and End; the choice in `$$tab` and the page's `tab` signal, its `current` kept through a patch; an anchor in a panel opens its tab | a tablist's roving focus and an anchor's tab are imperative focus and hash work across many elements |
| `sluice-fold` | `components.js` | Show all and Show less over a long text, said only when it is cut; or a More fold kept open per project or opened on a wide screen | whether a text is cut is a layout measurement, and the fold's memory is this browser's |
| `sluice-confirm` | `components.js` | its summary opens the page's one dialog with its form; disabled while its summary is `aria-disabled` | moving a form into a modal `<dialog>` and back, and `showModal`, are DOM calls |
| `sluice-dialog` | `components.js` | the page's one confirmation dialog: keep closes it, Tab stays in it, a write taken from its form (its answer leaving no alert) closes it, closing puts the form back and the focus on its opener; a form in it runs its `@post` in its host | a focus trap and focus return are imperative; closing a modal `<dialog>` is a method call no patch makes; and a form moved out of its own host needs a host to run its actions in |
| `sluice-conversation` | `components.js` | Jump to latest, and a thread's page opening at its end | scrolling to and focusing a message is imperative |
| `sluice-answer` | `components.js` | a question's Answer opens its box, drawn by `openui.js` from the question's OpenUI program; once answered the question gives way at once to the line that says so | the OpenUI program is parsed and drawn in the browser, and the answered line takes the focus |
| `sluice-menu` | `components.js` | the project switcher, display preferences and every ⋯ and ⓘ: a click elsewhere or Escape closes it and gives the focus back, its button says `aria-expanded`, the arrows, Home and End move through it, a sub-menu opens one level at a time, the panel kept inside the window | a click elsewhere, focus return, a menu's keys and fitting the panel to the window are imperative |
| `sluice-copy` | `components.js` | an id or a SHA in data mono with a copy button ("Copied") | the clipboard is a browser API |
| `sluice-search` | `components.js` | the board's search and the functions' find: what does not match hides as one types, a status counts what is left | filtering hundreds of items as one types, with no request, would put an expression on every item |
| `sluice-banner` | `components.js` | the stream line ("Updates paused. Reconnecting…", "Updates stopped at 14:02." with Reconnect) and the build line ("sluice was updated · Reload"); the stream's phase also as `html[data-stream]` | Datastar reports its stream's life as `datastar-fetch` events and draws nothing from them; the line, its time and the reopening on online or focus are the banner's |
| `sluice-splitter` | `components.js` | the board's splitter: drag, arrows, its width kept per project | a pointer drag and a width kept in this browser |
| `sluice-keys` | `components.js` | `/` finds, `[` and `]` move the drawer, `g` then a letter goes to a page, `?` opens Keyboard shortcuts; never while typing or under a dialog | a two-key sequence and its exceptions over the whole page |
| `sluice-steps` | `components.js` | the step view: a row's click opens its step, the arrows move between rows, the selected row in `selected`; its now line and running bars move on the clock's tick | roving focus, and marks placed by time on each tick |
| `sluice-day` | `components.js` | the day line: its now rule and running bars move on the clock's tick | marks placed by time on each tick, measured against the window |
| `sluice-drawer` | `sluice.js` | the step drawer: `#step:<id>` opens it beside the plan (over it below 1200px, a modal dialog; a sheet on a phone) and binds the step's stream; it follows the browser's own `hashchange` (no `pushState`, no event of its own): its close is a plain link to `#`, the scrim and Escape set `location.hash` from `data-on` (`data-on:keydown__window`), and a step link's click sets the hash | `inert` behind a modal, its focus trap, `[` and `]` (the plan's order as drawn, read from its links) and the stream binding it swaps per step |
| `sluice-board` | `board.js` | a project's page: the view switch at once (kept per project), the board column's height and its "More below" cue, a board Button's say sent in place, a step's new state said once in `#announce`, and after a form with `data-focus-after` is taken the focus on that element once a patch draws it | switching views without a load, fitting a column to the window, announcing changes and moving the focus |

Beside the components, one piece of script belongs to every page rather than a part:
`nav.js`'s clock and title (below: a time that ticks in the reader's own words and zone is
not something an attribute draws, and a hidden tab's title is fetched while its stream rests),
and nothing else: a write sluice never answered is said by its own form's `.catch` (Writes).

**The clock is infrastructure, not a component.** `nav.js` ticks every `<time>` (`data-since`
a two-unit duration, `data-ago` "12m ago", `data-clock` a time of day on the reader's own clock,
which the server writes in UTC and names, "17:49 UTC", so it reads right before the clock first ticks) every five
seconds and after a patch, then dispatches `sluice-tick` on `window` with the time; a component
that draws against now (`sluice-steps`, `sluice-day`) moves its own marks on that event. The
server writes each `<time>` as it read the clock, and its text is left out of the stream's
version, so a quiet page sends nothing. `nav.js` also keeps the tab's title and favicon (the
plan's glance) and a first open's theme.

**`/_ui`** ("States and parts") is the gallery and its documentation: each part led by what the
owner sees it as and how it is built, every helper and component in every state with the
invented project's data, light and dark side by side (stacked under 900px, and at every width
for a part that needs the grid's room), and a table of every component, its props and events.
A new part goes there first. Its sections: Status, Page head, Step head, Details, Finished
last, Summary sentence, Module grid, Modules, Section heads, Plan briefing, Stage strip, Stats,
Step view, Pills, Buttons, Status line, Tool calls, Message cards, Tabs, Cards and wells,
Fields, Folds, Conversation, Empty states, Confirmation, Menus, Copy, Settings, Search,
Banners, Notice, Keys, Splitter, Day line, Project page, Themes and the Components table. The
two page components, `sluice-board` and `sluice-drawer`, are one a page and bind the page's
address and its streams, so the gallery draws their parts (the view switch in each state; the
step head that tops the drawer) rather than a second live instance; their live states (beside,
over, a sheet; each view) are held by the Chromium tests (`board_browser`, `live_path`).

### The audit

Where the frontend departed from the practices, and what was done:

| Departure | Resolution |
|---|---|
| `board.js` was a page script: listeners on `document` added at load, never removed, and state in module variables, so a morph that replaced the project page left stale handlers. | It is `rocket("sluice-board")` on a `<sluice-board id="project-board">` host the template draws; every listener goes through one `AbortController`, observers are disconnected and frames cancelled in `cleanup`. In `COMPONENTS`. |
| `nav.js` drew the day line's now rule and the step view's now line and running bars itself, reaching into components' markup on every tick. | `nav.js` only ticks `<time>` and dispatches `sluice-tick`; the day line is a `sluice-day` component and `sluice-steps` moves its own marks. Both in `COMPONENTS` and `/_ui`. |
| The drawer published `window.sluiceStream`, `window.sluiceClose` and `window.sluiceStepController` (its stream's controller, built into a `data-init` string), and `sluice-search` rewrote the `@get('…'` in `main[data-init]` with a regex to add the find. | Deleted. The drawer's stream is an element of its own whose binding a new step replaces (Datastar ends the old: `requestCancellation: 'cleanup'`); the find is the `q` signal, sent with the plan's one stream address (Practices: stream addresses). |
| The step view's head, Now and messages were drawn ad hoc per page (triple rule, counter tiles, an 850 title). | Built from helpers: `page_head`, `status_line`, `ui::tabs`, `calls`, `message_card`; Cancel and the log moved into the ⋯. |
| Section heads, pills, buttons and wells each had two or three spellings (`.tag`, `.chip`, inline `font-weight: 850`, square boxes). | One helper each (`SectionHead`, `pill`, `button::submit`/`link`, `well_open`); the duplicates are deleted, and every corner reads a radius token. |
| `components.js` kept one `submit` listener on `document` that sent every owner form with `fetch`, disabling its buttons and drawing its errors by hand, beside a plain post and redirect for a page without script. | Deleted, with the redirects and the notices kept for them: every form is a Datastar `@post` (`contentType: 'form'`), its busy state `data-indicator`, its answer the form's notice patched by the server (Writes). |
| History was written in five places (a tab's `?tab=`, the find, the drawer). | Only the drawer's `#step:<id>` is left, as navigation (Practices: history); a tab and the find are signals. |
| A page was kept whole without script: a second way for each write, a theme form with Save, tests and screenshots of the scriptless page. | Retired: the dashboard follows Datastar's own recommendations instead (Practices); the scriptless twins, their tests and screenshots are deleted. |
| `nav.js`'s clock and title. | Kept as infrastructure (above): they belong to every page, not to a part. |

The confirmation dialogs (Cancel, a succeeded step's Retry, Delete project, Clear board, Remove a
resource, Close all) use one native `<dialog>` (`ui::rocket::confirmation`, in a `sluice-dialog`), opened by `ui::Confirm` in a `sluice-confirm`: on
`paper-raised` on `--radius-surface` with the lift and the scrim, its title naming what it acts
on in words (a step by its stage and title, never its id). Focus starts on the keep button,
stays in the dialog, Escape closes it and focus returns to the opener (or, when the opener sat
in a menu that closed, to that menu's button). While it is in the dialog the form answers as
`confirming` (its notice `confirming-said`), since a patch may draw its details a new form under
its own id meanwhile; closing gives it its id back, or keeps the newer form.

**Icons**: Lucide (lucide-static 1.52.0, ISC), the published SVG unmodified in `assets/icons/`,
each a `<symbol>` in the page's one sprite and drawn as a `<use>` of it in `currentColor`,
`aria-hidden`. A new icon is fetched from Lucide at that version, never drawn; the mark and
favicon are the owner's SVGs, served as they are.

## Pages

Every page is the frame (the nav bar, display preferences, the banners: `templates/layout.html`,
`render_framed`) around a page head and the page's own regions, each region a patch target of
the page's stream. A lane that redraws a page writes its notes here.

### The inventory

| Page | Route | Drawn by | Components and helpers |
|---|---|---|---|
| Home | `/` (its stream `/stream`; `/title` the tab title alone) | `views::home` | page head and summary sentence, section heads, For you as Needs-you lines, Today, a card a project, pills, the index table |
| A project's plan | `/projects/id/<p>` (`?view=plan`, `both`, `board`; `/stream`; posts `/needs`, `/seen`) | `views::board::project_page`, `views::plan`, `views::panel` (the board), `templates/project.html` | `sluice-board`, page head, pill tabs and the view switch, the Needs you well, Finished cards, At work rows with `sluice-steps`, pills, buttons, `sluice-search` (Find), `sluice-splitter`, `sluice-drawer`, `sluice-keys`, `sluice-menu`, `sluice-confirm` |
| A step | `/projects/id/<p>/steps/<s>` (`/stream`; posts `/actions`), and in the drawer | `views::step`, `templates/step.html`, `step_band.html`, `activity.html` | the step head (page head, status line, stage strip, buttons, the ⋯ with its actions), `sluice-tabs`, Now's well with the tool calls fold, asking and stopped modules, message cards, `sluice-steps` (Runs), field tables, `sluice-fold`, `sluice-confirm`, the message box (`ui::post_form`) |
| A unit | `/projects/id/<p>/units/<u>` (`/stream`) | `views::unit_page`, `templates/unit_page.html` | page head, status line, `sluice-steps`, asking and stopped modules, `sluice-answer` |
| A run's file | `/projects/id/<p>/runs/<r>/files/<name>` | `views::step::run_file_page` | page head, the file on a cell-radius card in data mono |
| Day | `/day`, `/projects/id/<p>/day` (`/stream`) | `views::day` | page head and note, `sluice-day`, the timetable with its pills and folds |
| Stats | `/projects/id/<p>/stats` (`/stream`) | `views::stats` | page head and note, the window switch (`ui::switch`), section heads, tables, the throughput chart |
| For you, Questions, History | `/inbox`, `/questions`, `/history`, and each under `/projects/id/<p>/` (`/stream`; posts `/messages`, `/messages/read`, `/messages/close`, a reply) | `views::inbox`, `views::threads`, `templates/inbox.html`, `history.html`, `conversation.html`, `answer.html` | page head, the messages' switch, question cards (`sluice-answer`), section heads, cards, `sluice-conversation`, `sluice-confirm` (Close all) |
| A thread | `/projects/id/<p>/thread?thread=<t>` | `views::inbox`, `templates/thread.html` | page head with Details, a conversation's rows, `sluice-conversation`, the message box (`ui::post_form`), `sluice-answer` |
| Log | `/log`, `/projects/id/<p>/log` (`/stream`) | `views::log`, `templates/log.html` | page head and note, the presets (`ui::switch`), `sluice-search` filters, a section head, the records table, `sluice-copy` |
| Functions | `/fns` (`?project=`; `/fns/stream`) | `views::home`, `templates/functions.html` | page head and note, `sluice-search`, section heads, a card a fn |
| Settings | `/projects/id/<p>/settings` (`/stream`; posts its fields, `/board`, `/icon`, `/delete`) | `crate::settings`, `views::project_settings`, `templates/settings.html` | page head, the sections' index, section heads, field rows, switches, the danger well, `sluice-confirm` |
| States and parts | `/_ui` | `views::gallery`, `templates/gallery.html` | every part and component, light and dark |
| Agent docs | `/docs`, `/docs/<topic>` | `views::gallery::agent_docs` | page head, a card a topic, the topic at the measure, the index sticky beside it |
| A page that cannot be drawn | any address a browser asks for that answers 4xx | `views::missing`, `templates/missing.html` | page head and note, the ways on |
| The frame, on every page | | `templates/layout.html`, `views::prefs` | the nav bar, the project switcher and display preferences (`sluice-menu`, `sluice-keys`), `sluice-banner`, the confirmation (`sluice-dialog`) |

### Per page

- **A project's plan** (`/projects/id/<p>`, `views::plan`): a briefing over a roster, on its
  own visual system (The briefing, below). The head: the project's name at 22px/650 with its
  Details, then one sentence from the status table's counts, the silences and the open
  questions, each state in its own word ("1 question for you, 2 failed, 1 quiet. 10 at work;
  73 of 92 done."; "3 at work, all heard from recently." when nothing needs the owner), the
  question linked under the coral underline; what the owner set aside still counts there
  (setting aside is about attention, never status). The project's sections are soft pills
  under it (the current one on `paper-2` at weight 600), one line that scrolls within itself
  on a phone; the row's tools (Find, the switch) beside them. One ⋯, the head's Details: the
  project's id, its words, Settings and Export (the plan as Mermaid text). On a phone Find is
  a 44px icon by that ⋯ that opens across the head as it takes the focus (or holds a find),
  never a row of its own. The page keeps the frame's centred column; three sections, each
  headed by an 18px/600 label (body text is 15px):
  - **Needs you**: the one raised well (`paper-2`, 14px), an item a line: its mark (a 12px
    coral dot a question, outranking an 8px sand square a stop and an 8px sand ring a silence,
    said in words to a screen reader), its sentence (at most 90ch; the unit's title in weight 600 linking the
    step, what happened, its owner small and muted; under it the failure's headline, what it
    last said and the advice, muted), its one action (a pill: Answer, Retry, Retry with
    feedback, Retry all, Message it) and its × (a 32px circle, 44px on a phone). Only the first
    item's action is filled navy; the rest are outlined. A cancel has two: Retry and "Settle
    as done" (outlined; its step's Dismiss, which settles its unit as done), and its × only sets
    it aside, like every ×. Questions first, then stops (a shared
    cause said once, its units folded in a `<details>`, Retry all), then silences, the longest
    first. A shared cause is said in the head's word, its advice in the plural ("Two units
    failed: they hit their wall-clock cap. Raise the agents' time limit (SLUICE_AGENT_MAX_MIN
    in the project's .env), then Retry all."). A cancel stays here: a cancelled step leaves its
    unit unfinished and holds the steps after it, and the status table counts it as needing a
    look everywhere, so the owner still decides: Retry, or Settle as done. Setting aside (the ×,
    Set all aside) is about attention only and never settles. A count and "Set all aside"
    stand in the head, said the same in every project; it takes the stops and silences only: a
    question keeps its own ×, so the head and the well never disagree about it. The well's one
    filled action is a question's Answer; Retry all, Retry, Settle as done and Message it are
    outlined everywhere. An item set aside leaves a line under the well for ten minutes,
    "Set aside: the silence of <title>" and Undo, and comes back by itself when it changes; with
    script the × and Undo post in place, the focus going to Undo and back to the item. With
    nothing to say, no well: one muted line, "Nothing needs you. 4 set aside.". Each Set aside
    line reads as one sentence with its Undo after it, wrapping as text does at any width.
  - **At work**: the roster, under an axis row (12h, 4h, 1h, 15m, now, 12px over the tracks'
    column; it says its scale to a screen reader and on hover: "The last 12 hours on a log
    scale, now at the right; a tick is a message."). Its head carries an ⓘ, a 32px round
    button (44px on a phone) whose popover, the Details' look, says how to read a track: the
    last 12 hours on a log scale, now at the right; blue the run going now, pale an earlier run,
    sand a quiet one, dotted a wait; a tick a message. A group a recipe (a `<details>`, its
    triangle, its name at 15px/600 in ink, so a big plan scans by its groups, and its counts
    13px muted: "1 quiet, 7 running, 2 waiting"); the units of no recipe after ("Other
    units"), the long runs last. **Folding, by row**: in a roster of more than 12 rows a
    group shows its remarkable rows first (a silence, a wait of an hour or more since its last
    run, a unit with an open item in Needs you) and folds the rest, when more than 3, to one
    line under them, "+7 running · each heard from in the last 11m", a `<details>` that opens
    in place; a group with nothing remarkable folds whole to its own line,
    "article · 23 running · each heard from in the last 27m". A stop is in Needs you, not a
    row, so it keeps no healthy sibling in sight. A row (8px,
    `paper-2` on hover and open) is a grid: the badge (a 52px chip, an engine's name; drawn
    only when rows run on more than one thing, never "fn"), its title in ink at weight 600 with
    its owner muted and under it its recipe's view or its last message on one line, muted (who
    said it, never when); its meta one statement about time, two lines at most: the stage pill
    (`ui::status_pill`: the state's glyph, the stage and how long it has run, in the state's
    tone), then "heard 12m ago" when
    it was heard from since its run began (in `ink` from 15 minutes, at weight 600 from 30, so
    a silence shows before the quiet pill says it; the sand stays for quiet), else "no word
    yet" muted; once quiet, the one pill turns sand and says how long it has been silent
    ("scan · quiet 3h"); its
    track (its last twelve hours on a log scale, `clamp(180px, 34cqi, 560px)` wide, so a wide
    screen gives its room to the time rather than to a gap after the title); the live dot. Waiting rows follow the running
    ones. A line ends it: "4 waiting on earlier work · 25 done, the last 2h ago". The head
    counts as the summary sentence does: a blocked, held or queued unit waits, so "At work · 2
    units · 3 waiting", or "Nothing at work · 3 waiting". Under 760px
    of the pane the row stacks: badge and title, then the meta, then the track full width; the
    long axis gives way to "12h … now" over each group's first track.
    **The track's tones**, each at least 3:1 on the paper in both schemes: a run going
    `--track-run` (the blue light, `run-ink` dark, where the blue would be 2.7:1); a done run
    `sky-ink` at 45% on the paper edged in `sky-ink`; a silence the sand edged in `sand-mark`
    (light) or filled sand (dark); a wait dotted; a message a 1px tick.
    **The drilldown**: open, a row's step view (The step view) on the row's own scale, the
    last twelve hours on the same log scale across exactly the row's track (the same x-range,
    measured, at every width), in the track's tones: a run still going reads as its step does
    now (a quiet one on the sand from when it fell silent), one that began earlier starts at the
    left edge, saying "from 3d ago". Its "Runs on" column only when its steps differ, its
    Progress only when one reports any. Its foot: "Last heard 22:51 (6m ago)" ("Not heard from
    since 19:14 (3h ago)" for a quiet unit; the row already says what it said last), then
    Message and Unit page as quiet links and Open step, its one outlined move.
  - **Finished since you looked**: a card a unit that produced something (10px, a hairline
    inset, filling their row, one under 760px, each as tall as its own content). A card shows
    when it finished and how long it took, then the title in weight 600 linking the unit. Next
    comes what it produced, led by the values `ui::ValueSet` picks out: a value that
    identifies it (a sha, as code) or says it in a word (a count of files, in ink), each named
    small and muted, at most three on one line. Under them is the output that explains it (its
    summary) or else its last message, cut to 160 characters. The units that left neither
    share one muted line after the cards: "3 more finished with nothing to say:" and their
    titles as links. "since 14:02" and "Mark as seen" sit in the head; with no look kept, the
    head says "Finished lately" and the section holds the latest four. Marked seen with nothing
    finished since, the section is one muted line, "Seen through 04:21 · Undo"; Undo takes the
    look back.
  - **Done**, asked for (Show: Done, a find, a recipe's units): the index, a dense list newest
    first in columns: finish time, glyph, title, how long it took (in `ink-muted`, like every
    finished duration: the blue is for what runs).
  Every section is a patch region, and every item and row one inside it, so a change patches
  the item or row it moved and nothing else; a step whose state moves on is said once to a
  screen reader. The step drawer opens beside the plan from any step link, on paper, a hairline at its edge, lifted off the plan. **The glance**: the plan's tab
  title is "(1) ⚠4 · almanac · sluice" (the open questions in brackets, a warning sign and the
  steps needing a look not set aside, never a cross that reads as "close", the name) and its favicon wears a coral dot while a question waits, a sand
  square while a step needs a look (`favicon-ask.svg`, `favicon-look.svg`).
- **The briefing** (the plan's visual system, now the whole frontend's): the well on
  `--radius-surface`, a card or the index on `--radius-module`, a row on `--radius-row`, a pill
  on `--radius-chip`; one raised well (Needs you) and nothing else lifted;
  hairlines (`rule`, `rule-faint`) for the rest; two weights, 600 and 400 (the name 650). The
  palette's roles hold: coral only for a question, sand for a look, the blue only for what
  runs, sky for done. It reads in every theme, light and dark, from the same tokens; a theme's
  `--radius-*` reshape it as they do every page. No line explains how to read it.
- **A step's page** (`/projects/id/<p>/steps/<s>`): under the row, its head (`StepView::band`,
  `templates/step_band.html`, the gallery's Step head): its way back to its plan and its unit,
  then its stage muted before its title in the title style, two lines at most with the whole in
  its tooltip, and beside it its one ⋯ (its Details: its id, unit, recipe, fn, run and tags;
  then its actions: Pause or Unpause, Cancel, a failure's Set it aside, its log, Message it),
  next to the close × in the drawer (a plain 44px icon). Under the title its status line: its
  state's glyph (the coral dot while it asks the owner) and one muted sentence ("Running 6h 34m
  · heard 2m ago · run 2, after 1 failed"; "Failed 1h 42m ago · after 18m"; "N awaiting reply"
  when its messages wait). Each part but the last ends with its "·", so a wrapped line never
  starts with one. Then its words,
  its unit's stage strip (its own stage ringed) and its moves, the next one filled: Retry with
  feedback (its box folded under it, Retry beside it; on a failure that repeated it leads to the
  box standing open and the bare Retry is quiet), a cancel's quiet Dismiss. Why it failed reads
  in order: its headline, the words its run said and its last failed call, then the advice, then
  the feedback box when a failure repeated. Its tabs follow as pill tabs (`sluice-tabs`).
  Overview is modules on the sheet: the question to the owner first as the question card,
  answered in place; then why it failed in the sand well, or **Now**: the latest sentence
  the agent said at lead size (17px), then its calls in a quiet fold (`ui::calls`: "12 calls ·
  1 failed", opening to a line each, the tool, its key argument and how long it took, the latest
  first); under Now on a running step, **Its brief** (its prompt's first paragraph, cut at about
  280 characters, "Whole brief" opening Inputs) and **So far** (the outputs it has set, an earlier
  run's said so and muted, then "Not set yet:" and each unset output by name with its type,
  "All outputs" opening Outputs); beside
  them, as titled modules, what it waits on (or when it starts), "Runs after" (what it comes
  after), its
  progress and, once it has stopped running, its key output. The columns stand one over the other on a narrow
  sheet (the drawer, a phone, to 900px of sheet), and at seven and five twelfths from there.
  Activity is its turns as rows numbered in data mono; Messages its conversation as message
  cards (who to whom, when, the first words cut with the way to the whole; an answered question
  whole); Inputs and Outputs field tables with where each came from; Runs its unit's step view,
  its own row ringed, over its runs. In the drawer the same head tops the step. No counter
  tiles, no rules between the head's parts.
- **A unit's page** (`/projects/id/<p>/units/<u>`): under the row, its head: its way back,
  its glyph and title with its Details (its id, recipe and params), how its steps stand, a short
  recipe view on that line, its owner. On the sheet its recipe's view whole when it says more than a line,
  then Steps: its step view across the sheet (The step view), and under it a module for each
  step that needs the owner, in the view's order: the one asking as the question card (Answer the way to its
  step, Close question in place), a stop in the sand well with why it stopped, each headed by its stage with its Details.
- **The inbox, Questions and a thread**: the head names the page (a thread its step's title,
  its way back over it and its Details); the messages' switch is under the name. Questions to
  the owner
  come first as question cards, answered in place: the card's head is its kind and who asks,
  its ⋯ in the head's corner; Answer filled and Close a quiet button beside it, at the
  one button height (32px, 44px on a phone), an answer's choices never stretched to a bar (on
  a phone a whole-width card); an answered or closed card is replaced whole by the one line
  that says so ("Closed just now"), drawn once, one a row to 900px of sheet, two from there; then those between agents (a step's latest message titled "Latest from the agent"), the ones nobody waits on and the unread notes
  (each its thread's name and project, its notes, then Mark read after them). A thread's page
  keeps its thread name and project in its Details.
  A conversation's messages are rows apart by hairlines, each with its Details (its id, its
  thread, what it replies to), an open question to the owner as the question card with its reply
  under it, the message box under a hairline.
- **Home** (`/`, `views::home`): the projects as cards, built from the plan's
  parts. The head is "All projects" and one sentence of counts alone: "3 questions for you,
  2 failed, 1 quiet; 63 running in 4 projects." Which project is each card's to say. "Runner
  stopped" sits under it on the sand, without a rule. For you (eight columns) is the Needs-you
  list, a line an open question: its coral dot, its project and who asked, its title linking
  where it is answered, Answer and the ×. An answered one becomes the line that says so. Today
  (four columns) is a project's last day in a line and the way to the day; with no question,
  For you is one line and Today a row across. Then Projects, ordered by what needs the owner:
  a card a project (`.pm.card`, as many to a row as fit 460px each, each as tall as what it
  says). A card holds:
  - its name and icon in the title face at 20px/26px, its state's glyph and size, its words muted;
  - its plan's sentence at body size, which counts what needs the owner once;
  - At work's groups, a line each, as the plan's group lines read, with its remarkable units
    named;
  - "Finished lately:" and the last three by name, each its unit's link (the plan says what
    they produced);
  - the way to its plan and its day.
  No table repeats the cards' counts: under them, a muted line names the projects with no
  steps yet and another the archived ones (with their steps counted). Its links and Today's
  are 44px targets on a phone.
- **Day** (`/day`, `/projects/id/<p>/day`, `views::day`): the day as a timetable. The head:
  "Day", its note the date. The day line under the day's sentence: a
  row a project, its name in the first 132px, its runs as 5px
  bars on 7px tracks (sky done, blue running, sand-ink needing a look), the hours along the top
  in data mono, the rule at now in the run blue; quick successes are counted, not drawn. The
  timetable: a 64px hour column (the hour in data mono at 18px/600, its day where it changes, its runs counted),
  then a column a project; each run a line, three columns: its minute in mono (never wrapped),
  its unit's title in ink and its stage muted (two lines at most; its step and run in the
  link's title), then at the right its status pill (`ui::status_pill`): its duration on the
  sky with the check, its word and time on the sand when it needs a look, or "running 43m" on
  the soft blue, never wrapped. A run still going
  reads as its step does in the status table, so a quiet one says "quiet 3h" on the sand; a line is at most
  480px, so its outcome stays by its name on a wide screen. Under 400px of frame the title takes
  a line of its own under the minute and outcome. A busy hour (more than six in a cell) lists
  only its notable runs (not a success, still running, or an hour or longer) and folds the successes behind a muted "+23 more succeeded, 1m to 12m" that opens
  in place; past eight listed, what still runs folds the same way behind "+14 more running",
  every look kept in sight; four projects or more stack under each hour below
  1100px of content, every project below 640px. Two or more hours in a row in which no run started are one
  line, the hours in data mono and "· nothing ran" muted ("06–17 · nothing ran"). A cell's runs
  flow into columns at least 400px wide, as many as the frame's width allows (a line still at
  most 480px), so one project's day fills the frame. The current hour ends on the now rule and
  says what runs. `nav.js` keeps the reader's zone in a cookie and `sluice-day` moves the
  rule and the running bars on the clock's tick; the page's version is its runs, so it patches only when they
  change.
- **Stats** (`/projects/id/<p>/stats`, `views::stats`): a project's numbers over a window, and
  the one place a stage's usual time is said. The head: "Stats", its note how many runs ended
  and units finished in the window; in the row the window
  switch (24 hours, 7 days, the default, 30 days, All), the view-switch bar of links
  (`?window=`). On the paper four sections, each under its section head: **Durations by stage**, a table a recipe in the project's recipe order (its
  name, how many units and runs) with a row a stage in the recipe's order: how many of its runs
  succeeded ("Succeeded") and their median, p90 and longest in data mono, then, under "median ·
  p90", the median drawn as a sky-ink bar inside its p90 in pale sky with a sky-ink edge, on
  one scale for every table (minutes on a log to the longest p90 of any stage, as the plan's
  track reads time), so a bar is the same time in each; the
  units of no recipe and the loose steps last, "Without a recipe", a row a step (the busiest
  first). A unit the plan has retired goes with the recipe whose stages hold every stage it ran
  with the fewest left over. **Outcomes**, the same tables: succeeded, failed, cancelled,
  retried (a second or later try of its step), the failure rate (in `sand-ink` when any
  failed) and the failures by kind in words, from the failure classification. **Throughput**:
  a sentence (finished, started, the busiest day) over a column a day (an hour for 24 hours, a
  week for a long All) in sky-ink, the busiest in navy, its largest count and its first and
  last dates in data mono, every count in a table for a screen reader. **The slowest runs**:
  the ten longest that ended in the window, a row each: the duration in data mono, the unit's
  title and its stage muted (linked to the step's runs, or its log once retired), its word on
  the sand when it did not succeed, when it ended, and its ids behind its ⋯. Tables have fixed
  columns so a recipe's lines up under the one before; under 760px of frame a row is a block,
  its stage then its numbers in labelled cells. The page's version is what it draws (it reads
  no clock but the window's edges), so its stream patches only when the runs change.
- **Log** (`views::log`): the head says "Log" and, as its note, what the page shows ("42
  records on this page; the newest …"). The log is the records whole, so it keeps each record's
  seq and names its step by title with its id after it; under the head the presets as one
  view-switch bar beside the filters (Kinds a closed fold whose list opens as a popover over
  the records, in the flow on a phone), the note on what is left out, then Records under its
  section head, the table under a hairline with seq, time and kind in data mono. A
  message row is its sender and recipient as name links, then its words in plain muted text and
  one small "thread" link after them.
- **Functions** (`/fns`): the head says "Functions" and counts them in its note; the picker and
  the
  find on one bar, the groups' index, then each group under its section head, its
  entries on the grid as many to a row as fit 400px, each under a hairline (a broken one saying its problem first under a sand "cannot be loaded" status pill), its name in data mono; the note counts them ("1 cannot be loaded").
- **Settings** (`/projects/id/<p>/settings`): the head says "Settings"; how the project stands
  (active, paused or archived; its resources; when done units retire) heads its sections. The
  sections' index stands sticky in a quarter of the width, at most 300px (a scrolling row of
  tabs on a phone), each section beside it under its section head, each field a row (its
  name, then its field at 78ch, the board's program as wide as the rest); Delete sits in a sand
  well. Each section is one form with one Save at its foot ("Save details", "Save resources",
  "Save board", "Save retiring") and its notice under it, "Saved." or why not; Activity is
  its Pause or Resume. Details: name, description (its preview drawn as it is typed, folded
  under "Show how it reads"), icon (letters or an emoji, or an image chosen through a pill,
  "Choose an image", that names the file chosen) and Fallback (a select of the project's
  sessions and "none"); then, while any session is registered, Sessions: one line, each its
  `@name` in ink and "seen 2m ago" muted. Resources: each resource's capacity (a number or the fn
  that reports it), a new one, and Remove behind a confirmation. A Save never writes over
  another author: it is refused when one of its section's fields changed after the page was
  drawn, saying which and by whom, and what was typed stays. The stream redraws the sections'
  standing (the live header, resources, the board's document, sessions, retiring, activity and
  the delete guard), never a form being edited. (`settings.css` is folded into `style.css`.)
- **History** (`/history`): the third of the message views, its head its name as For you
  and Questions; the threads with the owner, counted in their section's head, a module each on
  the grid, as many to a row as fit
  360px; one with an open question says "Waiting on your answer" after the coral dot.
- **A page that cannot be drawn** (`views::missing`): the head says what is missing and, as
  its note, why; the paper lists the ways on, each a 48px link at 17px/600 with its arrow.
- **Agent docs** (`/docs`): the head names them; the index a row of topic modules; a topic
  its markdown at the measure in eight columns, every topic listed sticky beside it.

## Words

The UI explains nothing it shows: no captions on how to read a view, no standing hints, no
second sentences; state, labels and errors only. A field's constraint is said by the error when
it is broken, never under the field. One short line where a blank would look broken ("Nothing
waiting on you."), a banner that says the condition and what to do in the fewest words, one
advice line under a failure only when it says more than the Retry beside it. No hedging ("most
often", "usually") and no second clause on why.

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
- Do build the way Datastar recommends ([the Tao](https://data-star.dev/guide/the_tao_of_datastar),
  [attributes](https://data-star.dev/reference/attributes),
  [actions](https://data-star.dev/reference/actions), [Rocket](https://data-star.dev/reference/rocket)),
  naming the doc beside the decision; write script only where Datastar can't, and say why in
  the component table.
- Do build a part from `views::ui`; when it behaves, make it a Rocket component in
  `COMPONENTS`, shown in `/_ui` in every state, its state in preserved attributes.

### Don't:
- Don't spend coral on anything but an open question waiting on the owner (and the logo).
- Don't fill anything blue that is not running.
- Don't draw a corner but from a radius token, a weight but 400 or 600 (a title's aside), a
  heavy rule, a card inside a card or a stripe (a theme's band edge aside).
- Don't draw content from script, or keep in the browser what the server should know.
- Don't name a project's own words in view code; take them from what the project supplies.
- Don't restyle or redraw the mark; it is the owner's SVG.
- Don't add a legend; each state says itself.
