use anyhow::{Result, bail};
use clap::{Subcommand, ValueEnum};
use log::{debug, info};
use std::collections::HashSet;
use std::env;
use std::io::IsTerminal;
use std::path::Path;

use crate::db::{AmbiguousItemLabel, Db, ScopeChange, SharedItemUpdate};
use crate::models::{
    ArtifactDependencyKind, ArtifactReaction, EventSeverity, EventStatus, ServiceLinkKind,
    SharedItem, SharedItemKind, WorkspaceEventKind,
};

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

#[derive(Debug, Clone, ValueEnum)]
pub enum CliServiceLinkKind {
    #[value(name = "depends_on")]
    DependsOn,
    #[value(name = "related_to")]
    RelatedTo,
}

impl From<CliServiceLinkKind> for ServiceLinkKind {
    fn from(kind: CliServiceLinkKind) -> Self {
        match kind {
            CliServiceLinkKind::DependsOn => ServiceLinkKind::DependsOn,
            CliServiceLinkKind::RelatedTo => ServiceLinkKind::RelatedTo,
        }
    }
}

#[derive(Debug, Clone, ValueEnum)]
pub enum CliArtifactDependencyKind {
    #[value(name = "references")]
    References,
    #[value(name = "consumes_api")]
    ConsumesApi,
    #[value(name = "documents")]
    Documents,
    #[value(name = "configures")]
    Configures,
}

impl From<CliArtifactDependencyKind> for ArtifactDependencyKind {
    fn from(kind: CliArtifactDependencyKind) -> Self {
        match kind {
            CliArtifactDependencyKind::References => ArtifactDependencyKind::References,
            CliArtifactDependencyKind::ConsumesApi => ArtifactDependencyKind::ConsumesApi,
            CliArtifactDependencyKind::Documents => ArtifactDependencyKind::Documents,
            CliArtifactDependencyKind::Configures => ArtifactDependencyKind::Configures,
        }
    }
}

