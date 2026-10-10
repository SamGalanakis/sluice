//! Where a plan page (`views::plan`) draws each thing, for the tests that read one: a step's
//! cell, a unit's row or module, a band's section, each by the region its stream patches.
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
/// A step's cell on the plan: its `li`, whose link carries `id="n-<step>"`.
pub fn cell<'a>(html: &'a str, step: &str) -> &'a str {
    let at = html
        .find(&format!(" id=\"n-{step}\" data-step=\"{step}\""))
        .unwrap_or_else(|| panic!("the cell of {step} in {html}"));
    let start = html[..at].rfind("<li class=\"sc").expect("its cell");
    let end = html[at..].find("</li>").expect("its end") + at;
    &html[start..end]
}
/// A unit's row (Running, Waiting).
pub fn row<'a>(html: &'a str, unit: &str) -> &'a str {
    region(html, &format!("u-{unit}"))
}
/// A stopped unit's module.
pub fn stopped<'a>(html: &'a str, unit: &str) -> &'a str {
    region(html, &format!("s-{unit}"))
}
/// A question's module in For you, by its message.
pub fn question(html: &str, message: i64) -> &str {
    region(html, &format!("q-{message}"))
}
/// A band's section: "plan-asks", "plan-stopped", "plan-running", "plan-waiting", "plan-done",
/// "plan-margin".
pub fn band<'a>(html: &'a str, id: &str) -> &'a str {
    region(html, id)
}
/// The band's summary sentence.
pub fn summary(html: &str) -> &str {
    between(html, "<p class=\"page-line\">", "</p>")
}
/// Where a unit is drawn: the band whose section holds its row or module, "done" in the index.
pub fn place(html: &str, unit: &str) -> &'static str {
    for (id, name) in [
        ("plan-asks", "asks"),
        ("plan-stopped", "stopped"),
        ("plan-running", "running"),
        ("plan-waiting", "waiting"),
        ("plan-margin", "margin"),
    ] {
        if has_region(html, id) {
            let b = band(html, id);
            if b.contains(&format!("data-unit=\"{unit}\"")) {
                return name;
            }
        }
    }
    if done_lines(html, unit) > 0 {
        return "done";
    }
    ""
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
