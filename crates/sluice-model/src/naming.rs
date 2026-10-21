//! Human names for steps and units (SPEC §13, titles), derived from what the plan already
//! stores: each step's `doc`, its prompt (`spec`, `prompt` or `task`, a literal or a file) and,
//! for a unit made from a recipe, the recipe's `title` over the params its steps give back.
//! Nothing here is stored. This module never reads a file itself: the caller reads one.

use crate::recipe::Recipe;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The most characters of a title: cut at a word before it, with an ellipsis.
pub const TITLE_CHARS: usize = 160;
/// The inputs a step's prompt is read from, in this order.
pub const PROMPT_INPUTS: [&str; 3] = ["spec", "prompt", "task"];

/// A step's name: its title ("" when it has none, and its id names it) and its stage, the id
/// without the unit's `<unit>-` ("land"), in a unit of several steps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StepName {
    pub title: String,
    pub stage: String,
    /// Its title whole, before `TITLE_CHARS` cut it: what its own page's heading says.
    #[serde(skip)]
    pub whole: String,
}
/// A unit's name: its title ("" when it has none), the recipe it was made from ("" when none
/// matches), that recipe's stages, and the params its steps give back, as text (a file param as
/// its file's title).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UnitNaming {
    pub title: String,
    /// Its title whole, before `TITLE_CHARS` cut it: what its own page's heading says.
    #[serde(skip)]
    pub whole: String,
    pub recipe: String,
    pub stages: Vec<String>,
    pub params: IndexMap<String, String>,
}
/// Every step's and unit's name in one plan.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Naming {
    pub steps: BTreeMap<String, StepName>,
    pub units: BTreeMap<String, UnitNaming>,
}
impl Naming {
    pub fn step(&self, id: &str) -> Option<&StepName> {
        self.steps.get(id)
    }
    /// The step's title, or its id when it has none (or is not in the plan).
    pub fn step_title<'a>(&'a self, id: &'a str) -> &'a str {
        self.steps
            .get(id)
            .map(|n| n.title.as_str())
            .filter(|t| !t.is_empty())
            .unwrap_or(id)
    }
    pub fn unit(&self, unit: &str) -> Option<&UnitNaming> {
        self.units.get(unit)
    }
    /// The unit's title, or its id when it has none.
    pub fn unit_title<'a>(&'a self, unit: &'a str) -> &'a str {
        self.units
            .get(unit)
            .map(|n| n.title.as_str())
            .filter(|t| !t.is_empty())
            .unwrap_or(unit)
    }
}

/// One line as a title: markdown's marks dropped (a heading's `#`, a list's or quote's lead,
/// emphasis, code ticks), whitespace collapsed, cut at a word before `TITLE_CHARS` with "…".
pub fn line_title(line: &str) -> String {
    cut(&whole_title(line), TITLE_CHARS)
}
/// One line as a title, as `line_title` reads it but whole.
pub fn whole_title(line: &str) -> String {
    let mut text = line.trim();
    text = text.trim_start_matches('#').trim_start();
    for lead in ["- ", "* ", "+ ", "> "] {
        if let Some(rest) = text.strip_prefix(lead) {
            text = rest.trim_start();
        }
    }
    text.replace("**", "")
        .replace("__", "")
        .replace('`', "")
        .trim_matches(|c: char| c == '*' || c == '_' || c.is_whitespace())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
/// At most `chars` characters, cut at a word with "…" when longer.
pub fn cut(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_owned();
    }
    let head: String = text.chars().take(chars.saturating_sub(1)).collect();
    let at = head.rfind(char::is_whitespace).filter(|at| *at > chars / 2);
    let mut out = at.map_or(head.clone(), |at| head[..at].to_owned());
    out = out.trim_end_matches([',', ';', ':', ' ']).to_owned();
    out.push('…');
    out
}
/// A prompt's title: its first markdown heading, else its first non-empty line.
pub fn heading(text: &str) -> Option<String> {
    whole_heading(text).map(|t| cut(&t, TITLE_CHARS))
}
/// A prompt's title whole, as `heading` reads it.
pub fn whole_heading(text: &str) -> Option<String> {
    let lines = || text.lines().map(str::trim).filter(|l| !l.is_empty());
    let line = lines()
        .find(|l| {
            let hashes = l.chars().take_while(|c| *c == '#').count();
            (1..=6).contains(&hashes) && l[hashes..].starts_with(' ')
        })
        .or_else(|| lines().find(|l| !l.starts_with("---") && !l.starts_with("```")))?;
    let title = whole_title(line);
    let hashes = line.chars().take_while(|c| *c == '#').count();
    let title = if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
        title
    } else {
        prompt_title(&title)
    };
    Some(title).filter(|t| !t.is_empty())
}

