//! The registry over scratch homes: scope precedence, collisions and blocking,
//! manifest and icon validation, fn_save's atomic generations, the pinned
//! bundle a run sees, and notify/fingerprint invalidation.

use serde_json::{Value, json};
use sluice_model::{
    error::PublicError,
    ids::{AttemptId, InvocationId, ProjectId, ProjectSelector, RunId},
    types::Type,
};
use sluice_process::host::guard_scratch_home;
use sluice_runtime::registry::{self, FnDispatch, FnIcon, FnRegistry, Scope};
use sluice_store::{
    RetrySafety, Writer, artifacts,
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("p4-02-{}", InvocationId::new()));
    fs::create_dir_all(&dir).unwrap();
    guard_scratch_home(&dir).unwrap()
}
fn manifest(name: &str) -> Value {
    json!({
        "name": name,
        "doc": "a test fn",
        "inputs": {"x": "int"},
        "outputs": {"y": "string"},
    })
}
/// Write fns_root/<dir_name>/{fn.json,main.py} and return the fn dir.
fn good_fn(fns_root: &Path, dir_name: &str, raw: &Value) -> PathBuf {
    let dir = fns_root.join(dir_name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("fn.json"),
        format!("{}\n", serde_json::to_string_pretty(raw).unwrap()),
    )
    .unwrap();
    fs::write(dir.join("main.py"), "# test fn\n").unwrap();
    dir
}
fn fns_dir(home: &Path) -> PathBuf {
    home.join("fns")
}
fn project_fns(home: &Path, project: ProjectId) -> PathBuf {
    home.join("projects").join(project.to_string()).join("fns")
}
fn errors_of(
    registry: &sluice_runtime::registry::Registry,
    name: &str,
    scope: Scope,
) -> Vec<String> {
    registry
        .entries()
        .iter()
        .find(|e| e.name == name && e.scope == scope)
        .map(|e| e.errors.clone())
        .unwrap_or_default()
}
const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor"><path d="M2 8h12"/></svg>"#;
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";
const WEBP: &[u8] = b"RIFF\x04\x00\x00\x00WEBPvp8";

// ---- scanning, precedence, collisions ---------------------------------------

#[test]
fn builtin_wins_over_a_global_of_the_same_name() {
    let home = scratch();
    good_fn(&fns_dir(&home), "core.echo", &manifest("core.echo"));
    let reg = FnRegistry::open(&home, vec![]);
    let registry = reg.registry(None);
    assert_eq!(registry.get("core.echo").unwrap().scope, Scope::Builtin);
    let errors = errors_of(&registry, "core.echo", Scope::Global);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("collides with the builtin fn")),
        "{errors:?}"
    );
    // a global collision is a problem but never blocks the project
    assert!(registry.blocking().is_empty());
    assert!(!registry.problems().is_empty());
}

#[test]
fn scope_order_is_builtin_global_fn_dirs_then_project() {
    let home = scratch();
    let extra_a = scratch();
    let extra_b = scratch();
    // the same name in home/fns, both fn_dirs and the project: home wins
    for root in [fns_dir(&home), extra_a.clone(), extra_b.clone()] {
        good_fn(&root, "x.same", &manifest("x.same"));
    }
    let project = ProjectId::new();
    good_fn(&project_fns(&home, project), "x.same", &manifest("x.same"));
    // fn_dirs order: extra_a wins over extra_b for its own name
    good_fn(&extra_a, "x.dup", &manifest("x.dup"));
    good_fn(&extra_b, "x.dup", &manifest("x.dup"));
    good_fn(&extra_b, "x.only_b", &manifest("x.only_b"));

    let reg = FnRegistry::open(&home, vec![extra_a.clone(), extra_b.clone()]);
    let registry = reg.registry(Some(project));
    let winner = registry.get("x.same").unwrap();
    assert_eq!(winner.scope, Scope::Global);
    assert_eq!(
        winner.dir.as_deref(),
        Some(fns_dir(&home).join("x.same").as_path())
    );
    let dup = registry.get("x.dup").unwrap();
    assert_eq!(dup.dir.as_deref(), Some(extra_a.join("x.dup").as_path()));
    // later-scope copies each carry the collision error
    for entry in registry.entries() {
        if entry.name == "x.dup" && entry.dir.as_deref() == Some(extra_b.join("x.dup").as_path()) {
            assert!(
                entry.errors.iter().any(|e| e.contains("collides")),
                "{entry:?}"
            );
        }
    }
    // the project copy's collision blocks the project
    assert!(
        registry
            .blocking()
            .iter()
            .any(|p| p.message.contains("x.same"))
    );
    // but the global view is unaffected
    let global = reg.registry(None);
    assert!(global.blocking().is_empty());
    assert_eq!(global.get("x.same").unwrap().scope, Scope::Global);
    // a project-only name resolves for the project
    good_fn(&project_fns(&home, project), "x.mine", &manifest("x.mine"));
    let registry = reg.registry(Some(project));
    assert_eq!(registry.get("x.mine").unwrap().scope, Scope::Project);
    assert!(reg.registry(None).get("x.mine").is_none());
}

