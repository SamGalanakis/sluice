//! The Synthesis foundation: the frame (the navy band, the paper row under it), the kit's
//! primitives over HTTP and in the gallery, the neutral fixture on every page, and in a real
//! Chromium the show-grid switch, select-to-trace (select, keyboard, clear, the fades and the
//! chain's sentence) and the band stacked on a phone. `SLUICE_SYNTH_SCREENS=<dir>` also saves
//! the frame on the plan and home and the gallery at 390, 1440 and 2560 in both themes.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
mod neutral;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::{Value, json};
use sluice_web::views::ui::{self, Shown, Stage};

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

#[test]
fn every_cell_of_a_strip_says_its_state_in_words_and_shape() {
    let stages = [
        Stage::new("draft", Some(Shown::Succeeded)).took(1_500.0),
        Stage::new("review", Some(Shown::Running))
            .running_since("2026-10-10T00:00:00Z", 4_000.0)
            .over(2.14),
        Stage::new("publish", None),
        Stage::new("check", Some(Shown::Failed)).took(60.0),
        Stage::new("hold", Some(Shown::Paused)),
        Stage::gap("then the rest", 2),
    ];
    let html = ui::stage_strip("Stages of a-1", &stages);
    let html = html.as_str();
    assert!(
        html.starts_with("<ol class=\"strip\" style=\"--n:7\" aria-label=\"Stages of a-1\">"),
        "{html}"
    );
    assert!(
        html.contains("<li class=\"sc sc-done\" data-state=\"succeeded\">"),
        "{html}"
    );
    assert!(
        html.contains("<span class=\"vh\">: succeeded</span>"),
        "{html}"
    );
    assert!(
        html.contains("<li class=\"sc sc-run\" data-state=\"running\">"),
        "{html}"
    );
    assert!(
        html.contains("<span class=\"sc-over\" aria-hidden=\"true\">2.1×</span>"),
        "{html}"
    );
    assert!(html.contains(": running, 2.1× its usual time"), "{html}");
    assert!(
        html.contains("<time data-since=\"2026-10-10T00:00:00Z\""),
        "{html}"
    );
    assert!(html.contains("class=\"sweep\""), "{html}");
    assert!(html.contains("<li class=\"sc sc-empty\"><span class=\"sc-name\">publish</span><span class=\"vh\">: not reached</span></li>"), "{html}");
    assert!(
        html.contains("<li class=\"sc sc-look\" data-state=\"failed\">"),
        "{html}"
    );
    // a waiting state that says itself writes its word in its outline
    assert!(html.contains("<li class=\"sc sc-empty\" data-state=\"paused\"><span class=\"sc-name\">hold</span><span class=\"sc-word\" aria-hidden=\"true\">"), "{html}");
    assert!(
        html.contains("<li class=\"sc sc-gap\" style=\"--gap:2\"><span>then the rest</span></li>"),
        "{html}"
    );
    assert_eq!(ui::overrun(2.14).as_str().matches("2.1× usual").count(), 1);
    assert_eq!(ui::ratio_text(12.4), "12×");
    let marks = ui::stage_marks("Stages of a-1", &stages[..3]);
    assert!(
        marks.as_str().contains(
            "aria-label=\"Stages of a-1: draft succeeded, review running, publish not reached\""
        ),
        "{}",
        marks.as_str()
    );
}

