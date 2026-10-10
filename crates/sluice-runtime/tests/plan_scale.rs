//! A small edit to a large plan costs the coordinator a small, bounded time, driven
//! through the coordinator socket: on a lash-shaped plan of 1,980 steps (330 units of
//! fork, work, land, landed, close and rm, with handoffs, step gates and unit gates)
//! each edit tool answers in under a second in a debug build, and a write made during an
//! edit does not wait for it. Each edit used to hold the one writer while it compiled
//! the plan four times and rewrote every step row, serialising the whole plan once per
//! row and copying the fn catalog once per step: seconds per edit in a release build,
//! and on this plan in a debug build the first edit alone outlasts the client's
//! two-minute timeout.
#[allow(dead_code)]
#[path = "../../../tests/support/home.rs"]
mod home;
use serde_json::{Map, Value, json};
use sluice_model::{
    commands::*,
    error::PublicError,
    ids::*,
    rpc::{FnInvocation, JsonMap, decode_json},
};
use sluice_process::guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence};
use sluice_runtime::{
    client::CoordinatorClient,
    coordinator::Coordinator,
    dispatch::Catalog,
    execution::{ExecutionHost, Launch, LaunchOutcome},
};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Fake;
impl FnHost for Fake {
    async fn invoke(&self, _: FnInvocation) -> Result<JsonMap, PublicError> {
        Ok(JsonMap::default())
    }
}
impl AdoptionHost for Fake {
    async fn reconcile(&self, _: &AdoptionAttempt) -> std::io::Result<GuardianPresence> {
        Ok(GuardianPresence::Ambiguous("fixture".into()))
    }
}
impl ExecutionHost for Fake {
    async fn launch(&self, _: Launch) -> Result<LaunchOutcome, PublicError> {
        Ok(LaunchOutcome::Accepted)
    }
    async fn cleanup_valid(
        &self,
        _: &sluice_process::journal::CompletionJournal,
    ) -> Result<bool, PublicError> {
        Ok(true)
    }
}

const UNITS: usize = 330;
const SUFFIXES: [&str; 6] = ["fork", "work", "land", "landed", "close", "rm"];
/// A debug build answers a one-step edit in about 0.2 s and a unit_add (whose recipe
/// expansion compiles the plan twice more) in about 0.6 s; the bounds leave room for a
/// loaded machine.
const BOUND: Duration = Duration::from_millis(1000);
const UNIT_ADD_BOUND: Duration = Duration::from_millis(2500);
/// A write made during an edit takes tens of milliseconds in a debug build.
const CONCURRENT_BOUND: Duration = Duration::from_millis(750);

/// The steps of one lash-shaped unit: a chain of handoffs and step gates inside the
/// unit, its entry gated on the previous unit.
fn unit(name: &str, previous: Option<&str>) -> Map<String, Value> {
    let mut steps = Map::new();
    for (index, suffix) in SUFFIXES.iter().enumerate() {
        let mut step = json!({
            "run": "fixture.echo",
            "doc": format!("{suffix} for {name}, a line of prose as plans carry"),
            "tags": [format!("unit:{name}"), "lane"],
        });
        if index == 0 {
            step["in"] =
                json!({"value": {"default": {"issue": name, "branch": format!("rw/{name}")}}});
            if let Some(previous) = previous {
                step["after"] = json!([format!("unit:{previous}?")]);
            }
        } else {
            let source = format!("{name}-{}", SUFFIXES[index - 1]);
            step["in"] = json!({"value": {"source": format!("{source}/value")}});
            step["after"] = json!([source]);
        }
        if *suffix == "rm" {
            step["tags"] = json!([format!("unit:{name}"), "lane", "exit"]);
        }
        steps.insert(format!("{name}-{suffix}"), step);
    }
    steps
}

