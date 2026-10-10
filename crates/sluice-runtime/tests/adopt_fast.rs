//! The coordinator answers while it adopts runs: reads and install control are
//! served during the startup pass, other commands and all admission wait for it,
//! runs adopt concurrently under a bound, and reads do not refresh a current
//! registry. Every wait here gates on an event; the timeouts only turn a hang
//! into a failure.
#[allow(dead_code)]
#[path = "../../../tests/support/home.rs"]
mod home;
use serde_json::{Value, json};
use sluice_model::{
    RuntimeApi,
    commands::{CommandReply, CommandRequest},
    error::PublicError,
    ids::*,
    rpc::{FnInvocation, JsonMap, PROTOCOL_VERSION, RequestId, RpcRequest, decode_json},
};
use sluice_process::{
    guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence},
    identity::ProcessIdentity,
    journal::{CleanupEvidence, CompletionJournal, PayloadResult, StartEvidence},
    socket::{
        self, CoordinatorCommand as C, CoordinatorLink, CoordinatorReply as R, GuardianIdentity,
        UnixCoordinatorLink,
    },
};
use sluice_runtime::{
    client::CoordinatorClient,
    coordinator::Coordinator,
    dispatch::Catalog,
    execution::{ExecutionHost, Launch, LaunchOutcome},
    install::Installation,
    scheduler::reconcile_project,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// What the fake host reports for a run.
#[derive(Clone, Copy)]
enum Presence {
    /// Still owned by something the fake cannot verify: adoption leaves it.
    Ambiguous,
    /// Gone with an empty cgroup: adoption imports it as lost.
    Gone,
    /// The host fails: adoption is deferred.
    Fails,
}

/// A host whose `reconcile` counts entries and blocks on `gate` until the test
/// adds permits, so a test can hold any number of adoptions in flight.
#[derive(Default)]
struct State {
    launches: Mutex<Vec<Launch>>,
    presence: Mutex<HashMap<RunId, Presence>>,
    composition: bool,
    gate: Option<tokio::sync::Semaphore>,
    entered: tokio::sync::watch::Sender<usize>,
    finished: tokio::sync::watch::Sender<usize>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}
#[derive(Clone)]
struct Fake(Arc<State>);
impl Fake {
    fn gated() -> Self {
        Self(Arc::new(State {
            gate: Some(tokio::sync::Semaphore::new(0)),
            ..Default::default()
        }))
    }
    fn open() -> Self {
        Self(Arc::new(State::default()))
    }
    fn composed() -> Self {
        Self(Arc::new(State {
            composition: true,
            ..Default::default()
        }))
    }
    fn release(&self, n: usize) {
        self.0.gate.as_ref().expect("gated host").add_permits(n);
    }
    fn launches(&self) -> usize {
        self.0.launches.lock().unwrap().len()
    }
    fn entered(&self) -> usize {
        *self.0.entered.borrow()
    }
    async fn until_entered(&self, n: usize) {
        let mut entered = self.0.entered.subscribe();
        within(entered.wait_for(|e| *e >= n))
            .await
            .unwrap()
            .unwrap();
    }
    async fn until_finished(&self, n: usize) {
        let mut finished = self.0.finished.subscribe();
        within(finished.wait_for(|f| *f >= n))
            .await
            .unwrap()
            .unwrap();
    }
    fn reset_counts(&self) {
        self.0.entered.send_replace(0);
        self.0.finished.send_replace(0);
        self.0.max_in_flight.store(0, Ordering::SeqCst);
    }
}
impl FnHost for Fake {
    async fn invoke(&self, _: FnInvocation) -> Result<JsonMap, PublicError> {
        Ok(JsonMap::default())
    }
}
impl AdoptionHost for Fake {
    async fn reconcile(&self, attempt: &AdoptionAttempt) -> std::io::Result<GuardianPresence> {
        let now = self.0.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.0.max_in_flight.fetch_max(now, Ordering::SeqCst);
        self.0.entered.send_modify(|n| *n += 1);
        if let Some(gate) = &self.0.gate {
            gate.acquire().await.unwrap().forget();
        }
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.0.finished.send_modify(|n| *n += 1);
        let presence = self
            .0
            .presence
            .lock()
            .unwrap()
            .get(&attempt.identity.run)
            .copied()
            .unwrap_or(Presence::Ambiguous);
        match presence {
            Presence::Ambiguous => Ok(GuardianPresence::Ambiguous("fixture".into())),
            Presence::Gone => Ok(GuardianPresence::Gone(CleanupEvidence {
                cgroup: format!("/fake/{}", attempt.unit),
                empty: true,
                escalated: false,
            })),
            Presence::Fails => Err(std::io::Error::other("fixture host failure")),
        }
    }
}
impl ExecutionHost for Fake {
    fn composition_enabled(&self) -> bool {
        self.0.composition
    }
    async fn launch(&self, launch: Launch) -> Result<LaunchOutcome, PublicError> {
        self.0.launches.lock().unwrap().push(launch);
        Ok(LaunchOutcome::Accepted)
    }
    async fn cleanup_valid(&self, journal: &CompletionJournal) -> Result<bool, PublicError> {
        Ok(journal.cleanup.iter().all(|proof| proof.empty))
    }
}

/// Fails a test that would otherwise hang; no assertion depends on it.
async fn within<T>(future: impl std::future::Future<Output = T>) -> Result<T, &'static str> {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .map_err(|_| "hung")
}
/// Polls a condition that an event makes true; the bound only guards a hang.
async fn until(mut condition: impl FnMut() -> bool) {
    within(async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
fn request(value: Value) -> CommandRequest {
    decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn selector(project: ProjectId) -> Value {
    json!({"kind":"id","value":project})
}
async fn project(broker: &Coordinator<Fake>, name: &str) -> ProjectId {
    let CommandReply::Project(created) = broker.command(request(json!({"command":"project_create","args":{"name":name,"description":"","icon":null,"resources":{},"author":"test"}}))).await.unwrap() else {
        panic!("project")
    };
    let id = created.project_id;
    broker.command(request(json!({"command":"plan_edit","args":{"project":selector(id),"rev":1,"ops":[{"op":"step.add","step":"work","spec":{"run":"fixture.submit","in":{"value":{"default":1}},"outputs":{"submitted":"boolean"}}}],"start":true,"dry_run":false,"reason":"test","author":"test"}}))).await.unwrap();
    id
}
/// A home with one nonterminal run per name, launched under scheduler lease "s"
/// (released afterwards unless `keep_lease`).
async fn home_with_runs(
    host: Fake,
    names: &[&str],
    keep_lease: bool,
) -> (
    home::ScratchHome,
    Coordinator<Fake>,
    Vec<(ProjectId, Launch)>,
) {
    let home = home::ScratchHome::new().unwrap();
    let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), host.clone())
        .await
        .unwrap();
    broker.acquire_scheduler("s".into()).await.unwrap();
    let mut runs = Vec::new();
    for name in names {
        let id = project(&broker, name).await;
        reconcile_project(&broker, id, "s").await.unwrap();
        let launch = host.0.launches.lock().unwrap().last().unwrap().clone();
        assert_eq!(launch.identity.project, Some(id));
        runs.push((id, launch));
    }
    if !keep_lease {
        broker.release_scheduler("s".into()).await.unwrap();
    }
    (home, broker, runs)
}
async fn serve(
    broker: &Coordinator<Fake>,
    home: &std::path::Path,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<Result<(), PublicError>>,
) {
    let stop = CancellationToken::new();
    let task = tokio::spawn({
        let (broker, stop) = (broker.clone(), stop.clone());
        async move { broker.serve(stop).await }
    });
    let socket = home.join("coordinator.sock");
    until(|| socket.exists()).await;
    (stop, task)
}
async fn attempt(broker: &Coordinator<Fake>, run: RunId) -> (String, bool) {
    broker
        .reads()
        .snapshot(move |sql| {
            Ok(sql.query_row(
                "SELECT a.phase,a.cancel_requested FROM runs r JOIN attempts a USING(attempt_id) WHERE r.run_id=?1",
                [run.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap()
}
fn status(project: ProjectId) -> CommandRequest {
    request(
        json!({"command":"status","args":{"project":selector(project),"selection":{"steps":null,"tags":null},"all":true}}),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reads_and_install_control_answer_while_adoption_is_blocked_and_a_write_waits() {
    let host = Fake::gated();
    let (home, broker, runs) = home_with_runs(host.clone(), &["p"], false).await;
    let (project, launch) = runs[0].clone();
    // A fenced installation that selects this home, as during a deploy.
    let installation = Installation::for_home(home.path()).unwrap();
    installation.fence("deploy".into()).unwrap();
    let release = home.root().join("release");
    std::fs::create_dir_all(release.join("bin")).unwrap();
    std::fs::write(release.join("bin/sluice"), b"").unwrap();
    installation.select(&release, home.path()).unwrap();

    let (stop, server) = serve(&broker, home.path()).await;
    host.until_entered(1).await;
    assert!(!broker.adopted());
    let client = CoordinatorClient::new(home.path());

    // A write that touches the run being adopted waits for the pass.
    let cancel = tokio::spawn({
        let client = client.clone();
        async move {
            client.command(request(json!({"command":"step_cancel","args":{"project":selector(project),"selection":{"steps":["work"],"tags":null},"reason":"during adoption","author":"test"}}))).await
        }
    });
    until(|| broker.waiting_for_adoption() == 1).await;

    assert!(matches!(
        client.command(CommandRequest::ProjectsList).await.unwrap(),
        CommandReply::Projects(p) if p.len() == 1
    ));
    let CommandReply::Data(view) = client.command(status(project)).await.unwrap() else {
        panic!("status")
    };
    assert_eq!(view.as_value()["steps"]["work"]["status"], "running");
    client
        .command(request(
            json!({"command":"plan_get","args":{"project":selector(project)}}),
        ))
        .await
        .unwrap();
    client
        .command(request(
            json!({"command":"log_read","args":{"project":selector(project),"since_seq":null,"kinds":null,"threads":null,"limit":10}}),
        ))
        .await
        .unwrap();
    // Install control takes the installation lock, never the coordinator's
    // adoption: status and unfence complete while the pass is held.
    let unfence = tokio::task::spawn_blocking({
        let installation = installation.clone();
        move || {
            assert!(installation.status()?.fence.is_some());
            installation.unfence()
        }
    });
    assert!(
        within(unfence)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .fence
            .is_none()
    );

    // Everything above was answered; the write was not.
    assert!(!broker.adopted());
    assert!(!cancel.is_finished());
    assert!(!attempt(&broker, launch.identity.run).await.1);
    assert_eq!(host.0.finished.borrow().to_owned(), 0);

    host.release(1);
    within(cancel).await.unwrap().unwrap().unwrap();
    assert!(broker.adopted());
    assert_eq!(broker.waiting_for_adoption(), 0);
    assert!(attempt(&broker, launch.identity.run).await.1);
    stop.cancel();
    within(server).await.unwrap().unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runs_adopt_concurrently_within_the_bound_and_one_adopter_per_run() {
    let host = Fake::gated();
    let (_home, broker, runs) = home_with_runs(host.clone(), &["a", "b", "c", "d"], false).await;

    // All four are in the host at once, and none has finished: not serial.
    let pass = tokio::spawn({
        let broker = broker.clone();
        async move { broker.adopt().await }
    });
    host.until_entered(4).await;
    assert_eq!(*host.0.finished.borrow(), 0);
    assert_eq!(host.0.in_flight.load(Ordering::SeqCst), 4);
    // A second pass meanwhile skips every run already being adopted, so it
    // returns while the gate is still shut.
    within(broker.adopt()).await.unwrap().unwrap();
    assert_eq!(host.entered(), 4);
    host.release(4);
    within(pass).await.unwrap().unwrap().unwrap();
    for (_, launch) in &runs {
        assert_eq!(attempt(&broker, launch.identity.run).await.0, "reserved");
    }

    // With a bound of two, the third run starts only once one has finished.
    host.reset_counts();
    let pass = tokio::spawn({
        let broker = broker.clone();
        async move { broker.adopt_bounded(2).await }
    });
    host.until_entered(2).await;
    assert_eq!(host.0.in_flight.load(Ordering::SeqCst), 2);
    for finished in 1..=4 {
        host.release(1);
        host.until_finished(finished).await;
        host.until_entered((finished + 2).min(4)).await;
        assert!(host.0.in_flight.load(Ordering::SeqCst) <= 2);
    }
    within(pass).await.unwrap().unwrap().unwrap();
    assert_eq!(host.entered(), 4);
    assert_eq!(host.0.max_in_flight.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_adoption_is_deferred_while_the_others_adopt_and_the_coordinator_becomes_ready() {
    let host = Fake::open();
    let (home, broker, runs) =
        home_with_runs(host.clone(), &["fails", "gone", "live"], false).await;
    let [(_, fails), (gone_project, gone), (_, live)] =
        [runs[0].clone(), runs[1].clone(), runs[2].clone()];
    {
        let mut presence = host.0.presence.lock().unwrap();
        presence.insert(fails.identity.run, Presence::Fails);
        presence.insert(gone.identity.run, Presence::Gone);
        presence.insert(live.identity.run, Presence::Ambiguous);
    }
    let (stop, server) = serve(&broker, home.path()).await;
    until(|| broker.adopted()).await;
    assert_eq!(host.entered(), 3);
    // The failure leaves its run as it was, for the next pass; the gone run is
    // imported as lost; the unverifiable one stays pending.
    assert_eq!(attempt(&broker, fails.identity.run).await.0, "reserved");
    assert_eq!(attempt(&broker, gone.identity.run).await.0, "terminal");
    assert_eq!(attempt(&broker, live.identity.run).await.0, "reserved");
    let client = CoordinatorClient::new(home.path());
    let CommandReply::Data(view) = client.command(status(gone_project)).await.unwrap() else {
        panic!("status")
    };
    assert_ne!(view.as_value()["steps"]["work"]["status"], "running");
    // Writes are served now.
    client.command(request(json!({"command":"step_cancel","args":{"project":selector(runs[2].0),"selection":{"steps":["work"],"tags":null},"reason":"after adoption","author":"test"}}))).await.unwrap();
    // The deferred run is tried again by the next pass; the settled one is not.
    host.reset_counts();
    broker.adopt().await.unwrap();
    assert_eq!(host.entered(), 2);
    assert_eq!(attempt(&broker, fails.identity.run).await.0, "reserved");
    stop.cancel();
    within(server).await.unwrap().unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_is_admitted_or_launched_before_the_startup_pass_completes() {
    let host = Fake::gated();
    let (home, broker, runs) = home_with_runs(host.clone(), &["adopted"], false).await;
    // A second project whose step is ready but was never admitted.
    let waiting = project(&broker, "ready").await;
    assert_eq!(host.launches(), 1);

    let (stop, server) = serve(&broker, home.path()).await;
    host.until_entered(1).await;
    let client = CoordinatorClient::new(home.path());
    // `loop` takes the scheduler lease over the socket during the pass: it is
    // granted, and admits nothing by itself.
    let lease = within(client.acquire_scheduler()).await.unwrap().unwrap();
    assert!(broker.scheduler_owner().await.unwrap().is_some());
    // A direct call is admission: it waits for the pass.
    let call = tokio::spawn({
        let client = client.clone();
        async move {
            client.command(request(json!({"command":"fn_call","args":{"name":"fixture.echo","inputs":{"value":1},"project":selector(waiting),"wait_seconds":0,"direct":true,"author":"test"}}))).await
        }
    });
    until(|| broker.waiting_for_adoption() == 1).await;
    client.command(CommandRequest::ProjectsList).await.unwrap();
    let CommandReply::Data(view) = client.command(status(waiting)).await.unwrap() else {
        panic!("status")
    };
    assert_eq!(view.as_value()["steps"]["work"]["status"], "pending");
    assert!(!broker.adopted());
    assert_eq!(host.launches(), 1);
    assert!(!call.is_finished());

    host.release(1);
    within(call).await.unwrap().unwrap().unwrap();
    // After the pass the scheduler admits the ready step and the call launches.
    until(|| host.launches() == 3).await;
    let launched: Vec<_> = host.0.launches.lock().unwrap()[1..]
        .iter()
        .map(|l| (l.identity.project, l.identity.step.is_some()))
        .collect();
    assert!(launched.contains(&(Some(waiting), true)));
    assert!(launched.contains(&(Some(waiting), false)));
    assert_eq!(attempt(&broker, runs[0].1.identity.run).await.0, "reserved");
    drop(lease);
    stop.cancel();
    within(server).await.unwrap().unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_refreshes_a_stale_registry_once_and_never_a_current_one() {
    let host = Fake::composed();
    let home = home::ScratchHome::new().unwrap();
    let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), host)
        .await
        .unwrap();
    let publication = broker.catalog().1.clone().expect("composition publishes");
    let id = project(&broker, "p").await;
    let reads = [
        CommandRequest::ProjectsList,
        status(id),
        request(json!({"command":"plan_get","args":{"project":selector(id)}})),
        request(json!({"command":"fn_list","args":{"project":selector(id)}})),
        request(
            json!({"command":"log_read","args":{"project":selector(id),"since_seq":null,"kinds":null,"threads":null,"limit":10}}),
        ),
        request(
            json!({"command":"messages","args":{"project":selector(id),"view":"history","thread":null,"since":null}}),
        ),
    ];
    let read_all = || async {
        for read in reads.clone() {
            broker.command(read).await.unwrap();
        }
    };
    read_all().await;
    let (scans, published) = (publication.scans(), publication.publications());

    // A current registry: reads neither scan nor republish.
    for _ in 0..3 {
        read_all().await;
    }
    assert_eq!(
        (publication.scans(), publication.publications()),
        (scans, published)
    );

    // A new project makes it stale after the creating write's own refresh:
    // the next read republishes once, later reads do not.
    broker.command(request(json!({"command":"project_create","args":{"name":"q","description":"","icon":null,"resources":{},"author":"test"}}))).await.unwrap();
    assert_eq!(publication.publications(), published);
    broker.command(CommandRequest::ProjectsList).await.unwrap();
    assert_eq!(publication.publications(), published + 1);
    read_all().await;
    assert_eq!(publication.publications(), published + 1);

    // A change the watcher reports makes it stale too: a read republishes.
    // (The republishing scan itself sees the fingerprint move and bumps the
    // version, and the publication keeps the version read before its scans,
    // so one later read republishes once more; then it is current again.)
    std::fs::create_dir_all(home.path().join("fns")).unwrap();
    let mut watcher = publication.registry.watch().unwrap();
    std::fs::write(home.root().join("notes.txt"), b"not a fn").unwrap();
    std::fs::rename(
        home.root().join("notes.txt"),
        home.path().join("fns/notes.txt"),
    )
    .unwrap();
    within(watcher.changed()).await.unwrap().unwrap();
    let scans = publication.scans();
    broker.command(CommandRequest::ProjectsList).await.unwrap();
    assert_eq!(publication.scans(), scans + 1);
    assert_eq!(publication.publications(), published + 2);
    read_all().await;
    let (settled, published) = (publication.scans(), publication.publications());
    read_all().await;
    assert_eq!(
        (publication.scans(), publication.publications()),
        (settled, published)
    );

    // A write still scans before it runs.
    broker.command(request(json!({"command":"project_update","args":{"project":selector(id),"new_name":null,"description":"changed","icon":null,"resources":null,"paused":null,"archived":null,"expected_settings_rev":null,"reason":null,"author":"test"}}))).await.unwrap();
    assert_eq!(publication.scans(), settled + 1);
}

/// The run's own guardian, talking to the coordinator over its socket.
fn link(home: &std::path::Path, launch: &Launch) -> UnixCoordinatorLink {
    UnixCoordinatorLink {
        path: home.join("coordinator.sock"),
        capability: launch.capability.clone(),
    }
}
fn guardian(launch: &Launch) -> GuardianIdentity {
    GuardianIdentity {
        identity: launch.identity.clone(),
        process: ProcessIdentity {
            pid: 123,
            start_time: 42,
            boot_id: "fixture-boot".into(),
            cgroup: format!("/fake/sluice-test-{}.service/control", launch.identity.run),
        },
        unit: format!("sluice-test-{}.service", launch.identity.run),
        socket_challenge: "challenge".into(),
    }
}
/// A run's callback that edits its own project: it waits for the pass.
fn cancel_callback(launch: &Launch, project: ProjectId, id: &str) -> C {
    C::Callback {
        identity: launch.identity.clone(),
        request: Box::new(RpcRequest {
            protocol: PROTOCOL_VERSION,
            request_id: RequestId(id.into()),
            run_capability: Some(launch.capability.clone()),
            command: request(
                json!({"command":"step_cancel","args":{"project":selector(project),"selection":{"steps":["work"],"tags":null},"reason":"from the run","author":"test"}}),
            ),
        }),
    }
}
fn step_cancel(project: ProjectId) -> CommandRequest {
    request(
        json!({"command":"step_cancel","args":{"project":selector(project),"selection":{"steps":["work"],"tags":null},"reason":"user","author":"test"}}),
    )
}
fn refused_unrun(error: &PublicError) -> bool {
    matches!(error, PublicError::Busy { message, retryable: true } if message.contains("was not executed"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_runs_guardian_is_served_during_the_pass_and_wins_the_race_with_its_adopter() {
    let host = Fake::gated();
    let (home, broker, runs) = home_with_runs(host.clone(), &["g"], false).await;
    let (project, l) = runs[0].clone();
    // The adopter will find the unit gone and try to import the run as lost.
    host.0
        .presence
        .lock()
        .unwrap()
        .insert(l.identity.run, Presence::Gone);
    let (stop, server) = serve(&broker, home.path()).await;
    host.until_entered(1).await;
    let guardian_link = link(home.path(), &l);

    // Its plan edit waits for the pass while its other requests are served.
    let edit = tokio::spawn({
        let (link, command) = (link(home.path(), &l), cancel_callback(&l, project, "edit"));
        async move { link.request(command).await }
    });
    until(|| broker.waiting_for_adoption() == 1).await;
    let send = |command| async { within(guardian_link.request(command)).await.unwrap() };
    assert!(matches!(
        send(C::Claim(guardian(&l))).await,
        Ok(R::Claimed(true))
    ));
    let executor = ProcessIdentity {
        pid: 124,
        start_time: 43,
        boot_id: "fixture-boot".into(),
        cgroup: "/fake/payload".into(),
    };
    assert!(matches!(
        send(C::Started {
            identity: l.identity.clone(),
            invocation: l.invocation.invocation,
            executor: executor.clone(),
        })
        .await,
        Ok(R::Started)
    ));
    assert!(matches!(
        send(C::Watch {
            identity: l.identity.clone(),
            after: MessageId(0),
            wait_ms: 0,
        })
        .await,
        Ok(R::Watched {
            cancelled: false,
            ..
        })
    ));
    let journal = CompletionJournal {
        protocol: 1,
        identity: l.identity.clone(),
        completion_id: "guardian".into(),
        result: PayloadResult::Succeeded(decode_json(br#"{"value":1,"submitted":true}"#).unwrap()),
        starts: vec![StartEvidence {
            invocation: l.invocation.invocation,
            executor,
        }],
        exits: vec![],
        cleanup: vec![CleanupEvidence {
            cgroup: "fake".into(),
            empty: true,
            escalated: false,
        }],
        submissions: JsonMap::default(),
        submission_version: None,
        delivery_acks: vec![],
    };
    assert!(matches!(
        send(C::Complete(Box::new(journal))).await,
        Ok(R::Completed(_))
    ));
    assert!(!broker.adopted());
    assert!(!edit.is_finished());
    assert_eq!(attempt(&broker, l.identity.run).await.0, "terminal");

    // The adopter then imports its lost journal against the finished run: the
    // guardian's result stands and the pass still completes.
    host.release(1);
    until(|| broker.adopted()).await;
    let CommandReply::Data(view) = broker.command(status(project)).await.unwrap() else {
        panic!("status")
    };
    assert_eq!(view.as_value()["steps"]["work"]["status"], "succeeded");
    // The parked edit ran after the pass, against the finished run.
    let edited = within(edit).await.unwrap().unwrap();
    assert!(!matches!(&edited, Err(e) if refused_unrun(e)), "{edited:?}");
    stop.cancel();
    within(server).await.unwrap().unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_lost_runs_in_one_project_release_every_hold() {
    let host = Fake::gated();
    let home = home::ScratchHome::new().unwrap();
    let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), host.clone())
        .await
        .unwrap();
    let CommandReply::Project(created) = broker.command(request(json!({"command":"project_create","args":{"name":"lanes","description":"","icon":null,"resources":{"lane":4},"author":"test"}}))).await.unwrap() else {
        panic!("project")
    };
    let id = created.project_id;
    let ops: Vec<Value> = (0..4)
        .map(|n| json!({"op":"step.add","step":format!("w{n}"),"spec":{"run":"fixture.submit","in":{"value":{"default":n}},"outputs":{"submitted":"boolean"},"needs":{"lane":1}}}))
        .collect();
    broker.command(request(json!({"command":"plan_edit","args":{"project":selector(id),"rev":1,"ops":ops,"start":true,"dry_run":false,"reason":"test","author":"test"}}))).await.unwrap();
    broker.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&broker, id, "s").await.unwrap();
    broker.release_scheduler("s".into()).await.unwrap();
    let launches = host.0.launches.lock().unwrap().clone();
    assert_eq!(launches.len(), 4);
    for launch in &launches {
        host.0
            .presence
            .lock()
            .unwrap()
            .insert(launch.identity.run, Presence::Gone);
    }
    let held = || {
        let broker = broker.clone();
        async move {
            broker
                .reads()
                .snapshot(move |sql| {
                    Ok(sql.query_row(
                        "SELECT coalesce(sum(amount),0) FROM leases WHERE project_id=?1 AND state='held'",
                        [id.to_string()],
                        |r| r.get::<_, i64>(0),
                    )?)
                })
                .await
                .unwrap()
        }
    };
    assert_eq!(held().await, 4);
    let pass = tokio::spawn({
        let broker = broker.clone();
        async move { broker.adopt().await }
    });
    // All four are in flight before any imports, then import together.
    host.until_entered(4).await;
    host.release(4);
    within(pass).await.unwrap().unwrap().unwrap();
    for launch in &launches {
        assert_eq!(attempt(&broker, launch.identity.run).await.0, "terminal");
    }
    assert_eq!(held().await, 0);
    let CommandReply::Data(view) = broker.command(status(id)).await.unwrap() else {
        panic!("status")
    };
    for n in 0..4 {
        assert_eq!(
            view.as_value()["steps"][format!("w{n}")]["status"],
            "failed"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn past_32_waiting_requests_a_request_is_refused_retryably_and_unrun() {
    let host = Fake::gated();
    let (home, broker, runs) = home_with_runs(host.clone(), &["p"], false).await;
    let (project, l) = runs[0].clone();
    let (stop, server) = serve(&broker, home.path()).await;
    host.until_entered(1).await;
    let client = CoordinatorClient::new(home.path());
    let waiting: Vec<_> = (0..32)
        .map(|n| {
            let client = client.clone();
            tokio::spawn(async move {
                client.command(request(json!({"command":"project_update","args":{"project":selector(project),"new_name":null,"description":format!("w{n}"),"icon":null,"resources":null,"paused":null,"archived":null,"expected_settings_rev":null,"reason":null,"author":"test"}}))).await
            })
        })
        .collect();
    until(|| broker.waiting_for_adoption() == 32).await;

    // The CLI (and the dashboard, through the same client) sees it.
    let cli = within(client.command(step_cancel(project)))
        .await
        .unwrap()
        .unwrap_err();
    assert!(refused_unrun(&cli), "{cli:?}");
    // MCP and HTTP return the error as it is serialized here.
    let wire = serde_json::to_value(&cli).unwrap();
    assert_eq!(
        (wire["error"].as_str(), wire["retryable"].as_bool()),
        (Some("busy"), Some(true))
    );
    // A run's guardian gets the same retryable refusal for its gated callback.
    let run = within(link(home.path(), &l).request(cancel_callback(&l, project, "over")))
        .await
        .unwrap()
        .unwrap_err();
    assert!(refused_unrun(&run), "{run:?}");
    assert!(!attempt(&broker, l.identity.run).await.1);

    host.release(1);
    for request in waiting {
        within(request).await.unwrap().unwrap().unwrap();
    }
    assert_eq!(broker.waiting_for_adoption(), 0);
    stop.cancel();
    within(server).await.unwrap().unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_during_the_pass_refuses_waiting_requests_and_serve_returns() {
    let host = Fake::gated();
    let (home, broker, runs) = home_with_runs(host.clone(), &["p"], false).await;
    let (project, l) = runs[0].clone();
    let (stop, server) = serve(&broker, home.path()).await;
    host.until_entered(1).await;
    let client = CoordinatorClient::new(home.path());
    let user = tokio::spawn({
        let client = client.clone();
        async move { client.command(step_cancel(project)).await }
    });
    let run = tokio::spawn({
        let (link, command) = (link(home.path(), &l), cancel_callback(&l, project, "stop"));
        async move { link.request(command).await }
    });
    until(|| broker.waiting_for_adoption() == 2).await;

    stop.cancel();
    // Both are told at once that they did not run, while the run being
    // adopted still holds the pass.
    let user = within(user).await.unwrap().unwrap().unwrap_err();
    assert!(refused_unrun(&user), "{user:?}");
    let run = within(run).await.unwrap().unwrap().unwrap_err();
    assert!(refused_unrun(&run), "{run:?}");
    assert!(!server.is_finished());
    // The adoption in flight finishes, and serve returns.
    host.release(1);
    within(server).await.unwrap().unwrap().unwrap();
    assert!(!broker.adopted());
    assert!(!attempt(&broker, l.identity.run).await.1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_startup_pass_refuses_waiting_requests_and_serve_returns_its_error() {
    let host = Fake::open();
    let (home, broker, runs) = home_with_runs(host.clone(), &["p"], false).await;
    let (project, l) = runs[0].clone();
    // The pass cannot read this attempt's frozen request.
    let attempt_id = l.identity.attempt;
    broker
        .writer()
        .write(sluice_store::RetrySafety::Idempotent, move |tx| {
            tx.sql().execute(
                "UPDATE attempts SET request=json_remove(request,'$.provenance.runtime.capability') WHERE attempt_id=?1",
                [attempt_id.to_string()],
            )?;
            tx.changed(None, "status");
            Ok(())
        })
        .await
        .unwrap();
    // Hold every read connection so the pass cannot start before a request waits.
    let (release_reads, blocked) = std::sync::mpsc::channel::<()>();
    let blocked = Arc::new(Mutex::new(blocked));
    let (entered_read, reads_held) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut holders = Vec::new();
    for _ in 0..4 {
        let (blocked, entered_read, reads) = (
            blocked.clone(),
            entered_read.clone(),
            broker.reads().clone(),
        );
        holders.push(tokio::spawn(async move {
            reads
                .snapshot(move |_| {
                    entered_read.send(()).unwrap();
                    let _ = blocked.lock().unwrap().recv();
                    Ok(())
                })
                .await
        }));
    }
    let mut reads_held = reads_held;
    for _ in 0..4 {
        within(reads_held.recv()).await.unwrap().unwrap();
    }
    let (_stop, server) = serve(&broker, home.path()).await;
    let client = CoordinatorClient::new(home.path());
    let user = tokio::spawn({
        let client = client.clone();
        async move { client.command(step_cancel(project)).await }
    });
    until(|| broker.waiting_for_adoption() == 1).await;
    drop(release_reads);
    for holder in holders {
        within(holder).await.unwrap().unwrap().unwrap();
    }
    let user = within(user).await.unwrap().unwrap().unwrap_err();
    assert!(refused_unrun(&user), "{user:?}");
    assert!(within(server).await.unwrap().unwrap().is_err());
    assert!(!broker.adopted());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn held_watches_leave_the_connection_pool_two_per_run() {
    let host = Fake::open();
    let home = home::ScratchHome::new().unwrap();
    let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), host.clone())
        .await
        .unwrap();
    let id = project(&broker, "many").await;
    let ops: Vec<Value> = (0..35)
        .map(|n| json!({"op":"step.add","step":format!("w{n}"),"spec":{"run":"fixture.submit","in":{"value":{"default":n}},"outputs":{"submitted":"boolean"}}}))
        .collect();
    broker.command(request(json!({"command":"plan_edit","args":{"project":selector(id),"rev":2,"ops":ops,"start":true,"dry_run":false,"reason":"test","author":"test"}}))).await.unwrap();
    broker.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&broker, id, "s").await.unwrap();
    broker.release_scheduler("s".into()).await.unwrap();
    let launches = host.0.launches.lock().unwrap().clone();
    assert_eq!(launches.len(), 36);
    let (stop, server) = serve(&broker, home.path()).await;
    until(|| broker.adopted()).await;
    let watch = |l: &Launch| {
        let (path, l) = (home.path().join("coordinator.sock"), l.clone());
        async move {
            let mut stream = tokio::net::UnixStream::connect(path).await.unwrap();
            socket::write_frame(
                &mut stream,
                &socket::Request {
                    protocol: PROTOCOL_VERSION,
                    request_id: RequestId(InvocationId::new().to_string()),
                    run_capability: Some(l.capability.clone()),
                    command: C::Watch {
                        identity: l.identity.clone(),
                        after: MessageId(0),
                        wait_ms: 60_000,
                    },
                },
            )
            .await
            .unwrap();
            stream
        }
    };
    // 70 held watches, two for each of 35 runs: more than the 64 connections.
    let mut held = Vec::new();
    for l in &launches[..35] {
        for _ in 0..2 {
            held.push(watch(l).await);
        }
    }
    until(|| broker.released_watches() == 70 && broker.connections_in_use() == 0).await;
    let client = CoordinatorClient::new(home.path());
    assert!(matches!(
        within(client.command(CommandRequest::ProjectsList))
            .await
            .unwrap()
            .unwrap(),
        CommandReply::Projects(_)
    ));
    // A third watch for one run keeps its connection permit.
    held.push(watch(&launches[0]).await);
    until(|| broker.connections_in_use() == 1).await;
    assert_eq!(broker.released_watches(), 70);
    // Hanging up ends them.
    drop(held);
    until(|| broker.released_watches() == 0 && broker.connections_in_use() == 0).await;
    stop.cancel();
    within(server).await.unwrap().unwrap().unwrap();
}
