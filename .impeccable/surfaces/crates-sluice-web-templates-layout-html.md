---
version: 1
slug: "crates-sluice-web-templates-layout-html"
primary_target: "crates/sluice-web/templates/layout.html"
related_targets: ["crates/sluice-web/src/views/ui/grid.rs","crates/sluice-web/assets/style.css"]
---

# Surface: the sluice dashboard (every page in the frame)

Scope: the whole dashboard (`crates/sluice-web`): the frame (layout.html), the kit
(`views::ui`, `ui::grid`), and the pages composed from it. Mode: Operate.

Audience and job: one owner supervising many agents, between other work, on a second screen or
a phone; within seconds, does anything need me and is the work moving; then drill into any unit
or step. Proof and content: the project's own plan, recipes, runs, progress fields, messages.
Constraints: server-rendered askama, Datastar patches, Rocket components that only behave; no
client-rendered content; works without JS; fonts from cdn.jsdelivr.net only; the Americana
palette is binding (PRODUCT.md); fully generic (no project's vocabulary in view code).

Chosen direction: Synthesis (owner, 2026-10-09). Memorable moment: the project's name set huge
in the navy band with one sentence beside it, and selecting a unit lighting its chain down the
margin.

## Direction contract

THESIS: the dashboard is a park map for work: a Unigrid title band and a visible module grid,
ordered by what needs the owner. It refuses the category default of a table of jobs with
status pills, and the old rounded-card board.

OWN-WORLD: navy band, cream paper (navy paper in the dark), square modules on twelve visible
columns under heavy navy rules; the channel's blue only for running, sky for done, sand for a
look, coral only for a question to the owner; Schibsted Grotesk set heavy and tight, JetBrains
Mono for ids and times.

STORY: the owner reads the band's sentence, sees the one swollen module that needs them
(a question), answers or retries, then scans Running for overruns and quiet runs, and leaves.

FIRST VIEWPORT: the band across the window: top row (mark, Projects, project, Inbox with the
coral count, Live), the name at 150px over five columns, description and summary sentence over
seven, Recently finished under a rule. On the paper: tabs, count line, Find and Show grid; the
trace line; For you (six columns) beside Stopped (four) beside the margin module (two). The
primary action, Answer, sits in the For you module.

FORM: owner-selected direction from the redesign comparison (Synthesis over Unigrid and
Transit); no seed roll, so no seed key: the comparison renders are the record.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance
