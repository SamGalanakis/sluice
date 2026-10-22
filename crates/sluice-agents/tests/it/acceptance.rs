use super::support;
#[tokio::test]
async fn same_run_transient_after_commit_without_messages() {
    for engine in ["codex"] {
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
    for engine in ["codex"] {
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
    for engine in ["codex"] {
        support::session_policy(engine).await;
    }

    let root = support::Scratch::new();
    let run = sluice_model::ids::RunId::new();
    let mut holder = sluice_agents::supervisor::SessionGuard::default();
    holder.acquire(&root.0, "codex", "a/b", "p", run).unwrap();
    let mut other = sluice_agents::supervisor::SessionGuard::default();
    other
        .acquire(
            &root.0,
            "codex",
            "a_b",
            "p",
            sluice_model::ids::RunId::new(),
        )
        .unwrap();
    assert_eq!(
        other
            .acquire(
                &root.0,
                "codex",
                "a/b",
                "p",
                sluice_model::ids::RunId::new()
            )
            .unwrap_err()
            .kind,
        sluice_agents::supervisor::FailureKind::LockConflict
    );
}

#[tokio::test]
async fn supervision_submission_compaction_and_refusal_policy() {
    for name in [
        "busy_submitted",
        "quiet",
        "compaction",
        "missing_outputs",
        "unknown_acceptance",
        "cancel_backoff",
        "retry_exhaustion",
        "session_cwd_mismatch",
        "engine_mismatch",
        "predecessor_cwd_mismatch",
    ] {
        support::scenario(name, "codex").await;
    }
}

// Regression: Pending Steer followed by a completed idle turn used to wait in
// Delivering forever, even as later addressed messages arrived.
#[tokio::test]
async fn unacknowledged_steer_cannot_idle_forever_after_its_turn() {
    use sluice_agents::{engines::*, supervisor::*};
    use sluice_model::ids::MessageId;
    use std::time::Duration;
    use support::*;
    for rejected in [false, true] {
        let root = Scratch::new();
        let mut cfg = config(&root.0, "claude");
        cfg.limits.delivery_ack = Duration::ZERO;
        let id = InputId::Message { id: MessageId(7) };
        let mut frames = happy()[..2].to_vec();
        let mut steer = frame(
            Some(EngineCommand::Steer {
                id: id.clone(),
                text: "*".into(),
            }),
            observation(EngineStatus::Busy, 1, 0),
        );
        steer.outcome = DeliveryOutcome::Pending;
        frames.push(steer);
        let mut ended = observation(EngineStatus::Idle, 1, 1);
        if rejected {
            ended.not_accepted.push(id.clone());
        }
        frames.push(frame(None, ended));
        if rejected {
            let mut retry = frame(
                Some(input(id.clone())),
                observation(EngineStatus::Idle, 1, 1),
            );
            retry.outcome = DeliveryOutcome::NotAccepted;
            frames.push(retry);
        }
        let mut engine = scripted("claude", frames);
        let mut host = Host {
            messages: if rejected {
                vec![message(7)]
            } else {
                vec![message(7), message(8)]
            },
            ..Default::default()
        };
        let result = tokio::time::timeout(Duration::from_secs(2), run(cfg, &mut engine, &mut host))
            .await
            .expect("unacknowledged steer hung in idle Delivering");
        let error = result.unwrap_err();
        assert_eq!(error.kind, FailureKind::TurnStartTimeout);
        if rejected {
            assert!(
                error.message.contains("one verified redelivery"),
                "{error:?}"
            );
        } else {
            assert!(
                error.message.contains("unacknowledged") && error.message.contains("MessageId(7)"),
                "{error:?}"
            );
        }
        let offers = engine.commands.iter().filter(|c| matches!(c,
            EngineCommand::DeliverText { id: sent, .. } | EngineCommand::Steer { id: sent, .. } if *sent == id)).count();
        assert_eq!(offers, if rejected { 2 } else { 1 });
        assert!(!host.acks.contains(&MessageId(7)));
    }
}
