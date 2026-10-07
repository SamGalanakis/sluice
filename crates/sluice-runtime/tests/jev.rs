//! jev.* builtins against a local HTTP fixture, porting packs/jev/tests/test_jev.py.
//! Requests stay on the local HTTP fixture.

use axum::{
    Json, Router,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use sluice_model::rpc::JsonMap;
use sluice_runtime::builtins::jev::{self, BuiltinCtx};
use std::{
    future::IntoFuture,
    sync::{Arc, Mutex},
};

const TICKET: &str = "Help! My payouts have been failing for 3 days and I have payroll tomorrow.";

#[derive(Debug)]
struct Captured {
    auth: Option<String>,
    body: Value,
}

type Respond = dyn Fn(&Value) -> (u16, String) + Send + Sync;

#[derive(Clone)]
struct Fixture {
    captured: Arc<Mutex<Vec<Captured>>>,
    respond: Arc<Respond>,
}

async fn handle(
    State(fixture): State<Fixture>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let auth = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    fixture.captured.lock().unwrap().push(Captured {
        auth,
        body: body.clone(),
    });
    let (status, text) = (fixture.respond)(&body);
    (axum::http::StatusCode::from_u16(status).unwrap(), text).into_response()
}

/// Bind 127.0.0.1 on a free port and serve `respond` for POST /v1/systemone.
async fn fixture(
    respond: impl Fn(&Value) -> (u16, String) + Send + Sync + 'static,
) -> (Fixture, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let fixture = Fixture {
        captured: Arc::new(Mutex::new(vec![])),
        respond: Arc::new(respond),
    };
    let app = Router::new()
        .route("/v1/systemone", post(handle))
        .with_state(fixture.clone());
    tokio::spawn(axum::serve(listener, app).into_future());
    (fixture, base)
}

fn ctx(env: &[(&str, &str)]) -> BuiltinCtx {
    BuiltinCtx::new(
        env.iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string())),
    )
}
fn inputs(value: Value) -> JsonMap {
    serde_json::from_value(value).unwrap()
}
fn get<'a>(out: &'a JsonMap, name: &str) -> &'a Value {
    out.0.get(name).unwrap().as_value()
}

#[tokio::test]
async fn choice_picks_the_obvious_option() {
    let (fixture, base) = fixture(|_| {
        (
            200,
            json!({
                "answers": {"q": {
                    "choice": "billing",
                    "probabilities": {"billing": 0.9, "sales": 0.07, "design": 0.03},
                    "confidence": 0.9}},
                "model": "jev-2026-10-03",
                "usage": {"input_tokens": 41},
            })
            .to_string(),
        )
    })
    .await;
    let ctx = ctx(&[
        ("TYPESAFE_API_KEY", "test-key"),
        ("TYPESAFE_BASE_URL", &base),
    ]);
    let out = jev::choice(
        &inputs(json!({
            "state": TICKET,
            "instructions": "Which team should handle this?",
            "options": {"billing": "Payments, payouts, refunds",
                        "sales": "Pricing, new accounts", "design": "Logos and branding"}})),
        &ctx,
    )
    .await
    .unwrap();

    assert_eq!(get(&out, "choice"), "billing");
    let probabilities = get(&out, "probabilities").as_object().unwrap();
    assert_eq!(
        probabilities
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        ["billing", "design", "sales"].into_iter().collect()
    );
    assert!(probabilities["billing"].as_f64().unwrap() > 0.5);
    assert_eq!(
        get(&out, "confident"),
        &Value::Bool(get(&out, "confidence").as_f64().unwrap() >= 0.8)
    );
    assert!(get(&out, "model").as_str().unwrap().starts_with("jev"));

    let captured = fixture.captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].auth.as_deref(), Some("Bearer test-key"));
    let sent = &captured[0].body;
    assert_eq!(sent["state"], TICKET);
    assert_eq!(sent["model"], "jev-latest");
    assert_eq!(
        sent["questions"],
        json!({"q": {"type": "choice", "instructions": "Which team should handle this?",
            "criteria": {"billing": "Payments, payouts, refunds",
                         "sales": "Pricing, new accounts", "design": "Logos and branding"}}})
    );
}

#[tokio::test]
async fn choice_accepts_a_plain_list_and_threshold() {
    let (fixture, base) = fixture(|_| {
        (
            200,
            json!({"answers": {"q": {"choice": "money",
                    "probabilities": {"money": 0.4, "colours": 0.6}, "confidence": 0.4}},
                   "model": "jev-x"})
            .to_string(),
        )
    })
    .await;
    let ctx = ctx(&[
        ("TYPESAFE_API_KEY", "test-key"),
        ("TYPESAFE_BASE_URL", &base),
    ]);
    let out = jev::choice(
        &inputs(json!({
            "state": TICKET,
            "instructions": "Is the customer asking about money or about colours?",
            "options": ["money", "colours"],
            "min_confidence": 0.0})),
        &ctx,
    )
    .await
    .unwrap();

    assert_eq!(get(&out, "choice"), "money");
    assert_eq!(get(&out, "confident"), &Value::Bool(true));

    let captured = fixture.captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(
        captured[0].body["questions"]["q"]["criteria"],
        json!({"money": null, "colours": null})
    );
}