#[test]
fn broken_fns_are_isolated_problems() {
    let home = scratch();
    let fns = fns_dir(&home);
    // a fn dir per failure shape; x.fine must still resolve beside them
    good_fn(&fns, "x.fine", &manifest("x.fine"));
    let bad_json = fns.join("x.badjson");
    fs::create_dir_all(&bad_json).unwrap();
    fs::write(bad_json.join("fn.json"), "{nope").unwrap();
    good_fn(
        &fns,
        "x.mismatch",
        &json!({"name": "x.other", "inputs": {}, "outputs": {}}),
    );
    let no_main = fns.join("x.nomain");
    fs::create_dir_all(&no_main).unwrap();
    fs::write(
        no_main.join("fn.json"),
        serde_json::to_string(&manifest("x.nomain")).unwrap(),
    )
    .unwrap();
    good_fn(
        &fns,
        "x.badname",
        &json!({"name": "BadName", "inputs": {}, "outputs": {}}),
    );
    good_fn(
        &fns,
        "x.extra",
        &json!({"name": "x.extra", "bogus": 1, "inputs": {}, "outputs": {}}),
    );
    good_fn(
        &fns,
        "x.noout",
        &json!({"name": "x.noout", "inputs": {"a": "int"}}),
    );
    good_fn(
        &fns,
        "x.badtype",
        &json!({"name": "x.badtype", "inputs": {"a": "bogus"}, "outputs": {}}),
    );
    good_fn(&fns, "x.notobject", &json!([1, 2]));

    let registry = FnRegistry::open(&home, vec![]).registry(None);
    assert!(registry.get("x.fine").is_some());
    let messages = |name: &str| errors_of(&registry, name, Scope::Global).join("; ");
    assert!(
        messages("x.badjson").contains("bad JSON"),
        "{}",
        messages("x.badjson")
    );
    // the entry is named by the manifest name even when it loses its dir name
    assert!(
        messages("x.other").contains("name x.other does not match its directory x.mismatch"),
        "{}",
        messages("x.other")
    );
    assert!(
        messages("x.nomain").contains("main.py is missing"),
        "{}",
        messages("x.nomain")
    );
    assert!(
        messages("BadName").contains("name must be dotted lowercase"),
        "{}",
        messages("BadName")
    );
    assert!(
        messages("x.extra").contains("unknown key 'bogus'"),
        "{}",
        messages("x.extra")
    );
    assert!(
        messages("x.noout").contains("outputs is required"),
        "{}",
        messages("x.noout")
    );
    assert!(
        messages("x.badtype").contains("inputs.a"),
        "{}",
        messages("x.badtype")
    );
    assert!(
        messages("x.notobject").contains("expected an object"),
        "{}",
        messages("x.notobject")
    );
    // error entries are in the listing with `error`; the good one is not
    let listing = registry.listing();
    let bad = listing.iter().find(|e| e["name"] == "x.badjson").unwrap();
    assert!(bad.get("error").is_some());
    let fine = listing.iter().find(|e| e["name"] == "x.fine").unwrap();
    assert!(fine.get("error").is_none());
    assert!(registry.blocking().is_empty());
}

