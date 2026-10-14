//! Each project's compiled plan, kept by what it was compiled from (plan-rows §8.1), and the two
//! generations an edit's tokens carry beside the store's: the fn catalog's (counted in memory by
//! `Catalog::generation`) and the recipe files' (`recipe_generation`).
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use sluice_model::{
    cost::{self, CommitPoint},
    error::PublicError,
    ids::{ProjectId, Revision},
    plan::{Plan, SignatureProvider, compile_rows},
    plan_rows::{CatalogGeneration, CertifiedPlan, RecipeGeneration},
};
use sluice_store::{WriteTransaction, plans};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::SystemTime,
};

/// The recipe files a project sees, as a token compared for equality only (plan-rows §3): the
/// first 16 hex digits of the SHA-256 of the home's `recipes/` then the project's
/// `projects/<id>/recipes/` listing, each `.json` file's name, size and modification time in
/// nanoseconds, sorted by name. A missing directory lists nothing. Recipe files change outside
/// sluice, so this is a digest, not a counter.
pub fn recipe_generation(home: &Path, project: ProjectId) -> RecipeGeneration {
    let mut listing = String::new();
    for dir in [
        home.join("recipes"),
        home.join("projects")
            .join(project.to_string())
            .join("recipes"),
    ] {
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|file| {
                let name = file.file_name().to_str()?.to_owned();
                if !name.ends_with(".json") {
                    return None;
                }
                // Through a link, as the recipe loader reads it.
                let meta = std::fs::metadata(file.path()).ok()?;
                let at = meta
                    .modified()
                    .ok()?
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .ok()?
                    .as_nanos();
                Some(format!("{name}\t{}\t{at}\n", meta.len()))
            })
            .collect();
        files.sort();
        listing.extend(files);
        listing.push_str("--\n");
    }
    let digest = Sha256::digest(listing.as_bytes());
    RecipeGeneration(digest[..8].iter().map(|b| format!("{b:02x}")).collect())
}

struct Entry<T> {
    rev: Revision,
    generation: CatalogGeneration,
    plan: Arc<T>,
}
/// A committed edit's candidate between its commit and its install, over its base.
struct Staged<T> {
    base: Revision,
    entry: Entry<T>,
}
struct Held<T> {
    plans: HashMap<ProjectId, Entry<T>>,
    staged: HashMap<ProjectId, Staged<T>>,
}

