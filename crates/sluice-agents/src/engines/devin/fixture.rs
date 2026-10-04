//! Executable redacted Devin TUI fixture; no provider calls or live session imports.
use super::protocol;
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// The `hook_failures: "continue"` setting: like CLI 3000.11.3, log a hook command that
/// exits nonzero and carry on, instead of failing the fixture.
static CONTINUE_PAST_HOOK_FAILURES: AtomicBool = AtomicBool::new(false);

struct Raw(String);
impl Raw {
    fn new() -> io::Result<Self> {
        let saved = Command::new("/bin/stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .output()?;
        if !saved.status.success() {
            return Err(io::Error::other("fixture needs a tty"));
        }
        if !Command::new("/bin/stty")
            .args(["raw", "-echo"])
            .status()?
            .success()
        {
            return Err(io::Error::other("fixture cannot enter raw mode"));
        }
        print!("\x1b[?2004h");
        io::stdout().flush()?;
        Ok(Self(String::from_utf8_lossy(&saved.stdout).trim().into()))
    }
}
impl Drop for Raw {
    fn drop(&mut self) {
        let _ = Command::new("/bin/stty").arg(&self.0).status();
    }
}

fn hook(config: &Value, event: &str, sid: &str, data: Value) -> io::Result<()> {
    let mut payload = data.as_object().cloned().unwrap_or_default();
    payload.insert("hook_event_name".into(), event.into());
    payload.insert("session_id".into(), sid.into());
    if let Some(groups) = config["hooks"][event].as_array() {
        for group in groups {
            if let Some(hooks) = group["hooks"].as_array() {
                for item in hooks {
                    let Some(command) = item["command"].as_str() else {
                        continue;
                    };
                    let mut child = Command::new("/bin/sh")
                        .args(["-c", command])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .spawn()?;
                    child
                        .stdin
                        .take()
                        .unwrap()
                        .write_all(&serde_json::to_vec(&payload)?)?;
                    let status = child.wait()?;
                    if !status.success() {
                        if !CONTINUE_PAST_HOOK_FAILURES.load(Ordering::Relaxed) {
                            return Err(io::Error::other("fixture hook failed"));
                        }
                        eprintln!("fixture {event} hook command exited {status}; continuing");
                    }
                }
            }
        }
    }
    Ok(())
}
fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|v| v == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
fn draw(draft: &str, collapsed: bool, wrap: usize, dialog: bool, bypass: bool) -> io::Result<()> {
    let rule = "─".repeat(100);
    let region = if dialog {
        "Select a menu item".into()
    } else if draft.is_empty() {
        "❯ Ask Devin to build features, fix bugs, or work on your code".into()
    } else if collapsed {
        format!("❯ \r\n  [Pasted text #1 +{} lines]", draft.lines().count())
    } else if wrap > 0 {
        format!(
            "❯ \r\n{}",
            draft
                .chars()
                .collect::<Vec<_>>()
                .chunks(wrap)
                .map(|s| s.iter().collect::<String>())
                .collect::<Vec<_>>()
                .join("\r\n")
        )
    } else {
        format!("❯ {}", draft.lines().next().unwrap_or(""))
    };
    // 3000.11.3 layout: Normal mode has no indicator; bypass is a row above the composer.
    print!(
        "\x1b[H\x1b[2J{}{rule}\r\n{region}\r\n{rule}\r\nSWE-2 High \u{b7} Context: 0k / 262k tokens (0%)",
        if bypass {
            "\x1b[33m(bypass permissions on)\x1b[0m\r\n"
        } else {
            ""
        }
    );
    io::stdout().flush()
}

pub fn main(args: &[String]) -> io::Result<()> {
    if args.first().map(String::as_str) == Some("devin-submit") {
        let path = PathBuf::from(
            args.get(1)
                .ok_or_else(|| io::Error::other("missing submission path"))?,
        );
        sluice_process::host::guard_scratch_home(&path).map_err(io::Error::other)?;
        let value: Value = serde_json::from_str(
            args.get(2)
                .ok_or_else(|| io::Error::other("missing output JSON"))?,
        )?;
        if value
            .as_object()
            .is_none_or(|m| m.len() != 1 || m.get("word").and_then(Value::as_str).is_none())
        {
            return Err(io::Error::other("declared output word must be a string"));
        }
        protocol::private_write(&path, &serde_json::to_vec(&value)?)?;
        println!("accepted declared word output");
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("agent-hook") {
        return protocol::hook_main(
            args.get(2)
                .ok_or_else(|| io::Error::other("missing hook event"))?,
        );
    }
    let settings: Value = serde_json::from_slice(&fs::read(
        std::env::var_os("FAKE_DEVIN").ok_or_else(|| io::Error::other("missing fixture script"))?,
    )?)?;
    if args.iter().any(|s| s == "--version") {
        println!(
            "devin {} (redacted-fixture)",
            settings["version"]
                .as_str()
                .unwrap_or(super::profile::VERSION)
        );
        return Ok(());
    }
    if args.iter().any(|s| s == "--help") {
        println!("--config --export --model --resume --respect-workspace-trust");
        return Ok(());
    }
    CONTINUE_PAST_HOOK_FAILURES.store(
        settings["hook_failures"].as_str() == Some("continue"),
        Ordering::Relaxed,
    );
    let config: Value = serde_json::from_slice(&fs::read(
        arg(args, "--config").ok_or_else(|| io::Error::other("missing config"))?,
    )?)?;
    let sid = arg(args, "--resume").unwrap_or_else(|| "fixture-devin-session".into());
    // The pinned real release restores session mode after processing launch flags.
    let mut bypass =
        arg(args, "--resume").is_none() || settings["resume_bypass"].as_bool().unwrap_or(false);
    let export =
        PathBuf::from(arg(args, "--export").ok_or_else(|| io::Error::other("missing export"))?);
    let prompts = PathBuf::from(
        settings["prompts"]
            .as_str()
            .ok_or_else(|| io::Error::other("missing prompts path"))?,
    );
    let data = PathBuf::from(
        std::env::var_os("XDG_DATA_HOME")
            .ok_or_else(|| io::Error::other("missing private data home"))?,
    );
    fs::create_dir_all(data.join("devin/cli"))?;
    let cwd = std::env::current_dir()?;
    let query = format!(
        "CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY, working_directory TEXT); INSERT OR IGNORE INTO sessions VALUES('{}','{}');",
        sid.replace('\'', "''"),
        cwd.to_string_lossy().replace('\'', "''")
    );
    if !Command::new("/usr/bin/sqlite3")
        .arg(data.join("devin/cli/sessions.db"))
        .arg(query)
        .status()?
        .success()
    {
        return Err(io::Error::other("fixture session write failed"));
    }
    let _raw = Raw::new()?;
    if let Some(ms) = settings["boot_ms"].as_u64() {
        std::thread::sleep(Duration::from_millis(ms));
    }
    if !settings["omit_session_start"].as_bool().unwrap_or(false) {
        hook(&config, "SessionStart", &sid, json!({}))?;
    }
    let mut dialog = settings["dialog"].as_bool().unwrap_or(false);
    let mut draft = String::new();
    let mut bytes = Vec::new();
    let mut in_paste = false;
    let mut collapsed = false;
    let mut drop_enters = settings["drop_enters"].as_u64().unwrap_or(0);
    let wrap = settings["wrap"].as_u64().unwrap_or(0) as usize;
    let mut index = 0usize;
    let mut cursor = 0usize;
    if let Some(pane) = settings["ready_pane"].as_str() {
        print!("\x1b[H\x1b[2J{}", pane.replace('\n', "\r\n"));
        io::stdout().flush()?;
    } else if let Some(ms) = settings["restore_ms"].as_u64().filter(|_| bypass) {
        // A resumed session's mode is restored after the composer first appears.
        draw(&draft, collapsed, wrap, dialog, false)?;
        std::thread::sleep(Duration::from_millis(ms));
        draw(&draft, collapsed, wrap, dialog, bypass)?;
    } else {
        draw(&draft, collapsed, wrap, dialog, bypass)?;
    }
    loop {
        let mut buf = [0u8; 65536];
        let n = io::stdin().read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&buf[..n]);
        while cursor < bytes.len() {
            if in_paste {
                let Some(end) = bytes[cursor..].windows(6).position(|w| w == b"\x1b[201~") else {
                    break;
                };
                let text =
                    String::from_utf8_lossy(&bytes[cursor..cursor + end]).replace('\r', "\n");
                collapsed = text.len() > 800 || text.lines().count() > 3;
                draft.push_str(&text);
                cursor += end + 6;
                in_paste = false;
                continue;
            }
            if bytes[cursor] == 0x1b {
                if b"\x1b[200~".starts_with(&bytes[cursor..]) && bytes.len() - cursor < 6 {
                    break;
                }
                if bytes[cursor..].starts_with(b"\x1b[200~") {
                    cursor += 6;
                    in_paste = true;
                    continue;
                }
                dialog = false;
                cursor += 1;
                continue;
            }
            let b = bytes[cursor];
            cursor += 1;
            if b == 0x0b {
                draft.clear();
                collapsed = false;
            } else if b == b'\r' {
                if drop_enters > 0 {
                    drop_enters -= 1;
                    draft.push('\n');
                } else if draft.ends_with('\\') {
                    draft.pop();
                    draft.push('\n');
                } else if !draft.trim().is_empty() {
                    if draft.trim() == "/exit"
                        && settings["drop_exit_enter"].as_bool().unwrap_or(false)
                        && !draft.ends_with("\n")
                    {
                        draft.push('\n');
                        continue;
                    }
                    if draft.trim() == "/exit" {
                        hook(&config, "SessionEnd", &sid, json!({}))?;
                        return Ok(());
                    }
                    if draft.trim() == "/bypass" {
                        bypass = !bypass;
                        draft.clear();
                        collapsed = false;
                        continue;
                    }
                    if !bypass {
                        return Err(io::Error::other("resumed fixture requires tool approval"));
                    }
                    let mut file = fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&prompts)?;
                    writeln!(file, "{}", json!({"text":draft}))?;
                    let turn = settings["turns"]
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| json!({"reply":"ok"}));
                    let prompt_id = index.to_string();
                    index += 1;
                    if !turn["omit_ack"].as_bool().unwrap_or(false) {
                        hook(
                            &config,
                            "UserPromptSubmit",
                            &sid,
                            json!({"prompt":draft,"prompt_id":prompt_id}),
                        )?;
                    }
                    draft.clear();
                    collapsed = false;
                    if turn["tool"].as_bool().unwrap_or(false) {
                        hook(
                            &config,
                            "PreToolUse",
                            &sid,
                            json!({"tool_name":"exec", "tool_input":{"command":"echo done"}, "prompt_id":prompt_id}),
                        )?;
                    }
                    if turn["compact"].as_bool().unwrap_or(false) {
                        hook(
                            &config,
                            "PostCompaction",
                            &sid,
                            json!({"prompt_id":prompt_id}),
                        )?;
                    }
                    if let Some(ms) = turn["busy_ms"].as_u64() {
                        std::thread::sleep(Duration::from_millis(ms));
                    }
                    if let Some(submit) = turn.get("submit") {
                        protocol::private_write(
                            &cwd.join("submission.json"),
                            &serde_json::to_vec(submit)?,
                        )?;
                    }
                    if let Some(commit) = turn["commit"].as_str() {
                        fs::write(cwd.join("work.txt"), commit)?;
                        for args in [vec!["add", "work.txt"], vec!["commit", "-qm", commit]] {
                            if !Command::new("git").args(args).status()?.success() {
                                return Err(io::Error::other("fixture commit failed"));
                            }
                        }
                    }
                    let reply = turn["reply"].as_str().unwrap_or("ok");
                    if turn["exit_before_stop"].as_bool().unwrap_or(false) {
                        return Ok(());
                    }
                    hook(
                        &config,
                        "Stop",
                        &sid,
                        json!({"prompt_id":prompt_id,"last_assistant_message":reply,"error":turn.get("error")}),
                    )?;
                    protocol::private_write(
                        &export,
                        &serde_json::to_vec(
                            &json!({"session_id":sid,"steps":[{"source":"agent","message":reply}]}),
                        )?,
                    )?;
                    if turn["exit"].as_bool().unwrap_or(false) {
                        return Ok(());
                    }
                }
            } else if b >= b' ' {
                draft.push(b as char);
            }
        }
        bytes.drain(..cursor);
        cursor = 0;
        draw(&draft, collapsed, wrap, dialog, bypass)?;
    }
}
