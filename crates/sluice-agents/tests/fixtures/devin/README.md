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

The supervisor readiness regression uses the nonblocking fixture journal to cover
fresh submission, live feedback, compaction and same-session resume; it failed with
ReadyTimeout before the readiness fix. The executable fixture's omit_session_start
setting separately proves that the visible ready composer reports Idle before any
input is offered.

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

real-fusion-busy-pane.txt is the bottom of a live Fusion pane (`fusion-claude-opus-5-5-high-sidekick-
swe-2-high`) captured read-only on 2026-10-05 while its lead waited on a blocking sidekick call: the
spinner row ends `(esc twice to interrupt)` and the composer shows `Guide Devin while it works`. In
Fusion the sidekick's turn end fires the Stop hook under the lead's prompt and the lead's blocked
call returns right after it (14 of 14 Fusion Stops in the live home's journals), so the executable
fixture's `after_stop` events keep a turn working after its Stop, under that busy pane, and queue
input submitted meanwhile until the turn ends (`queued`, `send now`, as the 3000.11.3 binary's
strings name them; no queued pane was captured).

real-quota-exhausted-pane.txt is the bottom of a live Fusion pane captured read-only on 2026-10-05
after Devin ran out of weekly usage quota (its run then stalled until the 30-minute stall cap), with
paths redacted to /scratch. Each prompt it took was followed by the `⚠︎ Quota exhausted` notice and
no further hook, under an idle composer. The executable fixture's `quota` turn draws that notice
after its prompt's UserPromptSubmit and sends nothing more.
Its `auth` turn draws the `⚠︎ Authentication required` notice the same way, with the explanation
3000.11.3's binary carries (`Your session is no longer authenticated. Run /login to
re-authenticate here (or devin auth login), then send a message to continue`); no live capture of
that notice exists.
