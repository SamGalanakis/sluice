//! Identities, owners and routing (SPEC §8.1) through `sluice tool` and a real coordinator:
//! a session names itself (`--me`, `SLUICE_ME`) before it speaks, `orchestrator` and `owner`
//! are not names a session takes or addresses, a recipient session must be registered and a
//! silent one is noted, a new unit belongs to the session that adds it, and the plan's export
//! never shows it.
#[allow(dead_code)]
#[path = "../../../tests/support/mod.rs"]
mod support;

use serde_json::{Value, json};
use sluice_model::{commands::CommandReply, rpc::decode_json};
use std::{
    path::Path,
    process::{Command, Output},
};
use support::home::ScratchHome;

/// `sluice tool <args>` as session `me` (`SLUICE_ME`), or anonymous.
fn tool(home: &Path, me: Option<&str>, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sluice"));
    command
        .env("SLUICE_HOME", home)
        .env_remove("SLUICE_STEP")
        .env_remove("SLUICE_AUTHOR")
        .env_remove("SLUICE_RUN_ID")
        .env_remove("SLUICE_PROJECT")
        .env_remove("SLUICE_PROJECT_ID")
        .env_remove("SLUICE_ME");
    if let Some(me) = me {
        command.env("SLUICE_ME", me);
    }
    command.arg("tool").args(args).output().unwrap()
}
fn ok(out: Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
/// The refusal's kind and message.
fn refused(out: Output) -> (String, String) {
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let error: Value = serde_json::from_slice(&out.stderr).unwrap_or_else(|e| {
        panic!("{e}: {}", String::from_utf8_lossy(&out.stderr));
    });
    (
        error["error"].as_str().unwrap().into(),
        error["message"].as_str().unwrap().into(),
    )
}
fn query(home: &Path, sql: &str) -> Vec<Value> {
    ok(tool(
        home,
        None,
        &["query", &json!({ "sql": sql }).to_string()],
    ))["rows"]
        .as_array()
        .unwrap()
        .clone()
}
fn messages(home: &Path) -> usize {
    query(home, "SELECT count(*) AS n FROM messages")[0][0]
        .as_u64()
        .unwrap() as usize
}
/// Project `p` with a pending step `q` (work done outside sluice).
fn project(home: &Path) {
    ok(tool(
        home,
        None,
        &["project_create", r#"{"name":"p","description":""}"#],
    ));
    ok(tool(
        home,
        None,
        &[
            "step_add",
            r#"{"project":"p","step":"q","spec":{"run":"core.external","outputs":{"ok":"boolean"}}}"#,
        ],
    ));
}

/// A session with no name may read but not speak: ask, say and reply are refused saying how
/// to name it, and nothing is stored; named, the same calls speak as it.
#[test]
fn a_session_names_itself_before_it_speaks() {
    let home = ScratchHome::new().unwrap();
    let home = home.path();
    project(home);
    for (name, args) in [
        ("say", json!({"project":"p","to":"q","body":"start"})),
        ("ask", json!({"project":"p","to":"owner","body":"ship?"})),
        ("reply", json!({"project":"p","to":"q","body":"yes"})),
    ] {
        let (kind, message) = refused(tool(home, None, &[name, &args.to_string()]));
        assert_eq!(kind, "invalid", "{name}");
        assert!(
            message.contains("name this session: pass me=@name or set SLUICE_ME"),
            "{name}: {message}"
        );
    }
    assert_eq!(messages(home), 0);
    // Reading needs no name.
    ok(tool(home, None, &["status", r#"{"project":"p"}"#]));
    ok(tool(home, None, &["messages", r#"{"project":"p"}"#]));
    // Named by SLUICE_ME or --me, the session speaks as itself.
    let said = ok(tool(
        home,
        Some("@lead"),
        &["say", r#"{"project":"p","to":"q","body":"start"}"#],
    ));
    assert_eq!(
        (said["to"].as_str(), said["thread"].as_str()),
        (Some("q"), Some("step-q"))
    );
    let asked = ok(tool(
        home,
        None,
        &[
            "ask",
            "--project",
            "p",
            "--to",
            "owner",
            "--body",
            "ship?",
            "--me",
            "@lead",
        ],
    ));
    // Between a session and the owner, the session's own thread.
    assert_eq!(asked["thread"], "@lead");
    let senders = query(
        home,
        "SELECT \"from\" AS f, \"to\" AS t FROM messages ORDER BY id",
    );
    assert_eq!(
        senders,
        vec![json!(["@lead", "q"]), json!(["@lead", "owner"])]
    );
}

/// `orchestrator` is no longer an address or a name, and only the dashboard is the owner.
#[test]
fn a_session_never_speaks_as_or_to_the_orchestrator_or_as_the_owner() {
    let home = ScratchHome::new().unwrap();
    let home = home.path();
    project(home);
    let to_orchestrator = refused(tool(
        home,
        Some("@lead"),
        &["say", r#"{"project":"p","to":"orchestrator","body":"x"}"#],
    ));
    assert_eq!(to_orchestrator.0, "invalid");
    assert!(
        to_orchestrator
            .1
            .contains("orchestrator is no longer an address"),
        "{}",
        to_orchestrator.1
    );
    for me in ["orchestrator", "owner", "lead", "@Lead"] {
        let (kind, message) = refused(tool(
            home,
            None,
            &["say", r#"{"project":"p","to":"q","body":"x"}"#, "--me", me],
        ));
        assert_eq!(kind, "invalid", "{me}: {message}");
    }
    let (_, as_owner) = refused(tool(
        home,
        None,
        &["ask", r#"{"project":"p","to":"q","body":"x","me":"owner"}"#],
    ));
    assert!(
        as_owner.contains("only the dashboard speaks as the owner"),
        "{as_owner}"
    );
    let (_, bare) = refused(tool(
        home,
        Some("lead"),
        &["say", r#"{"project":"p","to":"q","body":"x"}"#],
    ));
    assert!(bare.contains("me=@lead"), "{bare}");
    assert_eq!(messages(home), 0);
}

/// A recipient session must be registered in the project (a typo is refused, nothing
/// stored); one that is registered but not reading gets the message, and the sender's
/// receipt says nobody is reading it. `next` keeps a reader's heartbeat: reading, no note.
#[test]
fn a_recipient_session_is_checked_and_a_silent_one_noted() {
    let home = ScratchHome::new().unwrap();
    let home = home.path();
    project(home);
    let (kind, typo) = refused(tool(
        home,
        Some("@lead"),
        &["say", r#"{"project":"p","to":"@laed","body":"x"}"#],
    ));
    assert_eq!(kind, "invalid");
    assert!(
        typo.contains("@laed is not a session of project p"),
        "{typo}"
    );
    assert_eq!(messages(home), 0);
    // @peer registers by speaking once, then goes quiet.
    ok(tool(
        home,
        Some("@peer"),
        &["say", r#"{"project":"p","to":"owner","body":"here"}"#],
    ));
    let queued = ok(tool(
        home,
        Some("@lead"),
        &["ask", r#"{"project":"p","to":"@peer","body":"which db?"}"#],
    ));
    assert_eq!(queued["to"], "@peer");
    assert_eq!(queued["delivery"], "delivered");
    assert_eq!(queued["note"], "queued; @peer has no live reader");
    assert_eq!(queued["thread"], "@peer");
    // A wait beats @peer's heartbeat: it is reading now.
    let out = Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", home)
        .env_remove("SLUICE_RUN_ID")
        .env("SLUICE_ME", "@peer")
        .args(["next", "-p", "p", "--timeout", "0", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let live = ok(tool(
        home,
        Some("@lead"),
        &["say", r#"{"project":"p","to":"@peer","body":"postgres"}"#],
    ));
    assert!(live.get("note").is_none(), "{live}");
    // @peer answers by sender: the question to it is the one it picks.
    let replied = ok(tool(
        home,
        Some("@peer"),
        &["reply", r#"{"project":"p","to":"@lead","body":"postgres"}"#],
    ));
    assert_eq!(replied["to"], "@lead");
}

/// A unit's owner: a new unit belongs to the session that adds it (or the owner it names);
/// a step added to an existing unit takes that unit's owner, whoever adds it; a unit added
/// with no session named is unowned. The owner is runtime metadata: the exported plan, as a
/// pinned release decodes it, is the declaration and nothing more.
#[test]
fn a_new_unit_belongs_to_its_editor_and_the_export_never_shows_it() {
    let home = ScratchHome::new().unwrap();
    let home = home.path();
    project(home);
    let spec = |unit: &str| json!({"run":"core.external","outputs":{"ok":"boolean"},"tags":[format!("unit:{unit}")]});
    let add = |me: Option<&str>, step: &str, unit: &str, owner: Option<&str>| {
        let mut args = json!({"project":"p","step":step,"spec":spec(unit)});
        if let Some(owner) = owner {
            args["owner"] = json!(owner);
        }
        tool(home, me, &["step_add", &args.to_string()])
    };
    ok(add(Some("@lead"), "lane-a-work", "lane-a", None));
    ok(add(Some("@peer"), "lane-a-land", "lane-a", None));
    ok(add(Some("@lead"), "lane-b-work", "lane-b", Some("owner")));
    ok(add(None, "lane-c-work", "lane-c", None));
    let owners = query(
        home,
        "SELECT step_id AS s, owner AS o FROM steps ORDER BY position",
    );
    assert_eq!(
        owners,
        vec![
            json!(["q", null]),
            json!(["lane-a-work", "@lead"]),
            json!(["lane-a-land", "@lead"]),
            json!(["lane-b-work", "owner"]),
            json!(["lane-c-work", null]),
        ]
    );
    // Naming another owner for an existing unit is refused; nothing is added.
    let (kind, message) = refused(add(Some("@peer"), "lane-a-check", "lane-a", Some("@peer")));
    assert_eq!(kind, "invalid");
    assert!(
        message.contains("unit lane-a already exists, owned by @lead"),
        "{message}"
    );
    // Authors name the session.
    let authors = query(
        home,
        "SELECT author AS a FROM plan_edits WHERE project_id=(SELECT project_id FROM projects WHERE name='p') ORDER BY rev",
    );
    assert_eq!(
        authors.iter().map(|r| r[0].clone()).collect::<Vec<_>>(),
        vec![
            json!("cli"),
            json!("cli"),
            json!("@lead"),
            json!("@peer"),
            json!("@lead"),
            json!("cli")
        ]
    );
    // The export carries the declarations alone, and decodes as a pinned release's strict
    // reply decoder reads it.
    let raw = tool(
        home,
        None,
        &[
            "rpc",
            r#"{"command":"plan_get","args":{"project":{"kind":"name","value":"p"}}}"#,
        ],
    );
    assert!(
        raw.status.success(),
        "{}",
        String::from_utf8_lossy(&raw.stderr)
    );
    let reply: CommandReply = decode_json(raw.stdout.trim_ascii()).unwrap();
    let text = String::from_utf8_lossy(&raw.stdout);
    assert!(!text.contains("owner"), "{text}");
    let CommandReply::Plan(plan) = reply else {
        panic!("{text}")
    };
    let exported = serde_json::to_value(&plan.plan).unwrap();
    assert_eq!(exported["steps"]["lane-a-work"], spec("lane-a"));
}

/// `sluice next` as a named session, from the shell: its own units only, resuming where its
/// last batch ended (no cursor file), its output ending with the cursor after a blank line.
fn next_text(home: &Path, me: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", home)
        .env_remove("SLUICE_RUN_ID")
        .env("SLUICE_ME", me)
        .args(["next", "-p", "p", "--timeout", "0", "--settle", "0"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Units change hands through the tools (SPEC §8.1, "Claim and release"): an unnamed claim is
/// refused; a session takes an unowned unit, a reading owner's unit only by compare-and-set,
/// and hands it on; each handoff leaves a note in the inbox of the session it concerns, whose
/// own inbox it is; and each session's `next` shows its own units' records, resuming from its
/// own position.
#[test]
fn units_change_hands_through_the_tools() {
    let home = ScratchHome::new().unwrap();
    let home = home.path();
    project(home);
    for me in ["@lead", "@peer"] {
        ok(tool(
            home,
            Some(me),
            &["say", r#"{"project":"p","to":"q","body":"hello"}"#],
        ));
    }
    let (kind, message) = refused(tool(
        home,
        None,
        &["unit_claim", r#"{"project":"p","unit":"q"}"#],
    ));
    assert_eq!(kind, "invalid");
    assert!(message.contains("name this session"), "{message}");
    let took = ok(tool(
        home,
        Some("@lead"),
        &[
            "unit_claim",
            r#"{"project":"p","unit":"q","reason":"mine now"}"#,
        ],
    ));
    assert_eq!(
        took["units"],
        json!([{"unit":"q","from":null,"to":"@lead"}])
    );
    assert_eq!(took["notices"].as_array().unwrap().len(), 1);
    // @lead waits (and so is reading): @peer's plain claim is a conflict naming it.
    assert!(next_text(home, "@lead").starts_with("timeout seq "));
    let (kind, message) = refused(tool(
        home,
        Some("@peer"),
        &["unit_claim", r#"{"project":"p","unit":"q"}"#],
    ));
    assert_eq!(kind, "conflict");
    assert!(
        message.contains("unit q is owned by @lead, which is reading"),
        "{message}"
    );
    ok(tool(
        home,
        Some("@peer"),
        &[
            "unit_claim",
            r#"{"project":"p","units":"q*","expected_owner":"@lead","reason":"taking over"}"#,
        ],
    ));
    // The note is in @lead's own inbox, and not in @peer's.
    let inbox = |me: &str| {
        ok(tool(
            home,
            Some(me),
            &["messages", r#"{"project":"p","view":"inbox"}"#],
        ))["messages"]
            .as_array()
            .unwrap()
            .clone()
    };
    let lead = inbox("@lead");
    let note = lead
        .iter()
        .find(|m| m["from"] == "sluice")
        .unwrap_or_else(|| panic!("{lead:?}"));
    assert!(
        note["body"]
            .as_str()
            .unwrap()
            .starts_with("@peer claimed unit q from @lead (taking over)."),
        "{note}"
    );
    assert_eq!(note["data"]["unit_owner"]["units"][0]["to"], "@peer");
    assert!(inbox("@peer").iter().all(|m| m["from"] != "sluice"));
    // @peer hands it back; the step's failure is now @lead's to see, not @peer's.
    ok(tool(
        home,
        Some("@peer"),
        &["unit_release", r#"{"project":"p","unit":"q","to":"@lead"}"#],
    ));
    assert!(next_text(home, "@peer").starts_with("timeout seq "));
    ok(tool(
        home,
        Some("@lead"),
        &[
            "step_cancel",
            r#"{"project":"p","steps":"q","reason":"done elsewhere"}"#,
        ],
    ));
    let lead = next_text(home, "@lead");
    assert!(lead.contains("\n\nseq "), "{lead}");
    assert!(
        lead.lines()
            .any(|l| l.starts_with("STEP q pending -> failed")),
        "{lead}"
    );
    // @peer's own position: it resumes after its last batch and sees nothing of q's.
    let peer = next_text(home, "@peer");
    assert!(!peer.contains("STEP q"), "{peer}");
}