struct Fixture {
    _home: home::ScratchHome,
    client: CoordinatorClient,
    project: ProjectId,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<Result<(), PublicError>>,
}
impl Fixture {
    async fn new() -> Self {
        let home = home::ScratchHome::new().unwrap();
        home::ScratchHome::validate(home.path()).unwrap();
        let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), Fake)
            .await
            .unwrap();
        let stop = CancellationToken::new();
        let token = stop.clone();
        let server = tokio::spawn(async move { broker.serve(token).await });
        let client = CoordinatorClient::new(home.path());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !client.path.exists() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let CommandReply::Project(p) = client
            .command(request(
                "project_create",
                json!({"name":"lash","description":"","resources":{}}),
            ))
            .await
            .unwrap()
        else {
            panic!("project")
        };
        let recipes = home
            .path()
            .join("projects")
            .join(p.project_id.to_string())
            .join("recipes");
        std::fs::create_dir_all(&recipes).unwrap();
        let mut steps = Map::new();
        for (key, mut step) in unit("{unit}", None) {
            step.as_object_mut().unwrap().remove("tags");
            steps.insert(key, step);
        }
        std::fs::write(
            recipes.join("lane.json"),
            serde_json::to_vec(&json!({"name":"lane","steps":steps})).unwrap(),
        )
        .unwrap();
        Self {
            _home: home,
            client,
            project: p.project_id,
            stop,
            server,
        }
    }
    async fn call(&self, name: &str, mut args: Value) -> Result<CommandReply, PublicError> {
        args["project"] = json!({"kind":"id","value":self.project});
        self.client.command(request(name, args)).await
    }
    async fn rev(&self) -> u64 {
        let plan = self.call("plan_get", json!({})).await.unwrap();
        serde_json::to_value(&plan).unwrap()["data"]["rev"]
            .as_u64()
            .unwrap()
    }
    /// One edit, timed from the client: its reply is an edit result at a new revision
    /// (or, for a prune that removes nothing, at the same one).
    async fn timed(&self, name: &str, args: Value) -> Duration {
        let before = self.rev().await;
        let started = Instant::now();
        let reply = self.call(name, args).await.unwrap();
        let took = started.elapsed();
        let result = match reply {
            CommandReply::Edit(result) => result,
            CommandReply::Pruned(pruned) => pruned.edit,
            other => panic!("{name}: expected an edit result, got {other:?}"),
        };
        assert!(result.rev.0 >= before, "{name}: {result:?}");
        took
    }
    async fn close(self) {
        self.stop.cancel();
        self.server.await.unwrap().unwrap();
    }
}
fn request(name: &str, args: Value) -> CommandRequest {
    decode_json(&serde_json::to_vec(&json!({"command":name,"args":args})).unwrap()).unwrap()
}
fn edit(reason: &str) -> Value {
    json!({"dry_run":false,"reason":reason})
}

