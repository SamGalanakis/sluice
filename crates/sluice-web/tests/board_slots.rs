//! A board's named text slots (`board_slot_set`), its Markdown and its LatestMessage: each
//! drawn on the server through the dashboard's markdown renderer, a slot with when it last
//! changed, a missing one as its fallback, and a slot change patched into an open page.
mod board_fixture;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use board_fixture::Fixture;
use sluice_model::ids::ProjectSelector;
use sluice_store::{RetrySafety, projects};
use tower::ServiceExt;

fn between<'a>(html: &'a str, start: &str, end: &str) -> &'a str {
    let from = html
        .find(start)
        .unwrap_or_else(|| panic!("no {start} in {html}"));
    let to = from + html[from..].find(end).unwrap_or_else(|| panic!("no {end}"));
    &html[from..to]
}

const BOARD: &str = r#"root = Stack([title, phase, ask, missing, plain, note, main, short, nobody, rows])
title = Heading("Slots", 1)
phase = Slot("phase")
ask = Slot("needs-sam", "Nothing needs you.")
missing = Slot("later")
plain = Text("**not** markdown")
note = Markdown("Read **the plan** and `docs`:\n\n- [lanes](https://example.com/lanes)\n- [bad](javascript:alert(1))")
main = LatestMessage("tests-main")
short = LatestMessage("tests-main", 12)
nobody = LatestMessage("nobody-here")
rows = Query("SELECT key, markdown FROM board_slots WHERE project_id = ? ORDER BY key", "Slots")
"#;

async fn set_board(f: &Fixture, program: &'static str) {
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::board_set(
                tx,
                &ProjectSelector::Id(id),
                projects::SetBoard {
                    program: Some(program.into()),
                    expected_rev: None,
                    reason: None,
                    author: "orch".into(),
                },
            )
        })
        .await
        .unwrap();
}
async fn set_slot(f: &Fixture, key: &'static str, markdown: Option<&'static str>) {
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::board_slot_set(
                tx,
                &ProjectSelector::Id(id),
                projects::SetBoardSlot {
                    key: key.into(),
                    markdown: markdown.map(Into::into),
                    author: "orch".into(),
                },
            )
        })
        .await
        .unwrap();
}
async fn message(f: &Fixture, id: i64, from: &'static str, body: String, at: &'static str) {
    let project = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,at) VALUES (?1,?2,?3,?4,'orchestrator',?5,?6)",
                rusqlite::params![id, project.to_string(), from, from, body, at],
            )?;
            tx.changed(Some(project), "log");
            Ok(())
        })
        .await
        .unwrap();
}
async fn board(f: &Fixture) -> String {
    let (status, html) = f.get(&format!("/projects/id/{}", f.id)).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    between(&html, "<aside id=\"board-pane\"", "</aside>").to_owned()
}
fn slot<'a>(board: &'a str, key: &str) -> &'a str {
    between(board, &format!("data-slot=\"{key}\""), "</div></div>")
}

