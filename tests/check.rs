use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn tmp_file(name: &str, body: &str) -> std::path::PathBuf {
    let dir = tmp_dir("hector-check");
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    path
}

fn tmp_dir(prefix: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "{prefix}-{}-{:?}-{}",
        std::process::id(),
        std::thread::current().id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn hector_check(body: &str) -> std::process::Output {
    let path = tmp_file("campaign.yaml", body);
    Command::new(env!("CARGO_BIN_EXE_hector"))
        .args(["check", "--file"])
        .arg(path)
        .output()
        .unwrap()
}

fn hector(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hector"))
        .args(args)
        .output()
        .unwrap()
}

fn hector_in(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hector"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn valid_campaign_passes() {
    let out = hector_check(
        r#"
name: ok
slices:
  - name: one
    task: Implement the tiny thing.
    verify_cmds: ["cargo test focused"]
    editable_paths: ["src/planner.rs"]
    reference_paths: ["tests/check.rs"]
    judge_policy: retry_on_fail
    max_changed_files: 1
    max_changed_lines: 80
"#,
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn missing_verify_gate_fails() {
    let out = hector_check(
        r#"
name: bad
slices:
  - task: Implement without proof.
    editable_paths: ["src/planner.rs"]
    max_changed_files: 1
    max_changed_lines: 80
"#,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("verify_cmds"));
}

#[test]
fn editable_test_file_fails() {
    let out = hector_check(
        r#"
name: bad
slices:
  - task: Cheat the test.
    verify_cmds: ["cargo test focused"]
    editable_paths: ["tests/check.rs"]
    max_changed_files: 1
    max_changed_lines: 80
"#,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("test files must be reference-only"));
}

#[test]
fn nested_test_directory_fails() {
    let out = hector_check(
        r#"
name: bad
slices:
  - task: Cheat a nested test helper.
    verify_cmds: ["cargo test focused"]
    editable_paths: ["src/tests/helper.rs"]
    max_changed_files: 1
    max_changed_lines: 80
"#,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("test files must be reference-only"));
}

#[test]
fn dependency_churn_fails() {
    let out = hector_check(
        r#"
name: bad
slices:
  - task: Add one helper.
    verify_cmds: ["cargo test focused"]
    editable_paths: ["Cargo.toml"]
    max_changed_files: 1
    max_changed_lines: 80
"#,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("dependency churn"));
}

#[test]
fn missing_scope_caps_fails() {
    let out = hector_check(
        r#"
name: bad
slices:
  - task: Too loose.
    verify_cmds: ["cargo test focused"]
    editable_paths: ["src/"]
"#,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("max_changed_files"));
}

#[test]
fn empty_required_fields_fail() {
    let out = hector_check(
        r#"
name: bad
slices:
  - task: ""
    verify_cmds: [""]
    editable_paths: []
    max_changed_files: 0
    max_changed_lines: 0
"#,
    );
    assert!(!out.status.success());
}

#[test]
fn missing_editable_paths_fails() {
    let out = hector_check(
        r#"
name: bad
slices:
  - task: No writable scope.
    verify_cmds: ["cargo test focused"]
    max_changed_files: 1
    max_changed_lines: 80
"#,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("editable_paths"));
}

#[test]
fn dangerous_paths_fail() {
    for path in ["/tmp/x.rs", "../src/lib.rs", "src/../tests/check.rs"] {
        let out = hector_check(&format!(
            r#"
name: bad
slices:
  - task: Dangerous path.
    verify_cmds: ["cargo test focused"]
    editable_paths: ["{path}"]
    max_changed_files: 1
    max_changed_lines: 80
"#
        ));
        assert!(!out.status.success(), "{path}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("unsafe path"),
            "{path}"
        );
    }
}

#[test]
fn dependency_churn_in_subdir_fails() {
    let out = hector_check(
        r#"
name: bad
slices:
  - task: Add one helper.
    verify_cmds: ["cargo test focused"]
    editable_paths: ["frontend/package.json"]
    max_changed_files: 1
    max_changed_lines: 80
"#,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("dependency churn"));
}

#[test]
fn plan_needs_input_without_verify_or_editable_paths() {
    let dir = tmp_dir("plan-needs-input");
    let out = hector_in(&dir, &["plan", "--task", "Add a useful behavior"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"status\": \"needs_input\""));
    assert!(stdout.contains("What command proves this slice is correct?"));
    assert!(stdout.contains("Which files or directories may Bob edit?"));
}

#[test]
fn plan_emits_campaign_that_check_accepts() {
    let out = hector(&[
        "plan",
        "--name",
        "tiny-client",
        "--task",
        "Add a tiny client helper.",
        "--verify",
        "cargo test tiny_client",
        "--editable-path",
        "src/client.rs",
        "--reference-path",
        "tests/client.rs",
        "--max-changed-files",
        "1",
        "--max-changed-lines",
        "80",
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("slices:"));
    assert!(stdout.contains("editable_paths:"));

    let campaign = tmp_file("campaign.yaml", &stdout);
    let checked = Command::new(env!("CARGO_BIN_EXE_hector"))
        .args(["check", "--file"])
        .arg(campaign)
        .output()
        .unwrap();
    assert!(
        checked.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&checked.stderr)
    );
}

#[test]
fn review_accepts_clean_completed_result() {
    let campaign = tmp_file(
        "campaign.yaml",
        r#"
name: review-ok
slices:
  - task: Implement the reviewed thing.
    verify_cmds: ["cargo test reviewed"]
    editable_paths: ["src/planner.rs"]
    max_changed_files: 1
    max_changed_lines: 80
"#,
    );
    let result = tmp_file(
        "result.json",
        r#"{"status":"completed","changed_files":["src/planner.rs"]}"#,
    );
    let out = Command::new(env!("CARGO_BIN_EXE_hector"))
        .args(["review", "--campaign"])
        .arg(campaign)
        .arg("--bob-result")
        .arg(result)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"decision\": \"accept\""));
}

#[test]
fn review_rejects_out_of_scope_prefix_escape() {
    let campaign = tmp_file(
        "campaign.yaml",
        r#"
name: review-bad
slices:
  - task: Implement a scoped thing.
    verify_cmds: ["cargo test scoped"]
    editable_paths: ["src"]
    max_changed_files: 1
    max_changed_lines: 80
"#,
    );
    let result = tmp_file(
        "result.json",
        r#"{"status":"completed","changed_files":["src2/planner.rs"]}"#,
    );
    let out = Command::new(env!("CARGO_BIN_EXE_hector"))
        .args(["review", "--campaign"])
        .arg(campaign)
        .arg("--bob-result")
        .arg(result)
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"decision\": \"revise_campaign\""));
    assert!(stdout.contains("changed file outside editable_paths"));
}

#[test]
fn review_sends_needs_review_to_human() {
    let campaign = tmp_file(
        "campaign.yaml",
        r#"
name: review-human
slices:
  - task: Implement a thing needing judgment.
    verify_cmds: ["cargo test judgment"]
    editable_paths: ["src/planner.rs"]
    max_changed_files: 1
    max_changed_lines: 80
"#,
    );
    let result = tmp_file(
        "result.json",
        r#"{"status":"needs_review","next_action":"review_candidate","changed_files":["src/planner.rs"]}"#,
    );
    let out = Command::new(env!("CARGO_BIN_EXE_hector"))
        .args(["review", "--campaign"])
        .arg(campaign)
        .arg("--bob-result")
        .arg(result)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\"decision\": \"accept_for_human_review\"")
    );
}

#[test]
fn review_splits_scope_exceeded_variants() {
    let campaign = tmp_file(
        "campaign.yaml",
        r#"
name: review-split
slices:
  - task: Implement a thing that was too broad.
    verify_cmds: ["cargo test broad"]
    editable_paths: ["src/planner.rs"]
    max_changed_files: 1
    max_changed_lines: 80
"#,
    );
    let result = tmp_file(
        "result.json",
        r#"{"status":"scope_exceeded","changed_files":["src/planner.rs"]}"#,
    );
    let out = Command::new(env!("CARGO_BIN_EXE_hector"))
        .args(["review", "--campaign"])
        .arg(campaign)
        .arg("--bob-result")
        .arg(result)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"decision\": \"split_task\""));
}

#[test]
fn frontier_brief_explains_handoff_contract() {
    let out = hector(&["frontier-brief"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Hector Frontier Brief"));
    assert!(stdout.contains("hector plan"));
    assert!(stdout.contains("editable_paths"));
    assert!(stdout.contains("needs_input"));
}

#[test]
fn compact_frontier_brief_is_short_and_actionable() {
    let out = hector(&["frontier-brief", "--compact"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.len() <= 1200);
    assert!(stdout.contains("verify_cmds"));
    assert!(stdout.contains("editable_paths"));
    assert!(stdout.contains("hector check"));
    assert!(stdout.contains("hector review"));
}

#[test]
fn init_refuses_existing_config_unless_forced() {
    let dir = tmp_dir("hector-init");
    let first = hector_in(&dir, &["init"]);
    assert!(first.status.success());
    fs::write(dir.join("hector.yaml"), "custom: true\n").unwrap();

    let refused = hector_in(&dir, &["init"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--force"));
    assert_eq!(
        fs::read_to_string(dir.join("hector.yaml")).unwrap(),
        "custom: true\n"
    );

    let forced = hector_in(&dir, &["init", "--force"]);
    assert!(forced.status.success());
    assert!(fs::read_to_string(dir.join("hector.yaml"))
        .unwrap()
        .contains("default_max_changed_files"));
}

#[test]
fn plan_uses_config_defaults_and_cli_overrides() {
    let dir = tmp_dir("hector-config");
    fs::write(
        dir.join("hector.yaml"),
        "scope:\n  default_max_changed_files: 1\n  default_max_changed_lines: 77\njudge:\n  default_policy: ask_human\nbob:\n  campaign_auto_commit: false\n",
    )
    .unwrap();

    let out = hector_in(
        &dir,
        &[
            "plan",
            "--task",
            "Use config defaults.",
            "--verify",
            "true",
            "--editable-path",
            "src/lib.rs",
        ],
    );
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("auto_commit: false"));
    assert!(stdout.contains("max_changed_files: 1"));
    assert!(stdout.contains("max_changed_lines: 77"));
    assert!(stdout.contains("judge_policy: ask_human"));

    let override_out = hector_in(
        &dir,
        &[
            "plan",
            "--task",
            "Override config defaults.",
            "--verify",
            "true",
            "--editable-path",
            "src/lib.rs",
            "--max-changed-files",
            "3",
            "--max-changed-lines",
            "88",
            "--judge-policy",
            "retry_on_fail",
        ],
    );
    assert!(override_out.status.success());
    let stdout = String::from_utf8_lossy(&override_out.stdout);
    assert!(stdout.contains("max_changed_files: 3"));
    assert!(stdout.contains("max_changed_lines: 88"));
    assert!(stdout.contains("judge_policy: retry_on_fail"));
}

#[test]
fn plan_reports_bad_hector_yaml() {
    let dir = tmp_dir("hector-bad-config");
    fs::write(dir.join("hector.yaml"), "scope: [").unwrap();
    let out = hector_in(
        &dir,
        &[
            "plan",
            "--task",
            "Bad config.",
            "--verify",
            "true",
            "--editable-path",
            "src/lib.rs",
        ],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("hector.yaml"));
}

#[test]
fn plan_with_symbol_degrades_gracefully_when_maple_missing() {
    let dir = tmp_dir("hector-no-maple");
    // Empty PATH → `maple` cannot be found. Explicit paths still work, and the
    // degradation is announced on stderr instead of failing the plan.
    let out = Command::new(env!("CARGO_BIN_EXE_hector"))
        .current_dir(&dir)
        .env("PATH", "")
        .args([
            "plan",
            "--task",
            "Add a focused behavior.",
            "--verify",
            "true",
            "--editable-path",
            "src/lib.rs",
            "--symbol",
            "whatever",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("maple not found"), "warns about fallback: {stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("editable_paths"), "campaign still emitted: {stdout}");

    // Without explicit paths either, the planner's normal needs_input flow
    // answers instead of a hard error.
    let out = Command::new(env!("CARGO_BIN_EXE_hector"))
        .current_dir(&dir)
        .env("PATH", "")
        .args(["plan", "--task", "Add a thing.", "--verify", "true", "--symbol", "whatever"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("needs_input"));
}

// ---- dispatch e2e with a fake bob -----------------------------------------
// A tiny sh script stands in for bob: task "a" converges with a diff adding
// a.txt; task "b" converges ONLY if a.txt already exists in the repo (proving
// dependency ordering + the between-batch commit); task "c" converges only
// when a --tier flag is present (proving --escalate retries at a higher tier);
// task "fail" never converges.
const FAKE_BOB: &str = r#"#!/bin/sh
task="$3"
case "$*" in *--tier*) tiered=1;; *) tiered=0;; esac
emit_ok() {
  f="$1"
  printf '{"status":"converged","changed_files":["%s"],"final_diff":"diff --git a/%s b/%s\\nnew file mode 100644\\n--- /dev/null\\n+++ b/%s\\n@@ -0,0 +1 @@\\n+hello\\n"}' "$f" "$f" "$f" "$f"
}
emit_fail() { printf '{"status":"not_converged","stop_reason":"JudgeRejected","final_diff":""}'; }
case "$task" in
  a) emit_ok a.txt ;;
  b) if [ -f a.txt ]; then emit_ok b.txt; else emit_fail; fi ;;
  c) if [ "$tiered" = 1 ]; then emit_ok c.txt; else emit_fail; fi ;;
  *) emit_fail ;;
esac
"#;

fn init_dispatch_repo(dir: &std::path::Path, campaign: &str) -> std::path::PathBuf {
    let git = |args: &[&str]| {
        let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@test"]);
    git(&["config", "user.name", "test"]);
    fs::write(dir.join("campaign.yaml"), campaign).unwrap();
    let bob = dir.join("fake-bob.sh");
    fs::write(&bob, FAKE_BOB).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bob, fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);
    bob
}

#[test]
fn dispatch_runs_dependent_batches_in_order_and_commits() {
    let dir = tmp_dir("hector-dispatch-deps");
    let bob = init_dispatch_repo(
        &dir,
        r#"
name: deps
auto_commit: true
slices:
  - name: a
    task: a
    verify_cmds: ["true"]
    editable_paths: ["a.txt"]
  - name: b
    task: b
    depends_on: [a]
    verify_cmds: ["true"]
    editable_paths: ["a.txt", "b.txt"]
"#,
    );
    // Note: b's editable_paths overlap a's — legal BECAUSE they're in
    // different batches.
    let out = hector_in(
        &dir,
        &["dispatch", "--file", "campaign.yaml", "--bob-cmd", bob.to_str().unwrap()],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    // b converged — which is only possible if a.txt was committed before b ran.
    assert!(dir.join("a.txt").exists() && dir.join("b.txt").exists(), "{stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"converged\": 2"), "{stdout}");
    // init + one commit per batch = 3.
    let count = Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "3");
}

#[test]
fn dispatch_skips_dependents_of_failed_slices() {
    let dir = tmp_dir("hector-dispatch-skip");
    let bob = init_dispatch_repo(
        &dir,
        r#"
name: skip
auto_commit: true
slices:
  - name: doomed
    task: fail
    verify_cmds: ["true"]
    editable_paths: ["x.txt"]
  - name: downstream
    task: a
    depends_on: [doomed]
    verify_cmds: ["true"]
    editable_paths: ["a.txt"]
"#,
    );
    let out = hector_in(
        &dir,
        &["dispatch", "--file", "campaign.yaml", "--bob-cmd", bob.to_str().unwrap()],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"converged\": 0"), "{stdout}");
    assert!(stdout.contains("\"skipped\""), "{stdout}");
    assert!(stdout.contains("dependency 'doomed' failed"), "{stdout}");
    assert!(!dir.join("a.txt").exists(), "downstream slice must not have run");
}

#[test]
fn dispatch_escalates_tier_on_failure_when_asked() {
    let dir = tmp_dir("hector-dispatch-escalate");
    let bob = init_dispatch_repo(
        &dir,
        r#"
name: escalate
slices:
  - name: c
    task: c
    verify_cmds: ["true"]
    editable_paths: ["c.txt"]
"#,
    );
    // Without --escalate: the tierless attempt fails and stays failed.
    let out = hector_in(
        &dir,
        &["dispatch", "--file", "campaign.yaml", "--bob-cmd", bob.to_str().unwrap()],
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"converged\": 0"));

    // With --escalate: retried once at tier medium and converges.
    let out = hector_in(
        &dir,
        &[
            "dispatch", "--file", "campaign.yaml",
            "--bob-cmd", bob.to_str().unwrap(),
            "--escalate",
        ],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"converged\": 1"), "{stdout}");
    assert!(stdout.contains("\"escalated_to\": \"medium\""), "{stdout}");
    assert!(dir.join("c.txt").exists());
}

