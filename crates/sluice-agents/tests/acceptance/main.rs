mod support;
macro_rules! matrix {
    ($($scenario:ident),* $(,)?) => {$ (
        #[tokio::test]
        async fn $scenario() {
            for engine in ["codex", "claude", "devin"] { support::scenario(stringify!($scenario), engine).await; }
        }
    )*};
}
matrix! { fresh_required_submit, busy_submitted, background, quiet, compaction, addressed_live_message, feedback_resume, missing_outputs, nudge, unknown_acceptance, cancel_backoff, retry_exhaustion, session_cwd_mismatch, engine_mismatch }

#[tokio::test]
#[ignore = "Read-only version probes on this host, using a private scratch HOME"]
async fn doctor_on_host() {
    let home = support::Scratch::new();
    let probes = support::Scratch::new();
    let report = sluice_agents::engine_diagnostics(&home.0, &probes.0)
        .await
        .unwrap();
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert_eq!(report.len(), 3);
}

#[tokio::test]
async fn same_run_transient_after_commit_without_messages() {
    for engine in ["codex", "claude", "devin"] {
        support::transient_commits(engine).await;
    }
}
#[tokio::test]
async fn missing_session_fallback_lock_conflict_and_missing_cwd() {
    for engine in ["codex", "claude", "devin"] {
        support::session_policy(engine).await;
    }
}
