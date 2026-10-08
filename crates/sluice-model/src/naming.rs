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
}
/// A unit's name: its title ("" when it has none), the recipe it was made from ("" when none
/// matches), that recipe's stages, and the params its steps give back, as text (a file param as
/// its file's title).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UnitNaming {
    pub title: String,
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
    let mut text = line.trim();
    text = text.trim_start_matches('#').trim_start();
    for lead in ["- ", "* ", "+ ", "> "] {
        if let Some(rest) = text.strip_prefix(lead) {
            text = rest.trim_start();
        }
    }
    let text = text.replace("**", "").replace("__", "").replace('`', "");
    let text = text
        .trim_matches(|c: char| c == '*' || c == '_' || c.is_whitespace())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    cut(&text, TITLE_CHARS)
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
    let lines = || text.lines().map(str::trim).filter(|l| !l.is_empty());
    let line = lines()
        .find(|l| {
            let hashes = l.chars().take_while(|c| *c == '#').count();
            (1..=6).contains(&hashes) && l[hashes..].starts_with(' ')
        })
        .or_else(|| lines().find(|l| !l.starts_with("---") && !l.starts_with("```")))?;
    Some(line_title(line)).filter(|t| !t.is_empty())
}

/// Name every step and unit of a plan. `steps` is the plan document's `steps` (raw, in plan
/// order); `recipes` the project's recipes, the preferred first (a unit takes the first that
/// matches it); `read` reads a prompt file (its first 16 KiB will do), `None` when it cannot.
pub fn name_plan(
    steps: &Map<String, Value>,
    recipes: &[&Recipe],
    read: &mut dyn FnMut(&str) -> Option<String>,
) -> Naming {
    let mut units: IndexMap<String, Map<String, Value>> = IndexMap::new();
    for (id, step) in steps {
        let unit = step
            .get("tags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find_map(|t| t.strip_prefix("unit:"))
            .unwrap_or(id);
        units
            .entry(unit.to_owned())
            .or_default()
            .insert(id.clone(), step.clone());
    }
    let mut naming = Naming::default();
    let mut file_title = |path: &str| read(path).as_deref().and_then(heading);
    for (unit, members) in &units {
        let mut unit_naming = UnitNaming::default();
        let matched = recipes
            .iter()
            .find_map(|r| r.match_unit(unit, members).map(|params| (*r, params)));
        let mut title = String::new();
        let borrow = matched.is_some();
        if let Some((recipe, params)) = matched {
            let files = recipe.file_params();
            unit_naming.recipe = recipe.name().to_owned();
            unit_naming.stages = recipe.stages();
            unit_naming.params = params
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
            if let Some(template) = recipe.title() {
                title = line_title(&Recipe::fill(template, &unit_naming.params));
            }
        }
        let explicit = !title.is_empty();
        // a stage borrows its unit's title only in a recipe's unit, whose stages are one piece
        // of work; elsewhere the first titled step may describe itself alone (`borrow`)
        let own: Vec<(String, String)> = members
            .iter()
            .map(|(id, step)| (id.clone(), own_title(step, &mut file_title)))
            .collect();
        if title.is_empty() {
            title = own
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
            let step_title = if explicit {
                title.clone()
            } else if !own.is_empty() {
                own
            } else if borrow && !stage.is_empty() {
                title.clone()
            } else {
                String::new()
            };
            naming.steps.insert(
                id,
                StepName {
                    title: step_title,
                    stage,
                },
            );
        }
        unit_naming.title = title;
        naming.units.insert(unit.clone(), unit_naming);
    }
    naming
}
/// A step's own title: its doc's first line, else its prompt's title.
fn own_title(step: &Value, file_title: &mut dyn FnMut(&str) -> Option<String>) -> String {
    if let Some(doc) = step.get("doc").and_then(Value::as_str)
        && let Some(line) = doc.lines().map(str::trim).find(|l| !l.is_empty())
    {
        let title = line_title(line);
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
            (Some(Value::String(text)), _) => heading(text),
            (_, Some(Value::String(path))) => file_title(path),
            _ => None,
        };
        if let Some(title) = found {
            return title;
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lane() -> Recipe {
        Recipe::parse(
            "lane",
            &json!({
                "name": "lane",
                "title": "{ticket}: {spec}",
                "params": {"ticket": "string", "spec": "string"},
                "steps": {
                    "{unit}-fork": {"run": "f.fork", "in": {"name": {"default": "{unit}"}}},
                    "{unit}-work": {"run": "f.work", "in": {
                        "ticket": {"default": "{ticket}"}, "spec": {"file": "{spec}"},
                        "cwd": {"source": "{unit}-fork/path"}}},
                    "{unit}-rm": {"run": "f.rm", "in": {"branch": {"default": "samuel-{unit}"}}}
                }
            }),
        )
        .unwrap()
    }

    #[test]
    fn a_title_is_one_clean_line() {
        assert_eq!(
            line_title("## The **cron** driver `fires`  once "),
            "The cron driver fires once"
        );
        assert_eq!(
            heading("intro\n\n# Real title\nbody").as_deref(),
            Some("Real title")
        );
        assert_eq!(heading("first line\nsecond").as_deref(), Some("first line"));
        let long = "word ".repeat(60);
        let cut = line_title(&long);
        assert!(cut.ends_with('…') && cut.chars().count() <= TITLE_CHARS);
    }

    #[test]
    fn a_recipe_unit_is_named_from_its_params_and_files() {
        let steps = json!({
            "fig-1-fork": {"run": "f.fork", "tags": ["unit:fig-1"], "in": {"name": {"default": "fig-1"}}},
            "fig-1-work": {"run": "f.work", "tags": ["unit:fig-1"], "in": {
                "ticket": {"default": "FIG-1"}, "spec": {"file": "/specs/fig-1.md"},
                "cwd": {"source": "fig-1-fork/path"}}},
            "fig-1-rm": {"run": "f.rm", "tags": ["unit:fig-1"], "in": {"branch": {"default": "samuel-fig-1"}}},
            "solo": {"run": "x.y", "doc": "Watches main\nmore", "in": {}},
            "bare": {"run": "x.y", "in": {"spec": {"default": "# Bare heading\nbody"}}},
            "none": {"run": "x.y", "in": {}}
        });
        let recipe = lane();
        let naming = name_plan(steps.as_object().unwrap(), &[&recipe], &mut |path| {
            (path == "/specs/fig-1.md").then(|| "# Cron fires on Postgres (FIG-1)\n".to_owned())
        });
        let unit = naming.unit("fig-1").unwrap();
        assert_eq!(unit.recipe, "lane");
        assert_eq!(unit.params["ticket"], "FIG-1");
        assert_eq!(unit.title, "FIG-1: Cron fires on Postgres (FIG-1)");
        assert_eq!(unit.stages, ["fork", "work", "rm"]);
        let fork = naming.step("fig-1-fork").unwrap();
        assert_eq!(
            (fork.title.as_str(), fork.stage.as_str()),
            (unit.title.as_str(), "fork")
        );
        assert_eq!(naming.step_title("solo"), "Watches main");
        assert_eq!(naming.step_title("bare"), "Bare heading");
        assert_eq!(naming.step_title("none"), "none");
    }

    #[test]
    fn a_recipe_unit_without_a_title_borrows_its_prompt_for_every_stage() {
        let recipe = Recipe::parse(
            "two",
            &json!({"name": "two", "params": {"spec": "string"}, "steps": {
                "{unit}-fork": {"run": "a"},
                "{unit}-work": {"run": "b", "in": {"spec": {"file": "{spec}"}}}}}),
        )
        .unwrap();
        let steps = json!({
            "l-fork": {"run": "a", "tags": ["unit:l"]},
            "l-work": {"run": "b", "tags": ["unit:l"], "in": {"spec": {"file": "/s.md"}}},
            "m-fork": {"run": "a", "tags": ["unit:m"]},
            "m-work": {"run": "b", "tags": ["unit:m"], "in": {"spec": {"file": "/s.md"}}},
            "m-other": {"run": "c", "tags": ["unit:m"]}
        });
        let naming = name_plan(steps.as_object().unwrap(), &[&recipe], &mut |_| {
            Some("# Ship it".into())
        });
        assert_eq!(naming.unit("l").unwrap().recipe, "two");
        assert_eq!(naming.unit_title("l"), "Ship it");
        assert_eq!(naming.step("l-fork").unwrap().title, "Ship it");
        assert_eq!(naming.step("l-fork").unwrap().stage, "fork");
        // m has a step the recipe lacks: no recipe, so its stages keep their own names
        assert_eq!(naming.unit("m").unwrap().recipe, "");
        assert_eq!(naming.unit_title("m"), "Ship it");
        assert_eq!(naming.step_title("m-fork"), "m-fork");
        assert_eq!(naming.step_title("m-work"), "Ship it");
    }
}
