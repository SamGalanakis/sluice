//! Owner notification dispatch: config.json `notify`, one run per open owner question.
use serde_json::{Value, json};
use sluice_model::{
    commands::{Message, MessagePost},
    error::PublicError,
    events::NotificationOutcome,
    ids::{ProjectId, RunId},
};
use sluice_runtime::notify::Notifier;
use sluice_store::{ReadPool, RetrySafety, Writer, messages};
use std::path::PathBuf;

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("sluice-test-notify-{}", RunId::new()));
        let p = sluice_process::host::guard_scratch_home(&p).unwrap();
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn out(&self) -> PathBuf {
        self.0.join("notified.jsonl")
    }
    /// A shell script run as argv (no interpolation of the message): $1 is the output file.
    fn configure(&self, script: &str, timeout_s: f64) {
        std::fs::write(
            self.0.join("config.json"),
            json!({"fn_dirs":[],"notify":{"command":["/bin/sh","-c",script,"notify",self.out()],"timeout_s":timeout_s}}).to_string(),
        )
        .unwrap();
    }
    fn lines(&self) -> Vec<Value> {
        std::fs::read_to_string(self.out())
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const APPEND: &str = "cat >> \"$1\"; echo >> \"$1\"";

async fn setup() -> (Home, Writer, ReadPool, ProjectId, Notifier) {
    let h = Home::new();
    let w = Writer::open(&h.0).unwrap();
    let r = ReadPool::open(&h.0, 2).unwrap();
    let p = ProjectId::new();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'alerts','now')",
            [p.to_string()],
        )?;
        tx.changed(Some(p), "projects");
        Ok(())
    })
    .await
    .unwrap();
    let n = Notifier::new(h.0.clone(), w.clone(), r.clone());
    (h, w, r, p, n)
}

async fn post(w: &Writer, p: ProjectId, value: Value) -> Message {
    let mut value = value;
    value["project"] = json!({"kind":"id","value":p});
    let post: MessagePost = serde_json::from_value(value).unwrap();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        messages::message_post(tx, post, &messages::NoPlanInputs)
    })
    .await
    .unwrap()
}

async fn question(w: &Writer, p: ProjectId) -> Message {
    post(
        w,
        p,
        json!({"body":"Ship it?","title":"Release","to":"owner","needs_reply":true,"from":"orchestrator"}),
    )
    .await
}

async fn attempt(r: &ReadPool, p: ProjectId, m: &Message) -> messages::NotifyAttempt {
    let id = m.id;
    r.snapshot(move |c| messages::notify_attempt(c, p, id))
        .await
        .unwrap()
}

async fn notify_records(r: &ReadPool) -> Vec<Value> {
    r.snapshot(|c| {
        let mut stmt =
            c.prepare("SELECT payload FROM records WHERE kind='project.notify' ORDER BY seq")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .map(|p| serde_json::from_str(&p).unwrap())
            .collect())
    })
    .await
    .unwrap()
}

async fn round(n: &mut Notifier) {
    n.tick().await.unwrap();
    n.settle().await;
}

