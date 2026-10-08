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
    /// Since when `input` has held only the start of an escape sequence.
    escape_wait: Option<Instant>,
    drop_enters: u64,
    wrap: usize,
    /// When the composer last took an Escape, for Claude's 800 ms double press.
    escaped: Option<Instant>,
    /// The dialog over the composer (Rewind), which takes every key but Escape.
    modal: Option<&'static str>,
    /// Until when a screen covers the composer (`cover_ms`) and input waits unread
    /// (`hold_ms`), as while Claude 2.1.284 loads a long resumed session.
    cover_until: Option<Instant>,
    hold_until: Option<Instant>,
    held: Vec<u8>,
    /// Pastes still to drop with no draft (`swallow_pastes`, counted across launches in the
    /// home), and still to land under a Rewind dialog that opens as they arrive
    /// (`modal_on_paste`).
    swallow: u64,
    modal_on_paste: u64,
    /// The paste in progress is dropped.
    dropping: bool,
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
        if self.cover_until.is_some() {
            write!(out, "\x1b[H\x1b[2J  Resuming the conversation\u{2026}\r\n")?;
            return out.flush();
        }
        if let Some(title) = self.modal {
            // 2.1.284's Rewind over an empty conversation; its top edge takes the composer's.
            write!(
                out,
                "\x1b[H\x1b[2J{} \u{25d0} medium \u{b7} /effort \u{2594}\r\n   {title}\r\n\r\n   Nothing to rewind to yet.\r\n\r\n   Esc to cancel\r\n",
                "\u{2594}".repeat(60)
            )?;
            return out.flush();
        }
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
    fn log(&self, file: &str, value: Value) -> io::Result<()> {
        append(&self.run.join(file), &value)
    }
    /// Escape as 2.1.284 takes it: it closes a dialog; on the composer a second press within
    /// 800 ms opens Rewind (empty composer) or clears the draft.
    fn escape(&mut self) -> io::Result<()> {
        self.log("fixture-keys.jsonl", json!("Escape"))?;
        if self.modal.take().is_some() {
            return Ok(());
        }
        let now = Instant::now();
        if self
            .escaped
            .take()
            .is_some_and(|at| now.duration_since(at) <= Duration::from_millis(800))
        {
            if self.draft.is_empty() {
                self.modal = Some("Rewind");
                self.log("fixture-modals.jsonl", json!("Rewind"))?;
            } else {
                self.draft.clear();
            }
        } else {
            self.escaped = Some(now);
        }
        Ok(())
    }
    fn feed(&mut self, bytes: &[u8]) -> io::Result<bool> {
        if self.hold_until.is_some() {
            self.held.extend(bytes);
            return Ok(true);
        }
        self.input.extend(bytes);
        while !self.input.is_empty() {
            if self.input.starts_with(b"\x1b[200~") {
                self.input.drain(..6);
                self.paste = true;
                self.log("fixture-keys.jsonl", json!("paste"))?;
                if self.modal.is_none() && self.modal_on_paste > 0 {
                    self.modal_on_paste -= 1;
                    self.modal = Some("Rewind");
                    self.log("fixture-modals.jsonl", json!("Rewind"))?;
                }
                if self.modal.is_none() && self.swallow > 0 {
                    self.swallow -= 1;
                    let swallowed = fs::read_to_string(self.home.join("fixture.swallowed"))
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0);
                    private_write(
                        &self.home.join("fixture.swallowed"),
                        (swallowed + 1).to_string().as_bytes(),
                    )?;
                    self.dropping = true;
                }
                continue;
            }
            if self.input.starts_with(b"\x1b[201~") {
                self.input.drain(..6);
                self.paste = false;
                self.dropping = false;
                continue;
            }
            if self.input[0] == 27 {
                // The start of a paste's bracket waits for the rest; a lone ESC (tmux sends
                // `Escape` as one byte) is the Escape key once nothing follows within 50 ms.
                let partial = self.input.len() < 6
                    && [&b"\x1b[200~"[..], &b"\x1b[201~"[..]]
                        .iter()
                        .any(|seq| seq.starts_with(&self.input));
                if partial
                    && self.escape_wait.get_or_insert_with(Instant::now).elapsed()
                        < Duration::from_millis(50)
                {
                    break;
                }
                self.escape_wait = None;
                if self.input.len() >= 3 && self.input[1] == b'[' && !partial {
                    // An arrow key: Claude's composer has no use for it here.
                    self.input.drain(..3);
                    continue;
                }
                self.input.remove(0);
                self.escape()?;
                continue;
            }
            let byte = self.input.remove(0);
            if self.modal.is_some() || self.dropping {
                // A dialog takes the keys and the paste; a dropped paste leaves no draft.
                continue;
            }
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
                1 | 5 => {}
                11 => self.draft.clear(),
                // C-u clears the cursor's (the last) line; BSpace takes one character back,
                // a newline too.
                21 => {
                    let at = self.draft.rfind('\n').map_or(0, |at| at + 1);
                    self.draft.truncate(at);
                }
                127 => {
                    self.draft.pop();
                }
                b if b >= 32 || b == b'\t' => self.draft.push(b as char),
                _ => {}
            }
        }
        self.draw()?;
        Ok(true)
    }
    fn tick(&mut self) -> io::Result<bool> {
        if self.cover_until.is_some_and(|at| Instant::now() >= at) {
            self.cover_until = None;
            self.draw()?;
        }
        if self.hold_until.is_some_and(|at| Instant::now() >= at) {
            // Claude reads what waited all at once, so keys sent far apart arrive together.
            self.hold_until = None;
            let held = std::mem::take(&mut self.held);
            if !self.feed(&held)? {
                return Ok(false);
            }
        }
        if self.escape_wait.is_some() && !self.feed(&[])? {
            return Ok(false);
        }
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
            // An API error entry as 2.1.284 writes it: `error` is its category and a rejected
            // plan limit carries `quotaLimits` (`resets_in_s` becomes `resetsAt`).
            let mut entry = json!({"type":"assistant","isApiErrorMessage":turn.get("error").is_some(),"message":{"content":[{"type":"text","text":text}]}});
            if let Some(kind) = turn.get("error_type") {
                entry["error"] = kind.clone();
            }
            if let Some(quota) = turn.get("quota").and_then(Value::as_object) {
                let mut quota = quota.clone();
                if let Some(after) = quota.remove("resets_in_s").and_then(|s| s.as_u64()) {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(io::Error::other)?
                        .as_secs();
                    quota.insert("resetsAt".into(), json!(now + after));
                }
                entry["quotaLimits"] = Value::Object(quota);
                entry["apiErrorStatus"] = json!(429);
            }
            self.entry(entry)?;
            self.hook(if turn.get("error").is_some() { "StopFailure" } else { "Stop" }, json!({"error":turn.get("error_type").unwrap_or(&turn["error"]),"last_assistant_message":text,
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
        Ok(true)
    }
}
/// `claude --help`, cut to the flags sluice's probe reads, as 2.1.284 prints them.
pub const FIXTURE_HELP: &str = "Options:\n  --dangerously-skip-permissions\n  --disallowedTools, --disallowed-tools <tools...>\n  --effort <level>\n  --mcp-config <configs...>\n  --model <model>\n  -r, --resume [value]\n  --settings <file-or-json>\n  --strict-mcp-config\n";
pub fn main(args: Vec<String>) -> io::Result<()> {
    if args.iter().any(|a| a == "--version") {
        println!("2.1.284 (Claude Code)");
        return Ok(());
    }
    if args.iter().any(|a| a == "--help") {
        print!("{FIXTURE_HELP}");
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
    let swallowed = fs::read_to_string(home.join("fixture.swallowed"))
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let swallow = config["swallow_pastes"]
        .as_u64()
        .unwrap_or(0)
        .saturating_sub(swallowed);
    let modal_on_paste = config["modal_on_paste"].as_u64().unwrap_or(0);
    let after = |key: &str| {
        config[key]
            .as_u64()
            .map(|ms| Instant::now() + Duration::from_millis(ms))
    };
    let (cover_until, hold_until) = (after("cover_ms"), after("hold_ms"));
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
        escape_wait: None,
        drop_enters,
        wrap,
        escaped: None,
        modal: None,
        cover_until,
        hold_until,
        held: vec![],
        swallow,
        modal_on_paste,
        dropping: false,
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
                append(&fake.run.join("fixture-dialogs.jsonl"), &json!("trust"))?;
                break;
            }
        }
    }
    if fake.config["bypass"].as_bool() == Some(true) {
        // 2.1.284's warning under --dangerously-skip-permissions, until accepted.
        let warning = "\x1b[H\x1b[2J WARNING: Claude Code running in Bypass Permissions mode\r\n\r\n In Bypass Permissions mode, Claude Code will not ask for your approval before running potentially dangerous commands.\r\n\r\n";
        println!("{warning} \u{276f} 1. No, exit\r\n   2. Yes, I accept\r\n");
        let mut selected = false;
        loop {
            let bytes = receiver.recv().map_err(io::Error::other)?;
            if bytes.windows(3).any(|b| b == b"\x1b[B") {
                selected = true;
                println!("{warning}   1. No, exit\r\n \u{276f} 2. Yes, I accept\r\n");
            }
            if bytes.contains(&b'\r') {
                if !selected {
                    return Err(io::Error::other("bypass refused"));
                }
                append(&fake.run.join("fixture-dialogs.jsonl"), &json!("bypass"))?;
                break;
            }
        }
    }
    if let Some(screen) = fake.config["screen"].as_str() {
        // A screen Claude stops on instead of its composer, such as its first-run setup; it
        // stays until the run ends.
        print!("\x1b[H\x1b[2J{}", screen.replace('\n', "\r\n"));
        io::stdout().flush()?;
        while receiver.recv().is_ok() {}
        return Ok(());
    }
    if fake.config["login"].as_bool() == Some(true) {
        // Logged out, 2.1.284 opens on its login screen and waits there.
        println!(
            "\x1b[H\x1b[2J Select login method:\r\n\r\n \u{276f} 1. Claude account with subscription \u{b7} Pro, Max, Team, or Enterprise\r\n\r\n   2. Anthropic Console account \u{b7} API usage billing\r\n"
        );
        while receiver.recv().is_ok() {}
        return Ok(());
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
        if !fake.tick()? {
            break;
        }
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
