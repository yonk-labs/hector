//! Repo convention detection — determines language, test runner, and test
//! directory from the repo root. Deterministic; no model calls.
//!
//! For JS/TS repos the layout is NOT assumed: an existing test file is located
//! and its language, extension, and directory become the convention. A repo
//! with co-located `core/*.test.ts` must not be told to write
//! `tests/*.test.js` (that exact miss produced an unrunnable campaign in the
//! expansion-empire pilot).

use std::path::Path;

#[derive(Debug, Clone)]
pub struct Conventions {
    pub language: String,
    pub test_runner: String,
    pub test_dir: String,
    pub test_pattern: String,
    pub test_file_ext: String,
    /// Repo-relative path of an existing test file, as a concrete example the
    /// test-writer prompt can point at. None when the repo has no tests yet.
    pub example_test: Option<String>,
}

pub fn detect(repo_root: &Path) -> Option<Conventions> {
    if repo_root.join("Cargo.toml").exists() {
        return Some(Conventions {
            language: "rust".into(),
            test_runner: "cargo test".into(),
            test_dir: "tests".into(),
            test_pattern: "tests/*_test.rs or #[cfg(test)] mod in src/".into(),
            test_file_ext: "_test.rs".into(),
            example_test: None,
        });
    }
    if let Ok(text) = std::fs::read_to_string(repo_root.join("package.json")) {
        if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) {
            let deps = pkg
                .get("devDependencies")
                .or_else(|| pkg.get("dependencies"))
                .and_then(|d| d.as_object());
            if let Some(deps) = deps {
                let ts = deps.contains_key("typescript")
                    || repo_root.join("tsconfig.json").exists();
                if deps.contains_key("vitest") {
                    return Some(js_conv(repo_root, "vitest", "npx vitest run", ts));
                }
                if deps.contains_key("jest") {
                    return Some(js_conv(repo_root, "jest", "npx jest", ts));
                }
            }
        }
    }
    if repo_root.join("pytest.ini").exists()
        || repo_root.join("conftest.py").exists()
        || repo_root.join("pyproject.toml").exists()
    {
        return Some(Conventions {
            language: "python".into(),
            test_runner: "pytest".into(),
            test_dir: "tests".into(),
            test_pattern: "tests/test_*.py".into(),
            test_file_ext: "_test.py".into(),
            example_test: None,
        });
    }
    if repo_root.join("go.mod").exists() {
        return Some(Conventions {
            language: "go".into(),
            test_runner: "go test".into(),
            test_dir: ".".into(),
            test_pattern: "*_test.go".into(),
            test_file_ext: "_test.go".into(),
            example_test: None,
        });
    }
    None
}

/// JS/TS conventions, derived from an existing test file when one exists —
/// its directory and extension ARE the convention. Falls back to
/// tests/*.test.js|ts when the repo has no tests yet.
fn js_conv(root: &Path, runner: &str, cmd: &str, ts: bool) -> Conventions {
    let language = if ts { "typescript" } else { "javascript" };
    let example = find_test_file(root, root, 3);
    let (test_dir, ext) = match &example {
        Some(rel) => {
            let p = Path::new(rel);
            let dir = p
                .parent()
                .filter(|d| !d.as_os_str().is_empty())
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".into());
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            let ext = [".test.ts", ".test.js", ".spec.ts", ".spec.js"]
                .iter()
                .find(|s| name.ends_with(*s))
                .copied()
                .unwrap_or(if ts { ".test.ts" } else { ".test.js" });
            (dir, ext.to_string())
        }
        None => (
            "tests".to_string(),
            (if ts { ".test.ts" } else { ".test.js" }).to_string(),
        ),
    };
    Conventions {
        language: language.into(),
        test_runner: cmd.into(),
        test_pattern: format!("{test_dir}/*{ext} ({runner})"),
        test_dir,
        test_file_ext: ext,
        example_test: example,
    }
}

