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
pub fn rpc_error(value: &Value) -> EngineError {
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
            for mut event in events {
                if scenario == "transient" && event["method"] == "turn/completed" {
                    event["params"]["turn"]["status"] = json!("failed");
                    event["params"]["turn"]["error"] = json!({"message":"rate limit", "code":429});
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

pub fn fixture_main(args: &[String]) -> io::Result<()> {
    if args == ["--version"] {
        println!("codex-cli 0.160.0");
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