#[tokio::test]
async fn open_owner_question_runs_the_command_once_with_the_message_on_stdin() {
    let (h, w, r, p, mut n) = setup().await;
    h.configure(APPEND, 10.0);
    let q = question(&w, p).await;
    // Notes and questions to anyone else never notify.
    post(
        &w,
        p,
        json!({"body":"fyi","to":"owner","needs_reply":false,"from":"orchestrator"}),
    )
    .await;
    post(
        &w,
        p,
        json!({"body":"which?","to":"orchestrator","needs_reply":true,"from":"worker"}),
    )
    .await;
    round(&mut n).await;
    let lines = h.lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    let sent = &lines[0];
    assert_eq!(sent["id"], json!(q.id));
    assert_eq!(sent["title"], "Release");
    assert_eq!(sent["body"], "Ship it?");
    assert_eq!(sent["to"], "owner");
    assert_eq!(sent["needs_reply"], true);
    assert_eq!(sent["project"], "alerts");
    assert_eq!(sent["project_id"], json!(p));
    let a = attempt(&r, p, &q).await;
    assert_eq!(a.outcome, NotificationOutcome::Dispatched);
    assert_eq!(a.error, None);
    let outcomes: Vec<_> = notify_records(&r)
        .await
        .iter()
        .map(|r| r["outcome"].clone())
        .collect();
    assert_eq!(outcomes, vec![json!("reserved"), json!("dispatched")]);
    // Never again, however often it ticks, and a later question goes out on its own.
    round(&mut n).await;
    round(&mut Notifier::new(h.0.clone(), w.clone(), r.clone())).await;
    assert_eq!(h.lines().len(), 1);
    let second = question(&w, p).await;
    round(&mut n).await;
    let lines = h.lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["id"], json!(second.id));
    w.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_command_is_retried_and_a_lasting_failure_is_recorded_with_its_stderr() {
    let (h, w, r, p, mut n) = setup().await;
    // Fails once, then delivers.
    h.configure(
        "if [ -e \"$1.first\" ]; then cat >> \"$1\"; echo >> \"$1\"; else touch \"$1.first\"; echo boom >&2; exit 3; fi",
        10.0,
    );
    let q = question(&w, p).await;
    round(&mut n).await;
    assert_eq!(h.lines().len(), 1);
    assert_eq!(
        attempt(&r, p, &q).await.outcome,
        NotificationOutcome::Dispatched
    );
    // Always fails: three tries, then failed with the stderr tail.
    h.configure("echo \"try\" >> \"$1.tries\"; echo boom >&2; exit 3", 10.0);
    let q = question(&w, p).await;
    round(&mut n).await;
    let a = attempt(&r, p, &q).await;
    assert_eq!(a.outcome, NotificationOutcome::Failed);
    assert_eq!(a.stderr.as_deref(), Some("boom\n"));
    let Some(PublicError::FnFailure { message }) = a.error else {
        panic!("{:?}", a.error)
    };
    assert!(message.contains("3 tries"), "{message}");
    let tries = std::fs::read_to_string(h.0.join("notified.jsonl.tries")).unwrap();
    assert_eq!(tries.lines().count(), 3);
    // A command that cannot start fails the same way.
    std::fs::write(
        h.0.join("config.json"),
        json!({"notify":{"command":[h.0.join("missing")],"timeout_s":1}}).to_string(),
    )
    .unwrap();
    let q = question(&w, p).await;
    round(&mut n).await;
    let a = attempt(&r, p, &q).await;
    assert_eq!(a.outcome, NotificationOutcome::Failed);
    assert!(format!("{:?}", a.error).contains("cannot start"));
    w.shutdown().await.unwrap();
}

#[tokio::test]
async fn timeout_is_uncertain_and_never_retried() {
    let (h, w, r, p, mut n) = setup().await;
    h.configure("echo try >> \"$1.tries\"; exec sleep 30", 0.3);
    let q = question(&w, p).await;
    round(&mut n).await;
    let a = attempt(&r, p, &q).await;
    assert_eq!(a.outcome, NotificationOutcome::Uncertain);
    assert!(format!("{:?}", a.error).contains("timed out"));
    let tries = std::fs::read_to_string(h.0.join("notified.jsonl.tries")).unwrap();
    assert_eq!(tries.lines().count(), 1);
    w.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_claim_left_by_a_stopped_coordinator_is_uncertain_and_not_replayed() {
    let (h, w, r, p, _) = setup().await;
    h.configure(APPEND, 10.0);
    let q = question(&w, p).await;
    let a = attempt(&r, p, &q).await;
    let (id, token) = (q.id, a.attempt);
    // The claim commits, then the coordinator dies before recording a result.
    assert!(
        w.write(
            RetrySafety::NonIdempotent,
            move |tx| messages::notify_claim(tx, p, id, token)
        )
        .await
        .unwrap()
    );
    let mut restarted = Notifier::new(h.0.clone(), w.clone(), r.clone());
    round(&mut restarted).await;
    let a = attempt(&r, p, &q).await;
    assert_eq!(a.outcome, NotificationOutcome::Uncertain);
    assert!(matches!(a.error, Some(PublicError::ProcessLost { .. })));
    assert!(h.lines().is_empty());
    w.shutdown().await.unwrap();
}

#[tokio::test]
async fn unconfigured_home_keeps_the_reservation_and_a_closed_question_is_not_sent() {
    let (h, w, r, p, mut n) = setup().await;
    let q = question(&w, p).await;
    round(&mut n).await;
    assert_eq!(
        attempt(&r, p, &q).await.outcome,
        NotificationOutcome::Reserved
    );
    // Answered before notify was configured: settled without running the command.
    post(
        &w,
        p,
        json!({"body":"yes","reply_to":q.id,"from":"owner","needs_reply":false}),
    )
    .await;
    h.configure(APPEND, 10.0);
    round(&mut n).await;
    let a = attempt(&r, p, &q).await;
    assert_eq!(a.outcome, NotificationOutcome::Failed);
    assert!(matches!(a.error, Some(PublicError::Cancelled { .. })));
    assert!(h.lines().is_empty());
    w.shutdown().await.unwrap();
}
