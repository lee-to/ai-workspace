mod cli;
mod db;
mod mcp;
mod models;

use anyhow::Result;
use clap::Parser;
use log::{debug, info};

#[derive(Parser)]
#[command(
    name = "ai-workspace",
    version,
    about = "Cross-project shared context CLI + MCP server"
)]
struct App {
    #[command(subcommand)]
    command: cli::Command,
}

fn main() -> Result<()> {
    env_logger::init();
    info!("ai-workspace starting");

    let app = App::parse();
    debug!("Parsed command: {:?}", app.command);

    cli::run(app.command)
}
