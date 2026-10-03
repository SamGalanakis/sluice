//! Transaction adapters shared by commands, admission and guardian completion.
use crate::{
    calls::{CallRegistry, FrozenFunction},
    verify::{RegistryInspection, VerificationRegistry},
};
use indexmap::IndexMap;
use serde_json::json;
use sluice_model::{
    commands::MessagePost,
    error::PublicError,
    ids::*,
    plan::{FnSignature, SignatureProvider},
    types::Type,
};
use sluice_store::{
    WriteTransaction,
    attempts::{self, ExecutionHooks},
    messages,
    plans::{self, PlanContext, RetryMessages},
    projects, resources,
};

/// A registry certificate supplied by the composition root. Later registry owners
/// can construct this from their visible, validated descriptors.
#[derive(Clone, Default)]
pub struct Catalog(pub IndexMap<String, FnSignature>);
impl Catalog {
    pub fn core() -> Self {
        let mut entries = IndexMap::new();
        for descriptor in crate::builtins::core::descriptors() {
            entries.insert(
                descriptor.name.into(),
                FnSignature {
                    inputs: descriptor
                        .inputs
                        .into_iter()
                        .map(|(n, t)| (n.into(), t))
                        .collect(),
                    outputs: descriptor
                        .outputs
                        .into_iter()
                        .map(|(n, t)| (n.into(), t))
                        .collect(),
                    open: descriptor.name == "core.external",
                    ..Default::default()
                },
            );
        }
        Self(entries)
    }
    pub fn fixtures() -> Self {
        let mut catalog = Self::core();
        for name in [
            "fixture.echo",
            "fixture.wait",
            "fixture.fail",
            "fixture.submit",
        ] {
            catalog.0.insert(
                name.into(),
                FnSignature {
                    inputs: [("value".into(), Type::Any)].into(),
                    outputs: [("value".into(), Type::Any)].into(),
                    open: name == "fixture.submit",
                    ..Default::default()
                },
            );
        }
        catalog.0.insert(
            "fixture.capacity".into(),
            FnSignature {
                outputs: [("capacity".into(), Type::Int)].into(),
                ..Default::default()
            },
        );
        catalog
    }
}
impl SignatureProvider for Catalog {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        self.0.get(name).cloned()
    }
}
impl VerificationRegistry for Catalog {
    fn inspect(&self, _: Option<ProjectId>) -> RegistryInspection {
        RegistryInspection {
            signatures: self.0.clone(),
            problems: vec![],
        }
    }
}
impl CallRegistry for Catalog {
    fn freeze(&self, _: Option<ProjectId>, name: &str) -> Result<FrozenFunction, PublicError> {
        let signature = self.signature(name).ok_or_else(|| PublicError::NotFound {
            message: format!("unknown fn {name}"),
        })?;
        let mut frozen =
            FrozenFunction::from_signature(name.into(), signature, "runtime-v1".into());
        frozen.bundle =
            serde_json::from_value(json!({"capability":new_capability()})).map_err(|e| {
                PublicError::Storage {
                    message: e.to_string(),
                }
            })?;
        Ok(frozen)
    }
}
pub fn new_capability() -> sluice_model::rpc::RunCapability {
    sluice_model::rpc::RunCapability::new(format!("{}{}", InvocationId::new(), InvocationId::new()))
}

pub struct Hooks;
impl RetryMessages for Hooks {
    fn validate_retry(
        &self,
        tx: &WriteTransaction<'_>,
        project: ProjectId,
        steps: &[StepId],
        body: &str,
        author: &str,
    ) -> sluice_store::Result<()> {
        if body.trim().is_empty() || body.len() > 65536 || author.len() > 1024 {
            return Err(PublicError::BadRequest {
                message: "invalid retry feedback or author".into(),
            }
            .into());
        }
        messages::resolve_project(tx.sql(), &ProjectSelector::Id(project))?;
        for step in steps {
            let exists: bool = tx.sql().query_row(
                "SELECT EXISTS(SELECT 1 FROM steps WHERE project_id=?1 AND step_id=?2)",
                (project.to_string(), step.as_str()),
                |r| r.get(0),
            )?;
            if !exists {
                return Err(PublicError::NotFound {
                    message: format!("no step {step}"),
                }
                .into());
            }
        }
        Ok(())
    }
    fn post_retry(
        &mut self,
        tx: &mut WriteTransaction<'_>,
        project: ProjectId,
        step: &StepId,
        body: &str,
        author: &str,
    ) -> sluice_store::Result<()> {
        messages::message_post(
            tx,
            MessagePost {
                project: ProjectSelector::Id(project),
                body: body.into(),
                thread: Some(format!("step-{step}")),
                to: Some(step.to_string()),
                needs_reply: Some(false),
                reply_to: None,
                answer: None,
                title: None,
                ui: None,
                input: None,
                data: None,
                from: Some(author.into()),
                run: None,
                author: Some(author.into()),
            },
            &messages::NoPlanInputs,
        )?;
        Ok(())
    }
}
impl ExecutionHooks for Hooks {
    fn assign(
        &mut self,
        tx: &mut WriteTransaction<'_>,
        id: &attempts::AttemptIdentity,
        cursor: i64,
        exact: Option<&attempts::AssignedRange>,
    ) -> sluice_store::Result<attempts::AssignedRange> {
        let range = messages::assign_run_range(tx, id.project, id.run, cursor, exact)?;
        Ok(attempts::AssignedRange {
            after: range.after.0,
            through: range.through.0,
        })
    }
    fn started(
        &mut self,
        tx: &mut WriteTransaction<'_>,
        id: &attempts::AttemptIdentity,
        _: &attempts::AssignedRange,
    ) -> sluice_store::Result<()> {
        messages::advance_cursor(tx, id.project, id.run)?;
        Ok(())
    }
    fn hold(
        &mut self,
        tx: &mut WriteTransaction<'_>,
        id: &attempts::AttemptIdentity,
        needs: &[(String, u64)],
        _: bool,
    ) -> sluice_store::Result<()> {
        resources::hold_needs(tx, id.run, &needs.iter().cloned().collect())?;
        Ok(())
    }
    fn release(
        &mut self,
        tx: &mut WriteTransaction<'_>,
        id: &attempts::AttemptIdentity,
    ) -> sluice_store::Result<()> {
        resources::release_stopped_run(tx, id.run)?;
        Ok(())
    }
}
pub struct InputSetter(pub PlanContext);
impl messages::PlanInputSetter for InputSetter {
    fn set_input(
        &self,
        tx: &mut WriteTransaction<'_>,
        input: messages::InputAnswer<'_>,
    ) -> sluice_store::Result<()> {
        if input.project != self.0.project {
            return Err(PublicError::BadRequest {
                message: "input project mismatch".into(),
            }
            .into());
        }
        plans::set_input(
            tx,
            &self.0,
            input.name,
            input.value.clone(),
            input.author.into(),
            input.reason.into(),
        )
    }
}
pub struct ResourceSettings(pub Catalog);
impl projects::ResourceSettings for ResourceSettings {
    fn set_resources(
        &self,
        tx: &mut WriteTransaction<'_>,
        project: ProjectId,
        patch: &serde_json::Value,
    ) -> sluice_store::Result<bool> {
        resources::patch_resources(tx, project, patch, &self.0)
    }
}
