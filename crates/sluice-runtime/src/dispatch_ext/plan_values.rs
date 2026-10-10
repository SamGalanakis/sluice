use sluice_model::{
    commands::PlanViewFormat,
    error::PublicError,
    ids::{ProjectId, Revision, UnitName},
    plan_rows::*,
};
use std::collections::BTreeMap;

pub fn row_selection(filter: &PlanReadFilter, recipes: &BTreeMap<String, String>) -> RowSelection {
    let mut units = filter.units.clone();
    if let Some(recipe) = &filter.recipe {
        let matching: Vec<_> = recipes
            .iter()
            .filter(|(_, r)| *r == recipe)
            .filter_map(|(id, _)| UnitName::new(id).ok())
            .collect();
        units = Some(match units {
            None => matching,
            Some(units) => units.into_iter().filter(|u| matching.contains(u)).collect(),
        });
    }
    RowSelection {
        units,
        steps: filter.steps.clone(),
        status: filter.status.clone(),
        ..Default::default()
    }
}

pub fn projection(compact: bool) -> StepProjection {
    if compact {
        StepProjection::Compact
    } else {
        StepProjection::Full
    }
}

/// A full projection uses only references of returned steps, in slot and ordinal order.
pub fn step_views(
    rows: Vec<StepRowView>,
    references: ReferenceRows,
    recipes: &BTreeMap<String, String>,
) -> Result<Vec<StepView>, PublicError> {
    rows.into_iter()
        .map(|row| {
            let recipe = recipes.get(row.unit.as_str()).cloned();
            Ok(match row.declaration {
                None => StepView::Compact(CompactStep {
                    id: row.step,
                    unit: row.unit,
                    recipe,
                    position: row.position,
                    run: row.run,
                    status: row.status,
                    paused: row.paused,
                    priority: row.priority,
                }),
                Some(spec) => {
                    let mut refs: Vec<_> = references
                        .0
                        .iter()
                        .filter(|r| {
                            r.consumer_kind == ConsumerKind::Step
                                && r.consumer_id == row.step.as_str()
                        })
                        .map(|r| StepReference {
                            kind: r.kind,
                            slot: r.slot.clone(),
                            ordinal: r.ordinal,
                            source_kind: r.source_kind,
                            source_id: r.source_id.clone(),
                            source_port: r.source_port.clone(),
                            source_path: r.source_path.clone(),
                        })
                        .collect();
                    refs.sort_by(|a, b| (&a.slot, a.ordinal).cmp(&(&b.slot, b.ordinal)));
                    StepView::Full(FullStep {
                        id: row.step,
                        unit: row.unit,
                        recipe,
                        position: row.position,
                        run: row.run,
                        status: row.status,
                        paused: row.paused,
                        priority: row.priority,
                        spec,
                        references: refs,
                    })
                }
            })
        })
        .collect()
}

pub fn cursor_after(
    request: &PlanRead,
    project: ProjectId,
    rev: Revision,
    state: StateEpoch,
    recipes: &RecipeGeneration,
) -> Result<Option<(u64, sluice_model::ids::StepId)>, PublicError> {
    let Some(text) = &request.cursor else {
        return Ok(None);
    };
    let cursor = PlanCursor::parse(text).map_err(PublicError::from)?;
    let filter = request.filter();
    if cursor.filter != filter.digest(project)
        || cursor.state_epoch.is_some() != filter.binds_state()
        || cursor.recipe_generation.is_some() != filter.binds_recipes()
    {
        return Err(PlanRowsError::CursorMismatch.into());
    }
    if cursor.rev != rev
        || cursor.state_epoch.is_some_and(|e| e != state)
        || cursor
            .recipe_generation
            .as_ref()
            .is_some_and(|g| g != recipes)
    {
        return Err(PlanRowsError::CursorExpired.into());
    }
    Ok(Some((cursor.position, cursor.step)))
}

pub fn next_cursor(
    request: &PlanRead,
    project: ProjectId,
    rows: &StepRows,
    recipes: &RecipeGeneration,
) -> Option<String> {
    if !rows.more {
        return None;
    }
    let last = rows.steps.last()?;
    let filter = request.filter();
    Some(
        PlanCursor {
            filter: filter.digest(project),
            rev: rows.rev,
            state_epoch: filter.binds_state().then_some(rows.state_epoch),
            recipe_generation: filter.binds_recipes().then(|| recipes.clone()),
            position: last.position,
            step: last.step.clone(),
        }
        .encode(),
    )
}

fn html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn label(text: &str) -> String {
    html(text)
        .replace('\n', " ")
        .replace('[', "&#91;")
        .replace(']', "&#93;")
}

