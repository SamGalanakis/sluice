use super::{
    super::{EngineError, EngineErrorKind},
    profile::error,
};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::Duration,
};
use tokio::{
    net::UnixStream,
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, timeout_at},
};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

/// Resuming a long thread returns its whole history in one message (5 MB seen live).
pub const MAX_WIRE_BYTES: usize = 256 * 1024 * 1024;
type Socket = WebSocketStream<UnixStream>;

pub struct Rpc {
    sink: SplitSink<Socket, Message>,
    incoming: mpsc::Receiver<Result<Value, EngineError>>,
    reader: JoinHandle<()>,
    events: VecDeque<Value>,
    sequence: u64,
    transcript: File,
    timeout: Duration,
    pending: bool,
    terminal_error: Option<EngineError>,
}
impl Rpc {
    pub async fn connect(
        socket: &Path,
        transcript: &Path,
        timeout: Duration,
    ) -> Result<Self, EngineError> {
        let stream = UnixStream::connect(socket).await.map_err(transport)?;
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_WIRE_BYTES))
            .max_frame_size(Some(MAX_WIRE_BYTES));
        let (socket, _) =
            tokio_tungstenite::client_async_with_config("ws://localhost/rpc", stream, Some(config))
                .await
                .map_err(transport)?;
        let (sink, mut source) = socket.split();
        let (sender, incoming) = mpsc::channel(128);
        let transcript = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(transcript)
            .map_err(transport)?;
        let reader = tokio::spawn(async move {
            while let Some(frame) = source.next().await {
                let decoded = match frame {
                    Ok(Message::Text(text)) => serde_json::from_str(&text).map_err(|_| {
                        error(EngineErrorKind::Fatal, "malformed Codex JSON-RPC frame")
                    }),
                    Ok(Message::Ping(_) | Message::Pong(_)) => continue,
                    Ok(Message::Close(_)) => break,
                    Ok(_) => Err(error(
                        EngineErrorKind::Fatal,
                        "unexpected Codex binary frame",
                    )),
                    Err(e) => Err(transport(e)),
                };
                let failed = decoded.is_err();
                if sender.send(decoded).await.is_err() || failed {
                    return;
                }
            }
            let _ = sender
                .send(Err(transport("Codex app-server disconnected")))
                .await;
        });
        Ok(Self {
            sink,
            incoming,
            reader,
            events: VecDeque::new(),
            sequence: 0,
            transcript,
            timeout,
            pending: false,
            terminal_error: None,
        })
    }
    fn record(&mut self, direction: &str, value: &Value) -> Result<(), EngineError> {
        writeln!(
            self.transcript,
            "{}",
            json!({"direction": direction, "frame": redact(value)})
        )
        .map_err(transport)
    }
    pub async fn notify(&mut self, method: &str) -> Result<(), EngineError> {
        let value = json!({"method":method});
        self.record("send", &value)?;
        self.sink
            .send(Message::Text(value.to_string().into()))
            .await
            .map_err(transport)
    }
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value, EngineError> {
        if self.pending {
            return Err(error(
                EngineErrorKind::UnknownAcceptance,
                "previous Codex request has unknown acceptance",
            ));
        }
        self.sequence += 1;
        let id = self.sequence;
        let value = json!({"id":id,"method":method,"params":params});
        self.record("send", &value)?;
        self.pending = true;
        let deadline = Instant::now() + self.timeout;
        let result = timeout_at(deadline, async {
            self.sink
                .send(Message::Text(value.to_string().into()))
                .await
                .map_err(transport)?;
            loop {
                let frame = self
                    .incoming
                    .recv()
                    .await
                    .ok_or_else(|| transport("Codex disconnected"))??;
                self.record("receive", &frame)?;
                if frame.get("id").and_then(Value::as_u64) == Some(id)
                    && frame.get("method").is_none()
                {
                    if let Some(failure) = frame.get("error") {
                        return Err(rpc_error(failure));
                    }
                    return frame.get("result").cloned().ok_or_else(|| {
                        error(
                            EngineErrorKind::UnknownAcceptance,
                            "Codex reply has no result",
                        )
                    });
                }
                if self.events.len() >= 1024 {
                    return Err(transport("Codex event backlog exceeded limit"));
                }
                self.events.push_back(frame);
            }
        })
        .await;
        match result {
            Ok(result) => {
                if result.is_ok()
                    || result
                        .as_ref()
                        .is_err_and(|e| e.kind != EngineErrorKind::UnknownAcceptance)
                {
                    self.pending = false;
                }
                result
            }
            Err(_) => Err(error(
                EngineErrorKind::UnknownAcceptance,
                format!("Codex {method} timed out; acceptance unknown"),
            )),
        }
    }
    pub fn drain(&mut self) -> Result<Vec<Value>, EngineError> {
        if let Some(e) = self.terminal_error.take() {
            return Err(e);
        }
        while let Ok(frame) = self.incoming.try_recv() {
            let frame = match frame {
                Ok(frame) => frame,
                Err(e) => {
                    self.terminal_error = Some(e);
                    break;
                }
            };
            self.record("receive", &frame)?;
            self.events.push_back(frame);
            if self.events.len() >= 1024 {
                break;
            }
        }
        Ok(self.events.drain(..).collect())
    }
    pub async fn close(&mut self) -> io::Result<()> {
        self.reader.abort();
        let joined = (&mut self.reader).await;
        if let Err(e) = joined
            && !e.is_cancelled()
        {
            return Err(io::Error::other(e));
        }
        Ok(())
    }
}
impl Drop for Rpc {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

fn transport(e: impl std::fmt::Display) -> EngineError {
    error(
        EngineErrorKind::UnknownAcceptance,
        format!("Codex transport: {e}"),
    )
}
/// Codex's own wording, as 0.160.0 prints it in an error object's `message`, at its start or
/// after a prefix such as `Error running remote compact task: `. Only error objects are read,
/// never an item, so the same words in tool output or the agent's prose cannot match.
fn says(message: &str, phrases: &[&str]) -> bool {
    let message = message.trim_start().replace('\u{2019}', "'");
    phrases
        .iter()
        .any(|p| message.starts_with(p) || message.contains(&format!(": {p}")))
}
/// A usage, plan or credit limit in Codex's words (`You've hit your usage limit. Visit
/// https://chatgpt.com/codex/settings/usage to purchase more credits or try again at Apr 8th,
/// 2026 10:13 AM.`).
pub fn usage_limit_text(message: &str) -> bool {
    says(
        message,
        &[
            "You've hit your usage limit",
            "You've reached your usage limit",
            "Usage limit reached",
            "Quota exceeded. Check your plan and billing details.",
            "Your workspace is out of credits.",
            "You're out of credits.",
            "You hit your spend cap",
            "To use Codex with your ChatGPT plan, upgrade to",
        ],
    )
}
/// An auth failure in Codex's words (`Your access token could not be refreshed because your
/// refresh token was revoked. Please log out and sign in again.`, `unexpected status 401
/// Unauthorized: Missing bearer or basic authentication in header, …`).
pub fn auth_text(message: &str) -> bool {
    says(
        message,
        &[
            "Your access token could not be refreshed",
            "Your authentication session could not be refreshed",
            "Your session has ended. Please log in again",
            "unexpected status 401 Unauthorized",
            "Token data is not available",
            "ChatGPT account ID not available, please re-run `codex login`",
            "Not signed in. Please run 'codex login'",
            "Not logged in",
        ],
    )
}
/// A JSON-RPC or turn error object, from its text alone: an auth failure or a usage limit in
/// Codex's words (a hard cap: its reset is unknown here), else `plain_rpc_error`. The adapter
/// reads a turn error's structured `codexErrorInfo` first.
pub fn rpc_error(value: &Value) -> EngineError {
    use super::super::account::{Auth, Cap, Engine, Limit, now};
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Codex RPC error");
    if auth_text(message) {
        return Auth::login(Engine::Codex, message).error();
    }
    if usage_limit_text(message) {
        return Limit::new(Engine::Codex, Cap::Usage, message).classify(now(), Duration::ZERO);
    }
    plain_rpc_error(value)
}
/// An error object classified by keywords only, as for an error Codex retries itself.
pub fn plain_rpc_error(value: &Value) -> EngineError {
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Codex RPC error");
    let lower = message.to_lowercase();
    let kind = if ["no rollout found", "thread not found", "missing session"]
        .iter()
        .any(|s| lower.contains(s))
    {
        EngineErrorKind::MissingSession
    } else if [
        "rate limit",
        "rate_limit",
        "429",
        "capacity",
        "overloaded",
        "usage limit",
    ]
    .iter()
    .any(|s| lower.contains(s))
    {
        EngineErrorKind::Transient
    } else if value.get("code").and_then(Value::as_i64) == Some(-32601)
        || lower.contains("experimentalapi")
    {
        EngineErrorKind::CapabilityMismatch
    } else {
        EngineErrorKind::Fatal
    };
    error(kind, message)
}

/// Preserve protocol structure while removing prose, credentials, paths and session identities.
pub fn redact(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let safe = matches!(
                        key.as_str(),
                        "method"
                            | "type"
                            | "status"
                            | "code"
                            | "experimentalApi"
                            | "model"
                            | "effort"
                            | "approvalPolicy"
                            | "sandbox"
                            | "direction"
                            | "phase"
                            | "codexErrorInfo"
                            | "rateLimitReachedType"
                    );
                    let value = if value.is_string() {
                        if safe {
                            value.clone()
                        } else {
                            Value::String("[redacted]".into())
                        }
                    } else {
                        redact(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact).collect()),
        Value::String(_) => Value::String("[redacted]".into()),
        _ => value.clone(),
    }
}