#[test]
fn dispatch_rejects_depends_on_with_propose_or_dirty_tree() {
    let dir = tmp_dir("hector-dispatch-guards");
    let bob = init_dispatch_repo(
        &dir,
        r#"
name: guards
auto_commit: true
slices:
  - name: a
    task: a
    verify_cmds: ["true"]
    editable_paths: ["a.txt"]
  - name: b
    task: b
    depends_on: [a]
    verify_cmds: ["true"]
    editable_paths: ["b.txt"]
"#,
    );
    let out = hector_in(
        &dir,
        &[
            "dispatch", "--file", "campaign.yaml",
            "--bob-cmd", bob.to_str().unwrap(),
            "--propose",
        ],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--propose is incompatible"));

    fs::write(dir.join("dirty.txt"), "uncommitted").unwrap();
    let out = hector_in(
        &dir,
        &["dispatch", "--file", "campaign.yaml", "--bob-cmd", bob.to_str().unwrap()],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("clean tree"));
}

#[test]
fn plan_carries_forward_project_lessons() {
    let dir = tmp_dir("hector-lessons");
    fs::create_dir_all(dir.join(".hector")).unwrap();
    fs::write(
        dir.join(".hector/lessons.md"),
        "vitest here needs --runInBand or the AI gates flake",
    )
    .unwrap();
    let out = hector_in(
        &dir,
        &[
            "plan",
            "--task",
            "Add a focused behavior.",
            "--verify",
            "true",
            "--editable-path",
            "src/lib.rs",
        ],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Project lessons"), "{stdout}");
    assert!(stdout.contains("--runInBand"), "{stdout}");
}
