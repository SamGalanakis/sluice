//! Every ```json block in docs/agent (the pages `docs` serves) that is a plan compiles against
//! the core catalog, and every recipe parses and substitutes into a valid
//! plan — the docs cannot rot. Ported from tests/test_docs_examples.py.
use serde_json::{Value, json};
use sluice_model::{
    plan::Plan,
    recipe::Recipe,
    rpc::{JsonMap, JsonValue, decode_json},
    types::Type,
};
use sluice_runtime::registry::FnRegistry;

const PAGES: &[&str] = &["plans", "examples", "threads", "inbox", "composing"];

/// The full builtin registry — every fn the docs may name.
fn catalog() -> sluice_runtime::dispatch::Catalog {
    let home = tempfile::tempdir().unwrap();
    let registry = FnRegistry::configured(home.path()).unwrap();
    sluice::me::catalog(&registry.registry(None))
}

fn read_page(name: &str) -> String {
    sluice_runtime::docs::PAGES
        .iter()
        .find(|(topic, _)| *topic == name)
        .map(|(_, page)| (*page).to_owned())
        .unwrap_or_else(|| panic!("docs/agent/{name}.md is not served"))
}

/// ```json fenced blocks, in page order.
fn json_blocks(page: &str) -> Vec<String> {
    let mut blocks = vec![];
    let mut rest = page;
    while let Some(start) = rest.find("```json") {
        rest = &rest[start + "```json".len()..];
        let end = rest.find("```").expect("unclosed json block");
        blocks.push(rest[..end].trim().to_string());
        rest = &rest[end + 3..];
    }
    blocks
}

/// A typed sample value, as the Python docs test gave every recipe param.
fn sample(ty: &Type) -> Value {
    match ty {
        Type::String => json!("/x/y"),
        Type::Int => json!(1),
        Type::Boolean => json!(true),
        Type::Optional(_) => Value::Null,
        _ => json!("devin"),
    }
}

fn errors(page: &str, index: usize, errors: &[sluice_model::types::PathError]) -> String {
    format!(
        "{page}-{index}: {}",
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    )
}

#[test]
fn every_plan_block_in_the_docs_compiles() {
    let catalog = catalog();
    let mut ids = vec![];
    for page in PAGES {
        for (index, block) in json_blocks(&read_page(page)).into_iter().enumerate() {
            let doc: Value = serde_json::from_str(&block)
                .unwrap_or_else(|e| panic!("{page}-{index}: not JSON: {e}"));
            if doc.get("steps").is_none() || doc.get("params").is_some() {
                continue;
            }
            ids.push(format!("{page}-{index}"));
            Plan::parse_json(block.as_bytes(), &catalog)
                .unwrap_or_else(|found| panic!("{}", errors(page, index, &found)));
        }
    }
    assert!(ids.len() >= 4, "only {} plan blocks: {ids:?}", ids.len());
    assert!(
        ids.iter().filter(|id| id.starts_with("composing-")).count() >= 3,
        "composing carries the walkthrough: {ids:?}"
    );
}

#[test]
fn every_recipe_block_in_the_docs_expands_and_compiles() {
    let catalog = catalog();
    let mut found = 0;
    for page in PAGES {
        for (index, block) in json_blocks(&read_page(page)).into_iter().enumerate() {
            let doc: Value = serde_json::from_str(&block)
                .unwrap_or_else(|e| panic!("{page}-{index}: not JSON: {e}"));
            if doc.get("params").is_none() {
                continue;
            }
            found += 1;
            let name = doc["name"].as_str().unwrap_or("doc").to_owned();
            let recipe = Recipe::parse_json(&name, block.as_bytes())
                .unwrap_or_else(|found| panic!("{}", errors(page, index, &found)));
            let mut params = JsonMap::default();
            params
                .0
                .insert("unit".into(), JsonValue::try_from(json!("u")).unwrap());
            for (name, declaration) in recipe.params() {
                if name == "unit" {
                    continue;
                }
                params.0.insert(
                    name.clone(),
                    JsonValue::try_from(sample(&declaration.ty)).unwrap(),
                );
            }
            let steps = recipe
                .substitute(&params)
                .unwrap_or_else(|found| panic!("{}", errors(page, index, &found)));
            // The substituted steps are a plan's steps object; compile it.
            let plan = decode_json(
                serde_json::to_vec(&json!({"steps": steps}))
                    .unwrap()
                    .as_slice(),
            )
            .unwrap();
            Plan::parse(&plan, &catalog)
                .unwrap_or_else(|found| panic!("{}", errors(page, index, &found)));
        }
    }
    assert!(found >= 1, "the docs carry at least one recipe");
}
