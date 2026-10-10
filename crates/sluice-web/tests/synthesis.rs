//! The Synthesis foundation: the frame (the slim navy bar, the page's head and its row on the
//! paper under it), the kit's primitives over HTTP and in the gallery, the neutral fixture on
//! every page, and in a real Chromium the plain sheet (no grid to show), select-to-trace
//! (select, keyboard, clear, the fades and the chain's sentence), Details by keyboard and
//! without script, the head's budget, and no page scrolling sideways from 320 to 3840px.
//! `SLUICE_SYNTH_SCREENS=<dir>` also saves the frame on the plan and home and the gallery at
//! 390, 1440 and 2560 in both of Sluice's themes.
mod board_fixture;
#[path = "../../../tests/support/chrome.rs"]
mod chrome;
mod neutral;
mod seed;
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
        Stage::new("review", Some(Shown::Running)).running_since("2026-10-10T00:00:00Z", 4_000.0),
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
    // a running cell says its state and its time, never against a usual time
    assert!(
        html.contains("<span class=\"vh\">: running</span>"),
        "{html}"
    );
    assert!(!html.contains("usual") && !html.contains("×"), "{html}");
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
fn the_frame_primitives_escape_what_they_are_given() {
    let head = ui::page_head("a<b>", &ui::summary_sentence(&ui::Summary::default()));
    assert_eq!(
        head.as_str(),
        "<div class=\"page-head\"><div class=\"ph-name\"><h1>a&lt;b&gt;</h1></div><p class=\"page-line\">Nothing has started yet.</p></div>"
    );
    // a page that says what it shows has a muted note under its name instead
    assert!(
        ui::page_head_note("Log", &ui::summary_sentence(&ui::Summary::default()))
            .as_str()
            .ends_with(
                "<h1>Log</h1></div><p class=\"page-note\">Nothing has started yet.</p></div>"
            )
    );
    // what finished last: a line each, its clock, its title (its name when untitled), how
    // long it took, and how it ended when that needs a look
    let latest = ui::latest_list(
        "Finished last",
        &[
            ui::Finished {
                name: "a-<1>".into(),
                title: "Gulls".into(),
                href: "/u/a-1".into(),
                at: "2026-10-09T17:49:00Z".into(),
                took: 3_840.0,
                runs: 2,
                shown: Some(Shown::Succeeded),
                place: String::new(),
            },
            ui::Finished {
                name: "a-<2>".into(),
                title: String::new(),
                href: "/u/a-2".into(),
                at: "2026-10-09T17:40:00Z".into(),
                took: 60.0,
                runs: 1,
                shown: Some(Shown::Failed),
                place: "almanac".into(),
            },
        ],
    );
    let latest = latest.as_str();
    assert!(
        latest.starts_with("<ol class=\"latest\" aria-label=\"Finished last\"><li><a href=\"/u/a-1\"><span class=\"lt-at\"><time data-clock datetime=\"2026-10-09T17:49:00Z\" title=\"2026-10-09 17:49 UTC\">17:49</time></span><span class=\"lt-t\">Gulls</span><span class=\"lt-took\">took 1h 4m, 2 runs</span></a></li>"),
        "{latest}"
    );
    assert!(
        latest.contains(
            "<span class=\"lt-t\"><span class=\"lt-place\">almanac</span> a-&lt;2&gt;</span>"
        ),
        "{latest}"
    );
    assert!(latest.contains("failed</span> took 1m"), "{latest}");
    assert!(ui::latest_list("Finished last", &[]).as_str().is_empty());
    // the margin shows what reads at a glance and keeps what identifies in its Details
    let margin = ui::margin_module(&ui::LongRun {
        name: "survey".into(),
        fields: vec![
            ("checked".into(), json!(1240)),
            ("note".into(), json!("<b>")),
            (
                "head".into(),
                json!("86aeeb2d5a4982dbb8d18bb269842c0c72f06979"),
            ),
        ],
        since: "2026-10-08T00:00:00Z".into(),
        ..Default::default()
    });
    let margin = margin.as_str();
    assert!(margin.starts_with("<article class=\"mod swell-margin\" style=\"--span:2\" data-span=\"2\" aria-label=\"survey, running\">"), "{margin}");
    assert!(margin.contains("<dl class=\"mm-fields\"><div class=\"mm-f mm-lead\"><dt>checked</dt><dd>1240</dd></div><div class=\"mm-f\"><dt>note</dt><dd>&lt;b&gt;</dd></div></dl>"), "{margin}");
    let (shown, details) = margin.split_once("<details class=\"dm\"").unwrap();
    assert!(
        !shown.contains("86aeeb2d") && details.contains("86aeeb2d5a4982dbb8d18bb269842c0c72f06979"),
        "{margin}"
    );
    let trace = ui::Trace::new([("b", "a\""), ("c", "b")]);
    assert_eq!(
        trace.attrs("b").as_str(),
        " data-unit=\"b\" data-up=\"a&quot;\" data-down=\"c\" data-chain=\"Tracing b. It waits for a&quot;. c waits on it.\""
    );
    assert_eq!(
        ui::section_head("stopped", "Stopped", "1 failed").as_str(),
        "<div class=\"sec-h\" id=\"stopped\"><h2>Stopped</h2><p class=\"sec-n\">1 failed</p></div>"
    );
    // the sheet is a plain element: its columns and nothing drawn over them
    assert_eq!(
        ui::grid_open("plan-grid", 12).as_str(),
        "<div id=\"plan-grid\" class=\"sheet\" style=\"--cols:12\">"
    );
}

