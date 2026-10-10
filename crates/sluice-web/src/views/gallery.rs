//! `/_ui`: the kit's gallery. Every shared part of the dashboard (`views::ui`, the field rows of
//! `templates/kit.html`, the conversation of `views::threads`) drawn in every state with
//! real-looking data, each in the house pair's light and dark side by side: the component
//! system's documentation (DESIGN.md, Components). Nothing on it is read from the store.
use super::step::FieldView;
use super::threads::{Build, Composer, Conversation, MessageItem, Who};
use super::ui::{self, Confirm, Shown, StepRef, Tab};
use super::{DashboardState, NavView, PageRegistration, TrustedHtml, Viewer};
use askama::Template;
use axum::{
    Router,
    extract::State,
    http::HeaderMap,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use sluice_model::{
    commands::{Message, MessageVerb, QuestionState},
    ids::{MessageId, ProjectId},
    naming::StepName,
};
use std::collections::{BTreeMap, BTreeSet};

/// One part of the kit as the gallery shows it: its name, when a page uses it, and its HTML in
/// each theme (drawn twice, its ids kept apart); a part with no dark copy (the components'
/// table) is drawn once, across the column.
pub struct Part {
    pub name: &'static str,
    /// Its two themes one above the other at every width: a part that needs the grid's room.
    pub wide: bool,
    /// What the owner sees it as and when a page shows it.
    pub about: &'static str,
    /// How a page draws it, for whoever builds one (`views::ui`, its component).
    pub built: &'static str,
    pub light: TrustedHtml,
    pub dark: TrustedHtml,
}
#[derive(Template)]
#[template(path = "gallery.html")]
struct GalleryTemplate<'a> {
    parts: &'a [Part],
}
#[derive(Template)]
#[template(
    source = "{% import \"kit.html\" as kit %}<dl class=\"fields\">{% for (field, same) in fields %}{% call kit::field(field, project, unset, 3, id, same) %}{% endcall %}{% endfor %}</dl>",
    ext = "html"
)]
struct Fields<'a> {
    fields: &'a [(FieldView, &'a str)],
    id: &'a str,
    project: &'a ProjectId,
    unset: &'a str,
}

