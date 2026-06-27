use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "hector", about = "TDD slice planner for Bob campaigns")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Draft a Bob campaign from a task/spec.
    Plan {
        /// Task description.
        #[arg(long)]
        task: String,
        /// Optional longer spec file.
        #[arg(long)]
        spec: Option<PathBuf>,
        /// Optional output campaign path.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Validate a Bob campaign before handing it to Bob.
    Check {
        /// Campaign YAML/JSON file.
        #[arg(long)]
        file: PathBuf,
    },
    /// Write starter hector.yaml.
    Init,
    /// Run the stdio MCP server.
    Mcp,
}
