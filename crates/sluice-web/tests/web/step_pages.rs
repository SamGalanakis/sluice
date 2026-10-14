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
            .eval("(() => { const b = document.querySelector('.page-top #step-band'); return [b.querySelector('h1#d-title').textContent, b.querySelector('[aria-current=step] .sc-name').textContent, [...b.querySelectorAll('.d-actions :is(button, summary)')].filter(x => x.checkVisibility()).map(x => x.textContent.trim())]; })()")
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
                .wait("document.readyState === 'complete' && !!document.querySelector('.swell-ask sluice-answer')")
                .unwrap();
            browser.eval(FRAMES).unwrap();
            let card: Value = browser
                .eval("(() => { const q = document.querySelector('.swell-ask'), s = q.closest('.sheet').getBoundingClientRect(), r = q.getBoundingClientRect(); const buttons = [...q.querySelectorAll('sluice-answer button.q-toggle, form.q-close button')].map(b => b.getBoundingClientRect().height); return [Math.round(r.width) === Math.round(s.width), buttons.length >= 2, buttons.every(h => h >= 44)]; })()")
                .unwrap();
            assert_eq!(card, json!([true, true, true]), "{url}");
        }

        // the swelled question is answered in place, the line saying so focused
        browser.viewport(1440, "light").unwrap();
        browser.navigate(&asking).unwrap();
        browser
            .wait("document.readyState === 'complete' && !!document.querySelector('#tp-overview .swell-ask sluice-answer .answer[data-drawn]')")
            .unwrap();
        // it swells first, above Now
        assert_eq!(
            browser
                .eval("(() => { const a = document.querySelector('#tp-overview .d-ask'), n = document.querySelector('#tp-overview .d-now'); return a.classList.contains('swell-ask') && !!(a.compareDocumentPosition(n) & Node.DOCUMENT_POSITION_FOLLOWING); })()")
                .unwrap(),
            json!(true)
        );
        browser
            .eval("document.querySelector('#tp-overview .swell-ask sluice-answer button.q-toggle').click()")
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
