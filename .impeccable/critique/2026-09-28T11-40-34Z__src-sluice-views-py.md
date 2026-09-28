---
target: the sluice dashboard
total_score: 28
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 1
target_identity: "file:/workspace/code/sluice/src/sluice/views.py"
target_fingerprint: "sha256:ab37d60226d1b8ece389ed07f729b8bfa1616c1f3853e494200f3365415fa1bc"
target_path: /workspace/code/sluice/src/sluice/views.py
timestamp: 2026-09-28T11-40-34Z
slug: src-sluice-views-py
---
Method: dual-agent (A: design review · B: detector + browser evidence), isolated; confirm round after eb15778.

## Design Health Score
| # | Heuristic | Score | Key Issue |
|---|---|---|---|
| 1 | Visibility of System Status | 3 | Stuck state reads everywhere; index does not flag a quiet run |
| 2 | Match System / Real World | 3 | Failure headline is a Python class and absolute paths; History titled "lash log" |
| 3 | User Control and Freedom | 3 | Esc/close/scrim/click-away work; 721-1199 overlay not modal |
| 4 | Consistency and Standards | 3 | Index names running steps by doc, board by id |
| 5 | Error Prevention | 3 | Pause/Archive one click, reversible |
| 6 | Recognition Rather Than Recall | 3 | Blocks visible; phone folded summaries drop their outcome |
| 7 | Flexibility and Efficiency | 3 | Skip link, arrows, deep links, stuck links |
| 8 | Aesthetic and Minimalist Design | 3 | Folding a big win; the open 30-card box tangles |
| 9 | Error Recovery | 2 | Headline gives the wrapper, not the cause; no next step |
| 10 | Help and Documentation | 2 | No glyph key; docs and quiet detail in tooltips |
| **Total** | | **28/40** | **Good** |

## Deterministic scan
Static 17 -> 0 (DESIGN.md drift gone). Pages 22 findings, all false positives (chat transcripts, run data, deliberate zero padding on folded boxes). Drawer: 0 overlap at 1200/1440/2560, click-away closes, protected targets keep it open; 1000 scrim closes; 390 dialog traps focus. Contrast: 0 real failures at rest, traced, drawer open. Skip link first stop.

## Priority Issues
- [P1] Failed headline is the wrapper exception with absolute paths; exit 143 unexplained.
- [P2] Folded summaries on a phone truncate both ids and drop "n steps · all succeeded".
- [P2] Regression: push reflow puts a new card under a still pointer, which traces and dims the board while the drawer is open.
- [P2] 721-1199 overlay not modal; skip-link target not focusable; phone drawer links under 44px.
- [P2] The open 30-card box tangles: a paused lane sits between failed lanes' rows, long edge detours.
- [P3] Stuck sentence wraps at 72ch; sticky close button covers pre text; History tab title.
