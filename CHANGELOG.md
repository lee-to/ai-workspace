# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2025-03-17

### Added

- CLI for managing cross-project shared context (`init`, `share`, `note`, `edit`, `rm`, `status`, `export`, `sync`)
- MCP server (`serve`) with 5 tools: `workspace_context`, `workspace_read`, `workspace_search`, `list_groups`, `list_projects`
- Group-based project organization
- File and directory sharing with labels
- Project-scoped and group-scoped notes
- Team sharing via `.ai-workspace.json` export/import
- Multi-platform release builds (macOS, Linux, Windows)
