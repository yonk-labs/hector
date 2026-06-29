//! Parallel campaign dispatcher. Reads a campaign YAML, spawns N concurrent
//! `bob build` processes (one per slice), bounded by --jobs, and collects
//! results into a consolidated report with a scoreboard.
//!
//! Each slice runs independently in its own bob worktree. No git conflicts
//! because slices create different files. Results are collected as they finish.

use crate::schema::{Campaign, Slice};
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tokio::process::Command;
use tokio::sync::Semaphore;

#[derive(Debug, Serialize)]
pub struct DispatchReport {
    campaign: String,
    jobs: usize,
    total_slices: usize,
    converged: usize,
    failed: usize,
    wall_secs: u64,
    slices: Vec<SliceResult>,
    /// Result of re-running the slices' verify gates against the MERGED tree.
    /// Catches integration breakage that per-slice (isolated) verification can't
    /// see. None when nothing was applied/merged.
    #[serde(skip_serializing_if = "Option::is_none")]
    integration: Option<IntegrationReport>,
    /// In --propose mode: path to the merged diff written for inspection (the
    /// working tree was NOT modified). None in apply mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    proposed_diff: Option<String>,
}

#[derive(Debug, Serialize)]
struct IntegrationReport {
    verified: bool,
    gates_run: usize,
    failures: Vec<String>,
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
    /// Candidate diff from bob's propose-mode run, applied sequentially after
    /// all parallel builds finish. Internal — not part of the JSON report.
    #[serde(skip)]
    diff: String,
}

/// Dispatch all slices in a campaign to bob in parallel.
pub async fn run_campaign(
    campaign_path: &Path,
    jobs: usize,
    bob_cmd: &str,
    propose: bool,
) -> anyhow::Result<DispatchReport> {
    let text = std::fs::read_to_string(campaign_path)?;
    let campaign: Campaign = serde_yaml::from_str(&text)?;
    let campaign_name = campaign.name.unwrap_or_else(|| "campaign".into());
    let total = campaign.slices.len();

    if total == 0 {
        anyhow::bail!("campaign has no slices");
    }

    // Collect the union of verify gates BEFORE consuming slices — these re-run
    // against the merged tree after apply to catch cross-slice integration breaks.
    let mut combined_verify: Vec<String> = campaign
        .slices
        .iter()
        .filter_map(|s| s.verify_cmds.as_ref())
        .flatten()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    combined_verify.sort();
    combined_verify.dedup();

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
                diff: String::new(),
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
        match handle.await {
            Ok(sr) => slices.push(sr),
            // A panicked task must not abort the whole dispatch — record it.
            Err(e) => slices.push(SliceResult {
                name: "(panicked)".into(),
                status: "error".into(),
                stop_reason: None,
                iterations: None,
                applied: false,
                wall_secs: 0,
                changed_files: vec![],
                model: None,
                error: Some(format!("dispatch task panicked: {e}")),
                diff: String::new(),
            }),
        }
    }

    // MERGE PHASE. Each slice built in propose mode in its own isolated worktree
    // (parallel, no contention). Bob's apply writes to the shared main repo, and
    // parallel git ops race on `.git/index.lock` — so we merge sequentially here.
    //   --propose: merge into a throwaway worktree, verify there, write the diff
    //              for inspection, discard. The working tree is untouched.
    //   default:   merge into the working tree (staged), then verify.
    let apply_dir = std::env::current_dir()?;
    let (integration, proposed_diff) = if propose {
        propose_in_scratch(&apply_dir, &mut slices, &combined_verify)
    } else {
        for sr in &mut slices {
            if sr.status == "converged" && !sr.diff.trim().is_empty() {
                match apply_diff(&apply_dir, &sr.diff) {
                    Ok(()) => sr.applied = true,
                    Err(e) => {
                        sr.status = "apply_failed".into();
                        sr.error = Some(format!("git apply failed: {e}"));
                    }
                }
            }
        }
        let merged = slices.iter().any(|s| s.applied);
        (run_combined_verify(&apply_dir, merged, &combined_verify), None)
    };

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
        integration,
        proposed_diff,
    })
}

/// Run the combined verify gates against a merged tree at `repo`. `merged` is
/// false when nothing landed (→ None, nothing to verify).
fn run_combined_verify(repo: &Path, merged: bool, gates: &[String]) -> Option<IntegrationReport> {
    if !merged || gates.is_empty() {
        return None;
    }
    let mut failures = Vec::new();
    for cmd in gates {
        if let Err(e) = run_gate(repo, cmd) {
            failures.push(e);
        }
    }
    if !failures.is_empty() {
        eprintln!(
            "hector dispatch: INTEGRATION FAILED — {} gate(s) broke on the merged tree",
            failures.len()
        );
    }
    Some(IntegrationReport {
        verified: failures.is_empty(),
        gates_run: gates.len(),
        failures,
    })
}