#[tokio::test]
async fn a_slot_draws_its_markdown_with_its_age_and_a_missing_one_its_fallback() {
    let f = Fixture::new().await;
    set_board(&f, BOARD).await;
    set_slot(
        &f,
        "phase",
        Some("**Main green.** See [the run](https://ci.example/run/7) and [this](javascript:alert(1)):\n\n- lanes cut 36 to 11\n- <b>raw</b> html is text"),
    )
    .await;
    let html = board(&f).await;
    let phase = slot(&html, "phase");
    assert!(phase.contains("<strong>Main green.</strong>"), "{phase}");
    assert!(
        phase.contains("<a href=\"https://ci.example/run/7\">the run</a>"),
        "{phase}"
    );
    assert!(!phase.contains("javascript:"), "{phase}");
    assert!(phase.contains("<a href=\"\">this</a>"), "{phase}");
    assert!(phase.contains("<li>lanes cut 36 to 11</li>"), "{phase}");
    assert!(
        phase.contains("&lt;b&gt;raw&lt;/b&gt;") && !phase.contains("<b>raw"),
        "{phase}"
    );
    // When it last changed: a time the page's script reads as "2m ago".
    assert!(
        phase.contains("<p class=\"meta board-fresh\">Updated <time data-ago datetime=\""),
        "{phase}"
    );
    // A missing slot is its fallback (markdown, muted), or "Not set yet.".
    let ask = slot(&html, "needs-sam");
    assert!(
        ask.contains("<div class=\"md board-md muted\"><p>Nothing needs you.</p>"),
        "{ask}"
    );
    assert!(!ask.contains("board-fresh"), "{ask}");
    let later = between(&html, "data-slot=\"later\"", "</div>");
    assert!(
        later.contains("<p class=\"ou-text muted\">Not set yet.</p>"),
        "{later}"
    );
    // Text stays plain; Markdown draws through the same renderer, unsafe links refused.
    assert!(
        html.contains("<p class=\"ou-text\">**not** markdown</p>"),
        "{html}"
    );
    let note = between(&html, "<div class=\"md board-md\"><p>Read", "</div>");
    assert!(
        note.contains("<strong>the plan</strong> and <code>docs</code>"),
        "{note}"
    );
    assert!(
        note.contains("<a href=\"https://example.com/lanes\">lanes</a>"),
        "{note}"
    );
    assert!(
        note.contains("<a href=\"\">bad</a>") && !note.contains("javascript"),
        "{note}"
    );
    // The slots are a view the query tool (and so a Query or Metric) reads.
    let table = between(&html, "<caption>Slots</caption>", "</table>");
    assert!(
        table.contains("<td>phase</td><td>**Main green.**"),
        "{table}"
    );
    // A cleared slot falls back again.
    set_slot(&f, "phase", Some("")).await;
    let html = board(&f).await;
    assert!(
        slot(&html, "phase").contains("Not set yet."),
        "{}",
        slot(&html, "phase")
    );
}

#[tokio::test]
async fn latest_message_shows_the_newest_from_its_sender_cut_with_a_link() {
    let f = Fixture::new().await;
    set_board(&f, BOARD).await;
    let html = board(&f).await;
    assert!(
        html.contains("<p class=\"ou-text muted board-message\">No message from <span class=\"ou-id\">tests-main</span> yet.</p>"),
        "{html}"
    );
    message(
        &f,
        101,
        "tests-main",
        "Run 1: 11 red of 400 targets".into(),
        "2026-10-05T09:00:00Z",
    )
    .await;
    message(
        &f,
        102,
        "orchestrator",
        "not this one".into(),
        "2026-10-05T09:30:00Z",
    )
    .await;
    message(
        &f,
        103,
        "tests-main",
        format!(
            "Run 2: **3 red** of 400 targets. {}",
            "More detail follows here. ".repeat(20)
        ),
        "2026-10-05T10:00:00Z",
    )
    .await;
    let html = board(&f).await;
    let messages: Vec<&str> = html
        .match_indices("<div class=\"board-message\">")
        .map(|(i, _)| &html[i..i + html[i..].find("</div></div>").unwrap()])
        .collect();
    let [full, short] = messages.as_slice() else {
        panic!("{messages:?}")
    };
    // The newest from tests-main wins, with its time linking to it in its thread.
    let href = format!("/projects/id/{}/thread?thread=tests-main#message-103", f.id);
    assert!(
        full.contains(&format!("<a href=\"{href}\"><time data-ago datetime=\"2026-10-05T10:00:00Z\">2026-10-05 10:00 UTC</time></a>")),
        "{full}"
    );
    assert!(full.contains("<strong>3 red</strong>"), "{full}");
    assert!(
        !full.contains("Run 1") && !full.contains("not this one"),
        "{full}"
    );
    // Cut to 280 characters by default, at a space, with an ellipsis and the whole message a
    // link away.
    let text = between(full, "<div class=\"md board-md\">", "</div>");
    assert!(text.contains("…</p>"), "{text}");
    assert!(text.len() < 400, "{text}");
    assert!(
        full.contains(&format!(
            "<p class=\"meta board-more\"><a href=\"{href}\">The whole message</a></p>"
        )),
        "{full}"
    );
    // chars sets the cut.
    assert!(short.contains("<p>Run 2: **3…</p>"), "{short}");
    // A sender with no message says so.
    assert!(html.contains("No message from <span class=\"ou-id\">nobody-here</span> yet."));
    // A short message is not cut and has no "whole message" link.
    message(
        &f,
        104,
        "tests-main",
        "Run 3: green".into(),
        "2026-10-05T11:00:00Z",
    )
    .await;
    let html = board(&f).await;
    let full = between(&html, "<div class=\"board-message\">", "</div></div>");
    assert!(
        full.contains("<p>Run 3: green</p>") && !full.contains("board-more"),
        "{full}"
    );
}

