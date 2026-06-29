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
        /// Campaign/slice name.
        #[arg(long)]
        name: Option<String>,
        /// Optional longer spec file.
        #[arg(long)]
        spec: Option<PathBuf>,
        /// Verify command. Repeat for multiple gates.
        #[arg(long = "verify")]
        verify_cmds: Vec<String>,
        /// Editable path Bob may change. Repeat for multiple paths.
        #[arg(long = "editable-path")]
        editable_paths: Vec<String>,
        /// Reference path Bob may read. Repeat for multiple paths.
        #[arg(long = "reference-path")]
        reference_paths: Vec<String>,
        /// Max changed files cap.
        #[arg(long)]
        max_changed_files: Option<u64>,
        /// Max changed lines cap.
        #[arg(long)]
        max_changed_lines: Option<u64>,
        /// Max Bob iterations.
        #[arg(long, default_value_t = 4)]
        max_iters: u32,
        /// Judge policy for Bob.
        #[arg(long)]
        judge_policy: Option<String>,
        /// Disable campaign auto_commit.
        #[arg(long)]
        no_auto_commit: bool,
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
    /// Review Bob's result against the original Hector campaign.
    Review {
        /// Original campaign YAML/JSON file.
        #[arg(long)]
        campaign: PathBuf,
        /// Bob result JSON file.
        #[arg(long = "bob-result")]
        bob_result: PathBuf,
        /// Force deep (expensive) review with the frontier reviewer.
        #[arg(long)]
        deep: bool,
    },
    /// Print instructions for frontier models writing Hector-ready slices.
    #[command(alias = "brief", alias = "prompt")]
    FrontierBrief {
        /// Print the short low-token handoff.
        #[arg(long)]
        compact: bool,
        /// Optional output path.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Write starter hector.yaml.
    Init {
        /// Overwrite an existing hector.yaml.
        #[arg(long)]
        force: bool,
    },
    /// Run the stdio MCP server.
    Mcp,
}
