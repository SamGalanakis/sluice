//! Automatic retiring of done units (SPEC §6.11): the project setting, one edit by `sluice`,
//! what it keeps, the rev race, the per-project turn and the waits it leaves asleep.
#[path = "../../../tests/support/home.rs"]
mod home;
use serde_json::{Value, json};
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
    retire::{self, Retirement, Retirer},
};
use sluice_store::{RetrySafety, projects::RetireSetting};
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

struct Fixture {
    _home: home::ScratchHome,
    broker: Coordinator<Fake>,
    client: CoordinatorClient,
    project: ProjectId,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<Result<(), PublicError>>,
}
impl Fixture {
    async fn new() -> Self {
        let home = home::ScratchHome::new().unwrap();
        assert!(home.root().exists());
        home::ScratchHome::validate(home.path()).unwrap();
        let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), Fake)
            .await
            .unwrap();
        let stop = CancellationToken::new();
        let token = stop.clone();
        let serving = broker.clone();
        let server = tokio::spawn(async move { serving.serve(token).await });
        let client = CoordinatorClient::new(home.path());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !client.path.exists() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let CommandReply::Project(p) = client
            .command(request(
                "project_create",
                json!({"name":"p","description":"","resources":{}}),
            ))
            .await
            .unwrap()
        else {
            panic!("project")
        };
        Self {
            _home: home,
            broker,
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
    async fn plan(&self) -> Value {
        data(self.call("plan_get", json!({})).await.unwrap())
    }
    async fn rev(&self) -> u64 {
        self.plan().await["rev"].as_u64().unwrap()
    }
    async fn steps(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.plan().await["plan"]["steps"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }
    async fn patch(&self, ops: Value) {
        let rev = self.rev().await;
        self.call(
            "plan_patch",
            json!({"rev":rev,"ops":ops,"dry_run":false,"reason":"fixture","start":true}),
        )
        .await
        .unwrap();
    }
    /// Record a done core.external step's outputs, as an orchestrator would.
    async fn finish(&self, step: &str) {
        self.call(
            "step_set_output",
            json!({"step":step,"outputs":{"ok":true},"force":true,"reason":"fixture"}),
        )
        .await
        .unwrap();
    }
    /// Turn retiring on through project_update, as any client does.
    async fn retire_after(&self, seconds: Value, keep: Value) {
        self.call(
            "project_update",
            json!({"prune_done_after":seconds,"prune_keep":keep,"author":"owner"}),
        )
        .await
        .unwrap();
    }
    async fn setting(&self) -> Option<RetireSetting> {
        self.broker
            .reads()
            .snapshot(sluice_store::projects::retire_settings)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.project == self.project)
    }
    /// Every plan edit as (author, reason).
    async fn edits(&self) -> Vec<(String, String)> {
        data(self.call("plan_history", json!({})).await.unwrap())
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["kind"] == "plan.edit")
            .map(|r| {
                (
                    r["author"].as_str().unwrap().to_owned(),
                    r["reason"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
    async fn last_seq(&self) -> u64 {
        let project = self.project.to_string();
        self.broker
            .reads()
            .snapshot(move |sql| {
                Ok(sql.query_row(
                    "SELECT coalesce(max(seq),0) FROM records WHERE project_id=?1",
                    [project],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap() as u64
    }
    async fn close(self) {
        self.stop.cancel();
        self.server.await.unwrap().unwrap();
    }
}
fn request(name: &str, args: Value) -> CommandRequest {
    decode_json(&serde_json::to_vec(&json!({"command":name,"args":args})).unwrap()).unwrap()
}
fn data(reply: CommandReply) -> Value {
    let CommandReply::Data(v) = reply else {
        panic!("expected data, got {reply:?}")
    };
    v.into_value()
}
fn external(unit: &str) -> Value {
    json!({"run":"core.external","outputs":{"ok":"boolean"},"tags":[format!("unit:{unit}")]})
}
fn names(units: &[UnitName]) -> Vec<&str> {
    units.iter().map(UnitName::as_str).collect()
}
/// Long enough for a result recorded now to be older than a one-second setting.
async fn age() {
    tokio::time::sleep(Duration::from_millis(1100)).await;
}

/// old and ta-x are done; held is done but a pending step reads it; live is pending and
/// failing is failed.
async fn lanes(f: &Fixture) {
    f.patch(json!([
        {"op":"add","path":"/steps/old","value":external("old")},
        {"op":"add","path":"/steps/ta-x","value":external("ta-x")},
        {"op":"add","path":"/steps/held","value":external("held")},
        {"op":"add","path":"/steps/reader","value":{"run":"fixture.echo","in":{"value":{"source":"held/ok"}},"tags":["unit:reader"]}},
        {"op":"add","path":"/steps/live","value":external("live")},
        {"op":"add","path":"/steps/failing","value":external("failing")}
    ]))
    .await;
    for step in ["old", "ta-x", "held"] {
        f.finish(step).await;
    }
    let project = f.project.to_string();
    f.broker
        .writer()
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='failed' WHERE project_id=?1 AND step_id='failing'",
                [&project],
            )?;
            tx.changed(Some(project.parse().unwrap()), "status");
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn the_setting_round_trips_through_project_update() {
    let f = Fixture::new().await;
    assert_eq!(f.setting().await, None);
    f.retire_after(json!(21600), json!(["ta-*"])).await;
    assert_eq!(
        f.setting().await,
        Some(RetireSetting {
            project: f.project,
            after: 21600,
            keep: vec!["ta-*".into()],
        })
    );
    // Leaving a field out keeps it; null turns retiring off.
    f.call("project_update", json!({"description":"lanes"}))
        .await
        .unwrap();
    assert_eq!(f.setting().await.unwrap().after, 21600);
    for bad in [json!({"prune_done_after":0}), json!({"prune_keep":["a b"]})] {
        assert!(matches!(
            f.call("project_update", bad).await,
            Err(PublicError::Invalid { .. })
        ));
    }
    f.call("project_update", json!({"prune_done_after":null}))
        .await
        .unwrap();
    assert_eq!(f.setting().await, None);
    let project = f.project.to_string();
    let keep: String = f
        .broker
        .reads()
        .snapshot(move |sql| {
            Ok(sql.query_row(
                "SELECT prune_keep FROM projects WHERE project_id=?1",
                [project],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(keep, r#"["ta-*"]"#);
    f.close().await;
}

#[tokio::test]
async fn done_units_past_the_age_go_in_one_edit_by_sluice_and_the_rest_stay() {
    let f = Fixture::new().await;
    lanes(&f).await;
    f.retire_after(json!(1), json!(["ta-*"])).await;
    age().await;
    let before = f.rev().await;
    let Retirement::Retired(result) = retire::retire(&f.broker, &f.setting().await.unwrap()).await
    else {
        panic!("nothing retired")
    };
    assert_eq!(names(&result.units), ["old"]);
    assert_eq!(result.edit.rev, Revision(before + 1));
    let kept: Vec<_> = result
        .kept
        .iter()
        .map(|k| serde_json::to_value(k).unwrap())
        .collect();
    assert_eq!(
        kept,
        [
            json!({"unit":"ta-x","keep":"ta-*"}),
            json!({"unit":"held","step":"reader"})
        ]
    );
    assert_eq!(
        f.steps().await,
        ["failing", "held", "live", "reader", "ta-x"]
    );
    let project = f.project.to_string();
    let failing: String = f
        .broker
        .reads()
        .snapshot(move |sql| {
            Ok(sql.query_row(
                "SELECT status FROM steps WHERE project_id=?1 AND step_id='failing'",
                [project],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(failing, "failed");
    assert_eq!(
        f.edits().await.last().unwrap(),
        &(
            "sluice".to_owned(),
            "retire done units older than 1s".to_owned()
        )
    );
    // The removed step's result stays in outcomes.
    let project = f.project.to_string();
    let outcomes: i64 = f
        .broker
        .reads()
        .snapshot(move |sql| {
            Ok(sql.query_row(
                "SELECT count(*) FROM outcomes WHERE project_id=?1 AND step_id='old'",
                [project],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(outcomes, 1);
    // Nothing qualifies now: no edit.
    assert!(matches!(
        retire::retire(&f.broker, &f.setting().await.unwrap()).await,
        Retirement::Nothing
    ));
    assert_eq!(f.rev().await, before + 1);
    f.close().await;
}

#[tokio::test]
async fn nothing_happens_when_the_setting_is_off_or_nothing_is_old_enough() {
    let f = Fixture::new().await;
    lanes(&f).await;
    age().await;
    let rev = f.rev().await;
    let mut retirer = Retirer::new(Duration::ZERO);
    // Off: the project is not looked at.
    assert!(
        retirer
            .pass(&f.broker, Instant::now())
            .await
            .unwrap()
            .is_empty()
    );
    // On, but no unit finished an hour ago: a look and no edit.
    f.retire_after(json!(3600), json!([])).await;
    let done = retirer.pass(&f.broker, Instant::now()).await.unwrap();
    assert!(matches!(done.as_slice(), [(p, Retirement::Nothing)] if *p == f.project));
    assert_eq!(f.rev().await, rev);
    assert!(f.edits().await.iter().all(|(author, _)| author != "sluice"));
    f.close().await;
}

#[tokio::test]
async fn an_edit_between_the_look_and_the_prune_skips_the_round() {
    let f = Fixture::new().await;
    lanes(&f).await;
    f.retire_after(json!(1), json!([])).await;
    age().await;
    let candidate = retire::candidate(&f.broker, &f.setting().await.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(names(&candidate.units), ["old", "ta-x"]);
    // An orchestrator edits the plan in between.
    f.patch(json!([{"op":"add","path":"/steps/new","value":external("new")}]))
        .await;
    let rev = f.rev().await;
    let outcome = retire::apply(&f.broker, candidate).await;
    assert!(
        matches!(outcome, Retirement::Skipped(PublicError::Conflict { .. })),
        "{outcome:?}"
    );
    assert_eq!(f.rev().await, rev);
    assert!(f.steps().await.contains(&"old".to_owned()));
    assert!(f.edits().await.iter().all(|(author, _)| author != "sluice"));
    f.close().await;
}

#[tokio::test]
async fn each_project_is_looked_at_once_per_interval() {
    let f = Fixture::new().await;
    lanes(&f).await;
    f.retire_after(json!(1), json!(["ta-*"])).await;
    age().await;
    let mut retirer = Retirer::new(Duration::from_secs(300));
    let start = Instant::now();
    let done = retirer.pass(&f.broker, start).await.unwrap();
    assert!(matches!(done.as_slice(), [(_, Retirement::Retired(_))]));
    f.patch(json!([{"op":"add","path":"/steps/later","value":external("later")}]))
        .await;
    f.finish("later").await;
    age().await;
    // Its turn has not come again: not looked at, however much qualifies.
    assert!(
        retirer
            .pass(&f.broker, start + Duration::from_secs(299))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(f.steps().await.contains(&"later".to_owned()));
    let done = retirer
        .pass(&f.broker, start + Duration::from_secs(300))
        .await
        .unwrap();
    let [(_, Retirement::Retired(result))] = done.as_slice() else {
        panic!("{done:?}")
    };
    assert_eq!(names(&result.units), ["later"]);
    f.close().await;
}

#[tokio::test]
async fn retiring_wakes_no_orchestrator_next() {
    let f = Fixture::new().await;
    lanes(&f).await;
    f.retire_after(json!(1), json!([])).await;
    age().await;
    let since = f.last_seq().await;
    assert!(matches!(
        retire::retire(&f.broker, &f.setting().await.unwrap()).await,
        Retirement::Retired(_)
    ));
    let next = |all: bool| {
        request(
            "next",
            json!({"projects":[{"kind":"id","value":f.project}],"since_seq":since,"me":"orchestrator",
                   "timeout_seconds":1,"all":all,"settle_seconds":0,"settle_max_seconds":0,"settles":"none"}),
        )
    };
    let CommandReply::Next(result) = f.client.command(next(false)).await.unwrap() else {
        panic!("next")
    };
    assert!(result.timed_out, "{result:?}");
    assert!(result.records.is_empty(), "{result:?}");
    // The edit is in the log after `since`: `next(all=true)` sees it, as any plan edit.
    let CommandReply::Next(result) = f.client.command(next(true)).await.unwrap() else {
        panic!("next")
    };
    assert!(
        result
            .records
            .iter()
            .any(|r| serde_json::to_value(r).unwrap()["kind"] == "plan.edit")
    );
    f.close().await;
}

#[tokio::test]
async fn plan_prune_keep_keeps_matching_units_and_names_the_pattern() {
    let f = Fixture::new().await;
    lanes(&f).await;
    let CommandReply::Pruned(result) = f
        .call(
            "plan_prune",
            json!({"older_than_seconds":0,"keep":["ta-*"],"edit":{"dry_run":false,"reason":"manual"}}),
        )
        .await
        .unwrap()
    else {
        panic!("pruned")
    };
    assert_eq!(names(&result.units), ["old"]);
    assert_eq!(
        serde_json::to_value(&result.kept).unwrap(),
        json!([{"unit":"ta-x","keep":"ta-*"},{"unit":"held","step":"reader"}])
    );
    assert!(f.steps().await.contains(&"ta-x".to_owned()));
    // Without keep, behaviour is as before: the done ta-x goes.
    let CommandReply::Pruned(result) = f
        .call(
            "plan_prune",
            json!({"older_than_seconds":0,"edit":{"dry_run":false,"reason":"manual"}}),
        )
        .await
        .unwrap()
    else {
        panic!("pruned")
    };
    assert_eq!(names(&result.units), ["ta-x"]);
    f.close().await;
}
