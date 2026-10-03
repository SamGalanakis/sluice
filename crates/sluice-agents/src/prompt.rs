//! Task text and declaration builders shared by standalone and composed agents.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sluice_model::types::Type;
use std::{collections::BTreeMap, fs, io, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    pub r#type: Type,
    #[serde(default)]
    pub doc: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptContext {
    pub header: String,
    pub project: String,
    pub step: String,
    pub run: String,
    pub inputs: BTreeMap<String, Port>,
    pub outputs: BTreeMap<String, Port>,
    pub listen: bool,
}
pub fn thread_name(step: &str) -> String {
    format!(
        "step-{}",
        step.to_lowercase()
            .chars()
            .map(
                |ch| if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-' {
                    ch
                } else {
                    '-'
                }
            )
            .collect::<String>()
    )
}
pub fn build(task: &str, values: &BTreeMap<String, Value>, ctx: &PromptContext) -> String {
    let mut text = String::new();
    if !ctx.header.is_empty() {
        text.push_str(&ctx.header);
        text.push_str("\n\n");
    }
    text.push_str(task);
    if !ctx.inputs.is_empty() {
        text.push_str("\n\n## Inputs\n");
        for (name, port) in &ctx.inputs {
            let value = values.get(name).unwrap_or(&Value::Null);
            let value = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| serde_json::to_string_pretty(value).expect("JSON value"));
            text.push_str(&format!(
                "\n`{name}` ({}):\n{value}\n",
                type_text(&port.r#type)
            ));
        }
    }
    if !ctx.outputs.is_empty() {
        text.push_str("\n\n## Outputs you must submit\n");
        for (name, port) in &ctx.outputs {
            text.push_str(&format!(
                "\n- `{name}` ({}): {}",
                type_text(&port.r#type),
                port.doc
            ));
        }
        let payload = json!({"project":ctx.project,"step":ctx.step,"run":ctx.run,"outputs":{}});
        let mut payload = serde_json::to_string(&payload).expect("JSON value");
        let outputs = ctx
            .outputs
            .iter()
            .map(|(name, port)| {
                format!(
                    "{}: <{}>",
                    serde_json::to_string(name).expect("name"),
                    type_text(&port.r#type)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        payload = payload.replace("\"outputs\":{}", &format!("\"outputs\":{{{outputs}}}"));
        text.push_str(&format!("\n\nSubmit them, as JSON values of those types, before you finish:\n`sluice tool step_submit {}`\nIf it returns `invalid`, fix what it lists and submit again (the last submission counts).", shell_quote(&payload)));
    }
    if ctx.listen && !ctx.project.is_empty() && !ctx.step.is_empty() {
        let thread = thread_name(&ctx.step);
        let post = json!({"project":ctx.project,"thread":thread,"from":ctx.step,"run":ctx.run,"to":"orchestrator","body":"...","needs_reply":false});
        text.push_str(&format!("\n\nMessages for you on sluice thread `{thread}` of project `{}` are delivered into this session as they arrive when addressed to this step (or to nobody); you need not poll for them. Follow instructions addressed to you. If you hit a question you cannot settle within your task, post it with `sluice tool message_post {}` and continue with anything not blocked by it. Use `needs_reply: true` for a question that needs an answer. Post questions and changes of scope, not progress.", ctx.project, shell_quote(&post.to_string())));
    }
    text
}
pub fn required_outputs(ctx: &PromptContext) -> Vec<String> {
    ctx.outputs
        .iter()
        .filter(|(_, p)| !matches!(p.r#type, Type::Optional(_)))
        .map(|(n, _)| n.clone())
        .collect()
}
pub fn hand_over(text: &str, path: &Path, description: &str) -> io::Result<String> {
    if !text.trim().contains(['\n', '\r']) && text.chars().count() <= 500 {
        return Ok(text.trim().into());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)?;
    Ok(format!(
        "{description} is in {}; read it fully, then do it.",
        fs::canonicalize(path)?.display()
    ))
}
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}
fn type_text(t: &Type) -> String {
    t.form()
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| t.form().to_string())
}
pub fn review(base: &str, standards: &str, notes: &str) -> String {
    format!(
        "You are reviewing a branch in the git repository at your working directory.\n\nReview the diff `git diff {base}...HEAD` against the coding standards file at {standards}. Read it first, then the diff.\n\n{notes}\n\nFix every problem by editing files and committing. Use plain-sentence commit messages without AI attribution. Hunt for tautological tests and fix or delete them. Do not rewrite history, force-push or push. Your final message is a short prose report of only what you could not fix."
    )
}
pub fn decision(
    question: &str,
    options: &[String],
    context: Option<&Value>,
) -> io::Result<(String, BTreeMap<String, Port>)> {
    if options.is_empty()
        || options.iter().any(String::is_empty)
        || options
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != options.len()
    {
        return Err(io::Error::other(
            "options must be nonempty distinct strings",
        ));
    }
    let text = format!(
        "Answer the question by picking exactly one listed option.\n\nQuestion: {question}\nContext: {}\nOptions: {}\n\nSubmit choice (verbatim) and p (probability 0..1 that this choice is right).",
        context.unwrap_or(&Value::Null),
        json!(options)
    );
    Ok((
        text,
        BTreeMap::from([
            (
                "choice".into(),
                Port {
                    r#type: Type::Enum(options.to_vec()),
                    doc: "The selected option".into(),
                },
            ),
            (
                "p".into(),
                Port {
                    r#type: Type::Float,
                    doc: "Probability 0..1".into(),
                },
            ),
        ]),
    ))
}
