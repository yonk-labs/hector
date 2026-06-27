use std::fs;
use std::process::Command;

fn tmp_file(name: &str, body: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("hector-check-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    path
}

fn hector_check(body: &str) -> std::process::Output {
    let path = tmp_file("campaign.yaml", body);
    Command::new(env!("CARGO_BIN_EXE_hector"))
        .args(["check", "--file"])
        .arg(path)
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