/// Words with their `code` spans drawn as code: "Built with `ui::tag`".
pub fn code_words(text: &str) -> TrustedHtml {
    TrustedHtml::owned(
        text.split('`')
            .enumerate()
            .map(|(i, part)| {
                if i % 2 == 1 {
                    format!("<code>{}</code>", ui::esc(part))
                } else {
                    ui::esc(part)
                }
            })
            .collect(),
    )
}
/// The gallery's one project id: a fixed one, so the page draws the same every time.
fn project() -> ProjectId {
    "01a10513-16c5-7742-a8a2-42b9f1812a08"
        .parse()
        .expect("a project id")
}
fn step_ref(id: &str, stage: &str, title: &str) -> StepRef {
    StepRef::new(
        id,
        Some(&StepName {
            title: title.into(),
            stage: stage.into(),
            ..Default::default()
        }),
    )
}
fn statuses() -> TrustedHtml {
    let rows: String = Shown::ALL
        .into_iter()
        .map(|s| format!("<li>{}<span class=\"meta\">{}</span></li>", ui::status(s), ui::esc(s.spec().help)))
        .collect();
    TrustedHtml::owned(format!("<ul class=\"gal-states\">{rows}</ul>"))
}
fn tags() -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<p class=\"gal-row\">{}{}{}{}{}{}{}</p>",
        ui::tag("unit: a-7", "", None),
        ui::tag("Answered", "muted", None),
        ui::tag("2 awaiting reply", "attn", None),
        ui::tag("Awaiting your reply", "ask", None),
        ui::tag("live", "live", Some(ui::mark(Shown::Running))),
        ui::tag("failed", "", Some(ui::mark(Shown::Failed))),
        ui::tag_link("#", "Its unit", "", None),
    ))
}
fn buttons() -> TrustedHtml {
    TrustedHtml::owned(
        "<p class=\"gal-row\"><button class=\"primary\" type=\"button\">Retry</button><button type=\"button\">Pause</button><button class=\"danger\" type=\"button\">Delete almanac</button><button type=\"button\" disabled>Unpause</button></p>"
            .into(),
    )
}
fn tabs(prefix: &str) -> TrustedHtml {
    let set = [
        Tab::new("overview", "Overview"),
        Tab::new("activity", "Activity").counted("8", "8 turns"),
        Tab::new("messages", "Messages").counted("15", "15 messages"),
        Tab::new("inputs", "Inputs").counted("6", ""),
        Tab::new("outputs", "Outputs").counted("3/7", "3 of 7 set"),
        Tab::new("runs", "Runs").counted("2", ""),
    ];
    let words = [
        "What it is doing now, why it stopped, what it waits on and its key output.",
        "One row per turn of its agent, each opening to its tool calls.",
        "Its conversation: its own thread and what it sent or was sent elsewhere.",
        "Each value and where it came from.",
        "Each value and which run set it; one repeated is said once.",
        "Its unit's timeline, then one row per run.",
    ];
    let mut html = ui::tabs_open(prefix, "Example", &set, "overview", false).0;
    for (tab, said) in set.iter().zip(words) {
        html.push_str(ui::panel_open(prefix, tab.key, tab.label, 3, tab.key == "overview", false).as_str());
        html.push_str(&format!("<p>{}</p>", ui::esc(said)));
        html.push_str(ui::panel_close(prefix, tab.key, false).as_str());
    }
    html.push_str(ui::tabs_close().as_str());
    TrustedHtml::owned(html)
}
fn sections() -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<section class=\"d-sec\">{}<p>A section: a head (a count after it, left out at zero), then its rows.</p></section><div class=\"card gal-card\"><p class=\"m-title\">A card</p><p class=\"meta\">A region on the card colour, its hairline and the region's corner. Never a card inside a card.</p></div>",
        ui::head(3, "Queued", Some(2), "")
    ))
}
fn fields(prefix: &str, project: &ProjectId) -> Result<TrustedHtml, askama::Error> {
    let summary = "Drafted the spring guide's shorebird entries: fourteen species, each with a range map, a photograph credit and a note on when to look. The two disputed sightings are marked for review.";
    let mut by_run = FieldView::new("evidence", "string", "", Some(&"Style check: 14 entries, 0 issues".into()), "");
    by_run.set_by = "Run 1".into();
    let outputs = vec![
        (FieldView::new("summary", "string", "What changed", Some(&summary.into()), ""), ""),
        (FieldView::new("final", "string", "", Some(&summary.into()), ""), "summary"),
        (FieldView::new("ready", "boolean", "", Some(&true.into()), ""), ""),
        (FieldView::new("tests", "integer", "", Some(&14.into()), ""), ""),
        (by_run, ""),
        (FieldView::new("model", "object", "", Some(&serde_json::json!({"type": "normal", "model": "sol", "effort": "high"})), ""), ""),
        (FieldView::new("notes", "string", "", Some(&serde_json::Value::Null), ""), ""),
    ];
    let inputs = vec![
        (FieldView::new("brief", "string", "", Some(&serde_json::json!({"file": "briefs/a-7.md"})), "A file, read when the run starts"), ""),
        (FieldView::new("edition", "string", "", Some(&"2027 spring".into()), "Its default"), ""),
        (FieldView::new("folder", "string", "", Some(&"/srv/almanac/drafts/a-7".into()), "a-7-gather/folder"), ""),
        (FieldView::new("section", "string", "", Some(&"shorebirds".into()), "section"), ""),
        (FieldView::new("review", "string", "", None, "a-7-review/verdict"), ""),
    ];
    Ok(TrustedHtml::owned(format!(
        "<p class=\"meta gal-cap\">Outputs: which run set one, one repeated said once</p>{}<p class=\"f-unset\"><span class=\"quiet\">2 outputs not set yet:</span> <span class=\"f-uname\">verdict</span>, <span class=\"f-uname\">pr</span></p><p class=\"meta gal-cap\">Inputs: where each value came from</p>{}",
        TrustedHtml::from_template(&Fields { fields: &outputs, id: &format!("{prefix}out"), project, unset: "Not set yet." })?,
        TrustedHtml::from_template(&Fields { fields: &inputs, id: &format!("{prefix}in"), project, unset: "No value yet." })?,
    )))
}
fn folds() -> TrustedHtml {
    let long = crate::markdown::render(
        "Three entries disagree with the regional checklist:\n\n- `entries/red-knot.md`: the spring range ends a week earlier in the checklist.\n- `entries/sanderling.md`: the photograph credit names a different archive.\n- `entries/whimbrel.md`: a-8 moved the shared range maps under `maps/coast/`.\n\nKeep both range notes, take the checklist's dates and say so in the entry, and point the whimbrel's map at its new folder.\n\nThen run the style check again before you submit.",
    );
    let short = crate::markdown::render("Revised; the style check passes.");
    TrustedHtml::owned(format!(
        "<p class=\"meta gal-cap\">A long text, cut</p>{}<p class=\"meta gal-cap\">One that fits its first lines: no Show all (with script)</p>{}<p class=\"meta gal-cap\">A list's sentence that opens to it</p>{}<p>a-5-draft · a-6-draft · a-7-draft · a-8-draft · a-9-draft · a-10-draft</p>{}<p class=\"meta gal-cap\">More and Less, Read less at its end</p>{}<div class=\"md\"><p>The rest of the document: what each section covers, and the order they go out in.</p></div>{}",
        ui::fold("md", &long),
        ui::fold("md", &short),
        ui::more_open("gates gal-fold", "6 steps: 5 done, 1 running", "", "", false),
        ui::more_close(""),
        ui::more_open("doc-more gal-fold", "Read more", "Read less", "", false),
        ui::more_close("Read less"),
    ))
}
fn menus() -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"gal-row gal-menus\"><div class=\"board-tools\">{}<details class=\"tool-more\" data-preserve-attr=\"open\"><summary aria-label=\"More ways to see the plan\" title=\"More ways to see the plan\">{}</summary><div class=\"menu\"><a href=\"#\">The plan as Mermaid text</a><a href=\"#\">Its units as JSON</a></div></details>{}</div><p class=\"meta\">The plan's More: Escape or a click elsewhere closes it, ArrowDown goes into it.</p></div>",
        ui::menu_open(),
        super::icons::icon(super::icons::Icon::Ellipsis, 16, ""),
        ui::menu_close(),
    ))
}
fn copies() -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<dl class=\"facts gal-copies\"><div><dt>Step id</dt><dd>{}</dd></div><div><dt>Run id</dt><dd>{}</dd></div><div><dt>Release</dt><dd>{}</dd></div></dl>",
        ui::copy("a-7-draft", "Copy step id"),
        ui::copy_with("01JA3V7KQ2W8M5T1X9C4ZB6NHE", "Copy run id", "a-id"),
        ui::copy("5f3c2e1a9b7d", "Copy release"),
    ))
}
fn toggles() -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"d-sec-h tp-top\"><p class=\"meta\">From run 2 · value types shown or not, on every page at once</p>{}</div>",
        ui::types_toggle()
    ))
}
fn searches(prefix: &str) -> TrustedHtml {
    let items: String = [
        ("agent.claude", "Runs Claude Code on a prompt"),
        ("agent.codex", "Runs Codex on a prompt"),
        ("files.publish", "Publishes a page to the site"),
        ("core.external", "Work done outside sluice"),
    ]
    .iter()
    .map(|(name, doc)| {
        format!(
            "<li data-find=\"{} {}\"><code>{}</code> <span class=\"meta\">{}</span></li>",
            ui::esc(name),
            ui::esc(&doc.to_lowercase()),
            ui::esc(name),
            ui::esc(doc)
        )
    })
    .collect();
    TrustedHtml::owned(format!(
        "{}{}<p class=\"meta\" role=\"status\" hidden data-find-status data-none=\"No function matches\" data-one=\"function matches\" data-many=\"functions match\"></p><ul class=\"gal-list\" data-find-group>{items}</ul>{}",
        ui::search_open("filter", ""),
        ui::search_field(&format!("{prefix}find"), "Find a function", "Find functions by name or description"),
        ui::search_close(),
    ))
}
fn banners() -> TrustedHtml {
    TrustedHtml::owned(format!(
        "{}{}{}",
        ui::banner("stream", "", "Updates paused. Reconnecting…", false),
        ui::banner("stream", "", "Updates stopped at 14:02.", false),
        ui::banner("release", "", "", false),
    ))
}
fn splitters(prefix: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"gal-split\" id=\"{prefix}split\"><div class=\"gal-plan\">Plan</div>{}<div class=\"gal-board\" id=\"{prefix}split-board\">Board</div></div>",
        ui::splitter_for(&format!("{prefix}split"), &format!("{prefix}split-board"), &format!("sluice.gallery.{prefix}split"), 96, 120),
    ))
}
fn components() -> TrustedHtml {
    let rows: String = ui::COMPONENTS
        .iter()
        .map(|c| {
            let props = c
                .props
                .iter()
                .map(|(name, ty)| format!("<code>{}</code> {}", ui::esc(name), ui::esc(ty)))
                .collect::<Vec<_>>()
                .join(", ");
            let events = c
                .events
                .iter()
                .map(|e| format!("<code>{}</code>", ui::esc(e)))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "<tr><th scope=\"row\"><code>{}</code><span class=\"meta\">{}</span></th><td>{}</td><td>{}</td><td>{}</td></tr>",
                ui::esc(c.tag),
                ui::esc(c.script),
                ui::esc(c.does),
                if props.is_empty() { "none".into() } else { props },
                if events.is_empty() { "none".into() } else { events },
            )
        })
        .collect();
    TrustedHtml::owned(format!(
        "<div class=\"gal-table\"><table class=\"gal-components\"><thead><tr><th scope=\"col\">Component</th><th scope=\"col\">What it does</th><th scope=\"col\">Props</th><th scope=\"col\">Events</th></tr></thead><tbody>{rows}</tbody></table></div>"
    ))
}
fn message(id: i64, from: &str, to: &str, at: &str, body: &str, verb: MessageVerb) -> Message {
    Message {
        id: MessageId(id),
        verb,
        from: from.into(),
        to: Some(to.into()),
        thread: "step-a-7-draft".into(),
        body: body.into(),
        title: None,
        ui: None,
        input: None,
        data: None,
        run: None,
        at: at.into(),
        to_message: None,
        answer: None,
        state: None,
        answered_by: None,
    }
}
fn conversation(prefix: &str, project: &ProjectId) -> Result<TrustedHtml, askama::Error> {
    let steps: BTreeMap<String, StepRef> = [
        step_ref("a-7-draft", "draft", "Shorebirds: the spring guide's entries"),
        step_ref("a-8-draft", "draft", "Coastal range maps for the spring guide"),
        step_ref("a-6-draft", "draft", "Waders: the autumn guide's entries"),
    ]
    .into_iter()
    .map(|s| (s.id.clone(), s))
    .collect();
    let item = |m: Message, state: &str| {
        let long = m.body.chars().count() > 700 || m.body.lines().count() > 14;
        MessageItem {
            project: *project,
            project_name: "almanac".into(),
            body: crate::markdown::render(&m.body),
            state: state.into(),
            stopped: String::new(),
            answer_json: String::new(),
            long,
            from_who: Who::of(Some(&m.from), None, &steps),
            to_who: Who::of(m.to.as_deref(), None, &steps),
            answered_at: String::new(),
            message: m,
        }
    };
    let mut ask = message(152918, "a-8-draft", "a-7-draft", "2026-10-08T19:34:27Z", "a-8 owns the coastal range maps: `maps/coast/` and `maps/legend.md`. Which of those do you touch, and where?", MessageVerb::Ask);
    ask.state = Some(QuestionState::Answered);
    let mut reply = message(152959, "a-7-draft", "a-8-draft", "2026-10-08T19:35:25Z", "Only `maps/coast/whimbrel.svg`: its spring range gets a second band. Nothing in your legend moves.", MessageVerb::Reply);
    reply.to_message = Some(MessageId(152918));
    let mut yours = message(157590, "a-7-draft", "owner", "2026-10-09T08:12:40Z", "The checklist's spring dates changed under me. Use the new dates in this draft, or wait for a-11 to publish the revised checklist first?", MessageVerb::Ask);
    yours.state = Some(QuestionState::Open);
    let long = "the regional checklist disagrees with entries/red-knot.md and with the range maps.\n\n".to_owned()
        + &"Keep both range notes; take the checklist's dates and say so in each entry. ".repeat(12);
    let items = vec![
        item(message(152885, "orchestrator", "a-7-draft", "2026-10-08T19:33:41Z", "The sibling entries in your brief are now concrete: the autumn waders are a-6-draft.", MessageVerb::Say), "note"),
        item(ask, "answered"),
        item(reply, "note"),
        item(message(153341, "a-8-draft", "a-7-draft", "2026-10-08T19:48:08Z", "The coastal maps are under `maps/coast/` now.", MessageVerb::Say), "note"),
        item(message(156475, "orchestrator", "a-7-draft", "2026-10-08T21:29:57Z", &long, MessageVerb::Say), "note"),
        item(message(157531, "a-7-draft", "orchestrator", "2026-10-09T08:10:02Z", "Revised; the style check passes. Submitting after the checklist question.", MessageVerb::Say), "note"),
        item(yours, "open"),
        item(message(157611, "owner", "a-7-draft", "2026-10-09T08:20:13Z", "Wait for a-11; it goes out within the hour.", MessageVerb::Say), "note"),
    ];
    let unread: BTreeSet<i64> = [157590].into();
    let here = |m: &MessageItem| format!("#message-{}", m.id());
    Conversation::build(
        Build {
            project: *project,
            subject: Some("a-7-draft"),
            steps: &steps,
            unread: &unread,
            most: None,
            excerpt: false,
            href: &here,
        },
        items,
    )
    .with_head("#part-8".into())
    .with_composer(Some(Composer {
        project: *project,
        to: "a-7-draft".into(),
        label: "Message to draft · Shorebirds: the spring guide's entries".into(),
        note: String::new(),
    }))
    // the two themes' copies keep their ids apart
    .with_slot(prefix)
    .html()
}
fn empties() -> TrustedHtml {
    TrustedHtml::owned(format!(
        "{}{}",
        ui::empty("Nothing is waiting on you."),
        ui::empty_with(
            "No run of it is kept: it ran under an earlier release, or its runs were cleared since.",
            &TrustedHtml::owned("<a href=\"#\">Search the log for it</a>".into())
        )
    ))
}
fn dialog() -> TrustedHtml {
    let cancel = Confirm {
        opener: "Cancel".into(),
        title: "Cancel draft · Shorebirds: the spring guide's entries".into(),
        id: "a-7-draft".into(),
        action: "#".into(),
        copy: "It has been running for 2h 14m. Cancelling stops that run; Retry starts it over.".into(),
        reason: Some("Why stop this run?"),
        confirm: "Cancel the run".into(),
        keep: "Keep running",
        danger: true,
        ..Default::default()
    };
    let delete = Confirm {
        opener: "Delete project".into(),
        title: "Delete almanac?".into(),
        action: "#".into(),
        copy: "This permanently removes the plan, messages, history, functions, secrets and artifacts. This cannot be undone.".into(),
        confirm: "Delete almanac".into(),
        keep: "Keep it",
        danger: true,
        disabled: true,
        ..Default::default()
    };
    TrustedHtml::owned(format!(
        "<div class=\"gal-row\">{}{}</div><p class=\"meta\">Delete project is dimmed while something blocks it.</p>",
        cancel.html(),
        delete.html()
    ))
}
fn notices() -> TrustedHtml {
    ui::notice("Nothing was done: this step changed since the page drew it, and it is running now. Look again, then retry. What you wrote is kept in its box: Retry sends it.")
}
/// The keys' list as display preferences draws it (its `sluice-keys` host is the page's own:
/// one per page).
fn keys() -> TrustedHtml {
    TrustedHtml::owned("<div class=\"keys gal-keys\"><p class=\"menu-label\">Keys</p><dl class=\"keys-list\"><div><dt><kbd>/</kbd></dt><dd>Find (plan, log, functions)</dd></div><div><dt><kbd>g</kbd> <kbd>h</kbd></dt><dd><a href=\"#\">All projects</a></dd></div><div><dt><kbd>g</kbd> <kbd>p</kbd></dt><dd><a href=\"#\">Plan</a></dd></div><div><dt><kbd>g</kbd> <kbd>m</kbd></dt><dd><a href=\"#\">Messages</a></dd></div><div><dt><kbd>g</kbd> <kbd>l</kbd></dt><dd><a href=\"#\">Log</a></dd></div><div><dt><kbd>g</kbd> <kbd>i</kbd></dt><dd><a href=\"#\">Inbox</a></dd></div><div><dt><kbd>?</kbd></dt><dd>These keys</dd></div></dl><p class=\"keys-note\">None while typing in a field.</p></div>".into())
}

