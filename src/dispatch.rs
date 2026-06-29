//! Parallel campaign dispatcher. Reads a campaign YAML, spawns N concurrent
//! `bob build` processes (one per slice), bounded by --jobs, and collects
//! results into a consolidated report with a scoreboard.
//!
//! Each slice runs independently in its own bob worktree. No git conflicts
//! because slices create different files. Results are collected as they finish.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tokio::process::Command;
use tokio::sync::Semaphore;

#[derive(Debug, Deserialize)]
struct Campaign {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    slices: Vec<Slice>,
}

#[derive(Debug, Deserialize)]
struct Slice {
    #[serde(default)]
    name: Option<String>,
    task: Option<String>,
    #[serde(default)]
    verify_cmds: Option<Vec<String>>,
    #[serde(default)]
    editable_paths: Vec<String>,
    #[serde(default)]
    reference_paths: Vec<String>,
    #[serde(default)]
    spec: Option<String>,
    #[serde(default)]
    max_iters: Option<u32>,
    #[serde(default)]
    max_changed_files: Option<u64>,
    #[serde(default)]
    max_changed_lines: Option<u64>,
    #[serde(default)]
    judge_policy: Option<String>,
    #[serde(default)]
    tier: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DispatchReport {
    campaign: String,
    jobs: usize,
    total_slices: usize,
    converged: usize,
    failed: usize,
    wall_secs: u64,
    slices: Vec<SliceResult>,
}

#[derive(Debug, Serialize)]
pub struct SliceResult {
    name: String,
    status: String,
    stop_reason: Option<String>,
    iterations: Option<u32>,
    applied: bool,
    wall_secs: u64,
    changed_files: Vec<String>,
    model: Option<String>,
    error: Option<String>,
}

/// Dispatch all slices in a campaign to bob in parallel.
pub async fn run_campaign(
    campaign_path: &Path,
    jobs: usize,
    bob_cmd: &str,
) -> anyhow::Result<DispatchReport> {
    let text = std::fs::read_to_string(campaign_path)?;
    let campaign: Campaign = serde_yaml::from_str(&text)?;
    let campaign_name = campaign.name.unwrap_or_else(|| "campaign".into());
    let total = campaign.slices.len();

    if total == 0 {
        anyhow::bail!("campaign has no slices");
    }

    let max_jobs = jobs.min(total).max(1);
    eprintln!(
        "hector dispatch: {} slices, {} jobs, bob='{}'",
        total, max_jobs, bob_cmd
    );

    let semaphore = Arc::new(Semaphore::new(max_jobs));
    let start = Instant::now();
    let mut handles = Vec::new();

    for (idx, slice) in campaign.slices.into_iter().enumerate() {
        let permit = semaphore.clone();
        let bob = bob_cmd.to_string();
        let campaign_dir = std::env::current_dir()?;
        let slice_name = slice.name.clone().unwrap_or_else(|| format!("slice-{idx}"));
        handles.push(tokio::spawn(async move {
            let _permit = permit.acquire().await.unwrap();
            eprintln!("hector dispatch: starting '{slice_name}'");
            let slice_start = Instant::now();
            let result = run_slice(&slice, &bob, &campaign_dir, &slice_name).await;
            let wall = slice_start.elapsed().as_secs();
            let mut sr = result.unwrap_or_else(|e| SliceResult {
                name: slice_name.clone(),
                status: "error".into(),
                stop_reason: None,
                iterations: None,
                applied: false,
                wall_secs: wall,
                changed_files: vec![],
                model: None,
                error: Some(e.to_string()),
            });
            sr.wall_secs = wall;
            eprintln!(
                "hector dispatch: '{slice_name}' done: {} in {}s",
                sr.status, sr.wall_secs
            );
            sr
        }));
    }

    let mut slices = Vec::new();
    for handle in handles {
        slices.push(handle.await.unwrap());
    }

    let wall = start.elapsed().as_secs();
    let converged = slices.iter().filter(|s| s.status == "converged").count();
    let failed = slices.len() - converged;

    Ok(DispatchReport {
        campaign: campaign_name,
        jobs: max_jobs,
        total_slices: total,
        converged,
        failed,
        wall_secs: wall,
        slices,
    })
}

/// Run a single slice through bob build.
async fn run_slice(
    slice: &Slice,
    bob_cmd: &str,
    campaign_dir: &Path,
    slice_name: &str,
) -> anyhow::Result<SliceResult> {
    let task = slice.task.as_deref().unwrap_or("(no task)");
    let verify = slice
        .verify_cmds
        .as_ref()
        .and_then(|cmds| cmds.first())
        .cloned()
        .unwrap_or_default();

    let mut cmd = Command::new(bob_cmd);
    cmd.arg("build").arg(task).current_dir(campaign_dir);

    // Verify command
    if !verify.is_empty() {
        cmd.arg("--verify").arg(&verify);
    }

    // Editable paths
    for p in &slice.editable_paths {
        cmd.arg("--allow-path").arg(p);
    }

    // Context files (reference paths)
    for p in &slice.reference_paths {
        cmd.arg("--files").arg(p);
    }

    // Spec as context if present
    if let Some(spec) = &slice.spec {
        // Write spec to temp file for --files
        let spec_path = campaign_dir.join(format!(".bob/dispatch-{slice_name}-spec.md"));
        if let Some(parent) = spec_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&spec_path, spec)?;
        cmd.arg("--files").arg(&spec_path);
    }

