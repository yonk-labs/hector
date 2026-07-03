mod cli;
mod config;
mod conventions;
mod dispatch;
mod guidance;
mod maple;
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
            symbols,
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
            // Path selection below keys off whether the USER supplied a spec;
            // lessons augmentation must not flip a no-spec plan into the LLM path.
            let user_provided_spec = spec_text.is_some();
            let spec_text = planner::apply_lessons(spec_text, &std::env::current_dir()?);
            let defaults = config::load_plan_defaults()?;

            // --symbol: derive scope from the code-symbol graph. Explicit path
            // flags win; maple fills only what the caller didn't provide.
            let (editable_paths, reference_paths) = if symbols.is_empty() {
                (editable_paths, reference_paths)
            } else {
                let repo = std::env::current_dir()?;
                match maple::scope_from_symbols(&repo, &symbols, defaults.maple_budget)? {
                    Some(scope) => {
                        eprintln!(
                            "hector: maple scoped {} symbol(s) → {} editable, {} reference path(s), ~{} tokens",
                            symbols.len(),
                            scope.editable_paths.len(),
                            scope.reference_paths.len(),
                            scope.total_tokens
                        );
                        (
                            if editable_paths.is_empty() { scope.editable_paths } else { editable_paths },
                            if reference_paths.is_empty() { scope.reference_paths } else { reference_paths },
                        )
                    }
                    None => {
                        eprintln!("hector: warning — {}", maple::FALLBACK_WARNING);
                        (editable_paths, reference_paths)
                    }
                }
            };

            // LLM planning path: a --spec is provided but no --verify, so hector
            // writes the focused test against the spec. Without a spec we fall
            // through to the deterministic planner, which returns friendly
            // needs_input guidance rather than hard-erroring.
            let no_verify = verify_cmds.iter().all(|c| c.trim().is_empty());
            if user_provided_spec && !no_verify {
                // Surprising-silence fix: providing --verify used to silently
                // downgrade to deterministic passthrough. Say which path ran.
                eprintln!(
                    "hector: --verify provided → deterministic planning (no LLM test-writing). \
                     Omit --verify to have a configured model write the focused test."
                );
            }
            if no_verify && user_provided_spec {
                let models = config::load_models()?;
                if models.is_empty() {
                    eprintln!(
                        "hector: no planner models configured (looked in {}) — \
                         LLM test-writing unavailable, falling back to deterministic planning",
                        config::CONFIG_SEARCH_HINT
                    );
                } else {
                    let repo_root = std::env::current_dir()?;
                    let conventions = conventions::detect(&repo_root).ok_or_else(|| {
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
                        &models,
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
            // needs_input is not a campaign: never write it to --out (a later
            // `hector check`/dispatch would choke on it) and never exit 0 —
            // automation must see that planning did not produce a campaign.
            if campaign.trim_start().starts_with('{') && campaign.contains("needs_input") {
                println!("{campaign}");
                eprintln!("hector: plan needs input — no campaign written");
                std::process::exit(2);
            }
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
        Command::Dispatch { file, jobs, bob_cmd, propose, escalate } => {
            let report = dispatch::run_campaign(
                &file,
                jobs,
                bob_cmd.as_deref().unwrap_or("bob"),
                propose,
                escalate,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            // A failed campaign must fail the process — automation reads the
            // exit code, not the scoreboard.
            if !report.succeeded() {
                anyhow::bail!("dispatch did not fully succeed (see report above)");
            }
            Ok(())
        }
        Command::Doctor { probe } => doctor(probe),
        Command::Mcp => mcp::serve().await,
    }
}

/// `hector doctor [--probe]`: report which config file is in effect and the
/// planner models in rotation order; with --probe, curl each endpoint's
/// /models and mark dead entries (non-zero exit if any are dead).
fn doctor(probe: bool) -> anyhow::Result<()> {
    let Some(view) = config::doctor_view()? else {
        println!("config: none found (searched {})", config::CONFIG_SEARCH_HINT);
        println!("models: none — LLM planning disabled, deterministic path only");
        return Ok(());
    };
    println!("config: {}", view.path.display());
    if view.models.is_empty() {
        println!("models: none configured — LLM planning disabled, deterministic path only");
        return Ok(());
    }
    let default = view
        .default_model
        .clone()
        .unwrap_or_else(|| view.models[0].name.clone());
    let mut dead = 0;
    for m in &view.models {
        let marker = if m.name == default { " (default)" } else { "" };
        if probe {
            let status = if endpoint_alive(m) {
                "ALIVE"
            } else {
                dead += 1;
                "DEAD"
            };
            println!("model {}{marker}: {} @ {} — {status}", m.name, m.model, m.base_url);
        } else {
            println!("model {}{marker}: {} @ {}", m.name, m.model, m.base_url);
        }
    }
    if dead > 0 {
        anyhow::bail!("{dead} configured model endpoint(s) are dead");
    }
    Ok(())
}

/// GET {base_url}/models with a short timeout — the cheapest liveness check an
/// OpenAI-compatible endpoint offers.
fn endpoint_alive(m: &model::ModelCfg) -> bool {
    let url = format!("{}/models", m.base_url.trim_end_matches('/'));
    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-sf", "--max-time", "5", &url]);
    if let Some(env) = &m.api_key_env {
        if let Ok(key) = std::env::var(env) {
            cmd.args(["-H", &format!("Authorization: Bearer {key}")]);
        }
    }
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
