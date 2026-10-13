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
    fontSize: "26px"
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
    typography: "34px/38px 850"
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
(failed, cancelled, stale) → **Running** (quiet first, then the longest running) →
**Waiting** → **Done** (a dense index, folded). The module that needs the owner swells. A
running step shows how long it has run; how long a stage usually takes is the project's Stats
page's to say, never a row's. Selecting a unit traces it: it opens in place, its upstream and downstream
chain lights with a rail in the margin, a sentence states the chain, and the rest fades.

The old world (rounded cards on cream, three racing stripes under the nav, Archivo and Public
Sans, seven themes, a green for success and a plum for paused) is gone; nothing of its look is
kept.

## The generic rule

The dashboard knows only sluice's own concepts: project, recipe, unit, step, status (the status
table, `sluice_model::shown`), run, wait, message (owner, orchestrator, steps), progress fields,
outputs and the project's board document. Nothing in view code assumes a project's vocabulary.

- **Stage columns** come from a unit's recipe's own steps, in recipe order. A project with
  several recipes gets a block per recipe, each with its own columns. A unit of no recipe has
  its steps as its cells, in plan order, six to a line (its gates are its step view's); a
  one-step unit is one cell.
- **"Finished last"** is the units (or steps of no unit) whose run ended most recently, by
  title, with when and how long they took.
- **A long run** (a step of no recipe running six hours or more, or reporting `step_progress`) shows
  its progress fields in the margin module, as it reported them: sluice never reads or names
  them.
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
  stale, quiet. A look cell is sand with navy on it; a
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
Pine Dawn and Rosé Pine, Flexoki Light and Dark; and deep themes from classics of American
design, which also set their faces, rules and materials (the deep tokens, below): Empire Builder,
Williamsburg, Interstate, Ranger, Park Service, Kodachrome, Fiesta and Road Atlas, each Light and
Dark. A theme's id is `<family>-light` or
`<family>-dark` (`views::THEMES`): `data-theme` on `<html>` and the `sluice_theme` cookie,
server-drawn, so a page without script is drawn in it; with script the picker (display
preferences' Theme panel, The frame) applies at once (`sluice-toggle`). The picker shows each theme's swatch in its own tokens: its band over its
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

**Held by a test** (`tests/themes.rs`), which reads every theme's tokens from
the stylesheet, in its own scheme:
text at 4.5:1 (ink and ink-muted on paper, paper-2, paper-raised, sand-pale and sky; run-ink
and sand-ink on paper and paper-2; band-ink, band-muted and band-accent on the band; the text on
running, sand, coral, navy and selection fills), 3:1 for the focus ring on paper, paper-2, sky
and sand-pale and for status marks (sky-ink on paper and sky, sand-ink on sand-pale, sand-mark
on sand, idle on paper, band-mark and the coral count on the band), the chart colours 3:1 on
paper and paper-2, the question colour at least 0.1 apart in Oklab from every other role's and
every chart colour, running apart from done, and the band no lighter than the paper. For the
deep tokens it also lays the texture's ink at its alpha over the paper and holds ink and navy
on that; holds the band edge's top row 3:1 against band-ink and band-mark, an outlined done
cell's edge 3:1 on the paper and a look cell's on sand; checks every token a family sets is
known and in its allowed set; works out each theme's head faces against the display face's
floor; and, in Chromium, draws each theme in place on `/_ui`, a plan and home at 320 to 2560px
with no sideways scroll, every 44px target still 44px on a phone and no words in a display face
under 19px, holds Williamsburg's sweeps still and every other theme's moving, and lays the
picker out in one column at 390px and two at 1440px with no name wrapped.

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
(display black 900, heads 800 in capitals tracked 0.05em at 0.74 of Sluice's size) and Courier
Prime for data. An Oxford rule (3px over 1px) over each section, a tighter rhythm (density 0.86,
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
faces: Libre Caslon Display at its one weight (400) for the display and the heads, a size up
(1.08), Libre Caslon Text, its kind lines in italic, and Anonymous Pro for data, a
typewriter-cut mono whose zero is slashed, so 0 and O differ by shape at 12px (the study's
Cutive Mono told them apart by width alone). A Scotch rule
(1px over 3px) over each section and a 1px rule over a stopped module, a 5px question rule, laid
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
(800) and mixed case for the display, heads and text (its middle dot, which Overpass draws with
no advance, comes from the fallback face), and Overpass Mono for data. Rounded corners (modules
8px, cells and buttons 6px, chips 4px), a 3px rule over a stopped module, look cells in the
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
bold, heads in capitals tracked 0.07em at 0.8, and DM Mono for data. The screen print's
misregistration: a navy rule with an aqua pass 3px off it (7px room) over each section and an
offset aqua line under the band; a 3px rule over a stopped module and a 7px question rule; cells
not reached on a 1.5px line; no shadows; a 3px focus ring; 2px links at 4px; 2.25px glyphs and a
slow 4.8s ease-in-out sweep. Adjusted from the source: paper #F3EDDD from the sampled cream
#D7CCB2; running #3F6A82 (5.0:1 under cream) from the sky #48748D, its text #345A70; the rose
#9D6B6A lifted to #C9736D with night text (4.65:1, and 4.65:1 on the band) / #D98A7F.

**Park Service** (`assets/themes/parks.css`), the NPS UniGuide standards' PMS colours (UniGuide
ch. 2.4). Band PMS 5605 #22372C (`--p-uniguide-green`, also the light ink) / #141C18; navy, the
rules and the filled button, PMS 1615 brown #8D4820 (`--p-uniguide-brown`) / a lifted #E2A272;
paper #F1EEE7 / #1F2A24; idle PMS 405 #696158; running PMS 301 #004A88 / #4F8FCF; done PMS 5753
olive (sky-ink #5F6639 on #DDE2CC); the look PMS 131 #D08A00; the question PMS 485 red. Its
faces: Besley, a Clarendon, for the display (800) and heads (700), Signika for text and Red Hat
Mono for data. The UniGuide's heavy overbar, an 8px rule in the band's green, over each section;
an 8px question rule and a 3px rule over a stopped module; a 3px brown sill under the band; an
open rhythm (density 1.08, 54px cells); buttons 3px round, chips 2px; and a 3.6s sweep. Adjusted
from the source: the red #E1251D lifted to #F33C30 with dark text (4.55:1; it reads on the band
at 3.33:1) / #F0574C; the look's text #7A5100 from PMS 131; the heads at 1.06 of Sluice's size
(the study's 0.95 put Besley under 19px on a phone's step modules and a recipe's block head);
the sill starts a pixel under the nav, so its top row is the band's.

**Kodachrome** (`assets/themes/kodachrome.css`), Kodak's trade dress on the Kodachrome carton:
Kodak Yellow (PMS 1235C, #FFB700) and Kodak Red (PMS 485C, #ED0000) as Kodak publishes them,
over the carton's dark stripe #282C2F, sampled from a 1961 carton (the primitives
`--p-kodak-yellow`, `--p-kodak-red`, `--p-box-stripe`). Paper white #FBFAF6 / #1D2023, ink
#1B1D1F / #F3F1EA; navy and the band the stripe, the dark band #121416; the yellow is the look
and the band's accent and mark; the question the red. Its faces: Alfa Slab One for the display
and the heads (400, at Sluice's sizes), IBM Plex Sans Condensed for the text, as on a data
sheet, and IBM Plex Mono for data. A 4px black rule over each section and a 3px one over a
stopped module, 2px corners as a perforation's, sprocket holes along the band's foot (a 12px
edge, the paper showing through 8px in every 24px), narrow columns (0.624: the study's 78% of
0.8), and the sweep
stepped, `steps(8)` over 2s, advancing a frame at a time. Adjusted from the source: Kodak
names no blue, so running is a designed blue #1F5FBF (white on it 6.09:1), its text #1D56AD; the
yellow, Kodak's identity, is the look colour and not the band, the theme's main compromise;
the red carries black text (4.6:1) and reads on the band at 3.08:1; the dark question lifted to
#FF4D3D. Done is a designed pale blue (#DDE8F6 / #24364C, its ink #2E5F8A / #9CC0F0). The
study's heads at 0.92 would set a recipe's 19px head at 17.5px in a display face, so the heads
keep Sluice's sizes. Contrast minima: text 4.60 / 5.61, marks 3.08 / 5.23.

**Fiesta** (`assets/themes/fiesta.css`), Homer Laughlin's Fiesta ware of 1936, Frederick
Rhead's concentric rings: Cobalt #13294B, Old Ivory #F2EFDE, Lapis #2B64A1, Turquoise #56B1BD,
Yellow #DAAC49 and Poppy #F24B21, a collectors' guide's matches from photographs, unofficial
(each a primitive, `--p-cobalt` and so on). Paper Old Ivory / Cobalt #16294A, ink Cobalt / Old
Ivory; the band Cobalt, #0D1B33 in the dark, with Turquoise its mark; running Lapis; done a
Turquoise wash; the look the Yellow; the question Poppy. Its faces: Righteous for the display
and the heads, Nunito for the text and DM Mono for data (declared once, in `ranger.css`, which
Ranger shares). Three 2px rings over each section (a
10px room), 14px modules, 10px cells, buttons and chips at 14px (a chip is round at that, a
button nearly so: the allowed set stops at 14px), a roomy rhythm (density 1.1, 54px cells),
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
boundary red. Its faces: Old Standard TT, bold for the display and regular for the heads, set
in spaced capitals (0.14em) as a map's place names; Fira Sans Condensed for the text and Source
Code Pro for data. A dash-dot boundary line over each section and a dashed rule over a stopped
module, a dashed centre line under the band, a faint graticule on the paper (a line every 72px
each way, the road ink at 6%), a compact rhythm (density 0.92, 48px cells), not-reached cells
dotted as an unpaved road, and narrow columns (0.512: the study's 64% of 0.8) on a dotted grid
in Stats, with a 3.4s
ease-in-out sweep. Adjusted from the source: a two-ink map, so done (a grey-blue wash #DFE0DC
/ #323C4C, its ink #3D5B7A / #A5BFD8) and the look (#D9B25E) are designed in its spirit; the
paper lifted from the aged #DBC89B to #F4EAD2; the road blue lifted to #34509A for running
(text on it 6.75:1); the band deepened to #1E1E36 so the boundary red, lifted to #B24C38, reads
on it (3.08:1) and carries cream text (4.89:1); the dark question #E0745C. Adjusted from the
study: its heads were bold at 0.72 of Sluice's size, which puts a display face at 13.7 to
18.7px, so they keep Sluice's sizes, in the regular weight with the tracking eased from 0.16em
to 0.14em; and its text face, Barlow Semi Condensed, draws I and l, 0 and O alike at 12px, so
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
`assets/themes/`, by name, into the one stylesheet a page loads (`/static/style.css`,
`views::STYLESHEET`, what `tests/themes.rs` reads). A family's file comes after Sluice's
defaults, so what it declares wins and what it leaves out inherits. Primitives (`--p-…`, a
source's named colours) are readable only inside their own family's block; a lint in the test
fails on one anywhere else. Last, `build.rs` appends the display face's floor (below).

### The deep tokens

A theme is more than its colours: these tokens set its materials. Sluice's values are the
defaults (`:root, [data-theme]` in `style.css`), so a family that leaves one out draws it as
Sluice does, and the editor schemes are Sluice's shape in their own colours. Content, order, the
grid, sizes and the 44px targets are never a theme's: every theme shows the same page. Every
declared value must be in its allowed set (`tests/themes.rs`).

| Token | Sluice | Allowed | What it sets |
|---|---|---|---|
| `--font-display` | Schibsted Grotesk Variable | a stack ending in a generic family | the page's name, a step's, home's project names, a question's title (19px and up), section heads at 19px and up (the floor, below) |
| `--font-text` | Schibsted Grotesk Variable | the same | body, modules, tables, controls |
| `--font-mono` | JetBrains Mono Variable | the same | ids, durations, clock times, data |
| `--display-weight` | 850 | 400 to 900 | the display face's weight (a question's title 50 lighter, a theme card's name 50 heavier) |
| `--display-tracking` | -0.025em | -0.05em to 0.1em | its letter spacing (the other display sizes keep their offsets from it) |
| `--head-weight` | 850 | 400 to 900 | a section head's weight (a recipe's block head 50 lighter) |
| `--head-tracking` | -0.025em | -0.05em to 0.2em; at least 0.05em in capitals | a section head's letter spacing |
| `--head-case` | none | none, uppercase | a section head's case (tracked capitals) |
| `--head-style` | normal | normal, italic | a section head's style |
| `--head-scale` | 1 | 0.7 to 1.1 | a section head's size against Sluice's (26px, 24px on a phone, 22px a step's module, 18px in a narrow drawer, 30px Settings, 19px a recipe block), to even out cap heights |
| `--head-face-N` | worked out | never a family's | the face a head drawn at N px times the head scale is set in (the floor, below) |
| `--kind-style` | normal | normal, italic | a module's kind line (its glyph and word stay upright) |
| `--prose-numerals` | normal | normal, oldstyle-nums, lining-nums | figures in prose; data figures stay tabular |
| `--radius-module` | 0px | 0 to 14px | a module, a card, an item, an empty state |
| `--radius-cell` | 0px | 0 to 14px | a stage cell |
| `--radius-control` | 0px | 0 to 14px | buttons, the primary moves, the view switch |
| `--radius-chip` | 0px | 0 to 14px | tags and the Inbox count |
| `--section-rule-height` | 3px | 1 to 10px | the heavy rule's room over a band section's head and a step's, a unit's, Settings' and the docs' modules |
| `--section-rule-color` | `navy` | a colour, or transparent | the rule drawn plain in that room (the host's top border) |
| `--section-rule` | none | none, or gradient layers | anything more, painted in that room from the host's border edge: Oxford, Scotch, rings, dashed, misregistered |
| `--look-rule` | 2px solid `navy` | 1 to 3px, solid or dashed | a stopped module's rule |
| `--ask-rule` | 6px | 4 to 8px | a question's coral rule (a message's 2px less) |
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
line; the trace's fades and the drawer's slide are never a theme's.

**The display face's floor.** A display face never sets text under 19px. Every head sized by
`--head-scale` reads its face from `--head-face-N` (N its size in px at scale 1), and `build.rs`
works each out, for Sluice's scale and every family block that sets its own, after the family
files: the display face when N times the scale is 19px or more, the text face under that (in
Kodachrome, Fiesta and Road Atlas the narrow drawer's 18px module heads; in Empire Builder its
heads at 0.74 but the 26px and 30px ones). The other display sizes are fixed at 19px and up. Coral is never a chart's, a rule's but the question's, or a
band edge's.

## Type

Sluice's: one family, **Schibsted Grotesk** (variable, 400 to 900) for display and text, with
**JetBrains Mono** for ids, durations, clock times and data only. Another theme sets its own
faces (Themes, the deep tokens); its sizes stay these.

**Loading.** Every theme's faces are `@font-face` rules in the one stylesheet, pointing at Sluice's
own copies of pinned fontsource 5.3.0 files (Sluice's `@fontsource-variable/schibsted-grotesk` and
`@fontsource-variable/jetbrains-mono`, each subset fontsource ships; a family's own faces the latin
subset), vendored under `crates/sluice-web/assets/fonts/` with each family's licence and every
file's sha256, and served at `/static/fonts/<file>?v=<fingerprint>` to be kept for good. A page
asks no host but the dashboard for anything. A browser fetches a face only when it
sets text in it, so a page downloads its own theme's faces and no other, a switch needs no new
link, and a page without script gets the cookie's theme's faces. A family's face has a fallback
face (a local Arial or Courier New with `size-adjust` and ascent and descent overrides measured
from the face) next in its stack, so the swap to it does not move the page. The picker's swatches
carry no text, so opening it fetches nothing; `/_ui`'s theme cards set each theme's faces.

- The page's name (`ui::page_head`): 34px/38px, 850, -0.025em (28px on a phone), balanced
  and wrapping anywhere rather than running off; a step's name the same 34px/38px (23px on a phone, 24px in the drawer, three lines at most
  there). The summary sentence under it: 17px/24px, 500
  (15px on a phone), at most 75ch a line; its links (the questions, a recipe) are
  44px targets on a phone without changing the line. A page's note in its place: 14px in
  `ink-muted`.
- A band section's head: 26px, 850, -0.025em (24px on a phone), so the page's name outranks it,
  and so does nothing in it: home's project names are 26px. A question's title in its module:
  23px, 800. A module's title: 16px, 750, two lines at most.
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
  (`clamp(68px, 6cqi, 104px)`) close beside it, then the row's end (72px: a plan row's
  disclosure and its "⋯"). So each cell stands under
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
  and the back control are 40px, 44px on a phone. Without script each row opens its panel in
  place under it (nested `<details>`), every theme and the value types are one form with Save,
  and the keys (script's alone) are not offered.
- **The page's head** (`Frame::head`, `ui::page_head` and `ui::page_head_with`): on the paper,
  its crumbs when it is one thing in a tab (a step's way back to its plan and its unit, the unit
  by its title or name cut to 40, never "Its unit"), its name at 34px with its Details' "⋯"
  pinned to the head's right edge, and under it on a plan and on home only the summary
  sentence (`ui::summary_sentence`), one line of large body text where it fits; a page that says
  what it shows has a muted note in its place (`ui::page_head_note`: the Log's records, the
  Day's date and clock, the Functions' counts). No page carries a description blurb.
- **The row** (`.subnav`): its sections as tabs (Plan, Day, Stats, Messages, Log, Functions, Settings;
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
its Progress module, the margin), Overview's outputs and a recipe's view on a unit's row and
head (a "land sha" is in the unit's Details); only the Inputs and Outputs tabs draw every value
whole. A message line on a row names its sender in words (the
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
  each under a 3px navy rule, its name at 26px at the left and its count line at the right
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
(48px on a phone), the cell's kind read from the status table (`ui::Cell::of`). Wherever a
strip is narrow (a phone's row, the drawer, the For you card) one rule holds: three cells a
row, never under 72px, so a stage's name is never cut; where a cell says its state in a word,
the word goes under the name, never beside it.

- **Not reached**: an outline in `rule` with the stage's name; a waiting state that says
  itself (held, queued, paused, blocked) writes its glyph before the name and its word under
  them ("⊖ land", then "blocked").
- **Done** (the Done band): sky, its glyph (check, set by hand, skipped) and its name, how long
  it took under it.
- **Running** (the rest of the Running band: running, finishing, stopping, outside): the blue,
  its glyph (in `on-run`) and its name small, its elapsed time big and ticking under them, a
  sweep along its foot. It is never set
  against a usual time: that is the Stats page's.
- **Needs a look** (a state that wants attention: failed, cancelled, stale, quiet): sand, its
  glyph, its name and its word, its time; a quiet run keeps running's blue edge.
- A note may span the cells not reached ("then review and publish").

Every cell has the same two lines: its glyph before its name (a cell not reached that says
nothing draws none), then its word or its time alone, big for a run and a look, small for done.
So a state word never shares its line with the glyph, and it fits the narrowest cell, a plan
row's 68px at 1440, in every theme; a word wider than its cell would wrap within it, never clip
or go out of sight. In a plan row a look cell's second line is its time (the row's head says its
state in words); a cell waiting in its own way keeps its word there.

Each cell's state is said in words to a screen reader, and a cell with a page links to it.
`ui::stage_marks` draws the same strip as 12px squares in a line of words.

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
cancelled. 3 units at work: 1 quiet for 53m. 2 waiting. 3 of 10 units done; the last finished
57m ago." It names no unit: quiet runs are counted, the longest said ("3 quiet, the longest for
1h 6m"), and the rows under it name them. The units at work are counted, not split by recipe
(Running's groups say which recipe made them), each part is left out
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

## The step view

`views::unit_steps` (`View`, from a unit by `unit_steps::of_unit`) in a `sluice-steps`: a unit
drawn step by step, after the reference the owner chose (a pipeline's run view), in Synthesis.
It is the unit page's body, what a plan row's disclosure opens in place, and a step's Runs tab's
first part (its own row ringed).

- **Rows are steps**, in graph order: the recipe's (else the plan's), each after every step of
  its unit it comes after. Each row: the rail, the name (its stage, linking the step; a
  condition it runs on muted before it, "if ok", "if not ok"; a wait on another unit under it,
  "waits for Terns"), **State** (`ui::status`: glyph and word, then `ui::tries`' "run 3"),
  **Progress** (a scatter's "12/40" or a short `step_progress` field, a 3px bar under an "a/b";
  "—" for none), the **timeline** and **Span** (how long its current run took, in `ink-muted`
  data mono; a running one's ticking in `run-ink`).
- **The rail** (an inline SVG a row, out of flow so its lines run the row's whole height): the
  gates between the unit's steps (`after` entries and values passed) in `line`, 1.5px; a node a
  step, 5px: filled `sky-ink` done, ringed `idle` not reached, an arc in `run-ink` turning while
  it runs as the glyphs turn (held still under reduced motion), `sand` edged in `sand-mark` for a
  look. A chain runs down lane 0, beside the names; a branch takes the lowest free lane to the
  left (never one freed on its own row), and an arrow marks where a branch or a join reaches a
  step. At most five lanes.
- **The time axis.** One axis for every row, from the unit's first start to now while it is not
  done (rounded up to a step of a tenth of the time drawn, 5 minutes at least, so the server
  redraws it once a step, never with the clock), else to its last end. Linear, because a reader
  compares lengths; but no run is drawn under 3px, so a two-second step beside a two-hour one is
  still a mark in its row, and a stretch where nothing ran that is a tenth or more of the time
  drawn to scale collapses to a break, 4% of the width on `paper-2` between dashed `rule` edges,
  labelled with its length ("+1d 2h"). A log axis was refused: it would make every hour-long
  run look alike and lie about the gaps. The labels (data mono, 11.5px) sit at a nice spacing
  of the scaled time, 17% apart at least, each with a hairline down the rows; "now" is a
  `run-ink` line with its word, which `nav.js` moves with the reader's clock every five seconds
  and after every patch (`data-now`: the last stretch's start, place and rate), as it does a
  running bar's end. A bar: 8px, `--chart-radius`; done `sky-ink`, running `run`, a look
  `sand-ink`, no result `idle`; earlier runs at 35%.
- **The foot**: "Last message", how long ago, its sender in words, its first line linking its
  thread.
- **Selecting.** A row is the step's link: a click anywhere on it opens the step (in the drawer
  on the plan and in the drawer, its page elsewhere); the arrows, Home and End move between rows;
  the row in focus, clicked or open in the drawer is selected: `paper-2` under a 2px navy ring
  (the host's `selected`, kept through a patch). Hover is `paper-2` alone.
- **In the plan.** A row's disclosure (`ui::expand`: a `<details>`, its summary a chevron 34px,
  44px on a phone, before the row's "⋯", named "Steps of <title>") opens the view under the row
  across the content; a Done index line has one at its end and opens across the index's
  columns. Enter or Space toggles it. The title still traces; the disclosure only opens. The
  units open are the trace host's `expanded`, so a patch, or the unit moving between bands,
  leaves them open.
- **Narrow** (under 560px of its own width: a phone, a narrow drawer): one column, the rail
  and name, then state, progress and span, then the runs as a 4px bar; the head and the axis's
  labels give way. Nothing scrolls sideways.
- Words: the column names, the states' words, durations. Nothing explains how to read it.

## Motion

One moment, the work moving: the sweep along a running cell's foot (Sluice's: 3.2s, ease-out,
the bright sky crossing the blue; a theme sets its duration, easing and shape, or none). The trace's fades (0.35s ease-out) and the rail drawing down
(0.45s) answer a selection, and a disclosure's chevron turns (0.2s). A step view's now line
moves with the clock in steps of five seconds, never animated. Everything else is still. Under reduced motion the sweep is a
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
Display preferences hold the Keys (their Keyboard shortcuts panel) and end with "States and
parts" (the gallery) and "Agent docs" (`/docs`).

The components:

- **`sluice-trace`**: select to trace (above); it keeps the plan rows whose step view the owner
  opened open (`expanded`).
- **`sluice-steps`**: the step view (above): a row's click opens its step, the arrows move
  between rows, the selected row in `selected`.
- **`sluice-tabs`**: an ARIA tablist over its panels (arrows wrap, Home and End); the choice in
  `$$tab`, its `current`, and with `url` in `?tab=`; without script every panel stands stacked.
- **`sluice-fold`**: Show all and Show less over a long text, said only when it is cut, and
  never over an open question the owner can answer (its last line is usually the ask); or a
  More fold kept open per project or opened on a wide screen.
- **`sluice-confirm`**: the confirmation dialog (below).
- **`sluice-conversation`**, **`sluice-composer`**, **`sluice-answer`**: a conversation (Jump
  to latest, Mark read), the message box (sends as JSON, keeps its text through a patch) and a
  question's Answer and Close.
- **`sluice-menu`**: the project switcher, display preferences, the plan's More and every
  Details "⋯": a click elsewhere or Escape closes it and gives the focus back, its button says
  `aria-expanded`; the arrows, Home and End move through it. A sub-menu (`details.sub`: display
  preferences' Theme and Keyboard shortcuts) opens in the same menu, one level at a time: Enter
  or → opens its row and the focus goes in; its back control, ← or Backspace returns to the row.
- **`sluice-copy`**: an id or a SHA in data mono with a copy button.
- **`sluice-toggle`**: a display setting (the theme, value types) applied at once and kept by
  `/settings`; the Theme row names the theme the page wears, even after a stream patch drawn
  before the pick.
- **`sluice-search`**: the board's search, the functions' find, a form of filters.
- **`sluice-banner`**: the stream line ("Updates paused. Reconnecting…", "Updates stopped at
  14:02." with Reconnect) and the build line ("sluice was updated · Reload"); the stream's
  phase is also `html[data-stream]`, which the nav bar's live line reads.
- **`sluice-keys`**: `/` finds, `[` and `]` move the drawer, `g` then a letter goes to a page,
  `?` opens display preferences at Keyboard shortcuts; never while typing or under a dialog.
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
  its title naming what it acts on in words (a step by its stage and title, never its id). Focus starts on the
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
    run, its stage strip the first time a unit asks (three cells a row in the half-width card,
    each name whole, its state its glyph), the question's body, Answer primary, Open
    step, Message, Close question, its Details). Beside it, **Stopped**: each failed, cancelled
    or stale unit a module on the sand (its glyph and word, its failure's kind in a word, its
    stage marks, the failure's sentence and what to try next (one advice with the step page,
    `failure::Advice`), Retry with feedback first, the bare Retry quiet when the failure
    repeated, Open step, Unit page, its Details). A failure's Details end with "Set it aside"
    under a hairline: Cancel and Cancel and dismiss, each an outlined row opening its
    confirmation ("Cancel <title>?"; Cancel and dismiss says Undo brings back the cancel, not
    the failure); with script both
    post in place, Cancel and dismiss handing the focus to Undo. Failures before
    cancels, in plan order. A cancelled card ends with a quiet Dismiss (a plain form, no
    confirm): its unit then stops marking the plan and the summary, and stands in Done's index
    as "b cancelled and dismissed". The dismissal persists through record retention and later
    project activity; a retry or a new cancel brings the card back. For ten minutes after,
    Stopped says "Dismissed: <title>" and Undo in one
    polite status line a cancel (glyph, title clamped to two lines, Undo), the Undo a plain
    form too. With script, both post in place and the live stream updates Stopped without a
    navigation or scroll jump. Dismiss gives Undo the focus; Undo gives the restored card the
    focus. Retry, retry feedback, Cancel and Close question also post in place, including forms
    moved into the confirmation dialog. A failed post shows a notice beside the form and keeps
    its typed feedback. Without script, the forms keep their POST and redirect round trip.
    A failure is never dismissed as it is: it is cancelled first (a cancel that set a failure
    aside says "It had failed: …" under its sentence). A card's stage names it on a unit of several; a step's
    id never heads or leads a card. A card or row is its unit's trace button, named "Trace
    <title>", its state and time its description.
  - **Running**, quiet first then the longest running, and **Waiting**, in plan order, as rows
    (Running's count line "3 running", Waiting's "4 waiting", with ", all held by <title>" when
    one source holds them all and ", 1 blocked"; a band of one block says its block's line):
    a block a recipe (its name, linking to every unit it made, its count by state, its stages
    as the columns over its rows), then the units of no recipe and the loose steps. A row (72 to
    88px at 1440) is the unit's trace button: its glyph and word, how long it has run ("1h 51m",
    and how long silent when quiet), then its title, two lines at most; under it one line, what holds a waiting unit
    (each source by title, linked, and what it is doing) or the recipe's `view` (else its last
    message, its sender in words); its cells close beside it, sized to them, on the recipe's
    columns (a loose step one cell; a unit of no recipe its steps, six to a line); its
    disclosure and its Details at the end. The disclosure opens its step view under it (The
    step view); selecting the row opens its last message and actions in place.
  - **The margin**: a long run alone (no recipe, reporting progress, or running six hours or
    more, `plan::LONG_RUN`) as the sky module in the sheet's last two columns, down beside the
    bands; its title opens the step in the drawer.
  - **Done**: its count and when the last finished; what finished last, the latest four by
    title with their clock time and how long they took (`ui::latest_list`); then the index
    folded behind "Show the n done units" (open for Show: Done, a find or a recipe's every
    unit), a dense list newest first in columns: finish time, glyph, title, how long it took
    (in `ink-muted`, like every finished duration: the blue is for what runs), its disclosure.
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
  long it has run, its words, then its unit's stage strip (its own
  stage ringed) and its actions, the next move filled: Retry with feedback as on the plan's
  Stopped cards (its box folded under it, Retry beside it; on a failure that repeated it leads
  to the box standing open in why it failed and the bare Retry is quiet), a cancel's quiet
  Dismiss; a failure's Details hold Set it aside as its card's do. Why it failed reads in order: its headline, the words its run said and its last failed
  call, then the advice (ink at body size), then, when a failure repeated the run before, the
  feedback box standing open, so the owner reads why before writing. Its tabs follow. Overview is modules on the sheet: the question
  to the owner swells first on the coral rule, answered in place; then why it failed on the
  sand, or Now (its live turn: last words, its calls counted once in the turn's line, by tool
  as tiles when it used more than one, named exactly as the engine's transcript names them,
  and its latest calls; a progress field that repeats its latest message is said once, as the
  message); a question to the owner says it is waiting once, in the band's chip and the coral
  rule; beside them, as titled modules, what
  it waits on (or when it starts), what it comes after, its progress and its key output. The
  columns stand one over the other on a narrow sheet (the drawer, a phone, to 900px of sheet),
  at seven and five twelfths to 2000px, as halves from there with the side's modules two to a
  row, and at a third and two thirds from 3000px: more modules a row, never longer lines.
  Activity is its turns as rows numbered in data mono, the running one on the run rule;
  Messages the conversation; Inputs and Outputs field tables with where each came from; Runs
  its unit's step view, its own row ringed, over its runs. In the drawer the same head tops the step a size down,
  three lines at most.
- **A unit's page** (`/projects/id/<p>/units/<u>`): under the row, its head: its way back,
  its glyph and title with its Details (its id, recipe and params), how its steps stand, a short
  recipe view on that line. On the sheet its recipe's view whole when it says more than a line,
  then Steps: its step view across the sheet (The step view), and under it a module for each
  step that needs the owner, in the view's order: the one asking on the coral rule, answered in
  place, a stop on the sand with why it stopped, each headed by its stage with its Details.
- **The inbox, Questions and a thread**: the head names the page (a thread its step's title,
  its way back over it and its Details); the messages' switch is in the row. Questions to the
  owner
  come first as swelled modules on the coral rule, answered in place (Answer and Close 44px; on
  a phone a whole-width card), one a row to 900px of sheet, two to 2000, three from there and
  four from 3000; then those between agents, the ones nobody waits on and the unread notes
  (each its thread's name and project, its notes, then Mark read after them). A thread's page
  keeps its thread name and project in its Details.
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
  steps, running steps by title, "running for 1h 10m"), what it finished last by title and the way to its plan and its day. Then the Index, a
  table of every project's units by band, archived ones muted, its counts 10ch each so on a
  wide screen they stand together at the right; on a phone it drops Changed and Waiting so it
  fits without a hidden scroll, and its links and Today's are 44px targets.
- **Day** (`/day`, `/projects/id/<p>/day`, `views::day`): the day as a timetable. The head:
  "Day", its note the date. The day line under the day's sentence: a
  row a project, its name in the first 132px, its runs as 5px
  bars on 7px tracks (sky done, blue running, sand-ink needing a look), the hours along the top
  in data mono, the rule at now in the run blue; quick successes are counted, not drawn. The
  timetable: a 64px hour column (the hour 22px, its day where it changes, its runs counted),
  then a column a project; each run a line, three columns: its minute in mono (never wrapped),
  its unit's title in ink and its stage muted (two lines at most; its step and run in the
  link's title), then at the right its glyph and duration (sky-ink check), its word on the sand
  chip when it needs a look, or "running 43m" on the blue, never wrapped; a line is at most
  480px, so its outcome stays by its name on a wide screen. Under 400px of frame the title takes
  a line of its own under the minute and outcome. A busy hour (more than six in a cell) lists
  only its notable runs (not a success, still running, or an hour or longer) and folds the successes behind a muted "+23 more succeeded, 1m to 12m" that opens
  in place; a long list flows into columns as wide as the screen allows; four projects or more stack under each hour below
  1100px of content, every project below 640px. The current hour ends on the now rule and
  says what runs. `nav.js` keeps the reader's zone in a cookie and moves the rule and the
  running bars with the clock; the page's version is its runs, so it patches only when they
  change.
- **Stats** (`/projects/id/<p>/stats`, `views::stats`): a project's numbers over a window, and
  the one place a stage's usual time is said. The head: "Stats", its note how many runs ended
  and units finished in the window; in the row the window
  switch (24 hours, 7 days, the default, 30 days, All), the view-switch bar of links
  (`?window=`), so it works without script. On the paper four band sections, each under its
  3px navy rule: **Durations by stage**, a table a recipe in the project's recipe order (its
  name, how many units and runs) with a row a stage in the recipe's order: how many of its runs
  succeeded ("Succeeded") and their median, p90 and longest in data mono, then, under "median ·
  p90", the median drawn as a sky-ink bar inside its p90 in pale sky with a sky-ink edge,
  against the recipe's longest p90 (no section line explains it); the
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
  view-switch bar beside the filters, the note on what is left out, then Records under a
  band section's head, the table under a 2px navy rule with seq, time and kind in data mono. A
  message row is its sender and recipient as name links, then its words in plain muted text and
  one small "thread" link after them.
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
- Do keep every page a server-rendered picture that works without JavaScript.

### Don't:
- Don't spend coral on anything but an open question waiting on the owner (and the logo).
- Don't fill anything blue that is not running.
- Don't round a corner, draw a card inside a card or add a stripe, but as a theme's deep tokens
  allow (Themes).
- Don't name a project's own words in view code; take them from what the project supplies.
- Don't restyle or redraw the mark; it is the owner's SVG.
- Don't add a legend; each state says itself.
