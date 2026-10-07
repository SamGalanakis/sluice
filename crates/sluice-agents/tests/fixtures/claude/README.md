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
