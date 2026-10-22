//! Real Chromium gate for the message pages and the page's times: a note is marked read only
//! when the owner asks ("Mark read" on its card or its thread, or "Mark all read"), never by
//! being on screen or by its thread being opened; once marked it stays out of the inbox
//! through a reload and can be read in History. A time a stream
//! patch writes back in the server's words reads in the page's words again at once.
use crate::board_fixture;
use crate::chrome;
use crate::stored_messages;
use board_fixture::Fixture;
use chrome::Chrome;
use sluice_model::commands::CommandRequest;
use sluice_store::RetrySafety;

fn read_marks(f: &Fixture) -> Vec<sluice_model::commands::MarkRead> {
    f.commands
        .0
        .lock()
        .unwrap()
        .iter()
        .flat_map(|r| match r {
            CommandRequest::ReadNotes { marks, undo: false } => marks.clone(),
            _ => vec![],
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_a_note_is_read_only_when_the_owner_asks_and_times_survive_a_patch() {
    let f = Fixture::new().await;
    // A named thread is written to the project's fallback session.
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE projects SET fallback='@lash-f4' WHERE project_id=?1",
                [id.to_string()],
            )?;
            tx.changed(Some(id), "project");
            Ok(())
        })
        .await
        .unwrap();
    for (thread, body) in [
        ("notes", "The lane landed on main."),
        ("later", "A second note, on its own thread."),
    ] {
        stored_messages::stored(
            &f.writer,
            f.id,
            stored_messages::Stored {
                thread,
                from: "worker",
                to: Some("owner"),
                body,
                ..Default::default()
            },
        )
        .await;
    }
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let lanes = f.id;
    let f = std::sync::Arc::new(f);
    let checks = f.clone();
    tokio::task::spawn_blocking(move || {
        let f = checks;
        let base = format!("http://{addr}");
        let mut browser = Chrome::open(&format!("{base}/inbox")).unwrap();
        browser.viewport(390, "light").unwrap();
        browser
            .wait("document.readyState === 'complete' && document.querySelectorAll('li.note').length === 2")
            .unwrap();
        // on screen and scrolled past for well over the old two seconds: still unread
        browser.eval("window.scrollTo(0, document.body.scrollHeight)").unwrap();
        browser.eval("window.readRequests=[]; const readFetch=window.fetch; window.fetch=(...args)=>{const p=readFetch(...args); if(String(args[0]).includes('/messages/read')) readRequests.push(p); return p;}").unwrap();
        let before = browser.eval("performance.now()").unwrap().as_f64().unwrap();
        browser.send("Emulation.setVirtualTimePolicy", serde_json::json!({"policy":"advance", "budget":2600})).unwrap();
        browser.wait(&format!("performance.now() >= {}", before + 2600.0)).unwrap();
        browser.eval("Promise.all(window.readRequests)").unwrap();
        assert!(read_marks(&f).is_empty(), "seeing a note never marks it read");
        drop(browser);
        let mut browser = Chrome::open(&format!("{base}/inbox")).unwrap();
        browser.wait("document.readyState === 'complete' && document.querySelectorAll('li.note').length === 2").unwrap();
        // One action reads the page's displayed threads, with Undo, never a button per note.
        assert_eq!(browser.eval("document.querySelectorAll('section.notes form.mark-read button').length").unwrap(), 1);
        browser.eval("document.querySelector('#mark-all-read button').click()").unwrap();
        browser.wait("document.querySelectorAll('li.note').length === 0 && !!document.querySelector('#undo-read button')").unwrap();
        assert_eq!(read_marks(&f).len(), 2);
        browser.eval("document.querySelector('#undo-read button').click()").unwrap();
        browser.wait("document.querySelectorAll('li.note').length === 2 && !document.querySelector('#undo-read')").unwrap();
        // The thread has its own read action, without reading it on navigation.
        browser.navigate(&format!("{base}/projects/id/{lanes}/thread?thread=notes")).unwrap();
        browser.wait("!!document.querySelector('.thread-of form.mark-read button')").unwrap();
        browser.eval("document.querySelector('.thread-of form.mark-read button').click()").unwrap();
        browser.wait("!!document.querySelector('#undo-read button') && !document.querySelector('.thread-of form.mark-read')").unwrap();
        // Reloading the inbox keeps the remaining note and links to the full read history;
        // it never ships the marked note in a hidden disclosure.
        for _ in 0..2 {
            browser.navigate(&format!("{base}/inbox")).unwrap();
            browser.wait("document.readyState === 'complete' && !!document.querySelector('section.notes')").unwrap();
            let state = browser.eval("[document.querySelector('section.notes').textContent.includes('The lane landed'), document.querySelector('section.notes').textContent.includes('A second note'), !!document.querySelector('a[href=\"/history\"]')]").unwrap();
            assert_eq!(state, serde_json::json!([false, true, true]), "{state}");
        }
        // opening a thread does not mark it read: it offers to
        browser
            .navigate(&format!("{base}/projects/id/{lanes}/thread?thread=later"))
            .unwrap();
        browser
            .wait("document.readyState === 'complete' && !!document.querySelector('.thread-reply')")
            .unwrap();
        browser.eval("window.readRequests=[]; const readFetch=window.fetch; window.fetch=(...args)=>{const p=readFetch(...args); if(String(args[0]).includes('/messages/read')) readRequests.push(p); return p;}").unwrap();
        let before = browser.eval("performance.now()").unwrap().as_f64().unwrap();
        browser.send("Emulation.setVirtualTimePolicy", serde_json::json!({"policy":"advance", "budget":2600})).unwrap();
        browser.wait(&format!("performance.now() >= {}", before + 2600.0)).unwrap();
        browser.eval("Promise.all(window.readRequests)").unwrap();
        assert_eq!(read_marks(&f).len(), 3, "opening a thread marks nothing");
        assert_eq!(
            browser.eval("(b => b && b.checkVisibility() ? b.textContent : null)(document.querySelector('.thread-of form.mark-read button'))").unwrap(),
            "Mark 1 note read"
        );
        drop(browser);
        let mut browser = Chrome::open("about:blank").unwrap();
        // a stream patch puts the server's text back into a <time>; it reads in the page's
        // words again before the next frame (a characterData change, not a new node)
        browser
            .navigate(&format!("{base}/projects/id/{lanes}/steps/beta-build"))
            .unwrap();
        browser.wait("document.readyState === 'complete' && !!document.querySelector('#step-detail')").unwrap();
        let text = browser
            .eval("(async () => { const t = document.createElement('time'); const at = new Date(Date.now() - 3 * 86400e3 - 5 * 3600e3).toISOString(); t.dataset.ago = at; t.setAttribute('datetime', at); t.textContent = '2026-10-07 20:47 UTC'; document.querySelector('#step-detail').append(t); await new Promise(r => requestAnimationFrame(r)); const first = t.textContent; t.firstChild.nodeValue = '2026-10-07 20:47 UTC'; await Promise.resolve(); await new Promise(r => setTimeout(r, 0)); return [first, t.textContent]; })()")
            .unwrap();
        assert_eq!(text, serde_json::json!(["3d 5h ago", "3d 5h ago"]), "{text}");
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), serde_json::json!([]));
    })
    .await
    .unwrap();
    server.abort();
}