/// Executable fake engine used by fixture.rs and tests. The actual adapter still uses Unix WebSockets.
pub async fn fixture_server(socket: &Path, scenario: &str) -> io::Result<()> {
    let listener = tokio::net::UnixListener::bind(socket)?;
    let (stream, _) = listener.accept().await?;
    let mut ws = tokio_tungstenite::accept_async(stream)
        .await
        .map_err(io::Error::other)?;
    let mut turns = 0;
    let mut active = String::new();
    let mut subscriptions = 0;
    while let Some(frame) = ws.next().await {
        let frame = frame.map_err(io::Error::other)?;
        if !frame.is_text() {
            continue;
        }
        let request: Value = serde_json::from_str(frame.to_text().map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
        let Some(id) = request.get("id") else {
            if scenario == "tui" && request["method"] == "initialized" {
                ws.send(Message::Text(
                    json!({"method":"thread/started","params":{"thread":{"id":"fixture-thread"}}})
                        .to_string()
                        .into(),
                ))
                .await
                .map_err(io::Error::other)?;
            }
            continue;
        };
        let method = request["method"].as_str().unwrap_or("");
        let mut failure = None;
        let result = match method {
            "initialize" => json!({"userAgent":"codex-cli/0.160.0"}),
            "thread/start" => {
                json!({"thread":{"id":"fixture-thread","cwd": std::env::current_dir()?}})
            }
            "thread/resume" => {
                subscriptions += 1;
                if scenario == "empty-rollout" && subscriptions < 3 {
                    failure = Some(json!({"code":-32603,"message":"rollout is empty"}));
                }
                if scenario == "missing" {
                    failure = Some(json!({"code":-32603,"message":"no rollout found"}));
                }
                json!({"thread":{"id":request["params"]["threadId"],"cwd":std::env::current_dir()?,"turns":[{"id":"history","status":"completed"}]}})
            }
            "thread/read" => {
                json!({"thread":{"id":"fixture-thread","status":{"type": if scenario == "race-busy" { "active" } else { "idle" }}}})
            }
            "turn/start" => {
                turns += 1;
                active = format!("turn-{turns}");
                if scenario == "uncertain" {
                    ws.close(None).await.map_err(io::Error::other)?;
                    return Ok(());
                }
                json!({"turn":{"id":active,"status":"inProgress"}})
            }
            "turn/steer" => {
                if scenario.starts_with("race-") {
                    failure = Some(json!({"code":-32602,"message":"no active turn"}));
                }
                json!({"turnId":active})
            }
            "account/read" if scenario == "logged-out" => {
                json!({"account":null,"requiresOpenaiAuth":true})
            }
            "account/read" => {
                json!({"account":{"type":"chatgpt","email":null,"planType":"pro"},"requiresOpenaiAuth":true})
            }
            "turn/interrupt" | "thread/unsubscribe" => json!({}),
            _ => {
                failure = Some(json!({"code":-32601,"message":"method not found"}));
                json!({})
            }
        };
        let reply = match failure {
            Some(failure) => json!({"id":id,"error":failure}),
            None => json!({"id":id,"result":result}),
        };
        if method == "initialize" && scenario == "capability-mismatch" {
            ws.send(Message::Text(
                json!({"id":id,"error":{"code":-32601,"message":"experimentalApi unavailable"}})
                    .to_string()
                    .into(),
            ))
            .await
            .map_err(io::Error::other)?;
            continue;
        }
        ws.send(Message::Text(reply.to_string().into()))
            .await
            .map_err(io::Error::other)?;
        if method == "turn/start" {
            let events: Vec<Value> =
                serde_json::from_str(include_str!("../../../tests/fixtures/codex/turn.json"))
                    .map_err(io::Error::other)?;
            let limited = scenario == "rate-limit-soon" && !rate_limited_before()?;
            for event in limit_events(scenario, &active, limited)? {
                ws.send(Message::Text(event.to_string().into()))
                    .await
                    .map_err(io::Error::other)?;
            }
            if matches!(
                scenario,
                "limit-reached" | "usage-limit" | "unauthorized" | "unauthorized-401"
            ) {
                // Out of quota or logged out the turn does no work; `limit-reached` sends
                // nothing more.
                continue;
            }
            for mut event in events {
                if scenario == "transient" && event["method"] == "turn/completed" {
                    event["params"]["turn"]["status"] = json!("failed");
                    event["params"]["turn"]["error"] = json!({"message":"rate limit", "code":429});
                }
                if limited {
                    if event["method"] != "turn/completed" {
                        continue;
                    }
                    event["params"]["turn"]["status"] = json!("failed");
                    event["params"]["turn"]["error"] = json!({"message":"Rate limit reached for gpt-6.1-sol. Please try again in 2s.","codexErrorInfo":"rateLimitExceeded","additionalDetails":null});
                }
                if scenario == "limit-words" && event["method"] == "item/completed" {
                    // Codex's own limit and login wording, quoted by the agent and printed by a
                    // command.
                    let words = ACCOUNT_WORDS_FIXTURE;
                    match event["params"]["item"]["type"].as_str() {
                        Some("agentMessage") => event["params"]["item"]["text"] = json!(words),
                        Some("commandExecution") => {
                            event["params"]["item"]["aggregatedOutput"] = json!(words)
                        }
                        _ => {}
                    }
                }
                event["params"]["turnId"] = json!(active);
                if event["params"].get("turn").is_some() {
                    event["params"]["turn"]["id"] = json!(active);
                }
                if (scenario == "busy" || scenario.starts_with("race-"))
                    && event["method"] == "turn/completed"
                {
                    continue;
                }
                ws.send(Message::Text(event.to_string().into()))
                    .await
                    .map_err(io::Error::other)?;
            }
        }
    }
    Ok(())
}

/// The auth failure 0.160.0 sent live when the refresh token was revoked (a lash lane's rollout,
/// 2026-10-05, `codex_error_info` `unauthorized`).
pub const REVOKED_FIXTURE: &str = "Your access token could not be refreshed because your refresh token was revoked. Please log out and sign in again.";
/// The 401 0.160.0 sent live with no credentials, `codex_error_info` `other` (ids redacted);
/// `unauthorized-401` adds a bearer token to it, which the message must mask.
pub const UNAUTHORIZED_FIXTURE: &str = "unexpected status 401 Unauthorized: Missing bearer or basic authentication in header, url: https://api.openai.com/v1/responses, cf-ray: a3f548979a7d55c4-PRG, request id: req_07f626009a4646dfb34bb8ac40e63cb7";
/// The usage-limit message 0.160.0 sends (the shape of the live rollouts' errors).
pub const USAGE_LIMIT_FIXTURE: &str = "You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at Oct 12th, 2026 3:04 PM.";
/// Codex's usage-limit and revoked-token wording together, as an agent might quote them.
pub const ACCOUNT_WORDS_FIXTURE: &str = "You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at Oct 12th, 2026 3:04 PM. Your access token could not be refreshed because your refresh token was revoked. Please log out and sign in again.";
/// The limit events a scenario sends right after `turn/start`'s reply, shaped as 0.160.0's
/// `account/rateLimits/updated` (`RateLimitSnapshot`) and `error` (`ErrorNotification`):
/// - `limit-reached`: the weekly window used up, resetting in six days, with
///   `rateLimitReachedType`; the turn then stays open with no further event.
/// - `usage-limit`: `usageLimitExceeded` with no window, then the failed turn.
/// - `rate-limit-soon`: the 5-hour window used up, resetting in two seconds; the turn then
///   fails with `rateLimitExceeded`, once per Codex home.
/// - `limit-words`: an ordinary update (70 % used, nothing reached) and one with a window used
///   up but no limit reached, as when credits carry on.
/// - `unauthorized`: the live revoked-token failure (`unauthorized`), then the failed turn;
///   `unauthorized-401` the live 401 text with `other`, a bearer token appended.
fn limit_events(scenario: &str, turn: &str, limited: bool) -> io::Result<Vec<Value>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs();
    let update = |used: u64, minutes: u64, resets: u64, reached: Value| json!({"method":"account/rateLimits/updated","params":{"rateLimits":{"limitId":"codex","limitName":null,"normalModelSlug":null,"primary":{"usedPercent":used,"windowDurationMins":minutes,"resetsAt":now + resets},"secondary":null,"credits":{"hasCredits":false,"unlimited":false,"balance":"0"},"individualLimit":null,"spendControlReached":null,"planType":"pro","rateLimitReachedType":reached}}});
    let started = json!({"method":"turn/started","params":{"threadId":"fixture-thread","turn":{"id":turn,"items":[],"status":"inProgress","error":null}}});
    Ok(match scenario {
        "limit-reached" => vec![
            started,
            update(100, 10080, 6 * 86400, json!("rate_limit_reached")),
        ],
        "usage-limit" => {
            let error = json!({"message":USAGE_LIMIT_FIXTURE,"codexErrorInfo":"usageLimitExceeded","additionalDetails":null});
            vec![
                started,
                json!({"method":"error","params":{"error":error,"willRetry":false,"threadId":"fixture-thread","turnId":turn}}),
                json!({"method":"turn/completed","params":{"threadId":"fixture-thread","turn":{"id":turn,"items":[],"status":"failed","error":error}}}),
            ]
        }
        "unauthorized" | "unauthorized-401" => {
            let error = if scenario == "unauthorized" {
                json!({"message":REVOKED_FIXTURE,"codexErrorInfo":"unauthorized","additionalDetails":null})
            } else {
                json!({"message":format!("{UNAUTHORIZED_FIXTURE}, sent Authorization: Bearer eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJmaXh0dXJlIn0.c2lnbmF0dXJlLWZpeHR1cmU"),"codexErrorInfo":"other","additionalDetails":null})
            };
            vec![
                started,
                json!({"method":"thread/status/changed","params":{"threadId":"fixture-thread","status":{"type":"systemError"}}}),
                json!({"method":"error","params":{"error":error,"willRetry":false,"threadId":"fixture-thread","turnId":turn}}),
                json!({"method":"turn/completed","params":{"threadId":"fixture-thread","turn":{"id":turn,"items":[],"status":"failed","error":error}}}),
            ]
        }
        "rate-limit-soon" if limited => {
            vec![update(100, 300, 2, json!("rate_limit_reached"))]
        }
        "limit-words" => vec![
            update(70, 10080, 6 * 86400, Value::Null),
            update(100, 10080, 6 * 86400, Value::Null),
        ],
        _ => vec![],
    })
}
/// `rate-limit-soon` limits only the first turn in a Codex home, so the retry resumes.
fn rate_limited_before() -> io::Result<bool> {
    let marker =
        Path::new(&std::env::var_os("CODEX_HOME").unwrap_or_default()).join("fixture-rate-limited");
    if marker.exists() {
        return Ok(true);
    }
    std::fs::write(marker, b"")?;
    Ok(false)
}

/// `codex debug models` as the real CLI printed it (each model's slug and reasoning efforts),
/// for launch validation against the fixture.
pub const FIXTURE_MODELS: &str = include_str!("models.json");

pub fn fixture_main(args: &[String]) -> io::Result<()> {
    if args == ["--version"] {
        println!("codex-cli 0.160.0");
        return Ok(());
    }
    if args == ["debug", "models"] {
        print!("{FIXTURE_MODELS}");
        return Ok(());
    }
    if args.first().map(String::as_str) != Some("app-server") {
        return Err(io::Error::other("fake codex expects app-server"));
    }
    let url = args
        .get(2)
        .ok_or_else(|| io::Error::other("missing fake codex socket"))?;
    let socket = url
        .strip_prefix("unix://")
        .ok_or_else(|| io::Error::other("fake codex requires Unix transport"))?;
    let scenario = std::env::var("SLUICE_CODEX_FIXTURE").unwrap_or_else(|_| "normal".into());
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(fixture_server(Path::new(socket), &scenario))
}
