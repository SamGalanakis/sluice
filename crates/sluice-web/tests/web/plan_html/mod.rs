//! Where a plan page (`views::plan`) draws each thing, for the tests that read one: an item of
//! Needs you, a unit's row at work and the steps it opens to, a section, each by the region its
//! stream patches or the element that carries its name.
#![allow(dead_code)]

/// `html` from the first `from` to the next `to` after it (or its end).
pub fn between<'a>(html: &'a str, from: &str, to: &str) -> &'a str {
    let start = html
        .find(from)
        .unwrap_or_else(|| panic!("{from} in {html}"));
    let end = html[start..].find(to).map_or(html.len(), |e| start + e);
    &html[start..end]
}
/// A region the page marks (`<!--r:id-->…<!--/r:id-->`), its markers left out.
pub fn region<'a>(html: &'a str, id: &str) -> &'a str {
    let open = format!("<!--r:{id}-->");
    let close = format!("<!--/r:{id}-->");
    let start = html
        .find(&open)
        .unwrap_or_else(|| panic!("region {id} in {html}"))
        + open.len();
    let end = html[start..]
        .find(&close)
        .unwrap_or_else(|| panic!("region {id}'s end"));
    &html[start..start + end]
}
/// Whether the page draws the region `id`.
pub fn has_region(html: &str, id: &str) -> bool {
    html.contains(&format!("<!--r:{id}-->"))
}
/// A step's row in the step view a unit's row opens to (`data-sv="<step>"`).
pub fn cell<'a>(html: &'a str, step: &str) -> &'a str {
    let at = html
        .find(&format!(" data-sv=\"{step}\">"))
        .unwrap_or_else(|| panic!("the row of {step} in {html}"));
    let start = html[..at].rfind("<li class=\"sv-row").expect("its row");
    let end = html[at..].find("</li>").expect("its end") + at;
    &html[start..end]
}
/// A unit's row in At work.
pub fn row<'a>(html: &'a str, unit: &str) -> &'a str {
    region(html, &format!("u-{unit}"))
}
/// Every item of Needs you, whole.
pub fn needs(html: &str) -> Vec<&str> {
    if !has_region(html, "plan-needs") {
        return vec![];
    }
    let well = region(html, "plan-needs");
    let mut out = vec![];
    let mut rest = well;
    while let Some(at) = rest.find("<li id=\"n-") {
        let item = &rest[at..];
        // an item ends where its actions (the × last, a form) close
        let end = item
            .find("</form></span></li>")
            .map_or(item.len(), |e| e + "</form></span></li>".len());
        out.push(&item[..end]);
        rest = &item[end..];
    }
    out
}
/// An item's mark when it is a state's glyph (a stop or a silence), and a silence's.
pub const STATE: &str = "<span class=\"nk\"><span class=\"g g-";
pub const QUIET: &str = "<span class=\"nk\"><span class=\"g g-quiet\"";
/// The item of Needs you about `unit`: its own, or the group that lists it.
pub fn stopped<'a>(html: &'a str, unit: &str) -> &'a str {
    let mark = format!("data-unit=\"{unit}\"");
    needs(html)
        .into_iter()
        .find(|item| item.contains(&mark) && !item.contains(QUIET))
        .unwrap_or_else(|| panic!("an item of Needs you for {unit} in {html}"))
}
/// A question's item of Needs you, by its message.
pub fn question(html: &str, message: i64) -> &str {
    let id = format!("<li id=\"n-ask-{message}\"");
    needs(html)
        .into_iter()
        .find(|item| item.starts_with(&id))
        .unwrap_or_else(|| panic!("the question {message} in {html}"))
}
/// A section: "plan-needs", "plan-finished", "plan-roster", "plan-done".
pub fn band<'a>(html: &'a str, id: &str) -> &'a str {
    region(html, id)
}
/// The head's sentence.
pub fn summary(html: &str) -> &str {
    between(html, "<p class=\"page-line\">", "</p>")
}
/// Where a unit is drawn: "asks" (a question of its in Needs you), "stopped" (a stop in Needs
/// you), "running" or "waiting" (its row at work), "done" (a card of what finished, or a line
/// of the Done index); "" when it is only counted (waiting on earlier work, done long ago).
pub fn place(html: &str, unit: &str) -> &'static str {
    let mark = format!("data-unit=\"{unit}\"");
    for item in needs(html) {
        if item.contains(&mark) {
            if item.contains("nk-ask") {
                return "asks";
            }
            if item.contains(STATE) && !item.contains(QUIET) {
                return "stopped";
            }
        }
    }
    if has_region(html, &format!("u-{unit}")) {
        return if row(html, unit).contains("class=\"ro-u is-wait\"") {
            "waiting"
        } else {
            "running"
        };
    }
    if finished(html, unit) || done_lines(html, unit) > 0 {
        return "done";
    }
    ""
}
/// Whether a card of Finished since you looked leads to `unit` (its page, or a loose step's).
pub fn finished(html: &str, unit: &str) -> bool {
    has_region(html, "plan-finished") && {
        let cards = band(html, "plan-finished");
        ["units", "steps"]
            .iter()
            .any(|kind| cards.contains(&format!("/{kind}/{unit}\">")))
    }
}
/// How many lines of the Done index lead to `unit` (its unit's page, or a loose step's own).
pub fn done_lines(html: &str, unit: &str) -> usize {
    if !has_region(html, "plan-done") {
        return 0;
    }
    let done = band(html, "plan-done");
    ["units", "steps"]
        .iter()
        .map(|kind| {
            done.matches(&format!("/{kind}/{unit}\"><span class=\"pl-at\">"))
                .count()
        })
        .sum()
}
/// The page `html` as it reads with each row of At work opened: each row's drilldown asked for
/// as a browser asks when the row opens (its `data-on:toggle`'s `@get`), drawn in its place.
pub async fn opened(router: &axum::Router, mut html: String) -> String {
    use tower::ServiceExt;
    const ASK: &str = "data-on:toggle=\"el.open &amp;&amp; @get('";
    let asks: Vec<String> = html
        .match_indices(ASK)
        .map(|(at, _)| {
            let src = &html[at + ASK.len()..];
            src[..src.find('\'').expect("its end")].replace("&amp;", "&")
        })
        .collect();
    for src in asks {
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::get(&src)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK, "{src}");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("data: mode replace"), "{body}");
        let drawn = body
            .lines()
            .filter_map(|l| l.strip_prefix("data: elements "))
            .collect::<Vec<_>>()
            .join("\n");
        let id = drawn["<div id=\"".len()..]
            .split('"')
            .next()
            .expect("its id")
            .to_owned();
        let empty = format!("<div id=\"{id}\" class=\"ro-drill\" data-ignore-morph></div>");
        assert!(html.contains(&empty), "{id}'s empty drilldown");
        html = html.replacen(&empty, &drawn, 1);
    }
    html
}
