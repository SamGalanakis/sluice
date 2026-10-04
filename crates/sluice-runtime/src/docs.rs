//! Agent documentation embedded in the release and shared by every interface.
use serde_json::{Value, json};
use sluice_model::error::PublicError;

pub const PAGES: &[(&str, &str)] = &[
    ("board", include_str!("../../../docs/agent/board.md")),
    (
        "composing",
        include_str!("../../../docs/agent/composing.md"),
    ),
    ("examples", include_str!("../../../docs/agent/examples.md")),
    ("fns", include_str!("../../../docs/agent/fns.md")),
    ("inbox", include_str!("../../../docs/agent/inbox.md")),
    (
        "instructions",
        include_str!("../../../docs/agent/instructions.md"),
    ),
    ("plans", include_str!("../../../docs/agent/plans.md")),
    ("threads", include_str!("../../../docs/agent/threads.md")),
    ("types", include_str!("../../../docs/agent/types.md")),
];

pub fn docs(topic: Option<&str>) -> Result<Value, PublicError> {
    match topic {
        Some(topic) => PAGES
            .iter()
            .find(|(name, _)| *name == topic)
            .map(|(_, page)| json!(page))
            .ok_or_else(|| PublicError::NotFound {
                message: format!("no docs topic {topic}"),
            }),
        None => Ok(Value::Object(
            PAGES
                .iter()
                .map(|(topic, page)| {
                    let first = page
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("");
                    (
                        (*topic).into(),
                        json!(first.trim_start_matches(['#', ' ']).trim()),
                    )
                })
                .collect(),
        )),
    }
}