/// Each project's plan as last compiled: `(project) → (rev, catalog_generation, plan)`. A
/// revision fixes the plan's rows and a catalog generation its signatures, so the same pair
/// compiles the same plan. Cold entries come from `compile_rows`; an edit's certified candidate
/// replaces its own base the moment it commits (`install_on_commit`). `T` is the compiled
/// plan; the tests hold the install rule with a stand-in.
pub struct PlanCache<T: Send + Sync + 'static = Plan> {
    held: Mutex<Held<T>>,
    /// The runtime replaced plans are freed on when the caller is outside one (the writer).
    runtime: Option<tokio::runtime::Handle>,
}
impl<T: Send + Sync + 'static> Default for PlanCache<T> {
    fn default() -> Self {
        Self {
            held: Mutex::new(Held {
                plans: HashMap::new(),
                staged: HashMap::new(),
            }),
            runtime: tokio::runtime::Handle::try_current().ok(),
        }
    }
}
impl<T: Send + Sync + 'static> PlanCache<T> {
    /// The plan compiled at `rev` against `generation`, when the cache holds exactly that, or a
    /// committed edit's candidate staged as exactly that (`stage`).
    pub fn get(
        &self,
        project: ProjectId,
        rev: Revision,
        generation: CatalogGeneration,
    ) -> Option<Arc<T>> {
        let held = self.lock();
        let at = |entry: &&Entry<T>| entry.rev == rev && entry.generation == generation;
        held.plans
            .get(&project)
            .filter(at)
            .or_else(|| held.staged.get(&project).map(|s| &s.entry).filter(at))
            .map(|entry| entry.plan.clone())
    }
    /// Keep a cold compile of the plan at `rev` against `generation`. It replaces an entry only
    /// when it is newer (a later revision, or a later catalog at the same revision), so a slow
    /// reader never puts an older plan over one an edit installed.
    pub fn put(
        &self,
        project: ProjectId,
        rev: Revision,
        generation: CatalogGeneration,
        plan: Arc<T>,
    ) {
        let mut held = self.lock();
        if held
            .plans
            .get(&project)
            .is_some_and(|entry| (entry.rev, entry.generation) >= (rev, generation))
        {
            drop(held);
            self.release(plan);
            return;
        }
        let replaced = held.plans.insert(
            project,
            Entry {
                rev,
                generation,
                plan,
            },
        );
        drop(held);
        if let Some(replaced) = replaced {
            self.release(replaced.plan);
        }
    }
    /// Install an edit's certified candidate as the plan at `rev` (plan-rows §8.1), only when
    /// the cache still holds the candidate's own base, `(base_rev, generation)`. When another
    /// edit was installed first, the catalog was republished or the base was never cached, the
    /// candidate is dropped and the next reader compiles cold. Whether it was installed.
    pub fn install(
        &self,
        project: ProjectId,
        rev: Revision,
        base_rev: Revision,
        generation: CatalogGeneration,
        plan: Arc<T>,
    ) -> bool {
        let mut held = self.lock();
        let (installed, freed) =
            Self::install_held(&mut held, project, rev, base_rev, generation, plan);
        drop(held);
        self.release(freed);
        installed
    }
    /// `install` under the lock: whether it was installed, and the plan it frees.
    fn install_held(
        held: &mut Held<T>,
        project: ProjectId,
        rev: Revision,
        base_rev: Revision,
        generation: CatalogGeneration,
        plan: Arc<T>,
    ) -> (bool, Arc<T>) {
        match held
            .plans
            .get_mut(&project)
            .filter(|entry| entry.rev == base_rev && entry.generation == generation)
        {
            Some(entry) => {
                entry.rev = rev;
                (true, std::mem::replace(&mut entry.plan, plan))
            }
            None => (false, plan),
        }
    }
    /// Stage a committed edit's candidate as the plan at `rev`, from its writer transaction,
    /// before that transaction commits: until the returned guard installs it (`install`'s rule)
    /// or drops it, a reader whose snapshot shows `rev` takes it (`get`) rather than compiling
    /// the plan whole. That is sound because the one transaction that can commit `rev` while
    /// the candidate is staged is the one that staged it: the writer runs one transaction at a
    /// time, and its guard is settled on the writer's thread before the next begins (see
    /// `install_on_commit`). A reader that sees `rev` therefore sees this edit's commit.
    pub fn stage(
        self: &Arc<Self>,
        project: ProjectId,
        rev: Revision,
        base_rev: Revision,
        generation: CatalogGeneration,
        plan: Arc<T>,
    ) -> Staging<T> {
        let staged = Staged {
            base: base_rev,
            entry: Entry {
                rev,
                generation,
                plan,
            },
        };
        let replaced = self.lock().staged.insert(project, staged);
        if let Some(replaced) = replaced {
            self.release(replaced.entry.plan);
        }
        Staging {
            cache: self.clone(),
            project,
            rev,
            settled: false,
        }
    }
    /// The staged candidate for `project` at `rev`, if it is still the one staged.
    fn take_staged(held: &mut Held<T>, project: ProjectId, rev: Revision) -> Option<Staged<T>> {
        if held
            .staged
            .get(&project)
            .is_some_and(|s| s.entry.rev == rev)
        {
            held.staged.remove(&project)
        } else {
            None
        }
    }
    fn lock(&self) -> MutexGuard<'_, Held<T>> {
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Free a replaced compiled plan off the async workers and the writer: freeing one of
    /// thousands of steps takes milliseconds.
    fn release(&self, plan: Arc<T>) {
        if Arc::strong_count(&plan) > 1 {
            return;
        }
        match tokio::runtime::Handle::try_current()
            .ok()
            .or_else(|| self.runtime.clone())
        {
            Some(runtime) => drop(runtime.spawn_blocking(move || drop(plan))),
            None => drop(std::thread::spawn(move || drop(plan))),
        }
    }
}

/// A staged candidate (`PlanCache::stage`): installed by `install`, or unstaged when dropped
/// first (its transaction rolled back).
pub struct Staging<T: Send + Sync + 'static> {
    cache: Arc<PlanCache<T>>,
    project: ProjectId,
    rev: Revision,
    settled: bool,
}
impl<T: Send + Sync + 'static> Staging<T> {
    /// Install the staged candidate by `install`'s rule, in the same step that unstages it, so
    /// a reader finds it either staged or installed. Whether it was installed.
    pub fn install(mut self) -> bool {
        self.settled = true;
        let mut held = self.cache.lock();
        let Some(staged) = PlanCache::take_staged(&mut held, self.project, self.rev) else {
            return false;
        };
        let (installed, freed) = PlanCache::install_held(
            &mut held,
            self.project,
            staged.entry.rev,
            staged.base,
            staged.entry.generation,
            staged.entry.plan,
        );
        drop(held);
        self.cache.release(freed);
        installed
    }
}
impl<T: Send + Sync + 'static> Drop for Staging<T> {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let staged = PlanCache::take_staged(&mut self.cache.lock(), self.project, self.rev);
        if let Some(staged) = staged {
            self.cache.release(staged.entry.plan);
        }
    }
}

