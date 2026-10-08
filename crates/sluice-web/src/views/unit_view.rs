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
            "Param" => {
                let name = c.str_arg(0).unwrap_or("");
                if let Some(value) = self.unit.params.get(name).filter(|v| !v.is_empty()) {
                    let _ = write!(
                        out,
                        "<span class=\"uv-param\" title=\"{}\">{}</span>",
                        esc(name),
                        esc(&sluice_model::naming::cut(value, self.chars()))
                    );
                }
            }
            "Output" => self.output(c.str_arg(0).unwrap_or(""), c.str_arg(1).unwrap_or(""), out),
            "StepStatus" => {
                if let Some(step) = self.unit.stage_step(c.str_arg(0).unwrap_or("")) {
                    let _ = write!(
                        out,
                        "<span class=\"uv-status\">{}<span>{}</span></span>",
                        ui::status(step.display_mark()),
                        esc(&step.caption())
                    );
                }
            }
            "LastMessage" => {
                // the unit's page says its last message whole, under its cards
                if self.unit.last_message.is_empty() || !self.row {
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
                    "<span class=\"uv-msg\"><span class=\"uv-from\">{}</span> · {}: {body}</span>",
                    esc(&self.unit.last_from),
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
        let _ = write!(
            out,
            "<span class=\"uv-out{}\" title=\"{} {}{}\">{}{}</span>",
            if live { " live" } else { "" },
            esc(stage),
            esc(field),
            if live { " (live progress)" } else { "" },
            if live {
                ui::glyph("running").0
            } else {
                String::new()
            },
            esc(&sluice_model::naming::cut(first.trim(), self.chars()))
        );
    }
}
/// The note a broken view leaves where it would draw: "This recipe's view does not check: …".
pub fn broken(error: &str) -> TrustedHtml {
    TrustedHtml::owned(format!(
        "<p class=\"uv-broken\" role=\"note\">{}<span>This recipe's view does not check, so its summary is left out: {}</span></p>",
        super::icons::icon(super::icons::Icon::TriangleAlert, 14, ""),
        esc(error)
    ))
}
