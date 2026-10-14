//! Tool calls (DESIGN.md, Components: calls): a quiet fold, "5 tool calls", closed by default
//! and opening without script to compact rows in data mono: the tool, its key argument clipped
//! to one line, how long it took on the right (a failure says so in words).
use super::{count, esc};
use crate::views::TrustedHtml;

/// One call as its row draws it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Call {
    pub tool: String,
    pub key: String,
    /// How long it took ("<1s", "34s"), or "" while it runs.
    pub took: String,
    pub failed: bool,
    pub running: bool,
    /// Where it is drawn whole (its row on the Activity tab), or "".
    pub href: String,
}
/// The fold: `total` calls in all (more than `calls` when only the latest are drawn), `failed`
/// of them failed, its rows newest first.
pub fn calls(total: usize, failed: usize, calls: &[Call]) -> TrustedHtml {
    if calls.is_empty() {
        return TrustedHtml::default();
    }
    let mut summary = count(total, "tool call", "tool calls");
    if failed > 0 {
        summary.push_str(&format!(", {failed} failed"));
    }
    let rows: String = calls
        .iter()
        .map(|c| {
            let end = if c.running {
                "running".to_owned()
            } else if c.failed {
                if c.took.is_empty() {
                    "failed".to_owned()
                } else {
                    format!("failed · {}", esc(&c.took))
                }
            } else {
                esc(&c.took)
            };
            let inner = format!(
                "<span class=\"call-t\">{}</span><span class=\"call-k\" title=\"{k}\">{k}</span><span class=\"call-d\">{end}</span>",
                esc(&c.tool),
                k = esc(&c.key),
            );
            format!(
                "<li class=\"call{}\">{}</li>",
                if c.failed { " is-failed" } else { "" },
                if c.href.is_empty() {
                    inner
                } else {
                    format!(
                        "<a href=\"{}\" data-tab-to=\"activity\">{inner}</a>",
                        esc(&c.href)
                    )
                }
            )
        })
        .collect();
    TrustedHtml::owned(format!(
        "<details class=\"qf calls\" data-preserve-attr=\"open\"><summary>{}</summary><ol>{rows}</ol></details>",
        esc(&summary)
    ))
}
