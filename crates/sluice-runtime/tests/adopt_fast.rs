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
    rpc::{FnInvocation, JsonMap, decode_json},
};
use sluice_process::{
    guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence},
    journal::{CleanupEvidence, CompletionJournal},
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
    broker.command(request(json!({"command":"plan_patch","args":{"project":selector(id),"rev":1,"ops":[{"op":"add","path":"/steps/work","value":{"run":"fixture.submit","in":{"value":{"default":1}},"outputs":{"submitted":"boolean"}}}],"start":true,"dry_run":false,"reason":"test","author":"test"}}))).await.unwrap();
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
        json!({"command":"status","args":{"project":selector(project),"selection":{"steps":null,"tags":null}}}),
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
    std::fs::write(release.join("manifest.json"), b"{}").unwrap();
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
    // install status is local; install unfence reaches the coordinator.
    assert!(installation.status().unwrap().fence.is_some());
    let released = sluice_runtime::install::release_cutover(installation.clone())
        .await
        .unwrap();
    assert!(released.fence.is_none());
    assert!(installation.status().unwrap().fence.is_none());

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
    // The lease is held in-process before `serve`, as `run_home` does.
    let (home, broker, runs) = home_with_runs(host.clone(), &["adopted"], true).await;
    assert_eq!(
        broker.scheduler_owner().await.unwrap().as_deref(),
        Some("s")
    );
    // A second project whose step is ready but was never admitted.
    let waiting = project(&broker, "ready").await;
    assert_eq!(host.launches(), 1);

    let (stop, server) = serve(&broker, home.path()).await;
    host.until_entered(1).await;
    let client = CoordinatorClient::new(home.path());
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
    stop.cancel();
    within(server).await.unwrap().unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_does_not_refresh_a_current_registry() {
    let host = Fake::composed();
    let home = home::ScratchHome::new().unwrap();
    let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), host)
        .await
        .unwrap();
    let publication = broker.catalog().1.clone().expect("composition publishes");
    let id = project(&broker, "p").await;
    // The project list changed after the write's own refresh: one read
    // republishes, and from then on the registry is current.
    broker.command(CommandRequest::ProjectsList).await.unwrap();
    let current = publication.refreshes();
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
    for _ in 0..3 {
        for read in reads.clone() {
            broker.command(read).await.unwrap();
        }
    }
    assert_eq!(publication.refreshes(), current);
    // A write still refreshes before it runs.
    broker.command(request(json!({"command":"project_update","args":{"project":selector(id),"new_name":null,"description":"changed","icon":null,"resources":null,"paused":null,"archived":null,"expected_settings_rev":null,"reason":null,"author":"test"}}))).await.unwrap();
    assert_eq!(publication.refreshes(), current + 1);
    broker.command(status(id)).await.unwrap();
    assert_eq!(publication.refreshes(), current + 1);
}
