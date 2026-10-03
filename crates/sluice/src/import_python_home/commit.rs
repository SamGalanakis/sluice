use super::{Prepared, state};
use rusqlite::params;
use serde_json::{Value, json};
use sluice_model::{
    commands::MessagePost,
    error::PublicError,
    ids::{ProjectId, ProjectSelector},
    plan::inputs_hash,
    rpc::JsonMap,
};
use sluice_store::{
    Result, StoreError, WriteTransaction,
    artifacts::{self, Bundle, BundleScope},
    messages, plans, projects, resources,
};
use std::collections::BTreeMap;

pub fn commit(
    tx: &mut WriteTransaction<'_>,
    prepared: Vec<Prepared>,
    mut ledger: Value,
    global: Option<Bundle>,
    fail: bool,
) -> Result<Value> {
    if let Some(bundle) = global {
        artifacts::stage_generation(tx, BundleScope::Home, bundle)?;
    }
    for (n, p) in prepared.into_iter().enumerate() {
        let project: ProjectId =
            serde_json::from_value(ledger["projects"][&p.old.name]["id"].clone())?;
        let id = project.to_string();
        let at = p.old.settings["created"]
            .as_str()
            .unwrap_or("1970-01-01T00:00:00Z");
        tx.sql().execute("INSERT INTO projects(project_id,name,description,paused,archived,created_at) VALUES (?1,?2,?3,1,?4,?5)",params![id,p.old.name,p.old.settings["description"].as_str().unwrap_or(""),p.old.settings["archived"].as_bool().unwrap_or(false),at])?;
        plans::initialize_plan(tx, project, &p.plan)?;
        resources::patch_resources(tx, project, &p.old.settings["resources"], &p.signatures)?;
        if p.icon.is_some() {
            projects::project_update(
                tx,
                &ProjectSelector::Id(project),
                projects::UpdateProject {
                    icon: p.icon,
                    ..Default::default()
                },
                &projects::NoResourceSettings,
            )?;
        }
        if let Some(bundle) = p.bundle {
            artifacts::stage_generation(tx, BundleScope::Project(project), bundle)?;
        }
        for (name, value) in &p.state.inputs.0 {
            tx.sql().execute(
                "UPDATE inputs SET value=?3 WHERE project_id=?1 AND name=?2",
                params![id, name, serde_json::to_string(value)?],
            )?;
        }
        for (step_id, step) in p.plan.steps() {
            let entry = &p.old.state["steps"][step_id.as_str()];
            let current = &p.state.steps[step_id];
            let status = serde_json::to_value(&current.status)?
                .as_str()
                .ok_or_else(|| StoreError::InvalidDatabase("status missing".into()))?
                .to_owned();
            let interrupted = entry["status"] == "running";
            let error = if interrupted {
                Some(PublicError::ProcessLost {
                    message: super::INTERRUPTED.into(),
                })
            } else {
                current
                    .error
                    .as_ref()
                    .map(|s| PublicError::FnFailure { message: s.clone() })
            };
            let hash = inputs_hash(&p.plan, &p.state, step).map(|h| h.to_string());
            let effective = state::effective(&p.plan, &p.state, step)
                .map_err(|e| StoreError::InvalidDatabase(e.to_string()))?;
            let mut instances = json!({});
            let mut run_ids = Vec::new();
            let mut total = None;
            let mut done = 0;
            if let Some(scatter) = &step.scatter {
                total = effective
                    .as_ref()
                    .and_then(|e| e[scatter].as_array())
                    .map(|a| a.len() as i64);
                let retained = if entry["kept"].is_object() {
                    &entry["kept"]["results"]
                } else {
                    &entry["results"]
                };
                if let Some(results) = retained.as_array()
                    && total != Some(results.len() as i64)
                {
                    return Err(StoreError::InvalidDatabase(format!(
                        "scatter {step_id}: retained item count differs from effective inputs"
                    )));
                }
                for (index, result) in retained.as_array().into_iter().flatten().enumerate() {
                    if !result.is_object() {
                        continue;
                    }
                    let mut result = result.clone();
                    super::convert::rewrite_paths(&mut result, &p.rewrites);
                    let result: JsonMap = serde_json::from_value(result)?;
                    state::validate_outputs(step, &result, true, false)
                        .map_err(|e| StoreError::InvalidDatabase(e.to_string()))?;
                    instances[index.to_string()] =
                        json!({"status":"succeeded","outputs":result,"inputs":{}});
                    done += 1;
                }
                if status == "succeeded" {
                    let count = total.ok_or_else(|| {
                        StoreError::InvalidDatabase(
                            "successful scatter has unknown item count".into(),
                        )
                    })?;
                    for index in 0..count {
                        let mut outputs = json!({});
                        for (name, value) in &current.outputs.0 {
                            outputs[name] = value.as_value()[index as usize].clone();
                        }
                        instances[index.to_string()] =
                            json!({"status":"succeeded","outputs":outputs,"inputs":{}});
                    }
                    done = count;
                }
            }
            for pred in p
                .predecessors
                .iter()
                .filter(|r| r["step"] == step_id.as_str())
            {
                let run = pred["run"]
                    .as_str()
                    .ok_or_else(|| StoreError::InvalidDatabase("predecessor run missing".into()))?;
                let attempt = pred["attempt"].as_str().ok_or_else(|| {
                    StoreError::InvalidDatabase("predecessor attempt missing".into())
                })?;
                let index = pred["index"].as_i64().unwrap_or(-1);
                let good = pred["good"] == true;
                let provenance = pred["checkpoint"]
                    .as_object()
                    .map(|checkpoint| {
                        checkpoint
                            .iter()
                            .filter(|(key, _)| {
                                ["head_before", "head_after", "git"].contains(&key.as_str())
                            })
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect::<serde_json::Map<_, _>>()
                    })
                    .unwrap_or_default();
                let frozen = json!({"declaration":p.plan.document().0["steps"].as_value()[step_id.as_str()],"inputs":pred["inputs"],"effective_inputs":effective,"returns":step.signature.outputs,"declared":step.declared_outputs.iter().map(|(n,d)|(n.clone(),serde_json::to_value(&d.ty).expect("type serializes"))).collect::<BTreeMap<_,_>>(),"item_count":total,"provenance":provenance});
                tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,item_index,phase,request,inputs_hash,provenance,unit,created_at,finished_at) VALUES (?1,?2,?3,?4,'terminal',?5,?6,?7,?8,?9,?9)",params![attempt,id,step_id.as_str(),index,frozen.to_string(),hash.as_deref().unwrap_or("unknown"),json!(provenance).to_string(),step.unit_name().to_string(),at])?;
                let result = json!({"status":if good {"succeeded"} else {"failed"},"outputs":pred["outputs"],"error":if good {None} else {Some(PublicError::ProcessLost {message:super::INTERRUPTED.into()})}});
                tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,item_index,unit,created_at,started_at,finished_at,completion_id,completion_ack,result) VALUES (?1,?2,?3,?4,?5,?6,?7,?7,?7,?8,1,?9)",params![run,id,attempt,step_id.as_str(),index,step.unit_name().to_string(),at,format!("import:{run}"),result.to_string()])?;
                if let Some(session) = pred.get("session").filter(|s| s.is_object()) {
                    tx.sql().execute("INSERT INTO sessions(run_id,project_id,engine,cwd,session_id,metadata,recorded_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",params![run,id,session["engine"].as_str(),session["cwd"].as_str(),session["session"].as_str(),session["metadata"].to_string(),at])?;
                }
                if pred["outputs"].is_object()
                    && pred["outputs"].as_object().is_some_and(|o| !o.is_empty())
                {
                    tx.sql().execute("INSERT INTO submissions(run_id,project_id,step_id,outputs,at) VALUES (?1,?2,?3,?4,?5)",params![run,id,step_id.as_str(),pred["outputs"].to_string(),at])?;
                }
                run_ids.push(json!(run));
                if index >= 0 {
                    instances[index.to_string()] = json!({"status":if good {"succeeded"} else {"failed"},"outputs":pred["outputs"],"run":run,"inputs":pred["inputs"],"error":result["error"]});
                }
            }
            let outputs = serde_json::to_string(&current.outputs)?;
            let manual = entry["manual"]
                .as_bool()
                .unwrap_or_else(|| entry["manual"] == 1);
            tx.sql().execute("UPDATE steps SET status=?3,outputs=?4,error=?5,manual=?6,inputs_hash=?7,skipped=?8,run_ids=?9,instances=?10,total=?11,done=?12 WHERE project_id=?1 AND step_id=?2",params![id,step_id.as_str(),status,outputs,error.as_ref().map(serde_json::to_string).transpose()?,manual,hash,serde_json::to_string(&current.skipped)?,json!(run_ids).to_string(),instances.to_string(),total,done])?;
            if status != "pending" {
                let result = ledger["projects"][&p.old.name]["steps"][step_id.as_str()]["result"]
                    .as_str()
                    .ok_or_else(|| StoreError::InvalidDatabase("result mapping missing".into()))?;
                tx.sql().execute("INSERT INTO step_results(result_id,project_id,step_id,generation,unit,declaration,inputs,inputs_hash,status,outputs,error,manual,run_ids,recorded_at) SELECT ?3,project_id,step_id,generation,unit,declaration,?4,inputs_hash,status,outputs,error,manual,run_ids,?5 FROM steps WHERE project_id=?1 AND step_id=?2",params![id,step_id.as_str(),result,effective.map(|v|v.to_string()),at])?;
                tx.sql().execute(
                    "UPDATE steps SET result_id=?3 WHERE project_id=?1 AND step_id=?2",
                    params![id, step_id.as_str(), result],
                )?;
            }
        }
        for q in &p.old.questions {
            // Unsupported input/UI questions are reported during preflight.
            if !p.questions.contains(&q["n"].as_i64().unwrap_or(-1)) {
                continue;
            }
            let sender = q["sender"]
                .as_str()
                .unwrap_or("import")
                .trim_start_matches("step:");
            let from = if sender.is_empty() { "import" } else { sender };
            // Set owner addressing after the normal insert, in this same
            // transaction, so carry-over never reserves a new notification.
            let post: MessagePost = serde_json::from_value(
                json!({"project":ProjectSelector::Id(project),"body":q["body"].as_str().unwrap_or(""),"thread":format!("import-question-{}",q["n"]),"from":from,"needs_reply":true,"title":q["title"],"ui":q["ui"],"input":q["input"]}),
            )?;
            let message = messages::message_post(tx, post, &messages::NoPlanInputs)?;
            tx.sql().execute(
                "UPDATE messages SET \"to\"='owner' WHERE project_id=?1 AND id=?2",
                params![id, message.id.0],
            )?;
            tx.sql().execute("UPDATE records SET payload=json_set(payload,'$.to','owner') WHERE project_id=?1 AND seq=?2",params![id,message.id.0])?;
            ledger["questions"][format!("{}:{}", p.old.name, q["n"])] = json!(message.id);
            if let Some(pred) = p
                .predecessors
                .iter()
                .find(|r| r["source_run"] == q["run"] && r["step"] == sender)
            {
                tx.sql().execute("INSERT INTO question_attachments(project_id,message_id,run_id,step_id,generation,item_index,title,attached_at,detached_at) VALUES (?1,?2,?3,?4,1,?5,?6,?7,?7)",params![id,message.id.0,pred["run"].as_str(),sender,pred["index"].as_i64(),q["title"].as_str(),at])?;
            }
        }
        tx.changed(Some(project), "settings");
        tx.changed(Some(project), "status");
        if fail && n == 0 {
            return Err(StoreError::InvalidDatabase(
                "injected mid-commit failure".into(),
            ));
        }
    }
    ledger["phase"] = json!("complete");
    tx.sql().execute(
        "UPDATE home_meta SET maintenance_settings=?1 WHERE singleton=1",
        [json!({"python_import":ledger}).to_string()],
    )?;
    tx.sql().execute("UPDATE maintenance SET mode='cutover',owner='import-python-home',settings=?1 WHERE singleton=1",[json!({"release_pause_states":ledger["projects"]}).to_string()])?;
    tx.changed(None, "import");
    Ok(ledger)
}
