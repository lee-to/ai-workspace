use anyhow::{Context as _, Result};
use log::{debug, error, info, warn};
use rusqlite::{Connection, Row, params, types::Type};
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::models::{
    ArtifactDependency, ArtifactDependencyKind, ArtifactReaction, EventArtifact,
    EventArtifactStatus, EventSeverity, EventStatus, EventTarget, EventTargetRelationKind,
    EventTargetStatus, FileSearchHit, Group, NoteEntry, Project, ServiceLink, ServiceLinkKind,
    ShareEntry, SharedItem, SharedItemKind, SyncReport, WorkspaceConfig, WorkspaceEvent,
    WorkspaceEventKind, normalize_project_slug,
};

pub enum ScopeChange {
    ToProject,
    ToGroup { group_id: i64 },
}

pub struct SharedItemUpdate {
    pub content: Option<String>,
    /// None = don't change, Some(None) = clear, Some(Some(x)) = set
    pub label: Option<Option<String>>,
    pub scope_change: Option<ScopeChange>,
}

pub struct Db {
    conn: Connection,
}

fn parse_row_enum<T>(row: &Row<'_>, column: usize) -> rusqlite::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let value: String = row.get(column)?;
    value.parse::<T>().map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                err.to_string(),
            )),
        )
    })
}