/// What identifies an item is behind its "⋯": a disclosure that opens without script, says
/// what it is about, and copies each id.
#[test]
fn details_hold_what_identifies_an_item() {
    let menu = ui::Details::new()
        .id("Step", "a-7-<draft>")
        .text("Recipe", "article")
        .code("Function", "almanac.write")
        .id("Empty", "")
        .menu("Shorebirds");
    let menu = menu.as_str();
    assert!(menu.starts_with("<sluice-menu"), "{menu}");
    assert!(menu.contains("<details class=\"dm\" data-preserve-attr=\"open\"><summary class=\"dm-b\" aria-label=\"Details of Shorebirds\""), "{menu}");
    assert!(menu.contains("<dt>Step</dt><dd>"), "{menu}");
    assert!(menu.contains("a-7-&lt;draft&gt;"), "{menu}");
    assert!(menu.contains("aria-label=\"Copy step\""), "{menu}");
    assert!(
        menu.contains("<dt>Recipe</dt><dd><span>article</span></dd>"),
        "{menu}"
    );
    assert!(menu.contains("<code>almanac.write</code>"), "{menu}");
    // an empty value is left out, and Details with nothing in them are not drawn
    assert!(!menu.contains("<dt>Empty</dt>"), "{menu}");
    assert!(
        ui::Details::new()
            .id("Step", "")
            .menu("x")
            .as_str()
            .is_empty()
    );
}

