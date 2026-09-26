---
name: sluice dashboard
description: A calm board for one person supervising many agents: what needs you, what is running, what each block produced.
colors:
  canvas: "oklch(0.975 0.008 190)"
  ink: "oklch(0.23 0.018 220)"
  card: "oklch(1 0 0)"
  quiet-fill: "oklch(0.955 0.01 190)"
  secondary-fill: "oklch(0.945 0.012 190)"
  muted-ink: "oklch(0.49 0.035 205)"
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
  muted-ink-dark: "oklch(0.72 0.025 175)"
  primary-dark: "oklch(0.79 0.105 158)"
typography:
  title:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "18px"
    fontWeight: 600
    lineHeight: "24px"
    letterSpacing: "-0.01em"
  section:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "16px"
    fontWeight: 600
    lineHeight: "22px"
  body:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: "20px"
  small:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "12.5px"
    fontWeight: 400
    lineHeight: "18px"
  meta:
    fontFamily: "Inter Variable, Inter, ui-sans-serif, system-ui, sans-serif"
    fontSize: "11px"
    fontWeight: 500
    lineHeight: "16px"
  data:
    fontFamily: "ui-monospace, SFMono-Regular, Cascadia Code, Liberation Mono, Menlo, monospace"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: "18px"
rounded:
  sm: "4px"
  md: "8px"
  lg: "0.625rem"
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
  project-tab-current:
    textColor: "{colors.ink}"
    padding: "8px 0 10px"
  status-filter-current:
    backgroundColor: "{colors.secondary-fill}"
    textColor: "{colors.ink}"
    rounded: "6px"
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
  stale and "Needs you", **idle grey** pending, **ink** failed.

### Named Rules
**The One Red Rule.** Red (`badge`) is the open-inbox count in the nav and nothing else. A
failed step reads through its cross glyph, full-ink border and bold error line, not red.

**The Shape Carries It Rule.** Every status has its own drawn glyph (dashed ring, spinning
ring, check, ring and dot, circular arrow, cross) plus a visually hidden word; colour only
repeats what the shape says.

## Typography

One family, Inter Variable (the system sans without it), on a fixed ramp: 18/24 page title,
16/22 section, 14/20 body, 12.5/18 small, 11/16 meta. Monospace is for data only: stderr, fn
names in the functions list, values, types. All numerals are tabular.

### Named Rules
**The Meta Voice Rule.** What a run says about itself (times, costs, counts, engines, labels)
is 11px meta in muted ink, sentence case. Nothing is uppercase; no kickers or eyebrows.

## Layout

The board is laid out on the server: columns by dependency depth (plan outputs last; plan inputs
are a two-line strip above), 248px card columns and 188px chip-only columns with 56px gaps, each column placed by the
mean height of what feeds it, edges as inline SVG through thin slots so they never run under a
card. It scrolls sideways inside itself and opens at its left edge, scrolling only when the
live frontier is off-screen; the page never scrolls
sideways. Text blocks hold a 72-75ch measure.

### Named Rules
**The One Column Rule.** Every page sits on one centred 960px column (`--column`, with at
least a 24px gutter, 16px below 720px): `main` is a three-track grid (gutter, column, gutter)
and everything goes in the middle track. The top nav's content aligns to the same edges (the
wordmark on the left edge, Inbox ending on the right), so nav, title, lists, tables and cards
share one left edge at every width. The board alone spans the window: its first column starts
on the column's edge; a plan too wide for that starts further left, only as far as it must to
end at the right gutter (never past the left one), and scrolls inside itself only when the
window cannot hold it. Nothing makes the page scroll sideways.

Below 720px the board stacks in the same order without edges, and the step drawer becomes a
full-screen sheet over the scrim.

## Elevation & Depth

Flat by default: cards separate by hairline, not shadow. One lift, on the step drawer
(`--lift`), because it floats over the board.

## Shapes

Radius 0.625rem for cards, chips, the needs list and inbox items; 8px for controls and code
blocks; 4px for inline code. Plan input and output nodes are dashed, since they are ends, not
work.

## Components

### Cards / Containers
- **Work card** (248x112): glyph + title (the step's doc, two lines), one line (progress in
  mono while running, the error when failed, the first text output when done), then meta
  (engine or fn, cost, stale or set by hand) with the duration pinned right. Running cards take
  an active-blue border, failed a full-ink one, stale an amber one; pending titles dim.
- **Glue chip** (188x34): inline built-ins (`core.*`); glyph + one-line title, no border.
- **Output node**: dashed, "Output **name**" in meta, the value on one line (muted when unset).
- **Inputs strip**: `name = value` pairs in one clamped line above the board; unset reads amber.

### Buttons
Primary is green on its own ink; others are card-coloured with an input hairline.

### Navigation
- **Top bar** (48px, card fill, on the column): the mark (an ink rounded square with a gate
  over water, cut in the card colour) and the sluice wordmark, then Projects, Functions, Log;
  Inbox, with the one red badge, sits at the right edge. The current section is ink with a 2px
  ink bar on the bar's bottom hairline and `aria-current` (`page`, or `true` on a page inside
  the section: a project's pages are in Projects); the rest are muted ink, with a quiet fill on
  hover.
- **Project tabs**: every page of a project opens with its name (the page title) and tabs,
  Plan, Log, Inbox, History, Functions, on a hairline; the current tab is ink with a 2px ink
  underline. They fit a phone and scroll sideways inside themselves if they ever do not.
- **Status filter** (inbox): a small segmented control, the current status in the secondary
  fill with a strong hairline; a filter, so it does not look like the tabs.

### Needs you
One bordered list, one row per thing that waits: a 64px meta kind word (Answer, Input in
amber; Failed in ink; Message in blue), the text on one line, its age in meta.

### Step drawer
Right-hand panel (680px) over the board without a scrim on desktop, full-screen with a scrim on
phones. Header: glyph and title, then the ids line. Sections under meta labels in need order:
Error, Progress, Outputs, Messages, Prompt, Inputs, Stderr (folded), Runs.

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
- Don't use uppercase labels, kickers, emoji icons or decorative motion; motion is the running
  spinner, the 220ms flip of a changed glyph and 150ms fades.
