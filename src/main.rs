mod cli;
mod guidance;
mod mcp;
mod planner;

use clap::Parser;
use cli::{Cli, Command};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    match args.command {
        Command::Plan {
            task,
            name,
            spec,
            verify_cmds,
            editable_paths,
            reference_paths,
            max_changed_files,
            max_changed_lines,
            max_iters,
            judge_policy,
            no_auto_commit,
            out,
        } => {
            let spec_text = match spec {
                Some(path) => Some(std::fs::read_to_string(path)?),
                None => None,
            };
            let campaign = planner::plan(planner::PlanOptions {
                task,
                name,
                spec: spec_text,
                verify_cmds,
                editable_paths,
                reference_paths,
                max_changed_files,
                max_changed_lines,
                max_iters,
                judge_policy,
                auto_commit: !no_auto_commit,
            })?;
            if let Some(path) = out {
                std::fs::write(path, campaign)?;
            } else {
                println!("{campaign}");
            }
            Ok(())
        }
        Command::Check { file } => planner::check(&file),
        Command::Review {
            campaign,
            bob_result,
        } => {
            let out = planner::review(&campaign, &bob_result)?;
            println!("{out}");
            Ok(())
        }
        Command::FrontierBrief { out } => {
            if let Some(path) = out {
                std::fs::write(path, guidance::FRONTIER_BRIEF)?;
            } else {
                println!("{}", guidance::FRONTIER_BRIEF);
            }
            Ok(())
        }
        Command::Init => {
            std::fs::write(
                "hector.yaml",
                "verify:\n  prefer_focused: true\nscope:\n  forbid_dependency_churn: true\n  default_max_changed_files: 2\n  default_max_changed_lines: 160\njudge:\n  default_policy: retry_on_fail\nbob:\n  campaign_auto_commit: true\n",
            )?;
            Ok(())
        }
        Command::Mcp => mcp::serve().await,
    }
}