/// --propose: merge converged diffs into a throwaway detached worktree off HEAD,
/// run the combined verify there, write the merged diff for inspection, then
/// remove the worktree. The caller's working tree is never modified. Returns the
/// integration result and the path to the written merged diff.
fn propose_in_scratch(
    repo: &Path,
    slices: &mut [SliceResult],
    gates: &[String],
) -> (Option<IntegrationReport>, Option<String>) {
    let scratch = repo.join(".bob").join("dispatch-propose");
    let scratch_str = scratch.to_string_lossy().to_string();
    if let Some(parent) = scratch.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Clear any stale worktree, then create a fresh detached one at HEAD.
    let _ = git(repo, &["worktree", "remove", "--force", &scratch_str]);
    if let Err(e) = git(repo, &["worktree", "add", "--detach", &scratch_str, "HEAD"]) {
        eprintln!("hector dispatch: --propose could not create scratch worktree: {e}");
        return (None, None);
    }

    let mut merged_any = false;
    for sr in slices.iter_mut() {
        if sr.status == "converged" && !sr.diff.trim().is_empty() {
            match apply_diff(&scratch, &sr.diff) {
                Ok(()) => merged_any = true,
                Err(e) => {
                    sr.status = "apply_failed".into();
                    sr.error = Some(format!("git apply (propose) failed: {e}"));
                }
            }
        }
    }

    let integration = run_combined_verify(&scratch, merged_any, gates);

    // Capture the merged diff (apply --index already staged everything).
    let diff_path = if merged_any {
        let merged = git(&scratch, &["diff", "--cached", "HEAD"]).unwrap_or_default();
        let path = repo.join(".bob").join("dispatch-merged.diff");
        let _ = std::fs::write(&path, &merged);
        eprintln!(
            "hector dispatch: --propose — working tree untouched; merged diff at {}",
            path.display()
        );
        Some(path.to_string_lossy().to_string())
    } else {
        None
    };

    let _ = git(repo, &["worktree", "remove", "--force", &scratch_str]);
    (integration, diff_path)
}

/// Minimal git runner: Ok(stdout) on success, Err(stderr) on failure.
fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("git {args:?}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Run one verify gate (`sh -c`) against the merged working tree. Ok(()) on
/// pass; Err(message) names the gate and a tail of stderr on failure.
fn run_gate(repo: &Path, cmd: &str) -> Result<(), String> {
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("gate `{cmd}`: could not run: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let tail: String = String::from_utf8_lossy(&out.stderr)
        .trim()
        .chars()
        .rev()
        .take(400)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Err(format!("gate `{cmd}` failed: {tail}"))
}

/// Run a single slice through bob build.
async fn run_slice(
    slice: &Slice,
    bob_cmd: &str,
    campaign_dir: &Path,
    slice_name: &str,
) -> anyhow::Result<SliceResult> {
    let task = slice.task.as_deref().unwrap_or("(no task)");

    let mut cmd = Command::new(bob_cmd);
    // --json: bob emits only the RunResult JSON (diff in final_diff). Without it
    // bob prints a human summary + raw diff, which isn't parseable.
    cmd.arg("build").arg("--json").arg(task).current_dir(campaign_dir);

    // All verify gates — bob's --verify is repeatable. (Previously only the
    // first gate was passed, silently dropping the rest.)
    if let Some(cmds) = &slice.verify_cmds {
        for c in cmds.iter().filter(|c| !c.trim().is_empty()) {
            cmd.arg("--verify").arg(c);
        }
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

    // Propose mode (no --apply): builds run in parallel in isolated worktrees;
    // the orchestrator applies the resulting diffs sequentially in run_campaign
    // to avoid racing on the shared main-repo git index.
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

    // Candidate diff (propose mode) — applied sequentially by the caller.
    let diff = result
        .get("final_diff")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Cleanup temp spec file
    let spec_path = campaign_dir.join(format!(".bob/dispatch-{slice_name}-spec.md"));
    let _ = std::fs::remove_file(&spec_path);

    Ok(SliceResult {
        name: slice_name.to_string(),
        status,
        stop_reason,
        iterations,
        applied, // false in propose mode; set true by the apply phase
        wall_secs: 0, // set by caller
        changed_files,
        model,
        error: None,
        diff,
    })
}

/// Apply a unified diff (bob's `final_diff`) to the repo working tree + index.
/// Used by the sequential apply phase so parallel slices never race on the
/// git index.
fn apply_diff(repo: &Path, diff: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let mut child = std::process::Command::new("git")
        .args(["apply", "--index", "--whitespace=nowarn"])
        .current_dir(repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("git apply: no stdin handle"))?
        .write_all(diff.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
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

    #[test]
    fn run_gate_reports_pass_and_fail() {
        let d = std::env::temp_dir();
        assert!(run_gate(&d, "true").is_ok());
        let e = run_gate(&d, "false").unwrap_err();
        assert!(e.contains("failed"), "fail message names the gate: {e}");
    }
}
