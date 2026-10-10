//! A step's page, a unit's page, the inbox, Questions and a thread on the neutral fixture
//! (`tests/neutral`, a field guide's `almanac`): a step's band carries its name, state, its
//! unit's stages with its own ringed and its actions; a unit's band draws its stage strip from
//! its recipe (two recipes and a unit of none); in Chromium, the question to the owner swells
//! first on Overview and is answered in place, Overview stands two to a row at 2560, a phone's
//! question is a whole-width card with 44px buttons, and no page scrolls sideways from 320 to
//! 3840px in Sluice Light or Dark. `SLUICE_STEP_SCREENS=<dir>` also saves each page at each width
//! and theme there.

mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
mod neutral;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::{Value, json};

/// The stage names of the strip labelled `Stages of <unit>` in `html`, in order, and which of
/// them is ringed as the page's own.
fn strip(html: &str, unit: &str) -> (Vec<String>, Option<String>) {
    let at = html
        .find(&format!("aria-label=\"Stages of {unit}\">"))
        .unwrap_or_else(|| panic!("a strip of {unit}: {html}"));
    let list = &html[at..at + html[at..].find("</ol>").unwrap()];
    let mut names = vec![];
    let mut here = None;
    for cell in list.split("<li ").skip(1) {
        let name = cell.split("<span class=\"sc-name\">").nth(1).unwrap();
        let name = name[..name.find('<').unwrap()].to_owned();
        if cell.starts_with("aria-current=\"step\"") {
            here = Some(name.clone());
        }
        names.push(name);
    }
    (names, here)
}

