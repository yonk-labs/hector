use serde::Deserialize;
use std::path::{Component, Path};

#[derive(Deserialize)]
struct Campaign {
    slices: Vec<Slice>,
}

#[derive(Deserialize)]
struct Slice {
    verify_cmds: Option<Vec<String>>,
    editable_paths: Vec<String>,
    max_changed_files: Option<u64>,
    max_changed_lines: Option<u64>,
}

pub fn plan(_task: &str, _spec: Option<&str>) -> anyhow::Result<String> {
    anyhow::bail!("planner is specified but not implemented yet; see HECTOR_SPEC.md")
}

pub fn check(path: &Path) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)?;
    let campaign: Campaign = serde_yaml::from_str(&content)?;

    if campaign.slices.is_empty() {
        anyhow::bail!("campaign must have at least one slice");
    }

    for slice in &campaign.slices {
        if slice.verify_cmds.as_ref().is_none_or(Vec::is_empty) {
            anyhow::bail!("slice missing verify_cmds");
        }
        if slice.max_changed_files.is_none() {
            anyhow::bail!("slice missing max_changed_files");
        }
        if slice.max_changed_lines.is_none() {
            anyhow::bail!("slice missing max_changed_lines");
        }
        for editable_path in &slice.editable_paths {
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
    matches!(
        path,
        "Cargo.toml"
            | "Cargo.lock"
            | "package.json"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
    )
}
