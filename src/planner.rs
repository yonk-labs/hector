//! Planning, static validation, and result review for Hector campaigns.
//!
//! Hector deliberately keeps these operations pure and file-format oriented:
//! the CLI and MCP server both call the same functions, so a campaign accepted
//! by `hector check` has the same guardrails no matter which host agent asked.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path};

#[derive(Debug, Deserialize, Serialize)]
struct Campaign {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    auto_commit: bool,
    slices: Vec<Slice>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Slice {
    #[serde(default)]
    name: Option<String>,
    task: Option<String>,
    #[serde(default)]
    spec: Option<String>,
    #[serde(default)]
    verify_cmds: Option<Vec<String>>,
    #[serde(default)]
    editable_paths: Vec<String>,
    #[serde(default)]
    reference_paths: Vec<String>,
    #[serde(default)]
    judge_policy: Option<String>,
    #[serde(default)]
    max_iters: Option<u32>,
    #[serde(default)]
    max_changed_files: Option<u64>,
    #[serde(default)]
    max_changed_lines: Option<u64>,
}

pub struct PlanOptions {
    pub task: String,
    pub name: Option<String>,
    pub spec: Option<String>,
    pub verify_cmds: Vec<String>,
    pub editable_paths: Vec<String>,
    pub reference_paths: Vec<String>,
    pub max_changed_files: u64,
    pub max_changed_lines: u64,
    pub max_iters: u32,
    pub judge_policy: String,
    pub auto_commit: bool,
}

#[derive(Serialize)]
struct NeedsInput {
    status: &'static str,
    human_questions: Vec<&'static str>,
}

pub fn plan(opts: PlanOptions) -> anyhow::Result<String> {
    let mut questions = Vec::new();
    if opts.task.trim().is_empty() {
        questions.push("What observable behavior should this slice implement?");
    }
    if opts.verify_cmds.iter().all(|c| c.trim().is_empty()) {
        questions.push("What command proves this slice is correct?");
    }
    if opts.editable_paths.is_empty() {
        questions.push("Which files or directories may Bob edit?");
    }
    if !questions.is_empty() {
        return Ok(serde_json::to_string_pretty(&NeedsInput {
            status: "needs_input",
            human_questions: questions,
        })?);
    }

    let name = opts.name.unwrap_or_else(|| slug(&opts.task));
    let campaign = Campaign {
        name: Some(name.clone()),
        auto_commit: opts.auto_commit,
        slices: vec![Slice {
            name: Some(name),
            task: Some(opts.task),
            spec: opts.spec,
            verify_cmds: Some(opts.verify_cmds),
            editable_paths: opts.editable_paths,
            reference_paths: opts.reference_paths,
            judge_policy: Some(opts.judge_policy),
            max_iters: Some(opts.max_iters),
            max_changed_files: Some(opts.max_changed_files),
            max_changed_lines: Some(opts.max_changed_lines),
        }],
    };
    let yaml = serde_yaml::to_string(&campaign)?;
    check_text(&yaml)?;
    Ok(yaml)
}

pub fn check(path: &Path) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)?;
    check_text(&content)
}

