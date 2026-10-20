//! A step's page, a unit's page, the inbox, Questions and a thread on the neutral fixture
//! (`tests/neutral`, a field guide's `almanac`): a step's band carries its name, state, its
//! unit's stages with its own ringed and its actions; a unit's band draws its stage strip from
//! its recipe (two recipes and a unit of none); in Chromium, the question to the owner swells
//! first on Overview and is answered in place, Overview stands two to a row at 2560, a phone's
//! question is a whole-width card with 44px buttons, and no page scrolls sideways from 320 to
//! 3840px in Sluice Light or Dark. `SLUICE_STEP_SCREENS=<dir>` also saves each page at each width
//! and theme there.

use crate::board_fixture;
use crate::chrome;
use crate::neutral;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::{Value, json};

fn serve(f: &Fixture) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let router = f.router();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    (
        addr,
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    )
}
const FRAMES: &str = "new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))";
const FITS: &str = "document.documentElement.scrollWidth <= document.documentElement.clientWidth";

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_the_pages_fit_every_width_overview_stands_two_up_and_the_question_is_answered_in_place()
 {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let (addr, server) = serve(&f);
    let base = format!("http://{addr}");
    tokio::task::spawn_blocking(move || {
        let p = n.almanac;
        let asking = format!("{base}/projects/id/{p}/steps/a-7-draft");
        let failed = format!("{base}/projects/id/{p}/steps/a-4-review");
        let pages = [
            ("step-asking", asking.clone()),
            ("step-failed", failed.clone()),
            ("unit-article", format!("{base}/projects/id/{p}/units/a-6")),
            ("unit-scan", format!("{base}/projects/id/{p}/units/s-3")),
            ("unit-index", format!("{base}/projects/id/{p}/units/index")),
            ("inbox", format!("{base}/inbox")),
            ("questions", format!("{base}/questions")),
            (
                "thread",
                format!("{base}/projects/id/{p}/thread?thread=step-a-7-draft"),
            ),
        ];
        let screens = std::env::var_os("SLUICE_STEP_SCREENS").map(std::path::PathBuf::from);
        let mut browser = Chrome::open(&asking).unwrap();

        // no page scrolls sideways, at any width, light or dark
        for width in [390, 1440, 2560] {
            for theme in ["light", "dark"] {
                for (name, url) in &pages {
                    browser.viewport(width, theme).unwrap();
                    browser.navigate(url).unwrap();
                    browser.wait("document.readyState === 'complete'").unwrap();
                    browser.eval("document.fonts.ready").unwrap();
                    browser.eval(FRAMES).unwrap();
                    assert_eq!(
                        browser.eval(FITS).unwrap(),
                        json!(true),
                        "{name} at {width} {theme} scrolls sideways"
                    );
                    if let Some(dir) = &screens {
                        std::fs::create_dir_all(dir).unwrap();
                        browser
                            .screenshot(&dir.join(format!("{name}-{width}-{theme}.png")))
                            .unwrap();
                    }
                }
            }
        }

        // the band: the step's name in the frame's head, its own stage ringed, its actions
        browser.viewport(1440, "light").unwrap();
        browser.navigate(&failed).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        let band: Value = browser
            .eval("(() => { const b = document.querySelector('.page-top #step-band'); return [b.querySelector('h1#d-title').textContent, b.querySelector('[aria-current=step] .sc-name').textContent, [...b.querySelectorAll('.sh-acts :is(button, summary)')].filter(x => x.checkVisibility()).map(x => x.textContent.trim())]; })()")
            .unwrap();
        // Retry with feedback, as on the plan's Stopped cards, its box folded; Retry beside it
        assert_eq!(
            band,
            json!(["review · Waders: the autumn guide's entries", "review", ["Retry with feedback", "Retry"]])
        );

        // Overview at 2560, in the frame's centred column: its two columns side by side from
        // the top, seven and five twelfths, the sheet centred under the nav
        browser.viewport(2560, "dark").unwrap();
        browser.navigate(&failed).unwrap();
        browser.wait("document.querySelector('#tp-overview .d-failure')").unwrap();
        browser.eval(FRAMES).unwrap();
        let cols: Value = browser
            .eval("(() => { const c = [...document.querySelectorAll('#tp-overview .ov > .sheet > .mod-col')].map(e => e.getBoundingClientRect()), s = document.querySelector('#tp-overview .ov > .sheet').getBoundingClientRect(); return [c.length, Math.round(c[0].top) === Math.round(c[1].top), c[0].right < c[1].left, c[0].width > c[1].width, s.width <= 1440.5, Math.abs(s.left - (innerWidth - s.right)) < 24]; })()")
            .unwrap();
        assert_eq!(cols, json!([2, true, true, true, true, true]));

        // a phone: the question first, a whole-width card, its Answer and Close 44px tall
        for url in [&asking, &format!("{base}/inbox")] {
            browser.viewport(390, "light").unwrap();
            browser.navigate(url).unwrap();
            browser
                .wait("document.readyState === 'complete' && !!document.querySelector('.q-card sluice-answer')")
                .unwrap();
            browser.eval(FRAMES).unwrap();
            let card: Value = browser
                .eval("(() => { const q = document.querySelector('.q-card'), s = (q.closest('.sheet') ?? q.parentElement).getBoundingClientRect(), r = q.getBoundingClientRect(); const buttons = [...q.querySelectorAll('sluice-answer button.q-toggle, form.q-close button')].map(b => b.getBoundingClientRect().height); return [Math.round(r.width) === Math.round(s.width), buttons.length >= 2, buttons.every(h => h >= 44)]; })()")
                .unwrap();
            assert_eq!(card, json!([true, true, true]), "{url}");
        }

        // the swelled question is answered in place, the line saying so focused
        browser.viewport(1440, "light").unwrap();
        browser.navigate(&asking).unwrap();
        browser
            .wait("document.readyState === 'complete' && !!document.querySelector('#tp-overview .q-card sluice-answer .answer[data-drawn]')")
            .unwrap();
        // it swells first: Overview's first module
        assert_eq!(
            browser
                .eval("(() => { const a = document.querySelector('#tp-overview .d-ask'); return !!a.querySelector(':scope > .q-card') && a === document.querySelector('#tp-overview .mod'); })()")
                .unwrap(),
            json!(true)
        );
        browser
            .eval("document.querySelector('#tp-overview .q-card sluice-answer button.q-toggle').click()")
            .unwrap();
        browser
            .wait("document.activeElement?.id?.startsWith('ov-reply-')")
            .unwrap();
        browser
            .eval("(t => { t.value = 'Take the checklist dates.'; t.form.requestSubmit(); })(document.activeElement)")
            .unwrap();
        browser
            .wait("document.activeElement?.matches('#tp-overview .q-answered[role=status]')")
            .unwrap();
        let said = browser
            .eval("document.querySelector('#tp-overview .q-answered').textContent")
            .unwrap();
        assert!(
            said.as_str().unwrap().starts_with("Answered just now: ")
                && said.as_str().unwrap().ends_with(" · Read the thread"),
            "{said}"
        );
        assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

/// A completed run must retain its actual budget and peak even when project defaults change;
/// the shared failure summary must identify memory exhaustion on the owning unit too.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_run_budgets_and_resource_failures_fit_step_and_unit_pages() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let project = n.almanac;
    f.writer
        .write(sluice_store::RetrySafety::NonIdempotent, move |tx| {
            let limits = json!({"memory_bytes": 8_u64 << 30, "cpu_millis": 1500, "tasks": 256});
            let usage = json!({"memory_peak_bytes": 8_u64 << 30, "oom_kills": 1,
                "pids_limit_hits": 0, "cpu_throttled_usec": 1200});
            let error = json!({"error": "resource_exhausted", "resource": "memory",
                "limit": 8_u64 << 30, "peak": 8_u64 << 30,
                "message": "Memory limit exceeded: 8589934592 byte budget; payload stopped"});
            tx.sql().execute(
                "UPDATE projects SET limits=?2 WHERE project_id=?1",
                (project.to_string(), json!({"memory":"512MiB","cpus":0.25,"tasks":64}).to_string()),
            )?;
            let changed = tx.sql().execute(
                "UPDATE attempts SET limits=?2,request=json_set(request,'$.limits',json(?2),'$.resource_usage',json(?3))
                 WHERE project_id=?1 AND step_id='a-4-review'",
                (project.to_string(), limits.to_string(), usage.to_string()),
            )?;
            assert_eq!(changed, 1, "the fixture has one failed review attempt");
            tx.sql().execute(
                "UPDATE runs SET result=json_set(result,'$.error',json(?2))
                 WHERE project_id=?1 AND step_id='a-4-review'",
                (project.to_string(), error.to_string()),
            )?;
            tx.sql().execute(
                "UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='a-4-review'",
                (project.to_string(), error.to_string()),
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    let (addr, server) = serve(&f);
    tokio::task::spawn_blocking(move || {
        let base = format!("http://{addr}/projects/id/{project}");
        let pages = [
            ("step-runs", format!("{base}/steps/a-4-review")),
            ("unit", format!("{base}/units/a-4")),
        ];
        let screens = std::env::var_os("SLUICE_BUDGET_SCREENS").map(std::path::PathBuf::from);
        let mut browser = Chrome::open(&pages[0].1).unwrap();
        for width in [390, 1440, 2560] {
            for theme in ["light", "dark"] {
                for (name, url) in &pages {
                    browser.viewport(width, theme).unwrap();
                    browser.navigate(url).unwrap();
                    browser.wait("document.readyState === 'complete'").unwrap();
                    browser.viewport(width, theme).unwrap();
                    browser.eval(&format!("document.querySelector('input[name=theme][value=sluice-{theme}]').click()")).unwrap();
                    browser.wait(&format!("document.documentElement.dataset.theme === 'sluice-{theme}' && getComputedStyle(document.documentElement).colorScheme === '{theme}'")).unwrap();
                    browser.eval("document.fonts.ready").unwrap();
                    if *name == "step-runs" {
                        browser
                            .wait("document.querySelector('[role=tab][data-tab=runs]') && customElements.get('sluice-tabs')")
                            .unwrap();
                        browser
                            .eval("document.querySelector('[role=tab][data-tab=runs]').click()")
                            .unwrap();
                        browser
                            .wait("document.querySelector('#tp-runs')?.checkVisibility()")
                            .unwrap();
                        let facts = browser.eval(r#"(() => {
                            const rows = [...document.querySelectorAll('#tp-runs .a-facts > div')];
                            return ['Budget', 'Usage'].map(label => {
                                const row = rows.find(r => r.querySelector('dt')?.textContent === label);
                                const value = row?.querySelector('dd');
                                return value?.checkVisibility() ? value.textContent.trim() : null;
                            });
                        })()"#).unwrap();
                        assert_eq!(facts, json!(["8192 MiB memory cap · 1.5 CPUs · 256 tasks", "8192 MiB peak memory"]), "{width} {theme}");
                        assert_eq!(browser.eval("[...document.querySelectorAll('#tp-runs .a-facts dd')].every(e => e.scrollWidth <= e.clientWidth + 1)").unwrap(), json!(true), "run facts clipped at {width} {theme}");
                    } else {
                        browser
                            .wait("document.querySelector('#us-a-4-review')")
                            .unwrap();
                        assert_eq!(browser.eval("document.querySelector('#us-a-4-review').closest('.u-step').querySelector('.u-why').innerText").unwrap(), json!("Stopped: it was killed at its memory cap."), "{width} {theme}");
                    }
                    browser.eval(FRAMES).unwrap();
                    assert_eq!(browser.eval(FITS).unwrap(), json!(true), "{name} at {width} {theme} scrolls sideways");
                    let alignment = browser.eval(r#"(() => {
                        const head = document.querySelector('.page-top');
                        const rect = head.getBoundingClientRect();
                        const nav = document.querySelector('#top-nav .brand').getBoundingClientRect();
                        const contentLeft = rect.left + parseFloat(getComputedStyle(head).paddingLeft);
                        return [Math.abs(rect.left - (document.documentElement.clientWidth - rect.right)) <= 1,
                                Math.abs(contentLeft - nav.left) <= 1];
                    })()"#).unwrap();
                    assert_eq!(alignment, json!([true, true]), "{name} column at {width} {theme}");
                    assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
                    assert_eq!(browser.eval("getComputedStyle(document.documentElement).colorScheme").unwrap(), json!(theme));
                    if let Some(dir) = &screens {
                        std::fs::create_dir_all(dir).unwrap();
                        browser.screenshot(&dir.join(format!("budget-{name}-{width}-{theme}.png"))).unwrap();
                    }
                }
            }
        }
    })
    .await
    .unwrap();
    server.abort();
}