/// DB path: AI_WORKSPACE_DB env var, or ~/.ai-workspace/workspace.db
pub fn default_db_path() -> Result<std::path::PathBuf> {
    if let Ok(p) = std::env::var("AI_WORKSPACE_DB") {
        debug!("Using AI_WORKSPACE_DB={}", p);
        let path = std::path::PathBuf::from(p);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        return Ok(path);
    }
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let dir = home.join(".ai-workspace");
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir.join("workspace.db"))
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        info!("Opening database at {}", path.display());
        let conn = Connection::open(path)?;
        super::schema::init_db(&conn)?;
        Ok(Db { conn })
    }

    pub fn open_default() -> Result<Self> {
        let path = default_db_path()?;
        Self::open(&path)
    }

    // --- Projects ---

    #[allow(dead_code)]
    pub fn create_project(&self, name: &str, path: &str) -> Result<i64> {
        self.create_project_with_slug(name, path, None)
    }

    pub fn create_project_with_slug(
        &self,
        name: &str,
        path: &str,
        requested_slug: Option<&str>,
    ) -> Result<i64> {
        debug!(
            "Creating project: name={}, path={}, requested_slug={:?}",
            name, path, requested_slug
        );
        let slug = match requested_slug {
            Some(slug) => {
                let normalized = normalize_project_slug(slug);
                if normalized != slug {
                    warn!(
                        "Normalized requested project slug '{}' to '{}'",
                        slug, normalized
                    );
                }
                if self.get_project_by_slug(&normalized)?.is_some() {
                    anyhow::bail!("Project slug '{}' already exists", normalized);
                }
                normalized
            }
            None => self.generate_unique_project_slug(name, path)?,
        };
        debug!("Using project slug '{}' for name='{}'", slug, name);
        self.conn
            .execute(
                "INSERT INTO projects (name, slug, path) VALUES (?1, ?2, ?3)",
                params![name, slug, path],
            )
            .with_context(|| {
                format!("Failed to create project name='{name}' slug='{slug}' path='{path}'")
            })?;
        let id = self.conn.last_insert_rowid();
        info!("Created project id={} name='{}' slug='{}'", id, name, slug);
        Ok(id)
    }

    fn generate_unique_project_slug(&self, name: &str, path: &str) -> Result<String> {
        let source = if name.trim().is_empty() {
            Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "project".to_string())
        } else {
            name.to_string()
        };
        let base = normalize_project_slug(&source);
        debug!(
            "Generated base project slug '{}' from source '{}'",
            base, source
        );
        let mut candidate = base.clone();
        let mut suffix = 2;
        while self.get_project_by_slug(&candidate)?.is_some() {
            warn!(
                "Project slug '{}' already exists; trying fallback suffix {}",
                candidate, suffix
            );
            candidate = format!("{base}-{suffix}");
            suffix += 1;
        }
        Ok(candidate)
    }

    pub fn rename_project(&self, id: i64, new_name: &str) -> Result<()> {
        debug!("Renaming project id={} to '{}'", id, new_name);
        self.conn.execute(
            "UPDATE projects SET name = ?1 WHERE id = ?2",
            params![new_name, id],
        )?;
        info!("Renamed project id={} to '{}'", id, new_name);
        Ok(())
    }

    pub fn delete_project(&self, project_id: i64) -> Result<()> {
        info!("Deleting project {}", project_id);
        let tx = self.conn.unchecked_transaction()?;

        // Clean up FTS entries for project-scoped notes
        tx.execute(
            "DELETE FROM notes_fts WHERE rowid IN (SELECT id FROM shared_items WHERE project_id = ?1 AND kind = 'note')",
            params![project_id],
        )?;
        // Clean up FTS entries for group-scoped notes created by this project
        tx.execute(
            "DELETE FROM notes_fts WHERE rowid IN (SELECT id FROM shared_items WHERE created_by_project_id = ?1 AND kind = 'note')",
            params![project_id],
        )?;
        // created_by_project_id does not cascade in the schema, so remove those first.
        tx.execute(
            "DELETE FROM shared_items WHERE created_by_project_id = ?1",
            params![project_id],
        )?;
        // shared_items.project_id and project_groups cascade-delete via FK.
        tx.execute("DELETE FROM projects WHERE id = ?1", params![project_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_project_by_path(&self, path: &str) -> Result<Option<Project>> {
        debug!("Looking up project by path: {}", path);
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, slug, path, created_at FROM projects WHERE path = ?1")?;
        let mut rows = stmt.query_map(params![path], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                slug: row.get(2)?,
                path: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Find project whose path is a prefix of the given cwd
    pub fn find_project_by_cwd(&self, cwd: &str) -> Result<Option<Project>> {
        debug!("Finding project by cwd: {}", cwd);
        let mut stmt = self.conn.prepare(
            "SELECT id, name, slug, path, created_at FROM projects WHERE ?1 = path OR ?1 LIKE path || '/%' ORDER BY length(path) DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![cwd], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                slug: row.get(2)?,
                path: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn get_project_by_id(&self, id: i64) -> Result<Option<Project>> {
        debug!("Looking up project by id: {}", id);
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, slug, path, created_at FROM projects WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                slug: row.get(2)?,
                path: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn get_project_by_slug(&self, slug: &str) -> Result<Option<Project>> {
        debug!("Looking up project by slug: {}", slug);
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, slug, path, created_at FROM projects WHERE slug = ?1")?;
        let mut rows = stmt.query_map(params![slug], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                slug: row.get(2)?,
                path: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Resolve a project by numeric ID, slug, or exact registered path.
    pub fn resolve_project_target(&self, target: &str) -> Result<Option<Project>> {
        debug!("Resolving project target '{}'", target);
        if let Ok(id) = target.parse::<i64>()
            && let Some(project) = self.get_project_by_id(id)?
        {
            return Ok(Some(project));
        }

        if let Some(project) = self.get_project_by_path(target)? {
            return Ok(Some(project));
        }

        let slug = normalize_project_slug(target);
        if let Some(project) = self.get_project_by_slug(&slug)? {
            return Ok(Some(project));
        }

        Ok(None)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        debug!("Listing all projects");
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, slug, path, created_at FROM projects ORDER BY name")?;
        let rows = stmt.query_map([], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                slug: row.get(2)?,
                path: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // --- Groups ---

    pub fn get_group_by_name(&self, name: &str) -> Result<Option<Group>> {
        debug!("Looking up group: {}", name);
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, created_at FROM groups WHERE name = ?1")?;
        let mut rows = stmt.query_map(params![name], |row| {
            Ok(Group {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn get_or_create_group(&self, name: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT OR IGNORE INTO groups (name) VALUES (?1)",
            params![name],
        )?;
        let mut stmt = self.conn.prepare("SELECT id FROM groups WHERE name = ?1")?;
        let id: i64 = stmt.query_row(params![name], |row| row.get(0))?;
        debug!("get_or_create_group '{}' -> id={}", name, id);
        Ok(id)
    }

    pub fn list_groups(&self) -> Result<Vec<Group>> {
        debug!("Listing all groups");
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, created_at FROM groups ORDER BY name")?;
        let rows = stmt.query_map([], |row| {
            Ok(Group {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // --- Project ↔ Group ---

    pub fn add_project_to_group(&self, project_id: i64, group_id: i64) -> Result<()> {
        debug!("Adding project {} to group {}", project_id, group_id);
        self.conn.execute(
            "INSERT OR IGNORE INTO project_groups (project_id, group_id) VALUES (?1, ?2)",
            params![project_id, group_id],
        )?;
        Ok(())
    }

    pub fn get_groups_for_project(&self, project_id: i64) -> Result<Vec<Group>> {
        debug!("Getting groups for project {}", project_id);
        let mut stmt = self.conn.prepare(
            "SELECT g.id, g.name, g.created_at FROM groups g
             JOIN project_groups pg ON pg.group_id = g.id
             WHERE pg.project_id = ?1 ORDER BY g.name",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok(Group {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn get_projects_for_group(&self, group_id: i64) -> Result<Vec<Project>> {
        debug!("Getting projects for group {}", group_id);
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.name, p.slug, p.path, p.created_at FROM projects p
             JOIN project_groups pg ON pg.project_id = p.id
             WHERE pg.group_id = ?1 ORDER BY p.name",
        )?;
        let rows = stmt.query_map(params![group_id], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                slug: row.get(2)?,
                path: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Remove a project from a group
    pub fn remove_project_from_group(&self, project_id: i64, group_id: i64) -> Result<bool> {
        debug!("Removing project {} from group {}", project_id, group_id);
        let affected = self.conn.execute(
            "DELETE FROM project_groups WHERE project_id = ?1 AND group_id = ?2",
            params![project_id, group_id],
        )?;
        if affected > 0 {
            // Auto-delete group if no projects remain
            let remaining: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM project_groups WHERE group_id = ?1",
                params![group_id],
                |row| row.get(0),
            )?;
            if remaining == 0 {
                info!("Group {} has no members left, deleting", group_id);
                self.delete_group(group_id)?;
            }
        }
        Ok(affected > 0)
    }

    /// Delete a group and all its shared items
    pub fn delete_group(&self, group_id: i64) -> Result<()> {
        info!("Deleting group {}", group_id);
        // Delete group-scoped shared items
        self.conn.execute(
            "DELETE FROM shared_items WHERE group_id = ?1",
            params![group_id],
        )?;
        // project_groups rows cascade-deleted via FK
        self.conn
            .execute("DELETE FROM groups WHERE id = ?1", params![group_id])?;
        Ok(())
    }

    // --- Service Links ---

    #[allow(dead_code)]
    pub fn create_service_link(
        &self,
        from_target: &str,
        to_target: &str,
        kind: ServiceLinkKind,
        label: Option<&str>,
    ) -> Result<i64> {
        debug!(
            "Creating service link: from_target='{}', to_target='{}', kind={}, label={:?}",
            from_target, to_target, kind, label
        );
        let from_project = self.resolve_required_project_target(
            from_target,
            &format!(
                "creating service link {} -> {} ({})",
                from_target, to_target, kind
            ),
        )?;
        let to_project = self.resolve_required_project_target(
            to_target,
            &format!(
                "creating service link {} -> {} ({})",
                from_target, to_target, kind
            ),
        )?;

        if from_project.id == to_project.id {
            warn!(
                "Rejected self-link for project slug='{}' kind={}",
                from_project.slug, kind
            );
            anyhow::bail!(
                "Cannot create service link from '{}' to itself",
                from_project.slug
            );
        }

        if let Some(existing) =
            self.get_service_link_by_endpoints(from_project.id, to_project.id, kind)?
        {
            warn!(
                "Service link already exists: from_slug='{}', to_slug='{}', kind={}",
                from_project.slug, to_project.slug, kind
            );
            return Ok(existing.id);
        }

        self.conn
            .execute(
                "INSERT INTO service_links (from_project_id, to_project_id, kind, label)
                 VALUES (?1, ?2, ?3, ?4)",
                params![from_project.id, to_project.id, kind.as_str(), label],
            )
            .with_context(|| {
                format!(
                    "Failed to create service link from '{}' to '{}' kind={}",
                    from_project.slug, to_project.slug, kind
                )
            })?;
        let id = self.conn.last_insert_rowid();
        info!(
            "Created service link id={} from_slug='{}' to_slug='{}' kind={}",
            id, from_project.slug, to_project.slug, kind
        );
        Ok(id)
    }

    #[allow(dead_code)]
    pub fn delete_service_link(
        &self,
        from_target: &str,
        to_target: &str,
        kind: ServiceLinkKind,
    ) -> Result<bool> {
        debug!(
            "Deleting service link: from_target='{}', to_target='{}', kind={}",
            from_target, to_target, kind
        );
        let from_project = self.resolve_required_project_target(
            from_target,
            &format!(
                "deleting service link {} -> {} ({})",
                from_target, to_target, kind
            ),
        )?;
        let to_project = self.resolve_required_project_target(
            to_target,
            &format!(
                "deleting service link {} -> {} ({})",
                from_target, to_target, kind
            ),
        )?;

        let affected = self.conn.execute(
            "DELETE FROM service_links
             WHERE from_project_id = ?1 AND to_project_id = ?2 AND kind = ?3",
            params![from_project.id, to_project.id, kind.as_str()],
        )?;
        if affected > 0 {
            info!(
                "Deleted service link from_slug='{}' to_slug='{}' kind={}",
                from_project.slug, to_project.slug, kind
            );
        }
        Ok(affected > 0)
    }

    #[allow(dead_code)]
    pub fn list_outgoing_service_links(&self, project_id: i64) -> Result<Vec<ServiceLink>> {
        debug!(
            "Listing outgoing service links with SQL filter from_project_id={}",
            project_id
        );
        let mut stmt = self.conn.prepare(
            "SELECT id, from_project_id, to_project_id, kind, label, created_at, updated_at
             FROM service_links
             WHERE from_project_id = ?1
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![project_id], Self::map_service_link)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    #[allow(dead_code)]
    pub fn list_incoming_service_links(&self, project_id: i64) -> Result<Vec<ServiceLink>> {
        debug!(
            "Listing incoming service links with SQL filter to_project_id={}",
            project_id
        );
        let mut stmt = self.conn.prepare(
            "SELECT id, from_project_id, to_project_id, kind, label, created_at, updated_at
             FROM service_links
             WHERE to_project_id = ?1
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![project_id], Self::map_service_link)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    #[allow(dead_code)]
    pub fn list_group_service_links(&self, group_id: i64) -> Result<Vec<ServiceLink>> {
        debug!(
            "Listing group service graph with SQL filter group_id={}",
            group_id
        );
        let mut stmt = self.conn.prepare(
            "SELECT sl.id, sl.from_project_id, sl.to_project_id, sl.kind, sl.label,
                    sl.created_at, sl.updated_at
             FROM service_links sl
             WHERE sl.from_project_id IN (
                 SELECT project_id FROM project_groups WHERE group_id = ?1
             )
             AND sl.to_project_id IN (
                 SELECT project_id FROM project_groups WHERE group_id = ?1
             )
             ORDER BY sl.created_at, sl.id",
        )?;
        let rows = stmt.query_map(params![group_id], Self::map_service_link)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    #[allow(dead_code)]
    pub fn get_service_link_by_endpoints(
        &self,
        from_project_id: i64,
        to_project_id: i64,
        kind: ServiceLinkKind,
    ) -> Result<Option<ServiceLink>> {
        debug!(
            "Resolving service link with SQL filters from_project_id={}, to_project_id={}, kind={}",
            from_project_id, to_project_id, kind
        );
        let mut stmt = self.conn.prepare(
            "SELECT id, from_project_id, to_project_id, kind, label, created_at, updated_at
             FROM service_links
             WHERE from_project_id = ?1 AND to_project_id = ?2 AND kind = ?3",
        )?;
        let mut rows = stmt.query_map(
            params![from_project_id, to_project_id, kind.as_str()],
            Self::map_service_link,
        )?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    #[allow(dead_code)]
    fn resolve_required_project_target(&self, target: &str, context: &str) -> Result<Project> {
        debug!("Resolving project endpoint '{}' while {}", target, context);
        match self.resolve_project_target(target)? {
            Some(project) => Ok(project),
            None => {
                error!("Project endpoint '{}' not found while {}", target, context);
                anyhow::bail!("Project endpoint '{}' not found while {}", target, context)
            }
        }
    }

    #[allow(dead_code)]
    fn map_service_link(row: &Row<'_>) -> rusqlite::Result<ServiceLink> {
        let kind_text: String = row.get(3)?;
        let kind = kind_text.parse::<ServiceLinkKind>().map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    err.to_string(),
                )),
            )
        })?;
        Ok(ServiceLink {
            id: row.get(0)?,
            from_project_id: row.get(1)?,
            to_project_id: row.get(2)?,
            kind,
            label: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    }

    // --- Artifact Dependencies ---

    #[allow(dead_code)]
    pub fn add_artifact_dependency(
        &self,
        current_project_id: i64,
        item_target: &str,
        service_target: &str,
        kind: ArtifactDependencyKind,
        reaction: ArtifactReaction,
    ) -> Result<i64> {
        debug!(
            "Adding artifact dependency: current_project_id={}, item_target='{}', service_target='{}', kind={}, reaction={}",
            current_project_id, item_target, service_target, kind, reaction
        );
        let item = self
            .resolve_item_for_project(item_target, current_project_id)?
            .ok_or_else(|| {
                error!(
                    "Shared item target '{}' not found for project {}",
                    item_target, current_project_id
                );
                anyhow::anyhow!(
                    "Shared item target '{}' not found for project {}",
                    item_target,
                    current_project_id
                )
            })?;
        if item.kind == SharedItemKind::Note {
            error!(
                "Artifact dependency target '{}' resolved to note item id={}",
                item_target, item.id
            );
            anyhow::bail!(
                "Artifact dependency target '{}' must be a shared file or directory",
                item_target
            );
        }

        let target_project = self.resolve_required_project_target(
            service_target,
            &format!(
                "adding artifact dependency for item '{}' from project {}",
                item_target, current_project_id
            ),
        )?;
        if !self.projects_share_group(current_project_id, target_project.id)? {
            warn!(
                "Dependency target slug='{}' is not in a shared group with project_id={}",
                target_project.slug, current_project_id
            );
        }

        if let Some(existing) =
            self.get_artifact_dependency_by_item_slug_kind(item.id, &target_project.slug, kind)?
        {
            self.conn.execute(
                "UPDATE artifact_dependencies
                 SET depends_on_project_id = ?1, reaction = ?2, updated_at = datetime('now')
                 WHERE id = ?3",
                params![target_project.id, reaction.as_str(), existing.id],
            )?;
            info!(
                "Updated artifact dependency id={} item_id={} target_slug='{}' kind={} reaction={}",
                existing.id, item.id, target_project.slug, kind, reaction
            );
            return Ok(existing.id);
        }

        self.conn.execute(
            "INSERT INTO artifact_dependencies (
                 shared_item_id,
                 depends_on_project_id,
                 depends_on_project_slug_snapshot,
                 kind,
                 reaction
             )
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                item.id,
                target_project.id,
                target_project.slug,
                kind.as_str(),
                reaction.as_str()
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        info!(
            "Added artifact dependency id={} item_id={} path={:?} target_slug='{}' kind={} reaction={}",
            id, item.id, item.path, target_project.slug, kind, reaction
        );
        Ok(id)
    }

    #[allow(dead_code)]
    pub fn remove_artifact_dependency(
        &self,
        current_project_id: i64,
        item_target: &str,
        service_target: &str,
        kind: Option<ArtifactDependencyKind>,
    ) -> Result<usize> {
        debug!(
            "Removing artifact dependency: current_project_id={}, item_target='{}', service_target='{}', kind={:?}",
            current_project_id, item_target, service_target, kind
        );
        let item = self
            .resolve_item_for_project(item_target, current_project_id)?
            .ok_or_else(|| {
                error!(
                    "Shared item target '{}' not found for project {}",
                    item_target, current_project_id
                );
                anyhow::anyhow!(
                    "Shared item target '{}' not found for project {}",
                    item_target,
                    current_project_id
                )
            })?;
        let target_project = self.resolve_required_project_target(
            service_target,
            &format!(
                "removing artifact dependency for item '{}' from project {}",
                item_target, current_project_id
            ),
        )?;

        let affected = match kind {
            Some(kind) => self.conn.execute(
                "DELETE FROM artifact_dependencies
                 WHERE shared_item_id = ?1
                   AND depends_on_project_slug_snapshot = ?2
                   AND kind = ?3",
                params![item.id, target_project.slug, kind.as_str()],
            )?,
            None => self.conn.execute(
                "DELETE FROM artifact_dependencies
                 WHERE shared_item_id = ?1
                   AND depends_on_project_slug_snapshot = ?2",
                params![item.id, target_project.slug],
            )?,
        };
        info!(
            "Removed {} artifact dependencies for item_id={} path={:?} target_slug='{}' kind={:?}",
            affected, item.id, item.path, target_project.slug, kind
        );
        Ok(affected)
    }

    #[allow(dead_code)]
    pub fn list_artifact_dependencies_for_project(
        &self,
        project_id: i64,
    ) -> Result<Vec<ArtifactDependency>> {
        debug!(
            "Listing artifact dependencies with SQL filter shared item project_id={}",
            project_id
        );
        let mut stmt = self.conn.prepare(
            "SELECT ad.id, ad.shared_item_id, ad.depends_on_project_id,
                    ad.depends_on_project_slug_snapshot, ad.kind, ad.reaction,
                    ad.created_at, ad.updated_at
             FROM artifact_dependencies ad
             JOIN shared_items si ON si.id = ad.shared_item_id
             WHERE si.project_id = ?1
             ORDER BY ad.created_at, ad.id",
        )?;
        let rows = stmt.query_map(params![project_id], Self::map_artifact_dependency)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    #[allow(dead_code)]
    pub fn list_artifact_dependencies_for_item(
        &self,
        current_project_id: i64,
        item_target: &str,
    ) -> Result<Vec<ArtifactDependency>> {
        debug!(
            "Listing artifact dependencies for item_target='{}' project_id={}",
            item_target, current_project_id
        );
        let item = self
            .resolve_item_for_project(item_target, current_project_id)?
            .ok_or_else(|| {
                error!(
                    "Shared item target '{}' not found for project {}",
                    item_target, current_project_id
                );
                anyhow::anyhow!(
                    "Shared item target '{}' not found for project {}",
                    item_target,
                    current_project_id
                )
            })?;
        let mut stmt = self.conn.prepare(
            "SELECT id, shared_item_id, depends_on_project_id,
                    depends_on_project_slug_snapshot, kind, reaction,
                    created_at, updated_at
             FROM artifact_dependencies
             WHERE shared_item_id = ?1
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![item.id], Self::map_artifact_dependency)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    #[allow(dead_code)]
    pub fn list_artifact_dependencies_on_service_slug(
        &self,
        service_slug: &str,
    ) -> Result<Vec<ArtifactDependency>> {
        debug!(
            "Listing artifact dependencies with SQL filter depends_on_project_slug_snapshot='{}'",
            service_slug
        );
        let mut stmt = self.conn.prepare(
            "SELECT id, shared_item_id, depends_on_project_id,
                    depends_on_project_slug_snapshot, kind, reaction,
                    created_at, updated_at
             FROM artifact_dependencies
             WHERE depends_on_project_slug_snapshot = ?1
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![service_slug], Self::map_artifact_dependency)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    #[allow(dead_code)]
    fn get_artifact_dependency_by_item_slug_kind(
        &self,
        shared_item_id: i64,
        target_slug: &str,
        kind: ArtifactDependencyKind,
    ) -> Result<Option<ArtifactDependency>> {
        debug!(
            "Resolving artifact dependency with SQL filters shared_item_id={}, target_slug='{}', kind={}",
            shared_item_id, target_slug, kind
        );
        let mut stmt = self.conn.prepare(
            "SELECT id, shared_item_id, depends_on_project_id,
                    depends_on_project_slug_snapshot, kind, reaction,
                    created_at, updated_at
             FROM artifact_dependencies
             WHERE shared_item_id = ?1
               AND depends_on_project_slug_snapshot = ?2
               AND kind = ?3",
        )?;
        let mut rows = stmt.query_map(
            params![shared_item_id, target_slug, kind.as_str()],
            Self::map_artifact_dependency,
        )?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    #[allow(dead_code)]
    fn projects_share_group(&self, left_project_id: i64, right_project_id: i64) -> Result<bool> {
        if left_project_id == right_project_id {
            return Ok(true);
        }
        let shares_group: i64 = self.conn.query_row(
            "SELECT EXISTS (
                 SELECT 1
                 FROM project_groups left_pg
                 JOIN project_groups right_pg ON right_pg.group_id = left_pg.group_id
                 WHERE left_pg.project_id = ?1
                   AND right_pg.project_id = ?2
             )",
            params![left_project_id, right_project_id],
            |row| row.get(0),
        )?;
        Ok(shares_group != 0)
    }

    #[allow(dead_code)]
    fn map_artifact_dependency(row: &Row<'_>) -> rusqlite::Result<ArtifactDependency> {
        let kind_text: String = row.get(4)?;
        let kind = kind_text.parse::<ArtifactDependencyKind>().map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    err.to_string(),
                )),
            )
        })?;
        let reaction_text: String = row.get(5)?;
        let reaction = reaction_text.parse::<ArtifactReaction>().map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    err.to_string(),
                )),
            )
        })?;
        Ok(ArtifactDependency {
            id: row.get(0)?,
            shared_item_id: row.get(1)?,
            depends_on_project_id: row.get(2)?,
            depends_on_project_slug_snapshot: row.get(3)?,
            kind,
            reaction,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }

    // --- Workspace Events ---

    #[allow(dead_code)]
    pub fn create_workspace_event(
        &self,
        source_target: &str,
        kind: WorkspaceEventKind,
        severity: EventSeverity,
        title: &str,
        body: Option<&str>,
    ) -> Result<i64> {
        debug!(
            "Creating workspace event: source_target='{}', kind={}, severity={}",
            source_target, kind, severity
        );
        let source = self.resolve_required_project_target(
            source_target,
            &format!("creating workspace event kind={}", kind),
        )?;
        let group_id = self.first_group_id_for_project(source.id)?;
        if group_id.is_none() {
            warn!(
                "Event source slug='{}' has no group; impact may be limited",
                source.slug
            );
        }

        self.conn
            .execute(
                "INSERT INTO workspace_events (
                 source_project_id,
                 source_project_slug,
                 source_project_name,
                 group_id,
                 kind,
                 title,
                 body,
                 severity
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    source.id,
                    source.slug,
                    source.name,
                    group_id,
                    kind.as_str(),
                    title,
                    body,
                    severity.as_str()
                ],
            )
            .with_context(|| {
                format!(
                    "Failed to create workspace event kind={} source_slug='{}'",
                    kind, source.slug
                )
            })?;
        let event_id = self.conn.last_insert_rowid();
        let linked_count = self.insert_linked_service_event_targets(event_id, source.id)?;
        let artifact_count = self.insert_artifact_event_impacts(event_id, &source.slug)?;
        if linked_count == 0 && artifact_count == 0 {
            warn!(
                "Event id={} source_slug='{}' has no affected services or artifacts",
                event_id, source.slug
            );
        }
        info!(
            "Created workspace event id={} source_slug='{}' kind={} affected_services={} affected_artifacts={}",
            event_id, source.slug, kind, linked_count, artifact_count
        );
        Ok(event_id)
    }

    #[allow(dead_code)]
    pub fn close_workspace_event(&self, event_id: i64) -> Result<bool> {
        debug!("Closing workspace event id={}", event_id);
        let affected = self.conn.execute(
            "UPDATE workspace_events
             SET status = ?1, updated_at = datetime('now')
             WHERE id = ?2 AND status != ?1",
            params![EventStatus::Closed.as_str(), event_id],
        )?;
        if affected > 0 {
            self.conn.execute(
                "UPDATE event_targets
                 SET status = ?1, updated_at = datetime('now')
                 WHERE event_id = ?2",
                params![EventTargetStatus::Resolved.as_str(), event_id],
            )?;
            self.conn.execute(
                "UPDATE event_artifacts
                 SET status = ?1, updated_at = datetime('now')
                 WHERE event_id = ?2",
                params![EventArtifactStatus::Resolved.as_str(), event_id],
            )?;
            info!("Closed workspace event id={}", event_id);
        }
        Ok(affected > 0)
    }

    #[allow(dead_code)]
    pub fn remove_workspace_event(&self, event_id: i64) -> Result<bool> {
        debug!("Removing workspace event id={}", event_id);
        let affected = self.conn.execute(
            "DELETE FROM workspace_events WHERE id = ?1",
            params![event_id],
        )?;
        if affected > 0 {
            info!("Removed workspace event id={}", event_id);
        }
        Ok(affected > 0)
    }

    #[allow(dead_code)]
    pub fn get_workspace_event(&self, event_id: i64) -> Result<Option<WorkspaceEvent>> {
        debug!("Getting workspace event id={}", event_id);
        let mut stmt = self.conn.prepare(
            "SELECT id, source_project_id, source_project_slug, source_project_name,
                    group_id, kind, title, body, severity, status, created_at, updated_at
             FROM workspace_events
             WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![event_id], Self::map_workspace_event)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    #[allow(dead_code)]
    pub fn list_workspace_events(
        &self,
        source_slug: Option<&str>,
        status: Option<EventStatus>,
    ) -> Result<Vec<WorkspaceEvent>> {
        debug!(
            "Listing workspace events with source_slug={:?}, status={:?}",
            source_slug, status
        );
        let mut stmt = match (source_slug, status) {
            (Some(_), Some(_)) => self.conn.prepare(
                "SELECT id, source_project_id, source_project_slug, source_project_name,
                        group_id, kind, title, body, severity, status, created_at, updated_at
                 FROM workspace_events
                 WHERE source_project_slug = ?1 AND status = ?2
                 ORDER BY created_at DESC, id DESC",
            )?,
            (Some(_), None) => self.conn.prepare(
                "SELECT id, source_project_id, source_project_slug, source_project_name,
                        group_id, kind, title, body, severity, status, created_at, updated_at
                 FROM workspace_events
                 WHERE source_project_slug = ?1
                 ORDER BY created_at DESC, id DESC",
            )?,
            (None, Some(_)) => self.conn.prepare(
                "SELECT id, source_project_id, source_project_slug, source_project_name,
                        group_id, kind, title, body, severity, status, created_at, updated_at
                 FROM workspace_events
                 WHERE status = ?1
                 ORDER BY created_at DESC, id DESC",
            )?,
            (None, None) => self.conn.prepare(
                "SELECT id, source_project_id, source_project_slug, source_project_name,
                        group_id, kind, title, body, severity, status, created_at, updated_at
                 FROM workspace_events
                 ORDER BY created_at DESC, id DESC",
            )?,
        };
        let rows = match (source_slug, status) {
            (Some(source_slug), Some(status)) => stmt.query_map(
                params![source_slug, status.as_str()],
                Self::map_workspace_event,
            )?,
            (Some(source_slug), None) => {
                stmt.query_map(params![source_slug], Self::map_workspace_event)?
            }
            (None, Some(status)) => {
                stmt.query_map(params![status.as_str()], Self::map_workspace_event)?
            }
            (None, None) => stmt.query_map([], Self::map_workspace_event)?,
        };
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    #[allow(dead_code)]
    pub fn list_workspace_event_inbox(&self, project_id: i64) -> Result<Vec<WorkspaceEvent>> {
        debug!(
            "Listing workspace event inbox with SQL filter project_id={}",
            project_id
        );
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT we.id, we.source_project_id, we.source_project_slug,
                    we.source_project_name, we.group_id, we.kind, we.title, we.body,
                    we.severity, we.status, we.created_at, we.updated_at
             FROM workspace_events we
             LEFT JOIN event_targets et ON et.event_id = we.id
             LEFT JOIN event_artifacts ea ON ea.event_id = we.id
             WHERE we.status = 'open'
               AND (et.affected_project_id = ?1 OR ea.affected_project_id = ?1)
             ORDER BY we.created_at DESC, we.id DESC",
        )?;
        let rows = stmt.query_map(params![project_id], Self::map_workspace_event)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    #[allow(dead_code)]
    pub fn list_event_targets(&self, event_id: i64) -> Result<Vec<EventTarget>> {
        debug!("Listing event targets with event_id={}", event_id);
        let mut stmt = self.conn.prepare(
            "SELECT id, event_id, affected_project_id, relation_kind, status, created_at, updated_at
             FROM event_targets
             WHERE event_id = ?1
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![event_id], Self::map_event_target)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    #[allow(dead_code)]
    pub fn list_event_artifacts(&self, event_id: i64) -> Result<Vec<EventArtifact>> {
        debug!("Listing event artifacts with event_id={}", event_id);
        let mut stmt = self.conn.prepare(
            "SELECT id, event_id, affected_project_id, shared_item_id, path_snapshot,
                    reaction, reason, status, created_at, updated_at
             FROM event_artifacts
             WHERE event_id = ?1
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![event_id], Self::map_event_artifact)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    #[allow(dead_code)]
    fn first_group_id_for_project(&self, project_id: i64) -> Result<Option<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT group_id FROM project_groups WHERE project_id = ?1 ORDER BY group_id LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![project_id], |row| row.get(0))?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    #[allow(dead_code)]
    fn insert_linked_service_event_targets(&self, event_id: i64, source_id: i64) -> Result<usize> {
        debug!(
            "Calculating linked service impact for event_id={} source_project_id={}",
            event_id, source_id
        );
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT
                 CASE
                     WHEN from_project_id = ?1 THEN to_project_id
                     ELSE from_project_id
                 END AS affected_project_id
             FROM service_links
             WHERE from_project_id = ?1 OR to_project_id = ?1",
        )?;
        let rows = stmt.query_map(params![source_id], |row| row.get::<_, i64>(0))?;
        let mut count = 0;
        for affected_project_id in rows {
            let affected_project_id = affected_project_id?;
            self.conn.execute(
                "INSERT OR IGNORE INTO event_targets (
                     event_id,
                     affected_project_id,
                     relation_kind
                 )
                 VALUES (?1, ?2, ?3)",
                params![
                    event_id,
                    affected_project_id,
                    EventTargetRelationKind::LinkedService.as_str()
                ],
            )?;
            count += 1;
        }
        Ok(count)
    }

    #[allow(dead_code)]
    fn insert_artifact_event_impacts(&self, event_id: i64, source_slug: &str) -> Result<usize> {
        debug!(
            "Calculating artifact dependency impact for event_id={} source_slug='{}'",
            event_id, source_slug
        );
        let mut stmt = self.conn.prepare(
            "SELECT ad.shared_item_id, ad.reaction, si.project_id, si.path, ad.kind
             FROM artifact_dependencies ad
             JOIN shared_items si ON si.id = ad.shared_item_id
             WHERE ad.depends_on_project_slug_snapshot = ?1
               AND si.project_id IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![source_slug], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut count = 0;
        for row in rows {
            let (shared_item_id, reaction, affected_project_id, path, dep_kind) = row?;
            self.conn.execute(
                "INSERT OR IGNORE INTO event_targets (
                     event_id,
                     affected_project_id,
                     relation_kind
                 )
                 VALUES (?1, ?2, ?3)",
                params![
                    event_id,
                    affected_project_id,
                    EventTargetRelationKind::ArtifactDependency.as_str()
                ],
            )?;
            self.conn.execute(
                "INSERT INTO event_artifacts (
                     event_id,
                     affected_project_id,
                     shared_item_id,
                     path_snapshot,
                     reaction,
                     reason
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    event_id,
                    affected_project_id,
                    shared_item_id,
                    path,
                    reaction,
                    format!("{} depends on {}", dep_kind, source_slug)
                ],
            )?;
            count += 1;
        }
        Ok(count)
    }

    #[allow(dead_code)]
    fn map_workspace_event(row: &Row<'_>) -> rusqlite::Result<WorkspaceEvent> {
        Ok(WorkspaceEvent {
            id: row.get(0)?,
            source_project_id: row.get(1)?,
            source_project_slug: row.get(2)?,
            source_project_name: row.get(3)?,
            group_id: row.get(4)?,
            kind: parse_row_enum(row, 5)?,
            title: row.get(6)?,
            body: row.get(7)?,
            severity: parse_row_enum(row, 8)?,
            status: parse_row_enum(row, 9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        })
    }

    #[allow(dead_code)]
    fn map_event_target(row: &Row<'_>) -> rusqlite::Result<EventTarget> {
        Ok(EventTarget {
            id: row.get(0)?,
            event_id: row.get(1)?,
            affected_project_id: row.get(2)?,
            relation_kind: parse_row_enum(row, 3)?,
            status: parse_row_enum(row, 4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    }

    #[allow(dead_code)]
    fn map_event_artifact(row: &Row<'_>) -> rusqlite::Result<EventArtifact> {
        Ok(EventArtifact {
            id: row.get(0)?,
            event_id: row.get(1)?,
            affected_project_id: row.get(2)?,
            shared_item_id: row.get(3)?,
            path_snapshot: row.get(4)?,
            reaction: parse_row_enum(row, 5)?,
            reason: row.get(6)?,
            status: parse_row_enum(row, 7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }

    // --- Shared Items ---

    pub fn share_file(&self, project_id: i64, path: &str, label: Option<&str>) -> Result<i64> {
        debug!(
            "Sharing file: project_id={}, path={}, label={:?}",
            project_id, path, label
        );
        self.conn.execute(
            "INSERT INTO shared_items (kind, path, project_id, label) VALUES ('file', ?1, ?2, ?3)",
            params![path, project_id, label],
        )?;
        let id = self.conn.last_insert_rowid();
        info!("Shared file id={}, label={:?}", id, label);
        Ok(id)
    }

    pub fn share_dir(&self, project_id: i64, path: &str, label: Option<&str>) -> Result<i64> {
        debug!(
            "Sharing dir: project_id={}, path={}, label={:?}",
            project_id, path, label
        );
        self.conn.execute(
            "INSERT INTO shared_items (kind, path, project_id, label) VALUES ('dir', ?1, ?2, ?3)",
            params![path, project_id, label],
        )?;
        let id = self.conn.last_insert_rowid();
        info!("Shared dir id={}, label={:?}", id, label);
        Ok(id)
    }

    pub fn add_project_note(
        &self,
        project_id: i64,
        content: &str,
        label: Option<&str>,
    ) -> Result<i64> {
        debug!(
            "Adding project note: project_id={}, label={:?}",
            project_id, label
        );
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO shared_items (kind, content, project_id, label) VALUES ('note', ?1, ?2, ?3)",
            params![content, project_id, label],
        )?;
        let id = self.conn.last_insert_rowid();
        tx.execute(
            "INSERT INTO notes_fts (rowid, label, content) VALUES (?1, ?2, ?3)",
            params![id, label.unwrap_or(""), content],
        )?;
        tx.commit()?;
        info!("Added project note id={}, label={:?}", id, label);
        Ok(id)
    }

    pub fn add_group_note(
        &self,
        group_id: i64,
        created_by_project_id: i64,
        content: &str,
        label: Option<&str>,
    ) -> Result<i64> {
        debug!(
            "Adding group note: group_id={}, created_by={}, label={:?}",
            group_id, created_by_project_id, label
        );
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO shared_items (kind, content, group_id, created_by_project_id, label) VALUES ('note', ?1, ?2, ?3, ?4)",
            params![content, group_id, created_by_project_id, label],
        )?;
        let id = self.conn.last_insert_rowid();
        tx.execute(
            "INSERT INTO notes_fts (rowid, label, content) VALUES (?1, ?2, ?3)",
            params![id, label.unwrap_or(""), content],
        )?;
        tx.commit()?;
        info!("Added group note id={}, label={:?}", id, label);
        Ok(id)
    }

    pub fn get_item_by_id(&self, id: i64) -> Result<Option<SharedItem>> {
        debug!("Looking up shared item by id: {}", id);
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, path, content, label, project_id, group_id, created_by_project_id, created_at, updated_at
             FROM shared_items WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            let kind_str: String = row.get(1)?;
            Ok(SharedItem {
                id: row.get(0)?,
                kind: kind_str.parse().unwrap_or(SharedItemKind::File),
                path: row.get(2)?,
                content: row.get(3)?,
                label: row.get(4)?,
                project_id: row.get(5)?,
                group_id: row.get(6)?,
                created_by_project_id: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn remove_shared_item(&self, id: i64) -> Result<bool> {
        debug!("Removing shared item id={}", id);
        let affected = self
            .conn
            .execute("DELETE FROM shared_items WHERE id = ?1", params![id])?;
        if affected > 0 {
            // Clean up FTS (entry may not exist for non-note items)
            let _ = self
                .conn
                .execute("DELETE FROM notes_fts WHERE rowid = ?1", params![id]);
        }
        Ok(affected > 0)
    }

    pub fn remove_shared_item_for_project(&self, id: i64, project_id: i64) -> Result<bool> {
        debug!(
            "Removing shared item id={} for project_id={}",
            id, project_id
        );
        let affected = self.conn.execute(
            "DELETE FROM shared_items WHERE id = ?1 AND (project_id = ?2 OR created_by_project_id = ?2)",
            params![id, project_id],
        )?;
        if affected > 0 {
            let _ = self
                .conn
                .execute("DELETE FROM notes_fts WHERE rowid = ?1", params![id]);
        }
        Ok(affected > 0)
    }

    pub fn remove_by_label(&self, project_id: i64, label: &str) -> Result<bool> {
        debug!(
            "Removing by label: project_id={}, label={}",
            project_id, label
        );
        let id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM shared_items WHERE project_id = ?1 AND label = ?2 LIMIT 1",
                params![project_id, label],
                |row| row.get(0),
            )
            .ok();
        match id {
            Some(id) => self.remove_shared_item(id),
            None => Ok(false),
        }
    }

    pub fn remove_by_path(&self, project_id: i64, path: &str) -> Result<bool> {
        debug!("Removing by path: project_id={}, path={}", project_id, path);
        let id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM shared_items WHERE project_id = ?1 AND path = ?2 LIMIT 1",
                params![project_id, path],
                |row| row.get(0),
            )
            .ok();
        match id {
            Some(id) => self.remove_shared_item(id),
            None => Ok(false),
        }
    }

    pub fn get_shared_items_for_project(&self, project_id: i64) -> Result<Vec<SharedItem>> {
        debug!("Getting shared items for project {}", project_id);
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, path, content, label, project_id, group_id, created_by_project_id, created_at, updated_at
             FROM shared_items WHERE project_id = ?1 ORDER BY created_at",
        )?;
        self.map_shared_items(&mut stmt, params![project_id])
    }

    pub fn search_items(&self, query: &str) -> Result<Vec<SharedItem>> {
        debug!("Searching items: query={}", query);

        // 1) FTS search over notes (by label + content)
        let sanitized: String = query
            .split_whitespace()
            .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ");
        let mut fts_stmt = self.conn.prepare(
            "SELECT si.id, si.kind, si.path, si.content, si.label, si.project_id, si.group_id, si.created_by_project_id, si.created_at, si.updated_at
             FROM notes_fts fts
             JOIN shared_items si ON si.id = fts.rowid
             WHERE notes_fts MATCH ?1
             ORDER BY rank",
        )?;
        let mut results = self.map_shared_items(&mut fts_stmt, params![sanitized])?;
        let seen_ids: std::collections::HashSet<i64> = results.iter().map(|i| i.id).collect();

        // 2) LIKE search over files/dirs (by path + label)
        let like_pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let mut like_stmt = self.conn.prepare(
            "SELECT id, kind, path, content, label, project_id, group_id, created_by_project_id, created_at, updated_at
             FROM shared_items
             WHERE kind IN ('file', 'dir')
               AND (path LIKE ?1 ESCAPE '\\' OR label LIKE ?1 ESCAPE '\\')
             ORDER BY created_at",
        )?;
        let file_results = self.map_shared_items(&mut like_stmt, params![like_pattern])?;
        for item in file_results {
            if !seen_ids.contains(&item.id) {
                results.push(item);
            }
        }

        Ok(results)
    }

    /// Get all shared items visible in a group (files/dirs from member projects + group notes)
    pub fn get_all_items_for_group(&self, group_id: i64) -> Result<Vec<SharedItem>> {
        debug!("Getting all items for group {}", group_id);
        let mut stmt = self.conn.prepare(
            "SELECT si.id, si.kind, si.path, si.content, si.label, si.project_id, si.group_id, si.created_by_project_id, si.created_at, si.updated_at
             FROM shared_items si
             WHERE si.group_id = ?1
                OR (si.kind IN ('file', 'dir') AND si.project_id IN (
                    SELECT project_id FROM project_groups WHERE group_id = ?1
                ))
             ORDER BY si.created_at",
        )?;
        self.map_shared_items(&mut stmt, params![group_id])
    }

    fn map_shared_items(
        &self,
        stmt: &mut rusqlite::Statement<'_>,
        params: impl rusqlite::Params,
    ) -> Result<Vec<SharedItem>> {
        let rows = stmt.query_map(params, |row| {
            let kind_str: String = row.get(1)?;
            Ok(SharedItem {
                id: row.get(0)?,
                kind: kind_str.parse().unwrap_or_else(|_| {
                    warn!(
                        "Unknown shared item kind '{}', defaulting to File",
                        kind_str
                    );
                    SharedItemKind::File
                }),
                path: row.get(2)?,
                content: row.get(3)?,
                label: row.get(4)?,
                project_id: row.get(5)?,
                group_id: row.get(6)?,
                created_by_project_id: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Resolve a shared item by ID, label, or path (scoped to project)
    pub fn resolve_item_for_project(
        &self,
        target: &str,
        project_id: i64,
    ) -> Result<Option<SharedItem>> {
        debug!(
            "Resolving item: target={}, project_id={}",
            target, project_id
        );

        // 1. Try as numeric ID
        let numeric_item = if let Ok(id) = target.parse::<i64>() {
            self.get_item_by_id(id)?
        } else {
            None
        };
        if let Some(item) = numeric_item {
            let belongs_to_project = item.project_id == Some(project_id)
                || item.created_by_project_id == Some(project_id);
            if belongs_to_project {
                return Ok(Some(item));
            }
        }

        // 2. Try as label
        let item: Option<SharedItem> = self
            .conn
            .query_row(
                "SELECT id, kind, path, content, label, project_id, group_id, created_by_project_id, created_at, updated_at
                 FROM shared_items
                 WHERE (project_id = ?2 OR created_by_project_id = ?2) AND label = ?1
                 LIMIT 1",
                params![target, project_id],
                |row| {
                    let kind_str: String = row.get(1)?;
                    Ok(SharedItem {
                        id: row.get(0)?,
                        kind: kind_str.parse().unwrap_or(SharedItemKind::File),
                        path: row.get(2)?,
                        content: row.get(3)?,
                        label: row.get(4)?,
                        project_id: row.get(5)?,
                        group_id: row.get(6)?,
                        created_by_project_id: row.get(7)?,
                        created_at: row.get(8)?,
                        updated_at: row.get(9)?,
                    })
                },
            )
            .ok();
        if item.is_some() {
            return Ok(item);
        }

        // 3. Try as path
        let item: Option<SharedItem> = self
            .conn
            .query_row(
                "SELECT id, kind, path, content, label, project_id, group_id, created_by_project_id, created_at, updated_at
                 FROM shared_items
                 WHERE project_id = ?2 AND path = ?1
                 LIMIT 1",
                params![target, project_id],
                |row| {
                    let kind_str: String = row.get(1)?;
                    Ok(SharedItem {
                        id: row.get(0)?,
                        kind: kind_str.parse().unwrap_or(SharedItemKind::File),
                        path: row.get(2)?,
                        content: row.get(3)?,
                        label: row.get(4)?,
                        project_id: row.get(5)?,
                        group_id: row.get(6)?,
                        created_by_project_id: row.get(7)?,
                        created_at: row.get(8)?,
                        updated_at: row.get(9)?,
                    })
                },
            )
            .ok();

        Ok(item)
    }

    /// Update a shared item's content, label, and/or scope
    pub fn update_shared_item(
        &self,
        id: i64,
        project_id: i64,
        update: &SharedItemUpdate,
    ) -> Result<bool> {
        debug!("Updating shared item id={}", id);

        let tx = self.conn.unchecked_transaction()?;

        // Fetch existing item and verify ownership
        let item: SharedItem = tx
            .query_row(
                "SELECT id, kind, path, content, label, project_id, group_id, created_by_project_id, created_at, updated_at
                 FROM shared_items WHERE id = ?1 AND (project_id = ?2 OR created_by_project_id = ?2)",
                params![id, project_id],
                |row| {
                    let kind_str: String = row.get(1)?;
                    Ok(SharedItem {
                        id: row.get(0)?,
                        kind: kind_str.parse().unwrap_or(SharedItemKind::File),
                        path: row.get(2)?,
                        content: row.get(3)?,
                        label: row.get(4)?,
                        project_id: row.get(5)?,
                        group_id: row.get(6)?,
                        created_by_project_id: row.get(7)?,
                        created_at: row.get(8)?,
                        updated_at: row.get(9)?,
                    })
                },
            )
            .map_err(|_| anyhow::anyhow!("Item not found or not owned by this project"))?;

        // Compute final state
        let final_content = match &update.content {
            Some(c) => Some(c.as_str()),
            None => item.content.as_deref(),
        };
        let final_label: Option<&str> = match &update.label {
            Some(opt) => opt.as_deref(),
            None => item.label.as_deref(),
        };
        let (final_project_id, final_group_id, final_created_by) = match &update.scope_change {
            Some(ScopeChange::ToProject) => (Some(project_id), None, None),
            Some(ScopeChange::ToGroup { group_id }) => (None, Some(*group_id), Some(project_id)),
            None => (item.project_id, item.group_id, item.created_by_project_id),
        };

        tx.execute(
            "UPDATE shared_items
             SET content = ?1, label = ?2, project_id = ?3, group_id = ?4,
                 created_by_project_id = ?5, updated_at = datetime('now')
             WHERE id = ?6",
            params![
                final_content,
                final_label,
                final_project_id,
                final_group_id,
                final_created_by,
                id
            ],
        )?;

        // Update FTS index for notes
        if item.kind == SharedItemKind::Note {
            let _ = tx.execute("DELETE FROM notes_fts WHERE rowid = ?1", params![id]);
            tx.execute(
                "INSERT INTO notes_fts (rowid, label, content) VALUES (?1, ?2, ?3)",
                params![id, final_label.unwrap_or(""), final_content.unwrap_or("")],
            )?;
        }

        tx.commit()?;
        info!("Updated shared item id={}", id);
        Ok(true)
    }

    // --- Workspace Config ---

    /// Build a WorkspaceConfig from the current DB state for a project.
    /// Includes project name, groups, project-scoped shares, and project-scoped notes.
    /// Excludes group notes.
    pub fn export_project_config(&self, project_id: i64) -> Result<WorkspaceConfig> {
        info!("Exporting project config for project_id={}", project_id);
        let project = self
            .get_project_by_id(project_id)?
            .ok_or_else(|| anyhow::anyhow!("Project {} not found", project_id))?;

        let groups = self.get_groups_for_project(project_id)?;
        let group_names: Vec<String> = groups.iter().map(|g| g.name.clone()).collect();
        debug!("Export: {} groups", group_names.len());

        let items = self.get_shared_items_for_project(project_id)?;

        let mut shares = Vec::new();
        let mut notes = Vec::new();

        for item in &items {
            match item.kind {
                SharedItemKind::File | SharedItemKind::Dir => {
                    if let Some(ref path) = item.path {
                        let entry = match &item.label {
                            Some(label) => ShareEntry::WithLabel {
                                path: path.clone(),
                                label: label.clone(),
                            },
                            None => ShareEntry::PathOnly(path.clone()),
                        };
                        shares.push(entry);
                    }
                }
                SharedItemKind::Note => {
                    // Only project-scoped notes (project_id is set, group_id is None)
                    let project_note_content =
                        if item.project_id.is_some() && item.group_id.is_none() {
                            item.content.as_ref()
                        } else {
                            None
                        };
                    if let Some(content) = project_note_content {
                        notes.push(NoteEntry {
                            content: content.clone(),
                            label: item.label.clone(),
                        });
                    }
                }
            }
        }
        debug!("Export: {} shares, {} notes", shares.len(), notes.len());

        Ok(WorkspaceConfig {
            name: project.name,
            groups: group_names,
            share: shares,
            notes,
        })
    }

    /// Declarative reconciliation: make DB match the config.
    /// Adds missing groups/shares/notes, removes extras, updates changed notes.
    /// Runs in a single transaction.
    pub fn sync_from_config(
        &self,
        project_id: i64,
        config: &WorkspaceConfig,
    ) -> Result<SyncReport> {
        info!(
            "Syncing project {} from config '{}'",
            project_id, config.name
        );
        let mut report = SyncReport::default();

        let tx = self.conn.unchecked_transaction()?;

        // --- Groups ---
        let current_groups = {
            let mut stmt = tx.prepare(
                "SELECT g.name FROM groups g
                 JOIN project_groups pg ON pg.group_id = g.id
                 WHERE pg.project_id = ?1",
            )?;
            let rows = stmt.query_map(params![project_id], |row| row.get::<_, String>(0))?;
            rows.collect::<std::result::Result<std::collections::HashSet<String>, _>>()?
        };

        let desired_groups: std::collections::HashSet<String> =
            config.groups.iter().cloned().collect();

        // Add missing groups
        for name in desired_groups.difference(&current_groups) {
            debug!("sync: adding group '{}'", name);
            tx.execute(
                "INSERT OR IGNORE INTO groups (name) VALUES (?1)",
                params![name],
            )?;
            let gid: i64 = tx.query_row(
                "SELECT id FROM groups WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO project_groups (project_id, group_id) VALUES (?1, ?2)",
                params![project_id, gid],
            )?;
            report.groups_added += 1;
        }

        // Remove extra groups (only the project's membership, not the group itself)
        for name in current_groups.difference(&desired_groups) {
            debug!("sync: removing project from group '{}'", name);
            let gid: Option<i64> = tx
                .query_row(
                    "SELECT id FROM groups WHERE name = ?1",
                    params![name],
                    |row| row.get(0),
                )
                .ok();
            if let Some(gid) = gid {
                tx.execute(
                    "DELETE FROM project_groups WHERE project_id = ?1 AND group_id = ?2",
                    params![project_id, gid],
                )?;
                // Auto-delete group if no members remain
                let remaining: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM project_groups WHERE group_id = ?1",
                    params![gid],
                    |row| row.get(0),
                )?;
                if remaining == 0 {
                    debug!("sync: group '{}' has no members, deleting", name);
                    tx.execute("DELETE FROM shared_items WHERE group_id = ?1", params![gid])?;
                    tx.execute("DELETE FROM groups WHERE id = ?1", params![gid])?;
                }
                report.groups_removed += 1;
            }
        }

        // --- Shares (files/dirs) ---
        let current_shares: Vec<(i64, String, Option<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT id, path, label FROM shared_items
                 WHERE project_id = ?1 AND kind IN ('file', 'dir')",
            )?;
            let rows = stmt.query_map(params![project_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let current_share_paths: std::collections::HashSet<String> =
            current_shares.iter().map(|(_, p, _)| p.clone()).collect();
        let desired_share_paths: std::collections::HashSet<String> =
            config.share.iter().map(|s| s.path().to_string()).collect();

        // Add missing shares (INSERT OR IGNORE to handle UNIQUE constraint)
        for entry in &config.share {
            if !current_share_paths.contains(entry.path()) {
                debug!("sync: adding share '{}'", entry.path());
                // Determine kind by checking if path looks like a dir (ends with /)
                // or fall back to 'file'. The actual kind is determined at share time,
                // but in config we store what was shared, so we re-insert as file.
                // The share command handles file vs dir detection.
                tx.execute(
                    "INSERT OR IGNORE INTO shared_items (kind, path, project_id, label) VALUES ('file', ?1, ?2, ?3)",
                    params![entry.path(), project_id, entry.label()],
                )?;
                if tx.changes() > 0 {
                    report.shares_added += 1;
                }
            }
        }

        // Remove shares not in config
        for (id, path, _label) in &current_shares {
            if !desired_share_paths.contains(path) {
                debug!("sync: removing share '{}'", path);
                tx.execute("DELETE FROM shared_items WHERE id = ?1", params![id])?;
                report.shares_removed += 1;
            }
        }

        // --- Notes (project-scoped only) ---
        let current_notes: Vec<(i64, String, Option<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT id, content, label FROM shared_items
                 WHERE project_id = ?1 AND kind = 'note' AND group_id IS NULL",
            )?;
            let rows = stmt.query_map(params![project_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };

        // Match notes by label (labeled notes) or by content (unlabeled notes)
        let mut matched_db_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();

        for config_note in &config.notes {
            if let Some(ref label) = config_note.label {
                // Labeled note: match by label
                let existing = current_notes
                    .iter()
                    .find(|(_, _, l)| l.as_deref() == Some(label));
                if let Some((id, content, _)) = existing {
                    matched_db_ids.insert(*id);
                    // Update content if changed
                    if *content != config_note.content {
                        debug!("sync: updating note label='{}' content", label);
                        tx.execute(
                            "UPDATE shared_items SET content = ?1, updated_at = datetime('now') WHERE id = ?2",
                            params![config_note.content, id],
                        )?;
                        let _ = tx.execute("DELETE FROM notes_fts WHERE rowid = ?1", params![id]);
                        tx.execute(
                            "INSERT INTO notes_fts (rowid, label, content) VALUES (?1, ?2, ?3)",
                            params![id, label, config_note.content],
                        )?;
                        report.notes_updated += 1;
                    }
                } else {
                    // Add new labeled note
                    debug!("sync: adding note label='{}'", label);
                    tx.execute(
                        "INSERT INTO shared_items (kind, content, project_id, label) VALUES ('note', ?1, ?2, ?3)",
                        params![config_note.content, project_id, label],
                    )?;
                    let new_id = tx.last_insert_rowid();
                    tx.execute(
                        "INSERT INTO notes_fts (rowid, label, content) VALUES (?1, ?2, ?3)",
                        params![new_id, label, config_note.content],
                    )?;
                    report.notes_added += 1;
                }
            } else {
                // Unlabeled note: match by content
                let existing = current_notes.iter().find(|(id, c, l)| {
                    l.is_none() && *c == config_note.content && !matched_db_ids.contains(id)
                });
                if let Some((id, _, _)) = existing {
                    matched_db_ids.insert(*id);
                    // Content matches, nothing to update
                } else {
                    // Add new unlabeled note
                    debug!("sync: adding unlabeled note");
                    tx.execute(
                        "INSERT INTO shared_items (kind, content, project_id) VALUES ('note', ?1, ?2)",
                        params![config_note.content, project_id],
                    )?;
                    let new_id = tx.last_insert_rowid();
                    tx.execute(
                        "INSERT INTO notes_fts (rowid, label, content) VALUES (?1, ?2, ?3)",
                        params![new_id, "", config_note.content],
                    )?;
                    report.notes_added += 1;
                }
            }
        }

        // Remove DB notes not matched (unlabeled DB notes not in config get removed)
        for (id, _content, label) in &current_notes {
            if !matched_db_ids.contains(id) {
                debug!("sync: removing note id={} label={:?}", id, label);
                tx.execute("DELETE FROM shared_items WHERE id = ?1", params![id])?;
                let _ = tx.execute("DELETE FROM notes_fts WHERE rowid = ?1", params![id]);
                report.notes_removed += 1;
            }
        }

        tx.commit()?;
        info!(
            "Sync complete: groups +{} -{}, shares +{} -{}, notes +{} -{} ~{}",
            report.groups_added,
            report.groups_removed,
            report.shares_added,
            report.shares_removed,
            report.notes_added,
            report.notes_removed,
            report.notes_updated,
        );
        Ok(report)
    }

    /// Remove shared file/dir entries whose paths no longer exist on disk
    pub fn sync_files(&self) -> Result<Vec<(i64, String)>> {
        info!("Syncing shared files with disk");
        let mut stmt = self.conn.prepare(
            "SELECT si.id, si.path, p.path as project_path, si.kind
             FROM shared_items si
             JOIN projects p ON p.id = si.project_id
             WHERE si.kind IN ('file', 'dir')",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;

        let mut removed = Vec::new();
        for row in rows {
            let (id, rel_path, project_path, _kind) = row?;
            let full_path = Path::new(&project_path).join(&rel_path);
            if !full_path.exists() {
                debug!("Path not found, removing: {}", full_path.display());
                self.remove_shared_item(id)?;
                removed.push((id, rel_path));
            }
        }
        info!("Sync complete: {} stale entries removed", removed.len());
        Ok(removed)
    }

    // --- Files FTS index ---

    /// UPSERT a file into the FTS index and update its meta row.
    /// `rel_path` is the project-relative path (used as the indexed `path` column).
    /// `abs_path` is the resolved absolute path (persisted in meta for stat-based refresh).
    pub fn index_file(
        &self,
        shared_item_id: i64,
        rel_path: &str,
        abs_path: &str,
        content: &str,
        mtime: i64,
        size: i64,
    ) -> Result<()> {
        debug!(
            "index_file: id={} path={} size={} mtime={}",
            shared_item_id, rel_path, size, mtime
        );
        self.conn.execute(
            "DELETE FROM files_fts WHERE rowid = ?1",
            params![shared_item_id],
        )?;
        self.conn.execute(
            "INSERT INTO files_fts (rowid, path, content) VALUES (?1, ?2, ?3)",
            params![shared_item_id, rel_path, content],
        )?;
        self.conn.execute(
            "INSERT INTO files_fts_meta (shared_item_id, abs_path, mtime, size, indexed_at) \
             VALUES (?1, ?2, ?3, ?4, datetime('now')) \
             ON CONFLICT(shared_item_id) DO UPDATE SET \
                abs_path = excluded.abs_path, \
                mtime = excluded.mtime, \
                size = excluded.size, \
                indexed_at = excluded.indexed_at",
            params![shared_item_id, abs_path, mtime, size],
        )?;
        Ok(())
    }

    /// Remove a file from the FTS index (and its meta row).
    pub fn delete_file_index(&self, shared_item_id: i64) -> Result<()> {
        debug!("delete_file_index: id={}", shared_item_id);
        self.conn.execute(
            "DELETE FROM files_fts WHERE rowid = ?1",
            params![shared_item_id],
        )?;
        self.conn.execute(
            "DELETE FROM files_fts_meta WHERE shared_item_id = ?1",
            params![shared_item_id],
        )?;
        Ok(())
    }

    /// Get (abs_path, mtime, size) of an indexed file, if present.
    #[allow(dead_code)]
    pub fn get_file_index_meta(&self, shared_item_id: i64) -> Result<Option<(String, i64, i64)>> {
        let row = self
            .conn
            .query_row(
                "SELECT abs_path, mtime, size FROM files_fts_meta WHERE shared_item_id = ?1",
                params![shared_item_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .ok();
        Ok(row)
    }

    /// List all indexed files (for reindex / refresh passes).
    /// Returns (shared_item_id, abs_path, mtime, size).
    pub fn list_file_index_meta(&self) -> Result<Vec<(i64, String, i64, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT shared_item_id, abs_path, mtime, size FROM files_fts_meta")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let out = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(out)
    }

    /// Full-text search over indexed files. Returns hits ordered by bm25 (best first).
    pub fn search_files(&self, query: &str, limit: usize) -> Result<Vec<FileSearchHit>> {
        debug!("search_files: query='{}' limit={}", query, limit);
        let mut stmt = self.conn.prepare(
            "SELECT f.rowid, \
                    s.project_id, \
                    f.path, \
                    snippet(files_fts, 1, '[', ']', '…', 12), \
                    bm25(files_fts) \
             FROM files_fts f \
             JOIN shared_items s ON s.id = f.rowid \
             WHERE files_fts MATCH ?1 \
             ORDER BY bm25(files_fts) \
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![query, limit as i64], |r| {
            Ok(FileSearchHit {
                shared_item_id: r.get(0)?,
                project_id: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                path: r.get(2)?,
                snippet: r.get(3)?,
                rank: r.get(4)?,
            })
        })?;
        let hits: Vec<FileSearchHit> = rows
            .filter_map(|r| match r {
                Ok(h) => Some(h),
                Err(e) => {
                    warn!("search_files row error: {}", e);
                    None
                }
            })
            .collect();
        debug!("search_files: {} hits", hits.len());
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn test_db() -> Db {
        let tmp = NamedTempFile::new().unwrap();
        Db::open(tmp.path()).unwrap()
    }

    // --- Projects ---

    #[test]
    fn create_and_get_project() {
        let db = test_db();
        let id = db.create_project("proj", "/tmp/proj").unwrap();
        assert!(id > 0);

        let p = db.get_project_by_path("/tmp/proj").unwrap().unwrap();
        assert_eq!(p.name, "proj");
        assert_eq!(p.slug, "proj");
        assert_eq!(p.path, "/tmp/proj");
        assert_eq!(p.id, id);
    }

    #[test]
    fn get_project_by_path_not_found() {
        let db = test_db();
        assert!(db.get_project_by_path("/nonexistent").unwrap().is_none());
    }

    #[test]
    fn get_project_by_id() {
        let db = test_db();
        let id = db.create_project("proj", "/tmp/proj").unwrap();
        let p = db.get_project_by_id(id).unwrap().unwrap();
        assert_eq!(p.name, "proj");
        assert_eq!(p.slug, "proj");
        assert!(db.get_project_by_id(9999).unwrap().is_none());
    }

    #[test]
    fn create_project_with_explicit_slug() {
        let db = test_db();
        let id = db
            .create_project_with_slug("Auth Service", "/tmp/auth", Some("auth-api"))
            .unwrap();
        let p = db.get_project_by_id(id).unwrap().unwrap();
        assert_eq!(p.slug, "auth-api");
    }

    #[test]
    fn create_project_generates_unique_slug() {
        let db = test_db();
        db.create_project("Service", "/tmp/service-a").unwrap();
        let id = db.create_project("Service", "/tmp/service-b").unwrap();
        let p = db.get_project_by_id(id).unwrap().unwrap();
        assert_eq!(p.slug, "service-2");
    }

    #[test]
    fn get_project_by_slug() {
        let db = test_db();
        let id = db
            .create_project_with_slug("Auth Service", "/tmp/auth", Some("auth"))
            .unwrap();
        let p = db.get_project_by_slug("auth").unwrap().unwrap();
        assert_eq!(p.id, id);
        assert!(db.get_project_by_slug("missing").unwrap().is_none());
    }

    #[test]
    fn resolve_project_target_by_slug() {
        let db = test_db();
        let id = db
            .create_project_with_slug("Auth Service", "/tmp/auth", Some("auth"))
            .unwrap();
        let p = db.resolve_project_target("auth").unwrap().unwrap();
        assert_eq!(p.id, id);
    }

    #[test]
    fn delete_project_removes_group_notes_created_by_project() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let gid = db.get_or_create_group("grp").unwrap();
        db.add_project_to_group(pid, gid).unwrap();
        let note_id = db
            .add_group_note(gid, pid, "group note", Some("ctx"))
            .unwrap();

        db.delete_project(pid).unwrap();

        assert!(db.get_project_by_id(pid).unwrap().is_none());
        assert!(db.get_item_by_id(note_id).unwrap().is_none());
        assert!(db.search_items("group note").unwrap().is_empty());
    }

    #[test]
    fn find_project_by_cwd_exact() {
        let db = test_db();
        db.create_project("proj", "/home/user/proj").unwrap();
        let p = db.find_project_by_cwd("/home/user/proj").unwrap().unwrap();
        assert_eq!(p.name, "proj");
    }

    #[test]
    fn find_project_by_cwd_subdirectory() {
        let db = test_db();
        db.create_project("proj", "/home/user/proj").unwrap();
        let p = db
            .find_project_by_cwd("/home/user/proj/src/lib")
            .unwrap()
            .unwrap();
        assert_eq!(p.name, "proj");
    }

    #[test]
    fn find_project_by_cwd_not_found() {
        let db = test_db();
        db.create_project("proj", "/home/user/proj").unwrap();
        assert!(db.find_project_by_cwd("/home/other").unwrap().is_none());
    }

    #[test]
    fn find_project_by_cwd_deepest_match() {
        let db = test_db();
        db.create_project("parent", "/home/user").unwrap();
        db.create_project("child", "/home/user/proj").unwrap();
        let p = db
            .find_project_by_cwd("/home/user/proj/src")
            .unwrap()
            .unwrap();
        assert_eq!(p.name, "child");
    }

    #[test]
    fn list_projects_empty() {
        let db = test_db();
        assert!(db.list_projects().unwrap().is_empty());
    }

    #[test]
    fn list_projects_sorted() {
        let db = test_db();
        db.create_project("beta", "/tmp/b").unwrap();
        db.create_project("alpha", "/tmp/a").unwrap();
        let projects = db.list_projects().unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].name, "alpha");
        assert_eq!(projects[1].name, "beta");
    }

    #[test]
    fn duplicate_project_path_fails() {
        let db = test_db();
        db.create_project("proj1", "/tmp/proj").unwrap();
        assert!(db.create_project("proj2", "/tmp/proj").is_err());
    }

    // --- Groups ---

    #[test]
    fn get_or_create_group_idempotent() {
        let db = test_db();
        let id1 = db.get_or_create_group("grp").unwrap();
        let id2 = db.get_or_create_group("grp").unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn get_group_by_name() {
        let db = test_db();
        db.get_or_create_group("mygroup").unwrap();
        let g = db.get_group_by_name("mygroup").unwrap().unwrap();
        assert_eq!(g.name, "mygroup");
        assert!(db.get_group_by_name("nonexistent").unwrap().is_none());
    }

    #[test]
    fn list_groups_sorted() {
        let db = test_db();
        db.get_or_create_group("beta").unwrap();
        db.get_or_create_group("alpha").unwrap();
        let groups = db.list_groups().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "alpha");
        assert_eq!(groups[1].name, "beta");
    }

    // --- Project ↔ Group ---

    #[test]
    fn project_group_association() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let gid = db.get_or_create_group("grp").unwrap();

        db.add_project_to_group(pid, gid).unwrap();

        let groups = db.get_groups_for_project(pid).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "grp");

        let projects = db.get_projects_for_group(gid).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "proj");
    }

    #[test]
    fn add_project_to_group_idempotent() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let gid = db.get_or_create_group("grp").unwrap();
        db.add_project_to_group(pid, gid).unwrap();
        db.add_project_to_group(pid, gid).unwrap(); // no error
        assert_eq!(db.get_groups_for_project(pid).unwrap().len(), 1);
    }

    #[test]
    fn remove_project_from_group() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let gid = db.get_or_create_group("grp").unwrap();
        db.add_project_to_group(pid, gid).unwrap();
        assert_eq!(db.get_groups_for_project(pid).unwrap().len(), 1);

        assert!(db.remove_project_from_group(pid, gid).unwrap());
        assert_eq!(db.get_groups_for_project(pid).unwrap().len(), 0);

        // group should be auto-deleted since no members remain
        assert!(db.get_group_by_name("grp").unwrap().is_none());

        // removing again returns false
        assert!(!db.remove_project_from_group(pid, gid).unwrap());
    }

    #[test]
    fn remove_project_from_group_keeps_group_with_other_members() {
        let db = test_db();
        let pid1 = db.create_project("proj1", "/tmp/proj1").unwrap();
        let pid2 = db.create_project("proj2", "/tmp/proj2").unwrap();
        let gid = db.get_or_create_group("grp").unwrap();
        db.add_project_to_group(pid1, gid).unwrap();
        db.add_project_to_group(pid2, gid).unwrap();

        assert!(db.remove_project_from_group(pid1, gid).unwrap());

        // group should still exist because pid2 is still a member
        assert!(db.get_group_by_name("grp").unwrap().is_some());
        assert_eq!(db.get_projects_for_group(gid).unwrap().len(), 1);
    }

    #[test]
    fn delete_group_removes_items_and_associations() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let gid = db.get_or_create_group("grp").unwrap();
        db.add_project_to_group(pid, gid).unwrap();
        db.add_group_note(gid, pid, "group note", None).unwrap();

        db.delete_group(gid).unwrap();

        assert!(db.get_group_by_name("grp").unwrap().is_none());
        assert_eq!(db.get_groups_for_project(pid).unwrap().len(), 0);
        assert_eq!(db.get_all_items_for_group(gid).unwrap().len(), 0);
    }

    // --- Service Links ---

    #[test]
    fn create_service_link_resolves_slug_and_lists_outgoing() {
        let db = test_db();
        let auth = db
            .create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        let api = db
            .create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();

        let id = db
            .create_service_link("api", "auth", ServiceLinkKind::DependsOn, Some("JWT"))
            .unwrap();

        let links = db.list_outgoing_service_links(api).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].id, id);
        assert_eq!(links[0].from_project_id, api);
        assert_eq!(links[0].to_project_id, auth);
        assert_eq!(links[0].kind, ServiceLinkKind::DependsOn);
        assert_eq!(links[0].label.as_deref(), Some("JWT"));
    }

    #[test]
    fn create_service_link_resolves_numeric_ids() {
        let db = test_db();
        let auth = db.create_project("auth", "/tmp/auth").unwrap();
        let api = db.create_project("api", "/tmp/api").unwrap();

        let id = db
            .create_service_link(
                &api.to_string(),
                &auth.to_string(),
                ServiceLinkKind::DependsOn,
                None,
            )
            .unwrap();

        let link = db
            .get_service_link_by_endpoints(api, auth, ServiceLinkKind::DependsOn)
            .unwrap()
            .unwrap();
        assert_eq!(link.id, id);
    }

    #[test]
    fn create_service_link_duplicate_is_idempotent() {
        let db = test_db();
        db.create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        let api = db
            .create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();

        let first = db
            .create_service_link("api", "auth", ServiceLinkKind::DependsOn, Some("first"))
            .unwrap();
        let second = db
            .create_service_link("api", "auth", ServiceLinkKind::DependsOn, Some("second"))
            .unwrap();

        assert_eq!(first, second);
        let links = db.list_outgoing_service_links(api).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label.as_deref(), Some("first"));
    }

    #[test]
    fn create_service_link_rejects_self_link() {
        let db = test_db();
        db.create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();

        let err = db
            .create_service_link("api", "api", ServiceLinkKind::DependsOn, None)
            .unwrap_err();

        assert!(err.to_string().contains("itself"));
    }

    #[test]
    fn list_incoming_service_links() {
        let db = test_db();
        let auth = db
            .create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        db.create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();
        db.create_project_with_slug("Web", "/tmp/web", Some("web"))
            .unwrap();

        db.create_service_link("api", "auth", ServiceLinkKind::DependsOn, None)
            .unwrap();
        db.create_service_link("web", "auth", ServiceLinkKind::DependsOn, None)
            .unwrap();

        let links = db.list_incoming_service_links(auth).unwrap();
        assert_eq!(links.len(), 2);
        assert!(links.iter().all(|link| link.to_project_id == auth));
    }

    #[test]
    fn list_group_service_links_only_returns_internal_graph_edges() {
        let db = test_db();
        let auth = db
            .create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        let api = db
            .create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();
        let web = db
            .create_project_with_slug("Web", "/tmp/web", Some("web"))
            .unwrap();
        let group = db.get_or_create_group("core").unwrap();
        db.add_project_to_group(auth, group).unwrap();
        db.add_project_to_group(api, group).unwrap();

        db.create_service_link("api", "auth", ServiceLinkKind::DependsOn, None)
            .unwrap();
        db.create_service_link("web", "auth", ServiceLinkKind::DependsOn, None)
            .unwrap();

        let links = db.list_group_service_links(group).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].from_project_id, api);
        assert_eq!(links[0].to_project_id, auth);
        assert_ne!(links[0].from_project_id, web);
    }

    #[test]
    fn delete_service_link_removes_matching_edge() {
        let db = test_db();
        db.create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        let api = db
            .create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();
        db.create_service_link("api", "auth", ServiceLinkKind::DependsOn, None)
            .unwrap();

        assert!(
            db.delete_service_link("api", "auth", ServiceLinkKind::DependsOn)
                .unwrap()
        );
        assert!(
            !db.delete_service_link("api", "auth", ServiceLinkKind::DependsOn)
                .unwrap()
        );
        assert!(db.list_outgoing_service_links(api).unwrap().is_empty());
    }

    // --- Artifact Dependencies ---

    #[test]
    fn add_artifact_dependency_resolves_item_label_and_service_slug() {
        let db = test_db();
        let api = db
            .create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();
        let auth = db
            .create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        let group = db.get_or_create_group("core").unwrap();
        db.add_project_to_group(api, group).unwrap();
        db.add_project_to_group(auth, group).unwrap();
        let item = db
            .share_file(api, "openapi.yaml", Some("api-spec"))
            .unwrap();

        let id = db
            .add_artifact_dependency(
                api,
                "api-spec",
                "auth",
                ArtifactDependencyKind::ConsumesApi,
                ArtifactReaction::Update,
            )
            .unwrap();

        let deps = db.list_artifact_dependencies_for_project(api).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].id, id);
        assert_eq!(deps[0].shared_item_id, item);
        assert_eq!(deps[0].depends_on_project_id, Some(auth));
        assert_eq!(deps[0].depends_on_project_slug_snapshot, "auth");
        assert_eq!(deps[0].kind, ArtifactDependencyKind::ConsumesApi);
        assert_eq!(deps[0].reaction, ArtifactReaction::Update);
    }

    #[test]
    fn add_artifact_dependency_updates_existing_kind_reaction() {
        let db = test_db();
        let api = db
            .create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();
        db.create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        db.share_file(api, "README.md", Some("readme")).unwrap();

        let first = db
            .add_artifact_dependency(
                api,
                "readme",
                "auth",
                ArtifactDependencyKind::Documents,
                ArtifactReaction::Inspect,
            )
            .unwrap();
        let second = db
            .add_artifact_dependency(
                api,
                "readme",
                "auth",
                ArtifactDependencyKind::Documents,
                ArtifactReaction::RemoveReference,
            )
            .unwrap();

        assert_eq!(first, second);
        let deps = db
            .list_artifact_dependencies_for_item(api, "readme")
            .unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].reaction, ArtifactReaction::RemoveReference);
    }

    #[test]
    fn add_artifact_dependency_rejects_notes() {
        let db = test_db();
        let api = db
            .create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();
        db.create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        db.add_project_note(api, "note body", Some("note")).unwrap();

        let err = db
            .add_artifact_dependency(
                api,
                "note",
                "auth",
                ArtifactDependencyKind::References,
                ArtifactReaction::Inspect,
            )
            .unwrap_err();

        assert!(err.to_string().contains("file or directory"));
    }

    #[test]
    fn remove_artifact_dependency_by_kind() {
        let db = test_db();
        let api = db
            .create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();
        db.create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        db.share_file(api, "openapi.yaml", Some("api-spec"))
            .unwrap();
        db.add_artifact_dependency(
            api,
            "api-spec",
            "auth",
            ArtifactDependencyKind::ConsumesApi,
            ArtifactReaction::Update,
        )
        .unwrap();
        db.add_artifact_dependency(
            api,
            "api-spec",
            "auth",
            ArtifactDependencyKind::References,
            ArtifactReaction::Inspect,
        )
        .unwrap();

        let removed = db
            .remove_artifact_dependency(
                api,
                "api-spec",
                "auth",
                Some(ArtifactDependencyKind::ConsumesApi),
            )
            .unwrap();

        assert_eq!(removed, 1);
        let deps = db
            .list_artifact_dependencies_for_item(api, "api-spec")
            .unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].kind, ArtifactDependencyKind::References);
    }

    #[test]
    fn list_artifact_dependencies_on_service_slug() {
        let db = test_db();
        let api = db
            .create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();
        let web = db
            .create_project_with_slug("Web", "/tmp/web", Some("web"))
            .unwrap();
        db.create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        db.share_file(api, "openapi.yaml", Some("api-spec"))
            .unwrap();
        db.share_file(web, "client.ts", Some("client")).unwrap();

        db.add_artifact_dependency(
            api,
            "api-spec",
            "auth",
            ArtifactDependencyKind::ConsumesApi,
            ArtifactReaction::Update,
        )
        .unwrap();
        db.add_artifact_dependency(
            web,
            "client",
            "auth",
            ArtifactDependencyKind::References,
            ArtifactReaction::Inspect,
        )
        .unwrap();

        let deps = db
            .list_artifact_dependencies_on_service_slug("auth")
            .unwrap();
        assert_eq!(deps.len(), 2);
        assert!(
            deps.iter()
                .all(|dep| dep.depends_on_project_slug_snapshot == "auth")
        );
    }

    #[test]
    fn deleting_shared_item_cascades_artifact_dependencies() {
        let db = test_db();
        let api = db
            .create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();
        db.create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        let item = db
            .share_file(api, "openapi.yaml", Some("api-spec"))
            .unwrap();
        db.add_artifact_dependency(
            api,
            "api-spec",
            "auth",
            ArtifactDependencyKind::ConsumesApi,
            ArtifactReaction::Update,
        )
        .unwrap();

        assert!(db.remove_shared_item_for_project(item, api).unwrap());

        assert!(
            db.list_artifact_dependencies_for_project(api)
                .unwrap()
                .is_empty()
        );
        assert!(
            db.list_artifact_dependencies_on_service_slug("auth")
                .unwrap()
                .is_empty()
        );
    }

    // --- Workspace Events ---

    #[test]
    fn create_workspace_event_snapshots_source_and_link_impacts() {
        let db = test_db();
        let auth = db
            .create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        let api = db
            .create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();
        let group = db.get_or_create_group("core").unwrap();
        db.add_project_to_group(auth, group).unwrap();
        db.add_project_to_group(api, group).unwrap();
        db.create_service_link("api", "auth", ServiceLinkKind::DependsOn, None)
            .unwrap();

        let event_id = db
            .create_workspace_event(
                "auth",
                WorkspaceEventKind::ServiceChanged,
                EventSeverity::Warning,
                "Auth changed",
                Some("Token format changed"),
            )
            .unwrap();

        let event = db.get_workspace_event(event_id).unwrap().unwrap();
        assert_eq!(event.source_project_id, Some(auth));
        assert_eq!(event.source_project_slug, "auth");
        assert_eq!(event.source_project_name, "Auth");
        assert_eq!(event.group_id, Some(group));
        assert_eq!(event.kind, WorkspaceEventKind::ServiceChanged);
        assert_eq!(event.severity, EventSeverity::Warning);
        assert_eq!(event.status, EventStatus::Open);

        let targets = db.list_event_targets(event_id).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].affected_project_id, Some(api));
        assert_eq!(
            targets[0].relation_kind,
            EventTargetRelationKind::LinkedService
        );
    }

    #[test]
    fn create_workspace_event_calculates_artifact_impacts() {
        let db = test_db();
        db.create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        let api = db
            .create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();
        db.share_file(api, "openapi.yaml", Some("api-spec"))
            .unwrap();
        db.add_artifact_dependency(
            api,
            "api-spec",
            "auth",
            ArtifactDependencyKind::ConsumesApi,
            ArtifactReaction::Update,
        )
        .unwrap();

        let event_id = db
            .create_workspace_event(
                "auth",
                WorkspaceEventKind::ServiceChanged,
                EventSeverity::Info,
                "Auth changed",
                None,
            )
            .unwrap();

        let targets = db.list_event_targets(event_id).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].affected_project_id, Some(api));
        assert_eq!(
            targets[0].relation_kind,
            EventTargetRelationKind::ArtifactDependency
        );

        let artifacts = db.list_event_artifacts(event_id).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].affected_project_id, Some(api));
        assert_eq!(artifacts[0].path_snapshot, "openapi.yaml");
        assert_eq!(artifacts[0].reaction, ArtifactReaction::Update);
        assert_eq!(artifacts[0].status, EventArtifactStatus::Open);
    }

    #[test]
    fn close_workspace_event_resolves_targets_and_artifacts() {
        let db = test_db();
        db.create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        let api = db
            .create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();
        db.share_file(api, "openapi.yaml", Some("api-spec"))
            .unwrap();
        db.add_artifact_dependency(
            api,
            "api-spec",
            "auth",
            ArtifactDependencyKind::ConsumesApi,
            ArtifactReaction::Update,
        )
        .unwrap();
        let event_id = db
            .create_workspace_event(
                "auth",
                WorkspaceEventKind::ServiceChanged,
                EventSeverity::Info,
                "Auth changed",
                None,
            )
            .unwrap();

        assert!(db.close_workspace_event(event_id).unwrap());
        assert!(!db.close_workspace_event(event_id).unwrap());

        let event = db.get_workspace_event(event_id).unwrap().unwrap();
        assert_eq!(event.status, EventStatus::Closed);
        assert_eq!(
            db.list_event_targets(event_id).unwrap()[0].status,
            EventTargetStatus::Resolved
        );
        assert_eq!(
            db.list_event_artifacts(event_id).unwrap()[0].status,
            EventArtifactStatus::Resolved
        );
    }

    #[test]
    fn workspace_event_inbox_and_remove() {
        let db = test_db();
        db.create_project_with_slug("Auth", "/tmp/auth", Some("auth"))
            .unwrap();
        let api = db
            .create_project_with_slug("API", "/tmp/api", Some("api"))
            .unwrap();
        db.create_service_link("api", "auth", ServiceLinkKind::DependsOn, None)
            .unwrap();
        let event_id = db
            .create_workspace_event(
                "auth",
                WorkspaceEventKind::ServiceDeleted,
                EventSeverity::Critical,
                "Auth deleted",
                None,
            )
            .unwrap();

        let inbox = db.list_workspace_event_inbox(api).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].id, event_id);

        let open_auth_events = db
            .list_workspace_events(Some("auth"), Some(EventStatus::Open))
            .unwrap();
        assert_eq!(open_auth_events.len(), 1);

        assert!(db.remove_workspace_event(event_id).unwrap());
        assert!(db.get_workspace_event(event_id).unwrap().is_none());
        assert!(db.list_event_targets(event_id).unwrap().is_empty());
    }

    // --- Shared Items: Files & Dirs ---

    #[test]
    fn share_file_and_retrieve() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let id = db.share_file(pid, "src/main.rs", Some("main")).unwrap();

        let item = db.get_item_by_id(id).unwrap().unwrap();
        assert_eq!(item.kind, SharedItemKind::File);
        assert_eq!(item.path.as_deref(), Some("src/main.rs"));
        assert_eq!(item.label.as_deref(), Some("main"));
        assert_eq!(item.project_id, Some(pid));
        assert!(item.content.is_none());
    }

    #[test]
    fn share_dir_and_retrieve() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let id = db.share_dir(pid, "src", None).unwrap();

        let item = db.get_item_by_id(id).unwrap().unwrap();
        assert_eq!(item.kind, SharedItemKind::Dir);
        assert_eq!(item.path.as_deref(), Some("src"));
        assert!(item.label.is_none());
    }

    #[test]
    fn share_file_duplicate_path_fails() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        db.share_file(pid, "file.txt", None).unwrap();
        assert!(db.share_file(pid, "file.txt", None).is_err());
    }

    #[test]
    fn get_shared_items_for_project() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        db.share_file(pid, "a.txt", None).unwrap();
        db.share_file(pid, "b.txt", None).unwrap();

        let items = db.get_shared_items_for_project(pid).unwrap();
        assert_eq!(items.len(), 2);
    }

    // --- Shared Items: Notes ---

    #[test]
    fn add_project_note_and_search() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let id = db
            .add_project_note(pid, "important information", Some("info"))
            .unwrap();

        let item = db.get_item_by_id(id).unwrap().unwrap();
        assert_eq!(item.kind, SharedItemKind::Note);
        assert_eq!(item.content.as_deref(), Some("important information"));
        assert!(item.path.is_none());

        let results = db.search_items("important").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, id);
    }

    #[test]
    fn add_group_note_and_search() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let gid = db.get_or_create_group("grp").unwrap();
        db.add_project_to_group(pid, gid).unwrap();

        let id = db
            .add_group_note(gid, pid, "group context", Some("ctx"))
            .unwrap();

        let item = db.get_item_by_id(id).unwrap().unwrap();
        assert_eq!(item.group_id, Some(gid));
        assert_eq!(item.created_by_project_id, Some(pid));
        assert!(item.project_id.is_none());

        let results = db.search_items("context").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_items_fts5_special_chars() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        db.add_project_note(pid, "test with special* chars", None)
            .unwrap();
        // Should not crash on FTS5 special characters
        let results = db.search_items("special*").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_items_no_results() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        db.add_project_note(pid, "hello world", None).unwrap();
        let results = db.search_items("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_items_finds_files_by_path() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        db.share_file(pid, "api.json", Some("tasky api")).unwrap();
        db.share_file(pid, "Makefile", None).unwrap();

        let results = db.search_items("api").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path.as_deref(), Some("api.json"));
    }

    #[test]
    fn search_items_finds_files_by_label() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        db.share_file(pid, "config.yml", Some("deploy config"))
            .unwrap();

        let results = db.search_items("deploy").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].label.as_deref(), Some("deploy config"));
    }

    #[test]
    fn search_items_no_duplicate_when_note_and_file_match() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        db.add_project_note(pid, "see api.json for details", Some("api ref"))
            .unwrap();
        db.share_file(pid, "api.json", Some("api spec")).unwrap();

        let results = db.search_items("api").unwrap();
        assert_eq!(results.len(), 2);
    }

    // --- Remove ---

    #[test]
    fn remove_shared_item() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let id = db.share_file(pid, "file.txt", None).unwrap();

        assert!(db.remove_shared_item(id).unwrap());
        assert!(db.get_item_by_id(id).unwrap().is_none());
    }

    #[test]
    fn remove_shared_item_not_found() {
        let db = test_db();
        assert!(!db.remove_shared_item(9999).unwrap());
    }

    #[test]
    fn remove_shared_item_for_project_wrong_project() {
        let db = test_db();
        let pid1 = db.create_project("p1", "/tmp/p1").unwrap();
        let pid2 = db.create_project("p2", "/tmp/p2").unwrap();
        let id = db.share_file(pid1, "file.txt", None).unwrap();

        // Can't remove item belonging to another project
        assert!(!db.remove_shared_item_for_project(id, pid2).unwrap());
        // Owner can remove
        assert!(db.remove_shared_item_for_project(id, pid1).unwrap());
    }

    #[test]
    fn remove_by_label() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        db.share_file(pid, "file.txt", Some("myfile")).unwrap();

        assert!(!db.remove_by_label(pid, "nonexistent").unwrap());
        assert!(db.remove_by_label(pid, "myfile").unwrap());
        assert!(db.get_shared_items_for_project(pid).unwrap().is_empty());
    }

    #[test]
    fn remove_by_path() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        db.share_file(pid, "file.txt", None).unwrap();

        assert!(!db.remove_by_path(pid, "other.txt").unwrap());
        assert!(db.remove_by_path(pid, "file.txt").unwrap());
    }

    #[test]
    fn remove_note_cleans_fts() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let id = db
            .add_project_note(pid, "searchable content", None)
            .unwrap();

        assert!(!db.search_items("searchable").unwrap().is_empty());
        db.remove_shared_item(id).unwrap();
        assert!(db.search_items("searchable").unwrap().is_empty());
    }

    // --- Group aggregation ---

    #[test]
    fn get_all_items_for_group() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let gid = db.get_or_create_group("grp").unwrap();
        db.add_project_to_group(pid, gid).unwrap();

        db.share_file(pid, "file.txt", None).unwrap();
        db.add_group_note(gid, pid, "note", None).unwrap();

        let items = db.get_all_items_for_group(gid).unwrap();
        assert_eq!(items.len(), 2);
    }

    // --- Sync ---

    #[test]
    fn sync_removes_stale_files() {
        let tmp = tempfile::tempdir().unwrap();
        let project_path = tmp.path().to_string_lossy().to_string();

        // Create a file, share it, then delete
        let file_path = tmp.path().join("test.txt");
        std::fs::write(&file_path, "hello").unwrap();

        let db_file = NamedTempFile::new().unwrap();
        let db = Db::open(db_file.path()).unwrap();
        let pid = db.create_project("proj", &project_path).unwrap();
        db.share_file(pid, "test.txt", None).unwrap();

        // File exists => nothing removed
        let removed = db.sync_files().unwrap();
        assert!(removed.is_empty());

        // Delete file => sync removes it
        std::fs::remove_file(&file_path).unwrap();
        let removed = db.sync_files().unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].1, "test.txt");
    }

    // --- Resolve ---

    #[test]
    fn resolve_by_id() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let id = db.add_project_note(pid, "hello", Some("lbl")).unwrap();

        let item = db
            .resolve_item_for_project(&id.to_string(), pid)
            .unwrap()
            .unwrap();
        assert_eq!(item.id, id);
    }

    #[test]
    fn resolve_by_label() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let id = db.add_project_note(pid, "hello", Some("my-label")).unwrap();

        let item = db
            .resolve_item_for_project("my-label", pid)
            .unwrap()
            .unwrap();
        assert_eq!(item.id, id);
    }

    #[test]
    fn resolve_by_path() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let id = db.share_file(pid, "src/main.rs", None).unwrap();

        let item = db
            .resolve_item_for_project("src/main.rs", pid)
            .unwrap()
            .unwrap();
        assert_eq!(item.id, id);
    }

    #[test]
    fn resolve_wrong_project() {
        let db = test_db();
        let pid1 = db.create_project("p1", "/tmp/p1").unwrap();
        let pid2 = db.create_project("p2", "/tmp/p2").unwrap();
        let id = db.add_project_note(pid1, "hello", Some("lbl")).unwrap();

        assert!(
            db.resolve_item_for_project(&id.to_string(), pid2)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn resolve_group_note_by_creator() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let gid = db.get_or_create_group("grp").unwrap();
        db.add_project_to_group(pid, gid).unwrap();
        let id = db
            .add_group_note(gid, pid, "group stuff", Some("gnote"))
            .unwrap();

        let item = db.resolve_item_for_project("gnote", pid).unwrap().unwrap();
        assert_eq!(item.id, id);
    }

    // --- Update ---

    #[test]
    fn edit_note_content() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let id = db
            .add_project_note(pid, "old content", Some("lbl"))
            .unwrap();

        let update = SharedItemUpdate {
            content: Some("new content".to_string()),
            label: None,
            scope_change: None,
        };
        assert!(db.update_shared_item(id, pid, &update).unwrap());

        let item = db.get_item_by_id(id).unwrap().unwrap();
        assert_eq!(item.content.as_deref(), Some("new content"));
    }

    #[test]
    fn edit_note_label() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let id = db
            .add_project_note(pid, "content", Some("old-label"))
            .unwrap();

        let update = SharedItemUpdate {
            content: None,
            label: Some(Some("new-label".to_string())),
            scope_change: None,
        };
        db.update_shared_item(id, pid, &update).unwrap();

        let item = db.get_item_by_id(id).unwrap().unwrap();
        assert_eq!(item.label.as_deref(), Some("new-label"));
    }

    #[test]
    fn edit_note_clear_label() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let id = db.add_project_note(pid, "content", Some("lbl")).unwrap();

        let update = SharedItemUpdate {
            content: None,
            label: Some(None),
            scope_change: None,
        };
        db.update_shared_item(id, pid, &update).unwrap();

        let item = db.get_item_by_id(id).unwrap().unwrap();
        assert!(item.label.is_none());
    }

    #[test]
    fn edit_note_scope_project_to_group() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let gid = db.get_or_create_group("grp").unwrap();
        db.add_project_to_group(pid, gid).unwrap();
        let id = db.add_project_note(pid, "was project", None).unwrap();

        let update = SharedItemUpdate {
            content: None,
            label: None,
            scope_change: Some(ScopeChange::ToGroup { group_id: gid }),
        };
        db.update_shared_item(id, pid, &update).unwrap();

        let item = db.get_item_by_id(id).unwrap().unwrap();
        assert!(item.project_id.is_none());
        assert_eq!(item.group_id, Some(gid));
        assert_eq!(item.created_by_project_id, Some(pid));
    }

    #[test]
    fn edit_note_scope_group_to_project() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let gid = db.get_or_create_group("grp").unwrap();
        db.add_project_to_group(pid, gid).unwrap();
        let id = db.add_group_note(gid, pid, "was group", None).unwrap();

        let update = SharedItemUpdate {
            content: None,
            label: None,
            scope_change: Some(ScopeChange::ToProject),
        };
        db.update_shared_item(id, pid, &update).unwrap();

        let item = db.get_item_by_id(id).unwrap().unwrap();
        assert_eq!(item.project_id, Some(pid));
        assert!(item.group_id.is_none());
        assert!(item.created_by_project_id.is_none());
    }

    #[test]
    fn edit_note_combined() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let gid = db.get_or_create_group("grp").unwrap();
        db.add_project_to_group(pid, gid).unwrap();
        let id = db.add_project_note(pid, "old", Some("old-lbl")).unwrap();

        let update = SharedItemUpdate {
            content: Some("new".to_string()),
            label: Some(Some("new-lbl".to_string())),
            scope_change: Some(ScopeChange::ToGroup { group_id: gid }),
        };
        db.update_shared_item(id, pid, &update).unwrap();

        let item = db.get_item_by_id(id).unwrap().unwrap();
        assert_eq!(item.content.as_deref(), Some("new"));
        assert_eq!(item.label.as_deref(), Some("new-lbl"));
        assert_eq!(item.group_id, Some(gid));
        assert!(item.project_id.is_none());
    }

    #[test]
    fn edit_file_label_only() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let id = db.share_file(pid, "f.txt", Some("old")).unwrap();

        let update = SharedItemUpdate {
            content: None,
            label: Some(Some("new".to_string())),
            scope_change: None,
        };
        db.update_shared_item(id, pid, &update).unwrap();

        let item = db.get_item_by_id(id).unwrap().unwrap();
        assert_eq!(item.label.as_deref(), Some("new"));
    }

    #[test]
    fn edit_wrong_project_fails() {
        let db = test_db();
        let pid1 = db.create_project("p1", "/tmp/p1").unwrap();
        let pid2 = db.create_project("p2", "/tmp/p2").unwrap();
        let id = db.add_project_note(pid1, "hello", None).unwrap();

        let update = SharedItemUpdate {
            content: Some("hack".to_string()),
            label: None,
            scope_change: None,
        };
        assert!(db.update_shared_item(id, pid2, &update).is_err());
    }

    #[test]
    fn edit_fts_updated() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let id = db.add_project_note(pid, "alpha content", None).unwrap();

        assert_eq!(db.search_items("alpha").unwrap().len(), 1);

        let update = SharedItemUpdate {
            content: Some("beta content".to_string()),
            label: None,
            scope_change: None,
        };
        db.update_shared_item(id, pid, &update).unwrap();

        assert!(db.search_items("alpha").unwrap().is_empty());
        assert_eq!(db.search_items("beta").unwrap().len(), 1);
        assert_eq!(db.search_items("beta").unwrap()[0].id, id);
    }

    // --- Export Config ---

    #[test]
    fn export_project_config_basic() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let gid = db.get_or_create_group("team").unwrap();
        db.add_project_to_group(pid, gid).unwrap();
        db.share_file(pid, "src/main.rs", Some("main")).unwrap();
        db.share_dir(pid, "docs", None).unwrap();
        db.add_project_note(pid, "important", Some("info")).unwrap();

        let config = db.export_project_config(pid).unwrap();
        assert_eq!(config.name, "proj");
        assert_eq!(config.groups, vec!["team"]);
        assert_eq!(config.share.len(), 2);
        assert_eq!(config.share[0].path(), "src/main.rs");
        assert_eq!(config.share[0].label(), Some("main"));
        assert_eq!(config.share[1].path(), "docs");
        assert!(config.share[1].label().is_none());
        assert_eq!(config.notes.len(), 1);
        assert_eq!(config.notes[0].content, "important");
        assert_eq!(config.notes[0].label.as_deref(), Some("info"));
    }

    #[test]
    fn export_excludes_group_notes() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let gid = db.get_or_create_group("team").unwrap();
        db.add_project_to_group(pid, gid).unwrap();
        db.add_project_note(pid, "proj note", Some("pn")).unwrap();
        db.add_group_note(gid, pid, "group note", Some("gn"))
            .unwrap();

        let config = db.export_project_config(pid).unwrap();
        assert_eq!(config.notes.len(), 1);
        assert_eq!(config.notes[0].content, "proj note");
    }

    // --- Sync from Config ---

    #[test]
    fn sync_adds_missing_groups_and_shares() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();

        let config = WorkspaceConfig {
            name: "proj".to_string(),
            groups: vec!["team-a".to_string(), "team-b".to_string()],
            share: vec![
                ShareEntry::PathOnly("README.md".to_string()),
                ShareEntry::WithLabel {
                    path: "api.json".to_string(),
                    label: "API spec".to_string(),
                },
            ],
            notes: vec![],
        };

        let report = db.sync_from_config(pid, &config).unwrap();
        assert_eq!(report.groups_added, 2);
        assert_eq!(report.shares_added, 2);

        let groups = db.get_groups_for_project(pid).unwrap();
        assert_eq!(groups.len(), 2);

        let items = db.get_shared_items_for_project(pid).unwrap();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn sync_removes_extra_groups_and_shares() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        let gid = db.get_or_create_group("old-group").unwrap();
        db.add_project_to_group(pid, gid).unwrap();
        db.share_file(pid, "old-file.txt", None).unwrap();

        let config = WorkspaceConfig {
            name: "proj".to_string(),
            groups: vec![],
            share: vec![],
            notes: vec![],
        };

        let report = db.sync_from_config(pid, &config).unwrap();
        assert_eq!(report.groups_removed, 1);
        assert_eq!(report.shares_removed, 1);

        assert!(db.get_groups_for_project(pid).unwrap().is_empty());
        assert!(db.get_shared_items_for_project(pid).unwrap().is_empty());
    }

    #[test]
    fn sync_adds_and_updates_notes() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        db.add_project_note(pid, "old content", Some("info"))
            .unwrap();

        let config = WorkspaceConfig {
            name: "proj".to_string(),
            groups: vec![],
            share: vec![],
            notes: vec![
                NoteEntry {
                    content: "new content".to_string(),
                    label: Some("info".to_string()),
                },
                NoteEntry {
                    content: "brand new".to_string(),
                    label: Some("extra".to_string()),
                },
            ],
        };

        let report = db.sync_from_config(pid, &config).unwrap();
        assert_eq!(report.notes_updated, 1);
        assert_eq!(report.notes_added, 1);

        let items = db.get_shared_items_for_project(pid).unwrap();
        let notes: Vec<_> = items
            .iter()
            .filter(|i| i.kind == SharedItemKind::Note)
            .collect();
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn sync_removes_unlabeled_notes_not_in_config() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        db.add_project_note(pid, "keep this", Some("keep")).unwrap();
        db.add_project_note(pid, "remove this", None).unwrap();

        let config = WorkspaceConfig {
            name: "proj".to_string(),
            groups: vec![],
            share: vec![],
            notes: vec![NoteEntry {
                content: "keep this".to_string(),
                label: Some("keep".to_string()),
            }],
        };

        let report = db.sync_from_config(pid, &config).unwrap();
        assert_eq!(report.notes_removed, 1);

        let items = db.get_shared_items_for_project(pid).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label.as_deref(), Some("keep"));
    }

    #[test]
    fn sync_duplicate_share_path_handled() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();
        db.share_file(pid, "existing.txt", None).unwrap();

        let config = WorkspaceConfig {
            name: "proj".to_string(),
            groups: vec![],
            share: vec![ShareEntry::PathOnly("existing.txt".to_string())],
            notes: vec![],
        };

        // Should not error on duplicate path
        let report = db.sync_from_config(pid, &config).unwrap();
        assert_eq!(report.shares_added, 0);

        let items = db.get_shared_items_for_project(pid).unwrap();
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn sync_idempotent() {
        let db = test_db();
        let pid = db.create_project("proj", "/tmp/proj").unwrap();

        let config = WorkspaceConfig {
            name: "proj".to_string(),
            groups: vec!["team".to_string()],
            share: vec![ShareEntry::PathOnly("file.txt".to_string())],
            notes: vec![NoteEntry {
                content: "note".to_string(),
                label: Some("lbl".to_string()),
            }],
        };

        let r1 = db.sync_from_config(pid, &config).unwrap();
        assert_eq!(r1.groups_added, 1);
        assert_eq!(r1.shares_added, 1);
        assert_eq!(r1.notes_added, 1);

        // Second sync should change nothing
        let r2 = db.sync_from_config(pid, &config).unwrap();
        assert_eq!(r2.groups_added, 0);
        assert_eq!(r2.groups_removed, 0);
        assert_eq!(r2.shares_added, 0);
        assert_eq!(r2.shares_removed, 0);
        assert_eq!(r2.notes_added, 0);
        assert_eq!(r2.notes_removed, 0);
        assert_eq!(r2.notes_updated, 0);
    }

    // --- Files FTS ---

    #[test]
    fn index_and_search_file() {
        let db = test_db();
        let pid = db.create_project("p", "/tmp/p").unwrap();
        let id = db.share_file(pid, "doc.md", None).unwrap();
        db.index_file(
            id,
            "doc.md",
            "/tmp/p/doc.md",
            "hello rustaceans world",
            100,
            22,
        )
        .unwrap();

        let hits = db.search_files("rustaceans", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].shared_item_id, id);
        assert_eq!(hits[0].path, "doc.md");
        assert!(hits[0].snippet.contains("["));
    }

    #[test]
    fn index_file_upsert_replaces_content() {
        let db = test_db();
        let pid = db.create_project("p", "/tmp/p").unwrap();
        let id = db.share_file(pid, "a.md", None).unwrap();
        db.index_file(id, "a.md", "/tmp/p/a.md", "alpha", 1, 5)
            .unwrap();
        db.index_file(id, "a.md", "/tmp/p/a.md", "bravo", 2, 5)
            .unwrap();

        let hits = db.search_files("bravo", 10).unwrap();
        assert_eq!(hits.len(), 1);
        let hits_old = db.search_files("alpha", 10).unwrap();
        assert!(hits_old.is_empty(), "old content should be gone");

        let meta = db.get_file_index_meta(id).unwrap().unwrap();
        assert_eq!(meta.1, 2);
    }

    #[test]
    fn delete_file_index_removes_rows() {
        let db = test_db();
        let pid = db.create_project("p", "/tmp/p").unwrap();
        let id = db.share_file(pid, "x.md", None).unwrap();
        db.index_file(id, "x.md", "/tmp/p/x.md", "lorem ipsum", 1, 11)
            .unwrap();
        db.delete_file_index(id).unwrap();
        assert!(db.search_files("lorem", 10).unwrap().is_empty());
        assert!(db.get_file_index_meta(id).unwrap().is_none());
    }

    #[test]
    fn search_cyrillic() {
        let db = test_db();
        let pid = db.create_project("p", "/tmp/p").unwrap();
        let id = db.share_file(pid, "ru.md", None).unwrap();
        db.index_file(
            id,
            "ru.md",
            "/tmp/p/ru.md",
            "привет мир полнотекстовый поиск",
            1,
            40,
        )
        .unwrap();
        let hits = db.search_files("полнотекстовый", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }
}
