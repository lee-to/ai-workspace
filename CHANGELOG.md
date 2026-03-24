# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.3.0] - 2026-03-24

### Added

- **`update` command** — self-update binary from GitHub Releases. Downloads the appropriate binary for the current platform and replaces the running executable in place. No Rust or Cargo required.
- **`destroy` command** — remove the current project from ai-workspace entirely (shared items, group memberships, notes). Files on disk are not affected.

### Changed

- `init` no longer renames a project when re-run without `--name`. The project name is only updated when `--name` is explicitly provided.

## [0.2.0] - 2026-03-24

### Added

- **`project_tree` MCP tool** — browse a project's file tree respecting `.gitignore`. Supports `path` (subdirectory) and `max_depth` parameters to control output scope.
- **`project_grep` MCP tool** — regex search across project files respecting `.gitignore`. Supports `glob` filter, returns up to 100 matches grouped by file. Skips binary files and files > 1 MB.
- **`workspace_read` by path** — read any file in a registered project via `project_id` + `path`, without pre-sharing it. Mutually exclusive with existing `item_id` mode.
- **Auto-share on init** — `ai-workspace init` automatically detects and shares key project files (`README*`, `Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`, `composer.json`, `Makefile`, `Taskfile.yml`, `Justfile`). Skipped when `.ai-workspace.json` exists. No duplicates on re-init.

### Changed

- `rusqlite` upgraded from 0.32 to 0.39.

## [0.1.0] - 2025-03-17

### Added

- CLI for managing cross-project shared context (`init`, `share`, `note`, `edit`, `rm`, `status`, `export`, `sync`)
- MCP server (`serve`) with 5 tools: `workspace_context`, `workspace_read`, `workspace_search`, `list_groups`, `list_projects`
- Group-based project organization
- File and directory sharing with labels
- Project-scoped and group-scoped notes
- Team sharing via `.ai-workspace.json` export/import
- Multi-platform release builds (macOS, Linux, Windows)