pub fn check_text(content: &str) -> anyhow::Result<()> {
    let campaign: Campaign = serde_yaml::from_str(content)?;

    if campaign.slices.is_empty() {
        anyhow::bail!("campaign must have at least one slice");
    }

    for slice in &campaign.slices {
        if slice.task.as_ref().is_none_or(|s| s.trim().is_empty()) {
            anyhow::bail!("slice missing task");
        }
        if slice
            .verify_cmds
            .as_ref()
            .is_none_or(|cmds| cmds.iter().all(|c| c.trim().is_empty()))
        {
            anyhow::bail!("slice missing verify_cmds");
        }
        if slice.editable_paths.is_empty() {
            anyhow::bail!("slice missing editable_paths");
        }
        if slice.max_changed_files.is_none_or(|n| n == 0) {
            anyhow::bail!("slice missing max_changed_files");
        }
        if slice.max_changed_lines.is_none_or(|n| n == 0) {
            anyhow::bail!("slice missing max_changed_lines");
        }
        for editable_path in &slice.editable_paths {
            if is_unsafe_path(editable_path) {
                anyhow::bail!("unsafe path: {editable_path}");
            }
            if is_test_path(editable_path) {
                anyhow::bail!("test files must be reference-only: {editable_path}");
            }
            if is_dependency_file(editable_path) {
                anyhow::bail!("dependency churn: {editable_path}");
            }
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct BobResult {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    next_action: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    changed_files: Vec<String>,
    #[serde(default)]
    slices: Vec<BobSliceResult>,
}

#[derive(Debug, Deserialize)]
struct BobSliceResult {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    next_action: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    changed_files: Vec<String>,
}

#[derive(Serialize)]
struct ReviewReport {
    decision: String,
    findings: Vec<String>,
}

pub fn review(campaign_path: &Path, bob_result_path: &Path) -> anyhow::Result<String> {
    let campaign = std::fs::read_to_string(campaign_path)?;
    let bob_result = std::fs::read_to_string(bob_result_path)?;
    review_text(&campaign, &bob_result)
}

pub fn review_text(campaign_text: &str, bob_result_text: &str) -> anyhow::Result<String> {
    check_text(campaign_text)?;
    let campaign: Campaign = serde_yaml::from_str(campaign_text)?;
    let result: BobResult = serde_json::from_str(bob_result_text)?;
    let allowed = editable_set(&campaign);
    let changed = changed_files(&result);
    let mut findings = Vec::new();

    for path in &changed {
        if !is_allowed_change(path, &allowed) {
            findings.push(format!("changed file outside editable_paths: {path}"));
        }
        if is_dependency_file(path) {
            findings.push(format!("dependency churn in Bob result: {path}"));
        }
    }

    let status = result_statuses(&result).join(" ").to_ascii_lowercase();
    let action = result_actions(&result).join(" ").to_ascii_lowercase();
    // Review is intentionally conservative: scope violations beat status, and
    // "needs_review" means the work may be useful but still needs a frontier
    // model or human to compare the diff against the product contract.
    let decision = if !findings.is_empty() {
        "revise_campaign"
    } else if action.contains("split_task")
        || status.contains("scopeexceeded")
        || status.contains("scope_exceeded")
        || status.contains("scope exceeded")
    {
        "split_task"
    } else if action.contains("retry_with_verify_failure") {
        "revise_campaign"
    } else if status.contains("needs_review") || action.contains("review_candidate") {
        "accept_for_human_review"
    } else if status.contains("completed") || status.contains("converged") {
        "accept"
    } else {
        "ask_human"
    };

    Ok(serde_json::to_string_pretty(&ReviewReport {
        decision: decision.to_string(),
        findings,
    })?)
}

fn editable_set(campaign: &Campaign) -> BTreeSet<String> {
    campaign
        .slices
        .iter()
        .flat_map(|s| s.editable_paths.iter().cloned())
        .collect()
}

fn changed_files(result: &BobResult) -> Vec<String> {
    let mut out = result.changed_files.clone();
    out.extend(result.slices.iter().flat_map(|s| s.changed_files.clone()));
    out.sort();
    out.dedup();
    out
}

fn result_statuses(result: &BobResult) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(s) = &result.status {
        out.push(s.clone());
    }
    if let Some(s) = &result.stop_reason {
        out.push(s.clone());
    }
    for s in &result.slices {
        if let Some(v) = &s.status {
            out.push(v.clone());
        }
        if let Some(v) = &s.stop_reason {
            out.push(v.clone());
        }
    }
    out
}

fn result_actions(result: &BobResult) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(s) = &result.next_action {
        out.push(s.clone());
    }
    for s in &result.slices {
        if let Some(v) = &s.next_action {
            out.push(v.clone());
        }
    }
    out
}

fn is_allowed_change(path: &str, allowed: &BTreeSet<String>) -> bool {
    allowed.iter().any(|prefix| {
        let prefix = prefix.trim_end_matches('/');
        path == prefix || path.starts_with(&format!("{prefix}/"))
    })
}

fn is_unsafe_path(path: &str) -> bool {
    let p = Path::new(path);
    p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir))
}

fn is_test_path(path: &str) -> bool {
    let p = Path::new(path);
    p.components()
        .any(|c| matches!(c, Component::Normal(s) if s == "tests" || s == "test"))
        || path.ends_with("_test.rs")
        || path.ends_with(".test.js")
        || path.ends_with(".spec.js")
        || path.ends_with(".test.ts")
}

fn is_dependency_file(path: &str) -> bool {
    let name = Path::new(path).file_name().and_then(|s| s.to_str());
    matches!(
        name,
        Some(
            "Cargo.toml"
                | "Cargo.lock"
                | "package.json"
                | "package-lock.json"
                | "pnpm-lock.yaml"
                | "yarn.lock"
        )
    )
}

fn slug(s: &str) -> String {
    let out = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let out = out.trim_matches('-');
    if out.is_empty() {
        "campaign".into()
    } else {
        out.chars().take(40).collect()
    }
}
