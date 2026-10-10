//! Each project's compiled plan, kept by what it was compiled from (plan-rows §8.1), and the two
//! generations an edit's tokens carry beside the store's: the fn catalog's (counted in memory by
//! `Catalog::generation`) and the recipe files' (`recipe_generation`).
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use sluice_model::{
    error::PublicError,
    ids::{ProjectId, Revision},
    plan::{Plan, SignatureProvider, compile_rows},
    plan_rows::{CatalogGeneration, CertifiedPlan, RecipeGeneration},
};
use sluice_store::plans;
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

/// Each project's plan as last compiled: `(project) → (rev, catalog_generation, plan)`. A
/// revision fixes the plan's rows and a catalog generation its signatures, so the same pair
/// compiles the same plan. Cold entries come from `compile_rows`; an edit's certified candidate
/// replaces its own base after it commits (`install`). `T` is the compiled plan; the tests
/// hold the install rule with a stand-in.
pub struct PlanCache<T: Send + Sync + 'static = Plan>(Mutex<HashMap<ProjectId, Entry<T>>>);
impl<T: Send + Sync + 'static> Default for PlanCache<T> {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}
impl<T: Send + Sync + 'static> PlanCache<T> {
    /// The plan compiled at `rev` against `generation`, when the cache holds exactly that.
    pub fn get(
        &self,
        project: ProjectId,
        rev: Revision,
        generation: CatalogGeneration,
    ) -> Option<Arc<T>> {
        self.lock()
            .get(&project)
            .filter(|entry| entry.rev == rev && entry.generation == generation)
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
            .get(&project)
            .is_some_and(|entry| (entry.rev, entry.generation) >= (rev, generation))
        {
            drop(held);
            release(plan);
            return;
        }
        let replaced = held.insert(
            project,
            Entry {
                rev,
                generation,
                plan,
            },
        );
        drop(held);
        if let Some(replaced) = replaced {
            release(replaced.plan);
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
        let Some(entry) = held
            .get_mut(&project)
            .filter(|entry| entry.rev == base_rev && entry.generation == generation)
        else {
            drop(held);
            release(plan);
            return false;
        };
        entry.rev = rev;
        let replaced = std::mem::replace(&mut entry.plan, plan);
        drop(held);
        release(replaced);
        true
    }
    fn lock(&self) -> MutexGuard<'_, HashMap<ProjectId, Entry<T>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
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
    /// Install a committed edit's candidate as the plan at `rev` (see `install`).
    pub fn install_certified(&self, project: ProjectId, rev: Revision, compiled: CertifiedPlan) {
        self.install(
            project,
            rev,
            compiled.base_rev,
            compiled.catalog_generation,
            compiled.plan,
        );
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

/// Free a replaced compiled plan off the async workers and the writer: freeing one of
/// thousands of steps takes milliseconds.
fn release<T: Send + Sync + 'static>(plan: Arc<T>) {
    if Arc::strong_count(&plan) > 1 {
        return;
    }
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => drop(runtime.spawn_blocking(move || drop(plan))),
        Err(_) => drop(std::thread::spawn(move || drop(plan))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> ProjectId {
        "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7b".parse().unwrap()
    }

    #[test]
    fn a_candidate_is_installed_only_over_its_own_base() {
        let cache = PlanCache::<&'static str>::default();
        let (p, g1, g2) = (project(), CatalogGeneration(1), CatalogGeneration(2));
        cache.put(p, Revision(5), g1, Arc::new("base 5"));
        // Two edits prepared from rev 5: the first commits as 6 and is installed over 5.
        assert!(cache.install(p, Revision(6), Revision(5), g1, Arc::new("edit a")));
        assert_eq!(cache.get(p, Revision(6), g1).as_deref(), Some(&"edit a"));
        // The second, also prepared from 5 and committed after it as 7 (a no-op net of the
        // first, say), finds 6 there: it is not its base, so it is dropped.
        assert!(!cache.install(p, Revision(7), Revision(5), g1, Arc::new("edit b")));
        assert_eq!(cache.get(p, Revision(6), g1).as_deref(), Some(&"edit a"));
        assert!(cache.get(p, Revision(7), g1).is_none());
        // A catalog publication since the preparation: the candidate's signatures are old.
        cache.put(p, Revision(6), g2, Arc::new("cold 6 on g2"));
        assert!(!cache.install(p, Revision(7), Revision(6), g1, Arc::new("edit c")));
        assert_eq!(
            cache.get(p, Revision(6), g2).as_deref(),
            Some(&"cold 6 on g2")
        );
        // Nothing cached for a project: nothing to install over.
        let other: ProjectId = "0199c3a4-5b6e-7f80-9a1b-2c3d4e5f6a7c".parse().unwrap();
        assert!(!cache.install(other, Revision(2), Revision(1), g1, Arc::new("x")));
        assert!(cache.get(other, Revision(2), g1).is_none());
    }

    #[test]
    fn a_cold_compile_never_replaces_a_newer_plan() {
        let cache = PlanCache::<&'static str>::default();
        let (p, g) = (project(), CatalogGeneration(3));
        cache.put(p, Revision(4), g, Arc::new("4"));
        assert!(cache.install(p, Revision(5), Revision(4), g, Arc::new("5")));
        // A reader that compiled rev 4 from an older snapshot puts it back too late.
        cache.put(p, Revision(4), g, Arc::new("stale 4"));
        assert_eq!(cache.get(p, Revision(5), g).as_deref(), Some(&"5"));
        assert!(cache.get(p, Revision(4), g).is_none());
        // An older catalog at the same revision is older too; a newer one replaces.
        cache.put(
            p,
            Revision(5),
            CatalogGeneration(2),
            Arc::new("old catalog"),
        );
        assert_eq!(cache.get(p, Revision(5), g).as_deref(), Some(&"5"));
        cache.put(
            p,
            Revision(5),
            CatalogGeneration(4),
            Arc::new("new catalog"),
        );
        assert_eq!(
            cache.get(p, Revision(5), CatalogGeneration(4)).as_deref(),
            Some(&"new catalog")
        );
    }

    #[test]
    fn the_recipe_generation_moves_with_the_recipe_files_only() {
        let home = tempfile::tempdir().unwrap();
        let p = project();
        let empty = recipe_generation(home.path(), p);
        assert_eq!(empty.0.len(), 16);
        assert_eq!(recipe_generation(home.path(), p), empty);
        let global = home.path().join("recipes");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(global.join("notes.txt"), "not a recipe").unwrap();
        assert_eq!(recipe_generation(home.path(), p), empty, "only .json files");
        std::fs::write(global.join("lane.json"), "{}").unwrap();
        let one = recipe_generation(home.path(), p);
        assert_ne!(one, empty);
        // The same file in the project's directory is another listing.
        std::fs::remove_file(global.join("lane.json")).unwrap();
        let local = home
            .path()
            .join("projects")
            .join(p.to_string())
            .join("recipes");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("lane.json"), "{}").unwrap();
        let moved = recipe_generation(home.path(), p);
        assert_ne!(moved, empty);
        assert_ne!(moved, one);
        // A size change is a change.
        std::fs::write(local.join("lane.json"), "{ }").unwrap();
        assert_ne!(recipe_generation(home.path(), p), moved);
    }
}
