//! The second critique's fixes, each through the pages it changed: a note sent to many steps
//! is one entry and a group of mixed recipients says each one; a rerun says what started it
//! and its outputs which run set them; a lane matrix says a wait its rows share once; a
//! succeeded step's Retry asks first, naming what may run again; an Overview with no key
//! output shows a few short outputs; a page's heading is its title whole; the step's tab is
//! Messages and the inbox's first view For you; an empty log names its filters with a way out
//! of each; a failure says what to try; the index says a run's time as running; an empty run
//! file is named, not linked; a long log record is one cut line. In Chromium: the page's keys.
use crate::board_fixture;
use crate::chrome;
use crate::plan_html;
use crate::stored_messages;
use board_fixture::Fixture;
use chrome::Chrome;
use serde_json::json;
use sluice_model::{
    error::PublicError,
    events::Event,
    ids::{AttemptId, ProjectId, Revision, RunId, StepId, WorkGeneration},
};
use sluice_store::RetrySafety;

fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
    let start = html
        .find(from)
        .unwrap_or_else(|| panic!("{from} in {html}"));
    let end = html[start..].find(to).map_or(html.len(), |e| start + e);
    &html[start..end]
}

/// One run of `step` from `started`, ended `finished` with `result` (both none while it runs).
async fn run(
    f: &Fixture,
    project: ProjectId,
    step: &'static str,
    started: &'static str,
    finished: Option<&'static str>,
    result: Option<serde_json::Value>,
) -> RunId {
    let run = RunId::new();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let attempt = AttemptId::new();
            tx.sql().execute(
                "INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) VALUES (?1,?2,?3,1,1,'terminal','{}','hash',?4)",
                (attempt.to_string(), project.to_string(), step, started),
            )?;
            tx.sql().execute(
                "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at,started_at,finished_at,result) VALUES (?1,?2,?3,?4,1,1,?5,?5,?6,?7)",
                (run.to_string(), project.to_string(), attempt.to_string(), step, started, finished, result.map(|r| r.to_string())),
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    run
}

/// A project `shared` whose lane units a1 and a2 both wait on `base`, and a3 on `other`.
async fn shared(f: &Fixture) -> ProjectId {
    let specs = f._home.path().join("shared-specs");
    std::fs::create_dir_all(&specs).unwrap();
    let mut steps = serde_json::Map::new();
    for (unit, after) in [("a1", "base"), ("a2", "base"), ("a3", "other")] {
        let spec = specs.join(format!("{unit}.md"));
        std::fs::write(&spec, format!("# Lane {unit}\n")).unwrap();
        let tags = json!([format!("unit:{unit}")]);
        steps.insert(
            format!("{unit}-fork"),
            json!({"run":"custom.open","tags":tags,"after":[after]}),
        );
        steps.insert(
            format!("{unit}-work"),
            json!({"run":"custom.open","tags":tags,"after":[format!("{unit}-fork")],
                "in":{"ticket":{"default":format!("T-{unit}")},"spec":{"file":spec.to_str().unwrap()}}}),
        );
        steps.insert(
            format!("{unit}-land"),
            json!({"run":"custom.open","tags":tags,"after":[format!("{unit}-work")]}),
        );
    }
    steps.insert(
        "base".into(),
        json!({"run":"custom.open","doc":"The substrate lands first"}),
    );
    steps.insert("other".into(), json!({"run":"custom.open"}));
    let id = f
        .project(
            "shared",
            json!({"steps": steps}),
            &[("base", "running"), ("other", "running")],
        )
        .await;
    let recipes = f
        ._home
        .path()
        .join("projects")
        .join(id.to_string())
        .join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    std::fs::write(
        recipes.join("lane.json"),
        serde_json::to_vec(&board_fixture::lane_recipe()).unwrap(),
    )
    .unwrap();
    id
}

#[tokio::test]
async fn a_step_with_messages_has_a_messages_tab() {
    let f = Fixture::new().await;
    let id = f.titled().await;
    stored_messages::stored(
        &f.writer,
        id,
        stored_messages::Stored {
            thread: "step-l1-work",
            from: "orchestrator",
            to: Some("l1-work"),
            body: "Rebase first.",
            ..Default::default()
        },
    )
    .await;
    // and one it sent on another thread: its Messages hold both
    stored_messages::stored(
        &f.writer,
        id,
        stored_messages::Stored {
            thread: "orchestrator",
            from: "l1-work",
            to: Some("orchestrator"),
            body: "Rebased.",
            ..Default::default()
        },
    )
    .await;
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/l1-work")).await;
    assert!(
        page.contains("data-tab=\"messages\" data-preserve-attr=\"aria-selected tabindex\" aria-label=\"Messages, 2 messages\">Messages<span class=\"n\" aria-hidden=\"true\">2</span></button>"),
        "{page}"
    );
    // `?tab=messages` opens it, and `?tab=thread`, its old name, still does
    for tab in ["messages", "thread"] {
        let (_, page) = f
            .get(&format!("/projects/id/{id}/steps/l1-work?tab={tab}"))
            .await;
        assert!(
            page.contains("<sluice-tabs id=\"tabs\" current=\"messages\""),
            "{tab}: {page}"
        );
    }
    // the thread page says how many are its own and leads to them all
    let (_, thread) = f
        .get(&format!("/projects/id/{id}/thread?thread=step-l1-work"))
        .await;
    assert!(
        thread.contains(&format!(
            "<p class=\"meta convo-of\">1 in this thread · <a href=\"/projects/id/{id}/steps/l1-work?tab=messages\">2 with this step</a>"
        )),
        "{thread}"
    );
    // a step with none offers its thread page in its head's menu
    let (_, other) = f.get(&format!("/projects/id/{id}/steps/l2-work")).await;
    assert!(
        other.contains(&format!(
            "<a href=\"/projects/id/{id}/thread?thread=step-l2-work\">Message it</a>"
        )),
        "{other}"
    );
    // and that page is no dead end before anyone wrote in it: the step's crumbs and title,
    // and the box the first message is written in
    let (_, empty) = f
        .get(&format!("/projects/id/{id}/thread?thread=step-l2-work"))
        .await;
    assert!(
        empty.contains(&format!("<a href=\"/projects/id/{id}/steps/l2-work\">"))
            && empty.contains("<textarea")
            && empty.contains("Message to ")
            && !empty.contains("No messages in this thread."),
        "{empty}"
    );
}

#[tokio::test]
async fn a_failure_says_what_to_try() {
    let f = Fixture::new().await;
    let id = f
        .project(
            "capped",
            json!({"steps":{"q":{"run":"custom.open"},"r":{"run":"custom.open"}}}),
            &[("q", "failed"), ("r", "running")],
        )
        .await;
    let quota = PublicError::AgentFailure {
        retry_at: None,
        kind: "QuotaExhausted".into(),
        message: "usage cap".into(),
        session: None,
    };
    let error = serde_json::to_string(&quota).unwrap();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='q'",
                (id.to_string(), error),
            )?;
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
    let (_, page) = f.get(&format!("/projects/id/{id}/steps/q")).await;
    assert!(
        page.contains("<p class=\"err-line\">Its engine hit a usage cap.</p>")
            && (page
                .contains("<p class=\"err-next\">Retry once its engine's usage cap resets.</p>")
                || page.contains(
                    "<p class=\"err-next\">Retry once its engine&#39;s usage cap resets.</p>"
                )
                || page.contains(
                    "<p class=\"err-next\">Retry once its engine&#x27;s usage cap resets.</p>"
                )),
        "{page}"
    );
}