// ---- the Synthesis kit, on an invented project (almanac: a team writing a field guide) -------

/// An instant `seconds` before now, as the store writes one.
fn before(seconds: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    super::rfc3339(now.saturating_sub(seconds))
}
/// The `article` recipe's stages for one unit: done, done, running past its usual time.
fn article(draft: Option<Shown>, review: Option<Shown>, publish: Option<Shown>) -> Vec<ui::Stage> {
    let cell = |name: &str, shown: Option<Shown>, took: f64| {
        let stage = ui::Stage::new(name, shown).href("#");
        match ui::Cell::of(shown) {
            ui::Cell::Run => stage.running_since(before(took as u64), took),
            ui::Cell::Done | ui::Cell::Look => stage.took(took),
            ui::Cell::Empty => stage,
        }
    };
    vec![
        cell("draft", draft, 2_460.0),
        cell("review", review, 3_720.0),
        cell("publish", publish, 95.0),
    ]
}
fn grid_part(prefix: &str) -> TrustedHtml {
    let id = format!("{prefix}grid");
    let module = |span: u8, wide: ui::Wide, words: &str| {
        format!(
            "{}<p>{}</p>{}",
            ui::module_open_wide(span, ui::Swell::Plain, words, wide),
            ui::esc(words),
            ui::module_close()
        )
    };
    TrustedHtml::owned(format!(
        "<div class=\"gal-row\">{}</div>{}{}{}{}{}{}{}{}",
        ui::grid_toggle(&id),
        ui::grid_open(&id, 12),
        module(6, ui::Wide::Keep, "Six columns"),
        module(4, ui::Wide::Keep, "Four"),
        module(2, ui::Wide::Keep, "Two"),
        module(12, ui::Wide::Keep, "Twelve"),
        module(12, ui::Wide::Halve, "Twelve, halved on a wide sheet"),
        module(12, ui::Wide::Halve, "Twelve, halved on a wide sheet"),
        ui::grid_close(),
    ))
}
fn modules_part() -> TrustedHtml {
    let ask = format!(
        "{open}<p class=\"mod-k\">{q}<b>Question for you</b><span class=\"quiet\">from a-7-draft, 25m ago</span></p><p class=\"mod-t\">Shorebirds: the spring guide's entries</p><p class=\"mod-meta\"><b>a-7</b> · article · running 41m</p>{strip}<div class=\"mod-body\"><p>The checklist's spring dates changed under me. Use the new dates in this draft, or wait for a-11 to publish the revised checklist first?</p></div><div class=\"mod-actions\"><a class=\"primary\" href=\"#\">Answer</a><a href=\"#\">Open a-7-draft</a></div>{close}",
        open = ui::module_open(6, ui::Swell::Ask, "a-7, question for you"),
        q = super::icons::icon(super::icons::Icon::MessageSquare, 16, "ask-i"),
        strip = ui::stage_strip("Stages of a-7", &article(Some(Shown::Running), None, None)),
        close = ui::module_close(),
    );
    let look = format!(
        "{open}<p class=\"mod-k\">{g}<b>failed</b><span class=\"quiet\">review, run 1, after 18m</span></p><p class=\"mod-t\">Waders: the autumn guide's entries</p><p class=\"mod-meta\"><b>a-6</b> · article {marks}</p><div class=\"mod-body\"><p>The style check found two entries without a photograph credit.</p></div><div class=\"mod-actions\"><button class=\"primary\" type=\"button\">Retry with feedback</button><button type=\"button\">Retry</button></div>{close}",
        open = ui::module_open(4, ui::Swell::Look, "a-6, failed"),
        g = ui::mark(Shown::Failed),
        marks = ui::stage_marks("Stages of a-6", &article(Some(Shown::Succeeded), Some(Shown::Failed), None)),
        close = ui::module_close(),
    );
    TrustedHtml::owned(format!(
        "{}{ask}{look}{}",
        ui::grid_open("", 10),
        ui::grid_close()
    ))
}
fn heads_part() -> TrustedHtml {
    let row = |name: &str, title: &str, stages: Vec<ui::Stage>| {
        format!(
            "{}<p class=\"mod-meta\"><b>{}</b> · {}</p>{}{}",
            ui::strip_row_open(6),
            ui::esc(name),
            ui::esc(title),
            ui::stage_strip(&format!("Stages of {name}"), &stages),
            ui::strip_row_close()
        )
    };
    TrustedHtml::owned(format!(
        "{}<p class=\"meta gal-cap\">Over a table of strips: each stage's name over its column, each row's stages under them</p><div class=\"gal-strips\">{}{}{}</div>",
        ui::section_head("", "Stopped", "1 failed · 1 cancelled"),
        ui::strip_head("", "Running", "Quiet first, then the longest.", 6, &["draft", "review", "publish"]),
        row("a-12", "Terns: the spring guide's entries", article(Some(Shown::Succeeded), Some(Shown::Running), None)),
        row("a-9", "Gannets: the spring guide's entries", article(Some(Shown::Succeeded), Some(Shown::Quiet), None)),
    ))
}
fn strips_part() -> TrustedHtml {
    let quiet = vec![
        ui::Stage::new("draft", Some(Shown::Succeeded)).took(2_460.0),
        ui::Stage::new("review", Some(Shown::Quiet)).took(3_180.0),
        ui::Stage::new("publish", None),
    ];
    let over = vec![
        ui::Stage::new("draft", Some(Shown::Running)).running_since(before(5_300), 5_300.0).over(2.14),
        ui::Stage::gap("then review and publish", 2),
    ];
    let scan = vec![ui::Stage::new("scan", Some(Shown::Succeeded)).took(42.0)];
    let held = vec![
        ui::Stage::new("draft", Some(Shown::Paused)),
        ui::Stage::new("review", Some(Shown::Blocked)),
        ui::Stage::new("publish", Some(Shown::Pending)),
    ];
    TrustedHtml::owned(format!(
        "<p class=\"meta gal-cap\">Done, running past its usual time ({}), not reached</p>{}<p class=\"meta gal-cap\">A run gone quiet; a note across the stages not reached</p>{}{}<p class=\"meta gal-cap\">Not reached, each saying why; a one-step recipe</p>{}{}<p class=\"meta gal-cap\">Small, in a line of words</p><p class=\"mod-meta\"><b>a-6</b> · article {}</p>",
        ui::overrun(2.14),
        ui::stage_strip("Stages of a-12", &article(Some(Shown::Succeeded), Some(Shown::Running), None)),
        ui::stage_strip("Stages of a-9", &quiet),
        ui::stage_strip("Stages of a-14", &over),
        ui::stage_strip("Stages of a-15", &held),
        ui::stage_strip("Stages of s-4", &scan),
        ui::stage_marks("Stages of a-6", &article(Some(Shown::Succeeded), Some(Shown::Failed), None)),
    ))
}
fn summary_part() -> TrustedHtml {
    let unit = |name: &str, recipe: &str, shown, over, quiet| ui::UnitFact {
        name: name.into(),
        recipe: recipe.into(),
        shown: Some(shown),
        over,
        quiet,
    };
    let units = [
        unit("a-6", "article", Shown::Failed, None, None),
        unit("a-10", "article", Shown::Cancelled, None, None),
        unit("a-7", "article", Shown::Running, None, None),
        unit("a-12", "article", Shown::Running, Some(2.14), None),
        unit("s-3", "scan", Shown::Quiet, None, Some(3_180.0)),
        unit("a-13", "article", Shown::Pending, None, None),
        unit("a-14", "article", Shown::Pending, None, None),
        unit("a-1", "article", Shown::Succeeded, None, None),
        unit("a-2", "article", Shown::Succeeded, None, None),
        unit("s-1", "scan", Shown::Succeeded, None, None),
    ];
    let last = before(3_420);
    TrustedHtml::owned(format!(
        "<p class=\"gal-summary\">{}</p>",
        ui::summary_sentence(&ui::Summary {
            asks: 1,
            ask_href: "#",
            units: &units,
            last_done: &last,
            noun: ("unit", "units"),
        })
    ))
}
fn band_part(prefix: &str) -> TrustedHtml {
    let finished = [
        ("a-5", "Plovers: the spring guide's entries", 3_420, 3_840.0, 2),
        ("s-2", "Scan the photograph archive for credits", 3_900, 312.0, 1),
        ("a-4", "Herons: the autumn guide's entries", 6_100, 2_520.0, 1),
        ("a-3", "Gulls: the winter guide's entries", 8_800, 1_980.0, 1),
        ("s-1", "Scan the checklist for renamed species", 10_400, 95.0, 1),
    ]
    .map(|(name, title, ago, took, runs)| ui::Finished {
        name: name.into(),
        title: title.into(),
        href: "#".into(),
        at: before(ago),
        took,
        runs,
        shown: Some(Shown::Succeeded),
    });
    TrustedHtml::owned(format!(
        "<div class=\"gal-band\">{}{}</div>",
        ui::band_head(
            "almanac",
            &TrustedHtml::owned("<p>The field guide's spring and autumn editions, written a section at a time and checked against the regional checklist.</p>".into()),
            &TrustedHtml::owned("<a class=\"ask\" href=\"#\">1 question for you</a>. 1 failed, 1 cancelled. 2 article and 1 scan at work: s-3 quiet for 53m, a-12 at 2.1× its usual time. 2 waiting. 3 of 10 units done; the last finished 57m ago.".into()),
        ),
        ui::recent_strip(&format!("{prefix}rf"), "Newest first. 5 in the last three hours.", &finished),
    ))
}
fn margin_part() -> TrustedHtml {
    let run = ui::LongRun {
        name: "survey".into(),
        title: "Survey the coast photographs".into(),
        doc: "Checks every photograph in the archive against the checklist, a folder at a time.".into(),
        href: "#".into(),
        run: 3,
        since: before(2 * 86_400 + 4 * 3_600),
        fields: vec![
            ("checked".into(), serde_json::json!(1240)),
            ("remaining".into(), serde_json::json!(310)),
            ("folder".into(), serde_json::json!("coast/2019-05")),
            ("last mismatch".into(), serde_json::json!("A sanderling filed as a dunlin.")),
        ],
        at: before(240),
    };
    TrustedHtml::owned(format!(
        "<div class=\"gal-margin\">{}</div>",
        ui::margin_module(&run)
    ))
}
fn trace_part(prefix: &str) -> TrustedHtml {
    let trace = ui::Trace::new([("a-13", "a-12"), ("a-14", "a-13"), ("a-12", "s-3"), ("a-15", "a-12")]);
    let unit = |name: &str, recipe: &str, title: &str, stages: Vec<ui::Stage>| {
        format!(
            "<div class=\"mod rail-slot\"{attrs}>{rail}{open}<span class=\"mod-meta\">{role}<b>{name}</b> · {recipe}</span><span class=\"mod-t\">{title}</span>{close}{strip}{more}<p class=\"meta\">Its steps, its last message and its buttons open here.</p>{more_end}</div>",
            attrs = trace.attrs(name),
            rail = ui::rail(),
            open = ui::trace_button_open(),
            role = ui::trace_role(),
            title = ui::esc(title),
            strip = ui::stage_strip(&format!("Stages of {name}"), &stages),
            close = ui::trace_button_close(),
            more = ui::trace_more_open(),
            more_end = ui::trace_more_close(),
        )
    };
    let head = |name: &str| {
        format!(
            "<div class=\"rail-slot\">{}{}</div>",
            ui::rail(),
            ui::section_head("", name, "")
        )
    };
    let scan = vec![ui::Stage::new("scan", Some(Shown::Quiet)).took(3_180.0)];
    let units = [
        head("Running"),
        unit("s-3", "scan", "Scan the coast photographs for credits", scan),
        unit("a-12", "article", "Terns: the spring guide's entries", article(Some(Shown::Running), None, None)),
        head("Waiting"),
        unit("a-13", "article", "Skuas: the spring guide's entries", article(None, None, None)),
        unit("a-14", "article", "Auks: the spring guide's entries", article(None, None, None)),
        unit("a-15", "article", "An index of every spring entry", article(None, None, None)),
    ]
    .concat();
    TrustedHtml::owned(format!(
        "<div class=\"gal-trace\">{}<div class=\"gal-units\">{units}</div>{}</div>",
        ui::trace_open(&format!("{prefix}trace")),
        ui::trace_close(),
    ))
}
/// Every theme in one appearance: each a small page in its own tokens, its band with its name
/// and a summary, then on its paper a unit's stages, the tags, prose with a link, the buttons.
fn themes_part() -> TrustedHtml {
    let cards: String = super::THEMES
        .iter()
        .map(|theme| {
            format!(
                "<figure class=\"gal-themecard\" data-theme=\"{id}\"><div class=\"gt-band\"><span class=\"gt-name\">{name}</span><span class=\"gt-sum\"><a class=\"ask\" href=\"#\">1 question for you</a>. 1 failed. 2 article at work.</span></div><div class=\"gt-paper\">{strip}<p class=\"gal-row\">{ask}{attn}{live}{over}</p><p class=\"gt-text\">The draft cites the checklist; <a href=\"#\">its thread</a> has the reply. <span class=\"meta\">a-12 · article · 41m</span></p><p class=\"gal-row\"><button class=\"primary\" type=\"button\">Answer</button><button type=\"button\">Retry</button></p></div><figcaption class=\"meta\">{source}</figcaption></figure>",
                id = theme.id,
                name = ui::esc(theme.name),
                source = ui::esc(theme.source),
                strip = ui::stage_strip(
                    "Stages of a-12",
                    &article(Some(Shown::Succeeded), Some(Shown::Running), Some(Shown::Failed))
                ),
                ask = ui::tag("Awaiting your reply", "ask", None),
                attn = ui::tag("quiet 42m", "attn", None),
                live = ui::tag("live", "live", Some(ui::mark(Shown::Running))),
                over = ui::overrun(2.1),
            )
        })
        .collect();
    TrustedHtml::owned(format!("<div class=\"gal-themes\">{cards}</div>"))
}
/// Every part, drawn twice: once per theme of the house pair, with its ids apart.
pub fn parts() -> Result<Vec<Part>, askama::Error> {
    let project = project();
    let both = |f: &dyn Fn(&str) -> Result<TrustedHtml, askama::Error>| -> Result<(TrustedHtml, TrustedHtml), askama::Error> {
        Ok((f("l-")?, f("d-")?))
    };
    let mut parts = vec![];
    let mut add = |name, about, built, (light, dark): (TrustedHtml, TrustedHtml)| {
        parts.push(Part { name, wide: false, about, built, light, dark })
    };
    add("Status", "Each state a step, unit or project can be in: its glyph, its word and what it means. The same glyph and word on every page.", "`ui::status`, `ui::glyph` or `ui::mark`, from the status table.", both(&|_| Ok(statuses()))?);
    add("The band", "The navy title band at the top of a page: the page's name huge on the grid, its words and its summary sentence beside it, and under them what finished most recently, newest first. The sentence says only what the counts say: questions for you, what stopped, what runs (named by recipe, quiet first, then past its usual time), what waits and how much is done.", "`ui::band_head` and `ui::recent_strip` in `Frame::head` (`render_framed`); `ui::summary_sentence` from `ui::UnitFact`s.", both(&|p| Ok(band_part(p)))?);
    add("Summary sentence", "One sentence from the counts alone, with the question for you linked first.", "`ui::summary_sentence`.", both(&|_| Ok(summary_part()))?);
    add("Module grid", "Twelve columns that every module spans, fluid at every width: six on a narrow sheet (every module across it), a module half or whole on a medium one, and twenty-four from 2000px, where a module keeps its share or halves so two whole-width ones stand side by side. Show grid draws the construction under the modules: each column tinted and numbered, each module's span in its corner, as the sheet has them now.", "`ui::grid_open`, `ui::module_open`, `ui::module_open_wide` with `ui::Wide`, `ui::grid_toggle` (`sluice-grid`).", both(&|p| Ok(grid_part(p)))?);
    add("Modules", "A module is a unit (or a question, or a step) on the grid. The one that needs you swells: a question to you under the coral rule, its title a size up; a stopped one sits on the sand.", "`ui::module_open` with `ui::Swell`, `.mod-k`, `.mod-t`, `.mod-meta`, `.mod-body`, `.mod-actions`.", both(&|_| Ok(modules_part()))?);
    add("Section heads", "Each band of the page (For you, Stopped, Running, Waiting, Done) under a heavy rule, its name big and its count line at the right; over a table of strips, each stage's name over its column.", "`ui::section_head`, `ui::strip_head`, `ui::strip_row_open`.", both(&|_| Ok(heads_part()))?);
    add("Stage strip", "A unit's stages in its recipe's order, one cell each: an outline not reached (saying why when something holds it), sky when done with how long it took, blue while running with its time and a sweep, sand when it needs a look with its word. Past its usual time a run says how far, and a one-step recipe is one cell.", "`ui::stage_strip` from `ui::Stage`s (`ui::Cell::of` reads the status table), `ui::overrun`, `ui::stage_marks`.", both(&|_| Ok(strips_part()))?);
    add("Margin module", "A step that has run far past any usual time, or reports progress, in the margin: its running time, then each field it reported, as it reported it.", "`ui::margin_module` from a `ui::LongRun`.", both(&|_| Ok(margin_part()))?);
    add("Trace", "Select a unit to trace its chain: it opens in place, what it waits for and what waits on it light up with a rail in the margin, the line says the chain, and the rest fades. Select it again, Clear trace or Escape clears; the arrows move between units.", "`ui::trace_open`, `ui::Trace::attrs`, `ui::rail`, `ui::trace_button_open`, `ui::trace_more_open` (`sluice-trace`).", both(&|p| Ok(trace_part(p)))?);
    add("Tags", "A small fact set apart: plain, muted once something is closed, gold when it wants a look, coral only for a question waiting on you, blue with the running glyph while live.", "`ui::tag`, `ui::tag_link`, `ui::ask_link`.", both(&|_| Ok(tags()))?);
    add("Buttons", "The filled button is the next move; the rest are plain; a delete is the dark danger button; a dimmed one cannot be pressed now.", "Plain `button`, `.primary`, `.danger`.", both(&|_| Ok(buttons()))?);
    add("Tabs", "A step's parts, one at a time. The arrow keys, Home and End move between them; the chosen tab stays chosen as the page updates and is in the address, so a link opens it. Without script every part stands stacked.", "`ui::tabs_open`, `panel_open`, `panel_close`, `tabs_close` (`sluice-tabs`).", both(&|p| Ok(tabs(&format!("{p}tabs-"))))?);
    add("Sections and cards", "A heading over a section's rows, and a card around a region of its own.", "`ui::head`.", both(&|_| Ok(sections()))?);
    add("Fields", "Inputs, outputs and progress, one row each: its name, its value read by its kind, and where it came from or which run set it under it. Show value types (display preferences) adds each type.", "`kit.html`'s `field`.", both(&|p| fields(p, &project))?);
    add("Folds", "A long value shows its first lines, faded, with Show all under them (only when it was cut). A list says what it holds in a sentence that opens to it; More and Less are remembered per project, or open on a wide screen.", "`ui::fold`, `ui::more_open` (`sluice-fold`).", both(&|_| Ok(folds()))?);
    add("Conversation", "Messages in one column: a run from one sender under who sent it to whom, a line at each day and at the first unread, a question's answer under it, long ones folded, the message box at the end. A question for you has Answer and Close, and says where it went once answered.", "`threads::Conversation` in `sluice-conversation`, `sluice-composer`, `sluice-answer`.", both(&|p| conversation(p, &project))?);
    add("Empty states", "What a place says when it has nothing to show, and where to go instead.", "`ui::empty`, `ui::empty_with`.", both(&|_| Ok(empties()))?);
    add("Confirmation", "A step asks before Cancel, a succeeded step's Retry, Close all and a delete. Focus starts on the safe choice, Escape keeps things as they are, and you return to the button you pressed. Without script the question opens in place.", "`ui::Confirm` in `sluice-confirm`.", both(&|_| Ok(dialog()))?);
    add("Menus", "The project switcher and display preferences: a click elsewhere or Escape closes one, the arrows, Home and End move through it.", "`ui::menu_open` (`sluice-menu`).", both(&|_| Ok(menus()))?);
    add("Copy", "An id or a SHA in data mono, with a copy button that says Copied for a moment.", "`ui::copy` (`sluice-copy`).", both(&|_| Ok(copies()))?);
    add("Settings", "Display preferences apply at once and are kept for next time; every Show value types switch on a page follows.", "`ui::types_toggle`, `ui::setting_open` (`sluice-toggle`).", both(&|_| Ok(toggles()))?);
    add("Search", "A list filtered as you type (the functions), or the plan's tools showing what matches; Escape clears.", "`ui::search_open` (`sluice-search`).", both(&|p| Ok(searches(p)))?);
    add("Banners", "Updates paused or stopped, with Reconnect, in gold; a newer build of sluice, with Reload, muted. Hidden while the page is live.", "`ui::banner` (`sluice-banner`).", both(&|_| Ok(banners()))?);
    add("Notice", "What a page says at its top when something you asked for was not done, with what to do next and your words kept in their box. Read out as it appears.", "`ui::notice`: a refused step action comes back to the step's address, the notice held ten minutes under a key the server made.", both(&|_| Ok(notices()))?);
    add("Keys", "Listed under display preferences on every page: / finds on the plan (from a step or unit page too), the log and functions; [ and ] move the drawer to the step before or after; g then a letter goes to a section; ? opens the list. Never while typing in a field or under an open dialog.", "`sluice-keys`.", both(&|_| Ok(keys()))?);
    add("Splitter", "The line between the plan and an open step: drag it, or use the arrows (16px, 64px with Shift), Home and End; a double-click resets it, and its width is kept per project.", "`ui::splitter` (`sluice-splitter`).", both(&|p| Ok(splitters(p)))?);
    add("Themes", "Each theme the display preferences offer, in light and dark: a well-loved colour scheme mapped onto sluice's roles, so its band is its deepest surface, its blue (or its nearest) runs, its done is calm, its yellow wants a look and one colour of its own asks you a question. Americana, the logo's palette, is the default.", "`data-theme` and `data-appearance` on the page (cookies `sluice_theme`, `sluice_appearance`); the tokens in `style.css`.", both(&|_| Ok(themes_part()))?);
    add("Components", "For whoever builds a page: every component, the attributes it reads and what it does. The server draws everything in it; the component only behaves.", "Rocket components in `components.js`, light DOM.", (components(), TrustedHtml::default()));
    for part in &mut parts {
        part.wide = matches!(part.name, "Themes" | "The band" | "Module grid" | "Modules" | "Section heads" | "Trace");
    }
    Ok(parts)
}
async fn gallery(State(state): State<DashboardState>, headers: HeaderMap) -> Response {
    let page = async {
        let snapshot = state.snapshot(None).await?;
        let nav = NavView::new(&snapshot, None, "")?;
        let parts = parts().map_err(super::threads::render_error)?;
        let body = TrustedHtml::from_template(&GalleryTemplate { parts: &parts })
            .map_err(super::threads::render_error)?;
        super::render_layout("States and parts", &body, &nav, &Viewer::from_headers(&headers), "", "", "/_ui")
            .map_err(super::threads::render_error)
    };
    match page.await {
        Ok(html) => Html(html.as_str().to_owned()).into_response(),
        Err(e) => crate::http::error_response(e),
    }
}
/// The agent docs (`docs/agent`, as `sluice docs` and the MCP `docs` tool serve them) as
/// pages: an index, then each topic rendered.
async fn agent_docs(
    State(state): State<DashboardState>,
    topic: Option<axum::extract::Path<String>>,
    headers: HeaderMap,
) -> Response {
    use super::ui::esc;
    let page = async {
        let snapshot = state.snapshot(None).await?;
        let nav = NavView::new(&snapshot, None, "")?;
        let pages = sluice_runtime::docs::PAGES;
        // by task, not by name: the overview first, then a plan, its parts, and talking
        const ORDER: [&str; 9] = [
            "instructions",
            "plans",
            "types",
            "fns",
            "composing",
            "examples",
            "threads",
            "inbox",
            "board",
        ];
        let mut ordered: Vec<&(&str, &str)> = pages.iter().collect();
        ordered.sort_by_key(|(name, _)| ORDER.iter().position(|o| o == name).unwrap_or(ORDER.len()));
        // a topic by its first line, its first sentence when that runs on
        let first = |page: &str| {
            let line = page
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim_start_matches(['#', ' '])
                .trim();
            let sentence = line.split_once(". ").map_or(line, |(s, _)| s);
            sluice_model::naming::cut(sentence.trim_end_matches('.'), 64)
        };
        let index: String = ordered
            .iter()
            .map(|(name, page)| {
                format!(
                    "<li><a href=\"/docs/{n}\"{c}>{t}</a> <code class=\"meta\">{n}</code></li>",
                    n = esc(name),
                    t = esc(&first(page)),
                    c = if topic.as_ref().is_some_and(|t| t.0 == *name) {
                        " aria-current=\"page\""
                    } else {
                        ""
                    },
                )
            })
            .collect();
        let lead = TrustedHtml::owned(
            "<p>What the agents that drive sluice read: the same pages <code>sluice docs</code> and the MCP <code>docs</code> tool give them.</p>"
                .into(),
        );
        let (title, body, head) = match &topic {
            None => (
                "Agent docs".to_owned(),
                format!(
                    "<div class=\"agent-docs\">{}<ul class=\"docs-index docs-cards\">{index}</ul></div>",
                    super::ui::section_head(
                        "docs-topics",
                        "Topics",
                        &super::ui::count(pages.len(), "page", "pages")
                    )
                ),
                super::ui::band_head(
                    "Agent docs",
                    &lead,
                    &TrustedHtml::owned(format!(
                        "{} on plans, functions, threads, the inbox and the board, the overview first.",
                        super::ui::count(pages.len(), "page", "pages")
                    )),
                ),
            ),
            Some(t) => {
                let Some((_, page)) = pages.iter().find(|(name, _)| *name == t.0) else {
                    return Err(sluice_model::error::PublicError::NotFound {
                        message: format!("No agent docs page is named {}.", t.0),
                    });
                };
                (
                    format!("{} · Agent docs", first(page)),
                    format!(
                        "<div class=\"agent-docs docs-read\"><div class=\"docs-body\"><nav class=\"crumbs\" aria-label=\"Breadcrumb\"><a href=\"/docs\">{back}Agent docs</a></nav><div class=\"md docs-page\">{}</div></div><nav class=\"docs-index docs-more\" aria-label=\"Agent docs\"><p class=\"docs-more-h\">Every page</p><ul>{index}</ul></nav></div>",
                        crate::markdown::render_from(page, 1),
                        back = super::icons::icon(super::icons::Icon::ArrowLeft, 16, ""),
                    ),
                    super::ui::band_head(
                        "Agent docs",
                        &lead,
                        &TrustedHtml::owned(format!(
                            "{} <code>{}</code>",
                            super::ui::esc(&first(page)),
                            super::ui::esc(t.0.as_str())
                        )),
                    ),
                )
            }
        };
        super::render_framed(
            &title,
            &TrustedHtml::owned(body),
            &nav,
            &Viewer::from_headers(&headers),
            "",
            "",
            "/docs",
            &super::Frame {
                head,
                ..super::Frame::default()
            },
        )
        .map_err(super::threads::render_error)
    };
    match page.await {
        Ok(html) => Html(html.as_str().to_owned()).into_response(),
        Err(e) => crate::http::error_response(e),
    }
}
pub fn registration() -> PageRegistration {
    PageRegistration {
        routes: |state| {
            Router::new()
                .route("/_ui", get(gallery))
                .route("/docs", get(agent_docs))
                .route("/docs/{topic}", get(agent_docs))
                .with_state(state.dashboard.clone())
        },
        nav: |_| vec![],
        assets: &[],
    }
}
