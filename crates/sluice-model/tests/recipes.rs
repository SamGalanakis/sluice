use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::{
    plan::{FnSignature, Plan},
    recipe::{ExpansionOptions, Recipe, catalog},
    rpc::{JsonMap, decode_json},
    types::Type,
};

fn map(value: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn recipe(steps: Value, params: Value) -> Recipe {
    Recipe::parse("r", &json!({"name":"r","params":params,"steps":steps})).unwrap()
}
fn registry() -> IndexMap<String, FnSignature> {
    IndexMap::from([
        (
            "echo".into(),
            FnSignature {
                inputs: IndexMap::from([("value".into(), Type::Any)]),
                outputs: IndexMap::from([("value".into(), Type::Any)]),
                ..FnSignature::default()
            },
        ),
        (
            "open".into(),
            FnSignature {
                inputs: IndexMap::from([("attempts".into(), "Any[]?".parse().unwrap())]),
                open: true,
                ..FnSignature::default()
            },
        ),
        (
            "core.external".into(),
            FnSignature {
                open: true,
                ..FnSignature::default()
            },
        ),
    ])
}
fn expand(r: &Recipe, params: Value, options: ExpansionOptions, base: Value) -> JsonMap {
    r.expand(&map(params), &options, &map(base), &registry())
        .unwrap()
}
fn value(map: &JsonMap) -> Value {
    serde_json::to_value(map).unwrap()
}

#[test]
fn substitution_in_ids_nested_strings_and_object_keys() {
    let r = recipe(
        json!({"{unit}-work":{"doc":"Work {unit} {n} {flag}", "in":{"{key}":{"default":["see {unit}",{"deep":"{unit}!"}]}}}}),
        json!({"n":"int","flag":"boolean","key":"string"}),
    );
    assert_eq!(
        value(
            &r.substitute(&map(json!({"unit":"u1","n":3,"flag":false,"key":"extra"})))
                .unwrap()
        ),
        json!({"u1-work":{"doc":"Work u1 3 false","in":{"extra":{"default":["see u1",{"deep":"u1!"}]}}}})
    );
}
#[test]
fn exact_placeholders_keep_types_optional_params_become_null() {
    let r = recipe(
        json!({"a":{"in":{"n":{"default":"{n}"},"tags":"{tags}","o":"{opt}","text":"{n}{n}","object":"{obj}"}}}),
        json!({"n":"int","tags":"string[]","opt":"string?","obj":"Any"}),
    );
    assert_eq!(
        value(
            &r.substitute(&map(
                json!({"unit":"u","n":7,"tags":["x","y"],"obj":{"a":true}})
            ))
            .unwrap()
        )["a"]["in"],
        json!({"n":{"default":7},"tags":["x","y"],"o":null,"text":"77","object":{"a":true}})
    );
}
#[test]
fn literal_braces_and_unicode_are_preserved() {
    let r = recipe(
        json!({"a":{"doc":"é{{unit}} is {unit}; {{not a param}} and }}{{"}}),
        json!({}),
    );
    assert_eq!(
        value(&r.substitute(&map(json!({"unit":"u"}))).unwrap())["a"]["doc"],
        "é{unit} is u; {not a param} and }{"
    );
}
#[test]
fn unknown_params_and_lone_braces_name_each_location() {
    let errors = Recipe::parse("r", &json!({"name":"r","steps":{"a-{unit}":{"doc":"{nope}","in":{"{unknown}":"a { b", "x":"oops }"}}}})).unwrap_err();
    let errors: Vec<_> = errors.iter().map(ToString::to_string).collect();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("steps.a-{unit}.doc: unknown param {nope}"))
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("(key): unknown param {unknown}"))
    );
    assert_eq!(errors.iter().filter(|e| e.contains("lone")).count(), 2);
}
#[test]
fn params_collect_unknown_missing_bad_unit_and_type_errors() {
    let r = recipe(
        json!({"a":{}}),
        json!({"n":"int","kind":{"type":"enum","symbols":["a","b"]}}),
    );
    let errors = r
        .substitute(&map(json!({"unit":"U 1","n":"three","kind":"c","extra":1})))
        .unwrap_err();
    assert_eq!(errors.len(), 4);
    assert_eq!(
        errors.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
        ["params.extra", "params.unit", "params.n", "params.kind"]
    );
    assert_eq!(
        r.substitute(&map(json!({"unit":"u","kind":"a"})))
            .unwrap_err()[0]
            .path,
        "params.n"
    );
    assert_eq!(
        r.substitute(&map(json!({"n":1,"kind":"a"}))).unwrap_err()[0].path,
        "params.unit"
    );
}
#[test]
fn parameter_declarations_reuse_model_types_and_docs() {
    let r = recipe(
        json!({"a":{}}),
        json!({"n":{"type":"int","doc":"A count"},"rec":{"type":"record","fields":{"name":"string"}}}),
    );
    assert_eq!(r.params()["n"].doc.as_deref(), Some("A count"));
    assert!(
        r.substitute(&map(json!({"unit":"u","n":2,"rec":{"name":"ok"}})))
            .is_ok()
    );
    for form in [
        json!({"unit":"int"}),
        json!({"Bad":"int"}),
        json!({"n":"strng"}),
        json!({"n":{"doc":"missing type"}}),
    ] {
        assert!(Recipe::parse("r", &json!({"name":"r","params":form,"steps":{"a":{}}})).is_err());
    }
}
#[test]
fn strict_json_and_broken_catalog_entries_do_not_hide_others() {
    let global = br#"{"name":"lane","doc":"global","steps":{"{unit}":{"run":"core.external"}}}"#;
    let project =
        br#"{"name":"lane","doc":"project","steps":{"{unit}-mine":{"run":"core.external"}}}"#;
    let good = br#"{"name":"good","steps":{"{unit}":{"run":"core.external"}}}"#;
    let old = br#"{"name":"old","steps":{"{unit}":{"run":"core.external","when":"yes"}}}"#;
    let entries = catalog([
        ("lane", "global", global.as_slice()),
        ("good", "global", good.as_slice()),
        ("lane", "project", project.as_slice()),
        ("broken", "project", b"{".as_slice()),
        ("old", "project", old.as_slice()),
    ]);
    assert_eq!(entries["lane"].scope, "project");
    assert_eq!(entries["lane"].recipe.as_ref().unwrap().doc(), "project");
    assert!(entries["good"].recipe.is_ok());
    assert!(entries["broken"].recipe.is_err());
    assert!(
        entries["old"].recipe.as_ref().unwrap_err()[0]
            .path
            .ends_with(".when")
    );
    for bytes in [
        br#"{"name":"r","name":"r","steps":{}}"#.as_slice(),
        b"[]",
        br#"{"name":"r","steps":{"a":{"n":9223372036854775808}}}"#,
    ] {
        assert!(Recipe::parse_json("r", bytes).is_err());
    }
    let shadowed = catalog([
        ("lane", "global", global.as_slice()),
        ("lane", "project", b"{".as_slice()),
    ]);
    assert!(shadowed["lane"].recipe.is_err());
}
#[test]
fn substitution_refuses_key_and_step_id_collisions() {
    let r = recipe(json!({"{unit}":{}, "a":{}}), json!({}));
    assert!(
        r.substitute(&map(json!({"unit":"a"})))
            .unwrap_err()
            .iter()
            .any(|e| e.message.contains("duplicate key"))
    );
    let r = recipe(
        json!({"a":{"in":{"{key}":1,"value":2}}}),
        json!({"key":"string"}),
    );
    assert!(
        r.substitute(&map(json!({"unit":"u","key":"value"})))
            .is_err()
    );
}
#[test]
fn expansion_tags_pauses_and_preserves_exit() {
    let r = recipe(
        json!({"{unit}-work":{"run":"core.external","tags":["heavy","exit"]},"{unit}-rm":{"run":"core.external","after":["{unit}-work"],"paused":false}}),
        json!({}),
    );
    let steps = expand(
        &r,
        json!({"unit":"u"}),
        ExpansionOptions {
            start: false,
            tags: vec!["arc:x".into(), "heavy".into()],
            ..ExpansionOptions::default()
        },
        json!({"steps":{}}),
    );
    let steps = value(&steps);
    assert_eq!(
        steps["u-work"]["tags"],
        json!(["unit:u", "heavy", "exit", "arc:x"])
    );
    assert_eq!(steps["u-work"]["paused"], true);
    assert_eq!(steps["u-rm"]["paused"], false);
    let plan = Plan::parse(&map(json!({"steps":steps})), &registry()).unwrap();
    assert_eq!(
        plan.units()[&"u".parse::<sluice_model::ids::UnitName>().unwrap()].exits,
        vec!["u-work".parse().unwrap()]
    );
}
#[test]
fn wildcard_targets_entries_before_external_gates_with_ordered_deduplication() {
    let r = recipe(
        json!({"{unit}-a":{"run":"echo","in":{"value":{"default":1}}},"{unit}-b":{"run":"echo","in":{"value":{"source":"{unit}-a/value"}}},"note-{unit}":{"run":"core.external"},"{unit}-c":{"run":"core.external","after":["{unit}-a?"]}}),
        json!({}),
    );
    let options = ExpansionOptions {
        after: IndexMap::from([
            (
                "*".into(),
                vec!["prior?".into(), "flag".into(), "prior?".into()],
            ),
            ("a".into(), vec!["!flag".into()]),
        ]),
        ..ExpansionOptions::default()
    };
    let steps = value(&expand(
        &r,
        json!({"unit":"u"}),
        options,
        json!({"inputs":{"flag":"boolean"},"steps":{"prior":{"run":"core.external"}}}),
    ));
    assert_eq!(steps["u-a"]["after"], json!(["prior?", "flag", "!flag"]));
    assert_eq!(steps["note-u"]["after"], json!(["prior?", "flag"]));
    assert!(steps["u-b"].get("after").is_none());
    assert_eq!(steps["u-c"]["after"], json!(["u-a?"]));
}
#[test]
fn suffix_input_overrides_accept_declared_and_recipe_bound_inputs() {
    let r = recipe(
        json!({"{unit}-work":{"run":"open","in":{"effort":{"default":"high"}}},"note-{unit}":{"run":"echo","in":{"value":{"default":"{unit}"}}}}),
        json!({}),
    );
    let options = ExpansionOptions {
        inputs: IndexMap::from([
            ("work".into(), map(json!({"effort":"xhigh","attempts":[1]}))),
            ("note-u".into(), map(json!({"value":"hi"}))),
        ]),
        ..ExpansionOptions::default()
    };
    let steps = value(&expand(
        &r,
        json!({"unit":"u"}),
        options,
        json!({"steps":{}}),
    ));
    assert_eq!(
        steps["u-work"]["in"],
        json!({"effort":{"default":"xhigh"},"attempts":{"default":[1]}})
    );
    assert_eq!(steps["note-u"]["in"]["value"], json!({"default":"hi"}));
}
#[test]
fn staging_collects_unknown_suffixes_inputs_and_reserved_tags() {
    let r = recipe(
        json!({"{unit}-work":{"run":"open","in":{"effort":{"default":"high"}}}}),
        json!({}),
    );
    let options = ExpansionOptions {
        tags: vec!["unit:nope".into()],
        after: IndexMap::from([("what".into(), vec!["missing".into()])]),
        inputs: IndexMap::from([
            ("work".into(), map(json!({"new":1,"other":2}))),
            ("where".into(), map(json!({"x":1}))),
        ]),
        ..ExpansionOptions::default()
    };
    let errors = r
        .expand(
            &map(json!({"unit":"u"})),
            &options,
            &map(json!({"steps":{}})),
            &registry(),
        )
        .unwrap_err();
    assert_eq!(
        errors.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
        [
            "tags",
            "after.what",
            "inputs.work.new",
            "inputs.work.other",
            "inputs.where"
        ]
    );
}
#[test]
fn expanded_ids_duplicates_gates_and_input_values_validate_atomically() {
    for (steps, base, options) in [
        (
            json!({"{unit}/bad":{"run":"core.external"}}),
            json!({"steps":{}}),
            ExpansionOptions::default(),
        ),
        (
            json!({"{unit}":{"run":"core.external"}}),
            json!({"steps":{"u":{"run":"core.external"}}}),
            ExpansionOptions::default(),
        ),
        (
            json!({"{unit}":{"run":"core.external"}}),
            json!({"steps":{}}),
            ExpansionOptions {
                after: IndexMap::from([("*".into(), vec!["missing".into()])]),
                ..ExpansionOptions::default()
            },
        ),
    ] {
        let r = recipe(steps, json!({}));
        assert!(
            r.expand(&map(json!({"unit":"u"})), &options, &map(base), &registry())
                .is_err()
        );
    }
}