#[tokio::test]
async fn a_steps_band_carries_its_name_state_stages_and_actions_and_a_units_band_its_recipes_stages()
 {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let p = n.almanac;

    // a step's own page: its head is the frame's, on the paper under the tab row, before <main>
    let (status, html) = f.get(&format!("/projects/id/{p}/steps/a-6-review")).await;
    assert_eq!(status, 200);
    let head = html
        .find("<div class=\"subnav\">")
        .expect("the frame's row");
    let band = html
        .find("<header id=\"step-band\" class=\"d-head step-band\" data-step=\"a-6-review\">")
        .expect("the step's band");
    let end = band + html[band..].find("</header>").unwrap();
    let main = html.find("<main").unwrap();
    assert!(head < band && end < main, "{html}");
    let band_html = &html[band..end];
    // its name at a reading size (its stage muted before its title), its Details' "⋯" with
    // its id, its state
    assert!(band_html.contains("<h1 id=\"d-title\" class=\"sb-t\"><span class=\"d-stage\">review ·</span> Terns: the spring guide&#39;s entries</h1><sluice-menu><details class=\"dm\""), "{band_html}");
    assert!(
        band_html.contains("<dt>Step</dt>") && band_html.contains("Copy step"),
        "{band_html}"
    );
    assert!(
        band_html.contains("<dt>Recipe</dt><dd><span>article</span></dd>"),
        "{band_html}"
    );
    assert!(band_html.contains("<p class=\"d-badges\">"), "{band_html}");
    // its unit's stages from the article recipe, its own ringed, the unit linked in its crumbs
    assert!(
        band_html.contains("<nav class=\"crumbs\" aria-label=\"Breadcrumb\">"),
        "{band_html}"
    );
    assert_eq!(
        strip(band_html, "its unit"),
        (
            neutral::ARTICLE.map(String::from).to_vec(),
            Some("review".to_owned())
        )
    );
    // its actions in the band, and none on the paper
    assert!(
        band_html.contains("<div class=\"d-actions\">"),
        "{band_html}"
    );
    assert!(
        !html[main..].contains("<div class=\"d-actions\">"),
        "{html}"
    );

    // a unit of one step draws no strip of its own in the step's band
    let (_, scan) = f.get(&format!("/projects/id/{p}/steps/s-3-scan")).await;
    let scan_band = &scan[scan.find("id=\"step-band\"").unwrap()..];
    let scan_band = &scan_band[..scan_band.find("</header>").unwrap()];
    assert!(!scan_band.contains("class=\"sb-lane\""), "{scan_band}");

    // a unit's band: its stage strip from its recipe, in the recipe's order
    for (unit, recipe, stages) in [
        ("a-6", Some("article"), &["draft", "review", "publish"][..]),
        ("s-3", Some("scan"), &["scan"][..]),
        ("index", None, &["birds", "places", "merge"][..]),
    ] {
        let (status, html) = f.get(&format!("/projects/id/{p}/units/{unit}")).await;
        assert_eq!(status, 200, "{unit}");
        let at = html
            .find("<div id=\"unit-band\" class=\"unit-band\">")
            .unwrap();
        let main = html.find("<main").unwrap();
        assert!(at < main, "{unit}: the band is the frame's");
        let band = &html[at..main];
        assert!(band.contains("<h1 class=\"unit-h"), "{band}");
        // its recipe is its Details', not its line's
        match recipe {
            Some(r) => assert!(
                band.contains(&format!("<dt>Recipe</dt><dd><span>{r}</span></dd>")),
                "{band}"
            ),
            None => assert!(!band.contains("<dt>Recipe</dt>"), "{band}"),
        }
        let (names, here) = strip(band, "the unit");
        assert_eq!(names, stages, "{unit}");
        assert_eq!(here, None, "{unit}: a unit's own strip rings no stage");
        // its steps as modules on the grid, each headed by its stage, its id in its Details;
        // the unit's title is in the band and never again in a module
        let detail = &html[main..];
        for stage in stages {
            let id = format!("{unit}-{stage}");
            assert!(html.contains(&format!("id=\"us-{id}\"")), "{unit}: {id}");
            assert!(
                detail.contains(&format!(
                    "<h3 class=\"mod-t\" id=\"us-{id}\"><a href=\"/projects/id/{p}/steps/{id}\">{stage}</a></h3><sluice-menu><details class=\"dm\""
                )),
                "{unit}: {id} {detail}"
            );
        }
        if unit == "a-6" {
            let title = "Terns: the spring guide&#39;s entries";
            assert!(band.contains(title), "{band}");
            let steps = &detail[detail.find("id=\"unit-steps\"").unwrap()..];
            let steps = &steps[..steps.find("unit-tl").unwrap_or(steps.len())];
            assert!(!steps.contains(title), "{steps}");
        }
    }
}

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
async fn chromium_the_pages_fit_every_width_overview_stands_two_up_and_the_question_is_answered_in_place()
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
        for width in [320, 390, 768, 1024, 1440, 1920, 2560, 3840] {
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

        // Overview at 2560: two to a row, its columns side by side from the top
        browser.viewport(2560, "dark").unwrap();
        browser.navigate(&failed).unwrap();
        browser.wait("document.querySelector('#tp-overview .d-failure')").unwrap();
        browser.eval(FRAMES).unwrap();
        let cols: Value = browser
            .eval("(() => { const c = [...document.querySelectorAll('#tp-overview .ov > .sheet > .mod-col')].map(e => e.getBoundingClientRect()); return [c.length, Math.round(c[0].top) === Math.round(c[1].top), c[0].right < c[1].left]; })()")
            .unwrap();
        assert_eq!(cols, json!([2, true, true]));
        // and with no side, the first column's own modules two to a row: the question, then Now
        browser.navigate(&asking).unwrap();
        browser.wait("document.querySelector('#tp-overview .d-ask')").unwrap();
        browser.eval(FRAMES).unwrap();
        let row: Value = browser
            .eval("(() => { const a = document.querySelector('#tp-overview .d-ask').getBoundingClientRect(), n = document.querySelector('#tp-overview .d-now').getBoundingClientRect(); return [Math.round(a.top) === Math.round(n.top), a.right < n.left]; })()")
            .unwrap();
        assert_eq!(row, json!([true, true]));

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
