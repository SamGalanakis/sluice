use sluice_agents::{delivery::*, engines::*, git, supervisor::*};
use sluice_model::{ids::*, rpc::JsonValue};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("sluice-test-session-{}", RunId::new()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn git_cmd(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(path)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
fn repo(path: &Path) {
    fs::create_dir(path).unwrap();
    git_cmd(path, &["init", "-q", "-b", "main"]);
    git_cmd(path, &["config", "user.name", "Test"]);
    git_cmd(path, &["config", "user.email", "test@example.com"]);
    fs::write(path.join("seed"), "seed").unwrap();
    git_cmd(path, &["add", "seed"]);
    git_cmd(path, &["commit", "-q", "-m", "Create seed"]);
}

#[test]
fn delivery_checkpoint_marks_interrupted_offers_uncertain_and_preserves_acks() {
    let mut ledger = DeliveryLedger::default();
    ledger.enqueue(InputId::Task, "task".into()).unwrap();
    ledger
        .enqueue(InputId::Message { id: MessageId(1) }, "msg".into())
        .unwrap();
    ledger.offer(&InputId::Task).unwrap();
    ledger
        .outcome(&InputId::Task, DeliveryOutcome::Acknowledged)
        .unwrap();
    ledger
        .offer(&InputId::Message { id: MessageId(1) })
        .unwrap();
    let bytes = serde_json::to_vec(&ledger).unwrap();
    let mut resumed: DeliveryLedger = serde_json::from_slice(&bytes).unwrap();
    resumed.recover();
    assert!(resumed.uncertain());
    assert!(resumed.next().is_none());
    assert_eq!(resumed.entries[0].state, DeliveryState::Acknowledged);
}

#[test]
fn session_lock_keys_are_hashes_not_lossy_path_sanitization() {
    let scratch = Scratch::new();
    let run = RunId::new();
    let mut guard = SessionGuard::default();
    guard.acquire(&scratch.0, "fake", "a/b", "p", run).unwrap();
    guard.acquire(&scratch.0, "fake", "a_b", "p", run).unwrap();
    assert_eq!(fs::read_dir(scratch.0.join("locks")).unwrap().count(), 4);
    let value = JsonValue::try_from(serde_json::json!({"ok":true})).unwrap();
    assert!(value.as_value()["ok"].as_bool().unwrap());
}
#[tokio::test]
async fn git_sampling_never_rewrites_the_index_under_an_agent() {
    let scratch = Scratch::new();
    let work = scratch.0.join("work");
    repo(&work);
    // Same content, newer stat: a locking `git status` refreshes and rewrites the index.
    let seed = fs::File::options()
        .write(true)
        .open(work.join("seed"))
        .unwrap();
    seed.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    let index = fs::read(work.join(".git/index")).unwrap();
    git::sample(&work).await.unwrap().unwrap();
    assert_eq!(fs::read(work.join(".git/index")).unwrap(), index);
}
