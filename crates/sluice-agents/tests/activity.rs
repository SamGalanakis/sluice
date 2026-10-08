//! A run's activity outline read from real (trimmed, masked) Claude, Codex and Devin
//! transcripts: turns from what sluice sent, calls with their key argument and outcome, failures
//! flagged from the result itself, a resumed session's earlier records left to their run, and a
//! growing transcript read where it left off.
#[path = "fixtures/activity/lay.rs"]
mod lay;
use sluice_agents::activity::{Homes, Kind, Outcome, SentKind, Window, outline};
use std::path::Path;

fn laid(engine: &str) -> (tempfile::TempDir, Homes, String, lay::Laid) {
    let home = tempfile::tempdir().unwrap();
    let claude = home.path().join("claude");
    let run = sluice_model::ids::RunId::new().to_string();
    let laid = lay::lay(home.path(), &claude, engine, &run);
    let homes = Homes {
        sluice: home.path().into(),
        claude: Some(claude),
    };
    (home, homes, run, laid)
}
const ALL: Window = Window {
    since_ms: 0,
    until_ms: None,
};

#[test]
fn a_claude_transcript_reads_as_turns_from_each_thing_sluice_sent() {
    let (_home, homes, run, _) = laid("claude");
    let o = outline(&homes, &run, ALL).unwrap();
    assert_eq!(o.turns.len(), 8);
    assert_eq!(o.calls().count(), 34);
    assert_eq!(o.profile(), vec![("Bash".to_owned(), 34)]);
    // the task is read from the file sluice handed over, from its "## Task" section
    assert_eq!(o.turns[0].sent.kind, SentKind::Task);
    assert!(
        o.turns[0]
            .sent
            .text
            .starts_with("You work in the kiln fork")
    );
    // a short message typed in whole is its own words; a long one is read from its file
    assert_eq!(o.turns[1].sent.kind, SentKind::Text);
    assert!(
        o.turns[1]
            .sent
            .text
            .starts_with("Question 138953 from fig-5417-work")
    );
    assert_eq!(o.turns[2].sent.kind, SentKind::Message);
    assert!(
        o.turns[2]
            .sent
            .text
            .starts_with("Question 138961 from fig-5416-work")
    );
    // Claude Code's own notes (a background task's) open no turn
    assert!(o.turns.iter().all(|t| !t.sent.text.starts_with('<')));
    // a refused call and a non-zero exit are failures, from the result itself
    let failed: Vec<_> = o.calls().filter(|c| c.outcome == Outcome::Failed).collect();
    assert_eq!(failed.len(), 2);
    assert!(failed[0].result.starts_with("<tool_use_error>Blocked"));
    assert!(failed[1].result.starts_with("Exit code 128"));
    assert_eq!(failed[1].kind, Kind::Shell);
    assert!(
        failed[1]
            .key
            .starts_with("sed -n 300,460p crates/lash-core-store")
    );
    // every turn is timed; a queued message's turn starts once the last one's work is done
    for pair in o.turns.windows(2) {
        assert!(pair[1].started_ms >= pair[0].ended_ms, "{pair:?}");
    }
    // the agent's last words; the engine closed its last turn
    let last = o.turns.last().unwrap();
    assert!(last.said.starts_with("Rebased onto main"));
    assert!(last.closed);
    // a secret in a result is masked; long results keep their start and end
    let masked = o.calls().find(|c| c.result.contains("GH_TOKEN")).unwrap();
    assert!(
        masked.result.contains("GH_TOKEN=[redacted]"),
        "{}",
        masked.result
    );
    assert!(o.calls().all(|c| !c.result.contains("ghp_FAKE")));
    assert!(
        o.calls().any(|c| c.result_chars > c.result.chars().count()
            && c.result.contains("characters left out"))
    );
}

#[test]
fn a_codex_rollout_reads_its_commands_edits_and_searches() {
    let (_home, homes, run, _) = laid("codex");
    let o = outline(&homes, &run, ALL).unwrap();
    assert_eq!(o.turns.len(), 2);
    assert_eq!(o.turns[0].sent.kind, SentKind::Task);
    assert!(
        o.turns[0]
            .sent
            .text
            .starts_with("Read /workspace/notes/cenote/prospect-perms/reader-brief.md")
    );
    assert!(o.turns[1].sent.text.starts_with("Reply from orchestrator"));
    // a command's kind is what Codex parsed it as
    let read = o.calls().find(|c| c.tool == "read").unwrap();
    assert_eq!(read.kind, Kind::Read);
    assert!(read.key.starts_with("cat "));
    assert!(read.args.iter().any(|(k, v)| k == "exit code" && v == "0"));
    // a failed command (python not found, exit 127)
    let failed: Vec<_> = o.calls().filter(|c| c.outcome == Outcome::Failed).collect();
    assert_eq!(failed.len(), 1);
    assert!(failed[0].result.contains("python: command not found"));
    // a file change is an edit keyed by its path
    let edit = o.calls().find(|c| c.kind == Kind::Edit).unwrap();
    assert_eq!(
        edit.key,
        "/workspace/notes/cenote/prospect-perms/reader-enterprise.md"
    );
    assert!(
        o.calls()
            .any(|c| c.kind == Kind::Web && c.tool == "web search")
    );
    // the task's completion says its last words and closes it
    assert!(o.turns[1].said.starts_with("Wrote [reader-enterprise.md]"));
    assert!(o.turns[1].closed);
    assert!(
        o.calls()
            .all(|c| c.started_ms.is_some() && c.ended_ms.is_some())
    );
}

