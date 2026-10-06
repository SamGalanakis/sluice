//! Immutable project identity, authored settings and guarded id-keyed deletion.

use crate::{
    Result, StoreError, WriteTransaction,
    artifacts::{self, Bundle, enqueue, invalid, now},
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use sluice_model::{
    commands::{IconUpload, ProjectIconSummary, ProjectSummary},
    error::PublicError,
    events::Event,
    ids::{InvocationId, ProjectId, ProjectName, ProjectSelector, Revision, RunId},
    rpc::{JsonMap, JsonValue},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::Path,
};

pub const ICON_MAX: usize = 256 * 1024;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    text: Option<String>,
    media_type: Option<String>,
    bytes: Vec<u8>,
}
impl Icon {
    /// Empty text clears the icon. Other text is stripped and bounded by characters.
    pub fn text(value: &str) -> Result<Self> {
        let value = value.trim();
        if value.chars().count() > 16 {
            return Err(invalid("text icon has at most 16 characters"));
        }
        if value.chars().any(char::is_control) {
            return Err(invalid("text icon contains control characters"));
        }
        Ok(Self {
            text: (!value.is_empty()).then(|| value.into()),
            media_type: None,
            bytes: Vec::new(),
        })
    }
    pub fn image(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() > ICON_MAX {
            return Err(invalid("icon is over 256 KB"));
        }
        let media_type = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            "image/jpeg"
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            "image/gif"
        } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
            "image/webp"
        } else if std::str::from_utf8(&bytes).is_ok_and(|s| {
            let s = s.trim_start_matches('\u{feff}').trim_start();
            s.starts_with("<svg") || s.starts_with("<?xml") && s.contains("<svg")
        }) {
            "image/svg+xml"
        } else {
            return Err(invalid("icon is not an SVG, PNG, WebP, JPEG or GIF"));
        };
        Ok(Self {
            text: None,
            media_type: Some(media_type.into()),
            bytes,
        })
    }
    /// Import once, with a bounded read through the opened file. Caller supplies
    /// the tilde root; store commands never consult or open a default live home.
    pub fn from_argument(value: &str, tilde_root: &Path) -> Result<Self> {
        let expanded;
        let path = if let Some(suffix) = value.strip_prefix("~/") {
            expanded = tilde_root.join(suffix);
            expanded.as_path()
        } else {
            Path::new(value)
        };
        match File::open(path) {
            Ok(file) => {
                if !file.metadata()?.is_file() {
                    return Err(invalid("icon has no readable file"));
                }
                let mut bytes = Vec::new();
                file.take(ICON_MAX as u64 + 1).read_to_end(&mut bytes)?;
                Self::image(bytes)
            }
            Err(_) if value.starts_with('/') || value.starts_with('~') => {
                Err(invalid("icon has no readable file"))
            }
            Err(_) => Self::text(value),
        }
    }
    /// A command's icon: text, image bytes, or an absolute (or `~/`, against
    /// `tilde_root`) path to an image file, read once here. Callers resolve it before
    /// their write transaction.
    pub fn from_upload(upload: IconUpload, tilde_root: Option<&Path>) -> Result<Self> {
        if let IconUpload::Text(value) = &upload {
            let value = value.trim();
            if value.starts_with('/') {
                return Self::from_argument(value, Path::new("/"));
            }
            if value.starts_with("~/") {
                let root =
                    tilde_root.ok_or_else(|| invalid("icon: no home directory to expand ~"))?;
                return Self::from_argument(value, root);
            }
        }
        Self::try_from(upload)
    }
    fn hash(&self) -> Option<String> {
        self.media_type
            .as_ref()
            .map(|_| artifacts::fingerprint(&self.bytes))
    }
}
impl From<Icon> for sluice_model::commands::IconUpload {
    fn from(icon: Icon) -> Self {
        use base64::Engine;
        match icon.media_type {
            Some(media_type) => Self::Image {
                media_type,
                bytes_base64: base64::engine::general_purpose::STANDARD.encode(icon.bytes),
            },
            None => Self::Text(icon.text.unwrap_or_default()),
        }
    }
}
impl TryFrom<sluice_model::commands::IconUpload> for Icon {
    type Error = StoreError;
    fn try_from(upload: sluice_model::commands::IconUpload) -> Result<Self> {
        use base64::Engine;
        match upload {
            sluice_model::commands::IconUpload::Text(text) => Self::text(&text),
            sluice_model::commands::IconUpload::Image {
                media_type,
                bytes_base64,
            } => {
                if bytes_base64.len() > ICON_MAX.div_ceil(3) * 4 {
                    return Err(invalid("icon is over 256 KB"));
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(bytes_base64)
                    .map_err(|_| invalid("icon has invalid base64"))?;
                let icon = Self::image(bytes)?;
                if icon.media_type.as_deref() != Some(media_type.as_str()) {
                    return Err(invalid("icon media type disagrees with its bytes"));
                }
                Ok(icon)
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectIcon {
    Text(String),
    Image {
        media_type: String,
        hash: String,
        generation: i64,
    },
}
impl ProjectIcon {
    pub fn url(&self, id: ProjectId) -> Option<String> {
        match self {
            Self::Image { generation, .. } => {
                Some(format!("/projects/id/{id}/icon?generation={generation}"))
            }
            Self::Text(_) => None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub project_id: ProjectId,
    pub name: ProjectName,
    pub description: String,
    pub icon: Option<ProjectIcon>,
    pub paused: bool,
    pub archived: bool,
    pub settings_rev: Revision,
    pub resources_rev: Revision,
    /// The board program (`docs("board")`), when one is set.
    pub board: Option<String>,
    /// 0 until a board is first set; one more per change.
    pub board_rev: Revision,
    /// Retire done units once their last step finished this many seconds ago (off when None).
    pub prune_done_after: Option<u64>,
    /// Unit-name patterns automatic retiring never removes.
    pub prune_keep: Vec<String>,
}
/// Resolve on admission; all subsequent state and callbacks carry the immutable id.
pub fn resolve(c: &Connection, selector: &ProjectSelector) -> Result<Project> {
    let (column, value) = match selector {
        ProjectSelector::Id(id) => ("project_id", id.to_string()),
        ProjectSelector::Name(name) => ("name", name.to_string()),
    };
    let row=c.query_row(&format!("SELECT project_id,name,description,icon_text,icon_type,icon_hash,icon_generation,paused,archived,settings_rev,resources_rev,board,board_rev,prune_done_after,prune_keep FROM projects WHERE {column}=?1 AND deleted_at IS NULL"),[value],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,i64>(6)?,r.get::<_,bool>(7)?,r.get::<_,bool>(8)?,r.get::<_,i64>(9)?,r.get::<_,i64>(10)?,r.get::<_,Option<String>>(11)?,r.get::<_,i64>(12)?,r.get::<_,Option<i64>>(13)?,r.get::<_,Option<String>>(14)?))).optional()?.ok_or_else(|| StoreError::from(PublicError::NotFound{message:format!("project {selector} not found")}))?;
    let icon = match (row.3, row.4, row.5) {
        (Some(t), None, None) => Some(ProjectIcon::Text(t)),
        (None, Some(media_type), Some(hash)) => Some(ProjectIcon::Image {
            media_type,
            hash,
            generation: row.6,
        }),
        (None, None, None) => None,
        _ => return Err(invalid("invalid persisted project icon")),
    };
    Ok(Project {
        project_id: row
            .0
            .parse()
            .map_err(|_| invalid("invalid persisted ProjectId"))?,
        name: row
            .1
            .parse()
            .map_err(|_| invalid("invalid persisted project name"))?,
        description: row.2,
        icon,
        paused: row.7,
        archived: row.8,
        settings_rev: Revision(
            row.9
                .try_into()
                .map_err(|_| invalid("invalid settings revision"))?,
        ),
        resources_rev: Revision(
            row.10
                .try_into()
                .map_err(|_| invalid("invalid resource revision"))?,
        ),
        board: row.11,
        board_rev: Revision(
            row.12
                .try_into()
                .map_err(|_| invalid("invalid board revision"))?,
        ),
        prune_done_after: row
            .13
            .map(u64::try_from)
            .transpose()
            .map_err(|_| invalid("invalid prune_done_after"))?,
        prune_keep: row
            .14
            .map(|keep| serde_json::from_str(&keep))
            .transpose()?
            .unwrap_or_default(),
    })
}

/// A project automatic retiring looks at (SPEC §6.11): live, neither paused nor archived,
/// with `prune_done_after` set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetireSetting {
    pub project: ProjectId,
    pub after: u64,
    pub keep: Vec<String>,
}
/// Every project automatic retiring looks at, in creation order.
pub fn retire_settings(c: &Connection) -> Result<Vec<RetireSetting>> {
    let rows: Vec<(String, i64, Option<String>)> = c
        .prepare(
            "SELECT project_id,prune_done_after,prune_keep FROM projects
             WHERE deleted_at IS NULL AND paused=0 AND archived=0 AND prune_done_after IS NOT NULL
             ORDER BY created_at,project_id",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    rows.into_iter()
        .map(|(id, after, keep)| {
            Ok(RetireSetting {
                project: id
                    .parse()
                    .map_err(|_| invalid("invalid persisted ProjectId"))?,
                after: u64::try_from(after).map_err(|_| invalid("invalid prune_done_after"))?,
                keep: keep
                    .map(|keep| serde_json::from_str(&keep))
                    .transpose()?
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// The longest `prune_done_after`: ten years.
pub const PRUNE_DONE_AFTER_MAX: u64 = 10 * 366 * 24 * 3600;

/// The project's latest automatic retirement: its plan edit's rev, time and step count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retirement {
    pub rev: Revision,
    pub at: String,
    pub steps: u64,
}
/// The newest plan edit automatic retiring made (author `sluice`), if any.
pub fn last_retirement(c: &Connection, id: ProjectId) -> Result<Option<Retirement>> {
    Ok(c.query_row(
        "SELECT rev,at,json_array_length(ops) FROM plan_edits
         WHERE project_id=?1 AND author=?2 AND reason LIKE 'retire done units%'
         ORDER BY rev DESC LIMIT 1",
        rusqlite::params![id.to_string(), RETIRE_AUTHOR],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        },
    )
    .optional()?
    .map(|(rev, at, steps)| Retirement {
        rev: Revision(rev.max(0) as u64),
        at,
        steps: steps.max(0) as u64,
    }))
}
/// The author automatic retiring's plan edits carry.
pub const RETIRE_AUTHOR: &str = "sluice";

/// Every live project, by name, as `projects_list` reports it.
pub fn list(c: &Connection) -> Result<Vec<ProjectSummary>> {
    let ids: Vec<String> = c
        .prepare("SELECT project_id FROM projects WHERE deleted_at IS NULL ORDER BY name")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let id: ProjectId = id
            .parse()
            .map_err(|_| invalid("invalid persisted ProjectId"))?;
        let project = resolve(c, &ProjectSelector::Id(id))?;
        let rev: i64 = c.query_row(
            "SELECT rev FROM plans WHERE project_id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        let counts = c
            .prepare("SELECT status,count(*) FROM steps WHERE project_id=?1 GROUP BY status")?
            .query_map([id.to_string()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut resources = JsonMap::default();
        for (name, resource) in crate::resources::declarations(c, id)? {
            let declaration = match resource.declaration {
                crate::resources::Capacity::Fixed(n) => json!({"capacity": n}),
                crate::resources::Capacity::Function(f) => json!({"capacity_fn": f}),
            };
            resources.0.insert(name, JsonValue::try_from(declaration)?);
        }
        out.push(ProjectSummary {
            project_id: id,
            name: project.name,
            description: project.description,
            rev: Revision(
                rev.try_into()
                    .map_err(|_| invalid("invalid plan revision"))?,
            ),
            settings_rev: project.settings_rev,
            counts,
            paused: project.paused,
            archived: project.archived,
            board_rev: project.board_rev,
            resources: (!resources.0.is_empty()).then_some(resources),
            icon: project.icon.map(|icon| match icon {
                ProjectIcon::Text(text) => ProjectIconSummary::Text { text },
                ProjectIcon::Image { media_type, .. } => ProjectIconSummary::Image { media_type },
            }),
        });
    }
    Ok(out)
}

/// P2.04 adapter: implement by calling its synchronous validated patch function.
/// Return whether declarations changed. This command records the authored change.
pub trait ResourceSettings {
    fn set_resources(
        &self,
        tx: &mut WriteTransaction<'_>,
        project: ProjectId,
        patch: &Value,
    ) -> Result<bool>;
}
/// Safe default while P2.04 is absent: no resource changes are silently accepted.
pub struct NoResourceSettings;
impl ResourceSettings for NoResourceSettings {
    fn set_resources(
        &self,
        _: &mut WriteTransaction<'_>,
        _: ProjectId,
        patch: &Value,
    ) -> Result<bool> {
        if patch.as_object().is_some_and(|p| p.is_empty()) {
            Ok(false)
        } else {
            Err(invalid("resource settings adapter is required"))
        }
    }
}
/// P2.02 adapter: initialize the validated empty plan, edit and authored record.
pub trait PlanInitializer {
    fn initialize(
        &self,
        tx: &mut WriteTransaction<'_>,
        project: ProjectId,
        author: &str,
    ) -> Result<()>;
}
/// Foundation requires creation here until the plans owner supplies its initializer.
pub struct EmptyPlanInitializer;
impl PlanInitializer for EmptyPlanInitializer {
    fn initialize(
        &self,
        tx: &mut WriteTransaction<'_>,
        project: ProjectId,
        author: &str,
    ) -> Result<()> {
        tx.sql().execute(
            "INSERT INTO plans(project_id,rev,doc) VALUES (?1,1,?2)",
            [project.to_string(), json!({"steps":{}}).to_string()],
        )?;
        let record = tx.append_record(
            Some(project),
            Event::PlanEdit {
                rev: Revision(1),
                author: author.into(),
                reason: "project created".into(),
                ops: Vec::new(),
            },
        )?;
        tx.sql().execute("INSERT INTO plan_edits(project_id,rev,seq,at,author,reason,ops) VALUES (?1,1,?2,?3,?4,'project created','[]')",rusqlite::params![project.to_string(),record.seq.0,record.at,author])?;
        tx.changed(Some(project), "plan");
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct CreateProject {
    pub name: ProjectName,
    pub description: String,
    pub icon: Option<Icon>,
    pub resources: Option<Value>,
    pub author: String,
}
pub fn project_create(
    tx: &mut WriteTransaction<'_>,
    request: CreateProject,
    plans: &dyn PlanInitializer,
    resources: &dyn ResourceSettings,
) -> Result<Project> {
    check_name(&request.name)?;
    let id = ProjectId::new();
    tx.sql().execute(
        "INSERT INTO projects(project_id,name,description,created_at) VALUES (?1,?2,?3,?4)",
        rusqlite::params![
            id.to_string(),
            request.name.to_string(),
            request.description,
            now()?
        ],
    )?;
    enqueue(
        tx,
        Some(id),
        None,
        "project_dir",
        1,
        format!("projects/{id}"),
        json!({"version":1}),
    )?;
    plans.initialize(tx, id, &request.author)?;
    if let Some(patch) = request.resources {
        resources.set_resources(tx, id, &patch)?;
    }
    if let Some(icon) = request.icon {
        set_icon(tx, id, icon)?;
    }
    changed(tx, id);
    resolve(tx.sql(), &ProjectSelector::Id(id))
}
/// A bare project id selects by id, so a name may not look like one.
fn check_name(name: &ProjectName) -> Result<()> {
    if name.looks_like_id() {
        return Err(invalid(format!(
            "project name {name} looks like a project id; choose a name that is not a UUID"
        )));
    }
    Ok(())
}
#[derive(Debug, Clone, Default)]
pub struct UpdateProject {
    pub new_name: Option<ProjectName>,
    pub description: Option<String>,
    pub icon: Option<Icon>,
    pub resources: Option<Value>,
    pub paused: Option<bool>,
    pub archived: Option<bool>,
    /// `Some(None)` turns automatic retiring off; `None` leaves it.
    pub prune_done_after: Option<Option<u64>>,
    /// The keep patterns; `Some(vec![])` clears them, `None` leaves them.
    pub prune_keep: Option<Vec<String>>,
    pub expected_settings_rev: Option<Revision>,
    pub reason: Option<String>,
    pub author: String,
}
fn check_revision(project: &Project, expected: Option<Revision>) -> Result<()> {
    if expected.is_some_and(|r| r != project.settings_rev) {
        return Err(PublicError::Conflict {
            message: "project settings changed".into(),
            current_rev: Some(project.settings_rev),
        }
        .into());
    }
    Ok(())
}
fn changed(tx: &mut WriteTransaction<'_>, id: ProjectId) {
    tx.changed(Some(id), "settings");
    tx.changed(Some(id), "status");
    tx.changed(None, "projects");
}
fn set_icon(tx: &mut WriteTransaction<'_>, id: ProjectId, icon: Icon) -> Result<bool> {
    let old = resolve(tx.sql(), &ProjectSelector::Id(id))?;
    let hash = icon.hash();
    let same = match &old.icon {
        Some(ProjectIcon::Text(t)) => icon.text.as_ref() == Some(t),
        Some(ProjectIcon::Image { hash: h, .. }) => hash.as_ref() == Some(h),
        None => icon.text.is_none() && hash.is_none(),
    };
    if same {
        return Ok(false);
    }
    let generation: i64 = tx.sql().query_row(
        "SELECT icon_generation+1 FROM projects WHERE project_id=?1",
        [id.to_string()],
        |r| r.get(0),
    )?;
    if icon.media_type.is_some() {
        let bundle = Bundle::new(BTreeMap::from([("image".into(), icon.bytes)]))?;
        let manifest = artifacts::manifest(bundle);
        enqueue(
            tx,
            Some(id),
            None,
            "icon",
            generation,
            format!("projects/{id}/icons/{generation}"),
            manifest,
        )?;
    }
    tx.sql().execute("UPDATE projects SET icon_generation=?2,icon_text=?3,icon_type=?4,icon_hash=?5 WHERE project_id=?1",rusqlite::params![id.to_string(),generation,icon.text,icon.media_type,hash])?;
    Ok(true)
}
/// Compose in one Writer::write closure. Any error must propagate out of it.
pub fn project_update(
    tx: &mut WriteTransaction<'_>,
    selector: &ProjectSelector,
    request: UpdateProject,
    resources: &dyn ResourceSettings,
) -> Result<Project> {
    let project = resolve(tx.sql(), selector)?;
    check_revision(&project, request.expected_settings_rev)?;
    let id = project.project_id;
    let rename = request.new_name.filter(|n| n != &project.name);
    let description = request.description.filter(|d| d != &project.description);
    let paused = request.paused.filter(|p| *p != project.paused);
    let archived = request.archived.filter(|a| *a != project.archived);
    // Name uniqueness is checked before adapters or other settings mutate.
    if let Some(name) = &rename {
        check_name(name)?;
        tx.sql().execute(
            "UPDATE projects SET name=?2,settings_rev=settings_rev+1,changed_at=?3 WHERE project_id=?1",
            [id.to_string(), name.to_string(), now()?],
        )?;
        tx.append_record(
            Some(id),
            Event::ProjectRename {
                old_name: project.name,
                new_name: name.clone(),
                author: request.author.clone(),
            },
        )?;
    }
    let mut fields = Vec::new();
    if let Some(value) = description {
        tx.sql().execute(
            "UPDATE projects SET description=?2 WHERE project_id=?1",
            [id.to_string(), value],
        )?;
        fields.push("description".into());
    }
    if let Some(patch) = request.resources
        && resources.set_resources(tx, id, &patch)?
    {
        tx.changed(Some(id), "resources");
        fields.push("resources".into());
    }
    if let Some(icon) = request.icon
        && set_icon(tx, id, icon)?
    {
        fields.push("icon".into());
    }
    if let Some(after) = request.prune_done_after {
        if after.is_some_and(|after| !(1..=PRUNE_DONE_AFTER_MAX).contains(&after)) {
            return Err(PublicError::Invalid {
                message: "invalid prune_done_after".into(),
                errors: vec![format!(
                    "prune_done_after: seconds from 1 to {PRUNE_DONE_AFTER_MAX}, or null to turn it off"
                )],
            }
            .into());
        }
        if after != project.prune_done_after {
            tx.sql().execute(
                "UPDATE projects SET prune_done_after=?2 WHERE project_id=?1",
                rusqlite::params![id.to_string(), after.map(|a| a as i64)],
            )?;
            fields.push("prune_done_after".into());
        }
    }
    if let Some(keep) = request.prune_keep {
        sluice_model::units::check_keep("prune_keep", &keep).map_err(|errors| {
            StoreError::from(PublicError::Invalid {
                message: "invalid prune_keep".into(),
                errors: errors.iter().map(ToString::to_string).collect(),
            })
        })?;
        if keep != project.prune_keep {
            let stored = (!keep.is_empty())
                .then(|| serde_json::to_string(&keep))
                .transpose()?;
            tx.sql().execute(
                "UPDATE projects SET prune_keep=?2 WHERE project_id=?1",
                rusqlite::params![id.to_string(), stored],
            )?;
            fields.push("prune_keep".into());
        }
    }
    for (column, value) in [("paused", paused), ("archived", archived)] {
        if let Some(value) = value {
            tx.sql().execute(
                &format!("UPDATE projects SET {column}=?2 WHERE project_id=?1"),
                rusqlite::params![id.to_string(), value],
            )?;
            let event = if column == "paused" {
                Event::ProjectPause {
                    paused: value,
                    reason: request.reason.clone(),
                    author: request.author.clone(),
                }
            } else {
                Event::ProjectArchive {
                    archived: value,
                    reason: request.reason.clone(),
                    author: request.author.clone(),
                }
            };
            tx.append_record(Some(id), event)?;
        }
    }
    let modified = rename.is_some() || !fields.is_empty() || paused.is_some() || archived.is_some();
    if !fields.is_empty() {
        tx.append_record(
            Some(id),
            Event::ProjectUpdate {
                fields,
                reason: request.reason,
                author: request.author,
            },
        )?;
    }
    if modified {
        if rename.is_none() {
            tx.sql().execute(
                "UPDATE projects SET settings_rev=settings_rev+1,changed_at=?2 WHERE project_id=?1",
                [id.to_string(), now()?],
            )?;
        }
        changed(tx, id);
    }
    resolve(tx.sql(), &ProjectSelector::Id(id))
}
#[derive(Debug, Clone, Default)]
pub struct SetBoard {
    /// The program; `None` clears the board.
    pub program: Option<String>,
    pub expected_rev: Option<Revision>,
    pub reason: Option<String>,
    pub author: String,
}
/// Set or clear a project's board in one transaction with its `project.board` record (which
/// leaves the program out). A stale `expected_rev` is a conflict; a program that does not
/// check against the board vocabulary is invalid, each problem with its line. Setting the
/// board it already has (or clearing none) changes nothing and returns the current rev.
pub fn board_set(
    tx: &mut WriteTransaction<'_>,
    selector: &ProjectSelector,
    request: SetBoard,
) -> Result<Revision> {
    let project = resolve(tx.sql(), selector)?;
    if request.expected_rev.is_some_and(|r| r != project.board_rev) {
        return Err(PublicError::Conflict {
            message: "the board changed".into(),
            current_rev: Some(project.board_rev),
        }
        .into());
    }
    if let Some(program) = &request.program
        && let Err(problems) = sluice_model::openui::check_board(program)
    {
        return Err(PublicError::Invalid {
            message: format!(
                "the board program has {} problem{}",
                problems.len(),
                if problems.len() == 1 { "" } else { "s" }
            ),
            errors: problems.iter().map(ToString::to_string).collect(),
        }
        .into());
    }
    if request.program == project.board {
        return Ok(project.board_rev);
    }
    let id = project.project_id;
    let rev = Revision(project.board_rev.0 + 1);
    tx.sql().execute(
        "UPDATE projects SET board=?2,board_rev=?3 WHERE project_id=?1",
        rusqlite::params![
            id.to_string(),
            request.program,
            i64::try_from(rev.0).map_err(|_| invalid("board revision overflow"))?
        ],
    )?;
    tx.append_record(
        Some(id),
        Event::ProjectBoard {
            rev,
            cleared: request.program.is_none(),
            reason: request.reason,
            author: request.author,
        },
    )?;
    tx.changed(Some(id), "board");
    tx.changed(Some(id), "status");
    tx.changed(None, "projects");
    Ok(rev)
}
/// The most bytes a project's slots take together, as stored (so the `board_slots` view, read
/// under the `query` tool's 1 MiB value limit, always reads).
pub const MAX_SLOTS_BYTES: usize = 256 * 1024;
#[derive(Debug, Clone, Default)]
pub struct SetBoardSlot {
    pub key: String,
    /// The slot's markdown; `None` or "" clears it.
    pub markdown: Option<String>,
    pub author: String,
}
/// One of a project's board slots: its markdown, when it last changed and who changed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardSlot {
    pub key: String,
    pub markdown: String,
    pub at: String,
    pub author: String,
}
/// What a `board_slot_set` did: the slot as it now is (`None` once cleared) and whether it
/// changed (the same markdown again, or clearing a slot that is not set, changes nothing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotChange {
    pub slot: Option<BoardSlot>,
    pub changed: bool,
}
/// A project's slots (`projects.board_slots`, a JSON object of key to `{markdown, at,
/// author}`), by key. A malformed entry is left out.
pub fn board_slots(c: &Connection, project: ProjectId) -> Result<Vec<BoardSlot>> {
    let raw: Option<String> = c
        .query_row(
            "SELECT board_slots FROM projects WHERE project_id=?1 AND deleted_at IS NULL",
            [project.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(parse_slots(raw.as_deref()))
}
fn parse_slots(raw: Option<&str>) -> Vec<BoardSlot> {
    let Some(Value::Object(map)) = raw.and_then(|r| serde_json::from_str::<Value>(r).ok()) else {
        return vec![];
    };
    let mut out: Vec<BoardSlot> = map
        .into_iter()
        .filter_map(|(key, v)| {
            Some(BoardSlot {
                markdown: v.get("markdown")?.as_str()?.to_owned(),
                at: v.get("at")?.as_str()?.to_owned(),
                author: v
                    .get("author")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                key,
            })
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}
/// Set or clear one board slot in one transaction with its record: a `project.update` whose
/// `fields` is `["board_slot:<key>"]` (reason "cleared" for a clear). A slot needs no
/// revision. The key must be `valid_slot_key`, the markdown at most `MAX_SLOT_BYTES`, a
/// project at most `MAX_BOARD_SLOTS` slots and `MAX_SLOTS_BYTES` in all; anything else is
/// `invalid`. The settings revision is left alone, so slot updates never fence a settings
/// save.
pub fn board_slot_set(
    tx: &mut WriteTransaction<'_>,
    selector: &ProjectSelector,
    request: SetBoardSlot,
) -> Result<SlotChange> {
    use sluice_model::openui::{MAX_BOARD_SLOTS, MAX_SLOT_BYTES, SLOT_KEY_RULE, valid_slot_key};
    let refuse = |message: String| -> StoreError {
        PublicError::Invalid {
            errors: vec![message.clone()],
            message,
        }
        .into()
    };
    if !valid_slot_key(&request.key) {
        return Err(refuse(format!(
            "key: {:?} is not a slot key ({SLOT_KEY_RULE})",
            request.key
        )));
    }
    let markdown = request.markdown.filter(|m| !m.is_empty());
    if let Some(text) = &markdown
        && text.len() > MAX_SLOT_BYTES
    {
        return Err(refuse(format!(
            "markdown: {} bytes is over the slot's {} KiB",
            text.len(),
            MAX_SLOT_BYTES / 1024
        )));
    }
    let project = resolve(tx.sql(), selector)?;
    let id = project.project_id;
    let raw: Option<String> = tx.sql().query_row(
        "SELECT board_slots FROM projects WHERE project_id=?1",
        [id.to_string()],
        |r| r.get(0),
    )?;
    let mut slots = match raw.as_deref().map(serde_json::from_str::<Value>) {
        Some(Ok(Value::Object(map))) => map,
        _ => serde_json::Map::new(),
    };
    let current = slots
        .get(&request.key)
        .and_then(|v| v.get("markdown"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if current == markdown {
        let slot = parse_slots(raw.as_deref())
            .into_iter()
            .find(|s| s.key == request.key);
        return Ok(SlotChange {
            slot,
            changed: false,
        });
    }
    let at = now()?;
    let slot = match markdown {
        Some(markdown) => {
            if current.is_none() && slots.len() >= MAX_BOARD_SLOTS {
                return Err(refuse(format!(
                    "the board has {MAX_BOARD_SLOTS} slots, its most: clear one first"
                )));
            }
            slots.insert(
                request.key.clone(),
                json!({"markdown": markdown, "at": at, "author": request.author}),
            );
            Some(BoardSlot {
                key: request.key.clone(),
                markdown,
                at,
                author: request.author.clone(),
            })
        }
        None => {
            slots.remove(&request.key);
            None
        }
    };
    let stored = (!slots.is_empty()).then(|| Value::Object(slots).to_string());
    if stored.as_ref().is_some_and(|s| s.len() > MAX_SLOTS_BYTES) {
        return Err(refuse(format!(
            "the board's slots would take over {} KiB together",
            MAX_SLOTS_BYTES / 1024
        )));
    }
    tx.sql().execute(
        "UPDATE projects SET board_slots=?2 WHERE project_id=?1",
        rusqlite::params![id.to_string(), stored],
    )?;
    tx.append_record(
        Some(id),
        Event::ProjectUpdate {
            fields: vec![format!("board_slot:{}", request.key)],
            reason: slot.is_none().then(|| "cleared".to_owned()),
            author: request.author,
        },
    )?;
    tx.changed(Some(id), "board");
    Ok(SlotChange {
        slot,
        changed: true,
    })
}
/// Coordinator can add live-process knowledge not yet reflected in attempt rows.
/// This check runs under the writer transaction and must not perform external I/O.
pub trait DeletionGuard {
    fn check(&self, project: ProjectId) -> Result<()>;
}
pub struct StoredWorkOnly;
impl DeletionGuard for StoredWorkOnly {
    fn check(&self, _: ProjectId) -> Result<()> {
        Ok(())
    }
}
/// A specific reason for the settings page's disabled delete action.
pub fn deletion_blocker(c: &Connection, selector: &ProjectSelector) -> Result<Option<String>> {
    let project = resolve(c, selector)?;
    if !project.archived {
        return Ok(Some("archive the project before deleting it".into()));
    }
    let id = project.project_id.to_string();
    for (sql, reason) in [
        (
            "SELECT EXISTS(SELECT 1 FROM steps WHERE project_id=?1 AND status='running')",
            "project has running steps",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM calls WHERE project_id=?1 AND status IN ('pending','running'))",
            "project has pending or running calls, including direct calls",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE project_id=?1 AND phase<>'terminal')",
            "project has active attempts",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM runs WHERE project_id=?1 AND finished_at IS NULL)",
            "project has a live run or guardian",
        ),
    ] {
        if c.query_row(sql, [&id], |r| r.get::<_, bool>(0))? {
            return Ok(Some(reason.into()));
        }
    }
    Ok(None)
}
#[derive(Debug, Clone)]
pub struct DeleteProject {
    pub confirm_name: String,
    pub expected_settings_rev: Revision,
    pub author: String,
}
#[derive(Debug, Clone)]
pub struct DeletedProject {
    pub project_id: ProjectId,
    pub name: ProjectName,
    pub cleanup_jobs: Vec<InvocationId>,
}
pub fn project_delete(
    tx: &mut WriteTransaction<'_>,
    selector: &ProjectSelector,
    request: DeleteProject,
    guard: &dyn DeletionGuard,
) -> Result<DeletedProject> {
    let project = resolve(tx.sql(), selector)?;
    check_revision(&project, Some(request.expected_settings_rev))?;
    if request.confirm_name != project.name.as_str() {
        return Err(invalid("confirmation must match the current project name"));
    }
    if let Some(reason) = deletion_blocker(tx.sql(), selector)? {
        return Err(invalid(reason));
    }
    let id = project.project_id;
    guard.check(id)?;
    let mut paths = BTreeSet::from([format!("projects/{id}")]);
    let runs = {
        let mut query = tx
            .sql()
            .prepare("SELECT run_id FROM runs WHERE project_id=?1")?;
        query
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for run in runs {
        let run: RunId = run
            .parse()
            .map_err(|_| invalid("invalid persisted run id"))?;
        paths.insert(format!("runs/{run}"));
    }
    let private_homes = {
        let mut query=tx.sql().prepare("SELECT DISTINCT json_extract(metadata,'$.private_home') FROM sessions WHERE project_id=?1 AND json_type(metadata,'$.private_home')='text'")?;
        query
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for path in private_homes {
        // External engine stores are engine-owned. Only explicit private_home
        // entries in our closed home grammar can be Sluice cleanup targets.
        if !path.starts_with("engine-homes/") {
            continue;
        }
        artifacts::validate_cleanup_path(&path)?;
        let shared:bool=tx.sql().query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE project_id IS NOT ?1 AND json_extract(metadata,'$.private_home')=?2)",rusqlite::params![id.to_string(),path],|r|r.get(0))?;
        if !shared {
            paths.insert(path);
        }
    }
    tx.sql().execute("UPDATE artifact_jobs SET state='failed',manifest='{}',error=?2,finished_at=?3 WHERE project_id=?1 AND kind<>'cleanup'",[id.to_string(),json!({"deleted":true}).to_string(),now()?])?;
    let mut cleanup_jobs = Vec::new();
    for path in paths {
        // Existing cleanup job is already durable and must not be duplicated.
        let existing: Option<String> = tx
            .sql()
            .query_row(
                "SELECT job_id FROM artifact_jobs WHERE kind='cleanup' AND path=?1",
                [&path],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            cleanup_jobs.push(
                existing
                    .parse()
                    .map_err(|_| invalid("invalid cleanup job id"))?,
            );
        } else {
            cleanup_jobs.push(enqueue(
                tx,
                Some(id),
                None,
                "cleanup",
                1,
                path,
                json!({"version":1}),
            )?);
        }
    }
    // Delete dependants before runs/resources. Tombstones preserve artifact FKs.
    for table in [
        "notification_attempts",
        "question_attachments",
        "message_deliveries",
        "readers",
        "messages",
        "leases",
        "submissions",
        "sessions",
        "calls",
        "runs",
        "attempts",
        "steps",
        "step_results",
        "inputs",
        "plan_edits",
        "plans",
        "resources",
        "records",
    ] {
        tx.sql().execute(
            &format!("DELETE FROM {table} WHERE project_id=?1"),
            [id.to_string()],
        )?;
    }
    tx.sql().execute("UPDATE maintenance SET paused_projects=(SELECT coalesce(json_group_array(value),'[]') FROM json_each(maintenance.paused_projects) WHERE value<>?1),revision=revision+1 WHERE EXISTS(SELECT 1 FROM json_each(maintenance.paused_projects) WHERE value=?1)",[id.to_string()])?;
    tx.sql().execute("UPDATE projects SET deleted_at=?2,changed_at=?2,settings_rev=settings_rev+1,icon_text=NULL,icon_type=NULL,icon_hash=NULL,description='',board=NULL,board_slots=NULL WHERE project_id=?1",[id.to_string(),now()?])?;
    tx.append_record(
        None,
        Event::ProjectDelete {
            project_id: id,
            name: project.name.clone(),
            author: request.author,
        },
    )?;
    for view in [
        "plan",
        "messages",
        "resources",
        "artifacts",
        "log",
        "questions",
        "edits",
        "outcomes",
        "readers",
    ] {
        tx.changed(Some(id), view);
    }
    tx.changed(None, "maintenance");
    changed(tx, id);
    Ok(DeletedProject {
        project_id: id,
        name: project.name,
        cleanup_jobs,
    })
}
/// Frozen image bytes are durable in the job even before filesystem recovery.
/// HTTP may serve these owned bytes without consulting a mutable path.
pub fn icon_image(
    c: &Connection,
    selector: &ProjectSelector,
    generation: i64,
) -> Result<(String, Vec<u8>, String)> {
    let project = resolve(c, selector)?;
    let Some(ProjectIcon::Image {
        media_type,
        hash,
        generation: current,
    }) = project.icon
    else {
        return Err(PublicError::NotFound {
            message: "project has no image icon".into(),
        }
        .into());
    };
    if generation != current {
        return Err(PublicError::NotFound {
            message: "icon generation is no longer current".into(),
        }
        .into());
    }
    let manifest:String=c.query_row("SELECT manifest FROM artifact_jobs WHERE project_id=?1 AND kind='icon' AND generation=?2 AND state<>'failed'",rusqlite::params![project.project_id.to_string(),generation],|r|r.get(0))?;
    let bundle = artifacts::decode_manifest(&serde_json::from_str(&manifest)?)?;
    let bytes = bundle
        .files()
        .get("image")
        .ok_or_else(|| invalid("icon job has no image"))?
        .clone();
    if artifacts::fingerprint(&bytes) != hash {
        return Err(invalid("icon hash mismatch"));
    }
    Ok((media_type, bytes, hash))
}
