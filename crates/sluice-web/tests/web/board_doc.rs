//! A board's document (`Doc()`), its Markdown and LatestMessage, each drawn on the server
//! through the dashboard's markdown renderer; a step named by `tag:<tag>`; and the warning a
//! widget draws when the step its data comes from is no longer in the plan.
use crate::board_fixture;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use board_fixture::Fixture;
use serde_json::json;
use sluice_model::ids::{ProjectId, ProjectSelector};
use sluice_store::{RetrySafety, projects};
use tower::ServiceExt;

fn between<'a>(html: &'a str, start: &str, end: &str) -> &'a str {
    let from = html
        .find(start)
        .unwrap_or_else(|| panic!("no {start} in {html}"));
    let to = from + html[from..].find(end).unwrap_or_else(|| panic!("no {end}"));
    &html[from..to]
}

const BOARD: &str = r#"root = Stack([title, doc, plain, note, main, short, nobody])
title = Heading("Release", 1)
doc = Doc("Nothing written yet.")
plain = Text("**not** markdown")
note = Markdown("Read **the plan** and `docs`:\n\n- [lanes](https://example.com/lanes)\n- [bad](javascript:alert(1))")
main = LatestMessage("tests-main")
short = LatestMessage("tests-main", 12)
nobody = LatestMessage("nobody-here")
"#;

