use super::files;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sluice_model::{FnSignature, Plan, SignatureProvider, rpc::JsonMap};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Clone, Default)]
pub struct Signatures(pub BTreeMap<String, FnSignature>);
impl SignatureProvider for Signatures {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        self.0.get(name).cloned()
    }
}
impl Signatures {
    pub fn add(&mut self, doc: Value) -> Result<()> {
        let name = doc["name"]
            .as_str()
            .context("fn descriptor has no name")?
            .to_owned();
        let mut sig = FnSignature {
            open: doc["open"].as_bool().unwrap_or(false),
            ..Default::default()
        };
        for (field, target) in [("inputs", &mut sig.inputs), ("outputs", &mut sig.outputs)] {
            if let Some(map) = doc[field].as_object() {
                for (name, value) in map {
                    target.insert(name.clone(), serde_json::from_value(value.clone())?);
                }
            }
        }
        ensure!(
            self.0.insert(name.clone(), sig).is_none(),
            "duplicate converted fn {name}"
        );
        Ok(())
    }
    pub fn load(&mut self, dir: &Path) -> Result<()> {
        if !dir.exists() {
            return Ok(());
        }
        for path in files::entries(dir)? {
            if path.join("fn.json").is_file() {
                self.add(files::json(&path.join("fn.json"))?)?;
            }
        }
        Ok(())
    }
}

fn convert_document(
    old: &Value,
    units: Option<&Value>,
    rewrites: &[(String, String)],
) -> Result<(Value, Vec<String>)> {
    let mut doc = old.clone();
    let steps = doc["steps"]
        .as_object_mut()
        .context("plan steps must be an object")?;
    let mut notes = vec![];
    for (id, step) in steps.iter_mut() {
        let step = step.as_object_mut().context("step must be an object")?;
        if let Some(when) = step.remove("when") {
            let gate = when.as_str().context("when must be a ref")?;
            let after = step
                .entry("after")
                .or_insert(json!([]))
                .as_array_mut()
                .context("after must be an array")?;
            if !after.iter().any(|v| v.as_str() == Some(gate)) {
                after.push(json!(gate));
            }
            notes.push(format!("{id}: when {gate} becomes an after entry"));
        }
        if let Some(entries) = step.get_mut("after").and_then(Value::as_array_mut) {
            let mut dedup = Vec::new();
            for entry in entries.iter() {
                if !dedup.contains(entry) {
                    dedup.push(entry.clone());
                }
            }
            *entries = dedup;
        }
        if let Some(name) = step.get("run").and_then(Value::as_str)
            && ["thread.post", "thread.wait", "inbox.ask"].contains(&name)
        {
            anyhow::bail!(
                "{id}: legacy message fn {name} requires explicit release-signature and binding conversion"
            );
        }
    }
    // Preserve each old untagged connected component, without letting a
    // cross-unit handoff absorb a tagged unit. Explicit manifest groups win.
    let untagged = steps
        .iter()
        .filter(|(_, s)| {
            !s["tags"].as_array().is_some_and(|a| {
                a.iter()
                    .any(|t| t.as_str().is_some_and(|s| s.starts_with("unit:")))
            })
        })
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    let mut edges: BTreeMap<String, BTreeSet<String>> = untagged
        .iter()
        .map(|id| (id.clone(), BTreeSet::new()))
        .collect();
    for id in &untagged {
        let step = &steps[id];
        let mut refs: Vec<&str> = step["after"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        for binding in step["in"].as_object().into_iter().flat_map(|m| m.values()) {
            if let Some(s) = binding["source"].as_str() {
                refs.push(s);
            }
            refs.extend(
                binding["source"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            );
        }
        for text in refs {
            let other = text
                .trim_start_matches('!')
                .trim_end_matches('?')
                .split('/')
                .next()
                .unwrap_or(text);
            if untagged.contains(other) {
                edges.get_mut(id).expect("known id").insert(other.into());
                edges.get_mut(other).expect("known id").insert(id.clone());
            }
        }
    }
    let mut seen = BTreeSet::new();
    let order: Vec<_> = steps.keys().cloned().collect();
    for first in &order {
        if !untagged.contains(first) || !seen.insert(first.clone()) {
            continue;
        }
        let mut group = vec![first.clone()];
        let mut n = 0;
        while n < group.len() {
            for next in &edges[&group[n]] {
                if seen.insert(next.clone()) {
                    group.push(next.clone());
                }
            }
            n += 1;
        }
        if group.len() > 1 {
            for id in &group {
                add_tag(&mut steps[id], first)?;
            }
            notes.push(format!(
                "{}: tag untagged group unit:{first}",
                group.join(", ")
            ));
        }
    }
    if let Some(groups) = units.and_then(Value::as_object) {
        for (unit, members) in groups {
            for id in members
                .as_array()
                .context("manifest unit members must be an array")?
            {
                let id = id.as_str().context("manifest step must be a string")?;
                let step = steps.get_mut(id).context("manifest names a missing step")?;
                if let Some(tags) = step["tags"].as_array_mut() {
                    tags.retain(|v| !v.as_str().is_some_and(|s| s.starts_with("unit:")));
                }
                add_tag(step, unit)?;
            }
        }
    }
    rewrite_paths(&mut doc, rewrites);
    Ok((doc, notes))
}

pub fn document(
    old: &Value,
    units: Option<&Value>,
    rewrites: &[(String, String)],
    sig: &Signatures,
) -> Result<(Plan, Vec<String>)> {
    // Use the conversion above with the caller's exact signatures, including
    // reviewed native builtins. No execution or inferred Any signatures.
    let (raw, notes) = convert_document(old, units, rewrites)?;
    Ok((
        Plan::parse(&serde_json::from_value::<JsonMap>(raw)?, sig).map_err(|e| {
            anyhow::anyhow!(
                "{}",
                e.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })?,
        notes,
    ))
}

fn add_tag(step: &mut Value, unit: &str) -> Result<()> {
    let tags = step
        .as_object_mut()
        .context("step must be an object")?
        .entry("tags")
        .or_insert(json!([]))
        .as_array_mut()
        .context("tags must be an array")?;
    let tag = json!(format!("unit:{unit}"));
    if !tags.contains(&tag) {
        tags.push(tag);
    }
    Ok(())
}

pub fn rewrite_paths(value: &mut Value, rewrites: &[(String, String)]) {
    match value {
        Value::String(s) => {
            for (old, new) in rewrites {
                if s == old
                    || s.strip_prefix(old)
                        .is_some_and(|tail| tail.starts_with('/'))
                {
                    *s = format!("{new}{}", &s[old.len()..]);
                    break;
                }
            }
        }
        Value::Array(a) => {
            for v in a {
                rewrite_paths(v, rewrites);
            }
        }
        Value::Object(o) => {
            for v in o.values_mut() {
                rewrite_paths(v, rewrites);
            }
        }
        _ => (),
    }
}
