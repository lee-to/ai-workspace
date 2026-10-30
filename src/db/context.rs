use super::Db;
use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use std::io::Write;
use std::path::{Path, PathBuf};

const ROOT_MARKER: &str = ".ai-workspace-id";

fn read_marker(root: &Path) -> Result<String> {
    let marker = root.join(ROOT_MARKER);
    let meta = std::fs::symlink_metadata(&marker)
        .context("Checkout identity missing; refusing to reuse its path")?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() != 36 {
        bail!("Invalid checkout identity marker");
    }
    let actual = std::fs::read_to_string(marker)?;
    uuid::Uuid::parse_str(&actual).context("Invalid checkout identity marker")?;
    Ok(actual)
}

impl Db {
    /// Search maintenance may skip unavailable bindings, but must never read
    /// their source or discard their historical index rows.
    pub(crate) fn available_checkout_ids(&self, permitted: Option<&[i64]>) -> Result<Vec<i64>> {
        let mut available = Vec::new();
        for project in self.list_projects()? {
            if permitted.is_some_and(|ids| !ids.contains(&project.id)) {
                continue;
            }
            match self.verify_checkout(project.id) {
                Ok(_) => available.push(project.id),
                Err(e) => log::warn!(
                    "Skipping unavailable checkout {} during search: {}",
                    project.id,
                    e
                ),
            }
        }
        Ok(available)
    }

    pub(crate) fn verify_checkout(&self, project_id: i64) -> Result<PathBuf> {
        let project = self
            .get_project_by_id(project_id)?
            .context("Project not found")?;
        let root = Path::new(&project.path)
            .canonicalize()
            .context("Checkout moved or missing; use worktree relocate explicitly")?;
        let (_, _, expected) = self.checkout_binding(project_id)?;
        let marker = root.join(ROOT_MARKER);
        if expected.is_none() && !marker.try_exists()? {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
                .context("Cannot establish checkout identity")?;
            file.write_all(uuid::Uuid::new_v4().to_string().as_bytes())?;
            file.sync_all()?;
        }
        let actual = read_marker(&root)?;
        if let Some(expected) = expected {
            if actual != expected {
                bail!("Checkout identity mismatch; refusing to read another checkout");
            }
        } else {
            self.set_checkout_token(project_id, &actual)?;
        }
        Ok(root)
    }
    pub(crate) fn checkout_binding(
        &self,
        project_id: i64,
    ) -> Result<(String, String, Option<String>)> {
        let existing = self.conn.query_row(
            "SELECT logical_id, binding_id, root_token FROM project_bindings WHERE project_id = ?1",
            [project_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        if let Some(existing) = existing {
            return Ok(existing);
        }
        // Old single-checkout registrations retain their IDs. New bindings use
        // opaque UUIDs and never expose the absolute root in JSON metadata.
        self.conn.execute(
            "INSERT OR IGNORE INTO project_bindings (project_id, logical_id, binding_id)
             SELECT id, 'project:' || id, ?2 FROM projects WHERE id = ?1",
            params![project_id, uuid::Uuid::new_v4().to_string()],
        )?;
        self.conn.query_row(
            "SELECT logical_id, binding_id, root_token FROM project_bindings WHERE project_id = ?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).context("Checkout binding not found")
    }

    pub(crate) fn set_checkout_token(&self, project_id: i64, token: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE project_bindings SET root_token = ?2 WHERE project_id = ?1 AND root_token IS NULL",
            params![project_id, token],
        )?;
        Ok(())
    }

    pub(crate) fn attach_checkout(&self, project_id: i64, target_id: i64) -> Result<()> {
        let (logical_id, _, _) = self.checkout_binding(target_id)?;
        self.checkout_binding(project_id)?;
        self.conn.execute(
            "UPDATE project_bindings SET logical_id = ?2 WHERE project_id = ?1",
            params![project_id, logical_id],
        )?;
        Ok(())
    }

    pub(crate) fn require_unambiguous_checkout(&self, project_id: i64) -> Result<()> {
        let (logical_id, _, _) = self.checkout_binding(project_id)?;
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM project_bindings WHERE logical_id = ?1",
            [logical_id],
            |row| row.get(0),
        )?;
        if count > 1 {
            bail!(
                "Multiple checkouts match this logical project; select an explicit project ID or registered checkout path"
            );
        }
        Ok(())
    }

    pub(crate) fn relocate_checkout(&self, project_id: i64, path: &str) -> Result<()> {
        let (_, _, token) = self.checkout_binding(project_id)?;
        let token = token.context("Initialize the checkout context before relocating it")?;
        let actual = read_marker(Path::new(path))
            .context("Relocation requires the original checkout identity marker")?;
        if actual != token {
            bail!("Checkout identity mismatch; refusing to bind another directory");
        }
        self.conn.execute(
            "UPDATE projects SET path = ?2 WHERE id = ?1",
            params![project_id, path],
        )?;
        Ok(())
    }

    pub(crate) fn indexed_code_hashes(&self, project_id: i64) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, content_hash FROM code_files WHERE project_id = ?1")?;
        let rows = stmt.query_map([project_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
