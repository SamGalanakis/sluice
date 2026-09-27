---
target: sluice dashboard
total_score: 26
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 2
target_identity: "file:/workspace/code/sluice/src/sluice/views.py"
target_fingerprint: "sha256:656dd87965ebed223789dd5a94c49e6ec1c775a14f3244b07f926a259d907c63"
target_path: /workspace/code/sluice/src/sluice/views.py
timestamp: 2026-09-27T16-01-45Z
slug: src-sluice-views-py
---
⚠️ DEGRADED: single-context (critique runs inline in this session at the user's direction)

| # | Heuristic | Score | Key Issue |
|---|---|---|---|
| 1 | Visibility of System Status | 3 | Live durations and glyphs are good; the project summary sits at the bottom, and 14 pending cards look identical |
| 2 | Match System / Real World | 3 | sluice's own terms; the project description shows raw markdown ("here: - Every agent brief…") |
| 3 | User Control and Freedom | 3 | Pause/unpause per step and per project, Esc closes the drawer |
| 4 | Consistency and Standards | 3 | Coherent; a thread repeats the same relative time three times |
| 5 | Error Prevention | 3 | Read-mostly surface; archiving is reversible |
| 6 | Recognition Rather Than Recall | 2 | Solid vs dashed edges, and which pending step is next, are left to memory; edge names show only on hover |
| 7 | Flexibility and Efficiency | 2 | Plans carry tags (b4, heavy) but the board can't filter to a lane |
| 8 | Aesthetic and Minimalist Design | 2 | Calm palette, but the edges are a tangle: long edges run behind cards, and the five lanes interleave |
| 9 | Error Recovery | 3 | Failed steps lead with their error in the drawer |
| 10 | Help and Documentation | 2 | No legend for glyphs or edge styles |
| **Total** | | **26/40** | **Acceptable** |

Specificity: authored for sluice (glyph pills, live durations, one red badge, thread cards keyed by step). The weak point is the board layout, which is what the page is about.

Detector: 17 design-system-color advisories in views.py:42-47 (Mermaid export classes, not the dashboard: false positive); repeated-container-text ×2 (thread times, real); in the live page, text-overflow on span.th-last (intended ellipsis: false positive), line-length ~86ch ×12 (drawer messages, minor and real), and text-occlusion on the visually hidden h1 (false positive).

Priority issues
1. [P1] Board layout. Rows by depth are kept in plan order, with no crossing reduction, and long edges pass behind cards (b4→rm-b5, fork-b4→rm-b4). The five independent lanes (fork → worker → close → rm) interleave, so no lane reads as a column. Fix: order each row by barycenter sweeps with each connected component kept contiguous, then thread long edges through the gaps between cards in the rows they cross. Build the board as a Rocket light-DOM component (<sluice-board>) with edges as a json prop, measuring in onFirstRender and cleaning up its ResizeObserver. /impeccable layout
2. [P1] Phone board. At 390px the rows wrap and the edges become vertical spaghetti; the board can't be read. Fix: stack the components vertically on phone (each lane is 1-3 cards wide), with no wrapping. /impeccable adapt
3. [P2] "Is it moving" sits at the bottom. The summary ("4 of 22 succeeded · 4 running · updated 4m ago") and Pause/Archive come after the board and Messages, so on phone they are the last thing on the page. Fix: move the summary under the nav, above the board. /impeccable layout
4. [P2] Pending is one undifferentiated state. 14 identical dashed cards, and the close-b6 drawer doesn't say it is waiting on b6. Fix: a "Waiting on b6 (running)" fact in the drawer and in the card's title; set the next-up pending cards (every dependency running or done) apart from deeper ones. /impeccable clarify
5. [P2] The drawer buries the spec. Six long messages come before Progress, Outputs, Spec and Inputs; the spec starts about 2000px down. Fix: show the last two messages plus an "N earlier" disclosure, opened when a question awaits a reply. /impeccable distill

Personas
- Alex (power user): can't isolate the b4 lane by tag; edge meaning appears only on hover.
- Sam (a11y): edges are aria-hidden, but the drawer lists the deps, so that's fine. Dashed vs solid is not colour-only (good).
- Sam on a phone between tasks: the board is unreadable, and the summary is at the very bottom.

Minor: the description shows markdown source; relative times repeat inside threads; drawer prose runs ~86ch; the index shows "All projects" plus a "Projects" tab; there is no legend for dashed "after" edges.