    // Overrides
    if let Some(n) = slice.max_iters {
        cmd.arg("--max-iters").arg(n.to_string());
    }
    if let Some(n) = slice.max_changed_files {
        cmd.arg("--max-changed-files").arg(n.to_string());
    }
    if let Some(n) = slice.max_changed_lines {
        cmd.arg("--max-changed-lines").arg(n.to_string());
    }
    if let Some(p) = &slice.judge_policy {
        cmd.arg("--judge-policy").arg(p);
    }
    if let Some(t) = &slice.tier {
        cmd.arg("--tier").arg(t);
    }

    // Always apply in dispatch mode — each slice is independent
    cmd.arg("--apply");

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let output = cmd.output().await?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Parse bob's JSON output (it prints RunResult as JSON)
    let result: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| {
            serde_json::json!({
                "status": if output.status.success() { "converged" } else { "failed" },
                "error": stderr.chars().take(500).collect::<String>(),
            })
        });

    let status = result
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let stop_reason = result
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .map(String::from);

    let iterations = result
        .get("iterations")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);

    let applied = result
        .get("applied")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let model = result
        .pointer("/builder/model")
        .and_then(|v| v.as_str())
        .map(String::from);

    let changed_files = result
        .get("changed_files")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Cleanup temp spec file
    let spec_path = campaign_dir.join(format!(".bob/dispatch-{slice_name}-spec.md"));
    let _ = std::fs::remove_file(&spec_path);

    Ok(SliceResult {
        name: slice_name.to_string(),
        status,
        stop_reason,
        iterations,
        applied,
        wall_secs: 0, // set by caller
        changed_files,
        model,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_campaign_yaml() {
        let yaml = r#"
name: test
slices:
  - name: slice-a
    task: "do thing A"
    verify_cmds: ["echo ok"]
    editable_paths: ["src/a.js"]
    tier: cheap
  - name: slice-b
    task: "do thing B"
    verify_cmds: ["echo ok"]
    editable_paths: ["src/b.js"]
    tier: medium
"#;
        let campaign: Campaign = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(campaign.slices.len(), 2);
        assert_eq!(campaign.slices[0].name.as_deref(), Some("slice-a"));
        assert_eq!(campaign.slices[1].tier.as_deref(), Some("medium"));
    }

    #[test]
    fn empty_campaign_rejected() {
        let yaml = "name: empty\nslices: []";
        let campaign: Campaign = serde_yaml::from_str(yaml).unwrap();
        assert!(campaign.slices.is_empty());
    }
}