#[test]
fn non_fn_dirs_and_files_are_ignored() {
    let home = scratch();
    let fns = fns_dir(&home);
    for helper in ["_lib", "tests", "examples", "generations"] {
        let dir = fns.join(helper);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("helper.py"), "x = 1\n").unwrap();
    }
    fs::write(fns.join("README.md"), "docs\n").unwrap();
    good_fn(&fns, "x.real", &manifest("x.real"));
    let registry = FnRegistry::open(&home, vec![]).registry(None);
    assert_eq!(registry.problems().len(), 0);
    let names: Vec<&str> = registry.names();
    assert!(names.contains(&"x.real"));
    for helper in ["_lib", "tests", "examples"] {
        assert!(!names.iter().any(|n| n.contains(helper)));
    }
}

#[test]
fn a_missing_configured_fn_dir_is_a_problem_not_a_crash() {
    let home = scratch();
    let missing = home.join("absent");
    let reg = FnRegistry::open(&home, vec![missing.clone()]);
    let registry = reg.registry(None);
    assert!(
        registry
            .problems()
            .iter()
            .any(|p| p.location == missing.display().to_string()
                && p.message == "fn directory does not exist"),
        "{:?}",
        registry.problems()
    );
    // a missing home fns dir is fine
    let home2 = scratch();
    assert!(
        FnRegistry::open(&home2, vec![])
            .registry(None)
            .problems()
            .is_empty()
    );
}

#[test]
fn only_the_projects_own_problems_block_it() {
    let home = scratch();
    let project = ProjectId::new();
    // a broken project fn blocks the project; a broken global one does not
    let broken = project_fns(&home, project).join("x.broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("fn.json"), "not json").unwrap();
    good_fn(
        &project_fns(&home, project),
        "core.echo",
        &manifest("core.echo"),
    );
    let fns = fns_dir(&home);
    let global_bad = fns.join("x.globalbad");
    fs::create_dir_all(&global_bad).unwrap();
    fs::write(global_bad.join("fn.json"), "nope").unwrap();

    let reg = FnRegistry::open(&home, vec![]);
    let registry = reg.registry(Some(project));
    assert_eq!(registry.blocking().len(), 2, "{:?}", registry.blocking());
    match reg.usable_registry(Some(project)) {
        Err(PublicError::Invalid { errors, .. }) => {
            assert_eq!(errors.len(), 2, "{errors:?}");
        }
        other => panic!("expected blocking Invalid, got {other:?}"),
    }
    // the global view is usable: the broken global fn is a problem, not a block
    assert!(reg.usable_registry(None).is_ok());
    assert_eq!(reg.registry(None).problems().len(), 1);
    // and the builtin still wins under the project's broken copy
    assert_eq!(
        reg.registry(Some(project)).get("core.echo").unwrap().scope,
        Scope::Builtin
    );
}

// ---- manifests: names, ports, open/submits -----------------------------------

#[test]
fn manifest_rules_are_the_fn_json_contract() {
    // valid ports parse in order with their declared types
    let (function, errs) = registry::parse_manifest(
        &json!({
            "name": "x.types",
            "doc": "d",
            "inputs": {"a": "int", "b": {"type": "array", "items": "string"}},
            "outputs": {"o": "int?"},
        }),
        None,
        Scope::Global,
        None,
    );
    assert!(errs.is_empty(), "{errs:?}");
    let function = function.unwrap();
    assert_eq!(function.inputs["b"], Type::List(Box::new(Type::String)));
    assert_eq!(function.outputs["o"], Type::Optional(Box::new(Type::Int)));

    // name grammar: needs a dot, lowercase-dotted segments
    for bad in ["nodot", "Upper.case", "x.1bad", "x..y", "x.Y", ".x", "x."] {
        let (_, errs) = registry::parse_manifest(&manifest(bad), None, Scope::Global, None);
        assert!(
            errs.iter().any(|e| e.contains("dotted lowercase")),
            "{bad}: {errs:?}"
        );
    }
    for good in ["x.y", "a.b.c", "x_y.z9", "x_.y_"] {
        let (function, errs) = registry::parse_manifest(&manifest(good), None, Scope::Global, None);
        assert!(errs.is_empty(), "{good}: {errs:?}");
        assert_eq!(function.unwrap().name, good);
    }

    // submits requires open; a submit may not shadow an output
    let (_, errs) = registry::parse_manifest(
        &json!({
            "name": "x.s", "inputs": {}, "outputs": {},
            "submits": {"out": "int"},
        }),
        None,
        Scope::Global,
        None,
    );
    assert!(
        errs.iter().any(|e| e.contains("submits needs open: true")),
        "{errs:?}"
    );
    let (_, errs) = registry::parse_manifest(
        &json!({
            "name": "x.s", "open": true, "inputs": {},
            "outputs": {"out": "int"},
            "submits": {"out": "int"},
        }),
        None,
        Scope::Global,
        None,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("submits.out: already an output")),
        "{errs:?}"
    );
    // a valid open fn with typed + documented submits
    let (function, errs) = registry::parse_manifest(
        &json!({
            "name": "x.s", "open": true, "inputs": {}, "outputs": {},
            "submits": {"answer": {"type": "int?", "doc": "the answer"}, "tag": "string"},
        }),
        None,
        Scope::Global,
        None,
    );
    assert!(errs.is_empty(), "{errs:?}");
    let function = function.unwrap();
    assert!(function.open);
    assert_eq!(
        function.submits["answer"],
        Type::Optional(Box::new(Type::Int))
    );
    assert_eq!(function.submit_docs["answer"], "the answer");
}

