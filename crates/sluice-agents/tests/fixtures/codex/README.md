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