// Copied from the six live recipe files into a temporary directory on 2026-10-03.
// These embedded fixtures were re-authored for semantics-v2: delivery exits,
// success gates for lane, accept-skip cleanup for branch/study/pr, audit unchanged.
const LASH_LANE: &str = r###"{"name":"lane","doc":"One lash lane: a kiln fork, a lash.worker (engine opus, codex with model sol, or devin with no model = SWE-2 High) on a spec file that commits a ready change, the landing queue (lash.land: one lander at a time, rebase, build only on overlap, push), proof the landed sha is on origin/main, the ticket closed, the fork removed. The land step fails (never skips) when the work submits ready=false, so a dependent ordered after <unit>-landed keeps waiting.","params":{"ticket":"string","spec":"string","model":["null",{"type":"enum","symbols":["sol","astra"]}],"engine":{"type":"enum","symbols":["devin","codex","opus"]}},"steps":{"{unit}-fork":{"run":"lash.fork","tags":["lane"],"in":{"name":{"default":"{unit}"}}},"{unit}-work":{"run":"lash.worker","tags":["lane"],"in":{"engine":{"default":"{engine}"},"model":{"default":"{model}"},"cwd":{"source":"{unit}-fork/path"},"ticket":{"default":"{ticket}"},"spec":{"file":"{spec}"},"queued":{"default":true}},"outputs":{"ready":{"type":"boolean","doc":"true once the commit is final and its proof passed"},"evidence":{"type":"string","doc":"Root cause, red side, counts"},"unresolved":{"type":"string","doc":"Open items; empty if none"}},"needs":{"lane":1}},"{unit}-land":{"run":"lash.land","tags":["lane"],"in":{"fork":{"source":"{unit}-fork/path"},"ready":{"source":"{unit}-work/ready"},"unresolved":{"source":"{unit}-work/unresolved"}}},"{unit}-landed":{"run":"lash.on_main","tags":["lane","exit"],"in":{"repo":{"default":"/workspace/code/lash"},"sha":{"source":"{unit}-land/landed_sha"},"interval":{"default":30},"timeout":{"default":900}},"after":["{unit}-land/landed"]},"{unit}-close":{"run":"linear.close","tags":["lane"],"in":{"issue":{"default":"{ticket}"},"evidence":{"source":"{unit}-work/evidence"},"message":{"source":"{unit}-land/message"}},"after":["{unit}-land/landed","{unit}-landed"]},"{unit}-rm":{"run":"lash.fork_rm","tags":["lane"],"after":["{unit}-close"],"in":{"name":{"default":"{unit}"},"path":{"source":"{unit}-fork/path"}}}}}"###;
const LASH_BRANCH: &str = r###"{"name":"branch","doc":"One lash branch lane: a kiln fork, a lash.worker that commits and pushes to lanes/{unit} (never main) for an integrating lane to merge, the fork removed once its work step is done","params":{"ticket":"string","spec":"string","model":["null",{"type":"enum","symbols":["sol","astra"]}],"engine":{"type":"enum","symbols":["devin","codex","opus"]}},"steps":{"{unit}-fork":{"run":"lash.fork","tags":["branch"],"in":{"name":{"default":"{unit}"}}},"{unit}-work":{"run":"lash.worker","tags":["branch","exit"],"in":{"engine":{"default":"{engine}"},"model":{"default":"{model}"},"cwd":{"source":"{unit}-fork/path"},"lands":{"default":false},"ticket":{"default":"{ticket}"},"spec":{"file":"{spec}"},"push_branch":{"default":"lanes/{unit}"}},"outputs":{"head_sha":{"type":"string","doc":"Branch head pushed to lanes/{unit}"},"evidence":{"type":"string","doc":"Gates with counts, red side"},"unresolved":{"type":"string","doc":"Open items; empty if none"}},"needs":{"lane":1}},"{unit}-rm":{"run":"lash.fork_rm","tags":["branch"],"after":["{unit}-work?"],"in":{"name":{"default":"{unit}"},"path":{"source":"{unit}-fork/path"}}}}}"###;
const LASH_STUDY: &str = r###"{"name":"study","doc":"One read-only lash lane: a kiln fork, a lash.worker (engine opus, codex with model sol/astra, or devin with no model = SWE-2 High) on a spec file that writes a report and changes no code, the fork removed","params":{"ticket":"string","spec":"string","model":["null",{"type":"enum","symbols":["sol","astra"]}],"engine":{"type":"enum","symbols":["devin","codex","opus"]}},"steps":{"{unit}-fork":{"run":"lash.fork","tags":["study"],"in":{"name":{"default":"{unit}"},"review":{"default":true}}},"{unit}-work":{"run":"lash.worker","tags":["study","exit"],"in":{"engine":{"default":"{engine}"},"model":{"default":"{model}"},"cwd":{"source":"{unit}-fork/path"},"lands":{"default":false},"ticket":{"default":"{ticket}"},"spec":{"file":"{spec}"},"read_only":{"default":true}},"outputs":{"report":{"type":"string","doc":"Path of the report file written"},"unresolved":{"type":"string","doc":"Open items; empty if none"}}},"{unit}-rm":{"run":"lash.fork_rm","tags":["study"],"after":["{unit}-work?"],"in":{"name":{"default":"{unit}"},"path":{"source":"{unit}-fork/path"}}}}}"###;
const LASH_TEST_AUDIT: &str = r###"{"name":"test-audit","doc":"One test-audit inspection lane: a kiln fork at the audit base, a codex sol worker that deletes tests in its scope per specs/rubric.md and pushes lanes/ta-{unit}, the fork removed after integration","params":{"scope":"string"},"steps":{"{unit}-fork":{"run":"lash.fork","in":{"name":{"default":"{unit}"},"base":{"default":"145d77f4c7bd363dc1ad0a78bd8527f6d530d32b"}}},"{unit}-work":{"run":"lash.worker","needs":{"lane":1},"after":["ta-history-work"],"in":{"engine":{"default":"codex"},"model":{"default":"sol"},"effort":{"default":"high"},"push_branch":{"default":"lanes/{unit}"},"cwd":{"source":"{unit}-fork/path"},"spec":{"default":"Spec: /workspace/notes/lash/test-audit/specs/rubric.md (read it first, follow it exactly). Lane `{unit}`, branch `lanes/{unit}`. Scope: {scope}"}},"tags":["exit"]},"{unit}-rm":{"run":"lash.fork_rm","after":["ta-integrate-work"],"in":{"name":{"default":"{unit}"},"path":{"source":"{unit}-fork/path"},"force":{"default":true}}}}}"###;
const FIGMENTS_STUDY: &str = r###"{"name":"study","doc":"One read-only figments lane: a review kiln fork at origin/main (or a given base such as a PR branch), a figments.worker (engine opus, codex with model sol or astra, or devin with no model = SWE-2 High) on a spec file that writes a report and changes no code, then the fork removed.","params":{"spec":"string","base":"string","engine":{"type":"enum","symbols":["devin","codex","opus"]},"model":["null",{"type":"enum","symbols":["sol","astra"]}],"effort":["null",{"type":"enum","symbols":["minimal","low","medium","high","xhigh","max"]}]},"steps":{"{unit}-fork":{"run":"figments.fork","tags":["study"],"in":{"name":{"default":"{unit}"},"base":{"default":"{base}"}}},"{unit}-work":{"run":"figments.worker","tags":["study","exit"],"in":{"engine":{"default":"{engine}"},"model":{"default":"{model}"},"effort":{"default":"{effort}"},"cwd":{"source":"{unit}-fork/path"},"read_only":{"default":true},"spec":{"file":"{spec}"}},"outputs":{"report":{"type":"string","doc":"Path of the report file written"},"unresolved":{"type":"string","doc":"Open items; empty if none"}}},"{unit}-rm":{"run":"figments.fork_rm","tags":["study"],"after":["{unit}-work?"],"in":{"name":{"default":"{unit}"},"path":{"source":"{unit}-fork/path"}}}}}"###;
const FIGMENTS_PR: &str = r###"{"name":"pr","doc":"One figments PR lane: a kiln fork on branch samuel-<unit> from origin/main (or an existing fork of that name, resumed as it is), a figments.worker (engine opus, codex with model sol, or devin with no model = SWE-2 High) on a spec file that commits, pushes and opens a PR, then the PR's checks watched until green or red. Sam merges. The fork (and its Bazel server) is removed once the checks settle; the work is safe on the pushed branch.","params":{"spec":"string","engine":{"type":"enum","symbols":["devin","codex","opus"]},"model":["null",{"type":"enum","symbols":["sol","astra"]}],"effort":["null",{"type":"enum","symbols":["minimal","low","medium","high","xhigh","max"]}]},"steps":{"{unit}-fork":{"run":"figments.fork","tags":["pr"],"in":{"name":{"default":"{unit}"},"branch":{"default":"samuel-{unit}"}}},"{unit}-work":{"run":"figments.worker","tags":["pr","exit"],"in":{"engine":{"default":"{engine}"},"model":{"default":"{model}"},"effort":{"default":"{effort}"},"cwd":{"source":"{unit}-fork/path"},"pr":{"default":true},"spec":{"file":"{spec}"}},"outputs":{"pr_url":{"type":"string","doc":"URL of the PR opened or updated"},"head_sha":{"type":"string","doc":"The fork's HEAD after the last push"},"unresolved":{"type":"string","doc":"Open items; empty if none"}}},"{unit}-checks":{"run":"gh.pr_wait","tags":["pr"],"in":{"path":{"source":"{unit}-fork/path"},"pr":{"source":"{unit}-work/pr_url"},"until":{"default":"checks"},"timeout":{"default":7200}}},"{unit}-rm":{"run":"figments.fork_rm","tags":["pr"],"after":["{unit}-checks?"],"in":{"name":{"default":"{unit}"},"path":{"source":"{unit}-fork/path"}}}}}"###;

