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
