//! A recipe's unit view (`docs("plans")`, recipes): an OpenUI program bound to one unit, drawn
//! on the server inside the frame sluice owns (the lane matrix's row, the unit page). Its
//! vocabulary is `openui::VIEW_COMPONENTS`; every value is escaped and every `{param}` is the
//! unit's param, read back from its steps.
use super::TrustedHtml;
use super::board::UnitView;
use super::ui::{self, esc};
use sluice_model::{openui::Component, recipe::Recipe};
use std::fmt::Write;

/// How much of a value or message a view shows in a matrix row before an ellipsis.
const ROW_CHARS: usize = 140;

/// The unit's view drawn: `row` for the lane matrix's summary cell (one line of parts), else
/// the unit page's block.
pub fn draw(root: &Component, unit: &UnitView, row: bool) -> TrustedHtml {
    let mut out = String::new();
    Draw { unit, row }.component(root, &mut out);
    TrustedHtml::owned(format!(
        "<div class=\"uv{}\">{out}</div>",
        if row { " uv-in-row" } else { " uv-page" }
    ))
}

/// Whether a drawn view says nothing (a view of params alone, which the unit's Details hold).
pub fn empty(html: &TrustedHtml) -> bool {
    let mut tag = false;
    !html.as_str().chars().any(|c| match c {
        '<' => {
            tag = true;
            false
        }
        '>' => {
            tag = false;
            false
        }
        c => !tag && !c.is_whitespace(),
    })
}
/// A view short enough to say on its unit's meta line ("land a1b2c3d"): one line of 60
/// characters or fewer once its markup is out, no list or block in it.
pub fn short(html: &TrustedHtml) -> bool {
    let html = html.as_str();
    if html.contains("<ul") || html.contains("<ol") || html.contains("<pre") || html.contains("<p")
    {
        return false;
    }
    let mut text = String::new();
    let mut tag = false;
    for c in html.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            c if !tag => text.push(c),
            _ => {}
        }
    }
    let words = text.split_whitespace().collect::<Vec<_>>().join(" ");
    !words.is_empty() && words.chars().count() <= 60
}
struct Draw<'a> {
    unit: &'a UnitView,
    row: bool,
}
impl Draw<'_> {
    fn fill(&self, text: &str) -> String {
        Recipe::fill(text, &self.unit.params)
    }
    fn chars(&self) -> usize {
        if self.row { ROW_CHARS } else { 600 }
    }
    /// On the unit page a value is named ("land a1b2c3d"): the matrix's column head names them
    /// there.
    fn label(&self, name: &str) -> String {
        if self.row {
            String::new()
        } else {
            format!(
                "<span class=\"uv-k\">{}</span> ",
                esc(&name.replace(['_', '-'], " "))
            )
        }
    }
    fn component(&self, c: &Component, out: &mut String) {
        match c.name.as_str() {
            "Stack" => {
                let row = c.str_arg(1) == Some("row");
                let _ = write!(
                    out,
                    "<div class=\"uv-stack {}\">",
                    if row { "uv-r" } else { "uv-c" }
                );
                for child in c.components_arg(0) {
                    self.component(child, out);
                }
                out.push_str("</div>");
            }
            "Text" => {
                let muted = c.str_arg(1) == Some("muted");
                let _ = write!(
                    out,
                    "<span class=\"uv-text{}\">{}</span>",
                    if muted { " muted" } else { "" },
                    esc(&sluice_model::naming::cut(
                        &self.fill(c.str_arg(0).unwrap_or("")),
                        self.chars()
                    ))
                );
            }
            "Markdown" => {
                let (html, _) =
                    crate::markdown::excerpt(&self.fill(c.str_arg(0).unwrap_or("")), self.chars());
                let _ = write!(out, "<span class=\"uv-md\">{html}</span>");
            }
            "Link" => {
                let href = self.fill(c.str_arg(1).unwrap_or(""));
                let safe = href.starts_with("https://")
                    || href.starts_with("http://")
                    || (href.starts_with('/') && !href.starts_with("//"));
                let label = esc(&self.fill(c.str_arg(0).unwrap_or("")));
                if safe {
                    let _ = write!(
                        out,
                        "<a class=\"uv-link\" href=\"{}\">{label}</a>",
                        esc(&href)
                    );
                } else {
                    let _ = write!(out, "<span class=\"uv-text\">{label}</span>");
                }
            }
            // a param is how the unit was made, not how it stands: the unit's Details say it,
            // on its row and on its page
            "Param" => {}
            "Output" => self.output(c.str_arg(0).unwrap_or(""), c.str_arg(1).unwrap_or(""), out),
            "StepStatus" => {
                if let Some(step) = self.unit.stage_step(c.str_arg(0).unwrap_or("")) {
                    let _ = write!(
                        out,
                        "<span class=\"uv-status\">{}{}<span>{}</span></span>",
                        self.label(c.str_arg(0).unwrap_or("")),
                        ui::status(step.shown()),
                        esc(&step.caption())
                    );
                }
            }
            "LastMessage" => {
                // the unit's page says its last message whole, under its cards
                if !self.row {
                    return;
                }
                // a row whose step waits on the owner says its question, not its last note
                if let Some(ask) = self.unit.asking_step().and_then(|s| s.asking.as_ref()) {
                    let _ = write!(
                        out,
                        "<span class=\"uv-msg uv-ask\"><span class=\"uv-from\">Question for you</span>: {}</span>",
                        esc(&sluice_model::naming::cut(&ask.title, ROW_CHARS * 2))
                    );
                    return;
                }
                if self.unit.last_message.is_empty() {
                    return;
                }
                let chars = c
                    .num_arg(0)
                    .map(|n| n as usize)
                    .unwrap_or(ROW_CHARS)
                    .min(if self.row { ROW_CHARS * 2 } else { usize::MAX });
                let (body, _) = crate::markdown::excerpt(&self.unit.last_message, chars);
                let _ = write!(
                    out,
                    "<span class=\"uv-msg\"><b class=\"uv-from\">{}{}</b> {}: {body}</span>",
                    if self.unit.last_received {
                        "Note from "
                    } else {
                        ""
                    },
                    esc(&self.unit.sender()),
                    ui::ago(&self.unit.changed)
                );
            }
            _ => {}
        }
    }
    /// A stage's output as the board's Output reads it: its progress while that is fresher
    /// (marked live while it runs), else its output; nothing until it has one.
    fn output(&self, stage: &str, field: &str, out: &mut String) {
        let Some(step) = self.unit.stage_step(stage) else {
            return;
        };
        let progress = step.progress.as_ref().and_then(|p| {
            p.fields
                .iter()
                .find(|f| f.name == field)
                .map(|f| (f, p.live))
        });
        let (value, live) = match progress {
            Some((f, live)) => (f, live),
            None => match step.outputs.iter().find(|f| f.name == field && f.available) {
                Some(f) => (f, false),
                None => return,
            },
        };
        if value.kind == "null" || value.value.is_empty() {
            return;
        }
        let first = value
            .value
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        // a row shows what explains: a hash, an id, a path or a long token is its Details'
        if self.row && ui::ValueSet::of_text(first) == ui::ValueSet::Detail {
            return;
        }
        let _ = write!(
            out,
            "<span class=\"uv-out{}\" title=\"{} {}{}\">{}{}{}</span>",
            if live { " live" } else { "" },
            esc(stage),
            esc(field),
            if live { " (live progress)" } else { "" },
            self.label(field),
            if live {
                ui::mark(ui::Shown::Running).0
            } else {
                String::new()
            },
            esc(&sluice_model::naming::cut(first.trim(), self.chars()))
        );
    }
}
/// What a view shows, named for a column's head: each param, output and stage status by its
/// name and the last message as "last message", in the view's order, the first word
/// capitalised ("Engine · last message"); "" when it names nothing (only text and links).
pub fn head(root: &Component) -> String {
    fn walk(c: &Component, out: &mut Vec<String>) {
        let named = match c.name.as_str() {
            "Stack" => {
                for child in c.components_arg(0) {
                    walk(child, out);
                }
                None
            }
            "Param" | "StepStatus" => c.str_arg(0).map(|n| n.replace(['_', '-'], " ")),
            "Output" => c.str_arg(1).map(|n| n.replace(['_', '-'], " ")),
            "LastMessage" => Some("last message".into()),
            _ => None,
        };
        if let Some(name) = named.filter(|n| !n.trim().is_empty())
            && !out.contains(&name)
        {
            out.push(name);
        }
    }
    let mut names = vec![];
    walk(root, &mut names);
    let words = names.join(" · ");
    let mut chars = words.chars();
    chars
        .next()
        .map(|f| f.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
/// The note a broken view leaves where it would draw: "This recipe's view does not check: …".
pub fn broken(error: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<p class=\"uv-broken\" role=\"note\">{}<span>This recipe's view does not check, so its summary is left out: {}</span></p>",
        super::icons::icon(super::icons::Icon::TriangleAlert, 14, ""),
        esc(error)
    ))
}
