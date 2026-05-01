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
    /// Remove current project from ai-workspace (keeps files on disk)
    Destroy,
    /// Show project status
    Status,
    /// Export project config to .ai-workspace.json
    Export,
    /// Sync: verify shared files/dirs exist, clean up stale entries, reconcile .ai-workspace.json
    Sync,
    /// Start MCP server (stdio transport)
    Serve,
    /// Update ai-workspace to the latest version
    Update,
    /// Full-text search across indexed .md files
    Search {
        /// FTS5 query (supports phrase "..." and operators AND/OR/NOT)
        query: String,
        /// Max number of results
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },
    /// Rebuild the full-text index for all shared .md files
    Reindex,
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
    if matches!(env::var("NO_COLOR"), Ok(value) if !value.is_empty()) {
        return false;
    }
    if matches!(env::var("CLICOLOR"), Ok(value) if value == "0") {
        return false;
    }
    if matches!(env::var("CLICOLOR_FORCE"), Ok(value) if value != "0") {
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

/// Key files to auto-share on init (when no .ai-workspace.json exists).
const AUTO_SHARE_FILES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "composer.json",
    "Makefile",
    "Taskfile.yml",
    "Justfile",
];

/// Prefixes for key files that may have varying names (e.g. README.md, README.txt).
const AUTO_SHARE_PREFIXES: &[&str] = &["README"];

/// Auto-share key project files on init. Returns the count of files shared.
fn auto_share_key_files(db: &Db, project_id: i64, project_dir: &Path) -> Result<usize> {
    debug!(
        "auto_share_key_files: project_id={}, dir={}",
        project_id,
        project_dir.display()
    );

    // Get already-shared paths to avoid duplicates
    let existing_items = db.get_shared_items_for_project(project_id)?;
    let existing_paths: HashSet<String> = existing_items
        .iter()
        .filter_map(|i| i.path.clone())
        .collect();

    let mut count = 0;

    // Check exact-name files
    for &filename in AUTO_SHARE_FILES {
        if existing_paths.contains(filename) {
            debug!("auto_share: skipping already shared {}", filename);
            continue;
        }
        let full = project_dir.join(filename);
        if full.exists() && full.is_file() {
            info!("auto_share: sharing {}", filename);
            db.share_file(project_id, filename, None)?;
            count += 1;
        }
    }

    // Check prefix-matched files (e.g. README*)
    if let Ok(entries) = std::fs::read_dir(project_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_string();
            if !entry.path().is_file() {
                continue;
            }
            for &prefix in AUTO_SHARE_PREFIXES {
                if name.starts_with(prefix) && !existing_paths.contains(&name) {
                    info!("auto_share: sharing {}", name);
                    db.share_file(project_id, &name, None)?;
                    count += 1;
                    break;
                }
            }
        }
    }

    Ok(count)
}

