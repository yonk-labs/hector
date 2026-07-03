//! Parallel campaign dispatcher. Reads a campaign YAML, spawns N concurrent
//! `bob build` processes (one per slice), bounded by --jobs, and collects
//! results into a consolidated report with a scoreboard.
//!
//! Each slice runs independently in its own bob worktree. No git conflicts
//! because slices create different files. Results are collected as they finish.

use crate::schema::{Campaign, Slice};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
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
    /// Slices whose verify gates were already green on base — a re-dispatch
    /// of a partially-landed campaign. Skipped (no bob run), counted as done.
    already_landed: usize,
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
    /// Advisory blast radius from `maple impact`: symbols the landed diff
    /// touched plus their callers. None when nothing landed, in --propose
    /// mode, or when maple is unavailable — never blocks a dispatch.
    #[serde(skip_serializing_if = "Option::is_none")]
    impact: Option<serde_json::Value>,
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
    /// Set when --escalate re-ran this slice at a higher tier after a failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    escalated_to: Option<String>,
    /// Candidate diff from bob's propose-mode run, applied sequentially after
    /// all parallel builds finish. Internal — not part of the JSON report.
    #[serde(skip)]
    diff: String,
}

/// Slice outcomes that count as "done" rather than failed.
fn is_success(status: &str) -> bool {
    status == "converged" || status == "already_landed"
}

impl DispatchReport {
    /// True when automation can trust this dispatch: every slice landed (or
    /// was already landed) and the merged-tree integration gates passed.
    pub fn succeeded(&self) -> bool {
        self.failed == 0 && self.integration.as_ref().is_none_or(|i| i.verified)
    }
}

/// Skip-green pre-flight: all of the slice's verify gates already pass on the
/// current tree. Re-dispatching a partially-landed campaign used to re-run the
/// landed slice — bob burned a full build (909s in the pilot) to produce an
/// empty diff that read as a failure and skipped its dependents.
fn slice_already_green(repo: &Path, slice: &Slice) -> bool {
    let gates: Vec<&String> = slice
        .verify_cmds
        .iter()
        .flatten()
        .filter(|c| !c.trim().is_empty())
        .collect();
    !gates.is_empty() && gates.iter().all(|c| run_gate(repo, c).is_ok())
}

/// Uniform error/skip result constructor.
fn error_result(name: &str, msg: String) -> SliceResult {
    SliceResult {
        name: name.to_string(),
        status: "error".into(),
        stop_reason: None,
        iterations: None,
        applied: false,
        wall_secs: 0,
        changed_files: vec![],
        model: None,
        error: Some(msg),
        escalated_to: None,
        diff: String::new(),
    }
}

/// Tier name → ordered member models, plus bob's default tier.
struct BobTiers {
    tiers: HashMap<String, Vec<String>>,
    default_tier: Option<String>,
}