/// Close question confirms where it was pressed, as an answer does: the buttons give way to the
/// server's "Closed just now: …" line, a polite status that takes the keyboard's focus, so the
/// next Tab goes on from the question; the nav's count drops.
#[tokio::test(flavor = "multi_thread")]
async fn slow_chromium_closing_a_question_confirms_it_in_place_and_keeps_the_focus() {
    let f = Fixture::new().await;
    let mut ids = Vec::new();
    for (thread, from, title) in [
        (
            "step-beta-build",
            "beta-build",
            "Land it before the rename?",
        ),
        ("owner", "orchestrator", "Accept revision 3?"),
    ] {
        let message = stored_messages::stored(
            &f.writer,
            f.id,
            stored_messages::Stored {
                thread,
                from,
                to: Some("owner"),
                body: "Say which.",
                ui: Some(r#"root = Stack([Text("<img data-injected src=x onerror=alert(1)>")])"#),
                title: Some(title),
                question: true,
                ..Default::default()
            },
        )
        .await;
        ids.push(message.id.0);
    }
    let second = stored_messages::stored(
        &f.writer,
        f.id,
        stored_messages::Stored {
            thread: "owner",
            from: "orchestrator",
            to: Some("owner"),
            body: "A second whole message on the same thread.",
            ..Default::default()
        },
    )
    .await;
    let second_id = second.id.0;
    let router = f.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let lanes = f.id;
    let f = std::sync::Arc::new(f);
    let checks = f.clone();
    tokio::task::spawn_blocking(move || {
        let f = checks;
        let base = format!("http://{addr}");
        let closes = |f: &Fixture| {
            f.commands
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|r| {
                    matches!(r, CommandRequest::Reply(reply)
                        if reply.owner && reply.answer.as_ref().is_some_and(|a| a.action == "close"))
                })
                .count()
        };
        // on the step's Overview, by keyboard: the focus lands on the line that says so
        let mut browser =
            Chrome::open(&format!("{base}/projects/id/{lanes}/steps/beta-build")).unwrap();
        browser.viewport(1440, "light").unwrap();
        browser
            .wait("document.readyState === 'complete' && !!document.querySelector('#tp-overview sluice-answer .answer[data-drawn]') && customElements.get('sluice-answer')")
            .unwrap();
        browser.eval("document.querySelector('#tp-overview form.q-close button').click()").unwrap();
        browser
            .wait("document.activeElement?.matches('#tp-overview .q-answered[role=status][aria-live=polite]')")
            .unwrap();
        let said = browser.eval("document.activeElement.textContent").unwrap();
        assert_eq!(said, "Closed just now: Land it before the rename? Read the threadUndo");
        assert_eq!(closes(&f), 1);
        // on the inbox the same: its line in the card's place, focused, the count down by one
        browser.send("Page.addScriptToEvaluateOnNewDocument", serde_json::json!({"source": "document.addEventListener('datastar-fetch', e => { if(e.detail.type==='datastar-patch-signals' && JSON.parse(e.detail.argsRaw?.signals??'{}').rel) window.streamReady=true; });"})).unwrap();
        browser.navigate(&format!("{base}/inbox")).unwrap();
        browser
            .wait("window.streamReady && document.readyState === 'complete' && customElements.get('sluice-answer') && [...document.querySelectorAll('#inbox-items .answer')].every(a => a.dataset.drawn)")
            .unwrap();
        // Two whole cards at once: a constant message-0 anchor must fail this check.
        let card_ids = browser.eval("[...document.querySelectorAll('#inbox-items article.q-card[id]')].map(a => a.id)").unwrap();
        let card_ids = card_ids.as_array().unwrap();
        assert!(card_ids.len() >= 2, "{card_ids:?}");
        let unique: std::collections::BTreeSet<_> = card_ids.iter().map(|id| id.as_str().unwrap()).collect();
        assert_eq!(unique.len(), card_ids.len());
        for id in &ids {
            assert!(unique.contains(format!("item-{lanes}-{id}").as_str()), "{unique:?}");
        }
        assert_eq!(browser.eval("document.querySelector('[data-injected]') === null").unwrap(), true);
        assert_eq!(browser.eval("[...document.querySelectorAll('#inbox-items .answer')].every(a => a.textContent.includes('<img data-injected'))").unwrap(), true);
        assert_eq!(browser.eval("document.querySelectorAll('sluice-menu:empty').length").unwrap(), 0);
        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), serde_json::json!([]));
        let badge = browser.eval("document.querySelector('#nav-inbox .count').textContent").unwrap();
        browser.eval("[...document.querySelectorAll('#inbox-items article.q-card:not(.q-done)')].at(-1).querySelector('form.q-close button').click()").unwrap();
        browser
            .wait("document.activeElement?.matches('#inbox-items article.q-card.q-done .q-answered')")
            .unwrap();
        let said = browser.eval("document.activeElement.textContent").unwrap();
        assert!(said.as_str().unwrap().starts_with("Closed just now: "), "{said}");
        assert_eq!(
            browser.eval("document.querySelector('#nav-inbox .count')?.textContent ?? '0'").unwrap(),
            (badge.as_str().unwrap().parse::<u32>().unwrap() - 1).to_string()
        );
        assert_eq!(closes(&f), 2);
        browser.navigate(&format!("{base}/projects/id/{lanes}/thread?thread=owner")).unwrap();
        browser.wait("document.readyState === 'complete' && !!customElements.get('sluice-menu')").unwrap();
        assert_eq!(browser.eval(&format!("document.querySelectorAll('#message-{}').length", ids[1])).unwrap(), 1);
        assert_eq!(browser.eval(&format!("document.querySelectorAll('#message-{second_id}').length")).unwrap(), 1);
        assert_eq!(browser.eval("[...document.querySelectorAll('[id]')].map(e=>e.id).filter((id,i,all)=>all.indexOf(id)!==i)").unwrap(), serde_json::json!([]));
        browser.navigate(&format!("{base}/history")).unwrap();
        browser.wait("document.readyState === 'complete' && !!document.querySelector('h1')").unwrap();
        assert_eq!(browser.eval("document.querySelectorAll('sluice-menu:empty').length").unwrap(), 0);

        assert_eq!(browser.eval("window.browserErrors ?? []").unwrap(), serde_json::json!([]));
    })
    .await
    .unwrap();
    server.abort();
}