pub fn run(cmd: Command) -> Result<()> {
    match cmd {
        Command::Serve => crate::mcp::serve(),

        Command::Init { name, group } => {
            let cwd = env::current_dir()?;
            let cwd_str = cwd.to_string_lossy().to_string();

            // Check for .ai-workspace.json
            let config_path = cwd.join(".ai-workspace.json");
            let mut config = if config_path.exists() {
                info!("Found .ai-workspace.json at {}", config_path.display());
                Some(crate::models::WorkspaceConfig::load(&config_path)?)
            } else {
                None
            };

            // Resolve project name: --name flag, then .json config, then dir name
            let name_from_flag = name.is_some();
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
                // Only rename if --name was explicitly provided
                if name_from_flag && existing.name != project_name {
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

            let mut config_changed_by_cli_group = false;

            // --group is additive to .json groups
            if let Some(group_name) = group {
                let group_id = db.get_or_create_group(&group_name)?;
                db.add_project_to_group(project_id, group_id)?;
                print_success(format!("Joined group '{}'", group_name));

                match config.as_mut() {
                    Some(cfg) if !cfg.groups.contains(&group_name) => {
                        debug!(
                            "Adding CLI group '{}' to loaded .ai-workspace.json config",
                            group_name
                        );
                        cfg.groups.push(group_name);
                        config_changed_by_cli_group = true;
                    }
                    _ => {}
                }
            }

            // Auto-share key files when NO .ai-workspace.json exists
            if config.is_none() {
                let auto_shared = auto_share_key_files(&db, project_id, &cwd)?;
                if auto_shared > 0 {
                    print_success(format!("Auto-shared {} key file(s)", auto_shared));
                }
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

                if config_changed_by_cli_group {
                    cfg.save(&config_path)?;
                    info!(
                        "Updated .ai-workspace.json with CLI group at {}",
                        config_path.display()
                    );
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

            let share_id = if canonical.is_dir() {
                info!("Sharing directory: {}", path);
                let id = db.share_dir(project.id, &path, label.as_deref())?;
                print_success(format!("Shared dir '{}' (id={})", path, id));
                id
            } else {
                let id = db.share_file(project.id, &path, label.as_deref())?;
                print_success(format!("Shared '{}' (id={})", path, id));
                id
            };

            // Index markdown content so it becomes searchable immediately.
            if let Some(item) = db.get_item_by_id(share_id)? {
                let project_root = Path::new(&project.path);
                match crate::indexer::index_shared_item(&db, &item, project_root) {
                    Ok(stats) if stats.indexed > 0 => {
                        print_info(format!("Indexed {} .md file(s) for search", stats.indexed));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        log::warn!("FTS indexing failed for id={}: {}", share_id, e);
                    }
                }
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
            if let Ok(id) = target.parse::<i64>() {
                let removed = db.remove_shared_item_for_project(id, project.id)?;
                if removed {
                    print_success(format!("Removed item id={}", id));
                    update_workspace_json_if_exists(&db, &project)?;
                    return Ok(());
                }
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

        Command::Destroy => {
            let db = Db::open_default()?;
            let project = require_project(&db)?;
            db.delete_project(project.id)?;
            print_success(format!(
                "Removed project '{}' from ai-workspace (files on disk are untouched)",
                project.name
            ));
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

        Command::Update => {
            let current = env!("CARGO_PKG_VERSION");
            println!("Current version: v{}", current);
            println!("Checking for updates...");

            let status = self_update::backends::github::Update::configure()
                .repo_owner("lee-to")
                .repo_name("ai-workspace")
                .bin_name("ai-workspace")
                .current_version(current)
                .build()?
                .update()?;

            if status.updated() {
                print_success(format!("Updated to v{}", status.version()));
            } else {
                print_info(format!("Already up to date (v{})", current));
            }

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

        Command::Search { query, limit } => {
            let db = Db::open_default()?;
            info!("CLI search: query='{}' limit={}", query, limit);

            // Best-effort lazy refresh so on-disk edits surface in results.
            let refreshed = crate::indexer::refresh_stale(&db, 200).unwrap_or_else(|e| {
                log::warn!("refresh_stale failed: {}", e);
                0
            });
            if refreshed > 0 {
                debug!("Refreshed {} stale files before search", refreshed);
            }

            let hits = db.search_files(&query, limit)?;
            if hits.is_empty() {
                print_info(format!("No matches for '{}'", query));
                return Ok(());
            }
            for hit in &hits {
                println!(
                    "{}  [id={} rank={:.3}]\n  {}\n",
                    style_ok(&hit.path),
                    hit.shared_item_id,
                    hit.rank,
                    hit.snippet.trim()
                );
            }
            print_info(format!("{} result(s)", hits.len()));
            Ok(())
        }

        Command::Reindex => {
            let db = Db::open_default()?;
            info!("CLI reindex: starting");
            let stats = crate::indexer::reindex_all(&db)?;
            print_success(format!(
                "Reindex complete: {} files indexed, {} skipped (size), {} skipped (non-utf8), {} missing",
                stats.indexed, stats.skipped_size, stats.skipped_non_utf8, stats.skipped_missing
            ));
            Ok(())
        }
    }
}
