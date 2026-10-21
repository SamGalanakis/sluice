//! `/_ui`: the kit's gallery. Every shared part of the dashboard (`views::ui`, the field rows of
//! `templates/kit.html`, the conversation of `views::threads`) drawn in every state with
//! real-looking data, each in the house pair's light and dark side by side: the component
//! system's documentation (DESIGN.md, Components). Nothing on it is read from the store.
use super::step::FieldView;
use super::unit_steps::Pulse;
use super::threads::{Build, Composer, Conversation, MessageItem, Who};
use super::ui::{self, Shown, StepRef, Tab};
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
impl Part {
    /// Its head, the `n`th part's (`part-<n>-h`, what its section is labelled by).
    pub fn head(&self, n: usize) -> TrustedHtml {
        ui::SectionHead::new(&format!("part-{n}-h"), self.name).html()
    }
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
/// Every pill's tone: a plain fact, a muted one, running, needs a look, quiet, done, and the
/// teal one for a question to the owner; one with a mark, one a link.
fn pills() -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<p class=\"gal-row\">{}{}{}{}{}{}{}{}{}{}{}{}</p>",
        ui::pill("unit: a-7", ui::Tone::Plain, None),
        ui::pill("Answered", ui::Tone::Muted, None),
        ui::pill("live", ui::Tone::Run, Some(ui::mark(Shown::Running))),
        ui::pill("2 awaiting reply", ui::Tone::Look, None),
        ui::status_pill(Shown::Running, "draft · 1h 11m"),
        ui::status_pill(Shown::Quiet, "scan · quiet 3h"),
        ui::status_pill(Shown::Failed, "failed 18m"),
        ui::status_pill(Shown::Succeeded, "25m"),
        ui::status_pill(Shown::Queued, "queued review"),
        ui::pill("done", ui::Tone::Done, None),
        ui::ask_link("#", "Use the new dates?"),
        ui::pill_link("#", "Its unit", ui::Tone::Plain, None),
    ))
}
/// Every button: the filled next move, the outlined others, a quiet one, the danger one, an
/// icon, one disabled, and the same as links.
fn buttons() -> TrustedHtml {
    use ui::{Btn, link, submit};
    let t = |s: &str| TrustedHtml::owned(ui::esc(s));
    TrustedHtml::owned(format!(
        "<p class=\"gal-row\">{}{}{}{}{}<button type=\"button\" disabled>Unpause</button></p><p class=\"gal-row\">{}{}{}</p>",
        submit(Btn::Primary, "action", "retry", &t("Retry with feedback"), ""),
        submit(Btn::Outlined, "action", "retry", &t("Retry"), ""),
        submit(Btn::Quiet, "action", "dismiss", &t("Dismiss"), ""),
        submit(Btn::Danger, "action", "delete", &t("Delete almanac"), ""),
        submit(Btn::Icon, "action", "dismiss", &t("×"), "Set it aside"),
        link(Btn::Primary, "#", &t("Answer"), ""),
        link(Btn::Outlined, "#", &t("Message it"), ""),
        link(Btn::Quiet, "#", &t("Set all aside"), ""),
    ))
}
/// A thing's status line in each state: running and heard lately (the dot breathing), running
/// and heard a while ago, gone quiet, waiting on the owner's answer, failed, succeeded and
/// waiting; a run of several and items done.
fn status_lines() -> TrustedHtml {
    use ui::{Dot, Shown, status_line};
    let t = |s: &str| TrustedHtml::owned(s.into());
    let rows = [
        status_line(Dot::State(Shown::Running), &[t("<b>Running</b> 6h 34m"), t("heard 2m ago"), t("run 2, after 1 failed")]),
        status_line(Dot::State(Shown::Running), &[t("<b>Running</b> 6h 34m"), t("heard 1h 18m ago")]),
        status_line(Dot::State(Shown::Quiet), &[t("<b>Running</b> 9h 2m"), ui::status_pill(Shown::Quiet, "quiet 3h"), t("not heard from since 3h ago")]),
        status_line(Dot::Ask, &[t("<b>Running</b> 20m"), t("<a href=\"#\">waiting on your answer</a>"), t("heard just now")]),
        status_line(Dot::State(Shown::Failed), &[t("<b>Failed</b> 1h 42m ago"), t("after 18m")]),
        status_line(Dot::State(Shown::Succeeded), &[t("<b>Succeeded</b> 57m ago"), t("after 1h 27m"), t("14 of 14 items done")]),
        status_line(Dot::State(Shown::Pending), &[t("<b>Pending</b>"), t("its last run ended 12h ago"), t("after 3m")]),
    ];
    TrustedHtml::owned(format!(
        "<div class=\"gal-stack\">{}</div>",
        rows.iter().map(|r| r.as_str()).collect::<String>()
    ))
}
/// A turn's tool calls, folded shut and open: the latest first, one failed, one running.
fn tool_calls() -> TrustedHtml {
    let call = |tool: &str, key: &str, took: &str, failed: bool, running: bool| ui::Call {
        tool: tool.into(),
        key: key.into(),
        took: took.into(),
        failed,
        running,
        href: "#".into(),
    };
    let rows = [
        call("Bash", "cargo nextest run -p sluice-web -E 'test(step)'", "", false, true),
        call("Edit", "crates/sluice-web/src/views/step.rs", "<1s", false, false),
        call("Bash", "git rebase origin/main", "4s", true, false),
        call("Read", "crates/sluice-web/templates/step.html", "<1s", false, false),
    ];
    let shut = ui::calls(12, 1, &rows);
    let open = shut.as_str().replacen("<details class=\"qf calls\"", "<details open class=\"qf calls\"", 1);
    TrustedHtml::owned(format!("{shut}{open}"))
}
/// A message as a card: its first words cut, with the way to the whole; and one short enough
/// to read whole.
fn message_cards() -> TrustedHtml {
    let who = |from: &str, to: &str| {
        TrustedHtml::owned(format!(
            "<b>{}</b><span class=\"mc-to\"> to </span>{}",
            ui::esc(from),
            ui::esc(to)
        ))
    };
    let when = TrustedHtml::owned("<time>12m ago</time>".into());
    TrustedHtml::owned(format!(
        "<div class=\"gal-stack\">{}{}</div>",
        ui::message_card(
            "",
            &who("This step", "@lead"),
            &when,
            &TrustedHtml::owned("<p>Landed the seams on main. Then the rest of the lane: the parser's leak, the two flaky checks and the docs for the new flag, each in its own commit so the review reads one change at a…</p>".into()),
            "#",
        ),
        ui::message_card(
            "",
            &who("@lead", "This step"),
            &when,
            &TrustedHtml::owned("<p>Rebase first: <code>lash-core-store__unit_test</code> is red.</p>".into()),
            "",
        ),
    ))
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
    let mut html = ui::tabs_open(prefix, "Example", &set, "overview").0;
    for (tab, said) in set.iter().zip(words) {
        html.push_str(ui::panel_open(prefix, tab.key, tab.label, 3, tab.key == "overview", false).as_str());
        html.push_str(&format!("<p>{}</p>", ui::esc(said)));
        html.push_str(ui::panel_close(prefix, tab.key, false).as_str());
    }
    html.push_str(ui::tabs_close().as_str());
    TrustedHtml::owned(html)
}
/// A card around a region.
fn sections() -> TrustedHtml {
    TrustedHtml::owned(
        "<div class=\"gal-stack\"><div class=\"card gal-card\"><p class=\"card-k\">00:12 · took 1h 29m</p><p class=\"card-t\">A card</p><p class=\"meta\">A region of its own.</p></div></div>".to_owned(),
    )
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
        "<p class=\"meta gal-cap\">Outputs</p>{}<p class=\"f-unset\"><span class=\"quiet\">2 outputs not set yet:</span> <span class=\"f-uname\">verdict</span>, <span class=\"f-uname\">pr</span></p><p class=\"meta gal-cap\">Inputs</p>{}",
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
        "<p class=\"meta gal-cap\">A long text, cut</p>{}<p class=\"meta gal-cap\">A long text that fits</p>{}<p class=\"meta gal-cap\">A list</p>{}<p>a-5-draft · a-6-draft · a-7-draft · a-8-draft · a-9-draft · a-10-draft</p>{}<p class=\"meta gal-cap\">More and Less</p>{}<div class=\"md\"><p>The rest of the document: what each section covers, and the order they go out in.</p></div>{}",
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
        "<div class=\"gal-row gal-menus\">{}<p class=\"meta\">A project's Details, its one ⋯: Escape or a click elsewhere closes it, ArrowDown goes into it.</p></div>",
        ui::Details::new()
            .id("Project", "01JA3V7KQ2W8M5T1X9C4ZB6NHE")
            .link("Settings", "#", "Its settings")
            .link("Export", "#", "The plan as Mermaid text")
            .menu("almanac"),
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
fn toggles(prefix: &str) -> TrustedHtml {
    use super::prefs::{Level, picture};
    let theme = if prefix == "d-" { "carbon" } else { "paper" };
    TrustedHtml::owned(format!(
        "<div class=\"d-sec-h tp-top\"><p class=\"meta\">From run 2</p>{}</div><div class=\"gal-levels\"><figure><figcaption class=\"meta\">Display preferences, opened</figcaption>{}</figure><figure><figcaption class=\"meta\">Keyboard shortcuts</figcaption>{}</figure></div>",
        ui::types_toggle(),
        picture(prefix, theme, Level::Top),
        picture(prefix, theme, Level::Keys),
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
        ui::search_open(),
        ui::search_field(&format!("{prefix}find"), "Find a function", "Find functions by name or description"),
        ui::search_close(),
    ))
}
fn banners() -> TrustedHtml {
    TrustedHtml::owned(format!(
        "{}{}{}",
        ui::banner("stream", "", "Updates paused. Reconnecting…", false, ""),
        ui::banner("stream", "", "Updates stopped at 14:02.", false, ""),
        ui::banner("release", "", "", false, ""),
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
        item(message(152885, "@lead", "a-7-draft", "2026-10-08T19:33:41Z", "The sibling entries in your brief are now concrete: the autumn waders are a-6-draft.", MessageVerb::Say), "note"),
        item(ask, "answered"),
        item(reply, "note"),
        item(message(153341, "a-8-draft", "a-7-draft", "2026-10-08T19:48:08Z", "The coastal maps are under `maps/coast/` now.", MessageVerb::Say), "note"),
        item(message(156475, "@editor", "a-7-draft", "2026-10-08T21:29:57Z", &long, MessageVerb::Say), "note"),
        item(message(157531, "a-7-draft", "@editor", "2026-10-09T08:10:02Z", "Revised; the style check passes. Submitting after the checklist question.", MessageVerb::Say), "note"),
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
        ui::empty("Nothing waiting on you."),
        ui::empty_with(
            "No run kept.",
            &TrustedHtml::owned("<a href=\"#\">Search the log for it</a>".into())
        )
    ))
}
fn notices() -> TrustedHtml {
    ui::notice("Nothing was done: this step changed since the page drew it, and it is running now. Look again, then retry. What you wrote is kept in its box: Retry sends it.")
}
/// The keys' list as display preferences' Keyboard shortcuts draws it (its `sluice-keys` host is
/// the page's own: one per page).
fn keys() -> TrustedHtml {
    super::prefs::keys_picture()
}

// ---- the Synthesis kit, on an invented project (almanac: a team writing a field guide) -------

/// An instant `seconds` before now, as the store writes one.
fn before(seconds: u64) -> String {
    let now = crate::clock::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    super::rfc3339(now.saturating_sub(seconds))
}
/// The `article` recipe's stages for one unit, each in the state given.
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
    let module = |span: u8, words: &str| {
        format!(
            "{}<p>{}</p>{}",
            ui::module_open(span, ui::Swell::Plain, words),
            ui::esc(words),
            ui::module_close()
        )
    };
    TrustedHtml::owned(format!(
        "{}{}{}{}{}{}",
        ui::grid_open(&format!("{prefix}grid"), 12),
        module(6, "Six columns"),
        module(4, "Four"),
        module(2, "Two"),
        module(12, "Twelve"),
        ui::grid_close(),
    ))
}
/// An invented unit's Details: what identifies it, behind its "⋯".
fn unit_details(unit: &str, step: &str) -> ui::Details {
    ui::Details::new()
        .id("Unit", unit)
        .text("Recipe", "article")
        .id("Step", step)
        .code("Function", "almanac.write")
        .text("Run", "2, after 1 failed")
        .text("region", "coast")
        .link("Page", "#", "The unit's page")
}
fn modules_part() -> TrustedHtml {
    let ask = format!(
        "<div class=\"mod\" style=\"--span:6\" data-span=\"6\">{}</div>",
        question_card("")
    );
    let look = format!(
        "{open}<div class=\"mod-k\">{g}<b>failed</b><span class=\"quiet\">review, after 18m</span>{menu}</div><p class=\"mod-t\">Waders: the autumn guide's entries</p><div class=\"mod-body\"><p>The style check found two entries without a photograph credit.</p></div><div class=\"mod-actions\"><button class=\"primary\" type=\"button\">Retry with feedback</button><button type=\"button\">Retry</button></div>{close}",
        open = ui::module_open(4, ui::Swell::Look, "Waders, failed"),
        g = ui::mark(Shown::Failed),
        menu = unit_details("a-6", "a-6-review").menu("Waders: the autumn guide's entries"),
        close = ui::module_close(),
    );
    TrustedHtml::owned(format!(
        "{}{ask}{look}{}",
        ui::grid_open("", 10),
        ui::grid_close()
    ))
}
/// A question to the owner as a card, answered in place: Answer opens its folded form, Close
/// question is quiet.
fn question_card(prefix: &str) -> TrustedHtml {
    ui::QuestionCard {
        id: &format!("{prefix}q-card"),
        level: 3,
        title: "Use the checklist's new spring dates in this draft?",
        titled: true,
        context: TrustedHtml::owned(
            "almanac · from <a href=\"#\">draft · Shorebirds: the spring guide's entries</a> · <time>25m ago</time>".into(),
        ),
        details: unit_details("a-7", "a-7-draft").menu("Use the checklist's new spring dates in this draft?"),
        body: TrustedHtml::owned(
            "<p>The regional checklist moved the spring dates a week earlier.</p><ul><li><strong>Now:</strong> use the new dates and say so in each entry.</li><li><strong>Later:</strong> wait for s-5 to finish the index first.</li></ul>".into(),
        ),
        answer: ui::answer_link("#", "#", "Use the checklist's new spring dates in this draft?"),
        way_on: ui::way_on("#", "Open its step"),
    }
    .html()
}
/// A question to the owner in both its densities: a line in Needs you or For you, answered in
/// place, and the card where it is answered.
fn questions(prefix: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<div class=\"gal-stack\"><ul class=\"nd\">{}</ul>{}</div>",
        ui::need_line(
            ui::Mark::Ask,
            &ui::NeedLine {
                id: &format!("{prefix}q-line"),
                title: "<a class=\"nd-t\" href=\"#\">Shorebirds: the spring guide's entries</a>",
                state: "asks you",
                who: &format!("<code class=\"ref\">a-7-draft</code> · {}", ui::ago(&before(240))),
                body: "<div class=\"md\"><p>Use the checklist's new spring dates in this draft?</p></div>",
                asker: Some("a-7"),
                form: &answer_sample(&format!("{prefix}q-answer"), "Shorebirds"),
                x: X_SAMPLE,
                ..Default::default()
            },
        ),
        question_card(prefix)
    ))
}
/// A set-aside × as the gallery draws it, posting nowhere.
const X_SAMPLE: &str = "<button type=\"button\" class=\"x\" aria-label=\"Set aside\" title=\"Set aside\">×</button>";
/// An answer in place as Needs you draws it, posting nowhere.
fn answer_sample(id: &str, to: &str) -> String {
    format!(
        "<form class=\"answer-in\" onsubmit=\"return false\">{}<div class=\"af\"><label class=\"vh\" for=\"{id}\">Your answer</label><textarea id=\"{id}\" rows=\"1\" data-answer placeholder=\"Answer {to}…\"></textarea><div class=\"hint\" aria-hidden=\"true\"><span><kbd>⌘↵</kbd> answer</span><span><kbd>Esc</kbd> cancel</span></div></div><button class=\"primary\" type=\"button\">Answer</button></form>",
        ui::avatar("owner", ui::AvSize::Md, true),
    )
}
/// An item's Details: its "⋯" closed, as every row, module and head carries it, and open.
fn details_part(prefix: &str) -> TrustedHtml {
    let details = unit_details("a-12", "a-12-review");
    let open = details
        .menu("Terns: the spring guide's entries")
        .as_str()
        .replacen("<details class=\"dm\"", "<details class=\"dm\" open", 1)
        .replace("Details of Terns", &format!("Details of Terns ({prefix}open)"));
    TrustedHtml::owned(format!(
        "<p class=\"meta gal-cap\">Closed</p><div class=\"gal-row gal-dm\"><span>Terns: the spring guide's entries</span>{}</div><p class=\"meta gal-cap\">Open</p><div class=\"gal-row gal-dm gal-dm-open\"><span>Terns: the spring guide's entries</span>{open}</div>",
        details.menu("Terns: the spring guide's entries")
    ))
}
/// A band section's head, as the other pages' sections wear it.
fn heads_part() -> TrustedHtml {
    TrustedHtml::owned(format!(
        "{}{}{}",
        ui::SectionHead::new("", "Needs you")
            .count(4)
            .action(&TrustedHtml::owned("<a class=\"quiet\" href=\"#\">Set all aside</a>".into()))
            .html(),
        ui::section_head("", "Durations by stage", "2 recipes · 41 runs"),
        ui::SectionHead::new("", "Projects").level(3).count(7).html(),
    ))
}
/// A plan's briefing (`views::plan`): Needs you with a question answered in place, a group of
/// stops and a silence, one item set aside; At work's head and five rows (busy, slow, quiet,
/// just started, started before the window); a Long runs group; what finished since the owner looked.
fn plan_part(prefix: &str) -> TrustedHtml {
    use super::plan::{RowParts, finished_row, roster_row, track};
    use ui::{Mark, NeedLine, need_line};
    let now = (crate::clock::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
        / 60.0)
        .floor()
        * 60.0;
    let needs = [
        need_line(
            Mark::Ask,
            &NeedLine {
                id: &format!("{prefix}n-ask"),
                title: "<a class=\"nd-t\" href=\"#\">Shorebirds: the spring guide's entries</a>",
                state: "asks you",
                who: &format!("<code class=\"ref\">a-7-draft</code> · {}", ui::ago(&before(240))),
                body: "<div class=\"md\"><p>The regional checklist moved the spring dates a week earlier. Use the new dates in this draft, or wait for <span class=\"ref\">s-5</span> to finish the index first?</p></div>",
                asker: Some("a-7"),
                form: &answer_sample(&format!("{prefix}n-answer"), "Shorebirds"),
                x: X_SAMPLE,
                ..Default::default()
            },
        ),
        need_line(
            Mark::State(Shown::Failed),
            &NeedLine {
                id: &format!("{prefix}n-fail"),
                title: "<a class=\"nd-t\" href=\"#\">Owls: the autumn guide's entries</a>",
                state: "failed at review after 12m",
                who: &format!("<code class=\"ref\">a-6-review</code> · {}", ui::ago(&before(720))),
                body: "The review found two entries citing the old checklist. <span class=\"nd-next\">Retry with feedback to say which dates to keep.</span>",
                act: "<button type=\"button\" data-fb aria-expanded=\"false\">Retry with feedback</button><button type=\"button\">Retry</button>",
                x: X_SAMPLE,
                ..Default::default()
            },
        ),
        need_line(
            Mark::State(Shown::Failed),
            &NeedLine {
                id: &format!("{prefix}n-cause"),
                title: "<span class=\"nd-t\">Three units</span>",
                state: "failed: they hit their wall-clock cap",
                more: "<details class=\"qf nd-more\"><summary>Show the three</summary><ul><li><a href=\"#\">Waders: the autumn guide's entries</a> <span>· “Two of five maps redrawn”</span></li><li><a href=\"#\">Gannets: the spring guide's entries</a></li><li><a href=\"#\">Scan the range maps for broken links</a></li></ul></details>",
                act: &ui::button(ui::Btn::Outlined, "Retry all", " type=\"button\""),
                x: X_SAMPLE,
                ..Default::default()
            },
        ),
        need_line(
            Mark::State(Shown::Quiet),
            &NeedLine {
                id: &format!("{prefix}n-quiet"),
                title: "<a class=\"nd-t\" href=\"#\">Scan the coast photographs for duplicates</a>",
                state: &format!("has not been heard from since {}", ui::clock(&before(9_840))),
                who: ui::ago(&before(9_840)).as_str(),
                act: &ui::link_with(ui::Btn::Outlined, "#", "Message it", ""),
                x: X_SAMPLE,
                ..Default::default()
            },
        ),
    ]
    .concat();
    let finished = [
        finished_row(&before(1_500), 15_300.0, "#", "Plovers: the spring guide's entries (FIG-7045)", ""),
        finished_row(&before(8_040), 14_160.0, "#", "Herons: the autumn guide's entries", ""),
    ]
    .concat();
    // a strip's states: busy, slow, quiet, just started, started before the window
    let row = |id: &str, engine: &str, title: &str, said: &str, track_html: String, pulse: Pulse<'_>| {
        roster_row(&RowParts {
            id: &format!("{prefix}{id}"),
            waiting: false,
            said_states: "",
            engine,
            runs_on: engine,
            title,
            owner: "",
            said,
            track: &track_html,
            pulse: &track(id, &pulse, now),
            drill: None,
        })
    };
    // seconds ago, every `step`, from `from` to `to`
    let every = |from: f64, to: f64, step: f64| -> Vec<f64> {
        let mut out = vec![];
        let mut t = from;
        while t >= to {
            out.push(now - t);
            t -= step;
        }
        out
    };
    let busy = [every(2_400.0, 1_500.0, 45.0), every(1_320.0, 30.0, 35.0)].concat();
    let slow = vec![now - 2_880.0, now - 2_460.0, now - 1_800.0, now - 1_080.0, now - 380.0];
    let quiet = vec![now - 10_200.0, now - 9_960.0];
    let fresh = every(230.0, 20.0, 40.0);
    let earlier = every(3_500.0, 400.0, 140.0);
    let stages = |_unit: &str, at: Shown, since: u64| {
        format!("<span class=\"current-stage {}\" data-state=\"{}\">draft {}{}</span>", if at == Shown::Quiet { "quiet" } else { "run" }, at.key(), if at == Shown::Quiet { "quiet " } else { "" }, ui::since(&before(since)))
    };
    let heard = |ago: u64, live: bool| {
        format!(
            "<span class=\"live{}\" aria-hidden=\"true\"></span><span class=\"heard\">· heard {}</span>",
            if live { " is-live" } else { "" },
            ui::ago(&before(ago))
        )
    };
    let said = |who: &str, words: &str| format!("{}{words}", ui::avatar(who, ui::AvSize::Sm, true));
    let rows = [
        row(
            "a-12",
            "opus",
            "Terns: the spring guide's entries",
            &said("a-12", "Entries 41 to 58 drafted; the maps moved under maps/coast/."),
            format!("{}{}", stages("a-12", Shown::Running, 2_400), heard(30, true)),
            Pulse { live: true, start: Some(now - 2_400.0), acts: &busy, said: &[now - 1_500.0, now - 480.0], active: Some(now - 30.0), quiet: false },
        ),
        row(
            "a-13",
            "sol",
            "Shorebirds: the spring guide's entries",
            &format!("{}<b>@lead</b> Checking each entry's dates against the checklist (FIG-7047).", ui::avatar("@lead", ui::AvSize::Sm, true)),
            format!("{}{}", stages("a-13", Shown::Running, 3_000), heard(380, false)),
            Pulse { live: true, start: Some(now - 3_000.0), acts: &slow, said: &[], active: Some(now - 380.0), quiet: false },
        ),
        row(
            "a-9",
            "sol",
            "Gannets: the spring guide's entries",
            &said("a-9", "Two of the five range maps redrawn; checking the third."),
            format!("{}<span class=\"live quiet\" aria-hidden=\"true\"></span>", stages("a-9", Shown::Quiet, 9_960)),
            Pulse { live: true, start: Some(now - 10_800.0), acts: &quiet, said: &[], active: Some(now - 9_960.0), quiet: true },
        ),
        row(
            "a-14",
            "opus",
            "Skuas: the spring guide's entries",
            "",
            format!("{}{}", stages("a-14", Shown::Running, 240), heard(20, true)),
            Pulse { live: true, start: Some(now - 240.0), acts: &fresh, said: &[], active: Some(now - 20.0), quiet: false },
        ),
        row(
            "a-15",
            "",
            "Auks: the spring guide's entries",
            &said("a-15", "Entries 1 to 9 drafted."),
            format!("{}{}", stages("a-15", Shown::Running, 18_000), heard(400, false)),
            Pulse { live: true, start: Some(now - 18_000.0), acts: &earlier, said: &[now - 2_100.0], active: Some(now - 400.0), quiet: false },
        ),
    ]
    .concat();
    let long = row("survey", "sol", "Checks every range map's links each night", "broken 3 of 412", format!("<span class=\"current-stage run\">survey {}</span><span class=\"heard\">· heard {}</span>", ui::since(&before(86_400)), ui::ago(&before(7_200))), Pulse { live: true, start: Some(now - 86_400.0), acts: &[], said: &[], active: Some(now - 7_200.0), quiet: false });
    TrustedHtml::owned(format!(
        "<div class=\"plan-pane\"><div class=\"plan-sheet\"><section class=\"pb-sec\">{nh}<ul class=\"nd\">{needs}</ul><ul class=\"nd-gone\"><li><span>Set aside: the silence of Herons: the autumn guide's entries</span><button type=\"button\" class=\"quiet\">Undo</button></li></ul></section><section class=\"pb-sec\"{listen}><div class=\"ro\"><div class=\"aw-head\"><div class=\"aw-left\"><h2>At work</h2><span class=\"n\">6 units</span></div></div><details class=\"ro-g\" open><summary class=\"ro-group\">{chev}<b>article</b><span>1 quiet, 4 running</span></summary>{rows}</details><details class=\"ro-g\" open><summary class=\"ro-group\">{chev}<b>Long runs</b><span>1 running</span></summary>{long}</details></div>{pop}</section><section class=\"pb-sec\">{fh}<ul class=\"fin\">{finished}</ul><button type=\"button\" class=\"quiet\">Mark as seen</button></section></div></div>",
        chev = super::icons::icon(super::icons::Icon::ChevronRight, 16, "chev"),
        nh = ui::SectionHead::new("", "Needs you").count(4).html(),
        listen = super::pulse::listen("/_ui/pulse", &format!("{prefix}pulse-pop")),
        pop = super::pulse::popover(&format!("{prefix}pulse-pop")),
        fh = ui::SectionHead::new("", "Finished since you looked").html(),
    ))
}
fn strips_part() -> TrustedHtml {
    let quiet = vec![
        ui::Stage::new("draft", Some(Shown::Succeeded)).took(2_460.0),
        ui::Stage::new("review", Some(Shown::Quiet)).took(3_180.0),
        ui::Stage::new("publish", None),
    ];
    let long = vec![
        ui::Stage::new("fork", Some(Shown::Succeeded)).took(40.0),
        ui::Stage::new("work", Some(Shown::Succeeded)).took(9_840.0),
        ui::Stage::new("land", Some(Shown::Running)).running_since(before(360), 360.0),
        ui::Stage::new("landed", None),
        ui::Stage::new("close", None),
        ui::Stage::new("rm", None),
    ];
    let asks = vec![
        ui::Stage::new("fork", Some(Shown::Succeeded)),
        ui::Stage::new("work", Some(Shown::Succeeded)).took(1_860.0),
        ui::Stage::new("land", Some(Shown::Pending)).asks(true),
        ui::Stage::new("landed", None),
    ];
    let scan = vec![ui::Stage::new("scan", Some(Shown::Running)).running_since(before(1_080), 1_080.0)];
    let held = vec![
        ui::Stage::new("draft", Some(Shown::Paused)),
        ui::Stage::new("review", Some(Shown::Blocked)),
        ui::Stage::new("publish", Some(Shown::Pending)),
    ];
    let failed = vec![
        ui::Stage::new("draft", Some(Shown::Succeeded)),
        ui::Stage::new("review", Some(Shown::Failed)),
        ui::Stage::new("publish", None),
    ];
    let track = |label: &str, stages: &[ui::Stage]| {
        format!("<div class=\"track\">{}</div>", ui::stage_track(label, stages, None))
    };
    TrustedHtml::owned(format!(
        "<p class=\"meta gal-cap\">Done, running, next; the rest folded</p>{}{}<p class=\"meta gal-cap\">Asks you; quiet; failed</p>{}{}{}<p class=\"meta gal-cap\">Paused; one stage</p>{}{}",
        track("Stages of a-12", &article(Some(Shown::Succeeded), Some(Shown::Running), None)),
        track("Stages of FIG-7048", &long),
        track("Stages of FIG-7047", &asks),
        track("Stages of a-9", &quiet),
        track("Stages of a-6", &failed),
        track("Stages of a-15", &held),
        track("Stages of s-4", &scan),
    ))
}
fn summary_part() -> TrustedHtml {
    let unit = |name: &str, recipe: &str, shown, quiet| ui::UnitFact {
        name: name.into(),
        recipe: recipe.into(),
        shown: Some(shown),
        quiet,
        ..ui::UnitFact::default()
    };
    let units = [
        unit("a-6", "article", Shown::Failed, None),
        unit("a-10", "article", Shown::Cancelled, None),
        unit("a-7", "article", Shown::Running, None),
        unit("a-12", "article", Shown::Running, None),
        unit("s-3", "scan", Shown::Quiet, Some(3_180.0)),
        unit("a-13", "article", Shown::Pending, None),
        unit("a-14", "article", Shown::Pending, None),
        unit("a-1", "article", Shown::Succeeded, None),
        unit("a-2", "article", Shown::Succeeded, None),
        unit("s-1", "scan", Shown::Succeeded, None),
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
/// The bar in its states: a project's plan with a board (Find closed, Both chosen), the same
/// holding a find, and a page of no project; each in a band of its own (the ids of the page's
/// own bar left to it).
fn bars() -> Result<TrustedHtml, askama::Error> {
    let id = project();
    let base = format!("/projects/id/{id}");
    let link = |label: &str, href: String, current: bool| super::NavLink {
        href,
        label: label.into(),
        current,
    };
    let almanac = super::ProjectView {
        id,
        name: "almanac".into(),
        description: String::new(),
        icon_text: String::new(),
        icon_url: String::new(),
        paused: false,
        archived: false,
        changed: String::new(),
        counts: Default::default(),
        running: vec![],
        stopped: vec![],
        dismissed: Default::default(),
        asks: vec![],
        looks: 0,
    };
    let project_nav = super::NavView {
        projects: vec![almanac],
        label: "almanac".into(),
        links: vec![
            link("Plan", base.clone(), true),
            link("Day", format!("{base}/day"), false),
            link("Stats", format!("{base}/stats"), false),
            link("Messages", format!("{base}/inbox"), false),
            link("Log", format!("{base}/log"), false),
            link("Functions", format!("/fns?project={id}"), false),
            link("Settings", format!("{base}/settings"), false),
        ],
        inbox: 3,
        notes: 0,
        selected: Some(id),
    };
    let home_nav = super::NavView {
        label: "All projects".into(),
        links: vec![
            link("Day", "/day".into(), false),
            link("Log", "/log".into(), false),
            link("Functions", "/fns".into(), false),
        ],
        inbox: 0,
        notes: 2,
        selected: None,
        ..project_nav.clone()
    };
    // a picture, not the page's own: its ids left out so the page keeps one of each
    let unid = |html: &str| {
        html.replace(" id=\"top-nav\"", "")
            .replace(" id=\"nav-inbox\"", "")
            .replace(" id=\"side-nav\"", "")
            .replace(" id=\"sidebar\"", "")
    };
    let bar = |nav: &super::NavView, stream: &str| -> Result<String, askama::Error> {
        let html = TrustedHtml::from_template(&super::Bar {
            nav,
            stream,
            path: "/_ui",
        })?;
        Ok(unid(html.as_str()))
    };
    Ok(TrustedHtml::owned(format!(
        "<div class=\"gal-bars\"><p class=\"meta\">A project's page</p>{}<p class=\"meta\">Updates paused</p><div data-stream=\"paused\">{}</div><p class=\"meta\">A page of no project</p>{}</div>",
        bar(&project_nav, "")?,
        bar(&project_nav, "/stream")?,
        bar(&home_nav, "")?,
    )))
}
fn head_part() -> TrustedHtml {
    let details = ui::Details::new()
        .id("Project", "01a2b3c4-5d6e-7f80-9a1b-2c3d4e5f6a7b")
        .text("About", "The field guide's spring and autumn editions, written a section at a time.");
    TrustedHtml::owned(format!(
        "<div class=\"gal-head\">{}</div>",
        ui::lead_head(
            "almanac",
            &TrustedHtml::owned("<a class=\"ask\" href=\"#\">1 question for you</a>, 1 failed, 1 cancelled. 3 at work; 3 of 10 done · 24 steps.".into()),
            &details.menu("almanac"),
            &TrustedHtml::default(),
        )
    ))
}
/// A step's head (`StepView::band`): a review stage of an invented article, failed on its
/// second run, its unit's three stages with its own ringed, Retry its next move.
fn step_band_part(prefix: &str) -> Result<TrustedHtml, askama::Error> {
    use sluice_model::commands::StepStatus;
    use sluice_model::gates::{StateSnapshot, StepState};
    use sluice_model::ids::Revision;
    use sluice_model::plan::{FnSignature, SignatureProvider, compile_rows};
    use sluice_model::plan_rows::{PlanHeader, PlanRows, RootSection, StateEpoch, StepRow};
    struct Open;
    impl SignatureProvider for Open {
        fn signature(&self, _: &str) -> Option<FnSignature> {
            Some(FnSignature {
                open: true,
                ..Default::default()
            })
        }
    }
    let declaration = serde_json::from_value(serde_json::json!({
        "run": "custom.open",
        "doc": "Terns: the spring guide's entries\n\nCheck each entry against the regional checklist and the photograph credits."
    }))
    .map_err(|e| askama::Error::Custom(e.to_string().into()))?;
    let rows = PlanRows {
        header: PlanHeader {
            rev: Revision(1),
            root_order: vec![RootSection::Steps],
            state_epoch: StateEpoch(0),
        },
        inputs: vec![],
        outputs: vec![],
        steps: vec![StepRow {
            step: "a-12-review".parse().expect("a step id"),
            position: 0,
            declaration,
        }],
    };
    let plan = compile_rows(&rows, &Open)
        .map_err(|e| askama::Error::Custom(format!("{e:?}").into()))?;
    let mut state = StateSnapshot::default();
    state.steps.insert(
        "a-12-review".parse().expect("a step id"),
        StepState {
            status: StepStatus::Failed,
            ..Default::default()
        },
    );
    let mut step = super::step::StepView::new(
        ProjectId::new(),
        &plan,
        &state,
        &"a-12-review".parse().expect("a step id"),
    );
    step.title = "Terns: the spring guide's entries".into();
    step.stage = "review".into();
    step.lane_unit = "a-12".into();
    step.lane_recipe = "article".into();
    step.lane = vec![
        ui::Stage::new("draft", Some(Shown::Succeeded)).took(1_680.0),
        ui::Stage::new("review", Some(Shown::Failed))
            .took(1_080.0)
            .href(step.href()),
        ui::Stage::new("publish", Some(Shown::Blocked)),
    ];
    let band = step.band("almanac", Some(("a-12", "Terns: the spring guide's entries")))?;
    // each copy keeps its ids (the feedback box, the dialogs) apart
    let html = band
        .as_str()
        .replacen(" id=\"step-band\"", "", 1)
        .replacen(" id=\"d-title\"", "", 1)
        .replace(" id=\"", &format!(" id=\"{prefix}"))
        .replace(" for=\"", &format!(" for=\"{prefix}"))
        .replace(" aria-labelledby=\"", &format!(" aria-labelledby=\"{prefix}"))
        .replace(" aria-describedby=\"", &format!(" aria-describedby=\"{prefix}"))
        .replace(" aria-controls=\"", &format!(" aria-controls=\"{prefix}"))
        .replace(" data-dialog=\"", &format!(" data-dialog=\"{prefix}"));
    Ok(TrustedHtml::owned(format!("<div class=\"gal-head\">{html}</div>")))
}
/// The day line (`sluice-day`) in its states: a project with a run going, its bar ending at the
/// now rule that moves with the clock; one whose runs all ended, one needing a look; and a day
/// with nothing running.
fn day_lines() -> TrustedHtml {
    use super::day::{DayData, DayProject, DayRun, DayView};
    let now = crate::clock::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (almanac, chores) = (ProjectId::new(), ProjectId::new());
    let run = |project: ProjectId, n: usize, title: &str, step: &str, ago: u64, took: Option<u64>, outcome: Shown| {
        let started = now - ago * 60;
        DayRun {
            project,
            run_id: format!("run-{n}"),
            step: step.into(),
            unit: title.to_lowercase(),
            n: 1,
            item: -1,
            started,
            ended: took.map(|t| started + t * 60),
            outcome: Some(outcome),
            started_at: super::rfc3339(started),
            kept: true,
            title: title.into(),
        }
    };
    let projects = vec![
        DayProject { id: almanac, name: "almanac".into(), paused: false, archived: false },
        DayProject { id: chores, name: "chores".into(), paused: false, archived: false },
    ];
    let runs = vec![
        run(almanac, 1, "Terns", "draft", 600, Some(95), Shown::Succeeded),
        run(almanac, 2, "Terns", "review", 470, Some(140), Shown::Succeeded),
        run(almanac, 3, "Plovers", "draft", 300, Some(70), Shown::Failed),
        run(almanac, 4, "Gulls", "draft", 95, None, Shown::Running),
        run(chores, 5, "Gutters", "check", 820, Some(40), Shown::Succeeded),
        run(chores, 6, "Boiler", "check", 380, Some(25), Shown::Succeeded),
    ];
    let live = DayView::new(
        DayData { projects: projects.clone(), runs: runs.clone(), last_before: BTreeMap::new() },
        None,
        now,
        0,
    );
    let still = DayView::new(
        DayData {
            projects: projects[1..].to_vec(),
            runs: runs.into_iter().filter(|r| r.project == chores).collect(),
            last_before: BTreeMap::new(),
        },
        None,
        now,
        0,
    );
    TrustedHtml::owned(format!(
        "<div class=\"gal-stack\">{}{}</div>",
        live.day_line(),
        still.day_line()
    ))
}
/// The page menu's text links: Plan, Plan and board, Board, with each choice current.
fn view_choices() -> TrustedHtml {
    let bar = |on: usize| {
        format!(
            "<div class=\"site-head\"><div class=\"band-in site-bar\">{}</div></div>",
            super::plan::view_links("#", Some(["plan", "both", "board"][on]))
        )
    };
    TrustedHtml::owned(format!(
        "<div class=\"gal-stack\">{}{}{}</div>",
        bar(0),
        bar(1),
        bar(2)
    ))
}
/// A project's stats, drawn from invented runs: the `article` recipe's stages with their
/// median and p90, and a week's units finished a day.
fn stats_part(prefix: &str) -> TrustedHtml {
    use super::stats::{Span, StatRun, StatsData, StatsView};
    let recipe = serde_json::json!({"name": "article", "steps": {
        "{unit}-draft": {"run": "almanac.write"},
        "{unit}-review": {"run": "almanac.write", "after": ["{unit}-draft"]},
        "{unit}-publish": {"run": "almanac.write", "after": ["{unit}-review"]}}});
    let mut names = sluice_runtime::naming::ProjectNaming::default();
    if let Ok(recipe) = sluice_model::recipe::Recipe::parse("article", &recipe) {
        names
            .recipes
            .insert("article".into(), std::sync::Arc::new(recipe));
    }
    let now = crate::clock::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut runs = vec![];
    // six units over five days: drafts 20 to 45 minutes, reviews 30 to 80, publishes 2
    for (i, (draft, review, failed)) in [
        (25, 40, false),
        (30, 35, false),
        (45, 80, true),
        (20, 30, false),
        (35, 50, false),
        (28, 44, false),
    ]
    .into_iter()
    .enumerate()
    {
        let unit = format!("a-{}", i + 1);
        let mut at = now - (5 - i as u64) * 86_400 + 3_600;
        let mut add = |stage: &str, minutes: u64, outcome: Shown, n: usize| {
            runs.push(StatRun {
                run_id: format!("r-{unit}-{stage}-{n}"),
                step: format!("{unit}-{stage}"),
                unit: unit.clone(),
                item: -1,
                n,
                started: at,
                ended: Some(at + minutes * 60),
                outcome: Some(outcome),
                kind: if outcome == Shown::Failed {
                    "fn failure".into()
                } else {
                    String::new()
                },
                kept: false,
            });
            at += minutes * 60 + 60;
        };
        add("draft", draft, Shown::Succeeded, 1);
        if failed {
            add("review", 12, Shown::Failed, 1);
            add("review", review, Shown::Succeeded, 2);
        } else {
            add("review", review, Shown::Succeeded, 1);
        }
        add("publish", 2, Shown::Succeeded, 1);
    }
    let data = StatsData {
        runs,
        planned: Default::default(),
        names: std::sync::Arc::new(names),
    };
    let mut view = StatsView::new(&data, ProjectId::new(), Span::Week, now, 0);
    view.ids = prefix.to_owned();
    TrustedHtml::owned(format!(
        "<div class=\"stats\">{}{}</div>",
        view.durations(),
        view.throughput()
    ))
}
/// The step view's cases (`unit_steps`), each a unit of the invented project: a chain, a
/// fan-out and its join, a condition, a failure on its third run, a long run beside quick ones,
/// a unit idle for a day between steps.
fn steps_part() -> TrustedHtml {
    use super::unit_steps::{Edge, Latest, Progress, RunSpan, Span, Step, View};
    let now = crate::clock::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs() as f64);
    // a run from `from` to `to` seconds before now (none: still going), ending as `outcome`
    let run = |from: f64, to: Option<f64>, outcome: Shown| RunSpan {
        started: before(from as u64),
        finished: to.map(|t| before(t as u64)),
        outcome: Some(outcome),
        items: 0,
        from: now - from,
        to: to.map(|t| now - t),
    };
    struct Case {
        name: &'static str,
        steps: Vec<(&'static str, Shown, Vec<RunSpan>, Progress)>,
        edges: Vec<(&'static str, &'static str, &'static str)>,
        latest: Option<(&'static str, &'static str, u64)>,
        live: bool,
        tries: Option<(usize, Vec<Shown>)>,
        owner: &'static str,
    }
    use Shown::*;
    let h = 3600.0;
    let cases = vec![
        Case {
            name: "A chain",
            steps: vec![
                ("draft", Succeeded, vec![run(1.8 * h, Some(1.1 * h), Succeeded)], Progress::None),
                ("review", Running, vec![run(1.1 * h, None, Running)], Progress::Text("2 of 3".into())),
                ("publish", Pending, vec![], Progress::None),
            ],
            edges: vec![("draft", "review", ""), ("review", "publish", "")],
            latest: Some(("review", "The tern maps moved under maps/coast/; the entries point there now.", 240)),
            live: true,
            tries: None,
            owner: "@editor",
        },
        Case {
            name: "A fan-out and its join",
            steps: vec![
                ("gather", Succeeded, vec![run(3_000.0, Some(2_700.0), Succeeded)], Progress::None),
                ("birds", Succeeded, vec![run(2_700.0, Some(1_500.0), Succeeded)], Progress::None),
                ("places", Running, vec![run(2_700.0, None, Running)], Progress::Part { done: 31.0, total: 48.0, text: "31/48".into() }),
                ("dates", Succeeded, vec![run(2_700.0, Some(2_100.0), Succeeded)], Progress::None),
                ("merge", Pending, vec![], Progress::None),
            ],
            edges: vec![
                ("gather", "birds", ""), ("gather", "places", ""), ("gather", "dates", ""),
                ("birds", "merge", ""), ("places", "merge", ""), ("dates", "merge", ""),
            ],
            latest: None,
            live: true,
            tries: None,
            owner: "",
        },
        Case {
            name: "A condition",
            steps: vec![
                ("count", Succeeded, vec![run(1_300.0, Some(1_180.0), Succeeded)], Progress::None),
                ("check", Succeeded, vec![run(1_180.0, Some(1_170.0), Succeeded)], Progress::None),
                ("publish", Succeeded, vec![run(1_170.0, Some(600.0), Succeeded)], Progress::None),
                ("set aside", Skipped, vec![], Progress::None),
                ("index", Succeeded, vec![run(600.0, Some(420.0), Succeeded)], Progress::None),
            ],
            edges: vec![
                ("count", "check", ""), ("check", "publish", "ok"), ("check", "set aside", "not ok"),
                ("publish", "index", ""), ("set aside", "index", ""),
            ],
            latest: None,
            live: false,
            tries: None,
            owner: "",
        },
        Case {
            name: "A failure, run 3",
            steps: vec![
                ("draft", Succeeded, vec![run(2.0 * h, Some(1.4 * h), Succeeded)], Progress::None),
                ("review", Failed, vec![
                    run(1.4 * h, Some(1.2 * h), Failed),
                    run(1.1 * h, Some(0.8 * h), Failed),
                    run(0.6 * h, Some(0.3 * h), Failed),
                ], Progress::None),
                ("publish", Blocked, vec![], Progress::None),
            ],
            edges: vec![("draft", "review", ""), ("review", "publish", "")],
            latest: Some(("review", "Two entries still lack a photograph credit.", 1_100)),
            live: true,
            tries: Some((3, vec![Failed, Failed])),
            owner: "@editor",
        },
        Case {
            name: "A long run beside quick ones",
            steps: vec![
                ("fetch", Succeeded, vec![run(7_210.0, Some(7_208.0), Succeeded)], Progress::None),
                ("parse", Succeeded, vec![run(7_208.0, Some(7_206.0), Succeeded)], Progress::None),
                ("scan", Running, vec![run(7_206.0, None, Running)], Progress::Text("1240".into())),
                ("index", Pending, vec![], Progress::None),
            ],
            edges: vec![("fetch", "parse", ""), ("parse", "scan", ""), ("scan", "index", "")],
            latest: None,
            live: true,
            tries: None,
            owner: "you",
        },
        Case {
            name: "Idle a day between steps",
            steps: vec![
                ("draft", Succeeded, vec![run(28.0 * h, Some(27.3 * h), Succeeded)], Progress::None),
                ("review", Succeeded, vec![run(1.5 * h, Some(1.1 * h), Succeeded)], Progress::None),
                ("publish", Succeeded, vec![run(1.1 * h, Some(1.05 * h), Succeeded)], Progress::None),
            ],
            edges: vec![("draft", "review", ""), ("review", "publish", "")],
            latest: None,
            live: false,
            tries: None,
            owner: "",
        },
    ];
    let mut out = String::from("<div class=\"gal-steps\">");
    for case in &cases {
        let steps = case
            .steps
            .iter()
            .map(|(name, shown, spans, progress)| Step {
                id: format!("a-12-{}", name.replace(' ', "-")),
                name: (*name).to_owned(),
                title: String::new(),
                href: "#".into(),
                shown: *shown,
                word: shown.word(),
                bypass_by: None,
                runs: match (&case.tries, shown) {
                    (Some((n, earlier)), Failed) => ui::tries(*n, earlier),
                    _ => TrustedHtml::default(),
                },
                progress: progress.clone(),
                spans,
                span: match spans.last() {
                    Some(r) if r.to.is_none() => Span::Since(r.started.clone()),
                    Some(r) => Span::Took(r.to.unwrap_or(r.from) - r.from),
                    None => Span::None,
                },
                note: String::new(),
                runs_on: "custom.open".into(),
                // a running step's messages: a tick each on its bar
                ticks: match spans.last() {
                    Some(r) if r.to.is_none() => vec![r.from + 0.3 * h, r.from + 0.6 * h],
                    _ => vec![],
                },
                acts: &[],
                active: None,
                current: spans.last().is_some_and(|r| r.to.is_none()),
                quiet_since: None,
            })
            .collect();
        let view = View {
            label: format!("Steps of {}", case.name),
            unit: String::new(),
            owner: case.owner.to_owned(),
            steps,
            edges: case
                .edges
                .iter()
                .map(|(a, b, when)| Edge {
                    from: format!("a-12-{}", a.replace(' ', "-")),
                    to: format!("a-12-{}", b.replace(' ', "-")),
                    when: (*when).to_owned(),
                })
                .collect(),
            latest: case.latest.map(|(from, text, ago)| Latest {
                at: before(ago),
                from: ui::esc(from),
                text: text.into(),
                href: "#".into(),
            }),
            selected: String::new(),
            opens: false,
            live: case.live,
            now,
            recent: false,
        };
        out.push_str(&format!(
            "<figure class=\"gal-case\"><figcaption class=\"gal-theme\">{}</figcaption>{}</figure>",
            ui::esc(case.name),
            view.html()
        ));
    }
    out.push_str("</div>");
    TrustedHtml::owned(out)
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
    add("Status", "Each state a step, unit or project can be in: its glyph, its word and what it means.", "`ui::status`, `ui::glyph` or `ui::mark`, from the status table.", both(&|_| Ok(statuses()))?);
    add("The top bar", "The page's sections, then Inbox at the right; while the page's stream reconnects, an ochre \"Updates paused · Reconnect\". On a phone the menu button and the project's name lead it, the sections a row of their own, Log, Functions and Settings in More.", "`templates/bar.html` (`views::Bar`) and `top_nav.html`, included by `layout.html`; its nav the region a stream patches as `top-nav`.", both(&|_| bars())?);
    add("Page head", "On a page the bar already names (a plan, home, a project's sections) the lead line alone, its name a heading for a screen reader, its ⋯ beside it; elsewhere (a unit, a step, a thread) the page's name over its line.", "`ui::lead_head`; `ui::page_head`, `ui::page_head_with` in `Frame::head` (`render_framed`); `ui::summary_sentence` from `ui::UnitFact`s.", both(&|_| Ok(head_part()))?);
    add("Step head", "A step's way back, name, state, words, its unit's stages and its actions.", "`StepView::band` (`templates/step_band.html`) in `Frame::head`, the region a step's stream patches as `step-band`; `ui::stage_track` at its own stage.", both(&|p| step_band_part(p))?);
    add("Details", "What identifies an item, behind one ⋯ at its end; a failure's ⋯ also holds Set it aside.", "`ui::Details`, its `actions` and its `menu` (`sluice-menu`).", both(&|p| Ok(details_part(p)))?);
    add("Summary sentence", "One sentence from the counts alone, the question for you first.", "`ui::summary_sentence`.", both(&|_| Ok(summary_part()))?);
    add("Module grid", "Twelve columns every module spans, half or whole on a medium sheet.", "`ui::grid_open`, `ui::module_open`.", both(&|p| Ok(grid_part(p)))?);
    add("Modules", "A unit, a question or a step on the grid; the one that needs you swells.", "`ui::module_open` with `ui::Swell`, `.mod-k`, `.mod-t`, `.mod-meta`, `.mod-body`, `.mod-actions`.", both(&|_| Ok(modules_part()))?);
    add("Section heads", "A section's name, its count muted, a note and its one action at the right.", "`ui::SectionHead`, `ui::section_head`.", both(&|_| Ok(heads_part()))?);
    add("Plan briefing", "A plan's Needs you (each item a sentence, one action and a ×; what was set aside, with Undo), what finished since you looked, and At work: a row a unit, its pulse strip over the last hour (busy, slow, quiet, just started, started earlier); point at a mark, or the faded left edge of one that started earlier, to see what happened then.", "`ui::need_line`, `plan::finished_row`, `plan::track` (a `sluice-pulse`, its popover `pulse::popover`, its details `pulse::details`) and `plan::roster_row`, drawn by `views::plan::Plan`.", both(&|p| Ok(plan_part(p)))?);
    add("Stage track", "A unit's stages in a line: the one before where it stands, that one (green while it runs, teal when it asks you, its word when it stopped) and the next; the rest folded into a count that names them. On a phone only where it stands, and its place.", "`ui::stage_track` from `ui::Stage`s.", both(&|_| Ok(strips_part()))?);
    add("Stats", "A project's numbers over a window.", "`views::stats::StatsView` (`durations`, `outcomes`, `throughput`, `slowest_html`), from `StatsData`.", both(&|p| Ok(stats_part(p)))?);
    add("Step view", "A unit step by step: its gates, what each step runs on, its state, progress, runs on one time axis with a tick a message, how long it took, and its last message.", "`unit_steps::View` (`unit_steps::of_unit` from a unit) in `sluice-steps`; a plan row opens to it (`plan::roster_row`).", both(&|_| Ok(steps_part()))?);
    add("Question", "A question to you, the one way it is drawn: a line in Needs you and For you, and the card where it is answered. The teal dot alone marks it; one filled Answer, a quiet Close question, the form folded until Answer.", "`ui::need_line` with `ui::Mark::Ask`; `ui::QuestionCard` (`MessageItem::question_card` with `answer.html` in `sluice-answer`).", both(&|p| Ok(questions(p)))?);
    add("Pills", "A small fact set apart, in its state's tone; a question to you is its teal dot, never a pill.", "`ui::pill`, `ui::pill_link`, `ui::ask_link` with `ui::Tone`.", both(&|_| Ok(pills()))?);
    add("Buttons", "The filled button is the next move; the others outlined, quiet or an icon.", "`ui::submit`, `ui::link` with `ui::Btn` (`ui/button.rs`).", both(&|_| Ok(buttons()))?);
    add("Status line", "How a thing stands in one muted sentence after its dot: running (breathing while heard lately), quiet, waiting on you, failed, done, waiting.", "`ui::status_line` with `ui::Dot`.", both(&|_| Ok(status_lines()))?);
    add("Tool calls", "A turn's calls in a quiet fold, shut and open: the tool, its key argument on one line, how long it took or that it failed.", "`ui::calls` from `ui::Call`s.", both(&|_| Ok(tool_calls()))?);
    add("Message cards", "A message as a card: who to whom, when, its first words and the way to the whole.", "`ui::message_card`; `threads::Conversation::cards`.", both(&|_| Ok(message_cards()))?);
    add("Tabs", "A step's parts, one at a time.", "`ui::tabs_open`, `panel_open`, `panel_close`, `tabs_close` (`sluice-tabs`).", both(&|p| Ok(tabs(&format!("{p}tabs-"))))?);
    add("Cards", "A card around a region of its own: a hairline, no shadow.", "`.card`.", both(&|_| Ok(sections()))?);
    add("Fields", "Inputs, outputs and progress, one row each.", "`kit.html`'s `field`.", both(&|p| fields(p, &project))?);
    add("Folds", "A long value cut to its first lines, and More and Less.", "`ui::fold`, `ui::more_open` (`sluice-fold`).", both(&|_| Ok(folds()))?);
    add("Conversation", "Messages in one column, the message box at the end.", "`threads::Conversation` in `sluice-conversation`, `sluice-answer`; its message box a `ui::post_form`.", both(&|p| conversation(p, &project))?);
    add("Empty states", "One line where a place has nothing to show.", "`ui::empty`, `ui::empty_with`.", both(&|_| Ok(empties()))?);
    add("Menus", "The project switcher, display preferences and an item's Details.", "`ui::menu_open` (`sluice-menu`).", both(&|_| Ok(menus()))?);
    add("Copy", "An id or a SHA, with a copy button.", "`ui::copy` (`sluice-copy`).", both(&|_| Ok(copies()))?);
    add("Settings", "Display preferences: Theme, Show value types and Keyboard shortcuts, applied at once and kept.", "`views::prefs::menu` (the `theme` and `types` signals; `details.sub` in `sluice-menu`), `ui::types_toggle`.", both(&|p| Ok(toggles(p)))?);
    add("Search", "A list filtered as you type, or the plan's find.", "`ui::search_open` (`sluice-search`).", both(&|p| Ok(searches(p)))?);
    add("Banners", "Updates paused or stopped, or a newer build of sluice.", "`ui::banner` (`sluice-banner`).", both(&|_| Ok(banners()))?);
    add("Notice", "An action that was not done, and your words kept.", "`ui::notice`.", both(&|_| Ok(notices()))?);
    add("Keys", "The page's keys, listed in display preferences' Keyboard shortcuts.", "`sluice-keys`.", both(&|_| Ok(keys()))?);
    add("Splitter", "The line between the plan and the board.", "`ui::splitter` (`sluice-splitter`).", both(&|p| Ok(splitters(p)))?);
    add("Day line", "The day's runs as bars, a row a project, the now rule moving with the clock while something runs.", "`DayView::day_line` in `sluice-day`, which moves its now rule and running bars on `sluice-tick`.", both(&|_| Ok(day_lines()))?);
    add("Project page", "Plan, Plan and board or Board: text links in the page menu, a link each, the server drawing the chosen view and keeping it per project, the board beside the plan. The step drawer opens over it from any step link; its head is the Step head above.", "View links in `Plan::band`, their selection rendered by the server; the drawer `ui::drawer` in `sluice-drawer` (`sluice.js`). Each is one a page and binds the page's address, so the gallery draws their parts, not a second live instance.", both(&|_| Ok(view_choices()))?);
    add("Components", "Every component, the attributes it reads and what it does.", "Rocket components in `components.js`, light DOM.", (components(), TrustedHtml::default()));
    for part in &mut parts {
        part.wide = matches!(part.name, "Page head" | "Step head" | "Module grid" | "Modules" | "Section heads" | "Plan briefing" | "Step view");
    }
    Ok(parts)
}
async fn gallery(State(state): State<DashboardState>, headers: HeaderMap) -> Response {
    let page = async {
        let snapshot = state.snapshot(None).await?;
        let nav = NavView::new(&snapshot, None, "")?;
        let parts = parts().map_err(super::threads::render_error)?;
        let viewer = Viewer::from_headers(&headers);
        // each part in Paper and in Carbon
        let body = TrustedHtml::from_template(&GalleryTemplate { parts: &parts })
            .map_err(super::threads::render_error)?;
        super::render_layout("States and parts", &body, &nav, &viewer, "", "", "/_ui")
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
                super::ui::page_head("Agent docs", &TrustedHtml::default()),
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
                    super::ui::page_head("Agent docs", &TrustedHtml::default()),
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
                .route("/_ui/pulse", get(super::pulse::gallery))
                .route("/docs", get(agent_docs))
                .route("/docs/{topic}", get(agent_docs))
                .with_state(state.dashboard.clone())
        },
        nav: |_| vec![],
        assets: &[],
    }
}