#[tokio::test]
async fn the_frame_draws_the_page_head_and_the_row_under_it_and_the_gallery_every_primitive() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let (status, home) = f.get("/").await;
    assert_eq!(status, 200);
    assert!(home.contains("<header class=\"site-head\"><div class=\"band-in\"><nav id=\"top-nav\" class=\"top\" aria-label=\"Main\">"), "{home}");
    assert!(home.contains("@fontsource-variable/schibsted-grotesk@5.3.0/index.css"));
    assert!(home.contains("@fontsource-variable/jetbrains-mono@5.3.0/index.css"));
    assert!(!home.contains("archivo") && !home.contains("public-sans"));
    // the page's head on the paper under the slim bar, its sections in the row under it; the
    // live line only on a page with a stream
    assert!(home.contains("<div class=\"page-top\"><div id=\"home-band\" class=\"band-wrap\"><div class=\"page-head\"><div class=\"ph-name\"><h1>All projects</h1>"), "{home}");
    assert!(home.contains("<div class=\"subnav\"><div class=\"subnav-in\"><nav class=\"links\" aria-label=\"Sections\">"), "{home}");
    assert!(home.contains("<span class=\"live\">Live</span>"), "{home}");
    let (_, plan) = f.get(&format!("/projects/id/{}", n.almanac)).await;
    assert!(
        plan.contains("<a class=\"home\" href=\"/\">Projects</a>"),
        "{plan}"
    );
    // the current project in the switcher, with its icon
    assert!(
        plan.contains("<details class=\"switcher current\""),
        "{plan}"
    );
    assert!(
        plan.contains("aria-label=\"Project: almanac, switch project\"><span class=\"proj-icon"),
        "{plan}"
    );
    // the plan's head: the project's name and its Details, the summary sentence under it
    assert!(plan.contains("<div id=\"plan-band\" class=\"plan-band\"><div class=\"page-head\"><div class=\"ph-name\"><h1>almanac</h1><sluice-menu><details class=\"dm\""), "{plan}");
    assert!(plan.contains("<p class=\"page-line\">"), "{plan}");
    assert!(
        plan.contains("<nav class=\"links\" aria-label=\"almanac\">"),
        "{plan}"
    );
    assert!(plan.contains(&format!("<a class=\"project-settings\" href=\"/projects/id/{}/settings\" aria-label=\"Project settings\">Settings</a>", n.almanac)), "{plan}");
    // one flat list of themes, each with its swatch; no appearance to pick and no grid to show
    assert_eq!(
        plan.matches("name=\"theme\" value=\"").count(),
        14,
        "{plan}"
    );
    assert_eq!(
        plan.matches("<span class=\"swatch\" data-theme=\"").count(),
        14,
        "{plan}"
    );
    for gone in [
        "name=\"appearance\"",
        "data-appearance",
        "grid-toggle",
        "grid-ovl",
        "sluice-grid",
        "Show grid",
        "band-head",
    ] {
        assert!(!plan.contains(gone), "{gone}: {plan}");
    }
    let (status, gallery) = f.get("/_ui").await;
    assert_eq!(status, 200);
    for gone in [
        "grid-toggle",
        "grid-ovl",
        "sluice-grid",
        "Show grid",
        "gc-n",
    ] {
        assert!(!gallery.contains(gone), "{gone}");
    }
    for (part, marks) in [
        (
            "Page head",
            &[
                "<div class=\"page-head\"><div class=\"ph-name\"><h1>almanac</h1>",
                "<p class=\"page-line\">",
            ][..],
        ),
        (
            "Details",
            &[
                "<details class=\"dm\" data-preserve-attr=\"open\"><summary class=\"dm-b\"",
                "<dl class=\"dm-l\">",
            ][..],
        ),
        (
            "Finished last",
            &["<ol class=\"latest\" aria-label=\"Finished last\">"][..],
        ),
        (
            "Summary sentence",
            &[
                "<a class=\"ask\" href=\"#\">1 question for you</a>. 1 failed, 1 cancelled. 3 units at work: 1 quiet for 53m. 2 waiting. 3 of 10 units done; the last finished",
            ][..],
        ),
        (
            "Module grid",
            &["<div id=\"l-grid\" class=\"sheet\" style=\"--cols:12\">"][..],
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
                "<div class=\"sec-h sec-strip\" style=\"--n:3\">",
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
    ] {
        assert!(plan.contains(word), "{word}: {plan}");
    }
    // Running holds a block a recipe, each headed by its name over its stages' columns
    let running = plan
        .split("<!--r:plan-running-->")
        .nth(1)
        .and_then(|r| r.split("<!--/r:plan-running-->").next())
        .unwrap_or_default();
    for head in [
        ">article</a></h3>",
        ">scan</a></h3>",
        "<span class=\"sh-stage\">draft</span>",
    ] {
        assert!(running.contains(head), "{head}: {running}");
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
async fn chromium_the_plain_sheet_the_trace_details_and_the_head_at_every_width() {
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    let (addr, server) = serve(&f);
    let base = format!("http://{addr}");
    tokio::task::spawn_blocking(move || {
        let gallery = format!("{base}/_ui");
        let mut browser = Chrome::open(&gallery).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser.navigate(&gallery).unwrap();
        browser.wait("document.readyState === 'complete' && !!customElements.get('sluice-trace')").unwrap();
        browser.eval(FRAMES).unwrap();

        // ---- the sheet is a plain element: no grid to show, nothing drawn over it, nothing kept
        let plain = browser.eval("[customElements.get('sluice-grid') === undefined, document.querySelector('#l-grid').localName, document.querySelectorAll('.grid-ovl, .gc, button.grid-toggle').length, [...document.querySelectorAll('button, a')].some(b => /show grid/i.test(b.textContent)), (() => { try { return localStorage.getItem('sluice.grid'); } catch { return null; } })()]").unwrap();
        assert_eq!(plain, json!([true, "div", 0, false, null]));

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

        // ---- Details: the "⋯" opens by keyboard, says it is open, Escape closes it and gives
        // the focus back; each id is whole with its copy button
        let dm = "document.querySelector('.gal-sec .dm')";
        browser.eval(&format!("{dm}.querySelector('summary').focus()")).unwrap();
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13, "text": "\r"})).unwrap();
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13})).unwrap();
        browser.eval(FRAMES).unwrap();
        let open = browser.eval(&format!("(d => [d.open, d.querySelector('summary').getAttribute('aria-expanded'), d.querySelector('.dm-p').checkVisibility(), d.querySelectorAll('.dm-l sluice-copy, .dm-l button.copy').length > 0])({dm})")).unwrap();
        assert_eq!(open, json!([true, "true", true, true]));
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27})).unwrap();
        browser.send("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27})).unwrap();
        browser.eval(FRAMES).unwrap();
        let shut = browser.eval(&format!("(d => [d.open, d.querySelector('summary').getAttribute('aria-expanded'), document.activeElement === d.querySelector('summary')])({dm})")).unwrap();
        assert_eq!(shut, json!([false, "false", true]));

        // ---- the head's budget: the plan's first section starts within 220px at 1440, its
        // row of rows shows no step id, and its Details hold them
        let plan = format!("{base}/projects/id/{}", n.almanac);
        browser.navigate(&plan).unwrap();
        browser.wait("document.readyState === 'complete' && document.querySelector('#project-board')").unwrap();
        browser.eval("document.fonts.ready").unwrap();
        browser.eval(FRAMES).unwrap();
        let head = browser.eval("(() => { const first = [...document.querySelectorAll('#plan-grid .sec-h')].find(h => h.checkVisibility()); return {nav: Math.round(document.querySelector('header.site-head').getBoundingClientRect().height), first: Math.round(first.getBoundingClientRect().top + scrollY), h1: getComputedStyle(document.querySelector('.page-head h1')).fontSize, line: document.querySelectorAll('.page-line').length}; })()").unwrap();
        assert!(head["first"].as_f64().unwrap() <= 220.0, "{head}");
        assert!(head["nav"].as_f64().unwrap() <= 56.0, "{head}");
        assert_eq!(head["h1"], "34px", "{head}");
        assert_eq!(head["line"], 1, "{head}");
        // a running row says its title and state, never its step's id; its Details do
        let ids = browser.eval("(() => { const row = document.querySelector('#plan-running .pl-row'); const seen = row.innerText; const d = row.querySelector('.pl-end details.dm'); const step = [...d.querySelectorAll('.dm-l > div')].find(x => x.querySelector('dt').textContent === 'Step').querySelector('code').textContent; return [row.querySelector('.pl-t') !== null, step.length > 0, seen.includes(step)]; })()").unwrap();
        assert_eq!(ids, json!([true, true, false]));
        // without script the "⋯" is a plain disclosure: a click opens it
        browser.send("Emulation.setScriptExecutionDisabled", json!({"value": true})).unwrap();
        browser.navigate(&plan).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        browser.eval("document.querySelector('#plan-running .pl-end summary').click()").unwrap();
        assert_eq!(browser.eval("(d => [d.open, d.querySelector('.dm-p').checkVisibility()])(document.querySelector('#plan-running .pl-end details.dm'))").unwrap(), json!([true, true]));
        browser.send("Emulation.setScriptExecutionDisabled", json!({"value": false})).unwrap();

        // ---- the head on a phone: the nav one slim row, the name at 28px, the plan's first
        // section near the top, and the sections a row of 44px targets
        browser.viewport(390, "light").unwrap();
        browser.navigate(&plan).unwrap();
        browser.wait("document.readyState === 'complete'").unwrap();
        browser.eval("document.fonts.ready").unwrap();
        browser.eval(FRAMES).unwrap();
        let phone = browser.eval("(() => { const first = [...document.querySelectorAll('#plan-grid .sec-h')].find(h => h.checkVisibility()); return {nav: Math.round(document.querySelector('header.site-head').getBoundingClientRect().height), h1: getComputedStyle(document.querySelector('.page-head h1')).fontSize, first: Math.round(first.getBoundingClientRect().top + scrollY), fits: document.querySelector('#top-nav').scrollWidth <= document.querySelector('#top-nav').clientWidth, links: [...document.querySelectorAll('.subnav nav.links > a')].every(a => a.getBoundingClientRect().height >= 44)}; })()").unwrap();
        assert!(phone["nav"].as_f64().unwrap() <= 52.0, "{phone}");
        assert_eq!(phone["h1"], "28px", "{phone}");
        assert!(phone["first"].as_f64().unwrap() <= 340.0, "{phone}");
        assert_eq!(phone["fits"], true, "{phone}");
        assert_eq!(phone["links"], true, "{phone}");

        // ---- no page scrolls sideways, from 320 to 3840
        let a = format!("{base}/projects/id/{}", n.almanac);
        let pages = [
            ("home", format!("{base}/")),
            ("plan", a.clone()),
            ("both", format!("{a}?view=both")),
            ("board", format!("{a}?view=board")),
            ("step", format!("{a}/steps/a-7-draft")),
            ("unit", format!("{a}/units/a-6")),
            ("inbox", format!("{base}/inbox")),
            ("day", format!("{a}/day")),
            ("log", format!("{a}/log")),
            ("settings", format!("{a}/settings")),
            ("fns", format!("{base}/fns")),
            ("ui", gallery.clone()),
        ];
        for width in [320, 390, 768, 1440, 2560, 3840] {
            for (name, url) in &pages {
                browser.viewport(width, "light").unwrap();
                browser.navigate(url).unwrap();
                browser.wait("document.readyState === 'complete'").unwrap();
                browser.eval(FRAMES).unwrap();
                let g: Value = browser.eval(PAGE).unwrap();
                assert!(g["scroll"].as_f64() <= g["width"].as_f64(), "{name} at {width} scrolls sideways: {g}");
            }
        }

        // ---- screenshots: the frame on the plan and home, and the gallery
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
                    // the nav's content and the page share their left edge
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
