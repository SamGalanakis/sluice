//! One published registry feeds signatures, inspection and frozen execution.
use crate::{
    calls::FrozenFunction,
    dispatch::{Catalog, new_capability},
    registry::{self, FnRegistry},
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sluice_model::{error::PublicError, ids::*, plan::FnSignature, rpc::JsonMap};
use sluice_store::{Writer, artifacts::ArtifactJob};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenExecution {
    pub name: String,
    pub fn_dir: Option<PathBuf>,
    pub bundle_root: Option<PathBuf>,
    pub job: Option<InvocationId>,
    pub inputs: IndexMap<String, sluice_model::types::Type>,
    pub outputs: IndexMap<String, sluice_model::types::Type>,
    pub open: bool,
    pub submits: BTreeMap<String, sluice_agents::prompt::Port>,
}
struct View {
    catalog: Catalog,
    functions: IndexMap<String, FrozenExecution>,
    listing: Vec<Value>,
    details: IndexMap<String, Value>,
    blocking: Vec<String>,
}
pub struct Publication {
    pub registry: FnRegistry,
    base: Catalog,
    views: RwLock<BTreeMap<Option<ProjectId>, View>>,
    /// The registry version and projects the current views were published
    /// from, read before their scans; guarded by the refresh lock.
    refresh: tokio::sync::Mutex<Option<(u64, Vec<ProjectId>)>>,
    /// Refreshes that got past the lock and scanned, for diagnostics.
    refreshes: AtomicU64,
}
type Published<'a> = tokio::sync::MutexGuard<'a, Option<(u64, Vec<ProjectId>)>>;
impl Publication {
    pub fn new(registry: FnRegistry, base: Catalog) -> Arc<Self> {
        Arc::new(Self {
            registry,
            base,
            views: RwLock::new(BTreeMap::new()),
            refresh: tokio::sync::Mutex::new(None),
            refreshes: AtomicU64::new(0),
        })
    }
    /// Scan every scope and republish what changed, waiting for any refresh
    /// already running.
    pub async fn refresh(
        &self,
        writer: &Writer,
        projects: Vec<ProjectId>,
    ) -> Result<(), PublicError> {
        let published = self.refresh.lock().await;
        self.publish_locked(published, writer, projects).await
    }
    /// The read path's refresh. It never waits behind a running refresh (the
    /// views it would replace are a complete earlier publication) and does not
    /// scan when neither the registry version (bumped by the watcher and by
    /// scans) nor the project list moved since the last publication.
    pub async fn refresh_if_stale(
        &self,
        writer: &Writer,
        projects: Vec<ProjectId>,
    ) -> Result<(), PublicError> {
        let Ok(published) = self.refresh.try_lock() else {
            return Ok(());
        };
        if published.as_ref().is_some_and(|(seen, seen_projects)| {
            *seen == self.registry.version() && *seen_projects == projects
        }) {
            return Ok(());
        }
        self.publish_locked(published, writer, projects).await
    }
    /// How many refreshes have scanned the fn scopes.
    pub fn refreshes(&self) -> u64 {
        self.refreshes.load(Ordering::Relaxed)
    }
    async fn publish_locked(
        &self,
        mut published: Published<'_>,
        writer: &Writer,
        projects: Vec<ProjectId>,
    ) -> Result<(), PublicError> {
        self.refreshes.fetch_add(1, Ordering::Relaxed);
        // Every scope is re-fingerprinted by its scan, and any change to a scope
        // (a scan's or the watcher's) bumps the version. Unchanged since the last
        // publication, the views stand: republishing reads and digests every fn
        // file and writes, which an idle refresh tick or every command would
        // otherwise repeat.
        let version = self.registry.version();
        let registries: Vec<_> = std::iter::once(None)
            .chain(projects.iter().copied().map(Some))
            .map(|project| (project, self.registry.registry(project)))
            .collect();
        if published.as_ref().is_some_and(|(seen, seen_projects)| {
            *seen == self.registry.version() && *seen_projects == projects
        }) {
            return Ok(());
        }
        let mut views = BTreeMap::new();
        let mut jobs: BTreeMap<Option<ProjectId>, ArtifactJob> = BTreeMap::new();
        for (project, registry) in registries {
            let mut catalog = self.base.clone();
            let mut functions = IndexMap::new();
            let mut details = IndexMap::new();
            for name in registry.names() {
                let f = registry.get(name).expect("resolved name");
                let mut signature = FnSignature {
                    inputs: f.inputs.clone(),
                    outputs: f.outputs.clone(),
                    open: f.open,
                    submits: f
                        .submits
                        .iter()
                        .map(|(n, t)| {
                            (
                                n.clone(),
                                sluice_model::plan::Declaration {
                                    ty: t.clone(),
                                    doc: f.submit_docs.get(n).cloned(),
                                },
                            )
                        })
                        .collect(),
                };
                if std::env::var_os("SLUICE_FIXTURE").is_some()
                    && name == "agent.run"
                    && let Some(sluice_model::types::Type::Enum(engines)) =
                        signature.inputs.get_mut("engine")
                {
                    engines.push("fake".into());
                }
                let scope = f.project;
                let job = if f.dir.is_some() {
                    if let std::collections::btree_map::Entry::Vacant(entry) = jobs.entry(scope) {
                        entry.insert(
                            registry::prepare_run_pin(writer, &self.registry, f)
                                .await?
                                .expect("Python bundle"),
                        );
                    }
                    jobs.get(&scope).cloned()
                } else {
                    None
                };
                let dispatch = registry::dispatch(&self.registry, f, job.clone());
                let fn_dir = dispatch.fn_dir();
                if let Some(path) = &fn_dir {
                    let pinned: Value = sluice_model::rpc::decode_json(
                        &std::fs::read(path.join("fn.json")).map_err(|e| PublicError::Storage {
                            message: e.to_string(),
                        })?,
                    )?;
                    if pinned != f.raw {
                        return Err(PublicError::Conflict {
                            message: "fn changed during publication".into(),
                            current_rev: None,
                        });
                    }
                }
                let bundle_root = job.as_ref().map(|j| self.registry.home().join(&j.path));
                functions.insert(
                    name.into(),
                    FrozenExecution {
                        name: name.into(),
                        fn_dir,
                        bundle_root,
                        job: job.map(|j| j.job_id),
                        inputs: signature.inputs.clone(),
                        outputs: signature.outputs.clone(),
                        open: signature.open,
                        submits: signature
                            .submits
                            .iter()
                            .map(|(n, d)| {
                                (
                                    n.clone(),
                                    sluice_agents::prompt::Port {
                                        r#type: d.ty.clone(),
                                        doc: d.doc.clone().unwrap_or_default(),
                                    },
                                )
                            })
                            .collect(),
                    },
                );
                catalog.0.insert(name.into(), signature);
                details.insert(name.into(), f.detail());
            }
            views.insert(
                project,
                View {
                    catalog,
                    functions,
                    listing: registry.listing(),
                    details,
                    blocking: registry
                        .blocking()
                        .iter()
                        .map(|p| format!("{}: {}", p.location, p.message))
                        .collect(),
                },
            );
        }
        *self.views.write().unwrap_or_else(|e| e.into_inner()) = views;
        *published = Some((version, projects));
        Ok(())
    }
    pub fn catalog(&self, project: Option<ProjectId>) -> Catalog {
        let views = self.views.read().unwrap_or_else(|e| e.into_inner());
        views
            .get(&project)
            .or_else(|| views.get(&None))
            .map(|v| v.catalog.clone())
            .unwrap_or_else(|| self.base.clone())
    }
    pub fn resolved(
        &self,
        project: Option<ProjectId>,
        name: &str,
    ) -> Result<FrozenExecution, PublicError> {
        let views = self.views.read().unwrap_or_else(|e| e.into_inner());
        let view = views
            .get(&project)
            .or_else(|| views.get(&None))
            .ok_or_else(|| PublicError::Storage {
                message: "registry not published".into(),
            })?;
        if !view.blocking.is_empty() {
            return Err(PublicError::Invalid {
                message: "project fn registry is blocked".into(),
                errors: view.blocking.clone(),
            });
        }
        if let Some(signature) = self.base.0.get(name) {
            return Ok(FrozenExecution {
                name: name.into(),
                fn_dir: None,
                bundle_root: None,
                job: None,
                inputs: signature.inputs.clone(),
                outputs: signature.outputs.clone(),
                open: signature.open,
                submits: Default::default(),
            });
        }
        view.functions
            .get(name)
            .cloned()
            .ok_or_else(|| PublicError::NotFound {
                message: format!("unknown fn {name}"),
            })
    }
    pub fn problems(&self, project: Option<ProjectId>) -> Vec<String> {
        self.views
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&project)
            .map(|v| v.blocking.clone())
            .unwrap_or_default()
    }
    pub fn listing(&self, project: Option<ProjectId>) -> Vec<Value> {
        let views = self.views.read().unwrap_or_else(|e| e.into_inner());
        views
            .get(&project)
            .or_else(|| views.get(&None))
            .map(|v| v.listing.clone())
            .unwrap_or_default()
    }
    pub fn detail(&self, project: Option<ProjectId>, name: &str) -> Result<Value, PublicError> {
        let views = self.views.read().unwrap_or_else(|e| e.into_inner());
        views
            .get(&project)
            .or_else(|| views.get(&None))
            .and_then(|v| v.details.get(name))
            .cloned()
            .ok_or_else(|| PublicError::NotFound {
                message: format!("unknown fn {name}"),
            })
    }
    pub fn freeze(
        &self,
        project: Option<ProjectId>,
        name: &str,
    ) -> Result<FrozenFunction, PublicError> {
        if self.base.0.contains_key(name) && name.starts_with("fixture.") {
            return crate::calls::CallRegistry::freeze(&self.base, project, name);
        }
        let execution = self.resolved(project, name)?;
        let mut function = FrozenFunction::from_signature(
            name.into(),
            FnSignature {
                inputs: execution.inputs.clone(),
                outputs: execution.outputs.clone(),
                open: execution.open,
                submits: execution
                    .submits
                    .iter()
                    .map(|(n, p)| {
                        (
                            n.clone(),
                            sluice_model::plan::Declaration {
                                ty: p.r#type.clone(),
                                doc: Some(p.doc.clone()),
                            },
                        )
                    })
                    .collect(),
            },
            "runtime-v1".into(),
        );
        function.bundle =
            serde_json::from_value(json!({"capability":new_capability(),"execution":execution}))
                .map_err(|e| PublicError::Storage {
                    message: e.to_string(),
                })?;
        Ok(function)
    }
}
pub fn declarations(types: &IndexMap<String, sluice_model::types::Type>) -> JsonMap {
    JsonMap(
        types
            .iter()
            .map(|(n, t)| (n.clone(), t.form().try_into().expect("type form")))
            .collect(),
    )
}