#[test]
fn the_frame_and_band_primitives_escape_what_they_are_given() {
    let head = ui::band_head(
        "a<b>",
        &sluice_web::views::TrustedHtml::default(),
        &ui::summary_sentence(&ui::Summary::default()),
    );
    assert_eq!(
        head.as_str(),
        "<div class=\"band-head\"><h1>a&lt;b&gt;</h1><div class=\"band-sub\"><p class=\"band-summary\">Nothing has started yet.</p></div></div>"
    );
    // a long name takes a size down
    assert!(
        ui::band_head(
            "a-project-with-a-long-name",
            &Default::default(),
            &Default::default()
        )
        .as_str()
        .contains("<h1 class=\"longest\">")
    );
    let strip = ui::recent_strip(
        "rf",
        "Newest first.",
        &[ui::Finished {
            name: "a-<1>".into(),
            title: "Gulls".into(),
            href: "/u/a-1".into(),
            at: "2026-10-09T17:49:00Z".into(),
            took: 3_840.0,
            runs: 2,
            shown: Some(Shown::Succeeded),
        }],
    );
    let strip = strip.as_str();
    assert!(
        strip.contains("<h2 id=\"rf\">Recently finished</h2><p>Newest first.</p>"),
        "{strip}"
    );
    assert!(strip.contains("<time data-clock datetime=\"2026-10-09T17:49:00Z\" title=\"2026-10-09 17:49 UTC\">17:49</time>"), "{strip}");
    assert!(
        strip.contains("<span class=\"rf-name\">a-&lt;1&gt;</span>"),
        "{strip}"
    );
    assert!(strip.contains("took 1h 4m · 2 runs"), "{strip}");
    assert!(ui::recent_strip("rf", "", &[]).as_str().is_empty());
    let margin = ui::margin_module(&ui::LongRun {
        name: "survey".into(),
        fields: vec![
            ("checked".into(), json!(1240)),
            ("note".into(), json!("<b>")),
        ],
        since: "2026-10-08T00:00:00Z".into(),
        ..Default::default()
    });
    let margin = margin.as_str();
    assert!(margin.starts_with("<article class=\"mod swell-margin\" style=\"--span:2\" data-span=\"2\" aria-label=\"survey, running\">"), "{margin}");
    assert!(margin.contains("<div class=\"mm-f mm-lead\"><dt>checked</dt><dd>1240</dd></div><div class=\"mm-f\"><dt>note</dt><dd>&lt;b&gt;</dd></div>"), "{margin}");
    let trace = ui::Trace::new([("b", "a\""), ("c", "b")]);
    assert_eq!(
        trace.attrs("b").as_str(),
        " data-unit=\"b\" data-up=\"a&quot;\" data-down=\"c\" data-chain=\"Tracing b. It waits for a&quot;. c waits on it.\""
    );
    assert_eq!(
        ui::section_head("stopped", "Stopped", "1 failed").as_str(),
        "<div class=\"sec-h\" id=\"stopped\"><h2>Stopped</h2><p class=\"sec-n\">1 failed</p></div>"
    );
    assert!(
        ui::grid_toggle("plan-grid")
            .as_str()
            .contains("aria-controls=\"plan-grid\" aria-pressed=\"false\"")
    );
    assert!(ui::grid_open("plan-grid", 12).as_str().starts_with("<sluice-grid id=\"plan-grid\" class=\"sheet\" style=\"--cols:12\" data-preserve-attr=\"showing\"><div class=\"grid-ovl\" aria-hidden=\"true\"><span class=\"gc\"><span class=\"gc-n\">1</span></span>"));
}

