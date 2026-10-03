//! core.* builtin tests: dispatch for echo/collect/format, the strict template
//! language, and the store-level core.external lifecycle the runtime must expose
//! (waits ready, reserves never, settles by hand, cancels at once, stales on new data).
use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::{
    commands::{StepCancel, StepSelection, StepSetOutput, StepStatus},
    error::PublicError,
    gates::{GateDecision, StateSnapshot, readiness},
    ids::{AttemptId, ProjectId, ProjectSelector, ResultId, RunId, StepId},
    plan::{FnSignature, Plan, SignatureProvider, inputs_hash},
    rpc::{JsonMap, JsonValue, decode_json},
    types::Type,
};
use sluice_runtime::builtins::{
    core,
    jev::{BuiltinCtx, FnFailure},
};
use sluice_store::{
    ReadPool, Result as StoreResult, RetrySafety, WriteTransaction, Writer,
    attempts::{self, AssignedRange, AttemptIdentity, ExecutionHooks, Reserve},
    plans::{self, PlanContext, RetryMessages},
};
use std::path::{Path, PathBuf};

fn map(value: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn ctx() -> BuiltinCtx {
    BuiltinCtx {
        env: IndexMap::new(),
    }
}
fn id(name: &str) -> StepId {
    name.parse().unwrap()
}
async fn dispatch(name: &str, inputs: Value) -> Result<JsonMap, FnFailure> {
    core::dispatch(name, &map(inputs), &ctx()).await
}
fn out(map: &JsonMap, name: &str) -> Value {
    map.0.get(name).unwrap().as_value().clone()
}
fn failed(result: Result<JsonMap, FnFailure>) -> String {
    match result {
        Err(FnFailure::Terminal(message)) => message,
        Err(FnFailure::Transient(message)) => panic!("unexpected transient failure: {message}"),
        Err(FnFailure::NotBuilt(name)) => panic!("unexpected not-built failure: {name}"),
        Ok(value) => panic!("expected a terminal failure, got {value:?}"),
    }
}

#[tokio::test]
async fn echo_passes_its_value_through_verbatim() {
    for value in [
        json!("text"),
        json!(7),
        json!({"a":[1,"x",null]}),
        json!([true,{"k":null}]),
        json!(null),
    ] {
        let result = dispatch("core.echo", json!({"value": value}))
            .await
            .unwrap();
        assert_eq!(out(&result, "value"), value);
    }
    assert_eq!(
        failed(dispatch("core.echo", json!({})).await),
        "missing required input \"value\""
    );
}

#[tokio::test]
async fn collect_returns_its_items_array() {
    let result = dispatch("core.collect", json!({"items": [1, "x", {"k": null}]}))
        .await
        .unwrap();
    assert_eq!(out(&result, "items"), json!([1, "x", {"k": null}]));
    let empty = dispatch("core.collect", json!({"items": []}))
        .await
        .unwrap();
    assert_eq!(out(&empty, "items"), json!([]));
    assert_eq!(
        failed(dispatch("core.collect", json!({})).await),
        "missing required input \"items\""
    );
}

#[tokio::test]
async fn format_fills_record_names_and_renders_non_strings_as_compact_json() {
    let result = dispatch(
        "core.format",
        json!({"template": "hi {who}, {n} + {rest} = {sum}",
               "values": {"who": "world", "n": 4, "rest": [1, 2], "sum": {"a": true}}}),
    )
    .await
    .unwrap();
    assert_eq!(out(&result, "text"), "hi world, 4 + [1,2] = {\"a\":true}");
}

#[tokio::test]
async fn format_fills_positions_from_arrays_or_a_single_value() {
    let result = dispatch(
        "core.format",
        json!({"template": "{0} then {1}", "values": ["first", 2]}),
    )
    .await
    .unwrap();
    assert_eq!(out(&result, "text"), "first then 2");
    let single = dispatch("core.format", json!({"template": "{0}!", "values": 7}))
        .await
        .unwrap();
    assert_eq!(out(&single, "text"), "7!");
    assert!(
        failed(dispatch("core.format", json!({"template": "{1}", "values": "one"})).await)
            .contains("single value")
    );
    assert!(
        failed(
            dispatch(
                "core.format",
                json!({"template": "{2}", "values": ["a", "b"]})
            )
            .await
        )
        .contains("has 2 item(s)")
    );
}

#[tokio::test]
async fn format_escapes_braces_and_rejects_lone_ones() {
    let result = dispatch(
        "core.format",
        json!({"template": "{{{0}}} {{literal}} }}", "values": ["x"]}),
    )
    .await
    .unwrap();
    assert_eq!(out(&result, "text"), "{x} {literal} }");
    assert!(
        failed(
            dispatch(
                "core.format",
                json!({"template": "unclosed {", "values": {}})
            )
            .await
        )
        .contains("unclosed '{'")
    );
    assert!(
        failed(
            dispatch(
                "core.format",
                json!({"template": "{a{b}", "values": {"a": 1, "b": 2}})
            )
            .await
        )
        .contains("unclosed '{'")
    );
    assert!(
        failed(dispatch("core.format", json!({"template": "stray }", "values": {}})).await)
            .contains("unmatched '}'")
    );
    assert!(
        failed(dispatch("core.format", json!({"template": "empty {}", "values": {}})).await)
            .contains("empty placeholder")
    );
}

#[tokio::test]
async fn format_refuses_python_format_syntax_and_wrong_value_shapes() {
    for (template, refused) in [
        ("{x.y}", "attribute or item access"),
        ("{x[0]}", "attribute or item access"),
        ("{x!r}", "conversion flags"),
        ("{x!s:>10}", "conversion flags"),
        ("{x:>10}", "format specifications"),
        ("{x:.2f}", "format specifications"),
    ] {
        let message = failed(
            dispatch(
                "core.format",
                json!({"template": template, "values": {"x": 1}}),
            )
            .await,
        );
        assert!(message.contains(refused), "{template}: {message}");
    }
    assert!(
        failed(
            dispatch(
                "core.format",
                json!({"template": "{missing}", "values": {"x": 1}})
            )
            .await
        )
        .contains("no value 'missing'")
    );
    assert!(
        failed(
            dispatch(
                "core.format",
                json!({"template": "{0}", "values": {"x": 1}})
            )
            .await
        )
        .contains("is a record")
    );
    assert!(
        failed(
            dispatch(
                "core.format",
                json!({"template": "{name}", "values": ["a"]})
            )
            .await
        )
        .contains("is an array")
    );
}

#[tokio::test]
async fn external_is_named_and_dispatch_refuses_it_terminally() {
    assert!(core::is_external(core::EXTERNAL));
    assert!(!core::is_external("core.echo"));
    assert!(failed(dispatch(core::EXTERNAL, json!({})).await).contains("never runs"));
    assert!(failed(dispatch("core.unknown", json!({})).await).contains("unknown core builtin"));
}

// --- core.external against the real store ---
// The runtime never executes external work; these tests pin the lifecycle the
// store already implements and the scheduler/calls paths rely on: ready waits,
// refused reservation, manual outputs, force/staleness, instant cancellation.

struct Scratch {
    dir: PathBuf,
}
impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("sluice-p403-core-{}", RunId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
    fn path(&self) -> &Path {
        &self.dir
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Signatures;
impl SignatureProvider for Signatures {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        let (inputs, outputs, open) = match name {
            "core.external" => (json!({}), json!({}), true),
            "empty" => (json!({}), json!({}), false),
            _ => return None,
        };
        Some(FnSignature {
            inputs: inputs
                .as_object()
                .unwrap()
                .iter()
                .map(|(n, t)| (n.clone(), Type::parse(t).unwrap()))
                .collect(),
            outputs: outputs
                .as_object()
                .unwrap()
                .iter()
                .map(|(n, t)| (n.clone(), Type::parse(t).unwrap()))
                .collect(),
            open,
            ..FnSignature::default()
        })
    }
}

struct Fixture {
    _home: Scratch,
    writer: Writer,
    reads: ReadPool,
    context: PlanContext,
}
impl Fixture {
    async fn new(doc: Value) -> Self {
        let home = Scratch::new();
        let writer = Writer::open(home.path()).unwrap();
        let project = ProjectId::new();
        let plan = Plan::parse(&map(doc), &Signatures).unwrap();
        let copy = plan.clone();
        writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute(
                    "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')",
                    (project.to_string(),),
                )?;
                plans::initialize_plan(tx, project, &copy)
            })
            .await
            .unwrap();
        let reads = ReadPool::open(home.path(), 2).unwrap();
        Self {
            _home: home,
            writer,
            reads,
            context: PlanContext {
                project,
                revision: sluice_model::ids::Revision(1),
                plan,
            },
        }
    }
    async fn state(&self) -> StateSnapshot {
        let project = self.context.project;
        self.reads
            .snapshot(move |c| plans::read_state(c, project))
            .await
            .unwrap()
    }
    async fn manual(&self, step: &str, outputs: Value, force: bool) -> ResultId {
        let context = self.context.clone();
        let request = StepSetOutput {
            project: ProjectSelector::Id(context.project),
            step: id(step),
            outputs: map(outputs),
            force,
            reason: "manual".into(),
            author: Some("sam".into()),
        };
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                plans::step_set_output(tx, &context, request)
            })
            .await
            .unwrap()
    }
}

