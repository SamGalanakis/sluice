mod support;
macro_rules! matrix {
    ($($scenario:ident),* $(,)?) => {$ (
        #[tokio::test]
        async fn $scenario() {
            for engine in ["codex", "claude", "devin"] { support::scenario(stringify!($scenario), engine).await; }
        }
    )*};
}
matrix! { fresh_required_submit, busy_submitted, background, quiet, compaction, addressed_live_message, feedback_resume, missing_outputs, nudge, unknown_acceptance, cancel_backoff, retry_exhaustion, session_cwd_mismatch, engine_mismatch, predecessor_cwd_mismatch }

#[tokio::test]
async fn same_run_transient_after_commit_without_messages() {
    for engine in ["codex", "claude", "devin"] {
        for (message, cause) in [
            (
                "capacity after commit",
                "the engine was temporarily at capacity",
            ),
            (
                "codex: network error (responseStreamDisconnected); Codex gave up after its own retries. Codex said: Bearer fixture-secret /private/fixture/path",
                "Codex lost its network connection after its own retries",
            ),
            (
                "claude: the pasted input did not show in the composer after 3 pastes; Claude showed /private/fixture/path",
                "your message could not be delivered and is being sent again",
            ),
            ("rate limit after commit", "a rate limit"),
            ("engine exited unexpectedly", "the engine exited"),
            (
                "unrecognized transient /private/fixture/path",
                "a temporary failure",
            ),
        ] {
            assert!(cause.chars().count() <= 120);
            let expected = format!(
                "Your session was interrupted ({cause}). Continue your task where you left off."
            );
            support::transient_commits(engine, message, &expected, false).await;
        }
    }
}
#[tokio::test]
async fn same_run_transient_reentry_uses_a_generic_continuation() {
    for engine in ["codex", "claude", "devin"] {
        support::transient_commits(
            engine,
            "codex: network error (responseStreamDisconnected); Codex gave up after its own retries",
            "Your session was interrupted. Continue your task where you left off.",
            true,
        )
        .await;
    }
}
#[tokio::test]
async fn missing_session_fallback_lock_conflict_and_missing_cwd() {
    for engine in ["codex", "claude", "devin"] {
        support::session_policy(engine).await;
    }
}