impl PlanCache<Plan> {
    /// The project's plan at its current revision (read in the caller's snapshot or writer
    /// transaction), compiled against `generation`'s `signatures`: the cached one when it
    /// matches, else compiled cold from the rows (`compile_rows`) and kept.
    pub fn current(
        &self,
        sql: &Connection,
        project: ProjectId,
        generation: CatalogGeneration,
        signatures: &impl SignatureProvider,
    ) -> sluice_store::Result<(Revision, Arc<Plan>)> {
        let rev: i64 = sql.query_row(
            "SELECT rev FROM plans WHERE project_id=?1",
            [project.to_string()],
            |r| r.get(0),
        )?;
        let rev = Revision(rev as u64);
        if let Some(plan) = self.get(project, rev, generation) {
            return Ok((rev, plan));
        }
        let plan = Arc::new(compile_cold(sql, project, signatures)?);
        self.put(project, rev, generation, plan.clone());
        Ok((rev, plan))
    }
    /// Make a committed edit's candidate the plan at `rev` from inside the writer transaction
    /// that committed it: staged now (`stage`), installed on the writer's thread the moment the
    /// transaction commits (`WriteTransaction::on_commit`), unstaged if it rolls back. No
    /// reader of `rev` finds the cache behind it, so none compiles the plan whole.
    pub fn install_on_commit(
        self: &Arc<Self>,
        tx: &mut WriteTransaction<'_>,
        project: ProjectId,
        rev: Revision,
        compiled: CertifiedPlan,
    ) {
        let staging = self.stage(
            project,
            rev,
            compiled.base_rev,
            compiled.catalog_generation,
            compiled.plan,
        );
        tx.on_commit(move || {
            cost::commit_point(&CommitPoint { project, rev });
            staging.install();
        });
    }
}

/// Compile the project's stored plan from its rows (a whole-plan read and compile).
pub fn compile_cold(
    sql: &Connection,
    project: ProjectId,
    signatures: &impl SignatureProvider,
) -> sluice_store::Result<Plan> {
    let rows = plans::read_plan_rows(sql, project)?;
    compile_rows(&rows, signatures).map_err(|errors| {
        PublicError::Invalid {
            message: "invalid stored plan".into(),
            errors: errors.into_iter().map(|e| e.to_string()).collect(),
        }
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> ProjectId {
        "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b".parse().unwrap()
    }

    #[test]
    fn a_staged_candidate_is_the_plan_at_its_revision_until_installed_or_dropped() {
        let cache = Arc::new(PlanCache::<&'static str>::default());
        let (p, g) = (project(), CatalogGeneration(1));
        cache.put(p, Revision(5), g, Arc::new("base 5"));
        // Committed as 6 and not installed yet: a reader of 6 takes the candidate, a reader of
        // 5 still has its base, and another catalog's reader has neither.
        let staging = cache.stage(p, Revision(6), Revision(5), g, Arc::new("edit 6"));
        assert_eq!(cache.get(p, Revision(6), g).as_deref(), Some(&"edit 6"));
        assert_eq!(cache.get(p, Revision(5), g).as_deref(), Some(&"base 5"));
        assert!(cache.get(p, Revision(6), CatalogGeneration(2)).is_none());
        // Installed over its base in the step that unstages it.
        assert!(staging.install());
        assert_eq!(cache.get(p, Revision(6), g).as_deref(), Some(&"edit 6"));
        assert!(cache.get(p, Revision(5), g).is_none());
        // A transaction that rolls back drops its guard: the candidate is gone.
        let staging = cache.stage(p, Revision(7), Revision(6), g, Arc::new("rolled back"));
        assert!(cache.get(p, Revision(7), g).is_some());
        drop(staging);
        assert!(cache.get(p, Revision(7), g).is_none());
        assert_eq!(cache.get(p, Revision(6), g).as_deref(), Some(&"edit 6"));
        // The install rule still holds: a base never cached is not installed over.
        let other: ProjectId = "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7c".parse().unwrap();
        let staging = cache.stage(other, Revision(2), Revision(1), g, Arc::new("x"));
        assert!(!staging.install());
        assert!(cache.get(other, Revision(2), g).is_none());
    }
}