/// reserve never runs its hooks on an external step — they are unreachable here.
struct Hooks;
impl RetryMessages for Hooks {
    fn validate_retry(
        &self,
        _: &WriteTransaction<'_>,
        _: ProjectId,
        _: &[StepId],
        _: &str,
        _: &str,
    ) -> StoreResult<()> {
        Ok(())
    }
    fn post_retry(
        &mut self,
        _: &mut WriteTransaction<'_>,
        _: ProjectId,
        _: &StepId,
        _: &str,
        _: &str,
    ) -> StoreResult<()> {
        unreachable!("external tests never post retries")
    }
}
impl ExecutionHooks for Hooks {
    fn assign(
        &mut self,
        _: &mut WriteTransaction<'_>,
        _: &AttemptIdentity,
        _: i64,
        _: Option<&AssignedRange>,
    ) -> StoreResult<AssignedRange> {
        unreachable!("external steps never reserve")
    }
    fn started(
        &mut self,
        _: &mut WriteTransaction<'_>,
        _: &AttemptIdentity,
        _: &AssignedRange,
    ) -> StoreResult<()> {
        unreachable!("external steps never start")
    }
    fn hold(
        &mut self,
        _: &mut WriteTransaction<'_>,
        _: &AttemptIdentity,
        _: &[(String, u64)],
        _: bool,
    ) -> StoreResult<()> {
        unreachable!("external steps never hold resources")
    }
    fn release(&mut self, _: &mut WriteTransaction<'_>, _: &AttemptIdentity) -> StoreResult<()> {
        unreachable!("external steps never release")
    }
}

