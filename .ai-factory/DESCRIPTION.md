# Project: ai-workspace

## Overview
A cross-project shared context CLI and MCP server that enables AI agents to access shared files, directories, and notes across multiple projects organized into groups. Provides both a command-line interface (`ai-workspace`) and an MCP stdio server for integration with AI coding tools.

## Core Features
- **Project & Group Management**: Register projects, create groups, assign projects to groups for cross-project context sharing
- **Shared Items**: Share files, directories, and notes (project-scoped or group-scoped) with labels
- **MCP Server**: Expose workspace context, file reading, and full-text search via JSON-RPC over stdio
- **Full-Text Search**: FTS5-powered search over shared note labels and content
- **CLI Interface**: Complete CRUD operations via `ai-workspace` binary with subcommands

## Tech Stack
- **Language:** Rust (edition 2024)
- **Database:** SQLite (via rusqlite with bundled feature)
- **CLI Framework:** Clap v4 (derive)
- **Serialization:** Serde + serde_json
- **Error Handling:** anyhow
- **Logging:** env_logger + log
- **Platform Dirs:** dirs v6
- **Testing:** built-in `#[test]` + tempfile for test isolation

## Architecture Notes
- Binary name: `ai-workspace`, package name: `ai-workspace`
- Modular structure: `cli/`, `db/`, `mcp/`, `models.rs`
- MCP server uses raw JSON-RPC over stdio (custom implementation, not rmcp SDK)
- Database stored in platform-specific data directory via `dirs` crate
- Schema uses CHECK constraints for shared item kind validation (file, dir, note)
- FTS5 virtual table indexes label + content for full-text search

## Architecture
See `.ai-factory/ARCHITECTURE.md` for detailed architecture guidelines.
Pattern: Layered Architecture

## Non-Functional Requirements
- Logging: Configurable via `RUST_LOG` environment variable
- Error handling: anyhow for CLI errors, JSON-RPC error codes for MCP
- Security: Local SQLite database, no network access, file paths validated against project root
- Portability: Cross-platform via `dirs` crate for data directory resolution