#[tokio::test]
async fn the_frame_draws_the_band_and_the_row_under_it_and_the_gallery_every_primitive() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let (status, home) = f.get("/").await;
    assert_eq!(status, 200);
    assert!(home.contains("<header class=\"site-head\"><div class=\"band-in\"><nav id=\"top-nav\" class=\"top\" aria-label=\"Main\">"), "{home}");
    assert!(home.contains("@fontsource-variable/schibsted-grotesk@5.3.0/index.css"));
    assert!(home.contains("@fontsource-variable/jetbrains-mono@5.3.0/index.css"));
    assert!(!home.contains("archivo") && !home.contains("public-sans"));
    // the sections on the paper under the band; the live line only on a page with a stream
    assert!(home.contains("<div class=\"subnav\"><div class=\"subnav-in\"><nav class=\"links\" aria-label=\"Sections\">"), "{home}");
    assert!(home.contains("<span class=\"live\">Live</span>"), "{home}");
    let (_, plan) = f.get(&format!("/projects/id/{}", n.almanac)).await;
    assert!(
        plan.contains("<a class=\"home\" href=\"/\">Projects</a>"),
        "{plan}"
    );
    assert!(
        plan.contains("<details class=\"switcher current\""),
        "{plan}"
    );
    assert!(
        plan.contains("<nav class=\"links\" aria-label=\"almanac\">"),
        "{plan}"
    );
    assert!(plan.contains(&format!("<a class=\"project-settings\" href=\"/projects/id/{}/settings\" aria-label=\"Project settings\">Settings</a>", n.almanac)), "{plan}");
    // seven themes, each with its swatch, and three appearances
    assert_eq!(plan.matches("name=\"theme\" value=\"").count(), 7, "{plan}");
    assert_eq!(
        plan.matches("<span class=\"swatch\" data-theme=\"").count(),
        7,
        "{plan}"
    );
    assert_eq!(
        plan.matches("name=\"appearance\" value=\"").count(),
        3,
        "{plan}"
    );
    let (status, gallery) = f.get("/_ui").await;
    assert_eq!(status, 200);
    for (part, marks) in [
        (
            "The band",
            &[
                "<div class=\"band-head\"><h1 class=\"long\">almanac</h1>",
                "<section class=\"band-strip\" aria-labelledby=\"l-rf\">",
                "<p class=\"band-summary\">",
            ][..],
        ),
        (
            "Summary sentence",
            &[
                "<a class=\"ask\" href=\"#\">1 question for you</a>. 1 failed, 1 cancelled. 2 article and 1 scan at work: s-3 quiet for 53m, a-12 at 2.1× its usual time. 2 waiting. 3 of 10 units done; the last finished",
            ][..],
        ),
        (
            "Module grid",
            &[
                "<sluice-grid id=\"l-grid\" class=\"sheet\"",
                "aria-controls=\"l-grid\"",
                "<span class=\"gc-n\">12</span>",
            ][..],
        ),
        (
            "Modules",
            &[
                "<article class=\"mod swell-ask\" style=\"--span:6\"",
                "<article class=\"mod swell-look\" style=\"--span:4\"",
            ][..],
        ),
        (
            "Section heads",
            &[
                "<div class=\"sec-h\"><h2>Stopped</h2>",
                "<div class=\"sec-h sec-strip\" style=\"--lead:6;--rest:6;--n:3\">",
            ][..],
        ),
        (
            "Stage strip",
            &[
                "<ol class=\"strip\"",
                "sc sc-done",
                "sc sc-run",
                "sc sc-look sc-quiet",
                "sc sc-gap",
                "class=\"overrun\"",
                "class=\"marks\"",
            ][..],
        ),
        (
            "Margin module",
            &[
                "<article class=\"mod swell-margin\"",
                "<dl class=\"mm-fields\">",
            ][..],
        ),
        (
            "Trace",
            &[
                "<sluice-trace id=\"l-trace\" class=\"trace\" data-preserve-attr=\"selected\">",
                "data-unit=\"a-12\" data-up=\"s-3\" data-down=\"a-13 a-15 a-14\"",
                "class=\"rail\"",
            ][..],
        ),
    ] {
        assert!(gallery.contains(&format!("-h\">{part}</h2>")), "{part}");
        for mark in marks {
            assert!(gallery.contains(mark), "{part}: {mark}");
        }
    }
}

#[tokio::test]
async fn every_page_renders_the_neutral_fixture() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let a = format!("/projects/id/{}", n.almanac);
    let c = format!("/projects/id/{}", n.chores);
    let mut pages = vec![
        "/".to_owned(),
        "/inbox".into(),
        "/log".into(),
        "/fns".into(),
        "/_ui".into(),
        a.clone(),
        format!("{a}?show=all"),
        format!("{a}?recipe=article&show=all"),
        format!("{a}/log"),
        format!("{a}/inbox"),
        format!("{a}/settings"),
        format!("{a}/thread?thread=step-a-7-draft"),
        c.clone(),
        format!("{c}/log"),
    ];
    for step in [
        "a-7-draft",
        "a-6-draft",
        "a-4-review",
        "a-5-draft",
        "s-3-scan",
        "index-merge",
        "survey",
        "a-9-draft",
    ] {
        pages.push(format!("{a}/steps/{step}"));
    }
    for unit in ["a-6", "index"] {
        pages.push(format!("{a}/units/{unit}"));
    }
    for page in &pages {
        let (status, html) = f.get(page).await;
        assert!(status.is_success(), "{page}: {status} {html}");
        assert!(html.contains("<header class=\"site-head\">"), "{page}");
    }
    // the recipes are the project's own: its stages by their names
    let (_, plan) = f.get(&a).await;
    for word in [
        "a-7",
        "s-3",
        "index",
        "survey",
        "Shorebirds: the spring guide",
        "mx-band-running-article",
        "mx-band-running-scan",
    ] {
        assert!(plan.contains(word), "{word}: {plan}");
    }
    let (_, inbox) = f.get("/inbox").await;
    assert!(
        inbox.contains("Use the checklist&#39;s new spring dates in this draft?")
            || inbox.contains("Use the checklist's new spring dates in this draft?"),
        "{inbox}"
    );
}

