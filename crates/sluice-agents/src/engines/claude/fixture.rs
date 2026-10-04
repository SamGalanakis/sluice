//! Deterministic fake Claude Code. Only used by the fixture executable in scratch homes.
use super::state::private_write;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

fn append(path: &Path, value: &Value) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.sync_data()
}
fn arg(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|v| v == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
struct Terminal(String);
impl Terminal {
    fn raw() -> io::Result<Self> {
        let old = Command::new("stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .output()?;
        if !old.status.success() {
            return Err(io::Error::other("fixture needs a PTY"));
        }
        let old = String::from_utf8_lossy(&old.stdout).trim().into();
        if !Command::new("stty")
            .args(["raw", "-echo"])
            .status()?
            .success()
        {
            return Err(io::Error::other("stty failed"));
        }
        print!("\x1b[?2004h");
        io::stdout().flush()?;
        Ok(Self(old))
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        print!("\x1b[?2004l");
        let _ = Command::new("stty").arg(&self.0).status();
    }
}
struct Background(Vec<Child>);
impl Drop for Background {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
struct Fake {
    home: PathBuf,
    run: PathBuf,
    settings: Value,
    config: Value,
    sid: String,
    transcript: PathBuf,
    status_file: PathBuf,
    cursor: usize,
    queue: std::collections::VecDeque<String>,
    active: Option<(Instant, Value)>,
    background: Background,
    notification: Option<Instant>,
    wakeup: bool,
    status: String,
    draft: String,
    paste: bool,
    input: Vec<u8>,
    drop_enters: u64,
    wrap: usize,
}
impl Fake {
    fn hook(&self, event: &str, extra: Value) -> io::Result<()> {
        let mut payload = json!({"session_id":self.sid,"transcript_path":self.transcript,"cwd":std::env::current_dir()?,"hook_event_name":event});
        if let Some(extra) = extra.as_object() {
            payload.as_object_mut().unwrap().extend(extra.clone());
        }
        append(&self.run.join("fixture-events.jsonl"), &payload)?;
        for group in self.settings["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
        {
            for hook in group["hooks"].as_array().into_iter().flatten() {
                if let Some(command) = hook["command"].as_str() {
                    let mut child = Command::new("/bin/sh")
                        .args(["-c", command])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()?;
                    child
                        .stdin
                        .take()
                        .unwrap()
                        .write_all(&serde_json::to_vec(&payload)?)?;
                    let output = child.wait_with_output()?;
                    append(
                        &self.run.join("fixture-hook-replies.jsonl"),
                        &json!({"event":event,"code":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout)}),
                    )?;
                }
            }
        }
        Ok(())
    }
    fn entry(&self, entry: Value) -> io::Result<()> {
        let mut value = json!({"cwd":std::env::current_dir()?,"sessionId":self.sid});
        value
            .as_object_mut()
            .unwrap()
            .extend(entry.as_object().unwrap().clone());
        append(&self.transcript, &value)
    }
    fn status(&mut self, status: &str) -> io::Result<()> {
        self.status = status.into();
        private_write(
            &self.status_file,
            &serde_json::to_vec(
                &json!({"pid":std::process::id(),"sessionId":self.sid,"kind":"interactive","status":status}),
            )?,
        )
    }
    fn draw(&self) -> io::Result<()> {
        let mut out = io::stdout().lock();
        write!(
            out,
            "\x1b[H\x1b[2J{}\r\n{}\r\n❯ ",
            if self.status == "busy" {
                "Working…"
            } else {
                ""
            },
            "─".repeat(70)
        )?;
        if self.draft.len() > 1000 {
            write!(out, "[Pasted text #1 +300 lines]")?;
        } else if self.wrap > 0 {
            for chunk in self.draft.chars().collect::<Vec<_>>().chunks(self.wrap) {
                write!(out, "\r\n  {}", chunk.iter().collect::<String>())?;
            }
        } else {
            write!(out, "{}", self.draft.replace('\n', "\r\n"))?;
        }
        write!(out, "\r\n{}\r\n", "─".repeat(70))?;
        out.flush()
    }
    fn feed(&mut self, bytes: &[u8]) -> io::Result<bool> {
        self.input.extend(bytes);
        while !self.input.is_empty() {
            if self.input.starts_with(b"\x1b[200~") {
                self.input.drain(..6);
                self.paste = true;
                continue;
            }
            if self.input.starts_with(b"\x1b[201~") {
                self.input.drain(..6);
                self.paste = false;
                continue;
            }
            if self.input[0] == 27 && self.input.len() < 6 {
                break;
            }
            let byte = self.input.remove(0);
            match byte {
                b'\r' if !self.paste => {
                    if self.drop_enters > 0 {
                        self.drop_enters -= 1;
                        continue;
                    }
                    let text = std::mem::take(&mut self.draft)
                        .trim_end_matches('\n')
                        .to_owned();
                    append(&self.run.join("fixture-prompts.jsonl"), &json!(text))?;
                    if text == "/exit" {
                        self.hook("SessionEnd", json!({}))?;
                        return Ok(false);
                    }
                    self.queue.push_back(text);
                }
                b'\r' => self.draft.push('\n'),
                1 => {}
                11 => self.draft.clear(),
                b if b >= 32 || b == b'\t' => self.draft.push(b as char),
                _ => {}
            }
        }
        self.draw()?;
        Ok(true)
    }
    fn tick(&mut self) -> io::Result<()> {
        if self.notification.is_some_and(|t| Instant::now() >= t) {
            self.notification = None;
            for child in &mut self.background.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
            self.background.0.clear();
            if self.wakeup {
                self.entry(json!({"type":"system","subtype":"scheduled_task_fire","content":"Claude resuming /loop wakeup"}))?;
            } else {
                self.entry(json!({"type":"user","origin":{"kind":"task-notification"},"message":{"content":"<summary>sleep finished</summary>"}}))?;
            }
            self.queue.push_back("background notification".into());
        }
        if self
            .active
            .as_ref()
            .is_some_and(|(at, _)| Instant::now() >= *at)
        {
            let (_, turn) = self.active.take().unwrap();
            if let Some(code) = turn["exit"].as_i64() {
                println!("{}", turn["stderr"].as_str().unwrap_or("engine exit"));
                std::process::exit(code as i32);
            }
            if let Some(command) = turn["run"].as_str() {
                let output = Command::new("/bin/sh").args(["-c", command]).output()?;
                if !output.status.success() {
                    return Err(io::Error::other("fixture turn command failed"));
                }
            }
            if let Some(submit) = turn.get("submit") {
                super::super::atomic_private(
                    &self.run.join("fixture-submission.json"),
                    &serde_json::to_vec(submit)?,
                )?;
            }
            if let Some(tool) = turn.get("tool") {
                self.hook("PreToolUse", json!({"tool_name":tool["name"]}))?;
                self.entry(json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":tool["name"],"input":tool["input"]}]}}))?;
                self.hook("PostToolUse", json!({"tool_name":tool["name"]}))?;
            }
            if let Some(error) = turn.get("tool_error") {
                self.entry(json!({"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":error,"tool_use_id":"t1"}]}}))?;
            }
            if turn["compact"].as_bool() == Some(true) {
                self.hook("SessionStart", json!({"source":"compact"}))?;
            }
            let delay = turn["background_s"].as_f64().or(turn["wakeup_s"].as_f64());
            if let Some(delay) = delay {
                self.notification = Some(Instant::now() + Duration::from_secs_f64(delay));
                self.wakeup = turn.get("wakeup_s").is_some();
                if self.wakeup {
                    let epoch = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs_f64()
                        + delay;
                    self.entry(json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"w1","name":"ScheduleWakeup","input":{}}]}}))?;
                    self.entry(json!({"type":"user","toolUseResult":{"scheduledFor":epoch*1000.0},"message":{"content":[{"type":"tool_result","tool_use_id":"w1"}]}}))?;
                } else {
                    self.background
                        .0
                        .push(Command::new("sleep").arg("90").spawn()?);
                }
            }
            let text = turn["error"]
                .as_str()
                .or(turn["reply"].as_str())
                .unwrap_or("ok");
            self.entry(json!({"type":"assistant","isApiErrorMessage":turn.get("error").is_some(),"message":{"content":[{"type":"text","text":text}]}}))?;
            self.hook(if turn.get("error").is_some() { "StopFailure" } else { "Stop" }, json!({"error":turn["error"],"last_assistant_message":text,
                "background_tasks":if self.background.0.is_empty(){json!([])}else{json!([{"id":"b1","type":"shell","status":"running","description":"sleep"}])},"session_crons":[]}))?;
            self.status(if self.background.0.is_empty() {
                "idle"
            } else {
                "shell"
            })?;
            self.draw()?;
        }
        if self.active.is_none()
            && let Some(text) = self.queue.pop_front()
        {
            let turn = self.config["turns"]
                .get(self.cursor)
                .cloned()
                .unwrap_or_else(|| json!({}));
            self.cursor += 1;
            private_write(
                &self.home.join("fixture.cursor"),
                self.cursor.to_string().as_bytes(),
            )?;
            self.status("busy")?;
            self.hook("UserPromptSubmit", json!({"prompt":format!("\n{text}")}))?;
            self.entry(json!({"type":"user","message":{"content":text}}))?;
            self.active = Some((
                Instant::now() + Duration::from_secs_f64(turn["busy_s"].as_f64().unwrap_or(0.1)),
                turn,
            ));
            self.draw()?;
        }
        Ok(())
    }
}
pub fn main(args: Vec<String>) -> io::Result<()> {
    if args.iter().any(|a| a == "--version") {
        println!("2.1.284 (Claude Code)");
        return Ok(());
    }
    let home = PathBuf::from(
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .ok_or_else(|| io::Error::other("missing private Claude home"))?,
    );
    let run = PathBuf::from(
        std::env::var_os("SLUICE_RUN_DIR").ok_or_else(|| io::Error::other("missing run dir"))?,
    );
    let settings: Value = serde_json::from_slice(&fs::read(
        arg(&args, "--settings").ok_or_else(|| io::Error::other("missing settings"))?,
    )?)?;
    let config: Value = serde_json::from_slice(&fs::read(
        std::env::var_os("SLUICE_FAKE_CLAUDE")
            .ok_or_else(|| io::Error::other("missing fixture config"))?,
    )?)?;
    private_write(&run.join("fixture-argv.json"), &serde_json::to_vec(&args)?)?;
    if let Some(code) = config["exit_at_start"].as_i64() {
        println!("{}", config["stderr"].as_str().unwrap_or_default());
        std::process::exit(code as i32);
    }
    let _terminal = Terminal::raw()?;
    if config["boot_ms"].as_u64().is_some() {
        std::thread::sleep(Duration::from_millis(config["boot_ms"].as_u64().unwrap()));
    }
    let sid = arg(&args, "--resume").unwrap_or_else(|| format!("fixture-{}", std::process::id()));
    let transcript = home
        .join("projects")
        .join("fixture")
        .join(format!("{sid}.jsonl"));
    if args.iter().any(|a| a == "--resume") && !transcript.exists() {
        println!("No conversation found with session ID");
        return Err(io::Error::other("missing session"));
    }
    fs::create_dir_all(transcript.parent().unwrap())?;
    fs::create_dir_all(home.join("sessions"))?;
    let status_file = home
        .join("sessions")
        .join(format!("{}.json", std::process::id()));
    let cursor = fs::read_to_string(home.join("fixture.cursor"))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let drop_enters = config["drop_enters"].as_u64().unwrap_or(0);
    let wrap = config["wrap"].as_u64().unwrap_or(0) as usize;
    let mut fake = Fake {
        home,
        run,
        settings,
        config,
        sid,
        transcript,
        status_file,
        cursor,
        queue: Default::default(),
        active: None,
        background: Background(vec![]),
        notification: None,
        wakeup: false,
        status: String::new(),
        draft: String::new(),
        paste: false,
        input: vec![],
        drop_enters,
        wrap,
    };
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        let mut bytes = [0; 4096];
        while let Ok(count) = input.read(&mut bytes) {
            if count == 0 || sender.send(bytes[..count].to_vec()).is_err() {
                break;
            }
        }
    });
    if fake.config["trust"].as_bool() == Some(true) {
        println!("\x1b[H\x1b[2JNo, exit\r\n❯ No, exit\r\n  Yes, I trust this folder\r\n");
        let mut selected = false;
        loop {
            let bytes = receiver.recv().map_err(io::Error::other)?;
            if bytes.windows(3).any(|b| b == b"\x1b[B") {
                selected = true;
                println!("\x1b[H\x1b[2JNo, exit\r\n❯ Yes, I trust this folder\r\n");
            }
            if bytes.contains(&b'\r') {
                if !selected {
                    return Err(io::Error::other("trust refused"));
                }
                break;
            }
        }
    }
    fake.entry(json!({"type":"permission-mode"}))?;
    fake.status("idle")?;
    fake.hook(
        "SessionStart",
        json!({"source":if args.iter().any(|a|a=="--resume"){"resume"}else{"startup"}}),
    )?;
    fake.draw()?;
    loop {
        match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(bytes) => {
                if !fake.feed(&bytes)? {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
        fake.tick()?;
    }
    let _ = fs::remove_file(&fake.status_file);
    Ok(())
}

/// Scratch-only hook relay for adapter acceptance tests. The production transport
/// is the authenticated guardian; this relay lets the isolated direct harness
/// answer actual CLI hooks without depending on coordinator dispatch.
pub fn hook_proxy(event: &str) -> io::Result<()> {
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixStream,
    };
    let mut payload = vec![];
    io::stdin()
        .take((super::protocol::MAX_EVENT_BYTES + 1) as u64)
        .read_to_end(&mut payload)?;
    if payload.len() > super::protocol::MAX_EVENT_BYTES {
        return Err(io::Error::other("oversized fixture hook"));
    }
    let payload: Value = serde_json::from_slice(&payload)?;
    super::protocol::decode_hook(event, &payload).map_err(io::Error::other)?;
    let socket = std::env::var_os("SLUICE_CLAUDE_HOOK_SOCKET")
        .ok_or_else(|| io::Error::other("missing private fixture hook socket"))?;
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut frame = serde_json::to_vec(&crate::engines::HookEvent {
        event: event.into(),
        payload,
    })?;
    frame.push(b'\n');
    stream.write_all(&frame)?;
    let mut bytes = vec![];
    BufReader::new(stream)
        .take((super::protocol::MAX_EVENT_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    let reply: crate::engines::HookReply = serde_json::from_slice(&bytes)?;
    if let Some(value) = reply.stdout {
        println!("{value}");
    }
    if reply.exit_code != 0 {
        return Err(io::Error::other("fixture hook rejected"));
    }
    Ok(())
}

/// Test-only submission endpoint for the isolated real-adapter gate.
pub fn submit(args: Vec<String>) -> io::Result<()> {
    if args.len() != 1 {
        return Err(io::Error::other("fixture submit needs one JSON argument"));
    }
    if args[0].len() > super::protocol::MAX_EVENT_BYTES {
        return Err(io::Error::other("oversized fixture submission"));
    }
    let value: Value = serde_json::from_str(&args[0])?;
    if !value.is_object() {
        return Err(io::Error::other("submission must be an object"));
    }
    let run = PathBuf::from(
        std::env::var_os("SLUICE_RUN_DIR").ok_or_else(|| io::Error::other("missing run dir"))?,
    );
    super::super::atomic_private(
        &run.join("fixture-submission.json"),
        &serde_json::to_vec(&value)?,
    )?;
    println!("{{\"status\":\"submitted\"}}");
    Ok(())
}
