//! Optional maple integration: derive slice scope from the code-symbol graph.
//!
//! `maple bundle --symbol S` returns the file defining S plus every caller and
//! resolved callee file, with an approximate token count against a budget.
//! Hector uses that to fill `editable_paths`/`reference_paths` the caller
//! didn't provide, and to refuse tasks whose context exceeds the budget
//! BEFORE dispatch — the split decision goes back to the orchestrator
//! (hector surfaces it, it does not auto-split).

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug)]
pub struct MapleScope {
    /// Files defining the requested symbols — what bob may edit.
    pub editable_paths: Vec<String>,
    /// Caller/callee files (minus editables) — context bob may read.
    pub reference_paths: Vec<String>,
    /// Combined approximate prompt tokens across all requested symbols.
    pub total_tokens: u64,
}

#[derive(Deserialize)]
struct Bundle {
    target: Target,
    #[serde(default)]
    callees: Vec<Callee>,
    #[serde(default)]
    callers: Vec<Caller>,
    report: Report,
}

#[derive(Deserialize)]
struct Target {
    file: String,
}

#[derive(Deserialize)]
struct Callee {
    file: Option<String>,
}

#[derive(Deserialize)]
struct Caller {
    call_site: CallSite,
}

#[derive(Deserialize)]
struct CallSite {
    file: String,
}

#[derive(Deserialize)]
struct Report {
    token_count: u64,
    over_budget: bool,
}

/// Derive scope for `symbols` by running `maple bundle` per symbol against
/// `repo`. Returns Ok(None) when maple is not installed — callers degrade
/// gracefully to explicit paths. Errors when maple IS present but a symbol is
/// unknown or the combined context exceeds `budget` (those are real answers,
/// not infra gaps) — the caller should split the task or raise `maple.budget`
/// in hector.yaml.
pub fn scope_from_symbols(
    repo: &Path,
    symbols: &[String],
    budget: u64,
) -> anyhow::Result<Option<MapleScope>> {
    let mut bundles = Vec::new();
    for sym in symbols {
        let out = match std::process::Command::new("maple")
            .arg("bundle")
            .arg(repo)
            .arg("--symbol")
            .arg(sym)
            .arg("--budget")
            .arg(budget.to_string())
            .output()
        {
            Ok(out) => out,
            // Not installed → graceful fallback, not a hard failure.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => anyhow::bail!("could not run maple: {e}"),
        };
        if !out.status.success() {
            anyhow::bail!(
                "maple bundle --symbol '{sym}' failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let bundle: Bundle = serde_json::from_slice(&out.stdout).map_err(|e| {
            anyhow::anyhow!("maple bundle --symbol '{sym}' returned unparseable JSON: {e}")
        })?;
        bundles.push((sym.clone(), bundle));
    }
    build_scope(bundles, budget).map(Some)
}

/// Warning text shown when --symbol/symbols were requested but maple isn't
/// installed. Shared by the CLI (stderr) and MCP (response warnings) paths.
pub const FALLBACK_WARNING: &str = "maple not found on PATH — symbol scoping and context-budget \
     check skipped; falling back to explicitly provided paths";

/// Pure half of scope derivation: merge per-symbol bundles into one scope and
/// enforce the combined budget. Separated from the shell-out for testing.
fn build_scope(bundles: Vec<(String, Bundle)>, budget: u64) -> anyhow::Result<MapleScope> {
    let mut editable: BTreeSet<String> = BTreeSet::new();
    let mut references: BTreeSet<String> = BTreeSet::new();
    let mut total_tokens = 0u64;

    for (sym, b) in bundles {
        if b.report.over_budget {
            anyhow::bail!(
                "symbol '{sym}' needs {} tokens of context (budget {budget}) — \
                 split the task into smaller slices or raise maple.budget in hector.yaml",
                b.report.token_count
            );
        }
        total_tokens += b.report.token_count;
        editable.insert(b.target.file);
        for c in b.callers {
            references.insert(c.call_site.file);
        }
        for c in b.callees.into_iter().filter_map(|c| c.file) {
            references.insert(c);
        }
    }

    if total_tokens > budget {
        anyhow::bail!(
            "combined context for the requested symbols is {total_tokens} tokens \
             (budget {budget}) — split the task into smaller slices or raise \
             maple.budget in hector.yaml"
        );
    }

    let editable_paths: Vec<String> = editable.into_iter().collect();
    let reference_paths = references
        .into_iter()
        .filter(|f| !editable_paths.contains(f))
        .collect();
    Ok(MapleScope {
        editable_paths,
        reference_paths,
        total_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(json: &str) -> Bundle {
        serde_json::from_str(json).unwrap()
    }

    const FIXTURE: &str = r#"{
        "target": {"file": "src/planner.rs"},
        "callees": [{"file": "src/schema.rs"}, {"file": null}],
        "callers": [
            {"call_site": {"file": "src/mcp.rs"}},
            {"call_site": {"file": "src/main.rs"}},
            {"call_site": {"file": "src/planner.rs"}}
        ],
        "report": {"token_count": 1200, "over_budget": false}
    }"#;

    #[test]
    fn derives_editable_and_reference_paths() {
        let scope = build_scope(vec![("check_text".into(), bundle(FIXTURE))], 12000).unwrap();
        assert_eq!(scope.editable_paths, vec!["src/planner.rs"]);
        // Editable file excluded from references; null callee file skipped.
        assert_eq!(
            scope.reference_paths,
            vec!["src/main.rs", "src/mcp.rs", "src/schema.rs"]
        );
        assert_eq!(scope.total_tokens, 1200);
    }

    #[test]
    fn per_symbol_over_budget_is_an_error() {
        let over = FIXTURE.replace("\"over_budget\": false", "\"over_budget\": true");
        let err = build_scope(vec![("big".into(), bundle(&over))], 12000).unwrap_err();
        assert!(err.to_string().contains("split the task"), "{err}");
    }

    #[test]
    fn combined_over_budget_is_an_error() {
        let bundles = vec![
            ("a".into(), bundle(FIXTURE)),
            ("b".into(), bundle(FIXTURE)),
        ];
        // Each fits (1200 <= 2000) but together they don't (2400 > 2000).
        let err = build_scope(bundles, 2000).unwrap_err();
        assert!(err.to_string().contains("combined context"), "{err}");
    }

    #[test]
    fn multiple_symbols_union_dedup() {
        let second = FIXTURE
            .replace("src/planner.rs", "src/dispatch.rs")
            .replace("src/schema.rs", "src/config.rs");
        let scope = build_scope(
            vec![
                ("a".into(), bundle(FIXTURE)),
                ("b".into(), bundle(&second)),
            ],
            12000,
        )
        .unwrap();
        assert_eq!(scope.editable_paths, vec!["src/dispatch.rs", "src/planner.rs"]);
        // src/mcp.rs and src/main.rs appear in both bundles — deduped.
        assert_eq!(
            scope.reference_paths,
            vec!["src/config.rs", "src/main.rs", "src/mcp.rs", "src/schema.rs"]
        );
    }
}