fn shoot(browser: &mut Chrome, name: &str) {
    if let Some(dir) = std::env::var_os("SLUICE_SYNTH_SCREENS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        browser
            .screenshot(&dir.join(format!("{name}.png")))
            .unwrap();
    }
}
const PAGE: &str = "({scroll: document.documentElement.scrollWidth, width: document.documentElement.clientWidth, errors: window.browserErrors ?? []})";

#[tokio::test(flavor = "multi_thread")]
async fn chromium_the_grid_switch_and_the_trace_and_the_band_on_a_phone() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let (addr, server) = serve(&f);
    let base = format!("http://{addr}");
    tokio::task::spawn_blocking(move || {
        let gallery = format!("{base}/_ui");
        let mut browser = Chrome::open(&gallery).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser.eval("try { localStorage.removeItem('sluice.grid') } catch {}").unwrap();
        browser.navigate(&gallery).unwrap();
        browser.wait("document.readyState === 'complete' && !!customElements.get('sluice-grid') && !!customElements.get('sluice-trace')").unwrap();
        browser.eval(FRAMES).unwrap();

        // ---- the grid switch: the construction shows, each column numbered, each span named
        let grid = "document.querySelector('#l-grid')";
        assert_eq!(browser.eval(&format!("getComputedStyle({grid}.querySelector('.grid-ovl')).display")).unwrap(), "none");
        browser.eval("document.querySelector('button.grid-toggle[aria-controls=\"l-grid\"]').click()").unwrap();
        browser.eval(FRAMES).unwrap();
        let shown = browser.eval(&format!("(g => ({{showing: g.hasAttribute('showing'), pressed: document.querySelector('button.grid-toggle[aria-controls=\"l-grid\"]').getAttribute('aria-pressed'), overlay: getComputedStyle(g.querySelector('.grid-ovl')).display, numbers: [...g.querySelectorAll('.gc')].filter(c => c.checkVisibility()).map(c => c.textContent).join(' '), tag: getComputedStyle(g.querySelector(':scope > .mod')).counterReset, cols: [...g.querySelectorAll('.gc')].filter(c => c.checkVisibility()).length, gap: parseFloat(getComputedStyle(g).columnGap), first: (r => Math.round(r.width))(g.querySelector(':scope > .mod').getBoundingClientRect()), col: (r => Math.round(r.width))(g.querySelector('.gc').getBoundingClientRect())}}))({grid})")).unwrap();
        assert_eq!(shown["showing"], true, "{shown}");
        assert_eq!(shown["pressed"], "true", "{shown}");
        assert_eq!(shown["overlay"], "grid", "{shown}");
        assert_eq!(shown["numbers"], "1 2 3 4 5 6 7 8 9 10 11 12", "{shown}");
        assert_eq!(shown["tag"], "take 6", "{shown}");
        assert_eq!(shown["cols"], 12, "{shown}");
        // a six-column module is six columns and five gaps of the construction
        let (first, col, gap) = (shown["first"].as_f64().unwrap(), shown["col"].as_f64().unwrap(), shown["gap"].as_f64().unwrap());
        assert!((first - (6.0 * col + 5.0 * gap)).abs() <= 2.0, "{shown}");
        // kept in this browser: the next page shows it too, until switched off
        browser.navigate(&gallery).unwrap();
        browser.wait("!!customElements.get('sluice-grid') && document.querySelector('#l-grid').hasAttribute('showing')").unwrap();
        browser.eval("document.querySelector('button.grid-toggle[aria-controls=\"l-grid\"]').click()").unwrap();
        assert_eq!(browser.eval(&format!("{grid}.hasAttribute('showing')")).unwrap(), false);

        // ---- the trace: select, the chain both ways, the fades, the rail and the sentence
        let host = "document.querySelector('#l-trace')";
        let roles = format!("Object.fromEntries([...{host}.querySelectorAll('[data-unit]')].map(u => [u.dataset.unit, [u.dataset.trace ?? '', getComputedStyle(u).opacity]]))");
        let said = format!("{host}.querySelector('.trace-words').textContent");
        assert_eq!(browser.eval(&said).unwrap(), ui::TRACE_IDLE);
        assert_eq!(browser.eval(&format!("{host}.querySelector('.trace-clear').checkVisibility()")).unwrap(), false);
        browser.eval(&format!("{host}.querySelector('[data-unit=\"a-13\"] [data-trace-pick]').click()")).unwrap();
        browser.eval("new Promise(r => setTimeout(r, 500))").unwrap();
        assert_eq!(browser.eval(&said).unwrap(), "Tracing a-13. It waits for a-12 and s-3. a-14 waits on it.");
        let r = browser.eval(&roles).unwrap();
        assert_eq!(r["a-13"][0], "selected", "{r}");
        assert_eq!(r["a-12"][0], "up", "{r}");
        assert_eq!(r["s-3"][0], "up", "{r}");
        assert_eq!(r["a-14"][0], "down", "{r}");
        assert_eq!(r["a-15"], json!(["faded", "0.25"]), "{r}");
        assert_eq!(r["a-12"][1], "1", "{r}");
        let opened = browser.eval(&format!("(u => [u.querySelector('[data-trace-pick]').getAttribute('aria-pressed'), u.querySelector('[data-trace-pick]').getAttribute('aria-expanded'), u.querySelector('[data-trace-more]').hidden, getComputedStyle(u.querySelector('.trace-role'), '::before').content])({host}.querySelector('[data-unit=\"a-13\"]'))")).unwrap();
        assert_eq!(opened, json!(["true", "true", false, "\"Selected\""]));
        // the rail runs from s-3 (the first lit) down to a-14 (the last), through the head between
        let rail = browser.eval(&format!("[...{host}.querySelectorAll('.rail-slot')].map(s => (s.dataset.unit ?? 'head') + ':' + (s.querySelector(':scope > .rail').dataset.rail ?? ''))")).unwrap();
        assert_eq!(rail, json!(["head:", "s-3:bottom node", "a-12:top bottom node", "head:top bottom", "a-13:top bottom chosen", "a-14:top node", "a-15:"]));
        assert_eq!(browser.eval(&format!("{host}.querySelector('.trace-clear').checkVisibility()")).unwrap(), true);
        // the arrows move between the units' buttons; Escape clears and keeps the focus there
        browser.eval(&format!("{host}.querySelector('[data-unit=\"a-13\"] [data-trace-pick]').focus()")).unwrap();
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "ArrowDown", "code": "ArrowDown", "windowsVirtualKeyCode": 40})).unwrap();
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": "ArrowDown", "code": "ArrowDown", "windowsVirtualKeyCode": 40})).unwrap();
        assert_eq!(browser.eval("document.activeElement.closest('[data-unit]').dataset.unit").unwrap(), "a-14");
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27})).unwrap();
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27})).unwrap();
        browser.eval(FRAMES).unwrap();
        assert_eq!(browser.eval(&said).unwrap(), ui::TRACE_IDLE);
        assert_eq!(browser.eval(&format!("{host}.hasAttribute('selected') || {host}.hasAttribute('tracing')")).unwrap(), false);
        assert_eq!(browser.eval(&format!("[...{host}.querySelectorAll('[data-trace]')].length")).unwrap(), 0);
        assert_eq!(browser.eval("document.activeElement.closest('[data-unit]').dataset.unit").unwrap(), "a-13");
        // selecting again clears; Clear trace clears and gives the focus back
        browser.eval(&format!("{host}.querySelector('[data-unit=\"a-12\"] [data-trace-pick]').click()")).unwrap();
        assert_eq!(browser.eval(&format!("{host}.getAttribute('selected')")).unwrap(), "a-12");
        browser.eval(&format!("{host}.querySelector('[data-unit=\"a-12\"] [data-trace-pick]').click()")).unwrap();
        assert_eq!(browser.eval(&format!("{host}.hasAttribute('selected')")).unwrap(), false);
        browser.eval(&format!("{host}.querySelector('[data-unit=\"s-3\"] [data-trace-pick]').click()")).unwrap();
        browser.eval(&format!("{host}.querySelector('.trace-clear').click()")).unwrap();
        assert_eq!(browser.eval(&format!("[{host}.hasAttribute('selected'), document.activeElement.closest('[data-unit]')?.dataset.unit]")).unwrap(), json!([false, "s-3"]));

        // ---- the band on a phone: the name over its words, one column, nothing sideways
        browser.viewport(390, "light").unwrap();
        browser.navigate(&gallery).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        browser.eval(FRAMES).unwrap();
        let band = browser.eval("(b => { const h = b.querySelector('h1').getBoundingClientRect(), l = b.querySelector('.band-lead').getBoundingClientRect(), s = b.querySelector('.band-summary').getBoundingClientRect(); return {size: getComputedStyle(b.querySelector('h1')).fontSize, whole: b.querySelector('h1').getBoundingClientRect().height < 80, stacked: l.top >= h.bottom - 1 && s.top >= l.bottom - 1, left: Math.abs(h.left - s.left) < 12, items: [...document.querySelectorAll('.band-strip .rf-list a')].map(a => Math.round(a.getBoundingClientRect().left)).filter((x, i, all) => all.indexOf(x) === i).length}; })(document.querySelector('.gal-band .band-head').parentElement)").unwrap();
        // a name of seven letters steps down from 88px so it stays whole
        assert_eq!(band["size"], "64px", "{band}");
        assert_eq!(band["whole"], true, "{band}");
        assert_eq!(band["stacked"], true, "{band}");
        assert_eq!(band["left"], true, "{band}");
        assert_eq!(band["items"], 1, "{band}");
        let g = browser.eval(PAGE).unwrap();
        assert!(g["scroll"].as_f64() <= g["width"].as_f64(), "{g}");
        // the frame on a phone: the band's row wraps inside the window, the sections a row of
        // 44px targets under it
        let frame = browser.eval("({nav: document.querySelector('#top-nav').scrollWidth <= document.querySelector('#top-nav').clientWidth, links: [...document.querySelectorAll('.subnav nav.links > a')].every(a => a.getBoundingClientRect().height >= 44)})").unwrap();
        assert_eq!(frame, json!({"nav": true, "links": true}));

        // ---- screenshots: the frame on the plan and home, and the gallery
        let plan = format!("{base}/projects/id/{}", n.almanac);
        for width in [390, 1440, 2560] {
            for theme in ["light", "dark"] {
                for (name, url) in [("plan", &plan), ("home", &format!("{base}/")), ("ui", &gallery)] {
                    browser.viewport(width, theme).unwrap();
                    browser.navigate(url).unwrap();
                    browser.wait("document.readyState === 'complete'").unwrap();
                    browser.eval("document.fonts.ready").unwrap();
                    browser.eval(FRAMES).unwrap();
                    let g: Value = browser.eval(PAGE).unwrap();
                    assert!(g["scroll"].as_f64() <= g["width"].as_f64(), "{name} {width} {theme}: {g}");
                    // the band's content and the page share their left edge
                    let edges = browser.eval("(n => [Math.round(n.getBoundingClientRect().left), Math.round(document.querySelector('main > :not(sluice-banner):not([hidden])')?.getBoundingClientRect().left ?? n.getBoundingClientRect().left)])(document.querySelector('#top-nav'))").unwrap();
                    assert_eq!(edges[0], edges[1], "{name} {width} {theme}: {edges}");
                    shoot(&mut browser, &format!("{name}-{width}-{theme}"));
                }
            }
        }
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), json!([]));
    })
    .await
    .unwrap();
    server.abort();
}