/// Every edge with a selected endpoint survives, including those crossing the selection.
pub fn render_graph(
    name: &str,
    graph: &GraphRows,
    format: PlanViewFormat,
    omitted: &str,
) -> String {
    let mut diagram = String::from("flowchart TD\n");
    let mut boxes = vec![];
    for step in &graph.steps {
        let node = format!("s_{}", step.step);
        let text = format!("{} / {} / {}", step.step, step.run, step.status);
        diagram.push_str(&format!(
            "  {node}[\"{}\"]:::{}\n",
            label(&text),
            step.status
        ));
        boxes.push((node, text, step.status.as_str().to_owned()));
    }
    for step in &graph.boundary {
        let node = format!("ext_{}", step.step);
        let text = format!("{} · outside", step.step);
        diagram.push_str(&format!("  {node}[\"{}\"]:::outside\n", label(&text)));
        boxes.push((node, text, "outside".into()));
    }
    let node = |id: &sluice_model::ids::StepId| {
        let prefix = if graph.steps.iter().any(|s| &s.step == id) {
            "s"
        } else {
            "ext"
        };
        format!("{prefix}_{id}")
    };
    let mut edges = vec![];
    for edge in &graph.edges {
        let from = node(&edge.source);
        let to = node(&edge.target);
        let text = edge
            .via_unit
            .as_ref()
            .map(|u| format!("unit:{u}"))
            .unwrap_or_else(|| match edge.kind {
                EdgeKind::Data => "data".into(),
                EdgeKind::Gate => "after".into(),
            });
        diagram.push_str(&format!("  {from} -->|\"{}\"| {to}\n", label(&text)));
        edges.push((from, to, text));
    }
    diagram.push_str("  classDef pending fill:#e5e7eb,color:#111827\n  classDef running fill:#bfdbfe,color:#111827\n  classDef succeeded fill:#bbf7d0,color:#111827\n  classDef failed fill:#172554,color:#fff,stroke-width:3px\n  classDef stale fill:#fde68a,color:#111827\n  classDef skipped fill:#f3f4f6,color:#6b7280\n  classDef outside fill:#f8fafc,color:#64748b,stroke-dasharray:4 4\n");
    if !omitted.is_empty() {
        diagram.push_str(&format!("  %% {omitted}\n"));
    }
    if !graph.boundary.is_empty() {
        diagram.push_str(&format!(
            "  %% {} steps outside the selection drawn as boundary nodes\n",
            graph.boundary.len()
        ));
    }
    if matches!(format, PlanViewFormat::Mermaid) {
        return diagram;
    }
    let positions: BTreeMap<_, _> = boxes
        .iter()
        .enumerate()
        .map(|(i, (id, _, _))| (id.clone(), i))
        .collect();
    let mut svg = format!(
        "<svg role=\"img\" aria-label=\"Plan graph\" viewBox=\"0 0 1000 {}\" xmlns=\"http://www.w3.org/2000/svg\"><defs><marker id=\"arrow\" viewBox=\"0 0 10 10\" refX=\"10\" refY=\"5\" markerWidth=\"6\" markerHeight=\"6\" orient=\"auto-start-reverse\"><path d=\"M 0 0 L 10 5 L 0 10 z\" fill=\"#64748b\"/></marker></defs>",
        boxes.len().max(1) * 100 + 30
    );
    for (from, to, text) in edges {
        let a = positions[&from] * 100 + 55;
        let b = positions[&to] * 100 + 55;
        svg.push_str(&format!("<path d=\"M 690 {a} C 800 {a},800 {b},690 {b}\" fill=\"none\" stroke=\"#64748b\" marker-end=\"url(#arrow)\"/><text x=\"805\" y=\"{}\" font-size=\"11\">{}</text>",(a+b)/2,html(&text)));
    }
    for (i, (_, text, status)) in boxes.iter().enumerate() {
        svg.push_str(&format!("<g class=\"{}\"><rect x=\"20\" y=\"{}\" width=\"670\" height=\"70\" rx=\"8\"/><text x=\"35\" y=\"{}\" font-size=\"12\">{}</text></g>",html(status),i*100+20,i*100+58,html(text)));
    }
    svg.push_str("</svg>");
    format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>body{{margin:auto;padding:24px;max-width:1100px;font:16px system-ui;color:#172554;background:#fff}}svg{{width:100%;height:auto}}rect{{fill:#e5e7eb;stroke:#64748b}}.running rect{{fill:#bfdbfe}}.succeeded rect{{fill:#bbf7d0}}.failed rect{{fill:#fecaca;stroke-width:3}}.stale rect{{fill:#fde68a}}.outside rect{{fill:none;stroke-dasharray:4 4}}pre{{white-space:pre-wrap;overflow-wrap:anywhere}}@media(prefers-color-scheme:dark){{body{{background:#0f172a;color:#e2e8f0}}text{{fill:#334155}}}}</style><h1>{}</h1><p>{}</p>{svg}<details><summary>Mermaid source</summary><pre>{}</pre></details></html>",
        html(name),
        html(name),
        html(omitted),
        html(&diagram)
    )
}
