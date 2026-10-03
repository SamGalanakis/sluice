//! The compiled catalog: 28 descriptors, each equal to its source fn.json
//! where one exists, with the SPEC §10 icons and the pack's same-run retry
//! budgets.

use serde_json::Value;
use sluice_model::{rpc::JsonMap, types::Type};
use sluice_runtime::{
    builtins::{BuiltinCtx, FnFailure, catalog, dispatch},
    registry::{FnRegistry, Scope},
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn source_manifest(name: &str) -> Option<PathBuf> {
    let root = repo();
    let path = match name.split('.').next().unwrap() {
        "core" | "inline" => root.join("src/sluice/fns").join(name),
        "agent" | "decide" => root.join("packs/agents").join(name),
        "git" | "gh" => root.join("packs/git").join(name),
        "jev" => root.join("packs/jev").join(name),
        _ => return None,
    };
    Some(path.join("fn.json"))
}

const NAMES: &[&str] = &[
    "core.echo",
    "core.collect",
    "core.format",
    "core.external",
    "inline.bash",
    "inline.python",
    "message.post",
    "message.wait",
    "agent.claude",
    "agent.codex",
    "agent.devin",
    "agent.review",
    "agent.run",
    "decide.llm",
    "git.head",
    "git.merge",
    "git.push",
    "git.rebase",
    "git.worktree",
    "git.worktree_rm",
    "gh.pr",
    "gh.pr_wait",
    "gh.run_cancel",
    "gh.run_latest",
    "jev.ask",
    "jev.choice",
    "jev.score",
    "jev.noul",
];

#[test]
fn the_catalog_is_the_28_builtins_in_order() {
    let names: Vec<&str> = catalog().iter().map(|d| d.name).collect();
    assert_eq!(names, NAMES);
    assert_eq!(catalog().len(), 28);
}

#[test]
fn every_descriptor_validates_as_a_manifest() {
    for descriptor in catalog() {
        let raw = descriptor.manifest();
        let (function, errors) =
            sluice_runtime::registry::parse_manifest(&raw, None, Scope::Builtin, None);
        assert!(errors.is_empty(), "{}: {errors:?}", descriptor.name);
        assert!(function.is_some(), "{}", descriptor.name);
    }
}

#[test]
fn descriptors_equal_their_source_fn_json() {
    let mut compared = 0;
    for descriptor in catalog() {
        let Some(path) = source_manifest(descriptor.name) else {
            continue;
        };
        compared += 1;
        let manifest: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(manifest["name"], descriptor.name);
        assert_eq!(manifest["doc"], descriptor.doc, "{}", descriptor.name);
        assert_eq!(
            manifest["open"].as_bool().unwrap_or(false),
            descriptor.open,
            "{}.open",
            descriptor.name
        );
        for (key, ports) in [
            ("inputs", &descriptor.inputs),
            ("outputs", &descriptor.outputs),
        ] {
            let declared = manifest[key].as_object().unwrap();
            let expected: Vec<(&String, Type)> = declared
                .iter()
                .map(|(name, form)| (name, Type::parse(form).unwrap()))
                .collect();
            let actual: Vec<(&&str, &Type)> = ports.iter().map(|(n, t)| (n, t)).collect();
            assert_eq!(
                actual
                    .iter()
                    .map(|(n, t)| (n.to_string(), (*t).clone()))
                    .collect::<Vec<_>>(),
                expected
                    .iter()
                    .map(|(n, t)| (n.to_string(), t.clone()))
                    .collect::<Vec<_>>(),
                "{}.{key}",
                descriptor.name
            );
        }
    }
    // 26 sources: the two message fns are new names without a checked-in fn.json
    assert_eq!(compared, 26);
}

#[test]
fn retry_budgets_match_the_pack_helpers() {
    let budgets: BTreeMap<&str, (u32, u64)> = catalog()
        .iter()
        .map(|d| (d.name, (d.retry.retries, d.retry.backoff.as_secs())))
        .collect();
    for name in [
        "agent.claude",
        "agent.codex",
        "agent.devin",
        "agent.review",
        "agent.run",
    ] {
        assert_eq!(budgets[name], (3, 600), "{name}");
    }
    assert_eq!(budgets["decide.llm"], (2, 30));
    assert_eq!(budgets["gh.pr_wait"], (3, 30));
    for name in ["jev.ask", "jev.choice", "jev.score", "jev.noul"] {
        assert_eq!(budgets[name], (3, 5), "{name}");
    }
    for name in [
        "core.echo",
        "core.collect",
        "core.format",
        "core.external",
        "inline.bash",
        "inline.python",
        "message.post",
        "message.wait",
        "git.head",
        "git.merge",
        "git.push",
        "git.rebase",
        "git.worktree",
        "git.worktree_rm",
        "gh.pr",
        "gh.run_cancel",
        "gh.run_latest",
    ] {
        assert_eq!(budgets[name], (0, 30), "{name}");
    }
    assert_eq!(budgets.len(), 28);
}

/// The icon each catalog entry carries, as the source asset it was embedded
/// from (all single-colour 16x16 status glyphs, SPEC §10).
#[test]
fn icons_are_their_source_assets() {
    let icon_of = |name: &str| {
        let rel = match name {
            "core.external" => "src/sluice/fns/core.external/icon.svg",
            "message.post" => "src/sluice/fns/inbox.ask/icon.svg",
            "message.wait" => "src/sluice/fns/thread.wait/icon.svg",
            "agent.review" => "packs/agents/agent.review/icon.svg",
            "decide.llm" => "packs/agents/decide.llm/icon.svg",
            n if n.starts_with("agent.") => "packs/agents/agent.run/icon.svg",
            n if n.starts_with("git.") => "packs/git/git.head/icon.svg",
            n if n.starts_with("gh.") => "packs/git/gh.pr/icon.svg",
            _ => return None,
        };
        Some(fs::read(repo().join(rel)).unwrap())
    };
    let mut with_icons = 0;
    for descriptor in catalog() {
        match (descriptor.icon, icon_of(descriptor.name)) {
            (Some(icon), Some(expected)) => {
                with_icons += 1;
                assert_eq!(icon.media_type, "image/svg+xml", "{}", descriptor.name);
                assert_eq!(
                    icon.bytes,
                    expected.as_slice(),
                    "{} icon bytes",
                    descriptor.name
                );
            }
            (None, None) => (),
            (got, expected) => panic!(
                "{}: icon {got:?} but expected {:?}",
                descriptor.name,
                expected.map(|e| e.len())
            ),
        }
    }
    assert_eq!(with_icons, 19);
}

#[test]
fn icon_svgs_are_single_colour_status_glyphs() {
    for descriptor in catalog() {
        let Some(icon) = descriptor.icon else {
            continue;
        };
        let svg = std::str::from_utf8(icon.bytes).unwrap();
        assert!(svg.contains("viewBox=\"0 0 16 16\""), "{}", descriptor.name);
        assert!(svg.contains("currentColor"), "{}", descriptor.name);
        assert!(!svg.contains("stroke=\"#"), "{}", descriptor.name);
        assert!(!svg.contains("fill=\"#"), "{}", descriptor.name);
    }
}

#[test]
fn a_builtin_resolves_with_null_path() {
    let home = std::env::temp_dir().join(format!(
        "p4-02-catalog-{}",
        sluice_model::ids::InvocationId::new()
    ));
    fs::create_dir_all(&home).unwrap();
    let registry = FnRegistry::open(&home, vec![]);
    let echo = registry.get("core.echo", None).unwrap();
    assert_eq!(echo.scope, Scope::Builtin);
    assert_eq!(echo.dir, None);
    let detail = echo.detail();
    assert_eq!(detail["scope"], "builtin");
    assert_eq!(detail["path"], Value::Null);
    assert_eq!(detail["name"], "core.echo");
    // and a saved copy of a builtin's fn.json in fns/ collides instead of shadowing
    let dir = home.join("fns/jev.ask");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("fn.json"),
        fs::read(repo().join("packs/jev/jev.ask/fn.json")).unwrap(),
    )
    .unwrap();
    fs::write(dir.join("main.py"), "# copy\n").unwrap();
    let registry = registry.registry(None);
    let jev = registry.get("jev.ask").unwrap();
    assert_eq!(jev.scope, Scope::Builtin);
    let problems: Vec<String> = registry
        .entries()
        .iter()
        .flat_map(|e| e.errors.iter().cloned())
        .collect();
    assert!(
        problems
            .iter()
            .any(|p| p.contains("collides with the builtin fn")),
        "{problems:?}"
    );
}

#[tokio::test]
async fn unbuilt_builtins_fail_with_a_typed_not_built() {
    let ctx = BuiltinCtx::default();
    let inputs = JsonMap::default();
    match dispatch("agent.run", &inputs, &ctx).await {
        Err(FnFailure::NotBuilt(name)) => assert_eq!(name, "agent.run"),
        other => panic!("expected NotBuilt, got {other:?}"),
    }
    match dispatch("bogus.thing", &inputs, &ctx).await {
        Err(FnFailure::Terminal(message)) => {
            assert!(message.contains("bogus.thing"), "{message}")
        }
        other => panic!("expected Terminal, got {other:?}"),
    }
}
