---
target: sluice dashboard (project board, nav, inbox)
total_score: 20
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 3
target_identity: "file:/workspace/code/sluice/src/sluice/views.py"
target_fingerprint: "sha256:3c3289e068357bd59c7c91d19fd56a1a9bca4acb9c9d69d144cf3231eac9e6d0"
target_path: /workspace/code/sluice/src/sluice/views.py
timestamp: 2026-09-26T23-17-29Z
slug: src-sluice-views-py
---
DEGRADED: single-context (user asked for an inline run, no subagents)

Heuristics: 1 Visibility 3 · 2 Match 2 · 3 Control 2 · 4 Consistency 2 · 5 Error prevention 3 · 6 Recognition 2 · 7 Flexibility 1 · 8 Minimalist 2 · 9 Error recovery 2 · 10 Help 1 = 20/40 (Acceptable, bottom edge)

Priority issues:
- P1 The board doesn't show what the work produced: card lines are truncated agent chatter or raw JSON; typed outputs are hidden; plan outputs are clipped raw markdown.
- P1 The layout breaks its own column and wastes the screen: the board escapes to the window edge at 2000px, outputs are clipped, and 60% of the viewport is empty.
- P1 Type is too small and faint for a glanceable dashboard: 11px muted meta, 14px body, 18px title.
- P2 Needs you nags about a dead project forever: tetris "1 failed step".
- P2 Raw text leaks into the step page: markdown isn't rendered, cost is unformatted, session and cost sit among the declared outputs.
- P3 Edges have no direction and tangle between the first two columns.
Detector: 1 finding, cramped-padding on the index needs-you <ul>.