async fn set_board(f: &Fixture, project: ProjectId, program: &'static str) -> Vec<String> {
    let checked_board = projects::SetBoard {
        program: Some(program.into()),
        expected_rev: None,
        reason: None,
        author: "orch".into(),
    }
    .check()
    .unwrap();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::board_set(tx, &ProjectSelector::Id(project), checked_board)
        })
        .await
        .unwrap()
        .warnings
}
async fn write_doc(f: &Fixture, markdown: &'static str, author: &'static str) {
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::board_doc_write(
                tx,
                &ProjectSelector::Id(id),
                projects::WriteBoardDoc {
                    markdown: markdown.into(),
                    expected_rev: None,
                    reason: None,
                    author: author.into(),
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
async fn board_of(f: &Fixture, project: ProjectId) -> String {
    let (status, html) = f.get(&format!("/projects/id/{project}")).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    between(&html, "<aside id=\"board-pane\"", "</aside>").to_owned()
}
async fn board(f: &Fixture) -> String {
    board_of(f, f.id).await
}

#[tokio::test]
async fn settings_show_the_document_read_only_with_its_rev_and_last_edit() {
    let f = Fixture::new().await;
    let settings = format!("/projects/id/{}/settings", f.id);
    let (_, page) = f.get(&settings).await;
    let section = between(&page, "<div id=\"board-doc\">", "<!--/board-doc-->");
    assert!(section.contains("Not written yet."), "{section}");
    write_doc(&f, "## Phase\n\nGreen <now>", "orch").await;
    let (_, page) = f.get(&settings).await;
    let section = between(&page, "<div id=\"board-doc\">", "<!--/board-doc-->");
    assert!(
        section.contains("Rev 1 · edited <time data-ago=\""),
        "{section}"
    );
    assert!(section.contains("</time> by orch"), "{section}");
    assert!(
        section.contains("<h4>Phase</h4>") && section.contains("Green &lt;now&gt;"),
        "{section}"
    );
    assert!(!section.contains("<textarea"), "read-only: {section}");
    // A program without a Doc() does not show it, and the page says so.
    set_board(&f, f.id, "root = Units()").await;
    let (_, page) = f.get(&settings).await;
    let section = between(&page, "<div id=\"board-doc\">", "<!--/board-doc-->");
    assert!(
        section.contains("Not shown: the program has no <code>Doc()</code>."),
        "{section}"
    );
}

#[tokio::test]
async fn board_document_messages_and_retired_data() {
    let f = Fixture::new().await;
    {
        set_board(&f, f.id, BOARD).await;
        // Before the first write: the fallback, muted, and no edit line.
        let html = board(&f).await;
        let doc = between(&html, "<div class=\"board-doc\">", "</div></div>");
        assert!(
            doc.contains("<div class=\"md board-md muted\"><p>Nothing written yet.</p>"),
            "{doc}"
        );
        assert!(!doc.contains("board-fresh"), "{doc}");
        write_doc(
        &f,
        "## Phase\n\n**Main green** on FIG-5279. See [the run](https://ci.example/run/7) and [this](javascript:alert(1)).\n\n### Needs you\n\n- approve S35\n- <b>raw</b> html is text\n\n> a quote\n",
        "orchestrator",
    )
    .await;
        let html = board(&f).await;
        let doc = between(&html, "<div class=\"board-doc\">", "</div></div>");
        // Its headings nest under the column's h2 ("Release"): ## is h3, ### h4.
        assert!(doc.contains("<h3>Phase</h3>"), "{doc}");
        assert!(doc.contains("<h4>Needs you</h4>"), "{doc}");
        // its first section shows; the rest folds under "Read more" (board.js opens it where the
        // board has the room), its headings at the levels the whole document gives them
        let fold = doc
            .find("<details class=\"doc-more more-fold\"")
            .expect("a fold");
        assert!(doc.find("<h3>Phase</h3>").unwrap() < fold, "{doc}");
        assert!(doc.find("<h4>Needs you</h4>").unwrap() > fold, "{doc}");
        assert!(
            doc.contains("<span class=\"m-more\">Read more</span>"),
            "{doc}"
        );
        assert!(doc.contains("<strong>Main green</strong>"), "{doc}");
        // An id is kept on one line.
        assert!(
            doc.contains("on <span class=\"ou-id\">FIG-5279.</span>"),
            "{doc}"
        );
        // The written document's line is the board's one time; the head leaves it out.
        let head = between(&html, "<div class=\"board-head\">", "</div>");
        assert!(!head.contains("Updated"), "{head}");
        assert!(
            doc.contains("<a href=\"https://ci.example/run/7\">the run</a>"),
            "{doc}"
        );
        assert!(
            !doc.contains("javascript:") && doc.contains("<a href=\"\">this</a>"),
            "{doc}"
        );
        assert!(doc.contains("<li>approve S35</li>"), "{doc}");
        assert!(doc.contains("<blockquote>"), "{doc}");
        assert!(
            doc.contains("&lt;b&gt;raw&lt;/b&gt;") && !doc.contains("<b>raw"),
            "{doc}"
        );
        // When and by whom: a time the page's script reads as "12m ago".
        assert!(
            doc.contains("<p class=\"meta board-fresh\">Edited <time data-ago=\""),
            "{doc}"
        );
        assert!(doc.contains("</time> by orchestrator</p>"), "{doc}");
        // Text stays plain; Markdown draws through the same renderer, unsafe links refused.
        assert!(
            html.contains("<p class=\"ou-text\">**not** markdown</p>"),
            "{html}"
        );
        let note = between(&html, "<div class=\"md board-md\"><p>Read", "</div>");
        assert!(
            note.contains("<a href=\"https://example.com/lanes\">lanes</a>")
                && note.contains("<a href=\"\">bad</a>"),
            "{note}"
        );
        // A board without a Doc() draws no document.
        set_board(&f, f.id, "root = Units()").await;
        assert!(!board(&f).await.contains("board-doc"));
    }
    {
        set_board(&f, f.id, BOARD).await;
        let html = board(&f).await;
        assert!(
        html.contains("<p class=\"ou-text muted board-none\">No message from <span class=\"ou-id\">tests-main</span> yet.</p>"),
        "{html}"
    );
        message(
            &f,
            101,
            "tests-main",
            "Run 1: 11 red".into(),
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
        let href = format!("/projects/id/{}/thread?thread=tests-main#message-103", f.id);
        assert!(
        full.contains(&format!("<a href=\"{href}\"><time data-ago=\"2026-10-05T10:00:00Z\" datetime=\"2026-10-05T10:00:00Z\" title=\"2026-10-05 10:00 UTC\">")) && full.contains(" ago</time></a>"),
        "{full}"
    );
        assert!(full.contains("<strong>3 red</strong>"), "{full}");
        assert!(
            !full.contains("Run 1") && !full.contains("not this one"),
            "{full}"
        );
        assert!(
            full.contains(&format!(
                "<p class=\"meta board-more\"><a href=\"{href}\">The whole message</a></p>"
            )),
            "{full}"
        );
        assert!(short.contains("<p>Run 2: **3…</p>"), "{short}");
        assert!(html.contains("No message from <span class=\"ou-id\">nobody-here</span> yet."));
        // tests-main is no step of this plan, but nothing says it ever was: no warning.
        assert!(!html.contains("board-stale"), "{html}");
    }
    {
        use futures_util::StreamExt;

        set_board(&f, f.id, BOARD).await;
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
        let first = String::from_utf8(body.next().await.unwrap().unwrap().to_vec()).unwrap();
        assert!(!first.contains("Lanes cut to"), "{first}");
        write_doc(&f, "Lanes cut to **11**", "orch").await;
        let patched = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let event =
                    String::from_utf8(body.next().await.unwrap().unwrap().to_vec()).unwrap();
                if event.contains("Lanes cut to <strong>11</strong>") {
                    return event;
                }
            }
        })
        .await
        .expect("the document is patched in");
        assert!(patched.contains("class=\"board-doc\""), "{patched}");
    }
    {
        let tagged = f
        .project(
            "tagged",
            json!({"steps":{
                "watch-main-tests":{"run":"custom.open","outputs":{"red":"int"},"tags":["main-tests"]},
                "a":{"run":"custom.open","tags":["pair"]},
                "b":{"run":"custom.open","tags":["pair"]}}}),
            &[("watch-main-tests", "failed")],
        )
        .await;
        set_board(
        &f,
        tagged,
        "root = Stack([one, out, two, none])\none = StepStatus(\"tag:main-tests\")\nout = Output(\"tag:main-tests\", \"red\")\ntwo = Output(\"tag:pair\", \"x\")\nnone = LatestMessage(\"tag:nothing\")",
    )
    .await;
        let html = board_of(&f, tagged).await;
        let step = between(&html, "<div class=\"board-step\">", "</div>");
        assert!(
            step.contains(">watch-main-tests<") && step.contains("is-failed"),
            "{step}"
        );
        assert!(
            html.contains("<span class=\"meta\">watch-main-tests/red</span>"),
            "{html}"
        );
        assert!(
        html.contains("<b>Output (line 4)</b><p>2 plan steps carry the tag pair (tag:pair); it must name one</p>"),
        "{html}"
    );
        assert!(
        html.contains(
            "<b>LatestMessage (line 5)</b><p>no plan step carries the tag nothing (tag:nothing)</p>"
        ),
        "{html}"
    );
    }
    {
        // tests-main was a step: a message from one of its runs says so.
        let project = f.id;
        f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,run_id,at) VALUES (104,?1,'tests-main','tests-main','orchestrator','Run 9: 2 red','019a2b3c-4d5e-7f01-8234-56789abcdef9','2026-10-06T10:00:00Z')",
                [project.to_string()],
            )?;
            tx.changed(Some(project), "messages");
            Ok(())
        })
        .await
        .unwrap();
        let warnings = set_board(
        &f,
        f.id,
        "root = Stack([red, latest, ok, status])\nred = Metric(\"Red\", \"SELECT count(*) FROM steps WHERE project_id = ? AND step_id = 'tests-main'\")\nlatest = LatestMessage(\"tests-main\")\nok = Metric(\"Built\", \"SELECT count(*) FROM steps WHERE project_id = ? AND step_id IN ('alpha-build')\")\nstatus = StepStatus(\"tests-main\")",
    )
    .await;
        assert_eq!(
            warnings,
            [
                "line 2: Metric names step `tests-main`, which is not in the plan",
                "line 3: LatestMessage names step `tests-main`, which is not in the plan",
                "line 5: StepStatus names step `tests-main`, which is not in the plan",
            ]
        );
        let html = board(&f).await;
        let line = "<p class=\"board-stale\" role=\"note\">";
        assert_eq!(html.matches(line).count(), 2, "{html}");
        let metric = between(&html, line, "<span class=\"metric-l\">Red</span>");
        assert!(
        metric.contains("<span>Step <code>tests-main</code> is not in the plan; showing its last data.</span></p><div class=\"board-metric\"><span class=\"metric-v\">0</span>"),
        "{metric}"
    );
        let latest = &html[html.rfind(line).unwrap()..];
        assert!(
            latest.contains(
                "is not in the plan; showing its last data.</span></p><div class=\"board-message\">"
            ) && latest.contains("Run 9: 2 red"),
            "{latest}"
        );
        // StepStatus keeps its error box.
        assert!(
            html.contains("<b>StepStatus (line 5)</b><p>the plan has no step tests-main</p>"),
            "{html}"
        );
    }
}

