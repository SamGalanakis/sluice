use askama::Template;
use futures_util::StreamExt;
use sluice_web::{
    streams::{Comparison, PatchRegion, RenderedBatch, StreamEvent, VersionSignal, page_events},
    views::TrustedHtml,
};
use std::sync::{Arc, atomic::AtomicBool};
#[derive(Template)]
#[template(source = "<div id=\"{{ id }}\">{{ text }}</div>", ext = "html")]
struct Region<'a> {
    id: &'a str,
    text: &'a str,
}
fn batch(version: &str, text: &str) -> RenderedBatch {
    RenderedBatch {
        version: version.into(),
        regions: ["one", "two", "three"]
            .into_iter()
            .map(|id| {
                PatchRegion::new(
                    id,
                    TrustedHtml::from_template(&Region { id, text }).unwrap(),
                )
            })
            .collect(),
    }
}
#[test]
fn every_partial_batch_replays_all_regions_until_the_version_was_applied() {
    let events =
        Comparison::new("old".into(), VersionSignal::Page).events(batch("new", "new content"));
    assert_eq!(events.len(), 4);
    for split in 1..events.len() {
        let mut dom = std::collections::BTreeMap::new();
        let mut applied = "old".to_string();
        for event in events.iter().take(split) {
            match event {
                StreamEvent::Elements(p) => {
                    dom.insert(p.selector.clone(), p.elements.clone());
                }
                StreamEvent::Signals(p) => {
                    applied = serde_json::from_str::<serde_json::Value>(&p.signals).unwrap()["ver"]
                        .as_str()
                        .unwrap()
                        .into();
                }
            }
        }
        assert_eq!(applied, "old");
        let replay = Comparison::new(applied.clone(), VersionSignal::Page)
            .events(batch("new", "new content"));
        assert_eq!(replay.len(), 4);
        for event in &replay {
            match event {
                StreamEvent::Elements(p) => {
                    dom.insert(p.selector.clone(), p.elements.clone());
                }
                StreamEvent::Signals(p) => {
                    applied = serde_json::from_str::<serde_json::Value>(&p.signals).unwrap()["ver"]
                        .as_str()
                        .unwrap()
                        .into();
                }
            }
        }
        assert_eq!(dom.len(), 3);
        assert_eq!(applied, "new");
        assert!(
            dom.values()
                .all(|v| v.as_ref().unwrap().contains("new content"))
        );
    }
    let current =
        Comparison::new("new".into(), VersionSignal::Page).events(batch("new", "new content"));
    assert_eq!(current.len(), 1);
    assert!(current[0].wire().contains("stale"));
    assert!(!current[0].wire().contains("ver"));
}
#[test]
fn idle_connections_diff_locally_and_log_only_changes_publish_version_alone() {
    let mut state = Comparison::new("old".into(), VersionSignal::Step);
    let initial = state.events(batch("v1", "same"));
    assert!(
        initial[..3]
            .iter()
            .all(|e| matches!(e, StreamEvent::Elements(_)))
    );
    assert!(initial[3].wire().contains("\"sver\":\"v1\""));
    assert!(initial[0].wire().contains("event: datastar-patch-elements"));
    assert!(state.events(batch("v1", "same")).is_empty());
    let changed = state.events(batch("v2", "same"));
    assert_eq!(changed.len(), 1);
    assert!(matches!(changed[0], StreamEvent::Signals(_)));
    let mut next = batch("v3", "same");
    next.regions[1].html = TrustedHtml::from_template(&Region {
        id: "two",
        text: "changed",
    })
    .unwrap();
    let changed = state.events(next);
    assert_eq!(changed.len(), 2);
    assert!(changed[0].wire().contains("#two"));
}
#[tokio::test]
async fn loader_errors_publish_visible_stale_state_and_end_without_a_producer_task() {
    let events = page_events(
        || async {
            Err(sluice_model::error::PublicError::NotFound {
                message: "gone".into(),
            })
        },
        String::new(),
        VersionSignal::Page,
        Arc::new(AtomicBool::new(false)),
    );
    futures_util::pin_mut!(events);
    assert!(events.next().await.unwrap().is_ok());
    assert!(events.next().await.is_none());
}
#[tokio::test]
async fn axum_wire_disconnects_between_each_pair_replay_targets_before_acknowledging() {
    use axum::{body::Body, http::Request};
    use sluice_store::{ReadPool, Writer};
    use sluice_web::views::{DashboardState, EmptyCatalog, dashboard_router};
    use tower::ServiceExt;
    let home = tempfile::tempdir().unwrap();
    let _writer = Writer::open(home.path()).unwrap();
    let router = dashboard_router(DashboardState::new(
        ReadPool::open(home.path(), 1).unwrap(),
        Arc::new(EmptyCatalog),
    ));
    for split in 1..3 {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/stream?datastar=%7B%22ver%22%3A%22old%22%7D")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let mut body = response.into_body().into_data_stream();
        for _ in 0..split {
            let chunk = body.next().await.unwrap().unwrap();
            let wire = String::from_utf8(chunk.to_vec()).unwrap();
            assert!(wire.contains("datastar-patch-elements"));
            assert!(!wire.contains("datastar-patch-signals"));
        }
        drop(body);
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/stream?datastar=%7B%22ver%22%3A%22old%22%7D")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let mut body = response.into_body().into_data_stream();
        let mut wires = vec![];
        for _ in 0..3 {
            wires.push(String::from_utf8(body.next().await.unwrap().unwrap().to_vec()).unwrap());
        }
        assert!(wires[0].contains("selector #projects"));
        assert!(wires[1].contains("selector #top-nav"));
        assert!(wires[2].contains("datastar-patch-signals"));
        let signals = wires[2]
            .lines()
            .find_map(|l| l.strip_prefix("data: signals "))
            .unwrap();
        let applied: String = serde_json::from_str::<serde_json::Value>(signals).unwrap()["ver"]
            .as_str()
            .unwrap()
            .into();
        assert_ne!(applied, "old");
        drop(body);
    }
}
#[test]
fn a_ticking_times_text_is_the_clocks_not_part_of_the_version() {
    #[derive(Template)]
    #[template(
        source = "<a class=\"node\">work<time data-since=\"{{ since }}\" class=\"took live\"><span class=\"tk\">{{ shown }}</span><span class=\"vh\">, for {{ shown }}</span></time> after</a>",
        ext = "html"
    )]
    struct Card<'a> {
        since: &'a str,
        shown: &'a str,
    }
    let version = |since: &str, shown: &str| {
        RenderedBatch::new(vec![PatchRegion::new(
            "board",
            TrustedHtml::from_template(&Card { since, shown }).unwrap(),
        )])
        .version
    };
    let start = version("2026-10-05T09:00:00Z", "2h 14m");
    // the clock moved: the same page
    assert_eq!(start, version("2026-10-05T09:00:00Z", "2h 15m"));
    // a new run started: a new page
    assert_ne!(start, version("2026-10-05T11:00:00Z", "<1s"));
}
