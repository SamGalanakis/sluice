//! Cutover files are data inputs. Validation never launches a live project function.
use std::{fs, path::PathBuf, process::Command};

use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::{
    ids::{StepId, UnitName},
    plan::{FnSignature, Plan},
    recipe::{ExpansionOptions, Recipe},
    rpc::{JsonMap, decode_json},
    types::Type,
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn map(value: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn signature(raw: &Value) -> FnSignature {
    let ports = |name: &str| {
        raw[name]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(name, form)| {
                let form = if form.get("doc").is_some() {
                    form.get("type").unwrap()
                } else {
                    form
                };
                (name.clone(), Type::parse(form).unwrap())
            })
            .collect()
    };
    FnSignature {
        inputs: ports("inputs"),
        outputs: ports("outputs"),
        open: raw["open"].as_bool().unwrap_or(false),
        ..FnSignature::default()
    }
}
fn manifests(project: &str) -> Vec<PathBuf> {
    let mut paths: Vec<_> =
        fs::read_dir(root().join(format!("cutover-staging/projects/{project}/fns")))
            .unwrap()
            .map(|entry| entry.unwrap().path().join("fn.json"))
            .filter(|path| path.is_file())
            .collect();
    paths.sort();
    paths
}
fn signatures(project: &str) -> IndexMap<String, FnSignature> {
    let mut registry = IndexMap::new();
    for path in manifests(project) {
        let raw: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        registry.insert(raw["name"].as_str().unwrap().to_owned(), signature(&raw));
    }
    let gh: Value =
        serde_json::from_slice(&fs::read(root().join("packs/git/gh.pr_wait/fn.json")).unwrap())
            .unwrap();
    registry.insert("gh.pr_wait".into(), signature(&gh));
    registry.insert(
        "core.external".into(),
        FnSignature {
            open: true,
            ..FnSignature::default()
        },
    );
    registry
}

#[test]
fn all_eighteen_staged_manifest_signatures_validate() {
    let mut count = 0;
    for project in ["lash", "figments"] {
        for path in manifests(project) {
            let bytes = fs::read(&path).unwrap();
            let raw: Value = serde_json::from_slice(&bytes).unwrap();
            let name = raw["name"].as_str().unwrap();
            assert_eq!(path.parent().unwrap().file_name().unwrap(), name);
            let registry = IndexMap::from([(name.to_owned(), signature(&raw))]);
            // Compile each signature in a plan as well as parsing its complete type forms.
            let inputs: Value = raw["inputs"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(name, form)| (name.clone(), json!({"type":form})))
                .collect();
            let bindings: Value = raw["inputs"]
                .as_object()
                .unwrap()
                .keys()
                .map(|name| (name.clone(), json!({"source":name})))
                .collect();
            let document =
                map(json!({"inputs":inputs,"steps":{"validate":{"run":name,"in":bindings}}}));
            Plan::parse(&document, &registry).unwrap_or_else(|errors| {
                panic!("{}: {errors:?}", path.display());
            });
            count += 1;
        }
    }
    assert_eq!(count, 18);
}

#[test]
fn all_six_staged_recipes_expand_with_delivery_exits_and_intended_gates() {
    let samples = json!({"unit":"sample","ticket":"FIG-1","spec":"/tmp/spec.md",
        "engine":"codex","model":"sol","effort":"high","base":"origin/main","scope":"crates/*"});
    for (project, name, size, exit, cleanup) in [
        ("lash", "lane", 6, "landed", "sample-close"),
        ("lash", "branch", 3, "work", "sample-work?"),
        ("lash", "study", 3, "work", "sample-work?"),
        ("lash", "test-audit", 3, "work", "ta-integrate-work"),
        ("figments", "pr", 4, "work", "sample-checks?"),
        ("figments", "study", 3, "work", "sample-work?"),
    ] {
        let path = root().join(format!(
            "cutover-staging/projects/{project}/recipes/{name}.json"
        ));
        let recipe = Recipe::parse_json(name, &fs::read(path).unwrap()).unwrap();
        let registry = signatures(project);
        let base = json!({"steps":{
            "ta-history-work":{"run":"core.external"},
            "ta-integrate-work":{"run":"core.external"}
        }});
        // Include an engine with null optional params to exercise typed substitution.
        for engine in ["codex", "devin", "opus"] {
            let mut values = samples.clone();
            values["engine"] = json!(engine);
            if engine != "codex" {
                values["model"] = Value::Null;
                values["effort"] = Value::Null;
            }
            let params = map(Value::Object(
                values
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter(|(k, _)| recipe.params().contains_key(*k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ));
            let steps = recipe
                .expand(
                    &params,
                    &ExpansionOptions::default(),
                    &map(base.clone()),
                    &registry,
                )
                .unwrap_or_else(|errors| panic!("{project}/{name}/{engine}: {errors:?}"));
            assert_eq!(steps.0.len(), size);
            let mut combined = base.clone();
            combined["steps"].as_object_mut().unwrap().extend(
                serde_json::to_value(&steps)
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .clone(),
            );
            let plan = Plan::parse(&map(combined), &registry).unwrap();
            assert_eq!(
                plan.units()[&UnitName::new("sample").unwrap()].exits,
                vec![StepId::new(format!("sample-{exit}")).unwrap()]
            );
            let rm = &plan.steps()[&StepId::new("sample-rm").unwrap()];
            assert_eq!(
                rm.after.iter().map(|gate| gate.entry()).collect::<Vec<_>>(),
                [cleanup]
            );
            if name == "lane" {
                assert_eq!(
                    steps.0["sample-close"].as_value()["after"],
                    json!(["sample-land/landed", "sample-landed"])
                );
                assert_eq!(
                    steps.0["sample-land"].as_value()["in"]["work_step"],
                    json!({"default":"sample-work"})
                );
                assert!(steps.0["sample-land"].as_value().get("after").is_none());
            }
            if name == "test-audit" {
                assert_eq!(
                    steps.0["sample-work"].as_value()["after"],
                    json!(["ta-history-work"])
                );
            }
        }
    }
}

#[test]
fn converted_project_logic_runs_offline() {
    let output = Command::new("uv")
        .args([
            "run",
            "--no-sync",
            "pytest",
            "-q",
            "-n0",
            "-rs",
            "cutover-staging",
        ])
        .current_dir(root())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .expect("uv and the synced Python development environment are required");
    assert!(
        output.status.success(),
        "offline staging tests failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn test_audit_keeps_all_twenty_five_cross_unit_handoffs() {
    let bytes = include_bytes!("fixtures/staging/audit-fan-in.json");
    let document: JsonMap = decode_json(bytes).unwrap();
    let registry = signatures("lash");
    let plan = Plan::parse(&document, &registry).unwrap();
    assert_eq!(plan.units().len(), 26);
    let step = &plan.steps()[&StepId::new("ta-integrate-work").unwrap()];
    assert_eq!(step.bindings["lanes"].references().len(), 25);
    assert!(step.bindings["lanes"].references().iter().all(|reference| {
        reference
            .parts()
            .unwrap()
            .step
            .is_some_and(|source| plan.steps()[&source].unit_name() != step.unit_name())
    }));
}
