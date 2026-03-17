use anyhow::{Result, bail};
use clap::{Subcommand, ValueEnum};
use log::{debug, info};
use std::collections::HashSet;
use std::env;
use std::io::IsTerminal;
use std::path::Path;

use crate::db::{Db, ScopeChange, SharedItemUpdate};

#[derive(Debug, Clone, ValueEnum)]
pub enum NoteScope {
    Project,
    Group,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum ListTarget {
    All,
    Projects,
    Groups,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize current directory as a project
    Init {
        /// Project name (defaults to directory name)
        #[arg(short, long)]
        name: Option<String>,
        /// Group to join or create
        #[arg(short, long)]
        group: Option<String>,
    },
    /// Share a file or directory with all project groups
    Share {
        /// Path to share (relative to project root)
        path: String,
        /// Human-readable label
        #[arg(short, long)]
        label: Option<String>,
    },
    /// Add a note (project-scoped or group-scoped)
    Note {
        /// Note content
        content: String,
        /// Human-readable label
        #[arg(short, long)]
        label: Option<String>,
        /// Note scope: project or group (default: group)
        #[arg(short, long, default_value = "group")]
        scope: NoteScope,
        /// Group name (required for group-scoped notes)
        #[arg(long)]
        group: Option<String>,
    },
    /// Edit a shared item (content, label, or scope)
    Edit {
        /// Item ID, label, or path to edit
        target: String,
        /// New content (notes only)
        #[arg(short, long)]
        content: Option<String>,
        /// New label (use empty string to clear)
        #[arg(short, long)]
        label: Option<String>,
        /// Change scope to project or group
        #[arg(short, long)]
        scope: Option<NoteScope>,
        /// Group name (required when changing scope to group)
        #[arg(long)]
        group: Option<String>,
    },
    /// Remove a shared item by ID, label, or path
    Rm {
        /// Item ID, label, or path to remove
        target: String,
    },
    /// Leave a group (remove current project from it)
    Leave {
        /// Group name
        group: String,
    },
    /// Delete a group entirely (removes all associations and group-scoped items)
    DeleteGroup {
        /// Group name
        group: String,
    },
    /// List all projects and groups in the workspace
    List {
        /// What to list: projects, groups, or all (default)
        #[arg(value_enum, default_value = "all")]
        what: ListTarget,
    },
    /// Show project status
    Status,
    /// Export project config to .ai-workspace.json
    Export,
    /// Sync: verify shared files/dirs exist, clean up stale entries, reconcile .ai-workspace.json
    Sync,
    /// Start MCP server (stdio transport)
    Serve,
}

/// Resolve the current project from cwd
fn require_project(db: &Db) -> Result<crate::models::Project> {
    let cwd = env::current_dir()?;
    let cwd_str = cwd.to_string_lossy();
    debug!("Current directory: {}", cwd_str);
    match db.find_project_by_cwd(&cwd_str)? {
        Some(p) => {
            debug!("Found project: {} (id={})", p.name, p.id);
            Ok(p)
        }
        None => bail!("No project found for current directory.\nRun `ai-workspace init` first."),
    }
}

/// If .ai-workspace.json exists in the project root, re-export and save it.
fn update_workspace_json_if_exists(db: &Db, project: &crate::models::Project) -> Result<()> {
    let config_path = Path::new(&project.path).join(".ai-workspace.json");
    if config_path.exists() {
        debug!("Updating .ai-workspace.json at {}", config_path.display());
        let config = db.export_project_config(project.id)?;
        config.save(&config_path)?;
        info!("Updated .ai-workspace.json");
    }
    Ok(())
}

fn print_success(message: impl AsRef<str>) {
    let prefix = style_ok("[ok]");
    println!("{} {}", prefix, message.as_ref());
}

fn print_info(message: impl AsRef<str>) {
    let prefix = style_info("[i]");
    println!("{} {}", prefix, message.as_ref());
}

fn truncate_for_cell(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }

    let mut truncated = input
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

fn make_table_row(row: &[String], widths: &[usize]) -> String {
    let mut out = String::from("|");
    for (i, cell) in row.iter().enumerate() {
        out.push(' ');
        out.push_str(&format!("{:width$}", cell, width = widths[i]));
        out.push(' ');
        out.push('|');
    }
    out
}

fn use_color_output() -> bool {
    if let Ok(value) = env::var("NO_COLOR")
        && !value.is_empty()
    {
        return false;
    }
    if let Ok(value) = env::var("CLICOLOR")
        && value == "0"
    {
        return false;
    }
    if let Ok(value) = env::var("CLICOLOR_FORCE")
        && value != "0"
    {
        return true;
    }
    std::io::stdout().is_terminal()
}

fn style_with_ansi(text: impl AsRef<str>, ansi_code: &str) -> String {
    if use_color_output() {
        format!("\x1b[{}m{}\x1b[0m", ansi_code, text.as_ref())
    } else {
        text.as_ref().to_string()
    }
}

fn style_ok(text: impl AsRef<str>) -> String {
    style_with_ansi(text, "32;1")
}

fn style_info(text: impl AsRef<str>) -> String {
    style_with_ansi(text, "36;1")
}

fn style_heading(text: impl AsRef<str>) -> String {
    style_with_ansi(text, "1")
}

fn style_table_border(text: impl AsRef<str>) -> String {
    style_with_ansi(text, "2")
}

fn style_table_header(text: impl AsRef<str>) -> String {
    style_with_ansi(text, "36;1")
}

fn print_section(title: impl AsRef<str>) {
    println!("{}", style_heading(title.as_ref()));
}

fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    if rows.is_empty() {
        return;
    }

    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        if row.len() != headers.len() {
            continue;
        }
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(cell.len());
        }
    }

    let border = format!(
        "+{}+",
        widths
            .iter()
            .map(|w| "-".repeat(w + 2))
            .collect::<Vec<_>>()
            .join("+")
    );

    println!("{}", style_table_border(&border));
    let header_row = headers.iter().map(|h| h.to_string()).collect::<Vec<_>>();
    println!(
        "{}",
        style_table_header(make_table_row(&header_row, &widths))
    );
    println!("{}", style_table_border(&border));
    for row in rows {
        if row.len() == headers.len() {
            println!("{}", make_table_row(row, &widths));
        }
    }
    println!("{}", style_table_border(&border));
}

