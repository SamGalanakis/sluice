# Codex 0.160.0 wire fixtures

The three captured JSONL transcripts came from the single labelled g3_codex gate on
2026-10-03. Each frame passed through protocol::redact before it was written. Prose,
credentials, paths and session identities are removed. Protocol methods, item types,
status and JSON structure remain. The session prefix was 01a103b2.

turn.json selects the first captured turn's lifecycle, command and agent-message events,
with a stable fake thread identity. It inserts one synthetic contextCompaction event to
exercise re-prime without consuming a second provider session. The fixture executable
replays these frames and supplies scenario-specific RPC replies for subscription delays,
steer races, missing sessions and a disconnected turn/start. These synthetic cases are
not claims that the real gate observed those errors.

Sources: the installed CLI's generated experimental JSON schema and the official
[app-server contract](https://developers.openai.com/codex/app-server/). The checked CLI
version is exact because the initialize reply does not advertise method version ranges.

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