#[tokio::test]
async fn preview_save_warning_and_invalid_stored_program() {
    let f = Fixture::new().await;
    {
        let response = f
        .router()
        .oneshot(
            super::forms::post(
                format!("/projects/id/{}/settings/board/preview", f.id),
                "board-program-form",
                &[(
                    "program",
                    "root = Stack([a, b, c, d, e])\na = Slot(\"phase\")\nb = LatestMessage(\"tests-main\", 0)\nc = Markdown(3)\nd = Doc()\ne = Doc()",
                )],
            ),
        )
        .await
        .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("This board does not check"), "{html}");
        assert!(
        html.contains("line 2: Slot was replaced by Doc: put the slots&#39; text in the board&#39;s document (board_doc_write) and a Doc() where they were"),
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
        assert!(
            html.contains("line 6: a board draws one Doc(), and line 5 already draws it"),
            "{html}"
        );
    }
    {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("op", "save"),
                ("expected_rev", "1"),
                (
                    "program",
                    "root = Stack([a])\na = StepStatus(\"tests-main\")",
                ),
            ])
            .finish();
        let response = f
            .router()
            .oneshot(
                Request::post(format!("/projects/id/{}/settings/board", f.id))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("sluice-form", "board-program-form")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        // its notice says it was saved, and what the program names that the plan lacks
        let feedback = &html[html.rfind("id=\"board-program-form-said\"").unwrap()..];
        assert!(
            feedback.contains("Saved (rev 2).\ndata: elements Warning: line 2: StepStatus names step `tests-main`, which is not in the plan"),
            "{feedback}"
        );
    }
    {
        let id = f.id;
        f.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute(
                    "UPDATE projects SET board=?2 WHERE project_id=?1",
                    [
                        id.to_string(),
                        "root = Stack([phase, lanes])\nphase = Slot(\"phase\")\nlanes = Units()"
                            .into(),
                    ],
                )?;
                tx.changed(Some(id), "board");
                Ok(())
            })
            .await
            .unwrap();
        let html = board(&f).await;
        assert!(html.contains("This board does not check"), "{html}");
        assert!(html.contains("line 2: Slot was replaced by Doc"), "{html}");
    }
}