/// Tier→members map from `bob models --json` (bob ≥0.4.0), run in the
/// campaign dir so repo-level tiers apply. None when bob is older, errors,
/// or emits something unparseable — round-robin then simply doesn't happen.
fn load_bob_tiers(bob_cmd: &str, dir: &Path) -> Option<BobTiers> {
    let out = std::process::Command::new(bob_cmd)
        .args(["models", "--json"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let tiers = v
        .get("tiers")?
        .as_object()?
        .iter()
        .map(|(name, members)| {
            let m: Vec<String> = members
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            (name.clone(), m)
        })
        .collect();
    let default_tier = v
        .get("default_tier")
        .and_then(|d| d.as_str())
        .map(String::from);
    Some(BobTiers { tiers, default_tier })
}

/// Round-robin unpinned slices in a parallel batch across their tier's member
/// models (notes #8/#17b): without this, every unpinned slice resolves to
/// bob's stats-best model and one endpoint saturates while the others idle.
/// Only kicks in when ≥2 unpinned slices share a tier with ≥2 members; a
/// single slice keeps bob's adaptive stats-ranked pick.
fn assign_round_robin(
    slices: &[Option<Slice>],
    batch: &[usize],
    tiers: &HashMap<String, Vec<String>>,
    default_tier: Option<&str>,
) -> HashMap<usize, String> {
    let tier_of = |i: usize| -> Option<&str> {
        let s = slices[i].as_ref()?;
        if s.model.is_some() {
            return None; // explicit pin wins, excluded from spreading
        }
        let t = s.tier.as_deref().or(default_tier)?;
        (tiers.get(t).map(|m| m.len()).unwrap_or(0) >= 2).then_some(t)
    };
    let mut per_tier: HashMap<&str, usize> = HashMap::new();
    for &i in batch {
        if let Some(t) = tier_of(i) {
            *per_tier.entry(t).or_insert(0) += 1;
        }
    }
    let mut counters: HashMap<&str, usize> = HashMap::new();
    let mut out = HashMap::new();
    for &i in batch {
        let Some(t) = tier_of(i) else { continue };
        if per_tier[t] < 2 {
            continue;
        }
        let members = &tiers[t];
        let c = counters.entry(t).or_insert(0);
        out.insert(i, members[*c % members.len()].clone());
        *c += 1;
    }
    out
}

/// Fixed escalation ladder mirroring bob's tiers. An unset tier (bob's
/// configured default) escalates to medium as the first meaningful bump;
/// frontier has nowhere to go.
fn next_tier(current: Option<&str>) -> Option<&'static str> {
    match current {
        None | Some("cheap") => Some("medium"),
        Some("medium") => Some("large"),
        Some("large") => Some("frontier"),
        _ => None,
    }
}

/// Run a slice; with `escalate`, retry ONCE at the next tier up when the first
/// attempt doesn't converge. The escalation is loud (stderr + `escalated_to`
/// in the report) — hector surfaces autonomy, it never hides it.
///
/// `rr_model` is a dispatch round-robin assignment (never a user pin): it
/// applies to the first attempt only and is dropped on escalation, so the
/// bumped tier picks its own (stronger) model.
async fn run_slice_escalating(
    slice: Slice,
    bob_cmd: &str,
    campaign_dir: &Path,
    slice_name: &str,
    escalate: bool,
    rr_model: Option<String>,
) -> SliceResult {
    let sr = run_slice(&slice, bob_cmd, campaign_dir, slice_name, rr_model.as_deref())
        .await
        .unwrap_or_else(|e| error_result(slice_name, e.to_string()));
    if sr.status == "converged" || !escalate {
        return sr;
    }
    let Some(tier) = next_tier(slice.tier.as_deref()) else {
        return sr;
    };
    eprintln!(
        "hector dispatch: '{slice_name}' {} at tier {} — escalating to '{tier}' and retrying",
        sr.status,
        slice.tier.as_deref().unwrap_or("(default)")
    );
    let mut retry_slice = slice;
    retry_slice.tier = Some(tier.to_string());
    let mut retry = run_slice(&retry_slice, bob_cmd, campaign_dir, slice_name, None)
        .await
        .unwrap_or_else(|e| error_result(slice_name, e.to_string()));
    retry.escalated_to = Some(tier.to_string());
    retry
}

/// Group slice indices into dependency-ordered batches (topological levels).
/// No depends_on anywhere → one flat batch (today's behavior). Errors on
/// duplicate names, unknown dependencies, and cycles.
fn batch_slices(slices: &[Slice]) -> anyhow::Result<Vec<Vec<usize>>> {
    let n = slices.len();
    if slices.iter().all(|s| s.depends_on.is_empty()) {
        return Ok(vec![(0..n).collect()]);
    }

    let names: Vec<String> = slices
        .iter()
        .enumerate()
        .map(|(i, s)| s.name.clone().unwrap_or_else(|| format!("slice-{i}")))
        .collect();
    let mut idx_of: HashMap<&str, usize> = HashMap::new();
    for (i, name) in names.iter().enumerate() {
        if idx_of.insert(name.as_str(), i).is_some() {
            anyhow::bail!("duplicate slice name '{name}' — depends_on needs unique names");
        }
    }
    for (i, s) in slices.iter().enumerate() {
        for d in &s.depends_on {
            if !idx_of.contains_key(d.as_str()) {
                anyhow::bail!("slice '{}' depends on unknown slice '{d}'", names[i]);
            }
        }
    }

    let mut placed = vec![false; n];
    let mut remaining = n;
    let mut batches = Vec::new();
    while remaining > 0 {
        let ready: Vec<usize> = (0..n)
            .filter(|&i| {
                !placed[i]
                    && slices[i]
                        .depends_on
                        .iter()
                        .all(|d| placed[idx_of[d.as_str()]])
            })
            .collect();
        if ready.is_empty() {
            let stuck: Vec<&str> = (0..n)
                .filter(|&i| !placed[i])
                .map(|i| names[i].as_str())
                .collect();
            anyhow::bail!("dependency cycle among slices: {}", stuck.join(", "));
        }
        for &i in &ready {
            placed[i] = true;
        }
        remaining -= ready.len();
        batches.push(ready);
    }
    Ok(batches)
}

/// Dispatch all slices in a campaign to bob in parallel.
pub async fn run_campaign(
    campaign_path: &Path,
    jobs: usize,
    bob_cmd: &str,
    propose: bool,
    escalate: bool,
) -> anyhow::Result<DispatchReport> {
    let text = std::fs::read_to_string(campaign_path)?;
    let campaign: Campaign = serde_yaml::from_str(&text)?;
    let campaign_name = campaign.name.clone().unwrap_or_else(|| "campaign".into());
    let total = campaign.slices.len();

    if total == 0 {
        anyhow::bail!("campaign has no slices");
    }

    // Dependency-ordered batches. One batch (no depends_on anywhere) is the
    // flat parallel dispatch this module always did. Multiple batches require
    // committing between them: bob builds each slice in a worktree branched
    // from HEAD, so a later batch only sees earlier results via a commit.
    let batches = batch_slices(&campaign.slices)?;
    let deps_mode = batches.len() > 1;
    if deps_mode {
        if propose {
            anyhow::bail!(
                "--propose is incompatible with depends_on: dependent batches \
                 must commit so later slices build on earlier results"
            );
        }
        if !campaign.auto_commit {
            anyhow::bail!(
                "depends_on requires auto_commit: true — dispatch commits each \
                 batch so the next one builds on it"
            );
        }
    }

    // Slices that run CONCURRENTLY must touch disjoint paths — overlapping
    // editable_paths would race semantically (last diff wins). Across batches
    // overlap is fine: the later slice builds on the committed earlier one.
    for batch in &batches {
        if let Some((a, b, path)) = overlapping_slices(&campaign.slices, batch) {
            anyhow::bail!(
                "slices '{a}' and '{b}' run in the same parallel batch but have \
                 overlapping editable_paths ('{path}') — make one depend_on the \
                 other, or run them via sequential `bob campaign`"
            );
        }
    }

    // Union of slice gates + campaign-level gates BEFORE consuming slices —
    // these re-run against the merged tree after apply to catch cross-slice
    // integration breaks.
    let combined_verify = collect_combined_verify(&campaign);

    let max_jobs = jobs.min(total).max(1);
    eprintln!(
        "hector dispatch: {} slices, {} batch(es), {} jobs, bob='{}'",
        total,
        batches.len(),
        max_jobs,
        bob_cmd
    );

    let apply_dir = std::env::current_dir()?;
    if deps_mode {
        // Untracked files are ignored: batch commits only sweep the staged
        // changes apply_diff creates, and real repos always carry untracked
        // artifacts (.bob/, .maple/, node_modules) that must not block dispatch.
        let dirty = git(&apply_dir, &["status", "--porcelain", "--untracked-files=no"])
            .map_err(|e| anyhow::anyhow!("git status failed: {e}"))?;
        if !dirty.trim().is_empty() {
            anyhow::bail!(
                "depends_on dispatch commits between batches — start from a \
                 clean tree (git status shows pending changes to tracked files)"
            );
        }
    }
    // Pre-dispatch HEAD: deps-mode batches commit as they land, so the final
    // blast-radius diff is working-tree-vs-this-rev.
    let start_rev = git(&apply_dir, &["rev-parse", "HEAD"])
        .map(|s| s.trim().to_string())
        .ok();

    let semaphore = Arc::new(Semaphore::new(max_jobs));
    let start = Instant::now();
    let mut slice_store: Vec<Option<Slice>> = campaign.slices.into_iter().map(Some).collect();
    let mut slices: Vec<SliceResult> = Vec::new();
    let mut failed_names: HashSet<String> = HashSet::new();
    let mut any_applied = false;

    // Tier→endpoint map for round-robin (bob ≥0.4.0; None degrades gracefully).
    let bob_tiers = load_bob_tiers(bob_cmd, &apply_dir);

    for (batch_no, batch) in batches.iter().enumerate() {
        let rr = match &bob_tiers {
            Some(bt) => {
                let rr =
                    assign_round_robin(&slice_store, batch, &bt.tiers, bt.default_tier.as_deref());
                if !rr.is_empty() {
                    let mut named: Vec<String> = rr
                        .iter()
                        .map(|(i, m)| {
                            let n = slice_store[*i]
                                .as_ref()
                                .and_then(|s| s.name.clone())
                                .unwrap_or_else(|| format!("slice-{i}"));
                            format!("{n}={m}")
                        })
                        .collect();
                    named.sort();
                    eprintln!(
                        "hector dispatch: round-robin across tier endpoints — {}",
                        named.join(", ")
                    );
                }
                rr
            }
            None => HashMap::new(),
        };
        let mut handles = Vec::new();
        for &idx in batch {
            let slice = slice_store[idx].take().expect("each slice dispatched once");
            let slice_name = slice.name.clone().unwrap_or_else(|| format!("slice-{idx}"));

            // A failed dependency explicitly fails its downstream chain.
            if let Some(dep) = slice.depends_on.iter().find(|d| failed_names.contains(*d)) {
                eprintln!("hector dispatch: skipping '{slice_name}' — dependency '{dep}' failed");
                failed_names.insert(slice_name.clone());
                let mut sr = error_result(&slice_name, format!("dependency '{dep}' failed"));
                sr.status = "skipped".into();
                slices.push(sr);
                continue;
            }

            let permit = semaphore.clone();
            let bob = bob_cmd.to_string();
            let campaign_dir = apply_dir.clone();
            let rr_model = rr.get(&idx).cloned();
            handles.push(tokio::spawn(async move {
                let _permit = permit.acquire().await.unwrap();
                let slice_start = Instant::now();
                if slice_already_green(&campaign_dir, &slice) {
                    eprintln!(
                        "hector dispatch: '{slice_name}' verify already green on base — already landed, skipping build"
                    );
                    let mut sr = error_result(&slice_name, String::new());
                    sr.status = "already_landed".into();
                    sr.error = None;
                    sr.wall_secs = slice_start.elapsed().as_secs();
                    return sr;
                }
                eprintln!("hector dispatch: starting '{slice_name}'");
                let mut sr =
                    run_slice_escalating(slice, &bob, &campaign_dir, &slice_name, escalate, rr_model)
                        .await;
                sr.wall_secs = slice_start.elapsed().as_secs();
                eprintln!(
                    "hector dispatch: '{slice_name}' done: {} in {}s",
                    sr.status, sr.wall_secs
                );
                sr
            }));
        }

        let mut batch_results = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(sr) => batch_results.push(sr),
                // A panicked task must not abort the whole dispatch — record it.
                Err(e) => batch_results
                    .push(error_result("(panicked)", format!("dispatch task panicked: {e}"))),
            }
        }

        if deps_mode {
            // Merge + commit this batch so the next batch's bob worktrees
            // (branched from HEAD) build on these results.
            let mut landed: Vec<String> = Vec::new();
            for sr in &mut batch_results {
                if sr.status == "converged" && !sr.diff.trim().is_empty() {
                    match apply_diff(&apply_dir, &sr.diff) {
                        Ok(()) => {
                            sr.applied = true;
                            landed.push(sr.name.clone());
                        }
                        Err(e) => {
                            sr.status = "apply_failed".into();
                            sr.error = Some(format!("git apply failed: {e}"));
                        }
                    }
                }
            }
            if !landed.is_empty() {
                any_applied = true;
                let msg = format!("hector dispatch: batch {} ({})", batch_no + 1, landed.join(", "));
                git(&apply_dir, &["commit", "-m", &msg])
                    .map_err(|e| anyhow::anyhow!("commit of batch {} failed: {e}", batch_no + 1))?;
            }
            for sr in &batch_results {
                if !is_success(&sr.status) {
                    failed_names.insert(sr.name.clone());
                }
            }
        }
        slices.extend(batch_results);
    }

    // MERGE PHASE. Each slice built in propose mode in its own isolated worktree
    // (parallel, no contention). Bob's apply writes to the shared main repo, and
    // parallel git ops race on `.git/index.lock` — so we merge sequentially here.
    //   deps_mode: batches already merged + committed above; just verify.
    //   --propose: merge into a throwaway worktree, verify there, write the diff
    //              for inspection, discard. The working tree is untouched.
    //   default:   merge into the working tree (staged), then verify.
    let (integration, proposed_diff) = if deps_mode {
        (run_combined_verify(&apply_dir, any_applied, &combined_verify), None)
    } else if propose {
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

    // Advisory blast radius of what actually landed. Skipped in --propose
    // (working tree untouched) and when nothing applied.
    let landed_any = slices.iter().any(|s| s.applied);
    let impact = if landed_any && !propose {
        let base = if deps_mode { start_rev.as_deref() } else { None };
        let summary = crate::maple::impact_summary(&apply_dir, base);
        if let Some(v) = &summary {
            let touched = v
                .get("changed_symbols")
                .and_then(|a| a.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            eprintln!(
                "hector dispatch: impact — {touched} symbol(s) touched (advisory, see report.impact)"
            );
        }
        summary
    } else {
        None
    };

    let wall = start.elapsed().as_secs();
    let converged = slices.iter().filter(|s| s.status == "converged").count();
    let already_landed = slices.iter().filter(|s| s.status == "already_landed").count();
    let failed = slices.len() - converged - already_landed;

    Ok(DispatchReport {
        campaign: campaign_name,
        jobs: max_jobs,
        total_slices: total,
        converged,
        already_landed,
        failed,
        wall_secs: wall,
        slices,
        integration,
        proposed_diff,
        impact,
    })
}

/// Union of every slice's verify gates plus the campaign-level gates, deduped.
/// Campaign-level `verify_cmds` covers integration gates (full suite,
/// typecheck) that belong to no single slice.
fn collect_combined_verify(campaign: &Campaign) -> Vec<String> {
    let mut gates: Vec<String> = campaign
        .slices
        .iter()
        .filter_map(|s| s.verify_cmds.as_ref())
        .chain(campaign.verify_cmds.as_ref())
        .flatten()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    gates.sort();
    gates.dedup();
    gates
}

/// Find the first pair of slices WITHIN `batch` whose editable_paths overlap
/// (equal, or one is a component-wise prefix of the other, e.g. `src/`
/// contains `src/a.rs`). Only concurrent slices need disjoint paths — across
/// batches the later slice builds on the earlier one's commit. Returns
/// (slice_a, slice_b, offending path) for the error message.
fn overlapping_slices(slices: &[Slice], batch: &[usize]) -> Option<(String, String, String)> {
    let name = |i: usize| {
        slices[i]
            .name
            .clone()
            .unwrap_or_else(|| format!("slice-{i}"))
    };
    for (pos, &i) in batch.iter().enumerate() {
        for &j in &batch[pos + 1..] {
            for a in &slices[i].editable_paths {
                for b in &slices[j].editable_paths {
                    if paths_overlap(a, b) {
                        return Some((name(i), name(j), a.clone()));
                    }
                }
            }
        }
    }
    None
}

/// True when the paths are equal or one contains the other, compared by
/// normalized components so `src/`, `./src` and `src/a.rs` all collide.
fn paths_overlap(a: &str, b: &str) -> bool {
    let comps = |p: &str| -> Vec<String> {
        Path::new(p)
            .components()
            .filter(|c| !matches!(c, std::path::Component::CurDir))
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect()
    };
    let (ca, cb) = (comps(a), comps(b));
    if ca.is_empty() || cb.is_empty() {
        return false;
    }
    let n = ca.len().min(cb.len());
    ca[..n] == cb[..n]
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

/// Run a single slice through bob build. `rr_model` is the dispatch
/// round-robin assignment; the slice's own `model` pin wins over it.
async fn run_slice(
    slice: &Slice,
    bob_cmd: &str,
    campaign_dir: &Path,
    slice_name: &str,
    rr_model: Option<&str>,
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
    if let Some(m) = slice.model.as_deref().or(rr_model) {
        cmd.arg("--model").arg(m);
    }
    for f in &slice.fallback_models {
        cmd.arg("--fallback-model").arg(f);
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
        escalated_to: None,
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

    fn slice_with_paths(name: &str, paths: &[&str]) -> Slice {
        Slice {
            name: Some(name.into()),
            editable_paths: paths.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn disjoint_paths_pass() {
        let slices = vec![
            slice_with_paths("a", &["src/a.rs", "src/a_helper.rs"]),
            slice_with_paths("b", &["src/b.rs"]),
        ];
        assert!(overlapping_slices(&slices, &[0, 1]).is_none());
    }

    #[test]
    fn identical_paths_collide() {
        let slices = vec![
            slice_with_paths("a", &["src/x.rs"]),
            slice_with_paths("b", &["src/x.rs"]),
        ];
        let (a, b, p) = overlapping_slices(&slices, &[0, 1]).unwrap();
        assert_eq!((a.as_str(), b.as_str(), p.as_str()), ("a", "b", "src/x.rs"));
        // Same slices in DIFFERENT batches don't collide — the check is
        // per-parallel-batch only.
        assert!(overlapping_slices(&slices, &[0]).is_none());
        assert!(overlapping_slices(&slices, &[1]).is_none());
    }

    #[test]
    fn directory_containing_file_collides() {
        // `src/` contains `src/a.rs`; `./src` normalizes to `src`.
        let slices = vec![
            slice_with_paths("dir", &["./src"]),
            slice_with_paths("file", &["src/a.rs"]),
        ];
        assert!(overlapping_slices(&slices, &[0, 1]).is_some());
        // But a sibling directory does not.
        let ok = vec![
            slice_with_paths("dir", &["src/"]),
            slice_with_paths("other", &["tests/a.rs"]),
        ];
        assert!(overlapping_slices(&ok, &[0, 1]).is_none());
    }

    fn slice_with_deps(name: &str, deps: &[&str]) -> Slice {
        Slice {
            name: Some(name.into()),
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn no_deps_is_one_flat_batch() {
        let slices = vec![slice_with_deps("a", &[]), slice_with_deps("b", &[])];
        assert_eq!(batch_slices(&slices).unwrap(), vec![vec![0, 1]]);
    }

    #[test]
    fn chain_and_diamond_batch_in_dependency_order() {
        // a → (b, c) → d: three levels, b and c parallel in the middle.
        let slices = vec![
            slice_with_deps("a", &[]),
            slice_with_deps("b", &["a"]),
            slice_with_deps("c", &["a"]),
            slice_with_deps("d", &["b", "c"]),
        ];
        assert_eq!(
            batch_slices(&slices).unwrap(),
            vec![vec![0], vec![1, 2], vec![3]]
        );
    }

    #[test]
    fn unknown_dep_and_cycle_and_dup_names_error() {
        let unknown = vec![slice_with_deps("a", &["ghost"])];
        assert!(batch_slices(&unknown).unwrap_err().to_string().contains("unknown slice"));

        let cycle = vec![slice_with_deps("a", &["b"]), slice_with_deps("b", &["a"])];
        assert!(batch_slices(&cycle).unwrap_err().to_string().contains("cycle"));

        let dup = vec![slice_with_deps("a", &[]), slice_with_deps("a", &["a"])];
        assert!(batch_slices(&dup).unwrap_err().to_string().contains("duplicate"));
    }

    #[test]
    fn tier_ladder_escalates_and_tops_out() {
        assert_eq!(next_tier(None), Some("medium"));
        assert_eq!(next_tier(Some("cheap")), Some("medium"));
        assert_eq!(next_tier(Some("medium")), Some("large"));
        assert_eq!(next_tier(Some("large")), Some("frontier"));
        assert_eq!(next_tier(Some("frontier")), None);
        assert_eq!(next_tier(Some("weird")), None);
    }

    #[test]
    fn combined_verify_includes_campaign_gates() {
        let yaml = r#"
name: c
verify_cmds: ["npm run test:all", "echo ok"]
slices:
  - name: s1
    verify_cmds: ["echo ok"]
"#;
        let campaign: Campaign = serde_yaml::from_str(yaml).unwrap();
        let gates = collect_combined_verify(&campaign);
        assert_eq!(gates, vec!["echo ok".to_string(), "npm run test:all".to_string()]);
    }

    #[test]
    fn run_gate_reports_pass_and_fail() {
        let d = std::env::temp_dir();
        assert!(run_gate(&d, "true").is_ok());
        let e = run_gate(&d, "false").unwrap_err();
        assert!(e.contains("failed"), "fail message names the gate: {e}");
    }

    #[test]
    fn skip_green_preflight_detects_landed_slice() {
        let d = std::env::temp_dir();
        let green = Slice {
            verify_cmds: Some(vec!["true".into(), "true".into()]),
            ..Default::default()
        };
        assert!(slice_already_green(&d, &green));
        // One red gate → not landed, dispatch proceeds.
        let red = Slice {
            verify_cmds: Some(vec!["true".into(), "false".into()]),
            ..Default::default()
        };
        assert!(!slice_already_green(&d, &red));
        // No gates → never "landed" (nothing was proven).
        assert!(!slice_already_green(&d, &Slice::default()));
    }

    #[test]
    fn round_robin_spreads_unpinned_same_tier_slices() {
        let tiers: HashMap<String, Vec<String>> = [(
            "medium".to_string(),
            vec!["qwen-193".to_string(), "gemma-133".to_string()],
        )]
        .into();
        let slice = |tier: Option<&str>, model: Option<&str>| {
            Some(Slice {
                tier: tier.map(String::from),
                model: model.map(String::from),
                ..Default::default()
            })
        };

        // Three unpinned slices on the default tier alternate across members.
        let slices = vec![slice(None, None), slice(None, None), slice(None, None)];
        let rr = assign_round_robin(&slices, &[0, 1, 2], &tiers, Some("medium"));
        assert_eq!(rr[&0], "qwen-193");
        assert_eq!(rr[&1], "gemma-133");
        assert_eq!(rr[&2], "qwen-193");

        // A pinned slice is excluded; the two unpinned ones still spread.
        let slices = vec![slice(None, Some("glm")), slice(None, None), slice(None, None)];
        let rr = assign_round_robin(&slices, &[0, 1, 2], &tiers, Some("medium"));
        assert!(!rr.contains_key(&0), "explicit pin wins");
        assert_eq!(rr.len(), 2);

        // A single unpinned slice keeps bob's stats-ranked pick.
        let slices = vec![slice(None, None)];
        assert!(assign_round_robin(&slices, &[0], &tiers, Some("medium")).is_empty());

        // No default tier and no slice tier → nothing to spread across.
        let slices = vec![slice(None, None), slice(None, None)];
        assert!(assign_round_robin(&slices, &[0, 1], &tiers, None).is_empty());

        // Unknown tier or single-member tier → no assignment.
        let one: HashMap<String, Vec<String>> =
            [("solo".to_string(), vec!["only".to_string()])].into();
        let slices = vec![slice(Some("solo"), None), slice(Some("solo"), None)];
        assert!(assign_round_robin(&slices, &[0, 1], &one, None).is_empty());
    }

    fn report(failed: usize, integration: Option<IntegrationReport>) -> DispatchReport {
        DispatchReport {
            campaign: "c".into(),
            jobs: 1,
            total_slices: 1,
            converged: 0,
            already_landed: 0,
            failed,
            wall_secs: 0,
            slices: vec![],
            integration,
            proposed_diff: None,
            impact: None,
        }
    }

    #[test]
    fn succeeded_requires_no_failures_and_green_integration() {
        assert!(report(0, None).succeeded());
        assert!(!report(1, None).succeeded());
        let green = IntegrationReport { verified: true, gates_run: 1, failures: vec![] };
        let red = IntegrationReport { verified: false, gates_run: 1, failures: vec!["x".into()] };
        assert!(report(0, Some(green)).succeeded());
        assert!(!report(0, Some(red)).succeeded());
    }
}
