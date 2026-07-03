mod cli;
mod config;
mod conventions;
mod dispatch;
mod guidance;
mod mcp;
mod model;
mod planner;
mod schema;

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
            let defaults = config::load_plan_defaults()?;

// LLM path: if no verify command provided AND model config exists,
// hector's model writes a focused test against the provided --spec.
// The spec (from the frontier model or human) is the authoritative contract.
// Otherwise, the deterministic path is used.
// LLM planning path: a --spec is provided but no --verify, so hector writes the
// focused test against the spec. Without a spec we fall through to the
// deterministic planner, which returns friendly needs_input guidance rather than
// hard-erroring — `hector plan --task X` should tell the user what's missing.
if verify_cmds.iter().all(|c| c.trim().is_empty()) && spec_text.is_some() {
    let model_cfg = config::load_default_model()?;
    if let Some(cfg) = model_cfg {
        let repo_root = std::env::current_dir()?;
        let conventions = conventions::detect(&repo_root)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "could not detect repo conventions (Cargo.toml/package.json/etc); \
                     provide --verify explicitly for deterministic planning"
                )
            })?;
                    let campaign = planner::plan_with_model(
                        planner::PlanOptions {
                            task,
                            name,
                            spec: spec_text,
                            verify_cmds,
                            editable_paths,
                            reference_paths,
                            max_changed_files: max_changed_files.unwrap_or(defaults.max_changed_files),
                            max_changed_lines: max_changed_lines.unwrap_or(defaults.max_changed_lines),
                            max_iters,
                            judge_policy: judge_policy.unwrap_or(defaults.judge_policy),
                            auto_commit: if no_auto_commit {
                                false
                            } else {
                                defaults.auto_commit
                            },
                            invariants: defaults.invariants,
                        },
                        &cfg,
                        &conventions,
                        &repo_root,
                    )
                    .await?;
                    if let Some(path) = out {
                        std::fs::write(path, campaign)?;
                    } else {
                        println!("{campaign}");
                    }
                    return Ok(());
                }
                // No model config either — fall through to deterministic plan,
                // which will return needs_input.
            }

            let campaign = planner::plan(planner::PlanOptions {
                task,
                name,
                spec: spec_text,
                verify_cmds,
                editable_paths,
                reference_paths,
                max_changed_files: max_changed_files.unwrap_or(defaults.max_changed_files),
                max_changed_lines: max_changed_lines.unwrap_or(defaults.max_changed_lines),
                max_iters,
                judge_policy: judge_policy.unwrap_or(defaults.judge_policy),
                auto_commit: if no_auto_commit {
                    false
                } else {
                    defaults.auto_commit
                },
                invariants: defaults.invariants,
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
            deep,
        } => {
            let out = planner::review(&campaign, &bob_result)?;
            // Tier 2: deep review with frontier model (only on accept, or if --deep forced)
            let review_cfg = config::load_review_config()?;
            let force_deep = deep || (review_cfg.deep_on_accept && review_cfg.deep_enabled());
            if force_deep {
                if let Some(reviewer) = &review_cfg.deep_reviewer {
                    let deep_out = planner::deep_review(&campaign, &bob_result, reviewer).await;
                    if let Ok(deep_json) = deep_out {
                        println!("{out}");
                        println!("\n--- deep review ({reviewer}) ---");
                        println!("{deep_json}");
                    } else {
                        println!("{out}");
                        eprintln!("hector: deep review failed: {:?}", deep_out.err());
                    }
                } else {
                    println!("{out}");
                    eprintln!("hector: --deep requested but no deep_reviewer configured in hector.yaml");
                }
            } else {
                println!("{out}");
            }
            Ok(())
        }
        Command::FrontierBrief { compact, out } => {
            let brief = if compact {
                guidance::COMPACT_FRONTIER_BRIEF
            } else {
                guidance::FRONTIER_BRIEF
            };
            if let Some(path) = out {
                std::fs::write(path, brief)?;
            } else {
                println!("{brief}");
            }
            Ok(())
        }
        Command::Init { force } => {
            let path = std::path::Path::new("hector.yaml");
            if path.exists() && !force {
                anyhow::bail!("hector.yaml already exists; use --force to overwrite");
            }
            std::fs::write(path, config::DEFAULT_CONFIG)?;
            Ok(())
        }
        Command::Dispatch { file, jobs, bob_cmd, propose } => {
            let report =
                dispatch::run_campaign(&file, jobs, bob_cmd.as_deref().unwrap_or("bob"), propose)
                    .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Command::Mcp => mcp::serve().await,
    }
}
