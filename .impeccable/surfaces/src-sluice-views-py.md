---
version: 1
slug: "src-sluice-views-py"
primary_target: "src/sluice/views.py"
related_targets: ["src/sluice/dashboard.py"]
---

# Surface: the sluice dashboard (all routes served by src/sluice/views.py)

Mode: Operate. Visitor: Sam, supervising many agents; glances between other work, second screen or phone.
Job: what is running, what is stuck, what needs me, what did each block produce, is the work moving.
Binding user constraints (mid-build): minimalism and clarity; no duplicated information; ids and plumbing
live in the step detail, not on the board; take inspiration from ../hirsel (Sam's own product); use a
vanilla JS graph library only if it earns its place (evaluated: elkjs/dagre; declined, see DESIGN.md).

## Direction contract

THESIS: The plan is the page: a quiet left-to-right board of the blocks of work, each saying one thing
per slot (outcome glyph, what it does, what it said last, how long). It refuses the node-editor look
(ports and ids everywhere) and the CI table of every job.

OWN-WORLD: The logo's world (redesign of 2026-09-28, brief-pinned by the owner, no roll): 1970s American supergraphics. Navy ink on a cream canvas, cream on deep navy in the dark; the logo's blue as the one accent and the colour of running; coral only in the logo and the inbox count. Archivo, heavy and a touch wide, for the wordmark, titles and step ids; Public Sans for text; flat fields, hairlines, the mark's rounded corners (14/10/5px). Status ramp: blue running, kelly green succeeded, harvest gold attention, plum paused, grey-navy idle, ink failed.

STORY: Sam reads "Needs you" first (answer, input, failed, message), sees the board move, opens a block
to read its prompt, its live progress and what it produced, and leaves.

FIRST VIEWPORT: One-line nav. Project title and one meta line (done of total, running, failed, cost,
updated). Needs-you rows below only when something waits. The board fills the rest: glue steps as slim
chips, work blocks as cards, hairline edges; it scrolls sideways inside itself, opened at the live
frontier. A block opens a right-hand drawer (full-screen sheet on phone) with everything else.

FORM: user-pinned reference (hirsel's grammar) over the rolled Solari departure board (seed 7e23b906,
candidate 7); kept from the roll: fixed-size modules in columns, tabular time, the flap of a status change.
Signature interaction: hovering or focusing a block traces its inputs and outputs (edges light and name
their ports, the rest dims).

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict,
DESIGN.md, and every shipping raster carrying its provenance
