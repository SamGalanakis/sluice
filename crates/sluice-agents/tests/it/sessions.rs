use sluice_agents::{delivery::*, engines::*, git};
use sluice_model::ids::*;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = crate::scratch::root().join(format!("sluice-test-session-{}", RunId::new()));
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

#[tokio::test]
async fn recovery_preserves_accepted_delivery_and_the_git_index() {
    {
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
    {
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
}
