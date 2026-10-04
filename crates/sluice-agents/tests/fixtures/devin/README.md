These fixtures preserve the Python fake_devin/fake_composer wire shapes, checked
against Devin CLI 3000.11.3 help and the official lifecycle-hook reference on
2026-10-03. They are synthetic redactions, not claimed live captures. The real
G3 gate supplies separately labelled live evidence. Hooks carry session/prompt
identity; composer text confines draft evidence to the current input region.

real-captured.jsonl was recorded from the second labelled G3 scratch session on
2026-10-03. Payload content, paths and identities are redacted. It preserves the
observed ordering: two accepted user prompts, with live steering merged into one
turn, followed by one Stop for the latest prompt. That session demonstrated the
exit Enter timing defect and was cleaned up without manual key injection.

supervisor-hook.py is the private synchronous-hook fixture ported from the p5-05
acceptance worktree. The two supervisor readiness regressions use it and the
nonblocking fixture journal to cover fresh submission, live feedback, compaction
and same-session resume. Both tests failed with ReadyTimeout before the readiness
fix. The executable fixture's omit_session_start setting separately proves that
the visible ready composer reports Idle before any input is offered.

resume-footer-excerpt.txt reproduces the bottom of the resumed real pane quoted in the
g3-real report (the raw captures were not retained). Devin 3000.11.3 shows
`(bypass permissions on)` above the composer and no indicator at all in Normal mode; the
executable fixture now draws that layout. exit-during-probe.py is a scripted tmux client
whose Devin journals its last hooks and exits during the pane probe or the next client
command, the window in which an exit used to drop the latest acknowledgement.

real-fresh-bypass-pane.txt and real-resume-bypass-pane.txt were captured by g3_devin
(SLUICE_G3_DEVIN_EVIDENCE) from labelled scratch session amusing-learning on 2026-10-04, fresh
and resumed, with scratch paths redacted to /scratch. The real release draws the indicator
right-aligned inside the composer's top rule: `──── (bypass permissions on) ─`.