/// Bounded recursive scan for an existing test file, returning its
/// repo-relative path. Skips dependency/build/artifact dirs.
/// ponytail: first match wins (alphabetical); ranking by count if repos mix layouts.
fn find_test_file(root: &Path, dir: &Path, depth: u32) -> Option<String> {
    const SKIP: &[&str] = &[
        "node_modules", ".git", ".bob", ".hector", "dist", "build", "coverage", "target",
    ];
    let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in &entries {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            if [".test.ts", ".test.js", ".spec.ts", ".spec.js"]
                .iter()
                .any(|s| name.ends_with(s))
            {
                return p
                    .strip_prefix(root)
                    .ok()
                    .map(|r| r.to_string_lossy().to_string());
            }
        }
    }
    if depth == 0 {
        return None;
    }
    for e in &entries {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let name = e.file_name();
        let name = name.to_string_lossy();
        if SKIP.contains(&name.as_ref()) || name.starts_with('.') {
            continue;
        }
        if let Some(found) = find_test_file(root, &p, depth - 1) {
            return Some(found);
        }
    }
    None
}

impl Conventions {
    pub fn prompt_block(&self) -> String {
        let mut block = format!(
            "- Language: {}\n- Test runner: {}\n- Test directory: {}\n- Test file pattern: {}\n- Test file extension: {}",
            self.language, self.test_runner, self.test_dir, self.test_pattern, self.test_file_ext
        );
        if let Some(example) = &self.example_test {
            block.push_str(&format!(
                "\n- Existing test example: {example} — put the new test in the same directory style, same language, same extension"
            ));
        }
        block
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust() {
        let tmp = tempfile_dir();
        std::fs::write(tmp.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let conv = detect(&tmp).unwrap();
        assert_eq!(conv.language, "rust");
        assert_eq!(conv.test_runner, "cargo test");
    }

    #[test]
    fn detects_jest() {
        let tmp = tempfile_dir();
        std::fs::write(
            tmp.join("package.json"),
            r#"{"devDependencies": {"jest": "^29"}}"#,
        )
        .unwrap();
        let conv = detect(&tmp).unwrap();
        assert_eq!(conv.language, "javascript");
        assert!(conv.test_runner.contains("jest"));
        // No tests yet → sensible JS defaults.
        assert_eq!(conv.test_dir, "tests");
        assert_eq!(conv.test_file_ext, ".test.js");
        assert!(conv.example_test.is_none());
    }

    #[test]
    fn detects_typescript_with_colocated_tests() {
        // The expansion-empire shape: vitest + typescript, tests co-located
        // in core/*.test.ts. The convention must NOT say tests/*.test.js.
        let tmp = tempfile_dir();
        std::fs::write(
            tmp.join("package.json"),
            r#"{"devDependencies": {"vitest": "^2", "typescript": "^5"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.join("core")).unwrap();
        std::fs::write(tmp.join("core/economy.test.ts"), "// test").unwrap();
        std::fs::write(tmp.join("core/economy.ts"), "// src").unwrap();

        let conv = detect(&tmp).unwrap();
        assert_eq!(conv.language, "typescript");
        assert_eq!(conv.test_dir, "core");
        assert_eq!(conv.test_file_ext, ".test.ts");
        assert_eq!(conv.example_test.as_deref(), Some("core/economy.test.ts"));
        let block = conv.prompt_block();
        assert!(block.contains("core/economy.test.ts"), "{block}");
    }

    #[test]
    fn test_scan_skips_dependency_dirs() {
        let tmp = tempfile_dir();
        std::fs::write(
            tmp.join("package.json"),
            r#"{"devDependencies": {"vitest": "^2"}}"#,
        )
        .unwrap();
        // Only "tests" under node_modules and .bob — must be ignored.
        for d in ["node_modules/pkg", ".bob/worktrees/w1"] {
            std::fs::create_dir_all(tmp.join(d)).unwrap();
            std::fs::write(tmp.join(d).join("x.test.js"), "// dep test").unwrap();
        }
        let conv = detect(&tmp).unwrap();
        assert!(conv.example_test.is_none());
    }

    #[test]
    fn returns_none_for_empty() {
        let tmp = tempfile_dir();
        assert!(detect(&tmp).is_none());
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hector-conv-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