// Typed model fixtures for the signatures used by the copied recipes. They do
// not load or execute the live fns. Worker submissions remain step declarations.
fn real_registry() -> IndexMap<String, FnSignature> {
    let signature = |inputs: Value, outputs: Value, open| FnSignature {
        inputs: inputs
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), Type::parse(v).unwrap()))
            .collect(),
        outputs: outputs
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), Type::parse(v).unwrap()))
            .collect(),
        open,
        ..FnSignature::default()
    };
    let mut r = registry();
    for name in ["lash.fork", "figments.fork"] {
        r.insert(
            name.into(),
            signature(
                json!({"name":"string","base":"string?","branch":"string?","review":"boolean?"}),
                json!({"path":"string"}),
                false,
            ),
        );
    }
    for name in ["lash.fork_rm", "figments.fork_rm"] {
        r.insert(
            name.into(),
            signature(
                json!({"name":"string","path":"string","force":"boolean?"}),
                json!({}),
                false,
            ),
        );
    }
    for name in ["lash.worker", "figments.worker"] {
        r.insert(name.into(),signature(json!({"engine":{"type":"enum","symbols":["devin","codex","opus"]},"cwd":"string","spec":"string","model":"string?","effort":"string?","lands":"boolean?","ticket":"string?","push_branch":"string?","queued":"boolean?","read_only":"boolean?","pr":"boolean?"}),json!({}),true));
    }
    r.insert(
        "lash.land".into(),
        signature(
            json!({"fork":"string","ready":"boolean","unresolved":"string"}),
            json!({"landed":"boolean","landed_sha":"string","message":"string"}),
            false,
        ),
    );
    r.insert(
        "lash.on_main".into(),
        signature(
            json!({"repo":"string","sha":"string","interval":"int?","timeout":"int?"}),
            json!({}),
            false,
        ),
    );
    r.insert(
        "linear.close".into(),
        signature(
            json!({"issue":"string","evidence":"string","message":"string"}),
            json!({}),
            false,
        ),
    );
    r.insert("gh.pr_wait".into(),signature(json!({"path":"string","pr":"string","until":{"type":"enum","symbols":["checks","merged"]},"interval":"int?","timeout":"int?"}),json!({"state":{"type":"enum","symbols":["green","red","conflicting","merged","closed","timeout"]},"sha":"string","url":"string","failed":"string[]"}),false));
    r
}
fn check_real(bytes: &str, expected_steps: usize, exit: &str, cleanup: &str) {
    let raw: Value = serde_json::from_str(bytes).unwrap();
    let name = raw["name"].as_str().unwrap();
    let recipe = Recipe::parse_json(name, bytes.as_bytes()).unwrap();
    let params = map(
        json!({"unit":"u","ticket":"FIG-1","spec":"/tmp/spec.md","engine":"codex","model":"sol","effort":"high","base":"origin/main","scope":"crates/*"}),
    );
    let params = JsonMap(
        params
            .0
            .into_iter()
            .filter(|(k, _)| recipe.params().contains_key(k))
            .collect(),
    );
    let base = map(
        json!({"steps":{"ta-history-work":{"run":"core.external"},"ta-integrate-work":{"run":"core.external"}}}),
    );
    let steps = recipe
        .expand(
            &params,
            &ExpansionOptions::default(),
            &base,
            &real_registry(),
        )
        .unwrap();
    assert_eq!(steps.0.len(), expected_steps);
    let mut document = value(&base);
    document["steps"]
        .as_object_mut()
        .unwrap()
        .extend(value(&steps).as_object().unwrap().clone());
    let plan = Plan::parse(&map(document), &real_registry()).unwrap();
    let unit = &plan.units()[&"u".parse::<sluice_model::ids::UnitName>().unwrap()];
    assert_eq!(unit.exits, vec![exit.parse().unwrap()]);
    assert_eq!(
        plan.steps()[&"u-rm".parse::<sluice_model::ids::StepId>().unwrap()]
            .after
            .iter()
            .map(|g| g.entry())
            .collect::<Vec<_>>(),
        vec![cleanup]
    );
    assert!(
        steps
            .0
            .values()
            .all(|s| s.as_value()["tags"][0] == "unit:u")
    );
}
#[test]
fn real_lash_lane_validates() {
    check_real(LASH_LANE, 6, "u-landed", "u-close");
}
#[test]
fn real_lash_branch_validates() {
    check_real(LASH_BRANCH, 3, "u-work", "u-work?");
}
#[test]
fn real_lash_study_validates() {
    check_real(LASH_STUDY, 3, "u-work", "u-work?");
}
#[test]
fn real_lash_test_audit_validates() {
    check_real(LASH_TEST_AUDIT, 3, "u-work", "ta-integrate-work");
}
#[test]
fn real_figments_study_validates() {
    check_real(FIGMENTS_STUDY, 3, "u-work", "u-work?");
}
#[test]
fn real_figments_pr_validates() {
    check_real(FIGMENTS_PR, 4, "u-work", "u-checks?");
}

#[test]
fn escaped_keys_are_checked_for_collisions_only_after_real_substitution() {
    let r = recipe(
        json!({"a":{"in":{"{key}":1,"{{key}}":2}}}),
        json!({"key":"string"}),
    );
    assert_eq!(
        value(&r.substitute(&map(json!({"unit":"u","key":"x"}))).unwrap())["a"]["in"],
        json!({"x":1,"{key}":2})
    );
    assert!(
        r.substitute(&map(json!({"unit":"u","key":"{key}"})))
            .is_err()
    );
}

#[test]
fn value_parser_refuses_overflow_and_bounds_placeholder_recursion() {
    assert!(Recipe::parse("r", &json!({"name":"r","steps":{"a":{"n":u64::MAX}}})).is_err());
    let mut nested = json!("{unit}");
    for _ in 0..130 {
        nested = json!([nested]);
    }
    let errors = Recipe::parse("r", &json!({"name":"r","steps":{"a":{"doc":nested}}})).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("nesting exceeds 128"))
    );
}
