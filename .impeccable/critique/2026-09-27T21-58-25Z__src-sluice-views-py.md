---
target: the sluice dashboard
total_score: 28
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 2
target_identity: "file:/workspace/code/sluice/src/sluice/views.py"
target_fingerprint: "sha256:5dd81829cb1637c91503a7d91d4126321ee147aa72eea84f676f9db960ab5ed7"
target_path: /workspace/code/sluice/src/sluice/views.py
timestamp: 2026-09-27T21-58-25Z
slug: src-sluice-views-py
---
⚠️ DEGRADED: single-context (the user asked for critiques to run inline)

| # | Heuristic | Score | Key Issue |
|---|---|---|---|
| 1 | Visibility of System Status | 3 | finished steps still showed amber "awaiting reply" |
| 2 | Match System / Real World | 3 | a gone step's thread showed its raw name `step-readers` |
| 3 | User Control and Freedom | 3 | solid: Esc, close, Pause switches |
| 4 | Consistency and Standards | 3 | markdown headings from specs claimed h1-h3 in the page outline |
| 5 | Error Prevention | 3 | read-only surface; nothing to break |
| 6 | Recognition Rather Than Recall | 2 | Functions: 45 fns, 7000px, no index |
| 7 | Flexibility and Efficiency | 3 | arrow keys on the board, hash links |
| 8 | Aesthetic and Minimalist Design | 3 | Log triples each message (call running, message, call succeeded) |
| 9 | Error Recovery | 3 | failed steps read clearly |
| 10 | Help and Documentation | 2 | tooltips and empty states only |
| Total | | 28/40 | Good |

Priority issues:
- [P1] Stale "awaiting reply" (amber) on threads of finished or removed steps (b4 succeeded; step-readers gone): Threads tab and drawer. Fixed.
- [P1] Phone board: a wrapped lane kept its depth rows, leaving a ~150px hole crossed by a long dashed edge. Fixed.
- [P2] Functions page: sideways scroll at 390px (long enum types) and no index over 45 fns. Fixed.
- [P2] Markdown headings in specs and values entered the page outline as h1-h3 (detector: skipped-heading, flat hierarchy). Fixed (top heading shifted to h4).
Minor: phone nav clips "Functions" without a cue (fade added); Log shows three rows per message (left, question); Log kind filter is 13 flat checkboxes (left).
