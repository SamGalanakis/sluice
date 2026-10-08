# Claude fixture records

The hook and transcript shapes are redacted from the existing Python Claude
fixtures and checked against the Claude Code hooks reference on 2026-10-03:
https://code.claude.com/docs/en/hooks

The wire schema is the evidence under test. Provider prose and session paths
are replacements. Recorded on 2.1.284; the wrapped composer case also preserves
the recorded 2.1.283 behavior. Those two are `profile::POLICY`'s tested versions;
a newer Claude Code runs untested (SPEC §15). The fake executable uses
these channels and scripts turn timing, submit, tools/errors, background shell,
wakeup, compaction, startup/trust, dropped Enter, resume and early exit. Its
submission file is test evidence, never a production completion signal.

`real-usage-limits.jsonl` holds the API error entries 2.1.284 wrote for usage and rate limits
in the owner's transcripts, cut to their error fields and with request ids redacted: the weekly
and 5-hour plan limits with `quotaLimits` (`status: "rejected"`, `rateLimitType`, `resetsAt`),
out of usage credits, a model's limit, and an ordinary rejected 429.

`real-auth-errors.jsonl` holds the API error entries 2.1.284 wrote when it could not
authenticate, from the owner's transcripts cut to their error fields: `authentication_failed`
(OAuth session expired, login expired, not logged in, a 403 asking for /login),
`oauth_org_not_allowed` and `account_on_hold`. They carry no credential.

`real-rewind-pane.txt` is the bottom of the pane a resumed lane failed on (`pasted draft could
not be verified`): Claude Code 2.1.284's Rewind dialog over a session with nothing to rewind to,
which two Escapes within 800 ms open. `real-rewind-list-pane.txt`, `real-history-picker-pane.txt`
and `real-composer-pane.txt` are 2.1.284 in a scratch home over a three-prompt session written for the check:
Rewind with prompts to rewind to, the history picker (`ctrl+r`) and the bare composer. The fake
takes Escape as 2.1.284 does (a second press within 800 ms on an empty composer opens Rewind,
which takes every key and paste but Escape) and scripts a resume's covered composer and unread
input (`cover_ms`, `hold_ms`), a dialog that takes the next paste (`modal_on_paste`) and pastes
that leave no draft (`swallow_pastes`, counted across launches). It logs the keys and pastes it
takes to `fixture-keys.jsonl` and each dialog it opens to `fixture-modals.jsonl`.
