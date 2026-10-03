use sluice_model::{
    ids::*,
    rpc::{JsonMap, PROTOCOL_VERSION, RunCapability},
};
use sluice_process::{guardian::*, identity::ProcessIdentity, journal::*, socket::*};
use std::{io, path::Path};
struct Presence(GuardianPresence);
impl AdoptionHost for Presence {
    async fn reconcile(&self, _: &AdoptionAttempt) -> io::Result<GuardianPresence> {
        Ok(self.0.clone())
    }
}
fn setup(dir: &Path) -> (AdoptionAttempt, MemoryCoordinator) {
    let run = RunId::new();
    let identity = AttemptKey {
        home: HomeId::new(),
        project: Some(ProjectId::new()),
        step: Some("work".parse().unwrap()),
        generation: StepGeneration(1),
        work: WorkGeneration(1),
        run,
        attempt: AttemptId::new(),
    };
    let guardian = GuardianIdentity {
        identity: identity.clone(),
        process: ProcessIdentity::read(std::process::id()).unwrap(),
        unit: format!("sluice-test-{run}.service"),
        socket_challenge: "challenge".into(),
    };
    let attempt = AdoptionAttempt {
        identity: identity.clone(),
        guardian: Some(guardian),
        run_dir: dir.into(),
        unit: format!("sluice-test-{run}.service"),
        service_cgroup: Some(format!("/fixture/sluice-test-{run}.service")),
        capability: RunCapability::new("secret"),
    };
    let link = MemoryCoordinator::default();
    link.reserve(
        identity,
        AssignedRange {
            after: MessageId(0),
            through: MessageId(3),
        },
        vec![],
    );
    (attempt, link)
}
fn proof(empty: bool) -> CleanupEvidence {
    CleanupEvidence {
        cgroup: "fixture/service".into(),
        empty,
        escalated: true,
    }
}
fn journal(a: &AdoptionAttempt) -> CompletionJournal {
    CompletionJournal {
        protocol: PROTOCOL_VERSION,
        identity: a.identity.clone(),
        completion_id: "completion".into(),
        result: PayloadResult::Succeeded(JsonMap::default()),
        starts: vec![StartEvidence {
            invocation: InvocationId::new(),
            executor: a.guardian.as_ref().unwrap().process.clone(),
        }],
        exits: vec![],
        cleanup: vec![proof(true)],
        submissions: JsonMap::default(),
        submission_version: None,
        delivery_acks: vec![],
    }
}
#[tokio::test]
async fn reconnects_only_the_exact_live_guardian() {
    let dir = tempfile::tempdir().unwrap();
    let (a, link) = setup(dir.path());
    assert_eq!(
        adopt_attempt(
            &a,
            &link,
            &Presence(GuardianPresence::Live(a.guardian.clone().unwrap()))
        )
        .await
        .unwrap(),
        AdoptionOutcome::Reconnected
    );
    let mut wrong = a.guardian.clone().unwrap();
    wrong.process.start_time += 1;
    assert!(matches!(
        adopt_attempt(&a, &link, &Presence(GuardianPresence::Live(wrong)))
            .await
            .unwrap(),
        AdoptionOutcome::Pending(_)
    ));
    link.with_state(|s| assert_eq!(s.attempts[&a.identity.run].releases, 0));
}
#[tokio::test]
async fn journal_import_replays_start_before_completion_and_busy_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let (a, link) = setup(dir.path());
    let j = journal(&a);
    j.write(dir.path()).unwrap();
    link.with_state(|s| s.busy_completions = 1);
    let host = Presence(GuardianPresence::Ambiguous("should not be needed".into()));
    assert!(matches!(
        adopt_attempt(&a, &link, &host).await.unwrap(),
        AdoptionOutcome::Pending(_)
    ));
    assert!(dir.path().join("completion.json").exists());
    link.with_state(|s| {
        let r = &s.attempts[&a.identity.run];
        assert_eq!(r.cursor, MessageId(3));
        assert_eq!(r.releases, 0);
    });
    assert!(matches!(
        adopt_attempt(&a, &link, &host).await.unwrap(),
        AdoptionOutcome::Imported(_)
    ));
    assert!(!dir.path().join("completion.json").exists());
    link.with_state(|s| assert_eq!(s.attempts[&a.identity.run].releases, 1));
}
#[tokio::test]
async fn loss_releases_holds_only_after_proven_recursive_emptiness() {
    let dir = tempfile::tempdir().unwrap();
    let (a, link) = setup(dir.path());
    assert!(matches!(
        adopt_attempt(&a, &link, &Presence(GuardianPresence::Gone(proof(false))))
            .await
            .unwrap(),
        AdoptionOutcome::Pending(_)
    ));
    assert!(matches!(
        adopt_attempt(
            &a,
            &link,
            &Presence(GuardianPresence::Ambiguous("service stopping".into()))
        )
        .await
        .unwrap(),
        AdoptionOutcome::Pending(_)
    ));
    link.with_state(|s| assert_eq!(s.attempts[&a.identity.run].releases, 0));
    assert!(matches!(
        adopt_attempt(&a, &link, &Presence(GuardianPresence::Gone(proof(true))))
            .await
            .unwrap(),
        AdoptionOutcome::Lost(_)
    ));
    link.with_state(|s| {
        let r = &s.attempts[&a.identity.run];
        assert_eq!(r.releases, 1);
        assert!(matches!(
            r.completion.as_ref().unwrap().result,
            PayloadResult::Lost(_)
        ));
        assert!(r.started.is_empty());
    });
}
#[tokio::test]
async fn changed_boot_and_unclaimed_reservations_are_lost_only_after_absence() {
    for unclaimed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (mut a, link) = setup(dir.path());
        if unclaimed {
            a.guardian = None;
        } else {
            a.guardian.as_mut().unwrap().process.boot_id = "old-boot".into();
        }
        assert!(matches!(
            adopt_attempt(
                &a,
                &link,
                &Presence(GuardianPresence::Ambiguous(
                    "absence not established".into()
                ))
            )
            .await
            .unwrap(),
            AdoptionOutcome::Pending(_)
        ));
        assert!(matches!(
            adopt_attempt(&a, &link, &Presence(GuardianPresence::Gone(proof(true))))
                .await
                .unwrap(),
            AdoptionOutcome::Lost(_)
        ));
        link.with_state(|s| assert!(s.attempts[&a.identity.run].started.is_empty()));
    }
}
#[test]
fn journal_publication_refuses_corruption_identity_changes_and_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let (a, _) = setup(dir.path());
    let mut j = journal(&a);
    j.write(dir.path()).unwrap();
    j.write(dir.path()).unwrap();
    assert_eq!(
        CompletionJournal::read(dir.path(), &a.identity).unwrap(),
        Some(j.clone())
    );
    j.completion_id = "different".into();
    assert!(j.write(dir.path()).is_err());
    let mut other = a.identity.clone();
    other.work = WorkGeneration(2);
    assert!(CompletionJournal::read(dir.path(), &other).is_err());
    std::fs::write(
        dir.path().join("completion.json"),
        br#"{"protocol":1,"protocol":1}"#,
    )
    .unwrap();
    assert!(CompletionJournal::read(dir.path(), &a.identity).is_err());
    std::fs::write(dir.path().join("completion.json"), b"{").unwrap();
    assert!(CompletionJournal::read(dir.path(), &a.identity).is_err());
}
#[tokio::test]
async fn wrong_completion_ack_never_deletes_journal() {
    struct WrongAck;
    impl CoordinatorLink for WrongAck {
        async fn request(
            &self,
            command: CoordinatorCommand,
        ) -> Result<CoordinatorReply, sluice_model::error::PublicError> {
            Ok(match command {
                CoordinatorCommand::Started { .. } => CoordinatorReply::Started,
                _ => CoordinatorReply::Completed(DurableAck {
                    run: RunId::new(),
                    completion_id: "wrong".into(),
                }),
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (a, _) = setup(dir.path());
    let j = journal(&a);
    j.write(dir.path()).unwrap();
    assert!(
        adopt_attempt(
            &a,
            &WrongAck,
            &Presence(GuardianPresence::Gone(proof(true)))
        )
        .await
        .is_err()
    );
    assert!(dir.path().join("completion.json").exists());
}

#[tokio::test]
async fn collected_result_landing_during_reconciliation_wins_over_loss() {
    struct DiesAfterCollection(CompletionJournal);
    impl AdoptionHost for DiesAfterCollection {
        async fn reconcile(&self, a: &AdoptionAttempt) -> io::Result<GuardianPresence> {
            std::fs::write(
                a.run_dir.join("collected.json"),
                serde_json::to_vec(&self.0).unwrap(),
            )?;
            Ok(GuardianPresence::Gone(proof(true)))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (a, link) = setup(dir.path());
    let j = journal(&a);
    assert!(matches!(
        adopt_attempt(&a, &link, &DiesAfterCollection(j))
            .await
            .unwrap(),
        AdoptionOutcome::Imported(_)
    ));
    link.with_state(|s| {
        assert!(matches!(
            s.attempts[&a.identity.run]
                .completion
                .as_ref()
                .unwrap()
                .result,
            PayloadResult::Succeeded(_)
        ))
    });
}