/// A slot set while the page is open is patched into it by the page's stream.
#[tokio::test]
async fn a_slot_change_patches_the_open_board() {
    use futures_util::StreamExt;
    let f = Fixture::new().await;
    set_board(&f, BOARD).await;
    let base = format!("/projects/id/{}", f.id);
    let (_, html) = f.get(&base).await;
    let mark = "&#34;ver&#34;:&#34;";
    let version = between(&html, mark, "&#34;,")[mark.len()..].to_owned();
    let response = f
        .router()
        .oneshot(
            Request::get(format!(
                "{base}/stream?datastar=%7B%22ver%22%3A%22{version}%22%7D"
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    // The page is current: the stream's first word is only that it is not stale.
    let first = String::from_utf8(body.next().await.unwrap().unwrap().to_vec()).unwrap();
    assert!(!first.contains("Lanes cut to 11"), "{first}");
    set_slot(&f, "phase", Some("Lanes cut to **11**")).await;
    let patched = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let event = String::from_utf8(body.next().await.unwrap().unwrap().to_vec()).unwrap();
            if event.contains("Lanes cut to <strong>11</strong>") {
                return event;
            }
        }
    })
    .await
    .expect("the slot is patched in");
    assert!(patched.contains("data-slot=\"phase\""), "{patched}");
}

#[tokio::test]
async fn settings_list_the_boards_slots() {
    let f = Fixture::new().await;
    set_slot(&f, "phase", Some("**Green**\n\nsoon <now>")).await;
    set_slot(&f, "needs-sam", Some("- approve S35")).await;
    let (_, page) = f.get(&format!("/projects/id/{}/settings", f.id)).await;
    let list = between(&page, "<div id=\"board-slots\">", "</div>");
    let needs = list.find("<code>needs-sam</code>").unwrap();
    let phase = list.find("<code>phase</code>").unwrap();
    assert!(needs < phase, "by key: {list}");
    assert!(
        list.contains("· updated <time data-ago datetime=\""),
        "{list}"
    );
    assert!(
        list.contains(" by orch<p class=\"slot-preview\">- approve S35</p>"),
        "{list}"
    );
    assert!(
        list.contains("<p class=\"slot-preview\">**Green** soon &#60;now&#62;</p>"),
        "{list}"
    );
    let (_, plain) = f.get(&format!("/projects/id/{}/settings", f.plain)).await;
    assert!(plain.contains("<li class=\"setting-help\">No slots set.</li>"));
}

/// The settings preview checks the new components' calls: a bad slot key or message length is
/// a problem on its line, and the board does not draw.
#[tokio::test]
async fn a_bad_slot_or_message_call_is_named_on_its_line() {
    let f = Fixture::new().await;
    let response = f
        .router()
        .oneshot(
            Request::post(format!("/projects/id/{}/settings/board/preview", f.id))
                .body(Body::from(
                    "root = Stack([a, b, c])\na = Slot(\"Bad Key\")\nb = LatestMessage(\"tests-main\", 0)\nc = Markdown(3)",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("This board does not check"), "{html}");
    assert!(
        html.contains("line 2: Slot: &quot;Bad Key&quot; is not a slot key"),
        "{html}"
    );
    assert!(
        html.contains("line 3: LatestMessage: chars must be a whole number from 1 to 4000"),
        "{html}"
    );
    assert!(
        html.contains("line 4: Markdown: text must be string"),
        "{html}"
    );
}