#[derive(Debug, Clone, ValueEnum)]
pub enum CliArtifactReaction {
    #[value(name = "inspect")]
    Inspect,
    #[value(name = "update")]
    Update,
    #[value(name = "delete")]
    Delete,
    #[value(name = "remove_reference")]
    RemoveReference,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum CliWorkspaceEventKind {
    #[value(name = "service_deleted")]
    ServiceDeleted,
    #[value(name = "service_changed")]
    ServiceChanged,
    #[value(name = "artifact_changed")]
    ArtifactChanged,
}

impl From<CliWorkspaceEventKind> for WorkspaceEventKind {
    fn from(kind: CliWorkspaceEventKind) -> Self {
        match kind {
            CliWorkspaceEventKind::ServiceDeleted => WorkspaceEventKind::ServiceDeleted,
            CliWorkspaceEventKind::ServiceChanged => WorkspaceEventKind::ServiceChanged,
            CliWorkspaceEventKind::ArtifactChanged => WorkspaceEventKind::ArtifactChanged,
        }
    }
}

#[derive(Debug, Clone, ValueEnum)]
pub enum CliEventSeverity {
    #[value(name = "info")]
    Info,
    #[value(name = "warning")]
    Warning,
    #[value(name = "error")]
    Error,
    #[value(name = "critical")]
    Critical,
}

impl From<CliEventSeverity> for EventSeverity {
    fn from(severity: CliEventSeverity) -> Self {
        match severity {
            CliEventSeverity::Info => EventSeverity::Info,
            CliEventSeverity::Warning => EventSeverity::Warning,
            CliEventSeverity::Error => EventSeverity::Error,
            CliEventSeverity::Critical => EventSeverity::Critical,
        }
    }
}

#[derive(Debug, Clone, ValueEnum)]
pub enum CliEventStatus {
    #[value(name = "open")]
    Open,
    #[value(name = "closed")]
    Closed,
}

impl From<CliEventStatus> for EventStatus {
    fn from(status: CliEventStatus) -> Self {
        match status {
            CliEventStatus::Open => EventStatus::Open,
            CliEventStatus::Closed => EventStatus::Closed,
        }
    }
}

impl From<CliArtifactReaction> for ArtifactReaction {
    fn from(reaction: CliArtifactReaction) -> Self {
        match reaction {
            CliArtifactReaction::Inspect => ArtifactReaction::Inspect,
            CliArtifactReaction::Update => ArtifactReaction::Update,
            CliArtifactReaction::Delete => ArtifactReaction::Delete,
            CliArtifactReaction::RemoveReference => ArtifactReaction::RemoveReference,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum LinkCommand {
    /// Add a directional service link
    Add {
        /// Source project id, slug, or registered path
        from: String,
        /// Target project id, slug, or registered path
        to: String,
        /// Link kind
        #[arg(long, value_enum)]
        kind: CliServiceLinkKind,
        /// Optional human-readable label
        #[arg(long)]
        label: Option<String>,
    },
    /// List service links
    List {
        /// Project id, slug, or registered path to show incoming/outgoing links for
        #[arg(long)]
        project: Option<String>,
    },
    /// Remove a service link by id
    Rm {
        /// Service link id
        id: i64,
    },
}

#[derive(Debug, Subcommand)]
pub enum ArtifactCommand {
    /// Mark a shared file or directory as depending on a service
    Depends {
        /// Shared item id, label, or path
        item: String,
        /// Service slug, project id, or registered path
        service_slug: String,
        /// Dependency kind
        #[arg(long, value_enum)]
        kind: CliArtifactDependencyKind,
        /// Suggested reaction when the dependency source changes
        #[arg(long, value_enum)]
        reaction: CliArtifactReaction,
    },
    /// List artifact dependencies for the current project or one item
    Deps {
        /// Optional shared item id, label, or path
        item: Option<String>,
    },
    /// Remove an artifact dependency
    Undepend {
        /// Shared item id, label, or path
        item: String,
        /// Service slug, project id, or registered path
        service_slug: String,
        /// Optional dependency kind filter
        #[arg(long, value_enum)]
        kind: Option<CliArtifactDependencyKind>,
    },
}

#[derive(Debug, Subcommand)]
pub enum EventCommand {
    /// Create a workspace event and calculate its impact
    Create {
        /// Event kind
        #[arg(long, value_enum)]
        kind: CliWorkspaceEventKind,
        /// Source service slug, project id, or registered path
        #[arg(long)]
        source: String,
        /// Event severity
        #[arg(long, value_enum, default_value = "info")]
        severity: CliEventSeverity,
        /// Optional title
        #[arg(long)]
        title: Option<String>,
        /// Optional body text
        #[arg(long)]
        body: Option<String>,
    },
    /// Show open events affecting the current project
    Inbox,
    /// List workspace events
    List {
        /// Source service slug filter
        #[arg(long)]
        source: Option<String>,
        /// Event status filter
        #[arg(long, value_enum)]
        status: Option<CliEventStatus>,
    },
    /// Show event details
    Show {
        /// Event id
        id: i64,
    },
    /// Close an event and resolve its targets/artifacts
    Close {
        /// Event id
        id: i64,
    },
    /// Physically remove an event
    Rm {
        /// Event id
        id: i64,
    },
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize current directory as a project
    Init {
        /// Project name (defaults to directory name)
        #[arg(short, long)]
        name: Option<String>,
        /// Stable service slug (defaults to a normalized project name)
        #[arg(long)]
        slug: Option<String>,
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
    /// Manage directional service links between projects
    Link {
        #[command(subcommand)]
        command: LinkCommand,
    },
    /// Manage dependencies from shared artifacts to services
    Artifact {
        #[command(subcommand)]
        command: ArtifactCommand,
    },
    /// Manage workspace service events
    Event {
        #[command(subcommand)]
        command: EventCommand,
    },
    /// Remove a project from ai-workspace (keeps files on disk)
    Destroy {
        /// Project ID or registered path to remove (defaults to current project)
        target: Option<String>,
        /// Project ID or registered path to remove (alternative flag form)
        #[arg(long = "target", value_name = "PROJECT")]
        target_flag: Option<String>,
    },
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

fn project_display_slug(db: &Db, project_id: i64) -> Result<String> {
    Ok(db
        .get_project_by_id(project_id)?
        .map(|project| project.slug)
        .unwrap_or_else(|| format!("project={project_id}")))
}

fn print_service_links(db: &Db, links: &[crate::models::ServiceLink]) -> Result<()> {
    if links.is_empty() {
        println!("Service links: (none)");
        return Ok(());
    }

    let mut rows = Vec::with_capacity(links.len());
    for link in links {
        rows.push(vec![
            link.id.to_string(),
            project_display_slug(db, link.from_project_id)?,
            project_display_slug(db, link.to_project_id)?,
            link.kind.to_string(),
            link.label.as_deref().unwrap_or("-").to_string(),
        ]);
    }
    print_table(&["ID", "From", "To", "Kind", "Label"], &rows);
    Ok(())
}

fn dependency_item_label(db: &Db, shared_item_id: i64) -> Result<String> {
    Ok(db
        .get_item_by_id(shared_item_id)?
        .and_then(|item| item.path.or(item.label))
        .unwrap_or_else(|| format!("item={shared_item_id}")))
}

fn dependency_summary(deps: &[crate::models::ArtifactDependency]) -> String {
    if deps.is_empty() {
        return "-".to_string();
    }

    let values = deps
        .iter()
        .map(|dep| {
            format!(
                "{}:{}:{}",
                dep.depends_on_project_slug_snapshot, dep.kind, dep.reaction
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    truncate_for_cell(&values, 56)
}

fn print_artifact_dependencies(db: &Db, deps: &[crate::models::ArtifactDependency]) -> Result<()> {
    if deps.is_empty() {
        println!("Artifact dependencies: (none)");
        return Ok(());
    }

    let mut rows = Vec::with_capacity(deps.len());
    for dep in deps {
        rows.push(vec![
            dep.id.to_string(),
            dependency_item_label(db, dep.shared_item_id)?,
            dep.depends_on_project_slug_snapshot.clone(),
            dep.kind.to_string(),
            dep.reaction.to_string(),
        ]);
    }
    print_table(&["ID", "Item", "Service", "Kind", "Reaction"], &rows);
    Ok(())
}

fn shared_item_value(item: &SharedItem) -> String {
    match item.kind {
        SharedItemKind::Note => item.content.as_deref().unwrap_or("").to_string(),
        SharedItemKind::File | SharedItemKind::Dir => {
            item.path.as_deref().unwrap_or("?").to_string()
        }
    }
}

fn normalize_table_cell(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn shared_item_scope(db: &Db, item: &SharedItem) -> Result<String> {
    if item.project_id.is_some() {
        return Ok("project".to_string());
    }

    if let Some(group_id) = item.group_id {
        let group = db
            .get_group_by_id(group_id)?
            .map(|group| group.name)
            .unwrap_or_else(|| format!("group={group_id}"));
        return Ok(format!("group:{group}"));
    }

    Ok("-".to_string())
}

fn shared_item_source(db: &Db, item: &SharedItem) -> Result<String> {
    if let Some(project_id) = item.project_id.or(item.created_by_project_id) {
        return project_display_slug(db, project_id);
    }

    Ok("-".to_string())
}

fn print_ambiguous_label_candidates(db: &Db, ambiguous: &AmbiguousItemLabel) -> Result<()> {
    let mut rows = Vec::with_capacity(ambiguous.matches.len());
    for item in &ambiguous.matches {
        rows.push(vec![
            item.id.to_string(),
            item.kind.to_string(),
            normalize_table_cell(item.label.as_deref().unwrap_or("-")),
            truncate_for_cell(&normalize_table_cell(&shared_item_value(item)), 72),
            normalize_table_cell(&shared_item_scope(db, item)?),
            normalize_table_cell(&shared_item_source(db, item)?),
        ]);
    }

    print_table(&["ID", "Kind", "Label", "Value", "Scope", "Source"], &rows);
    Ok(())
}

fn with_ambiguous_label_table<T>(db: &Db, result: Result<T>) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(err) => {
            if let Some(ambiguous) = err.downcast_ref::<AmbiguousItemLabel>()
                && let Err(render_err) = print_ambiguous_label_candidates(db, ambiguous)
            {
                log::warn!(
                    "Failed to render ambiguous label candidates: {}",
                    render_err
                );
            }
            Err(err)
        }
    }
}

fn print_workspace_events(events: &[crate::models::WorkspaceEvent]) {
    if events.is_empty() {
        println!("Events: (none)");
        return;
    }

    let rows = events
        .iter()
        .map(|event| {
            vec![
                event.id.to_string(),
                event.source_project_slug.clone(),
                event.kind.to_string(),
                event.severity.to_string(),
                event.status.to_string(),
                truncate_for_cell(&event.title, 48),
            ]
        })
        .collect::<Vec<_>>();
    print_table(
        &["ID", "Source", "Kind", "Severity", "Status", "Title"],
        &rows,
    );
}

fn print_event_details(db: &Db, event_id: i64) -> Result<()> {
    let event = db
        .get_workspace_event(event_id)?
        .ok_or_else(|| anyhow::anyhow!("Event id={} not found", event_id))?;
    println!("Event: {} (id={})", event.title, event.id);
    println!("Source: {}", event.source_project_slug);
    println!("Kind: {}", event.kind);
    println!("Severity: {}", event.severity);
    println!("Status: {}", event.status);
    if let Some(body) = event.body {
        println!("Body: {}", body);
    }

    let targets = db.list_event_targets(event.id)?;
    print_section("Affected services:");
    if targets.is_empty() {
        println!("Affected services: (none)");
    } else {
        let mut rows = Vec::with_capacity(targets.len());
        for target in targets {
            let project = target
                .affected_project_id
                .map(|id| project_display_slug(db, id))
                .transpose()?
                .unwrap_or_else(|| "-".to_string());
            rows.push(vec![
                target.id.to_string(),
                project,
                target.relation_kind.to_string(),
                target.status.to_string(),
            ]);
        }
        print_table(&["ID", "Project", "Relation", "Status"], &rows);
    }

    let artifacts = db.list_event_artifacts(event.id)?;
    print_section("Affected artifacts:");
    if artifacts.is_empty() {
        println!("Affected artifacts: (none)");
    } else {
        let rows = artifacts
            .iter()
            .map(|artifact| {
                vec![
                    artifact.id.to_string(),
                    artifact.path_snapshot.clone(),
                    artifact.reaction.to_string(),
                    artifact.status.to_string(),
                    truncate_for_cell(&artifact.reason, 48),
                ]
            })
            .collect::<Vec<_>>();
        print_table(&["ID", "Path", "Reaction", "Status", "Reason"], &rows);
    }
    Ok(())
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

        Command::Init { name, slug, group } => {
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
            let project_slug = slug.or_else(|| config.as_ref().and_then(|cfg| cfg.slug.clone()));

            info!("Initializing project '{}' at {}", project_name, cwd_str);
            let db = Db::open_default()?;

            // Check if already initialized
            let project_id = if let Some(existing) = db.get_project_by_path(&cwd_str)? {
                if let Some(slug) = project_slug.as_deref()
                    && existing.slug != crate::models::normalize_project_slug(slug)
                {
                    bail!(
                        "Project already initialized with slug '{}'. Slug changes are not supported yet.",
                        existing.slug
                    );
                }
                // Only rename if --name was explicitly provided
                if name_from_flag && existing.name != project_name {
                    db.rename_project(existing.id, &project_name)?;
                    print_success(format!(
                        "Renamed project '{}' -> '{}' (id={})",
                        existing.name, project_name, existing.id
                    ));
                } else {
                    print_info(format!(
                        "Project '{}' already initialized (id={}, slug={})",
                        existing.name, existing.id, existing.slug
                    ));
                }
                existing.id
            } else {
                let id =
                    db.create_project_with_slug(&project_name, &cwd_str, project_slug.as_deref())?;
                let project = db
                    .get_project_by_id(id)?
                    .ok_or_else(|| anyhow::anyhow!("Project {} not found after create", id))?;
                print_success(format!(
                    "Initialized project '{}' (id={}, slug={})",
                    project_name, id, project.slug
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
                    + report.dependencies_added
                    + report.dependencies_removed
                    + report.dependencies_updated
                    + report.notes_added
                    + report.notes_removed
                    + report.notes_updated;
                if total > 0 {
                    print_success(format!(
                        "Applied .ai-workspace.json: groups +{} -{}, shares +{} -{}, dependencies +{} -{} ~{}, notes +{} -{} ~{}",
                        report.groups_added,
                        report.groups_removed,
                        report.shares_added,
                        report.shares_removed,
                        report.dependencies_added,
                        report.dependencies_removed,
                        report.dependencies_updated,
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

            let item =
                with_ambiguous_label_table(&db, db.resolve_item_for_project(&target, project.id))?
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

            let item =
                with_ambiguous_label_table(&db, db.resolve_item_for_project(&target, project.id))?;

            let Some(item) = item else {
                print_info(format!("Item '{}' not found", target));
                return Ok(());
            };

            let removed = db.remove_shared_item_for_project(item.id, project.id)?;
            if !removed {
                print_info(format!("Item '{}' not found", target));
                return Ok(());
            }

            if target.parse::<i64>().ok() == Some(item.id) {
                print_success(format!("Removed item id={}", item.id));
            } else if item.label.as_deref() == Some(target.as_str()) {
                print_success(format!("Removed item with label '{}'", target));
            } else if item.path.as_deref() == Some(target.as_str()) {
                print_success(format!("Removed item with path '{}'", target));
            } else {
                print_success(format!("Removed item id={}", item.id));
            }
            update_workspace_json_if_exists(&db, &project)?;
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

        Command::Destroy {
            target,
            target_flag,
        } => {
            let db = Db::open_default()?;
            if target.is_some() && target_flag.is_some() {
                bail!("Specify either <target> or --target, not both.");
            }

            let project = match target.or(target_flag) {
                Some(target) => db
                    .resolve_project_target(&target)?
                    .ok_or_else(|| anyhow::anyhow!("Project '{}' not found", target))?,
                None => require_project(&db)?,
            };

            let event_id = db.destroy_project_with_service_deleted_event(project.id)?;
            print_success(format!(
                "Removed project '{}' from ai-workspace (files on disk are untouched, event id={})",
                project.name, event_id
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
                            p.slug,
                            truncate_for_cell(&p.path, 56),
                            truncate_for_cell(&group_names, 30),
                        ]);
                    }
                    print_table(&["ID", "Project", "Slug", "Path", "Groups"], &rows);
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
                                .map(|p| p.slug.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        };
                        rows.push(vec![
                            g.id.to_string(),
                            g.name,
                            truncate_for_cell(&member_names, 48),
                        ]);
                    }
                    print_table(&["ID", "Group", "Member slugs"], &rows);
                }
            }

            Ok(())
        }

        Command::Link { command } => {
            let db = Db::open_default()?;
            match command {
                LinkCommand::Add {
                    from,
                    to,
                    kind,
                    label,
                } => {
                    let kind = ServiceLinkKind::from(kind);
                    debug!(
                        "Parsed link add command: from='{}', to='{}', kind={}, label={:?}",
                        from, to, kind, label
                    );
                    let id = db.create_service_link(&from, &to, kind, label.as_deref())?;
                    let link = db.get_service_link_by_id(id)?.ok_or_else(|| {
                        anyhow::anyhow!("Service link id={} not found after create", id)
                    })?;
                    let from_slug = project_display_slug(&db, link.from_project_id)?;
                    let to_slug = project_display_slug(&db, link.to_project_id)?;
                    info!(
                        "Created or reused service link id={} from={} to={} kind={}",
                        id, from_slug, to_slug, link.kind
                    );
                    print_success(format!(
                        "Linked {} -> {} (id={}, kind={})",
                        from_slug, to_slug, id, link.kind
                    ));
                    Ok(())
                }
                LinkCommand::List { project } => {
                    debug!("Parsed link list command: project={:?}", project);
                    if let Some(project_target) = project {
                        let project =
                            db.resolve_project_target(&project_target)?.ok_or_else(|| {
                                anyhow::anyhow!("Project '{}' not found", project_target)
                            })?;
                        info!("Listing service links for project slug='{}'", project.slug);
                        print_section(format!("Outgoing links for {}:", project.slug));
                        let outgoing = db.list_outgoing_service_links(project.id)?;
                        print_service_links(&db, &outgoing)?;
                        println!();
                        print_section(format!("Incoming links for {}:", project.slug));
                        let incoming = db.list_incoming_service_links(project.id)?;
                        print_service_links(&db, &incoming)?;
                        return Ok(());
                    }

                    let cwd_project = env::current_dir().ok().and_then(|cwd| {
                        db.find_project_by_cwd(&cwd.to_string_lossy())
                            .ok()
                            .flatten()
                    });

                    if let Some(project) = cwd_project {
                        let groups = db.get_groups_for_project(project.id)?;
                        if groups.is_empty() {
                            info!(
                                "Listing incoming/outgoing service links for ungrouped project slug='{}'",
                                project.slug
                            );
                            let mut links = db.list_outgoing_service_links(project.id)?;
                            let mut seen = links.iter().map(|link| link.id).collect::<HashSet<_>>();
                            for link in db.list_incoming_service_links(project.id)? {
                                if seen.insert(link.id) {
                                    links.push(link);
                                }
                            }
                            print_section(format!("Service links for {}:", project.slug));
                            print_service_links(&db, &links)?;
                            return Ok(());
                        }

                        let mut links = Vec::new();
                        let mut seen = HashSet::new();
                        for group in groups {
                            debug!(
                                "Listing service graph for group id={} name='{}'",
                                group.id, group.name
                            );
                            for link in db.list_group_service_links(group.id)? {
                                if seen.insert(link.id) {
                                    links.push(link);
                                }
                            }
                        }
                        info!(
                            "Listed {} service links from current project group graph",
                            links.len()
                        );
                        print_section(format!("Service graph for {}:", project.slug));
                        print_service_links(&db, &links)?;
                    } else {
                        info!("Listing all service links outside a project context");
                        let links = db.list_service_links()?;
                        print_section("Service links:");
                        print_service_links(&db, &links)?;
                    }
                    Ok(())
                }
                LinkCommand::Rm { id } => {
                    debug!("Parsed link rm command: id={}", id);
                    match db.delete_service_link_by_id(id)? {
                        Some(link) => {
                            let from_slug = project_display_slug(&db, link.from_project_id)?;
                            let to_slug = project_display_slug(&db, link.to_project_id)?;
                            print_success(format!(
                                "Removed link {} -> {} ({}, id={})",
                                from_slug, to_slug, link.kind, id
                            ));
                        }
                        None => print_info(format!("Service link id={} not found", id)),
                    }
                    Ok(())
                }
            }
        }

        Command::Artifact { command } => {
            let db = Db::open_default()?;
            let project = require_project(&db)?;
            match command {
                ArtifactCommand::Depends {
                    item,
                    service_slug,
                    kind,
                    reaction,
                } => {
                    let kind = ArtifactDependencyKind::from(kind);
                    let reaction = ArtifactReaction::from(reaction);
                    debug!(
                        "Parsed artifact depends command: item='{}', service='{}', kind={}, reaction={}",
                        item, service_slug, kind, reaction
                    );
                    let id = with_ambiguous_label_table(
                        &db,
                        db.add_artifact_dependency(
                            project.id,
                            &item,
                            &service_slug,
                            kind,
                            reaction,
                        ),
                    )?;
                    info!(
                        "Added artifact dependency id={} project_slug='{}' item='{}' service='{}'",
                        id, project.slug, item, service_slug
                    );
                    print_success(format!(
                        "Marked '{}' as depending on '{}' (id={}, kind={}, reaction={})",
                        item, service_slug, id, kind, reaction
                    ));
                    Ok(())
                }
                ArtifactCommand::Deps { item } => {
                    debug!("Parsed artifact deps command: item={:?}", item);
                    let deps = if let Some(item_target) = item {
                        info!(
                            "Listing artifact dependencies for project_slug='{}' item='{}'",
                            project.slug, item_target
                        );
                        with_ambiguous_label_table(
                            &db,
                            db.list_artifact_dependencies_for_item(project.id, &item_target),
                        )?
                    } else {
                        info!(
                            "Listing artifact dependencies for project_slug='{}'",
                            project.slug
                        );
                        db.list_artifact_dependencies_for_project(project.id)?
                    };
                    print_artifact_dependencies(&db, &deps)?;
                    Ok(())
                }
                ArtifactCommand::Undepend {
                    item,
                    service_slug,
                    kind,
                } => {
                    let kind = kind.map(ArtifactDependencyKind::from);
                    debug!(
                        "Parsed artifact undepend command: item='{}', service='{}', kind={:?}",
                        item, service_slug, kind
                    );
                    let removed = with_ambiguous_label_table(
                        &db,
                        db.remove_artifact_dependency(project.id, &item, &service_slug, kind),
                    )?;
                    if removed == 0 {
                        print_info(format!(
                            "No artifact dependencies removed for '{}' -> '{}'",
                            item, service_slug
                        ));
                    } else {
                        print_success(format!(
                            "Removed {} artifact dependenc{} for '{}' -> '{}'",
                            removed,
                            if removed == 1 { "y" } else { "ies" },
                            item,
                            service_slug
                        ));
                    }
                    Ok(())
                }
            }
        }

        Command::Event { command } => {
            let db = Db::open_default()?;
            match command {
                EventCommand::Create {
                    kind,
                    source,
                    severity,
                    title,
                    body,
                } => {
                    let kind = WorkspaceEventKind::from(kind);
                    let severity = EventSeverity::from(severity);
                    let title = title.unwrap_or_else(|| format!("{} event from {}", kind, source));
                    debug!(
                        "Parsed event create command: kind={}, source='{}', severity={}",
                        kind, source, severity
                    );
                    let id = db.create_workspace_event(
                        &source,
                        kind,
                        severity,
                        &title,
                        body.as_deref(),
                    )?;
                    info!(
                        "Created workspace event id={} source='{}' kind={}",
                        id, source, kind
                    );
                    print_success(format!("Created event '{}' (id={})", title, id));
                    print_event_details(&db, id)?;
                    Ok(())
                }
                EventCommand::Inbox => {
                    let project = require_project(&db)?;
                    debug!(
                        "Parsed event inbox command for project_slug='{}'",
                        project.slug
                    );
                    let events = db.list_workspace_event_inbox(project.id)?;
                    info!(
                        "Listed {} inbox events for project_slug='{}'",
                        events.len(),
                        project.slug
                    );
                    print_workspace_events(&events);
                    Ok(())
                }
                EventCommand::List { source, status } => {
                    let status = status.map(EventStatus::from);
                    debug!(
                        "Parsed event list command: source={:?}, status={:?}",
                        source, status
                    );
                    let events = db.list_workspace_events(source.as_deref(), status)?;
                    info!("Listed {} workspace events", events.len());
                    print_workspace_events(&events);
                    Ok(())
                }
                EventCommand::Show { id } => {
                    debug!("Parsed event show command: id={}", id);
                    print_event_details(&db, id)
                }
                EventCommand::Close { id } => {
                    debug!("Parsed event close command: id={}", id);
                    if db.close_workspace_event(id)? {
                        print_success(format!("Closed event id={}", id));
                    } else {
                        print_info(format!("Event id={} was not open or was not found", id));
                    }
                    Ok(())
                }
                EventCommand::Rm { id } => {
                    debug!("Parsed event rm command: id={}", id);
                    if db.remove_workspace_event(id)? {
                        print_success(format!("Removed event id={}", id));
                    } else {
                        print_info(format!("Event id={} not found", id));
                    }
                    Ok(())
                }
            }
        }

        Command::Status => {
            let db = Db::open_default()?;
            let project = require_project(&db)?;
            println!("Project: {} (id={})", project.name, project.id);
            println!("Slug: {}", project.slug);
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
                let deps = db.list_artifact_dependencies_for_project(project.id)?;
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
                        dependency_summary(
                            &deps
                                .iter()
                                .filter(|dep| dep.shared_item_id == item.id)
                                .cloned()
                                .collect::<Vec<_>>(),
                        ),
                    ]);
                }
                print_table(&["ID", "Kind", "Label", "Value", "Dependencies"], &rows);
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
                        + report.dependencies_added
                        + report.dependencies_removed
                        + report.dependencies_updated
                        + report.notes_added
                        + report.notes_removed
                        + report.notes_updated;
                    if total > 0 {
                        print_success(format!(
                            "Config sync: groups +{} -{}, shares +{} -{}, dependencies +{} -{} ~{}, notes +{} -{} ~{}",
                            report.groups_added,
                            report.groups_removed,
                            report.shares_added,
                            report.shares_removed,
                            report.dependencies_added,
                            report.dependencies_removed,
                            report.dependencies_updated,
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
