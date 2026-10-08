//! Lays a fixture transcript out as its engine leaves it: the run's directory (its `native.json`,
//! invocations, task and handed-over messages) under a sluice home, and the transcript where the
//! engine keeps it (Claude's config home, the run's Codex home, Devin's invocation). The fixtures
//! are real runs' transcripts, trimmed and masked; `{RUN_DIR}` stands for the run's directory.
#![allow(dead_code)]
use std::{
    fs,
    path::{Path, PathBuf},
};

const ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../sluice-agents/tests/fixtures/activity"
);
pub const CLAUDE_SESSION: &str = "0340e4b7-f358-47c6-bab9-7d6f661029a7";
pub const CLAUDE_INVOCATION: &str = "01a11afc-9e75-7190-9aa1-69a63f057829";
pub const CODEX_SESSION: &str = "01a11b9b-1eaf-7db2-9d93-c35331e86f78";
pub const DEVIN_INVOCATION: &str = "01a10a48-0c4b-7982-a733-70486e0a7418";

/// A laid-out run: its transcript's path and the fixture's lines, `{RUN_DIR}` filled in.
pub struct Laid {
    pub transcript: PathBuf,
    pub lines: Vec<String>,
}
impl Laid {
    /// Writes the transcript's first `n` lines (all of them for `usize::MAX`).
    pub fn write(&self, n: usize) {
        let text: String = self
            .lines
            .iter()
            .take(n)
            .map(|l| format!("{l}\n"))
            .collect();
        fs::write(&self.transcript, text).unwrap();
    }
}

fn copy_dir(from: &Path, to: &Path, run_dir: &str) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            copy_dir(&path, &to.join(entry.file_name()), run_dir);
        } else if path.extension().is_some_and(|e| e == "md") {
            let text = fs::read_to_string(&path).unwrap();
            fs::write(
                to.join(entry.file_name()),
                text.replace("{RUN_DIR}", run_dir),
            )
            .unwrap();
        }
    }
}
fn lines(file: &str, run_dir: &str) -> Vec<String> {
    fs::read_to_string(format!("{ROOT}/{file}"))
        .unwrap()
        .lines()
        .map(|l| l.replace("{RUN_DIR}", run_dir))
        .collect()
}

/// Lays `engine`'s fixture out as run `run` under sluice home `home` (and Claude's config home
/// `claude`), its whole transcript written.
pub fn lay(home: &Path, claude: &Path, engine: &str, run: &str) -> Laid {
    let run_dir = home.join("runs").join(run);
    let dir = run_dir.to_string_lossy().into_owned();
    fs::create_dir_all(&run_dir).unwrap();
    let laid = match engine {
        "claude" => {
            let invocation = run_dir.join("invocations").join(CLAUDE_INVOCATION);
            copy_dir(&Path::new(ROOT).join("claude"), &invocation, &dir);
            fs::write(
                run_dir.join("native.json"),
                format!(r#"{{"engine":"claude","session":"{CLAUDE_SESSION}","state":"Done"}}"#),
            )
            .unwrap();
            fs::write(
                invocation.join("native.json"),
                format!(r#"{{"engine":"claude","session":"{CLAUDE_SESSION}"}}"#),
            )
            .unwrap();
            let project = claude
                .join("projects")
                .join("-workspace-kiln-lash-forks-fig-5415");
            fs::create_dir_all(&project).unwrap();
            Laid {
                transcript: project.join(format!("{CLAUDE_SESSION}.jsonl")),
                lines: lines("claude/transcript.jsonl", &dir),
            }
        }
        "codex" => {
            copy_dir(&Path::new(ROOT).join("codex"), &run_dir, &dir);
            fs::write(
                run_dir.join("native.json"),
                format!(r#"{{"engine":"codex","session":"{CODEX_SESSION}","state":"Done"}}"#),
            )
            .unwrap();
            let sessions = home
                .join("codex-native-homes")
                .join(format!("pending-{run}"))
                .join("sessions/2026/10/08");
            fs::create_dir_all(&sessions).unwrap();
            Laid {
                transcript: sessions
                    .join(format!("rollout-2026-10-08T15-01-53-{CODEX_SESSION}.jsonl")),
                lines: lines("codex/rollout.jsonl", &dir),
            }
        }
        "devin" => {
            let invocation = run_dir.join("invocations").join(DEVIN_INVOCATION);
            copy_dir(&Path::new(ROOT).join("devin"), &invocation, &dir);
            fs::write(
                run_dir.join("native.json"),
                r#"{"engine":"devin","session":"thread-saxophone","state":"Done"}"#,
            )
            .unwrap();
            Laid {
                transcript: invocation.join("devin-hooks.jsonl"),
                lines: lines("devin/devin-hooks.jsonl", &dir),
            }
        }
        other => panic!("no fixture for {other}"),
    };
    laid.write(usize::MAX);
    laid
}
