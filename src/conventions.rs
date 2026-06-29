//! Repo convention detection — determines language, test runner, and test
//! directory from the repo root. Deterministic; no model calls.

use std::path::Path;

#[derive(Debug, Clone)]
pub struct Conventions {
    pub language: String,
    pub test_runner: String,
    pub test_dir: String,
    pub test_pattern: String,
    pub test_file_ext: String,
}

pub fn detect(repo_root: &Path) -> Option<Conventions> {
    if repo_root.join("Cargo.toml").exists() {
        return Some(Conventions {
            language: "rust".into(),
            test_runner: "cargo test".into(),
            test_dir: "tests".into(),
            test_pattern: "tests/*_test.rs or #[cfg(test)] mod in src/".into(),
            test_file_ext: "_test.rs".into(),
        });
    }
    if let Ok(text) = std::fs::read_to_string(repo_root.join("package.json")) {
        if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text) {
            let deps = pkg
                .get("devDependencies")
                .or_else(|| pkg.get("dependencies"))
                .and_then(|d| d.as_object());
            if let Some(deps) = deps {
                if deps.contains_key("vitest") {
                    return Some(js_conv("vitest", "npx vitest run"));
                }
                if deps.contains_key("jest") {
                    return Some(js_conv("jest", "npx jest"));
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
        });
    }
    if repo_root.join("go.mod").exists() {
        return Some(Conventions {
            language: "go".into(),
            test_runner: "go test".into(),
            test_dir: ".".into(),
            test_pattern: "*_test.go".into(),
            test_file_ext: "_test.go".into(),
        });
    }
    None
}

fn js_conv(runner: &str, cmd: &str) -> Conventions {
    Conventions {
        language: "javascript".into(),
        test_runner: cmd.into(),
        test_dir: "tests".into(),
        test_pattern: format!("tests/*.test.js ({runner})"),
        test_file_ext: ".test.js".into(),
    }
}

impl Conventions {
    pub fn prompt_block(&self) -> String {
        format!(
            "- Language: {}\n- Test runner: {}\n- Test directory: {}\n- Test file pattern: {}",
            self.language, self.test_runner, self.test_dir, self.test_pattern
        )
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