#[tokio::test(flavor = "multi_thread")]
async fn small_edits_to_a_plan_of_two_thousand_steps_take_a_bounded_time() {
    let f = Fixture::new().await;
    let mut steps = Map::new();
    let mut previous = None;
    for index in 0..UNITS {
        let name = format!("fig-{}", 4000 + index);
        steps.extend(unit(&name, previous.as_deref()));
        previous = Some(name);
    }
    assert_eq!(steps.len(), UNITS * SUFFIXES.len());
    // The whole plan in one edit, every step paused so nothing is launched.
    let rev = f.rev().await;
    let adds: Vec<Value> = steps
        .iter()
        .map(|(step, spec)| json!({"op":"step.add","step":step,"spec":spec}))
        .collect();
    f.call(
        "plan_edit",
        json!({"rev":rev,"ops":adds,"reason":"seed","start":false}),
    )
    .await
    .unwrap();

    let mut took = vec![];
    let rev = f.rev().await;
    took.push((
        "plan_edit",
        f.timed(
            "plan_edit",
            json!({"rev":rev,"ops":[{"op":"step.update","step":"fig-4140-rm","changes":{"doc":"probe"}}],
                "reason":"probe"}),
        )
        .await,
    ));
    took.push((
        "step_add",
        f.timed(
            "step_add",
            json!({"step":"extra","spec":{"run":"fixture.echo","in":{"value":{"source":"fig-4140-rm/value"}},"after":["unit:fig-4140"]},
                "start":false,"edit":edit("add")}),
        )
        .await,
    ));
    took.push((
        "edge_add",
        f.timed(
            "edge_add",
            json!({"step":"extra","after":["fig-4200-land"],"edit":edit("gate")}),
        )
        .await,
    ));
    took.push((
        "unit_add",
        f.timed(
            "unit_add",
            json!({"recipe":"lane","unit":"fig-9000","params":{},"after":{"*":["unit:fig-4329?"]},
                "start":false,"edit":edit("unit")}),
        )
        .await,
    ));
    took.push((
        "step_remove",
        f.timed(
            "step_remove",
            json!({"selection":{"steps":["extra"],"tags":null},"edit":edit("remove")}),
        )
        .await,
    ));
    took.push((
        "plan_prune",
        f.timed(
            "plan_prune",
            json!({"units":null,"tags":null,"older_than_seconds":0,"edit":edit("prune")}),
        )
        .await,
    ));
    for (name, one) in &took {
        let bound = if *name == "unit_add" {
            UNIT_ADD_BOUND
        } else {
            BOUND
        };
        assert!(
            *one < bound,
            "{name} on a plan of {} steps took {one:?}; every edit: {took:?}",
            UNITS * SUFFIXES.len(),
        );
    }
    eprintln!("edit times on {} steps: {took:?}", UNITS * SUFFIXES.len());

    // A write made while an edit is being prepared does not wait for it: the edit holds
    // the one writer only to check its tokens and write its rows; it is never prepared in
    // the writer.
    let rev = f.rev().await;
    let patch = f.call(
        "plan_edit",
        json!({"rev":rev,"ops":[{"op":"step.update","step":"fig-4141-rm","changes":{"doc":"held"}}],
            "reason":"probe"}),
    );
    let slot = async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        let started = Instant::now();
        f.call(
            "board_set",
            json!({"program":"root = Doc(\"set while an edit is prepared\")"}),
        )
        .await
        .unwrap();
        started.elapsed()
    };
    let (patched, slot) = tokio::join!(patch, slot);
    let CommandReply::Edit(patched) = patched.unwrap() else {
        panic!("an edit result")
    };
    assert_eq!(patched.rev.0, rev + 1);
    eprintln!("a board set during an edit took {slot:?}");
    assert!(
        slot < CONCURRENT_BOUND,
        "a write during an edit took {slot:?}"
    );

    // Two edits prepared from one revision: the first commits and the second, finding
    // the plan changed when it reaches the writer, is prepared again outside it and commits
    // after it. A plan_edit that names the old revision is refused as stale.
    let rev = f.rev().await;
    let add = |step: &str| {
        json!({"step":step,"spec":{"run":"fixture.echo","in":{"value":{"default":step}}},
            "start":false,"edit":edit("race")})
    };
    let (first, second, stale) = tokio::join!(
        f.call("step_add", add("race-a")),
        f.call("step_add", add("race-b")),
        f.call(
            "plan_edit",
            json!({"rev":rev,"ops":[{"op":"step.add","step":"race-c","spec":{"run":"fixture.echo","in":{"value":{"default":1}},"paused":true}}],
                "reason":"race"}),
        ),
    );
    let mut revs = vec![];
    for reply in [first, second] {
        let CommandReply::Edit(result) = reply.unwrap() else {
            panic!("an edit result")
        };
        revs.push(result.rev.0);
    }
    revs.sort();
    match stale {
        Ok(CommandReply::Edit(result)) => {
            revs.push(result.rev.0);
            revs.sort();
            assert_eq!(revs, [rev + 1, rev + 2, rev + 3]);
        }
        Err(PublicError::Conflict { current_rev, .. }) => {
            assert!(current_rev.is_some_and(|current| current.0 > rev));
            assert_eq!(revs, [rev + 1, rev + 2]);
        }
        other => panic!("plan_edit at rev {rev}: {other:?}"),
    }
    assert_eq!(f.rev().await, *revs.last().unwrap());
    f.close().await;
}