#[tokio::test]
async fn board_markdown_refuses_unsafe_urls() {
    let f = Fixture::new().await;
    for destination in [
        "javascript:alert",
        "JaVaScRiPt:alert",
        "jav&#x61;script:alert",
        "javascript&colon;alert",
        "java%73cript:alert",
        "%6a%61vascript%3aalert",
        "%256aavascript%253aalert",
        "java&#9;script:alert",
        "java%0ascript:alert",
        "vbscript:run",
        "file:///etc/passwd",
        "data:image/png;base64,abc",
        "data:text/html,x",
        "ftp://host/x",
        "//other.example/x",
        "%2f%2fother.example/x",
    ] {
        let text = format!("[bad]({destination})\n\n![bad]({destination})");
        let program = format!("root = Markdown({})", serde_json::to_string(&text).unwrap());
        let response = f
            .router()
            .oneshot(super::forms::post(
                format!("/projects/id/{}/settings/board/preview", f.id),
                "board-program-form",
                &[("program", &program)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("href=\"\""), "{destination}: {html}");
        assert!(html.contains("src=\"\""), "{destination}: {html}");
    }
    for destination in [
        "/projects/id/1",
        "/percent%25literal",
        "%é/x",
        "../readme",
        "docs/file",
        "#section",
        "?tab=one",
        "http://example.org",
        "HTTPS://example.org",
        "mailto:sam@example.org",
    ] {
        let text = format!("[allowed]({destination})");
        let program = format!("root = Markdown({})", serde_json::to_string(&text).unwrap());
        let response = f
            .router()
            .oneshot(super::forms::post(
                format!("/projects/id/{}/settings/board/preview", f.id),
                "board-program-form",
                &[("program", &program)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(
            html.contains("<a href=\"") && !html.contains("href=\"\""),
            "{destination}: {html}"
        );
    }
}
