# Codex 0.160.0 wire fixtures

turn.json selects lifecycle, command and agent-message events from one labelled g3_codex
gate on 2026-10-03 (session prefix 01a103b2). Each captured frame passed through
protocol::redact: prose, credentials, paths and session identities were removed, while
protocol methods, item types, status and JSON structure remained. The unconsumed full
fresh, feedback and transient recordings were removed; the selected events remain
with a stable fake thread identity. It inserts one synthetic contextCompaction event to
exercise re-prime without consuming a second provider session. The fixture executable
replays these frames and supplies scenario-specific RPC replies for subscription delays,
steer races, missing sessions and a disconnected turn/start. These synthetic cases are
not claims that the real gate observed those errors.

Sources: the installed CLI's generated experimental JSON schema and the official
[app-server contract](https://developers.openai.com/codex/app-server/). The initialize reply
does not advertise method version ranges, so a launch checks the schema each new CLI
generates (`codex app-server generate-json-schema --experimental`) for every request and
notification sluice uses, and a newer CLI runs untested (SPEC §15). 0.160.1 is tested too: its
generated schema is byte-identical to 0.160.0's, and `real_codex_wire_without_credentials`
(an invalid API key in a scratch home, no owner credential) ran the same launch, thread and
turn exchange on both with the same redacted wire.

The account scenarios (`limit-reached`, `usage-limit`, `rate-limit-soon`, `limit-words`,
`unauthorized`, `unauthorized-401`, `logged-out`) send frames shaped by the 0.160.0 generated
schema (`RateLimitSnapshot`, `RateLimitReachedType`, `ErrorNotification`, `TurnError`,
`CodexErrorInfo`, `GetAccountResponse`). Their messages are the owner's own Codex rollouts'
errors: `You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase
more credits or try again at <date>.` with `usage_limit_exceeded` (a weekly 10080-minute window
at 100 % and `has_credits` false just before it), `Your access token could not be refreshed
because your refresh token was revoked. Please log out and sign in again.` with `unauthorized`
(a live lash lane, 2026-10-05), and `unexpected status 401 Unauthorized: Missing bearer or basic
authentication in header, …` with `other`. No live wire captured a reached limit: the sluice
wires seen hold over 22000 updates, all with `rateLimitReachedType` null, some with a window at
100 % and `hasCredits` true while turns ran on. Wires now keep `codexErrorInfo` and
`rateLimitReachedType` unredacted.
