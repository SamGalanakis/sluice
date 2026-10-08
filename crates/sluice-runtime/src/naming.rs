//! Each project's step and unit names (`sluice_model::naming`), read from its stored plan, its
//! recipe files and the prompt files its steps name, and kept per process while none of them
//! changes: recomputed when the plan's rev or a recipe file changes, and at least every minute
//! so an edited prompt file shows.
use rusqlite::Connection;
use sluice_model::{
    ids::ProjectId,
    naming::{Naming, name_plan},
    recipe::Recipe,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};

/// How long a project's names are kept while its plan and recipes stay the same.
const FRESH: Duration = Duration::from_secs(60);
/// How much of a prompt file is read for its title.
const PROMPT_BYTES: u64 = 4096;

/// A project's names and the recipes they came from (the ones that check, by name).
#[derive(Debug, Default)]
pub struct ProjectNaming {
    pub naming: Naming,
    pub recipes: indexmap::IndexMap<String, Arc<Recipe>>,
}
impl ProjectNaming {
    /// The recipe a unit was made from, when one matches it.
    pub fn recipe_of(&self, unit: &str) -> Option<&Arc<Recipe>> {
        self.naming
            .unit(unit)
            .filter(|u| !u.recipe.is_empty())
            .and_then(|u| self.recipes.get(&u.recipe))
    }
}
struct Kept {
    rev: i64,
    recipes: String,
    at: Instant,
    names: Arc<ProjectNaming>,
}
type Cache = Mutex<HashMap<(PathBuf, ProjectId), Kept>>;
fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The project's names, inside the caller's read snapshot. `home` is the home directory.
pub fn for_project(
    sql: &Connection,
    home: &Path,
    project: ProjectId,
) -> sluice_store::Result<Arc<ProjectNaming>> {
    let rev: i64 = sql.query_row(
        "SELECT rev FROM plans WHERE project_id=?1",
        [project.to_string()],
        |r| r.get(0),
    )?;
    let recipes = recipe_files(home, project);
    let key = (home.to_owned(), project);
    if let Some(kept) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key)
        && kept.rev == rev
        && kept.recipes == recipes
        && kept.at.elapsed() < FRESH
    {
        return Ok(kept.names.clone());
    }
    let doc: String = sql.query_row(
        "SELECT doc FROM plans WHERE project_id=?1",
        [project.to_string()],
        |r| r.get(0),
    )?;
    let doc: serde_json::Value = serde_json::from_str(&doc)?;
    let entries = crate::dispatch_ext::load_recipes(home, project).unwrap_or_default();
    // the project's own recipes before the global ones, each by name
    let mut usable: Vec<(bool, String, Arc<Recipe>)> = entries
        .values()
        .filter_map(|e| {
            let recipe = e.recipe.as_ref().ok()?;
            Some((
                e.scope != "project",
                e.name.clone(),
                Arc::new(recipe.clone()),
            ))
        })
        .collect();
    usable.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let ordered: Vec<&Recipe> = usable.iter().map(|(_, _, r)| r.as_ref()).collect();
    let empty = serde_json::Map::new();
    let steps = doc
        .get("steps")
        .and_then(|s| s.as_object())
        .unwrap_or(&empty);
    let naming = name_plan(steps, &ordered, &mut |path| prompt_head(path));
    let names = Arc::new(ProjectNaming {
        naming,
        recipes: usable
            .into_iter()
            .map(|(_, name, recipe)| (name, recipe))
            .collect(),
    });
    let mut held = cache().lock().unwrap_or_else(|e| e.into_inner());
    held.insert(
        key,
        Kept {
            rev,
            recipes,
            at: Instant::now(),
            names: names.clone(),
        },
    );
    Ok(names)
}

/// The recipe files a project sees, by name, size and modification time: names change with them.
fn recipe_files(home: &Path, project: ProjectId) -> String {
    let mut out = String::new();
    for dir in [
        home.join("recipes"),
        home.join("projects")
            .join(project.to_string())
            .join("recipes"),
    ] {
        let Ok(files) = std::fs::read_dir(&dir) else {
            out.push('|');
            continue;
        };
        let mut found: Vec<String> = files
            .flatten()
            .filter_map(|f| {
                let meta = f.metadata().ok()?;
                let at = meta
                    .modified()
                    .ok()?
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .ok()?
                    .as_nanos();
                Some(format!(
                    "{}:{}:{at}",
                    f.file_name().to_string_lossy(),
                    meta.len()
                ))
            })
            .collect();
        found.sort();
        out.push_str(&found.join(","));
        out.push('|');
    }
    out
}

/// A prompt file's opening (what its title is read from), cached by path, size and time.
fn prompt_head(path: &str) -> Option<String> {
    use std::io::Read;
    type Heads = Mutex<HashMap<String, (u64, SystemTime, Option<String>)>>;
    static HEADS: OnceLock<Heads> = OnceLock::new();
    let heads = HEADS.get_or_init(Default::default);
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    let modified = meta.modified().ok()?;
    if let Some((len, at, head)) = heads.lock().unwrap_or_else(|e| e.into_inner()).get(path)
        && *len == meta.len()
        && *at == modified
    {
        return head.clone();
    }
    let mut bytes = vec![];
    let head = std::fs::File::open(path)
        .ok()
        .and_then(|f| f.take(PROMPT_BYTES).read_to_end(&mut bytes).ok())
        .map(|_| String::from_utf8_lossy(&bytes).into_owned());
    let mut held = heads.lock().unwrap_or_else(|e| e.into_inner());
    if held.len() > 4096 {
        held.clear();
    }
    held.insert(path.to_owned(), (meta.len(), modified, head.clone()));
    head
}
