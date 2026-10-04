//! The browser consumes the actual Rust SDK wire events with Rocket's vendored runtime.
use super::chrome::Chrome;
use askama::Template;
use axum::{
    response::{Html, Sse},
    routing::get,
};
use futures_util::stream;
use serde_json::json;
use sluice_web::{
    streams::{Comparison, PatchRegion, RenderedBatch, VersionSignal},
    views::TrustedHtml,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Template)]
#[template(
    source = "<section id=\"{{ id }}\"><span>{{ text }}</span>{% if id == \"one\" %}<input id=\"draft\" data-ignore-morph value=\"original\">{% endif %}</section>",
    ext = "html"
)]
struct Region<'a> {
    id: &'a str,
    text: &'a str,
}
fn batch() -> RenderedBatch {
    RenderedBatch {
        version: "complete".into(),
        regions: ["one", "two", "three"]
            .into_iter()
            .map(|id| {
                PatchRegion::new(
                    id,
                    TrustedHtml::from_template(&Region {
                        id,
                        text: "morphed",
                    })
                    .unwrap(),
                )
            })
            .collect(),
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn browser_replays_after_every_event_boundary_and_preserves_focus() {
    // Split after each HTML event. The browser must reconnect using its last
    // acknowledged version and apply every stable target before the final marker.
    for split in 1..4 {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let requests = count.clone();
        let versions = seen.clone();
        let app = axum::Router::new()
            .route("/", get(|| async { Html(r#"<!doctype html><html><head><script type="module" data-datastar-runtime src="/static/datastar-rocket.js"></script></head><body data-signals="{ver:'old',stale:true}"><main id="probe" data-init="@get('/probe', {retry:'always',retryInterval:1500})"><section id="one">old</section><section id="two">old</section><section id="three">old</section><output data-text="$ver" id="version"></output></main></body></html>"#) }))
            .route("/probe", get(move |axum::extract::Query(query): axum::extract::Query<sluice_web::streams::StreamQuery>| {
                let requests = requests.clone(); let versions = versions.clone();
                async move {
                    let applied = query.version(VersionSignal::Page);
                    versions.lock().unwrap().push(applied.clone());
                    let turn = requests.fetch_add(1, Ordering::SeqCst);
                    let events = Comparison::new(applied, VersionSignal::Page).events(batch());
                    let wire: Vec<_> = events.into_iter().take(if turn == 0 {split} else {4}).map(|event| Ok::<_,std::convert::Infallible>(event.axum_event())).collect();
                    Sse::new(stream::iter(wire))
                }
            }))
            .route("/static/datastar-rocket.js", get(|| async { ([ ("content-type", "text/javascript") ], include_str!("../../assets/datastar-rocket-1.0.4.js")) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        assert_ne!(address.port(), 3065);
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let versions = seen.clone();
        tokio::task::spawn_blocking(move || {
            let mut browser = Chrome::open(&format!("http://{address}/")).unwrap();
            browser.wait("document.querySelector('#one span')?.textContent==='morphed'").unwrap();
            assert_eq!(browser.eval("document.querySelector('#version').textContent").unwrap(),json!("old"));
            browser.eval("document.querySelector('#draft').focus();document.querySelector('#draft').value='retained draft'").unwrap();
            browser.wait("document.querySelector('#version').textContent==='complete'").unwrap();
            assert_eq!(browser.eval("['one','two','three'].every(id=>document.querySelector('#'+id+' span').textContent==='morphed')").unwrap(),json!(true));
            assert_eq!(browser.eval("document.querySelector('#draft').value").unwrap(),json!("retained draft"));
            assert_eq!(browser.eval("document.activeElement.id").unwrap(),json!("draft"));
            assert_eq!(versions.lock().unwrap()[..2], ["old","old"]);
            assert_eq!(browser.eval("browserErrors").unwrap(),json!([]));
        }).await.unwrap();
        task.abort();
        let _ = task.await;
        assert!(count.load(Ordering::SeqCst) >= 2);
    }
}

fn listeners(browser: &mut Chrome, selector: &str, types: &[&str]) -> serde_json::Value {
    browser.send("Runtime.evaluate",json!({"expression":format!("Object.fromEntries({}.map(t=>[t,(getEventListeners(document.querySelector({}))[t]||[]).length]))",json!(types),json!(selector)),"includeCommandLineAPI":true,"returnByValue":true})).unwrap()["result"]["value"].clone()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "G6 native component reconnect, keyboard navigation and drawer cancellation"]
async fn native_components_reconnect_without_duplicate_listeners() {
    let home = tempfile::tempdir().unwrap();
    let writer = sluice_store::Writer::open(home.path()).unwrap();
    let (_, board, _) = super::visual::seed(&writer).await;
    writer.shutdown().await.unwrap();
    let mut scratch = super::clients::Scratch::new(home);
    scratch.boot().await;
    let url = scratch.url.clone();
    tokio::task::spawn_blocking(move || {
        let mut browser=Chrome::open(&format!("{url}/projects/id/{board}")).unwrap();
        browser.viewport(1440,"light").unwrap();
        browser.wait("!!window.sluiceStream && document.querySelectorAll('.wires path').length>0").unwrap();
        let types=["pointerover","pointerout","focusin","focusout","keydown","toggle"];
        let expected=listeners(&mut browser,"sluice-board",&types);
        assert!(expected.as_object().unwrap().values().all(|n|n==&json!(1)),"{expected}");
        let scroll=listeners(&mut browser,"#drawer",&["scroll"]);
        for _ in 0..3 {
            browser.eval("(()=>{window.controller=sluiceStream();const board=document.querySelector('sluice-board'),parent=board.parentNode;board.remove();parent.append(board);window.host=document.querySelector('sluice-drawer');window.drawerParent=host.parentNode;window.after=host.nextSibling;host.remove()})()").unwrap();
            assert_eq!(browser.eval("controller.signal.aborted").unwrap(),json!(true));
            browser.eval("window.drawerParent.insertBefore(host,window.after)").unwrap();
            assert_eq!(listeners(&mut browser,"sluice-board",&types),expected);
            assert_eq!(listeners(&mut browser,"#drawer",&["scroll"]),scroll);
        }
        browser.eval("document.querySelector('[data-step=source]').focus()").unwrap();
        browser.wait("document.querySelector('.plane').classList.contains('tracing')").unwrap();
        browser.send("Input.dispatchKeyEvent",json!({"type":"keyDown","key":"ArrowDown","code":"ArrowDown"})).unwrap();
        assert_ne!(browser.eval("document.activeElement.dataset.step").unwrap(),json!("source"));
        browser.eval("window.late=0;document.querySelector('#drawer').focus=()=>late++;document.querySelector('[data-step=failed]').scrollIntoView=()=>late++;history.replaceState(null,'','#step:failed');dispatchEvent(new HashChangeEvent('hashchange'));host.remove()").unwrap();
        browser.eval("new Promise(r=>setTimeout(r,350))").unwrap();
        assert_eq!(browser.eval("late").unwrap(),json!(0));
        assert_eq!(browser.eval("document.documentElement.classList.contains('drawer-open')").unwrap(),json!(false));
        browser.eval("window.drawerParent.insertBefore(host,window.after);history.replaceState(null,'',location.pathname);document.querySelector('nav.top a[href*=fns]').click()").unwrap();
        browser.wait("location.pathname==='/fns' && document.querySelector('main')").unwrap();
        assert_eq!(browser.eval("browserErrors").unwrap(),json!([]));
    }).await.unwrap();
}