#[tokio::test]
async fn noul_separates_yes_from_no() {
    let (fixture, base) = fixture(|body| {
        let urgent = body["state"].as_str().unwrap().contains("payroll");
        (
            200,
            json!({"answers": {"q": {"noul": if urgent { 0.93 } else { 0.04 }}},
                   "model": "jev-x"})
            .to_string(),
        )
    })
    .await;
    let ctx = ctx(&[
        ("TYPESAFE_API_KEY", "test-key"),
        ("TYPESAFE_BASE_URL", &base),
    ]);
    let urgent = jev::noul(
        &inputs(json!({"state": TICKET, "instructions": "Is this urgent?",
                       "yes": "Time-sensitive", "no": "Can wait"})),
        &ctx,
    )
    .await
    .unwrap();
    let calm = jev::noul(
        &inputs(
            json!({"state": "Just curious whether you have a dark mode, no rush at all.",
                       "instructions": "Is this urgent?"}),
        ),
        &ctx,
    )
    .await
    .unwrap();

    assert!(get(&urgent, "noul").as_f64().unwrap() > get(&calm, "noul").as_f64().unwrap());
    assert!(get(&urgent, "noul").as_f64().unwrap() > 0.5);
    assert!(get(&calm, "noul").as_f64().unwrap() < 0.5);

    let captured = fixture.captured.lock().unwrap();
    assert_eq!(captured.len(), 2);
    assert_eq!(
        captured[0].body["questions"]["q"]["criteria"],
        json!({"true": "Time-sensitive", "false": "Can wait"})
    );
    assert!(captured[1].body["questions"]["q"].get("criteria").is_none());
    assert_eq!(captured[1].body["questions"]["q"]["type"], "noul");
}

#[tokio::test]
async fn score_uses_the_given_levels() {
    let (fixture, base) = fixture(|_| {
        (
            200,
            json!({"answers": {"q": {"score": 1.8,
                    "probabilities": {"0": 0.05, "1": 0.3, "2": 0.65},
                    "confidence": 0.82,
                    "legend": {"0": "Calm", "1": "Worried", "2": "Panicking"}}},
                   "model": "jev-x"})
            .to_string(),
        )
    })
    .await;
    let ctx = ctx(&[
        ("TYPESAFE_API_KEY", "test-key"),
        ("TYPESAFE_BASE_URL", &base),
    ]);
    let out = jev::score(
        &inputs(
            json!({"state": TICKET, "instructions": "How stressed is the customer?",
                       "levels": ["Calm", "Worried", "Panicking"]}),
        ),
        &ctx,
    )
    .await
    .unwrap();

    let score = get(&out, "score").as_f64().unwrap();
    assert!((0.0..=2.0).contains(&score));
    assert_eq!(
        get(&out, "legend"),
        &json!({"0": "Calm", "1": "Worried", "2": "Panicking"})
    );
    assert!(score > 0.5);

    let captured = fixture.captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].body["questions"]["q"]["type"], "score");
    assert_eq!(
        captured[0].body["questions"]["q"]["criteria"],
        json!(["Calm", "Worried", "Panicking"])
    );
}

#[tokio::test]
async fn ask_answers_several_questions_in_one_call() {
    let (fixture, base) = fixture(|_| {
        (
            200,
            json!({"answers": {
                    "team": {"choice": "billing",
                             "probabilities": {"billing": 0.9, "sales": 0.1},
                             "confidence": 0.9},
                    "urgent": {"noul": 0.91}},
                   "model": "jev-x",
                   "usage": {"input_tokens": 123, "output_tokens": 17}})
            .to_string(),
        )
    })
    .await;
    let ctx = ctx(&[
        ("TYPESAFE_API_KEY", "test-key"),
        ("TYPESAFE_BASE_URL", &base),
    ]);
    let out = jev::ask(
        &inputs(json!({"state": TICKET, "questions": {
            "team": {"type": "choice", "instructions": "Which team?",
                     "criteria": {"billing": null, "sales": null}},
            "urgent": {"type": "noul", "instructions": "Is this urgent?"}}})),
        &ctx,
    )
    .await
    .unwrap();

    let answers = get(&out, "answers").as_object().unwrap();
    assert_eq!(
        answers
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        ["team", "urgent"].into_iter().collect()
    );
    assert_eq!(answers["team"]["choice"], "billing");
    assert!(get(&out, "usage")["input_tokens"].as_i64().unwrap() > 0);

    let captured = fixture.captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let questions = captured[0].body["questions"].as_object().unwrap();
    assert_eq!(
        questions
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        ["team", "urgent"].into_iter().collect()
    );
    assert_eq!(questions["team"]["type"], "choice");
    assert_eq!(questions["urgent"]["type"], "noul");
    assert!(get(&out, "model").as_str().unwrap().starts_with("jev"));
}

