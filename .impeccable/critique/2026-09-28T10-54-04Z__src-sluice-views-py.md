---
target: the sluice dashboard
total_score: 25
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 3
target_identity: "file:/workspace/code/sluice/src/sluice/views.py"
target_fingerprint: "sha256:fd99e18a75f128a60f9cd43491a53a90701c0086f0e9aa30567ffe189f862704"
target_path: /workspace/code/sluice/src/sluice/views.py
timestamp: 2026-09-28T10-54-04Z
slug: src-sluice-views-py
---
Method: dual-agent (A: design review · B: detector + browser evidence), both isolated; A finished before B's findings entered synthesis.

## Design Health Score

| # | Heuristic | Score | Key Issue |
|---|-----------|-------|-----------|
| 1 | Visibility of System Status | 2 | A stuck project reads calm: summary omits paused (11) and blocked (4); index says "Stopped: nothing is running"; Inbox "Nothing is waiting on you" |
| 2 | Match System / Real World | 3 | sluice's own terms; `run.adopt` rows raw JSON, "by mcp" noise |
| 3 | User Control and Freedom | 3 | Esc/close/Back work, focus returns; click-away does nothing; drawer covers the board |
| 4 | Consistency and Standards | 3 | Pause on succeeded steps, two Types switches per drawer, "After" plain vs "Waits on" linked |
| 5 | Error Prevention | 3 | Pause/Archive one click, side by side, first in tab order (both reversible) |
| 6 | Recognition Rather Than Recall | 2 | "Waits on X (failed)" only in a tooltip; the fact grid ellipsizes it away |
| 7 | Flexibility and Efficiency | 2 | Arrow keys and deep links exist; no jump-to-failed, ArrowDown skips rows, no folding |
| 8 | Aesthetic and Minimalist Design | 3 | Calm; 45 finished cards set the weight of a 3,300px board |
| 9 | Error Recovery | 2 | Failed drawer opens on the head of the output; the cause is the last line |
| 10 | Help and Documentation | 2 | Edge legend and tooltips; no glyph key |
| **Total** | | **25/40** | **Acceptable** |

## Design Specificity Verdict
LLM: grounded — drawn status glyphs, lanes in quiet boxes, traced edges naming the value they carry, a drawer that reads like a run history. Log and Functions are generic admin tables.
Detector: pages exit 2 with 13 findings, all false positives on server-rendered user data (marketing-buzzword and aphoristic-cadence on plan prompts in History; repeated-container-text on chat transcripts). Static: 17 DESIGN.md drift advisories (radius 2/3/6/999px, 11/20/22px sizes, #000). Overlay injected on 5 views: line-length on the about text, text-overflow on long step ids at 390 (intended ellipsis, real cost on phones), text-occlusion on the closed switcher menu (false positive).
Measured: drawer overlaps 440px of the board at 1440 (12 of 20 visible cards) and covers the nav's Inbox link; floats ~120px off the board at 2560; background click never closes it; 68 of 72 board controls under 44px tall at 390; 17 Log checkboxes 13x13; traced state dims unrelated cards to 1.9-3.9:1; nav focus ring clipped top/bottom; no overflow on any page.

## Priority Issues
- [P1] Stuck state invisible at every summary level (summary line, bar label, index row, inbox, blocked pending cards look ordinary). Fix: paused and blocked counts, an attention sentence on index and project, `is-blocked` cards, glyph on index row.
- [P1] Drawer covers the board; click-away does nothing. Fix: push layout at >=1200px, scrim overlay 721-1199px, click-away closes, scroll the card into view, drawer below the header.
- [P1] Failed error shows the head of the output; the cause is the last line. Fix: headline from the last non-empty line, pre scrolled to the bottom, same headline in Log and tooltip, a Blocks fact.
- [P2] Phone board 6,200px tall and still draws edges (DESIGN.md says none below 720px).
- [P2] Fact grid ellipsizes "Waits on ... (failed)", "After", "When".
- [P2] Screen reader/keyboard: run-together card names, drawer not a dialog on phone and focus escapes, no live region, no skip link, sub-44px targets, clipped nav focus ring.

## Persona Red Flags
Alex: no jump to failed/blocked, ArrowDown skips a row, no folding, ticking a box turns History back into Log.
Sam (a11y): tooltips carry docs and blockers; tracing dims to ~2:1 while the drawer is open; no dialog/inert on phone; no live region.
Operator (second screen/phone): index and inbox look calm while the project is dead-stopped; tab title has no status; 3px failed sliver; failed card border in dark reads as selected; ~6,000px of finished work on a phone.

## Minor Observations
Pause on succeeded steps; two Types switches per drawer; `run.adopt` raw JSON in Log; message rows repeat the thread; Log filter ~250px on a phone; "16m ago" on index unexplained; 2560 board stays 960px wide.

## Questions to Consider
Should a failure that blocks work reach the inbox? Should finished boxes fold and attention sort first? Should 2560 get a permanent side pane?