#[test]
fn a_devin_journal_pairs_each_call_with_its_result() {
    let (_home, homes, run, _) = laid("devin");
    let o = outline(&homes, &run, ALL).unwrap();
    assert_eq!(o.turns.len(), 4);
    assert_eq!(o.turns[0].sent.kind, SentKind::Task);
    assert_eq!(o.turns[1].sent.kind, SentKind::Message);
    assert!(
        o.turns[1]
            .sent
            .text
            .starts_with("Message from orchestrator")
    );
    assert_eq!(o.turns[3].sent.kind, SentKind::Text);
    assert!(
        o.turns[3]
            .sent
            .text
            .starts_with("Your turn ended but these outputs")
    );
    // a call whose result says it failed
    let failed: Vec<_> = o.calls().filter(|c| c.outcome == Outcome::Failed).collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].tool, "get_output");
    assert!(failed[0].result.starts_with("No shell with id `3a3711`"));
    // a call interrupted before its result has none
    assert_eq!(o.turns[0].calls.last().unwrap().outcome, Outcome::Running);
    // Stop's last words close each turn; the hook's time times it
    assert!(o.turns[1].said.starts_with("**Report: run killed"));
    assert!(o.turns[1].closed);
    assert!(o.turns.iter().all(|t| t.started_ms.is_some()));
    // sidekicks are agents, reads and execs their kinds
    assert!(
        o.calls()
            .any(|c| c.tool == "sidekick" && c.kind == Kind::Agent)
    );
    assert!(o.calls().any(|c| c.tool == "exec" && c.kind == Kind::Shell));
}

#[test]
fn a_resumed_sessions_earlier_records_belong_to_their_own_run() {
    let (_home, homes, run, _) = laid("claude");
    let all = outline(&homes, &run, ALL).unwrap();
    // a later run of the same session starts at the fifth turn
    let since = all.turns[4].started_ms.unwrap();
    let later = outline(
        &homes,
        &run,
        Window {
            since_ms: since,
            until_ms: None,
        },
    )
    .unwrap();
    assert_eq!(later.turns.len(), 4);
    assert_eq!(later.turns[0].sent, all.turns[4].sent);
    // and the earlier run ends where it began
    let earlier = outline(
        &homes,
        &run,
        Window {
            since_ms: 0,
            until_ms: Some(since),
        },
    )
    .unwrap();
    assert_eq!(earlier.turns.len(), 4);
}

#[test]
fn a_growing_transcript_is_read_where_it_left_off() {
    for engine in ["claude", "codex", "devin"] {
        let (_home, homes, run, laid) = laid(engine);
        let whole = outline(&homes, &run, ALL).unwrap();
        // the same run read as it grows, part by part, a line cut in two included
        let (_home2, homes2, run2, laid2) = self::laid(engine);
        let window = ALL;
        laid2.write(0);
        assert!(outline(&homes2, &run2, window).unwrap().turns.is_empty());
        let half = laid2.lines.len() / 2;
        laid2.write(half);
        let part = outline(&homes2, &run2, window).unwrap();
        assert!(part.turns.len() <= whole.turns.len());
        let mut text: String = laid2.lines[..half]
            .iter()
            .map(|l| format!("{l}\n"))
            .collect();
        let next = &laid2.lines[half];
        text.push_str(&next[..next.len() / 2]);
        std::fs::write(&laid2.transcript, &text).unwrap();
        outline(&homes2, &run2, window).unwrap();
        laid2.write(usize::MAX);
        let grown = outline(&homes2, &run2, window).unwrap();
        // the same outline as one read of the whole, but for where the run's files live
        let strip = |o: &sluice_agents::activity::Outline, dir: &Path| {
            format!("{o:?}").replace(&dir.to_string_lossy().into_owned(), "")
        };
        assert_eq!(
            strip(&grown, &homes2.sluice.join("runs").join(&run2)),
            strip(&whole, &homes.sluice.join("runs").join(&run)),
            "{engine}"
        );
        drop(laid);
    }
}

#[test]
fn a_run_with_no_transcript_has_no_outline() {
    let (_home, homes, run, laid) = laid("devin");
    assert!(outline(&homes, "not-a-run", ALL).is_none());
    std::fs::remove_file(&laid.transcript).unwrap();
    assert!(outline(&homes, &run, ALL).is_none());
    // a run its engine left no native state for is no agent run
    let other = sluice_model::ids::RunId::new().to_string();
    std::fs::create_dir_all(homes.sluice.join("runs").join(&other)).unwrap();
    assert!(outline(&homes, &other, ALL).is_none());
}
