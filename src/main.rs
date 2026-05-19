mod cli;
mod db;
mod indexer;
mod mcp;
mod models;
mod walk;

use anyhow::Result;
use clap::Parser;
use log::{debug, info};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "ai-workspace",
    version,
    about = "Cross-project shared context CLI + MCP server"
)]
struct App {
    /// Path to the project config JSON (defaults to .ai-workspace.json, or AI_WORKSPACE_CONFIG)
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: cli::Command,
}

fn main() -> Result<()> {
    env_logger::init();
    info!("ai-workspace starting");

    let app = App::parse();
    debug!("Parsed command: {:?}", app.command);

    cli::run(app.command, app.config)
}