async fn serve(f: &Fixture) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (
        addr,
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    )
}
fn key(browser: &mut Chrome, key: &str) {
    browser
        .eval(&format!(
            "(document.activeElement ?? document.body).dispatchEvent(new KeyboardEvent('keydown', {{key: {key:?}, bubbles: true, cancelable: true}}))"
        ))
        .unwrap();
}

/// The page's keys: / finds on the page, g then a letter goes to a section, ? opens their list;
/// none while typing in a field.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_the_pages_keys_find_go_and_list_but_never_while_typing() {
    let f = Fixture::new().await;
    let id = f.id;
    let (addr, server) = serve(&f).await;
    tokio::task::spawn_blocking(move || {
        let mut browser = Chrome::open(&format!("http://{addr}/projects/id/{id}")).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .wait("customElements.get('sluice-keys') && document.querySelector('[data-find]')")
            .unwrap();
        // the list is drawn, each section with its letter
        let list = browser
            .eval("[...document.querySelectorAll('sluice-keys a[data-go]')].map(a => [a.dataset.go, a.textContent])")
            .unwrap();
        assert!(
            list.as_array().unwrap().iter().any(|e| e[0] == "l" && e[1] == "Log"),
            "{list}"
        );
        // / finds on the page
        browser.eval("document.activeElement?.blur()").unwrap();
        key(&mut browser, "/");
        assert_eq!(
            browser.eval("document.activeElement.matches('[data-find]')").unwrap(),
            json!(true)
        );
        // typed into the find field, g and l are its letters: nothing moves
        for (key, code, number) in [("g", "KeyG", 71), ("l", "KeyL", 76)] {
            browser.send("Input.dispatchKeyEvent", json!({"type":"keyDown","key":key,"code":code,"text":key,"windowsVirtualKeyCode":number})).unwrap();
            browser.send("Input.dispatchKeyEvent", json!({"type":"keyUp","key":key,"code":code,"windowsVirtualKeyCode":number})).unwrap();
        }
        assert_eq!(browser.eval("document.querySelector('[data-find]').value").unwrap(), "gl");
        assert_eq!(
            browser.eval("location.pathname").unwrap(),
            json!(format!("/projects/id/{id}"))
        );
        // ? opens the list: display preferences at Keyboard shortcuts, the focus on its first key
        browser.eval("document.activeElement.blur()").unwrap();
        key(&mut browser, "?");
        browser
            .wait("document.activeElement.matches('sluice-keys a[data-go]')")
            .unwrap();
        assert_eq!(
            browser
                .eval("[document.querySelector('details.settings').open, document.querySelector('sluice-keys').closest('details.sub').open, document.querySelector('sluice-keys a[data-go]').checkVisibility(), document.querySelector('.sub-themes > summary').checkVisibility()]")
                .unwrap(),
            json!([true, true, true, false])
        );
        browser
            .eval("document.querySelector('details.settings').open = false; document.activeElement.blur()")
            .unwrap();
        assert_eq!(browser.eval("window.browserErrors").unwrap(), json!([]));
        // g then l goes to the project's log
        key(&mut browser, "g");
        key(&mut browser, "l");
        browser
            .wait(&format!("location.pathname === '/projects/id/{id}/log'"))
            .unwrap();
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test]
async fn messages_retries_waits_and_run_files() {
    let f = Fixture::new().await;
    {
        let id = f.titled().await;
        let say = |to: &'static str, body: &'static str| stored_messages::Stored {
            thread: "step-l1-work",
            from: "l1-work",
            to: Some(to),
            body,
            ..Default::default()
        };
        let mut ids = vec![];
        for to in ["l2-work", "l3-work", "probe"] {
            ids.push(
                stored_messages::stored(
                    &f.writer,
                    id,
                    say(to, "Rebase onto the cutover when it lands."),
                )
                .await
                .id
                .0,
            );
        }
        stored_messages::stored(&f.writer, id, say("orchestrator", "Done: W0 is final.")).await;
        let (_, page) = f
            .get(&format!("/projects/id/{id}/thread?thread=step-l1-work"))
            .await;
        let list = between(&page, "<ol class=\"convo-list\">", "</ol></li></ol>");
        // the note once, "to 3 steps", each recipient in its fold, each message's anchor kept
        assert_eq!(
            list.matches("Rebase onto the cutover when it lands.")
                .count(),
            1,
            "{list}"
        );
        assert!(
        list.contains("<details class=\"m-to m-many\" data-preserve-attr=\"open\"><summary>to 3 steps</summary>"),
        "{list}"
    );
        for (n, message) in ids.iter().enumerate() {
            assert!(
                list.contains(&format!("id=\"message-{message}\"")),
                "{n}: {list}"
            );
        }
        assert!(list.contains("/steps/probe"), "{list}");
        // the group's recipients are mixed: its head names only who sent them, and each message
        // says to whom
        let head = between(list, "<p class=\"mg-head\">", "</p>");
        assert!(!head.contains("mg-to"), "{head}");
        assert!(
            list.contains(
                "<span class=\"m-to\">to <span class=\"who who-old\">orchestrator</span></span>"
            ),
            "{list}"
        );
        // the conversation counts every message
        assert!(page.contains("4 messages"), "{page}");
    }
    {
        let id = f
            .project(
                "rerun",
                json!({"steps":{"w":{"run":"custom.open","outputs":{"summary":"string"}}}}),
                &[("w", "running")],
            )
            .await;
        f.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute(
                    "UPDATE steps SET outputs='{\"summary\":\"First pass\"}' WHERE project_id=?1",
                    [id.to_string()],
                )?;
                tx.changed(Some(id), "status");
                Ok(())
            })
            .await
            .unwrap();
        run(
            &f,
            id,
            "w",
            "2026-10-01T00:00:00Z",
            Some("2026-10-01T00:10:00Z"),
            Some(json!({"status":"succeeded","outputs":{"summary":"First pass"}})),
        )
        .await;
        // the owner retried it with feedback, then its second run started
        f.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.append_record(
                    Some(id),
                    Event::StepRetry {
                        rev: Revision(1),
                        author: "owner".into(),
                        reason: "check the docs too.".into(),
                        step: StepId::new("w").unwrap(),
                        work: WorkGeneration(2),
                    },
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let feedback = stored_messages::stored(
            &f.writer,
            id,
            stored_messages::Stored {
                thread: "step-w",
                from: "owner",
                to: Some("w"),
                body: "Look at the docs as well.",
                ..Default::default()
            },
        )
        .await
        .id
        .0;
        run(&f, id, "w", "2099-01-01T00:00:00Z", None, None).await;
        let (_, page) = f.get(&format!("/projects/id/{id}/steps/w")).await;
        let now = between(&page, "<article class=\"mod d-sec d-now\"", "</article>");
        let why = between(now, "<p class=\"meta now-retry\">", "</p>");
        assert!(
            why.starts_with("<p class=\"meta now-retry\">Run 2 started <time"),
            "{why}"
        );
        assert!(
        why.contains(&format!(
            ", after <a href=\"#run-1\">run 1</a> succeeded. You retried it: check the docs too, <a href=\"#message-{feedback}\">with feedback</a>."
        )),
        "{why}"
    );
        // its output, from the run before this one, under Now: said so, and standing back
        let key = between(&page, "<article class=\"mod d-sec d-key", "</article>");
        assert!(
            key.starts_with("<article class=\"mod d-sec d-key d-sofar is-earlier\""),
            "{key}"
        );
        assert!(
            key.contains("<p class=\"meta d-from\">From run 1, before this run · <time"),
            "{key}"
        );
    }
    {
        let id = shared(&f).await;
        let (_, page) = f.get(&format!("/projects/id/{id}")).await;
        assert!(
            !page.contains("mx-shared") && !page.contains("board-edges"),
            "{page}"
        );
        // not begun, they are counted on the plan; each one's steps say what they wait for
        for unit in ["a1", "a2", "a3"] {
            assert_eq!(plan_html::place(&page, unit), "", "{unit}");
        }
        assert!(
            plan_html::band(&page, "plan-roster").contains("<b>3 waiting</b> on earlier work"),
            "{page}"
        );
        for (unit, on) in [
            ("a1", "The substrate lands first"),
            ("a2", "The substrate lands first"),
            ("a3", "other"),
        ] {
            let (_, unit_page) = f.get(&format!("/projects/id/{id}/units/{unit}")).await;
            let note = between(&unit_page, "<span class=\"sv-note\">", "</span>");
            assert!(note.contains(&format!("waits for {on}")), "{unit}: {note}");
        }
    }
    {
        let id = f
            .project(
                "chain",
                json!({"steps":{
                "a":{"run":"custom.open","outputs":{"path":"string","head":"string"}},
                "b":{"run":"custom.open","after":["a"]},
                "c":{"run":"custom.open","after":["b"]}}}),
                &[("a", "succeeded")],
            )
            .await;
        f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET outputs='{\"path\":\"/forks/a\",\"head\":\"8128e0d\"}' WHERE project_id=?1 AND step_id='a'",
                [id.to_string()],
            )?;
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
        let (_, page) = f.get(&format!("/projects/id/{id}/steps/a")).await;
        let actions = between(&page, "<div class=\"sh-acts\">", "</header>");
        assert!(
            actions.contains("<sluice-confirm heading=\"Retry a?\"")
                && actions.contains("<details class=\"confirm-flow\"><summary>Retry</summary>"),
            "{actions}"
        );
        assert!(
        actions.contains("<p class=\"confirm-copy\">It succeeded. The 2 steps after it rerun only if its result changes.</p>"),
        "{actions}"
    );
        assert!(
            actions.contains(
                "<label class=\"confirm-reason\">Feedback (optional)<textarea name=\"message\""
            ),
            "{actions}"
        );
        assert!(actions.contains("<button type=\"button\" data-keep>Keep its result</button>"));
        assert!(
            !actions.contains("value=\"retry\" class=\"primary\""),
            "{actions}"
        );
        // with no key output, its outputs are a path and a hash: they identify rather than
        // explain, so Overview's preview leaves them to the Outputs tab, which draws them whole
        let overview = between(&page, "id=\"tp-overview\"", "<!--/r:tp-overview-->");
        assert!(
            !overview.contains("/forks/a") && !overview.contains("8128e0d"),
            "{overview}"
        );
        let outputs = between(&page, "id=\"tp-outputs\"", "<!--/r:tp-outputs-->");
        assert!(
            outputs.contains("/forks/a") && outputs.contains("8128e0d"),
            "{outputs}"
        );
    }
    {
        let long = "The lashlang substrate is Lash VM: crates lash-vm and lash-vm-runtime, LashVm symbols and stored engine kind lashvm, with old stored formats refused and no aliases at all (FIG-5571)";
        assert!(long.chars().count() > 160);
        let id = f
            .project(
                "long",
                json!({"steps":{"w":{"run":"custom.open","doc":long,"in":{"x":{"default":1}}}}}),
                &[],
            )
            .await;
        let (_, page) = f.get(&format!("/projects/id/{id}/steps/w")).await;
        let h1 = between(&page, "<h1 id=\"d-title\" class=\"sh-t\"", "</h1>");
        assert!(h1.ends_with("no aliases at all (FIG-5571)"), "{h1}");
        // the tab's own title stays cut
        assert!(between(&page, "<title>", "</title>").contains('…'));
        // a tab's name is one phrase, its count in words
        assert!(
        page.contains(" aria-label=\"Inputs, 1 input\">Inputs<span class=\"n\" aria-hidden=\"true\">1</span></button>"),
        "{page}"
    );
        // the inbox's first view is For you; Inbox is the tray's name alone
        let (_, inbox) = f.get(&format!("/projects/id/{}/inbox", f.id)).await;
        let seg = between(
            &inbox,
            "<nav class=\"tabs\" aria-label=\"Messages\">",
            "</nav>",
        );
        assert!(
            seg.contains(">For you</a>") && !seg.contains(">Inbox<"),
            "{seg}"
        );
    }
    {
        let (_, page) = f
            .get(&format!(
                "/projects/id/{}/log?errors=1&step=alpha-build",
                f.id
            ))
            .await;
        let empty = between(&page, "<div class=\"empty log-empty\">", "</div>");
        assert!(
            empty.contains("<p>No error records for step alpha-build.</p>"),
            "{empty}"
        );
        let base = format!("/projects/id/{}/log", f.id);
        assert!(
            empty.contains(&format!(
                "<a href=\"{base}?step=alpha-build\">Show every kind</a>"
            )) && empty.contains(&format!("<a href=\"{base}?errors=1\">Any step</a>")),
            "{empty}"
        );
        assert!(
            !empty.contains("retired"),
            "it is the filter, not a retirement: {empty}"
        );
    }
    {
        let id = f
            .project(
                "files",
                json!({"steps":{"s":{"run":"custom.open"}}}),
                &[("s", "failed")],
            )
            .await;
        let run = run(
            &f,
            id,
            "s",
            "2026-10-01T00:00:00Z",
            Some("2026-10-01T00:01:00Z"),
            Some(json!({"status":"failed"})),
        )
        .await;
        let dir = f._home.path().join("runs").join(run.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stderr.log"), "").unwrap();
        std::fs::write(dir.join("summary.txt"), "It ran.").unwrap();
        let (_, page) = f.get(&format!("/projects/id/{id}/steps/s?tab=runs")).await;
        let files = between(&page, "<dd class=\"a-files\">", "</dd>");
        assert!(
            files.contains("<span class=\"run-file-empty\">stderr.log · empty</span>")
                && !files.contains("files/stderr.log"),
            "{files}"
        );
        assert!(
            files.contains("files/summary.txt\">summary.txt</a>"),
            "{files}"
        );
        // a record whose one sentence runs long is cut on its row; its JSON keeps it whole
        let long = "conflict in ".to_owned()
            + &"crates/lash-protocol-rlm/src/driver/tests.rs, ".repeat(30);
        let message = long.clone();
        f.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.append_record(
                    Some(id),
                    Event::StepStatus {
                        step: StepId::new("s").unwrap(),
                        from: Some(sluice_model::commands::StepStatus::Running),
                        to: sluice_model::commands::StepStatus::Failed,
                        error: Some(PublicError::FnFailure { message }),
                        run_ids: vec![],
                        needs: Default::default(),
                    },
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let (_, log) = f.get(&format!("/projects/id/{id}/log?errors=1")).await;
        let what = between(&log, "<p class=\"log-what\">", "</p>");
        let text: String = {
            let mut out = String::new();
            let mut tag = false;
            for c in what.chars() {
                match c {
                    '<' => tag = true,
                    '>' => tag = false,
                    c if !tag => out.push(c),
                    _ => {}
                }
            }
            out
        };
        assert!(text.chars().count() <= 245 && text.ends_with('…'), "{text}");
        assert!(
            log.contains(&long[..200]) || log.contains("driver/tests.rs, crates"),
            "the JSON keeps it"
        );
    }
}
