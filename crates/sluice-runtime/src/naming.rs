//! Each project's step and unit names (`sluice_model::naming`), read from its step rows, its
//! recipe files and the prompt files its steps name, and kept per process while none of them
//! changes: keyed by the plan's revision and the recipe files' generation (plan-rows §3,
//! `recipe_generation`), and recomputed at least every minute so an edited prompt file shows.
use rusqlite::Connection;
use sluice_model::{
    ids::{ProjectId, Revision, UnitName},
    naming::{Naming, name_plan},
    plan_rows::{RecipeGeneration, RowSelection, StepProjection},
    recipe::Recipe,
};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};

/// How long a project's names are kept while its plan and recipes stay the same.
const FRESH: Duration = Duration::from_secs(60);
/// How much of a prompt file is read for its title.
const PROMPT_BYTES: u64 = 4096;

/// A project's names, the recipes they came from (the ones that check, by name), and what
/// they were worked out from: the plan's revision and the recipe files' generation.
#[derive(Debug)]
pub struct ProjectNaming {
    pub naming: Naming,
    pub recipes: indexmap::IndexMap<String, Arc<Recipe>>,
    pub rev: Revision,
    pub recipe_generation: RecipeGeneration,
}
/// No names yet: a plan at no revision, before any recipe was read.
impl Default for ProjectNaming {
    fn default() -> Self {
        Self {
            naming: Naming::default(),
            recipes: Default::default(),
            rev: Revision(0),
            recipe_generation: RecipeGeneration(String::new()),
        }
    }
}
impl ProjectNaming {
    /// The recipe a unit was made from, when one matches it.
    pub fn recipe_of(&self, unit: &str) -> Option<&Arc<Recipe>> {
        self.naming
            .unit(unit)
            .filter(|u| !u.recipe.is_empty())
            .and_then(|u| self.recipes.get(&u.recipe))
    }
    /// The name of the recipe a unit matches now, if any (`plan_read`'s `recipe`).
    pub fn recipe_name(&self, unit: &str) -> Option<&str> {
        self.naming
            .unit(unit)
            .map(|u| u.recipe.as_str())
            .filter(|name| !name.is_empty())
    }
    /// The units a recipe matches now, in name order: a `recipe` filter resolved to units.
    pub fn units_of(&self, recipe: &str) -> Vec<UnitName> {
        self.naming
            .units
            .iter()
            .filter(|(_, unit)| unit.recipe == recipe)
            .filter_map(|(name, _)| name.parse().ok())
            .collect()
    }
}
struct Kept {
    rev: Revision,
    recipes: RecipeGeneration,
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
    let rev = Revision(rev as u64);
    let recipes = crate::plan_cache::recipe_generation(home, project);
    let key = (home.to_owned(), project);
    if let Some(kept) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key)
        && kept.rev == rev
        && kept.recipes == recipes
        && kept.at.elapsed() < FRESH
    {
        return Ok(kept.names.clone());
    }
    // Titles read every step's declaration (its doc and prompt), once per revision.
    let rows = sluice_store::plans::read_steps(
        sql,
        project,
        &RowSelection::default(),
        StepProjection::Full,
    )?;
    // the project's own recipes before the global ones, each by name
    let usable = ordered_recipes(home, project);
    let ordered: Vec<&Recipe> = usable.iter().map(|(_, _, r)| r.as_ref()).collect();
    let naming = name_plan(&rows.steps, &ordered, &mut |path| prompt_head(path));
    let names = Arc::new(ProjectNaming {
        naming,
        recipes: usable
            .into_iter()
            .map(|(_, name, recipe)| (name, recipe))
            .collect(),
        rev: rows.rev,
        recipe_generation: recipes.clone(),
    });
    let mut held = cache().lock().unwrap_or_else(|e| e.into_inner());
    held.insert(
        key,
        Kept {
            rev: rows.rev,
            recipes,
            at: Instant::now(),
            names: names.clone(),
        },
    );
    Ok(names)
}

/// The project's recipes in matching order: its own before the home's, each by name, the ones
/// that check.
fn ordered_recipes(home: &Path, project: ProjectId) -> Vec<(bool, String, Arc<Recipe>)> {
    let entries = crate::dispatch_ext::load_recipes(home, project).unwrap_or_default();
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
    usable
}

/// The recipe each unit matches now, by unit name (`plan_read`'s and `unit_get`'s `recipe`, and
/// the `recipe` filter), inside the caller's read snapshot. Matching needs only each member's
/// id and fn (`Recipe::match_unit`: the recipe's step ids for the unit and their `run`s), so it
/// reads the compact step rows and never a declaration (plan-rows §7.3, §9). Kept per process
/// while the plan's revision and the recipe files' generation stay the same.
pub fn unit_recipes(
    sql: &Connection,
    home: &Path,
    project: ProjectId,
) -> sluice_store::Result<Arc<BTreeMap<String, String>>> {
    type Matches = Mutex<
        HashMap<(PathBuf, ProjectId), (Revision, RecipeGeneration, Arc<BTreeMap<String, String>>)>,
    >;
    static MATCHES: OnceLock<Matches> = OnceLock::new();
    let matches = MATCHES.get_or_init(Default::default);
    let rev = sluice_store::plans::plan_revision(sql, project)?;
    let generation = crate::plan_cache::recipe_generation(home, project);
    let key = (home.to_owned(), project);
    if let Some((kept_rev, kept_generation, units)) =
        matches.lock().unwrap_or_else(|e| e.into_inner()).get(&key)
        && *kept_rev == rev
        && *kept_generation == generation
    {
        return Ok(units.clone());
    }
    let rows = sluice_store::plans::read_steps(
        sql,
        project,
        &RowSelection::default(),
        StepProjection::Compact,
    )?;
    let mut members: indexmap::IndexMap<String, serde_json::Map<String, serde_json::Value>> =
        Default::default();
    for row in &rows.steps {
        members
            .entry(row.unit.to_string())
            .or_default()
            .insert(row.step.to_string(), serde_json::json!({ "run": row.run }));
    }
    let recipes = ordered_recipes(home, project);
    let units: BTreeMap<String, String> = members
        .iter()
        .filter_map(|(unit, steps)| {
            let (_, name, _) = recipes
                .iter()
                .find(|(_, _, recipe)| recipe.match_unit(unit, steps).is_some())?;
            Some((unit.clone(), name.clone()))
        })
        .collect();
    let units = Arc::new(units);
    let mut held = matches.lock().unwrap_or_else(|e| e.into_inner());
    if held.len() > 256 {
        held.clear();
    }
    held.insert(key, (rows.rev, generation, units.clone()));
    Ok(units)
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