pub fn run(cmd: Command) -> Result<()> {
    match cmd {
        Command::Serve => crate::mcp::serve(),

        Command::Init { name, group } => {
            let cwd = env::current_dir()?;
            let cwd_str = cwd.to_string_lossy().to_string();

            // Check for .ai-workspace.json
            let config_path = cwd.join(".ai-workspace.json");
            let config = if config_path.exists() {
                info!("Found .ai-workspace.json at {}", config_path.display());
                Some(crate::models::WorkspaceConfig::load(&config_path)?)
            } else {
                None
            };

            // --name wins over .json name, which wins over dir name
            let project_name = name.unwrap_or_else(|| {
                if let Some(ref cfg) = config {
                    cfg.name.clone()
                } else {
                    cwd.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "unnamed".to_string())
                }
            });

            info!("Initializing project '{}' at {}", project_name, cwd_str);
            let db = Db::open_default()?;

            // Check if already initialized
            let project_id = if let Some(existing) = db.get_project_by_path(&cwd_str)? {
                // Update name if differs
                if existing.name != project_name {
                    db.rename_project(existing.id, &project_name)?;
                    print_success(format!(
                        "Renamed project '{}' -> '{}' (id={})",
                        existing.name, project_name, existing.id
                    ));
                } else {
                    print_info(format!(
                        "Project '{}' already initialized (id={})",
                        existing.name, existing.id
                    ));
                }
                existing.id
            } else {
                let id = db.create_project(&project_name, &cwd_str)?;
                print_success(format!(
                    "Initialized project '{}' (id={})",
                    project_name, id
                ));
                print_info(format!("Path: {}", cwd_str));
                id
            };

            // --group is additive to .json groups
            if let Some(group_name) = group {
                let group_id = db.get_or_create_group(&group_name)?;
                db.add_project_to_group(project_id, group_id)?;
                print_success(format!("Joined group '{}'", group_name));
            }

            // Sync from config if present
            if let Some(ref cfg) = config {
                let report = db.sync_from_config(project_id, cfg)?;
                let total = report.groups_added
                    + report.groups_removed
                    + report.shares_added
                    + report.shares_removed
                    + report.notes_added
                    + report.notes_removed
                    + report.notes_updated;
                if total > 0 {
                    print_success(format!(
                        "Applied .ai-workspace.json: groups +{} -{}, shares +{} -{}, notes +{} -{} ~{}",
                        report.groups_added,
                        report.groups_removed,
                        report.shares_added,
                        report.shares_removed,
                        report.notes_added,
                        report.notes_removed,
                        report.notes_updated,
                    ));
                } else {
                    print_info("Config already in sync with database.");
                }
            }

            Ok(())
        }

        Command::Share { path, label } => {
            let db = Db::open_default()?;
            let project = require_project(&db)?;

            let project_root = Path::new(&project.path);
            let full_path = project_root.join(&path);

            // Verify the path exists and is within the project root
            let canonical = full_path
                .canonicalize()
                .map_err(|_| anyhow::anyhow!("Path not found: {}", full_path.display()))?;
            let canonical_root = project_root.canonicalize()?;
            if !canonical.starts_with(&canonical_root) {
                bail!("Path is outside project directory");
            }

            if canonical.is_dir() {
                info!("Sharing directory: {}", path);
                let id = db.share_dir(project.id, &path, label.as_deref())?;
                print_success(format!("Shared dir '{}' (id={})", path, id));
            } else {
                let id = db.share_file(project.id, &path, label.as_deref())?;
                print_success(format!("Shared '{}' (id={})", path, id));
            }

            update_workspace_json_if_exists(&db, &project)?;
            Ok(())
        }

        Command::Note {
            content,
            label,
            scope,
            group,
        } => {
            let db = Db::open_default()?;
            let project = require_project(&db)?;

            let is_project_scope = matches!(scope, NoteScope::Project);
            match scope {
                NoteScope::Project => {
                    info!("Creating project note for project_id={}", project.id);
                    let id = db.add_project_note(project.id, &content, label.as_deref())?;
                    print_success(format!("Added project note (id={})", id));
                }
                NoteScope::Group => {
                    let group = if let Some(group_name) = group {
                        db.get_group_by_name(&group_name)?
                            .ok_or_else(|| anyhow::anyhow!("Group '{}' not found", group_name))?
                    } else {
                        let groups = db.get_groups_for_project(project.id)?;
                        match groups.len() {
                            0 => bail!(
                                "Project is not in any group. Use --scope project or join a group first."
                            ),
                            1 => groups.into_iter().next().unwrap(),
                            _ => bail!(
                                "Project is in multiple groups. Specify --group: {}",
                                groups
                                    .iter()
                                    .map(|g| g.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        }
                    };
                    let id = db.add_group_note(group.id, project.id, &content, label.as_deref())?;
                    print_success(format!("Added note (id={}) to group '{}'", id, group.name));
                }
            }
            // Only project-scoped notes update .json (group notes do NOT)
            if is_project_scope {
                update_workspace_json_if_exists(&db, &project)?;
            }
            Ok(())
        }

        Command::Edit {
            target,
            content,
            label,
            scope,
            group,
        } => {
            let db = Db::open_default()?;
            let project = require_project(&db)?;

            if content.is_none() && label.is_none() && scope.is_none() {
                bail!("Nothing to edit. Provide --content, --label, or --scope.");
            }

            let item = db
                .resolve_item_for_project(&target, project.id)?
                .ok_or_else(|| anyhow::anyhow!("Item '{}' not found", target))?;

            // Content and scope changes only for notes
            if item.kind != crate::models::SharedItemKind::Note
                && (content.is_some() || scope.is_some())
            {
                bail!("Only notes support --content and --scope. Use --label for files/dirs.");
            }

            let scope_change = match scope {
                Some(NoteScope::Group) => {
                    let g = if let Some(group_name) = group {
                        db.get_group_by_name(&group_name)?
                            .ok_or_else(|| anyhow::anyhow!("Group '{}' not found", group_name))?
                    } else {
                        let groups = db.get_groups_for_project(project.id)?;
                        match groups.len() {
                            0 => bail!(
                                "Project is not in any group. Use --scope project or join a group first."
                            ),
                            1 => groups.into_iter().next().unwrap(),
                            _ => bail!(
                                "Project is in multiple groups. Specify --group: {}",
                                groups
                                    .iter()
                                    .map(|g| g.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        }
                    };
                    Some(ScopeChange::ToGroup { group_id: g.id })
                }
                Some(NoteScope::Project) => Some(ScopeChange::ToProject),
                None => None,
            };

            let update = SharedItemUpdate {
                content,
                label: label.map(|l| if l.is_empty() { None } else { Some(l) }),
                scope_change,
            };

            db.update_shared_item(item.id, project.id, &update)?;
            print_success(format!("Updated item (id={})", item.id));
            update_workspace_json_if_exists(&db, &project)?;
            Ok(())
        }

        Command::Rm { target } => {
            let db = Db::open_default()?;
            let project = require_project(&db)?;

            // Try as numeric ID first (scoped to current project)
            if let Ok(id) = target.parse::<i64>()
                && db.remove_shared_item_for_project(id, project.id)?
            {
                print_success(format!("Removed item id={}", id));
                update_workspace_json_if_exists(&db, &project)?;
                return Ok(());
            }

            // Try as label
            info!("Trying rm by label: {}", target);
            if db.remove_by_label(project.id, &target)? {
                print_success(format!("Removed item with label '{}'", target));
                update_workspace_json_if_exists(&db, &project)?;
                return Ok(());
            }

            // Try as path
            info!("Trying rm by path: {}", target);
            if db.remove_by_path(project.id, &target)? {
                print_success(format!("Removed item with path '{}'", target));
                update_workspace_json_if_exists(&db, &project)?;
                return Ok(());
            }

            print_info(format!("Item '{}' not found", target));
            Ok(())
        }

        Command::Leave { group } => {
            let db = Db::open_default()?;
            let project = require_project(&db)?;
            let g = db
                .get_group_by_name(&group)?
                .ok_or_else(|| anyhow::anyhow!("Group '{}' not found", group))?;
            if db.remove_project_from_group(project.id, g.id)? {
                print_success(format!("Left group '{}'", group));
                // Check if group was auto-deleted (no members left)
                if db.get_group_by_name(&group)?.is_none() {
                    print_info(format!(
                        "Group '{}' had no members left and was deleted",
                        group
                    ));
                }
                update_workspace_json_if_exists(&db, &project)?;
            } else {
                print_info(format!("Project is not a member of group '{}'", group));
            }
            Ok(())
        }

        Command::DeleteGroup { group } => {
            let db = Db::open_default()?;
            let g = db
                .get_group_by_name(&group)?
                .ok_or_else(|| anyhow::anyhow!("Group '{}' not found", group))?;
            db.delete_group(g.id)?;
            print_success(format!("Deleted group '{}'", group));
            Ok(())
        }

        Command::List { what } => {
            let db = Db::open_default()?;

            let show_projects = matches!(what, ListTarget::All | ListTarget::Projects);
            let show_groups = matches!(what, ListTarget::All | ListTarget::Groups);

            if show_projects {
                let projects = db.list_projects()?;
                if projects.is_empty() {
                    println!("Projects: (none)");
                } else {
                    print_section("Projects:");
                    let mut rows = Vec::with_capacity(projects.len());
                    for p in projects {
                        let groups = db.get_groups_for_project(p.id)?;
                        let group_names = if groups.is_empty() {
                            "-".to_string()
                        } else {
                            groups
                                .iter()
                                .map(|g| g.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        };
                        rows.push(vec![
                            p.id.to_string(),
                            p.name,
                            truncate_for_cell(&p.path, 56),
                            truncate_for_cell(&group_names, 30),
                        ]);
                    }
                    print_table(&["ID", "Project", "Path", "Groups"], &rows);
                }
            }

            if show_projects && show_groups {
                println!();
            }

            if show_groups {
                let groups = db.list_groups()?;
                if groups.is_empty() {
                    println!("Groups: (none)");
                } else {
                    print_section("Groups:");
                    let mut rows = Vec::with_capacity(groups.len());
                    for g in groups {
                        let members = db.get_projects_for_group(g.id)?;
                        let member_names = if members.is_empty() {
                            "no members".to_string()
                        } else {
                            members
                                .iter()
                                .map(|p| p.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        };
                        rows.push(vec![
                            g.id.to_string(),
                            g.name,
                            truncate_for_cell(&member_names, 48),
                        ]);
                    }
                    print_table(&["ID", "Group", "Members"], &rows);
                }
            }

            Ok(())
        }

        Command::Status => {
            let db = Db::open_default()?;
            let project = require_project(&db)?;
            println!("Project: {} (id={})", project.name, project.id);
            println!("Path: {}", project.path);

            let groups = db.get_groups_for_project(project.id)?;
            if groups.is_empty() {
                println!("Groups: (none)");
            } else {
                print_section("Groups:");
                let rows = groups
                    .iter()
                    .map(|g| vec![g.id.to_string(), g.name.clone()])
                    .collect::<Vec<_>>();
                print_table(&["ID", "Group"], &rows);
            }

            let items = db.get_shared_items_for_project(project.id)?;
            if items.is_empty() {
                println!("Shared items: (none)");
            } else {
                print_section("Shared items:");
                let mut rows = Vec::with_capacity(items.len());
                for item in &items {
                    let value = match item.kind {
                        crate::models::SharedItemKind::Note => {
                            item.content.as_deref().unwrap_or("").to_string()
                        }
                        _ => item.path.as_deref().unwrap_or("?").to_string(),
                    };
                    rows.push(vec![
                        item.id.to_string(),
                        item.kind.to_string(),
                        item.label.as_deref().unwrap_or("-").to_string(),
                        truncate_for_cell(&value, 80),
                    ]);
                }
                print_table(&["ID", "Kind", "Label", "Value"], &rows);
            }

            // Collect own paths and note contents for dedup in group view
            let own_paths: HashSet<String> = items.iter().filter_map(|i| i.path.clone()).collect();
            let own_note_contents: HashSet<String> =
                items.iter().filter_map(|i| i.content.clone()).collect();

            // Show items shared within each group (excluding own and duplicates)
            for g in &groups {
                let group_items = db.get_all_items_for_group(g.id)?;

                // Filter out own items and dedup by path/content
                let mut seen_paths = HashSet::new();
                let mut seen_contents = HashSet::new();
                let filtered: Vec<_> = group_items
                    .iter()
                    .filter(|item| {
                        // Skip own items — already shown in "Shared items" above
                        if item.project_id == Some(project.id) {
                            return false;
                        }
                        // Skip files/dirs whose path matches an own item
                        if let Some(ref path) = item.path {
                            if own_paths.contains(path) {
                                return false;
                            }
                            if !seen_paths.insert(path.clone()) {
                                return false;
                            }
                        }
                        // Dedup notes by content
                        if let Some(ref content) = item.content {
                            if own_note_contents.contains(content) {
                                return false;
                            }
                            if !seen_contents.insert(content.clone()) {
                                return false;
                            }
                        }
                        true
                    })
                    .collect();

                if filtered.is_empty() {
                    println!("Group '{}' shared items: (none)", g.name);
                } else {
                    print_section(format!("Group '{}' shared items:", g.name));
                    let mut rows = Vec::with_capacity(filtered.len());
                    for item in &filtered {
                        let source = if let Some(pid) = item.project_id {
                            match db.get_project_by_id(pid) {
                                Ok(Some(p)) => p.name,
                                _ => format!("project={}", pid),
                            }
                        } else if let Some(cpid) = item.created_by_project_id {
                            match db.get_project_by_id(cpid) {
                                Ok(Some(p)) => p.name,
                                _ => format!("project={}", cpid),
                            }
                        } else {
                            "-".to_string()
                        };
                        let value = match item.kind {
                            crate::models::SharedItemKind::Note => {
                                item.content.as_deref().unwrap_or("").to_string()
                            }
                            _ => item.path.as_deref().unwrap_or("?").to_string(),
                        };
                        rows.push(vec![
                            item.id.to_string(),
                            item.kind.to_string(),
                            item.label.as_deref().unwrap_or("-").to_string(),
                            truncate_for_cell(&value, 72),
                            truncate_for_cell(&source, 28),
                        ]);
                    }
                    print_table(&["ID", "Kind", "Label", "Value", "Source"], &rows);
                }
            }

            Ok(())
        }

        Command::Export => {
            let db = Db::open_default()?;
            let project = require_project(&db)?;
            let config = db.export_project_config(project.id)?;
            let config_path = Path::new(&project.path).join(".ai-workspace.json");
            config.save(&config_path)?;
            print_success(format!("Exported config to {}", config_path.display()));
            Ok(())
        }

        Command::Sync => {
            let db = Db::open_default()?;

            // Step 1: sync files (always, no project needed)
            let removed = db.sync_files()?;
            if removed.is_empty() {
                print_success("All shared files are up to date.");
            } else {
                print_success(format!("Removed {} stale entries:", removed.len()));
                let rows = removed
                    .iter()
                    .map(|(id, path)| vec![id.to_string(), truncate_for_cell(path, 80)])
                    .collect::<Vec<_>>();
                print_table(&["ID", "Path"], &rows);
            }

            // Step 2: sync from .ai-workspace.json if present
            let cwd = env::current_dir()?;
            let cwd_str = cwd.to_string_lossy();
            if let Some(project) = db.find_project_by_cwd(&cwd_str)? {
                let config_path = Path::new(&project.path).join(".ai-workspace.json");
                if config_path.exists() {
                    info!("Found .ai-workspace.json, syncing config");
                    let config = crate::models::WorkspaceConfig::load(&config_path)?;
                    let report = db.sync_from_config(project.id, &config)?;
                    let total = report.groups_added
                        + report.groups_removed
                        + report.shares_added
                        + report.shares_removed
                        + report.notes_added
                        + report.notes_removed
                        + report.notes_updated;
                    if total > 0 {
                        print_success(format!(
                            "Config sync: groups +{} -{}, shares +{} -{}, notes +{} -{} ~{}",
                            report.groups_added,
                            report.groups_removed,
                            report.shares_added,
                            report.shares_removed,
                            report.notes_added,
                            report.notes_removed,
                            report.notes_updated,
                        ));
                    } else {
                        print_success("Config is in sync with database.");
                    }
                }
            }

            Ok(())
        }
    }
}