#[tokio::test]
async fn missing_key_fails_without_calling_out() {
    let (fixture, base) = fixture(|_| unreachable!("must not be called")).await;
    let ctx = ctx(&[("TYPESAFE_BASE_URL", &base)]);
    let error = jev::noul(&inputs(json!({"state": "x", "instructions": "y?"})), &ctx)
        .await
        .unwrap_err();
    assert!(!error.is_transient());
    assert!(error.to_string().contains("TYPESAFE_API_KEY is not set"));
    assert!(fixture.captured.lock().unwrap().is_empty());
}

#[tokio::test]
async fn bad_key_fails_with_the_api_error_and_does_not_echo_it() {
    let (fixture, base) = fixture(|_| {
        (
            401,
            json!({"error": "authentication failed for the supplied credential"}).to_string(),
        )
    })
    .await;
    let bad = "apikey_not_a_real_key_0000";
    let ctx = ctx(&[("TYPESAFE_API_KEY", bad), ("TYPESAFE_BASE_URL", &base)]);
    let error = jev::noul(&inputs(json!({"state": "x", "instructions": "y?"})), &ctx)
        .await
        .unwrap_err();
    assert!(!error.is_transient());
    assert!(error.to_string().contains("typesafe 401"));
    assert!(!error.to_string().contains(bad));
    assert_eq!(fixture.captured.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn score_rejects_too_few_levels_before_calling() {
    let (fixture, base) = fixture(|_| unreachable!("must not be called")).await;
    let ctx = ctx(&[("TYPESAFE_API_KEY", "k"), ("TYPESAFE_BASE_URL", &base)]);
    let error = jev::score(
        &inputs(json!({"state": "x", "instructions": "y?", "levels": ["only"]})),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(!error.is_transient());
    assert!(
        error
            .to_string()
            .contains("levels must have 2 to 10 entries")
    );
    assert!(fixture.captured.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rate_limit_and_server_errors_are_transient_other_statuses_terminal() {
    for (status, transient) in [
        (429, true),
        (500, true),
        (503, true),
        (529, true),
        (400, false),
        (401, false),
        (404, false),
        (422, false),
    ] {
        let (fixture, base) = fixture(move |_| (status, format!("detail for {status}"))).await;
        let ctx = ctx(&[("TYPESAFE_API_KEY", "k"), ("TYPESAFE_BASE_URL", &base)]);
        let error = jev::noul(&inputs(json!({"state": "x", "instructions": "y?"})), &ctx)
            .await
            .unwrap_err();
        assert_eq!(error.is_transient(), transient, "status {status}");
        assert!(
            error
                .to_string()
                .starts_with(&format!("typesafe {status}:")),
            "{}",
            error
        );
        assert_eq!(fixture.captured.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn unreachable_host_is_transient() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let ctx = ctx(&[
        ("TYPESAFE_API_KEY", "k"),
        ("TYPESAFE_BASE_URL", &format!("http://127.0.0.1:{port}")),
    ]);
    let error = jev::noul(&inputs(json!({"state": "x", "instructions": "y?"})), &ctx)
        .await
        .unwrap_err();
    assert!(error.is_transient());
}

#[tokio::test]
async fn malformed_success_body_is_terminal() {
    let (_fixture, base) = fixture(|_| (200, "this is not JSON".into())).await;
    let ctx = ctx(&[("TYPESAFE_API_KEY", "k"), ("TYPESAFE_BASE_URL", &base)]);
    let error = jev::noul(&inputs(json!({"state": "x", "instructions": "y?"})), &ctx)
        .await
        .unwrap_err();
    assert!(!error.is_transient());
}

#[tokio::test]
async fn model_comes_from_input_then_env_then_default() {
    let (fixture, base) = fixture(|_| {
        (
            200,
            json!({"answers": {"q": {"noul": 0.5}}, "model": "jev-x"}).to_string(),
        )
    })
    .await;
    let call_inputs =
        |model: Option<&str>| inputs(json!({"state": "x", "instructions": "y?", "model": model}));
    let env_ctx = |model: Option<&str>| {
        let mut env = vec![
            ("TYPESAFE_API_KEY", "k"),
            ("TYPESAFE_BASE_URL", base.as_str()),
        ];
        if let Some(model) = model {
            env.push(("TYPESAFE_MODEL", model));
        }
        ctx(&env)
    };
    jev::noul(&call_inputs(None), &env_ctx(Some("jev-env")))
        .await
        .unwrap();
    jev::noul(&call_inputs(Some("jev-in")), &env_ctx(Some("jev-env")))
        .await
        .unwrap();
    jev::noul(&call_inputs(None), &env_ctx(None)).await.unwrap();
    let captured = fixture.captured.lock().unwrap();
    assert_eq!(captured[0].body["model"], "jev-env");
    assert_eq!(captured[1].body["model"], "jev-in");
    assert_eq!(captured[2].body["model"], "jev-latest");
}