#[tokio::test]
async fn external_waits_ready_and_can_never_reserve() {
    let f =
        Fixture::new(json!({"steps":{"a":{"run":"core.external","outputs":{"n":"int"}}}})).await;
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Pending);
    let decision = readiness(&f.context.plan, &f.state().await)
        .swap_remove(&id("a"))
        .unwrap();
    assert_eq!(
        decision,
        GateDecision::Wait(vec![
            "external: set its outputs with step_set_output".into()
        ])
    );
    // Admission refuses to reserve it: a ready external step stays pending.
    let state = f.state().await;
    let request = Reserve {
        step: id("a"),
        attempt: AttemptId::new(),
        run: RunId::new(),
        item_index: -1,
        item_count: None,
        inputs: JsonMap::default(),
        inputs_hash: inputs_hash(&f.context.plan, &state, &f.context.plan.steps()[&id("a")])
            .unwrap(),
        provenance: JsonMap::default(),
        release_id: "fixture".into(),
        protocol_major: 1,
    };
    let context = f.context.clone();
    let error = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            attempts::reserve(tx, &context, request, &mut Hooks)
        })
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("external steps never execute"),
        "{error}"
    );
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Pending);
}

#[tokio::test]
async fn external_settles_through_manually_supplied_outputs() {
    let f = Fixture::new(
        json!({"steps":{"a":{"run":"core.external","outputs":{"n":"int"}},
                         "b":{"run":"empty","after":["a"]}}}),
    )
    .await;
    f.manual("a", json!({"n": 3}), false).await;
    let state = f.state().await;
    assert_eq!(state.status(&id("a")), StepStatus::Succeeded);
    assert_eq!(state.steps[&id("a")].outputs, map(json!({"n": 3})));
    // Settling by hand unblocks the next step like a normal completion would.
    assert_eq!(
        readiness(&f.context.plan, &state).swap_remove(&id("b")),
        Some(GateDecision::Ready)
    );
}

#[tokio::test]
async fn forced_external_output_goes_stale_when_missing_data_arrives() {
    let f = Fixture::new(
        json!({"inputs":{"n":"int"},
               "steps":{"a":{"run":"core.external","in":{"v":{"source":"n"}},"outputs":{"n":"int"}}}}),
    )
    .await;
    // The gate blocks without force: the input is unset.
    let context = f.context.clone();
    let request = StepSetOutput {
        project: ProjectSelector::Id(context.project),
        step: id("a"),
        outputs: map(json!({"n": 1})),
        force: false,
        reason: "manual".into(),
        author: None,
    };
    let error = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::step_set_output(tx, &context, request)
        })
        .await
        .unwrap_err();
    assert!(matches!(error, PublicError::Invalid { .. }));
    f.manual("a", json!({"n": 1}), true).await;
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Succeeded);
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::set_input(
                tx,
                &context,
                "n",
                JsonValue::try_from(json!(9)).unwrap(),
                "sam".into(),
                "now known".into(),
            )
        })
        .await
        .unwrap();
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Stale);
}

#[tokio::test]
async fn pending_external_cancels_into_failure_at_once() {
    let f =
        Fixture::new(json!({"steps":{"a":{"run":"core.external","outputs":{"n":"int"}}}})).await;
    let context = f.context.clone();
    let request = StepCancel {
        project: ProjectSelector::Id(context.project),
        selection: StepSelection {
            steps: Some(vec![id("a")]),
            tags: None,
        },
        reason: "cancel outside work".into(),
        author: Some("sam".into()),
    };
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::step_cancel(tx, &context, request)
        })
        .await
        .unwrap();
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Failed);
    let results: i64 = f
        .reads
        .snapshot(|c| Ok(c.query_row("SELECT count(*) FROM step_results", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(results, 1);
}
