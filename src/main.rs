mod cli;
mod cloud;
mod codegraph;
mod db;
mod indexer;
mod mcp;
mod models;
mod path;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cloud_push_without_a_token_argument() {
        let app = App::try_parse_from([
            "ai-workspace",
            "cloud",
            "push",
            "--include-markdown",
            "--url",
            "https://cloud.example",
            "--workspace",
            "team",
        ])
        .unwrap();
        assert!(matches!(
            app.command,
            cli::Command::Cloud {
                command: cli::CloudCommand::Push {
                    include_markdown: true,
                    ..
                }
            }
        ));
        assert!(
            App::try_parse_from(["ai-workspace", "cloud", "push", "--token", "secret"]).is_err()
        );
    }
}