/// Name every step and unit of a plan from its step rows: `steps` in position order, each
/// with its unit (the `steps.unit` index) and its declaration as written (a full read; a row
/// read without one is named by its id alone); `recipes` the project's recipes, the preferred
/// first (a unit takes the first that matches it); `read` reads a prompt file (its first
/// 16 KiB will do), `None` when it cannot.
pub fn name_plan(
    steps: &[crate::plan_rows::StepRowView],
    recipes: &[&Recipe],
    read: &mut dyn FnMut(&str) -> Option<String>,
) -> Naming {
    let mut units: IndexMap<String, Map<String, Value>> = IndexMap::new();
    for row in steps {
        let declaration = row
            .declaration
            .as_ref()
            .and_then(|declaration| serde_json::to_value(declaration).ok())
            .unwrap_or_else(|| Value::Object(Map::new()));
        units
            .entry(row.unit.to_string())
            .or_default()
            .insert(row.step.to_string(), declaration);
    }
    let mut naming = Naming::default();
    // a file's title whole; a param's text and every title are cut from it
    let mut file_title = |path: &str| read(path).as_deref().and_then(whole_heading);
    for (unit, members) in &units {
        let mut unit_naming = UnitNaming::default();
        let matched = recipes
            .iter()
            .find_map(|r| r.match_unit(unit, members).map(|params| (*r, params)));
        let mut whole = String::new();
        let borrow = matched.is_some();
        if let Some((recipe, params)) = matched {
            let files = recipe.file_params();
            unit_naming.recipe = recipe.name().to_owned();
            unit_naming.stages = recipe.stages();
            // each param as text: a file param as its file's title, whole for the unit's title
            // and cut as the params the views show
            let texts: IndexMap<String, String> = params
                .iter()
                .map(|(name, value)| {
                    let text = match value {
                        Value::String(path) if files.contains(name) => {
                            file_title(path).unwrap_or_default()
                        }
                        Value::String(text) => text.clone(),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    };
                    (name.clone(), text)
                })
                .collect();
            unit_naming.params = texts
                .iter()
                .map(|(name, text)| {
                    let cut_text = match params.get(name) {
                        Some(Value::String(_)) if files.contains(name) => cut(text, TITLE_CHARS),
                        _ => text.clone(),
                    };
                    (name.clone(), cut_text)
                })
                .collect();
            if let Some(template) = recipe.title() {
                whole = whole_title(&Recipe::fill(template, &texts));
            }
        }
        let explicit = !whole.is_empty();
        // a stage borrows its unit's title only in a recipe's unit, whose stages are one piece
        // of work; elsewhere the first titled step may describe itself alone (`borrow`)
        let own: Vec<(String, String)> = members
            .iter()
            .map(|(id, step)| (id.clone(), own_title(step, &mut file_title)))
            .collect();
        if whole.is_empty() {
            whole = own
                .iter()
                .map(|(_, t)| t)
                .find(|t| !t.is_empty())
                .cloned()
                .unwrap_or_default();
        }
        let prefix = format!("{unit}-");
        for (id, own) in own {
            let stage = if members.len() > 1 {
                id.strip_prefix(&prefix).unwrap_or("").to_owned()
            } else {
                String::new()
            };
            let step_whole = if explicit {
                whole.clone()
            } else if !own.is_empty() {
                own
            } else if borrow && !stage.is_empty() {
                whole.clone()
            } else {
                String::new()
            };
            naming.steps.insert(
                id,
                StepName {
                    title: cut(&step_whole, TITLE_CHARS),
                    stage,
                    whole: step_whole,
                },
            );
        }
        unit_naming.title = cut(&whole, TITLE_CHARS);
        unit_naming.whole = whole;
        naming.units.insert(unit.clone(), unit_naming);
    }
    naming
}
/// A body used as a title: its first sentence or line, bounded before it reaches a row.
fn prompt_title(text: &str) -> String {
    let end = text
        .char_indices()
        .find_map(|(at, c)| {
            if matches!(c, '.' | '!' | '?')
                && text[at + c.len_utf8()..].starts_with(char::is_whitespace)
            {
                Some(at + c.len_utf8())
            } else {
                None
            }
        })
        .unwrap_or(text.len());
    cut(&text[..end], 80)
}
/// A step's own title, whole: its doc's first line, else its prompt's title.
fn own_title(step: &Value, file_title: &mut dyn FnMut(&str) -> Option<String>) -> String {
    if let Some(doc) = step.get("doc").and_then(Value::as_str)
        && let Some(line) = doc.lines().map(str::trim).find(|l| !l.is_empty())
    {
        let title = whole_title(line);
        if !title.is_empty() {
            return title;
        }
    }
    let inputs = step.get("in").and_then(Value::as_object);
    for name in PROMPT_INPUTS {
        let Some(binding) = inputs.and_then(|i| i.get(name)) else {
            continue;
        };
        let found = match (binding.get("default"), binding.get("file")) {
            (Some(Value::String(text)), _) => whole_heading(text),
            (_, Some(Value::String(path))) => file_title(path),
            _ => None,
        };
        if let Some(title) = found {
            return title;
        }
    }
    String::new()
}