// ---- icons --------------------------------------------------------------------

#[test]
fn file_icons_validate_and_beat_text_icons() {
    let home = scratch();
    let fns = fns_dir(&home);
    let svg_fn = fns.join("x.svg");
    fs::create_dir_all(&svg_fn).unwrap();
    fs::write(
        svg_fn.join("fn.json"),
        serde_json::to_string(&json!({
            "name": "x.svg", "icon": "text-loses",
            "inputs": {}, "outputs": {},
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(svg_fn.join("main.py"), "x\n").unwrap();
    fs::write(svg_fn.join("icon.svg"), SVG).unwrap();
    let png_fn = good_fn(&fns, "x.png", &manifest("x.png"));
    fs::write(png_fn.join("icon.png"), PNG).unwrap();
    let webp_fn = good_fn(&fns, "x.webp", &manifest("x.webp"));
    fs::write(webp_fn.join("icon.webp"), WEBP).unwrap();
    let text_fn = good_fn(
        &fns,
        "x.text",
        &json!({"name": "x.text", "icon": "  gear  ", "inputs": {}, "outputs": {}}),
    );
    let _ = text_fn;

    let registry = FnRegistry::open(&home, vec![]).registry(None);
    match registry.get("x.svg").unwrap().icon.as_ref().unwrap() {
        FnIcon::Image {
            media_type,
            bytes,
            hash,
        } => {
            assert_eq!(media_type, "image/svg+xml");
            assert_eq!(bytes.as_slice(), SVG);
            assert_eq!(hash.len(), 64);
        }
        other => panic!("expected image icon, got {other:?}"),
    }
    for (name, media) in [("x.png", "image/png"), ("x.webp", "image/webp")] {
        match registry.get(name).unwrap().icon.as_ref().unwrap() {
            FnIcon::Image { media_type, .. } => assert_eq!(media_type, media),
            other => panic!("{name}: {other:?}"),
        }
    }
    assert_eq!(
        registry.get("x.text").unwrap().icon,
        Some(FnIcon::Text("gear".into()))
    );
    // fn_list reports the summary shape
    let listing = registry.listing();
    let svg_entry = listing.iter().find(|e| e["name"] == "x.svg").unwrap();
    assert_eq!(
        svg_entry["icon"],
        json!({"kind": "image", "type": "image/svg+xml"})
    );
    let text_entry = listing.iter().find(|e| e["name"] == "x.text").unwrap();
    assert_eq!(text_entry["icon"], json!({"kind": "text", "text": "gear"}));
}

#[test]
fn icon_problems_are_entry_errors() {
    let home = scratch();
    let fns = fns_dir(&home);
    let two = good_fn(&fns, "x.two", &manifest("x.two"));
    fs::write(two.join("icon.svg"), SVG).unwrap();
    fs::write(two.join("icon.png"), PNG).unwrap();
    let big = good_fn(&fns, "x.big", &manifest("x.big"));
    fs::write(
        big.join("icon.png"),
        [PNG, &vec![0u8; 256 * 1024][..]].concat(),
    )
    .unwrap();
    let wrong = good_fn(&fns, "x.wrong", &manifest("x.wrong"));
    fs::write(wrong.join("icon.svg"), PNG).unwrap();
    let long = good_fn(
        &fns,
        "x.long",
        &json!({"name": "x.long", "icon": "0123456789abcdefg", "inputs": {}, "outputs": {}}),
    );
    let _ = long;
    let ctrl = good_fn(
        &fns,
        "x.ctrl",
        &json!({"name": "x.ctrl", "icon": "a\tb", "inputs": {}, "outputs": {}}),
    );
    let _ = ctrl;
    let numeric = good_fn(
        &fns,
        "x.num",
        &json!({"name": "x.num", "icon": 7, "inputs": {}, "outputs": {}}),
    );
    let _ = numeric;
    let blank = good_fn(
        &fns,
        "x.blank",
        &json!({"name": "x.blank", "icon": "   ", "inputs": {}, "outputs": {}}),
    );
    let _ = blank;

    let registry = FnRegistry::open(&home, vec![]).registry(None);
    let messages = |name: &str| errors_of(&registry, name, Scope::Global).join("; ");
    assert!(
        messages("x.two").contains("icon.svg and icon.png are both there"),
        "{}",
        messages("x.two")
    );
    assert!(
        messages("x.big").contains("is over 256 KB"),
        "{}",
        messages("x.big")
    );
    assert!(
        messages("x.wrong").contains("is not an SVG image"),
        "{}",
        messages("x.wrong")
    );
    assert!(
        messages("x.long").contains("at most 16 characters"),
        "{}",
        messages("x.long")
    );
    assert!(
        messages("x.ctrl").contains("control characters"),
        "{}",
        messages("x.ctrl")
    );
    assert!(
        messages("x.num").contains("short text"),
        "{}",
        messages("x.num")
    );
    assert!(
        messages("x.blank").contains("short text"),
        "{}",
        messages("x.blank")
    );
    // broken entries do not resolve
    assert!(registry.get("x.two").is_none());
}

// ---- fn_save, generations and the pinned bundle --------------------------------

async fn store(home: &Path) -> Writer {
    Writer::open(home).unwrap()
}
async fn project(writer: &Writer, name: &str) -> ProjectId {
    let name = name.parse().unwrap();
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            Ok(projects::project_create(
                tx,
                CreateProject {
                    name,
                    description: "".into(),
                    icon: None,
                    resources: None,
                    author: "test".into(),
                },
                &EmptyPlanInitializer,
                &NoResourceSettings,
            )?
            .project_id)
        })
        .await
        .unwrap()
}
async fn live_run(writer: &Writer, project: ProjectId) -> RunId {
    let run = RunId::new();
    let attempt = AttemptId::new();
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                &format!(
                    "INSERT INTO attempts(attempt_id,project_id,phase,request,inputs_hash,\
created_at) VALUES ('{attempt}','{project}','executing','{{}}','hash','now')"
                ),
                [],
            )?;
            tx.sql().execute(
                &format!(
                    "INSERT INTO runs(run_id,project_id,attempt_id,prev_run,created_at) \
VALUES ('{run}','{project}','{attempt}',NULL,'now')"
                ),
                [],
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    run
}

#[tokio::test]
async fn save_writes_the_fn_and_publishes_the_scope() {
    let home = scratch();
    let writer = store(&home).await;
    let reg = FnRegistry::open(&home, vec![]);
    let saved = registry::save(&reg, &writer, &manifest("x.saved"), "print('hi')\n", None)
        .await
        .unwrap();
    assert_eq!(saved.scope, Scope::Global);
    assert_eq!(saved.path, home.join("fns/x.saved"));
    assert_eq!(saved.generation, 1);
    assert!(saved.path.join("fn.json").is_file());
    assert!(saved.path.join("main.py").is_file());
    // the registry sees it at once
    let resolved = reg.get("x.saved", None).unwrap();
    assert_eq!(resolved.scope, Scope::Global);
    // the generation froze the scope's files
    let generation_dir = home.join("fns/generations/1");
    assert!(generation_dir.join("x.saved/fn.json").is_file());
    assert!(generation_dir.join("x.saved/main.py").is_file());
    // republishing without a change keeps the generation
    let again = registry::publish(
        &writer,
        &home,
        registry::PublishScope::Home { fn_dirs: &[] },
    )
    .await
    .unwrap();
    assert_eq!(again.generation, 1);
    // a changed file republishes as a new generation
    fs::write(saved.path.join("main.py"), "# v2\n").unwrap();
    let next = registry::publish(
        &writer,
        &home,
        registry::PublishScope::Home { fn_dirs: &[] },
    )
    .await
    .unwrap();
    assert_eq!(next.generation, 2);
    assert_eq!(
        fs::read_to_string(home.join(&next.path).join("x.saved/main.py")).unwrap(),
        "# v2\n"
    );
    // a live run can pin the published generation
    let project_id = project(&writer, "p").await;
    let run = live_run(&writer, project_id).await;
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            artifacts::pin_generation(tx, next.job_id, run)
        })
        .await
        .unwrap();
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn save_rejects_bad_manifests_and_collisions() {
    let home = scratch();
    let writer = store(&home).await;
    let reg = FnRegistry::open(&home, vec![]);
    // a broken manifest is an Invalid with every problem, nothing written
    match registry::save(
        &reg,
        &writer,
        &json!({"name": "Bad.Name", "inputs": {}}),
        "",
        None,
    )
    .await
    {
        Err(PublicError::Invalid { errors, .. }) => {
            assert!(
                errors.iter().any(|e| e.contains("dotted lowercase")),
                "{errors:?}"
            );
            assert!(errors.iter().any(|e| e.contains("outputs is required")));
            assert!(errors.iter().any(|e| e.contains("main_py")));
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    assert!(!home.join("fns/Bad.Name").exists());
    // a builtin's name can never be overwritten
    match registry::save(&reg, &writer, &manifest("jev.ask"), "x\n", None).await {
        Err(PublicError::BadRequest { message }) => {
            assert!(message.contains("collide"), "{message}");
            assert!(message.contains("builtin"), "{message}");
        }
        other => panic!("expected BadRequest, got {other:?}"),
    }
    // a global name taken by any project's fn is refused
    let project_id = project(&writer, "p").await;
    good_fn(
        &project_fns(&home, project_id),
        "x.taken",
        &manifest("x.taken"),
    );
    match registry::save(&reg, &writer, &manifest("x.taken"), "x\n", None).await {
        Err(PublicError::BadRequest { message }) => {
            assert!(message.contains("project"), "{message}")
        }
        other => panic!("expected BadRequest, got {other:?}"),
    }
    // a project save shadowing a global name is refused, a fresh one lands
    good_fn(&fns_dir(&home), "x.global", &manifest("x.global"));
    let selector = Some(ProjectSelector::Id(project_id));
    match registry::save(
        &reg,
        &writer,
        &manifest("x.global"),
        "x\n",
        selector.clone(),
    )
    .await
    {
        Err(PublicError::BadRequest { message }) => {
            assert!(message.contains("collide"), "{message}")
        }
        other => panic!("expected BadRequest, got {other:?}"),
    }
    let saved = registry::save(&reg, &writer, &manifest("x.mine"), "x\n", selector)
        .await
        .unwrap();
    assert_eq!(saved.scope, Scope::Project);
    assert_eq!(saved.path, project_fns(&home, project_id).join("x.mine"));
    // its generation lives under the project, not the home scope
    assert!(
        home.join(format!(
            "projects/{project_id}/generations/1/x.mine/fn.json"
        ))
        .is_file()
    );
    assert_eq!(
        reg.get("x.mine", Some(project_id)).unwrap().scope,
        Scope::Project
    );
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_pinned_bundle_carries_sibling_helpers() {
    let home = scratch();
    let writer = store(&home).await;
    let reg = FnRegistry::open(&home, vec![]);
    // a fn plus a sibling helper dir and loose file it imports
    good_fn(&fns_dir(&home), "x.uses_lib", &manifest("x.uses_lib"));
    let lib = fns_dir(&home).join("_lib");
    fs::create_dir_all(&lib).unwrap();
    fs::write(lib.join("helper.py"), "def helper(): ...\n").unwrap();
    fs::write(lib.join("data.json"), "{}\n").unwrap();

    let resolved = reg.get("x.uses_lib", None).unwrap();
    let job = registry::prepare_run_pin(&writer, &reg, &resolved)
        .await
        .unwrap()
        .unwrap();
    let project_id = project(&writer, "p").await;
    let run = live_run(&writer, project_id).await;
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            artifacts::pin_generation(tx, job.job_id, run)
        })
        .await
        .unwrap();
    match registry::dispatch(&reg, &resolved, Some(job.clone())) {
        FnDispatch::Python { bundle, .. } => {
            let bundle = bundle.unwrap();
            assert_eq!(bundle.generation, job.generation);
            assert!(bundle.fn_dir.join("main.py").is_file());
            assert!(bundle.fn_dir.join("fn.json").is_file());
            assert_eq!(
                fs::read_to_string(bundle.dir.join("_lib/helper.py")).unwrap(),
                "def helper(): ...\n"
            );
            assert!(bundle.dir.join("_lib/data.json").is_file());
        }
        other => panic!("expected Python dispatch, got {other:?}"),
    }
    // a builtin resolves to the descriptor, no pin needed
    let builtin = reg.get("core.echo", None).unwrap();
    assert!(
        registry::prepare_run_pin(&writer, &reg, &builtin)
            .await
            .unwrap()
            .is_none()
    );
    match registry::dispatch(&reg, &builtin, None) {
        FnDispatch::Builtin(d) => assert_eq!(d.name, "core.echo"),
        other => panic!("expected Builtin, got {other:?}"),
    }
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_fn_dir_fn_pins_through_the_home_generation() {
    let home = scratch();
    let extra = scratch();
    let writer = store(&home).await;
    let reg = FnRegistry::open(&home, vec![extra.clone()]);
    good_fn(&extra, "x.external_dir", &manifest("x.external_dir"));
    // a sibling helper dir the fn imports: not a fn itself, but pinned along
    let shared = extra.join("_shared");
    fs::create_dir_all(&shared).unwrap();
    fs::write(shared.join("util.py"), "u = 1\n").unwrap();

    let resolved = reg.get("x.external_dir", None).unwrap();
    let job = registry::prepare_run_pin(&writer, &reg, &resolved)
        .await
        .unwrap()
        .unwrap();
    match registry::dispatch(&reg, &resolved, Some(job.clone())) {
        FnDispatch::Python { bundle, .. } => {
            let bundle = bundle.unwrap();
            // fn_dirs[i] lands under @fn_dirs/<i>/ in the home bundle
            assert!(
                bundle
                    .fn_dir
                    .ends_with(Path::new("@fn_dirs/0/x.external_dir")),
                "{:?}",
                bundle.fn_dir
            );
            assert!(bundle.fn_dir.join("main.py").is_file());
        }
        other => panic!("expected Python, got {other:?}"),
    }
    writer.shutdown().await.unwrap();
}

// ---- invalidation ---------------------------------------------------------------

#[tokio::test]
async fn notify_events_and_fingerprints_invalidate_the_scan() {
    let home = scratch();
    let reg = FnRegistry::open(&home, vec![]);
    let _empty = reg.registry(None); // prime the cache
    let mut watcher = reg.watch().unwrap();
    let before = watcher.version();

    // a write through the filesystem is seen on the next access — the
    // fingerprint reconciles even before (or without) a delivered event
    good_fn(&fns_dir(&home), "x.new", &manifest("x.new"));
    assert!(reg.registry(None).get("x.new").is_some());
    assert!(reg.version() > before);

    // and the watcher delivers the change to subscribers
    fs::write(fns_dir(&home).join("x.new/main.py"), "# edit\n").unwrap();
    tokio::time::timeout(Duration::from_secs(15), watcher.changed())
        .await
        .expect("watcher timed out")
        .unwrap();
    assert!(reg.version() > before);
}
