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
    pub about: &'static str,
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
        ui::tag("unit: fig-5492", "", None),
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
        "<p class=\"gal-row\"><button class=\"primary\" type=\"button\">Retry</button><button type=\"button\">Pause</button><button class=\"danger\" type=\"button\">Delete lanes</button><button type=\"button\" disabled>Unpause</button></p>"
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
    let summary = "Landed the SQLite policy split: `synchronous` is a required argument now, the operational presets moved under `SqliteConnectionPolicy.operational`, and every caller in lash-store passes one.";
    let mut by_run = FieldView::new("evidence", "string", "", Some(&"cargo test --workspace: 1,204 passed".into()), "");
    by_run.set_by = "Run 1".into();
    let outputs = vec![
        (FieldView::new("summary", "string", "What changed", Some(&summary.into()), ""), ""),
        (FieldView::new("final", "string", "", Some(&summary.into()), ""), "summary"),
        (FieldView::new("ready", "boolean", "", Some(&true.into()), ""), ""),
        (FieldView::new("tests", "integer", "", Some(&1204.into()), ""), ""),
        (by_run, ""),
        (FieldView::new("model", "object", "", Some(&serde_json::json!({"type": "normal", "model": "sol", "effort": "high"})), ""), ""),
        (FieldView::new("notes", "string", "", Some(&serde_json::Value::Null), ""), ""),
    ];
    let inputs = vec![
        (FieldView::new("spec", "string", "", Some(&serde_json::json!({"file": "specs/fig-5492.md"})), "A file, read when the run starts"), ""),
        (FieldView::new("base", "string", "", Some(&"origin/main".into()), "Its default"), ""),
        (FieldView::new("fork", "string", "", Some(&"/workspace/code/lash-wt/fig-5492".into()), "fig-5492-fork/path"), ""),
        (FieldView::new("lane", "string", "", Some(&"req-sqlite".into()), "lane"), ""),
        (FieldView::new("review", "string", "", None, "fig-5492-review/verdict"), ""),
    ];
    Ok(TrustedHtml::owned(format!(
        "<p class=\"meta gal-cap\">Outputs: which run set one, one repeated said once</p>{}<p class=\"f-unset\"><span class=\"quiet\">2 outputs not set yet:</span> <span class=\"f-uname\">verdict</span>, <span class=\"f-uname\">pr</span></p><p class=\"meta gal-cap\">Inputs: where each value came from</p>{}",
        TrustedHtml::from_template(&Fields { fields: &outputs, id: &format!("{prefix}out"), project, unset: "Not set yet." })?,
        TrustedHtml::from_template(&Fields { fields: &inputs, id: &format!("{prefix}in"), project, unset: "No value yet." })?,
    )))
}
fn folds() -> TrustedHtml {
    let long = crate::markdown::render(
        "The rebase onto origin/main conflicts in three places:\n\n- `crates/lash-conformance/src/conformance/attachment_reclamation.rs`: both sides add a case to the same table.\n- `crates/lash-durable-test/tests/fixtures/formats/lashlang`: the fixture images were re-blessed on main.\n- `crates/lash-store/src/sqlite/conn.rs`: FIG-5498 moved the operational presets.\n\nKeep both table rows, take main's fixtures and re-bless after, and move your `synchronous` argument into the new `SqliteConnectionPolicy.operational`.\n\nThen run the conformance suite again before you submit.",
    );
    let short = crate::markdown::render("Rebased; the conformance suite passes.");
    TrustedHtml::owned(format!(
        "<p class=\"meta gal-cap\">A long text, cut</p>{}<p class=\"meta gal-cap\">One that fits its first lines: no Show all (with script)</p>{}<p class=\"meta gal-cap\">A list's sentence that opens to it</p>{}<p>fig-5491-work · fig-5493-work · fig-5496-work · fig-5498-work · fig-5499-work · fig-5500-work</p>{}<p class=\"meta gal-cap\">More and Less, Read less at its end</p>{}<div class=\"md\"><p>The rest of the document: what each lane owns, and the order they land in.</p></div>{}",
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
        ui::copy("fig-5492-work", "Copy step id"),
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
        ("git.land", "Rebases, gates and lands a branch"),
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
        thread: "step-fig-5492-work".into(),
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
        step_ref("fig-5492-work", "work", "SQLite: synchronous is a required argument"),
        step_ref("fig-5498-work", "work", "SQLite operational presets and config"),
        step_ref("fig-5491-work", "work", "req-core: the requirements core"),
    ]
    .into_iter()
    .map(|s| (s.id.clone(), s))
    .collect();
    let item = |m: Message, state: &str| {
        let long = m.body.chars().count() > 700 || m.body.lines().count() > 14;
        MessageItem {
            project: *project,
            project_name: "lash".into(),
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
    let mut ask = message(152918, "fig-5498-work", "fig-5492-work", "2026-10-08T19:34:27Z", "FIG-5498 owns the SQLite operational presets: `conn.rs` and `connection_policy.rs`. Which of those do you touch, and where?", MessageVerb::Ask);
    ask.state = Some(QuestionState::Answered);
    let mut reply = message(152959, "fig-5492-work", "fig-5498-work", "2026-10-08T19:35:25Z", "Only `conn.rs`: `synchronous` becomes a required argument of `open`. Nothing in your presets moves.", MessageVerb::Reply);
    reply.to_message = Some(MessageId(152918));
    let mut yours = message(157590, "fig-5492-work", "owner", "2026-10-09T08:12:40Z", "Main's fixture images changed under me. Re-bless them on this branch, or wait for FIG-5501 to land its own?", MessageVerb::Ask);
    yours.state = Some(QuestionState::Open);
    let long = "rebase onto origin/main conflicts in crates/lash-conformance/src/conformance/attachment_reclamation.rs and in the format fixtures.\n\n".to_owned()
        + &"Keep both table rows; take main's fixture images and re-bless them after the rebase. ".repeat(12);
    let items = vec![
        item(message(152885, "orchestrator", "fig-5492-work", "2026-10-08T19:33:41Z", "The sibling lane ids in your spec are now concrete: req-core is fig-5491-work.", MessageVerb::Say), "note"),
        item(ask, "answered"),
        item(reply, "note"),
        item(message(153341, "fig-5498-work", "fig-5492-work", "2026-10-08T19:48:08Z", "SQLite operational settings are nested in `SqliteConnectionPolicy.operational` now.", MessageVerb::Say), "note"),
        item(message(156475, "orchestrator", "fig-5492-work", "2026-10-08T21:29:57Z", &long, MessageVerb::Say), "note"),
        item(message(157531, "fig-5492-work", "orchestrator", "2026-10-09T08:10:02Z", "Rebased; the conformance suite passes. Submitting after the fixture question.", MessageVerb::Say), "note"),
        item(yours, "open"),
        item(message(157611, "owner", "fig-5492-work", "2026-10-09T08:20:13Z", "Wait for FIG-5501; it lands within the hour.", MessageVerb::Say), "note"),
    ];
    let unread: BTreeSet<i64> = [157590].into();
    let here = |m: &MessageItem| format!("#message-{}", m.id());
    Conversation::build(
        Build {
            project: *project,
            subject: Some("fig-5492-work"),
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
        to: "fig-5492-work".into(),
        label: "Message to work · SQLite: synchronous is a required argument".into(),
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
        title: "Cancel work · SQLite: synchronous is a required argument".into(),
        id: "fig-5492-work".into(),
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
        title: "Delete lash?".into(),
        action: "#".into(),
        copy: "This permanently removes the plan, messages, history, functions, secrets and artifacts. This cannot be undone.".into(),
        confirm: "Delete lash".into(),
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
/// Every part, drawn twice: once per theme of the house pair, with its ids apart.
pub fn parts() -> Result<Vec<Part>, askama::Error> {
    let project = project();
    let both = |f: &dyn Fn(&str) -> Result<TrustedHtml, askama::Error>| -> Result<(TrustedHtml, TrustedHtml), askama::Error> {
        Ok((f("l-")?, f("d-")?))
    };
    let mut parts = vec![];
    let mut add = |name, about, (light, dark): (TrustedHtml, TrustedHtml)| {
        parts.push(Part { name, about, light, dark })
    };
    add("Status", "A state's glyph and word, from the status table: every page draws a status through `ui::status`, `ui::glyph` or `ui::mark`.", both(&|_| Ok(statuses()))?);
    add("Tags", "A small fact set apart (`ui::tag`): plain, muted for a closed state, gold for attention, coral only for a question waiting on you, live with the running glyph.", both(&|_| Ok(tags()))?);
    add("Buttons", "Primary for the next move, plain for the rest, the ink danger button for a delete, the Types switch.", both(&|_| Ok(buttons()))?);
    add("Tabs", "`ui::tabs_open`, `panel_open`, `panel_close`, `tabs_close` (`sluice-tabs`): an ARIA tablist (arrow keys, Home, End), the chosen tab in `current`, kept through a stream patch and mirrored into `?tab=` on a step's page; without script every panel stands stacked under its head.", both(&|p| Ok(tabs(&format!("{p}tabs-"))))?);
    add("Sections and cards", "`ui::head` over a section's rows; a card for a region of its own.", both(&|_| Ok(sections()))?);
    add("Fields", "One row for inputs, outputs and progress (`kit.html`'s `field`): the name in a narrow column, its type behind Types, where its value came from or what set it under it, the value beside it read by its kind.", both(&|p| fields(p, &project))?);
    add("Folds", "`ui::fold`, `ui::more_open` (`sluice-fold`): a long value's first lines, faded, with Show all under them, said only when it is cut; a list's sentence that opens to it; More and Less kept per project or opened on a wide screen.", both(&|_| Ok(folds()))?);
    add("Conversation", "`threads::Conversation` in `sluice-conversation`, its box `sluice-composer`, a question's answer `sluice-answer`: messages in one column, a run from one sender grouped under who sent it to whom, a line at each day and at the first unread, a question's reply under it, long bodies folded, the message box at the end.", both(&|p| conversation(p, &project))?);
    add("Empty states", "What a place says when it has nothing to show (`ui::empty`, `ui::empty_with`).", both(&|_| Ok(empties()))?);
    add("Confirmation", "`ui::Confirm` in `sluice-confirm`: the server's details, whose summary opens the shared dialog (focus starts on keep, stays in it, Escape closes it, back to the opener); dimmed while `disabled`; without script the form opens inline.", both(&|_| Ok(dialog()))?);
    add("Menus", "`ui::menu_open` around a details menu (`sluice-menu`): a click elsewhere or Escape closes it, focus back on its summary; the arrows, Home and End move through it.", both(&|_| Ok(menus()))?);
    add("Copy", "`ui::copy` (`sluice-copy`): an id or a SHA in data mono, its copy button there only with script, saying Copied for a moment.", both(&|_| Ok(copies()))?);
    add("Settings", "`ui::types_toggle` and the display preferences' `ui::setting_open` (`sluice-toggle`): applied at once, kept by /settings; every Types switch on a page follows.", both(&|_| Ok(toggles()))?);
    add("Search", "`ui::search_open` (`sluice-search`): a list filtered as one types (the functions), or the board's tools pointing the page's stream at their query; Escape clears.", both(&|p| Ok(searches(p)))?);
    add("Banners", "`ui::banner` (`sluice-banner`): the page's stream paused or stopped with Reconnect, in gold; a newer build with Reload, muted. Hidden while live.", both(&|_| Ok(banners()))?);
    add("Notice", "`ui::notice`: what a page says at its top when something asked for was not done (a step's action refused: the browser is sent back to the step's own address, the notice and what was typed held for ten minutes under a key the server made), with what to do next; read out as it appears.", both(&|_| Ok(notices()))?);
    add("Keys", "`sluice-keys`, listed under display preferences on every page: / finds on the plan, the log and functions, g then a letter goes to a section, ? opens the list; never while typing in a field, with a modifier held or under an open dialog.", both(&|_| Ok(keys()))?);
    add("Splitter", "`ui::splitter` (`sluice-splitter`): drag, the arrows (16px, 64px with Shift), Home and End; a double-click resets; the width kept per project.", both(&|p| Ok(splitters(p)))?);
    add("Components", "Every Rocket component: Rust draws its host and all in it, the component only behaves (light DOM, no content of its own). Its props are its host's attributes.", (components(), TrustedHtml::default()));
    Ok(parts)
}
async fn gallery(State(state): State<DashboardState>, headers: HeaderMap) -> Response {
    let page = async {
        let snapshot = state.snapshot(None).await?;
        let nav = NavView::new(&snapshot, None, "")?;
        let parts = parts().map_err(super::threads::render_error)?;
        let body = TrustedHtml::from_template(&GalleryTemplate { parts: &parts })
            .map_err(super::threads::render_error)?;
        super::render_layout("Kit", &body, &nav, &Viewer::from_headers(&headers), "", "", "/_ui")
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
                .with_state(state.dashboard.clone())
        },
        nav: |_| vec![],
        assets: &[],
    }
}
