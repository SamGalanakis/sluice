# Claude fixture records

The hook and transcript shapes are redacted from the existing Python Claude
fixtures and checked against the Claude Code hooks reference on 2026-10-03:
https://code.claude.com/docs/en/hooks

The wire schema is the evidence under test. Provider prose and session paths
are replacements. Supported installed version: 2.1.284; the wrapped composer
case also preserves the recorded 2.1.283 behavior. The fake executable uses
these channels and scripts turn timing, submit, tools/errors, background shell,
wakeup, compaction, startup/trust, dropped Enter, resume and early exit. Its
submission file is test evidence, never a production completion signal.
