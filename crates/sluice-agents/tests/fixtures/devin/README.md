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
