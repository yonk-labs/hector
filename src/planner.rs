//! Planning, static validation, and result review for Hector campaigns.
//!
//! Hector deliberately keeps these operations pure and file-format oriented:
//! the CLI and MCP server both call the same functions, so a campaign accepted
//! by `hector check` has the same guardrails no matter which host agent asked.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path};

use crate::conventions::Conventions;
use crate::model::{self, ModelCfg};
use crate::schema::{Campaign, Slice};

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
    /// Standing project constraints appended to every generated spec
    /// (from hector.yaml `invariants:`). Empty = no block emitted.
    pub invariants: Vec<String>,
}

/// Append the standing invariants block to a spec. None spec + invariants
/// still yields a spec — the constraints must reach the builder either way.
fn apply_invariants(spec: Option<String>, invariants: &[String]) -> Option<String> {
    let rules: Vec<&String> = invariants.iter().filter(|s| !s.trim().is_empty()).collect();
    if rules.is_empty() {
        return spec;
    }
    let mut block = String::from("## Standing invariants (project-wide, non-negotiable)\n");
    for r in rules {
        block.push_str(&format!("- {}\n", r.trim()));
    }
    Some(match spec {
        Some(s) => format!("{}\n\n{}", s.trim_end(), block),
        None => block,
    })
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
        verify_cmds: None,
        slices: vec![Slice {
            name: Some(name),
            task: Some(opts.task),
            spec: apply_invariants(opts.spec, &opts.invariants),
            verify_cmds: Some(opts.verify_cmds),
            editable_paths: opts.editable_paths,
            reference_paths: opts.reference_paths,
            judge_policy: Some(opts.judge_policy),
            max_iters: Some(opts.max_iters),
            max_changed_files: Some(opts.max_changed_files),
            max_changed_lines: Some(opts.max_changed_lines),
            tier: None,
            model: None,
            fallback_models: Vec::new(),
            depends_on: Vec::new(),
        }],
    };
    let yaml = serde_yaml::to_string(&campaign)?;
    check_text(&yaml)?;
    Ok(yaml)
}

/// Cap on carried-forward lessons text per file (chars). Lessons files append
/// over time, so the TAIL (newest entries) is what survives the cap.
const LESSONS_CAP: usize = 4_000;

/// HECTOR_SPEC's "carry forward project lessons": append `.hector/lessons.md`
/// and `.bob/lessons.md` (when present under `root`) to the spec so hard-won
/// project knowledge reaches every builder. Missing/empty files are skipped;
/// no lessons → spec unchanged.
pub fn apply_lessons(spec: Option<String>, root: &Path) -> Option<String> {
    let mut lessons = String::new();
    for rel in [".hector/lessons.md", ".bob/lessons.md"] {
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let truncated = text.chars().count() > LESSONS_CAP;
        let tail = tail_str(text, LESSONS_CAP);
        let marker = if truncated { " (most recent tail)" } else { "" };
        lessons.push_str(&format!("### From {rel}{marker}\n{tail}\n\n"));
    }
    if lessons.is_empty() {
        return spec;
    }
    let block = format!(
        "## Project lessons (carried forward)\n\n{}",
        lessons.trim_end()
    );
    Some(match spec {
        Some(s) => format!("{}\n\n{}", s.trim_end(), block),
        None => block,
    })
}

#[cfg(test)]
mod lessons_tests {
    use super::apply_lessons;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hector-lessons-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".hector")).unwrap();
        dir
    }

    #[test]
    fn appends_lessons_and_keeps_tail_when_capped() {
        let dir = tmp("basic");
        std::fs::write(dir.join(".hector/lessons.md"), "always run tests --runInBand").unwrap();
        let out = apply_lessons(Some("spec".into()), &dir).unwrap();
        assert!(out.starts_with("spec"));
        assert!(out.contains("## Project lessons"));
        assert!(out.contains("--runInBand"));

        // Oversized file: only the newest tail survives, flagged as such.
        let big = format!("{}NEWEST", "x".repeat(10_000));
        std::fs::write(dir.join(".hector/lessons.md"), big).unwrap();
        let out = apply_lessons(None, &dir).unwrap();
        assert!(out.contains("NEWEST"));
        assert!(out.contains("most recent tail"));
        assert!(out.chars().count() < 5_000);
    }

    #[test]
    fn no_lessons_files_leaves_spec_untouched() {
        let dir = tmp("none");
        assert_eq!(apply_lessons(Some("spec".into()), &dir).as_deref(), Some("spec"));
        assert_eq!(apply_lessons(None, &dir), None);
        // Empty file also skipped.
        std::fs::write(dir.join(".hector/lessons.md"), "  \n").unwrap();
        assert_eq!(apply_lessons(None, &dir), None);
    }
}

#[cfg(test)]
mod reference_block_tests {
    use super::reference_files_block;

    #[test]
    fn includes_bodies_skips_missing_and_caps() {
        let dir = std::env::temp_dir().join(format!("hector-refs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("small.ts"), "export const FIXTURE = 1;").unwrap();
        std::fs::write(dir.join("big.ts"), "x".repeat(10_000)).unwrap();

        let block = reference_files_block(
            &["small.ts".into(), "missing.ts".into(), "big.ts".into()],
            &dir,
        );
        assert!(block.contains("### small.ts"));
        assert!(block.contains("export const FIXTURE = 1;"));
        assert!(!block.contains("missing.ts"), "unreadable paths are skipped");
        assert!(block.contains("(truncated)"), "oversized file is truncated");
        assert!(block.chars().count() < 20_000);
    }

    #[test]
    fn empty_paths_yield_empty_block() {
        assert_eq!(reference_files_block(&[], &std::env::temp_dir()), "");
    }
}

#[cfg(test)]
mod invariant_tests {
    use super::apply_invariants;

    #[test]
    fn appends_block_to_existing_spec() {
        let out = apply_invariants(
            Some("Do the thing.".into()),
            &["no new deps".into(), "never weaken an assertion".into()],
        )
        .unwrap();
        assert!(out.starts_with("Do the thing."));
        assert!(out.contains("## Standing invariants"));
        assert!(out.contains("- no new deps\n"));
        assert!(out.contains("- never weaken an assertion\n"));
    }

    #[test]
    fn creates_spec_when_none_but_invariants_exist() {
        let out = apply_invariants(None, &["no new deps".into()]).unwrap();
        assert!(out.starts_with("## Standing invariants"));
    }

    #[test]
    fn no_invariants_leaves_spec_untouched() {
        assert_eq!(
            apply_invariants(Some("spec".into()), &[]).as_deref(),
            Some("spec")
        );
        assert_eq!(apply_invariants(None, &[]), None);
        // Whitespace-only entries don't produce an empty block.
        assert_eq!(apply_invariants(None, &["  ".into()]), None);
    }
}

/// The model's response when asked to write a test and plan a slice.
#[derive(Debug, Deserialize)]
struct ModelPlanResponse {
    test_code: String,
    test_path: String,
    verify_cmd: String,
    editable_paths: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    reasoning: String,
}

/// Take the last `max` chars of a string (for error output truncation).
/// Char-based, not byte-based, so multibyte UTF-8 output can't panic on a
/// non-boundary slice.
fn tail_str(s: &str, max: usize) -> String {
    let mut chars: Vec<char> = s.chars().rev().take(max).collect();
    chars.reverse();
    chars.into_iter().collect()
}

/// Pull the identifier a "missing symbol" error names, e.g. "warWearinessPenalty"
/// out of `TypeError: warWearinessPenalty is not a function` or the Babel/CJS
/// interop form `(0 , _core_moraleUtils.warWearinessPenalty) is not a function`.
/// Also extracts the module path from `Cannot find module 'X'` (#24: brand-new
/// modules under test). Returns None when the text before the marker isn't a
/// plain identifier (e.g. `Class extends value #<Object> is not a constructor`
/// — nothing nameable there).
fn extract_missing_symbol(output: &str) -> Option<String> {
    // "Cannot find module 'packages/worldgen/src/bandits'" — the quoted path
    // is what's missing. Returned as-is (caller checks against editable_paths).
    const MODULE_MARKERS: &[&str] = &["Cannot find module", "Cannot find module"];
    for marker in MODULE_MARKERS {
        if let Some(idx) = output.find(marker) {
            let after = output[idx + marker.len()..].trim_start();
            // Strip surrounding quotes: '...' or "..."
            let stripped = after.trim_start_matches(['\'', '"']);
            let path: String = stripped
                .chars()
                .take_while(|c| *c != '\'' && *c != '"')
                .collect();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }
    const SUFFIX_MARKERS: &[&str] = &[" is not a function", " is not a constructor", " is not defined"];
    for marker in SUFFIX_MARKERS {
        if let Some(idx) = output.find(marker) {
            // CJS interop wraps the reference in a trailing paren, e.g.
            // "(0 , _mod.fn) is not a function" — peel it before scanning back.
            let before = output[..idx].trim_end_matches(')');
            let token: String = before
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$' || *c == '.')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            // Dotted access (module.symbol) — the symbol under test is the
            // last segment, not the module namespace it hangs off of.
            if let Some(ident) = token.rsplit('.').next().filter(|s| !s.is_empty()) {
                return Some(ident.to_string());
            }
        }
    }
    const PREFIX_MARKER: &str = "has no exported member";
    if let Some(idx) = output.find(PREFIX_MARKER) {
        let after = output[idx + PREFIX_MARKER.len()..]
            .trim_start()
            .trim_start_matches(['\'', '"']);
        let ident: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if !ident.is_empty() {
            return Some(ident);
        }
    }
    None
}

/// Distinguish "test file itself is broken" (import/syntax/module errors) from
/// "feature isn't implemented yet" (assertion failures). Hector retries on
/// infrastructure errors but accepts assertion failures as the correct RED state.
///
/// `base_prompt` is the TASK+SPEC text the test-writer was given. A "missing
/// symbol" error (is not a function / is not defined / has no exported member)
/// that names the symbol under test is TDD working as designed — the function
/// doesn't exist yet, that's the correct red state, not a broken test. The same
/// error naming some OTHER symbol (a broken import, a typo'd helper) is still
/// infrastructure: the test itself can't load.
///
/// `editable_paths` are the production files bob will create. A "Cannot find
/// module" error naming one of those is a brand-new module under test (#24) —
/// the module doesn't exist yet by design, not a broken import.
fn is_infrastructure_error(output: &str, base_prompt: &str, editable_paths: &[String]) -> bool {
    let lower = output.to_lowercase();
    const PATTERNS: &[&str] = &[
        "cannot find module",
        "is not a constructor",
        "is not a function",
        "is not defined",
        "syntaxerror",
        "unexpected token",
        "cannot use import statement",
        "err_require_module",
        "err_module_not_found",
        "referenceerror",
        "module not found",
        "no such file or directory",
        "class extends value",
        "has no exported member",
    ];
    // But NOT when it's clearly an assertion failure that happens to contain a keyword
    if lower.contains("expected:") && lower.contains("received:")
        || lower.contains("assertionerror")
        || lower.contains("number of assertions")
    {
        return false;
    }
    if !PATTERNS.iter().any(|p| lower.contains(p)) {
        return false;
    }
    if let Some(symbol) = extract_missing_symbol(output) {
        // #24: "Cannot find module 'packages/worldgen/src/bandits'" — the
        // extracted value is a path. If it matches an editable_path (the file
        // bob will create), this is a brand-new module under test, not a typo.
        if lower.contains("cannot find module") || lower.contains("module not found") {
            // #24: only editable_paths can prove this is a brand-new module
            // under test. Prompt matching is too loose — reference paths
            // (read-only files) would false-positive.
            if is_editable_module(&symbol, editable_paths) {
                return false;
            }
        } else {
            // Symbol-name patterns (is not a function / has no exported member):
            // check against the prompt as before.
            if base_prompt.contains(&symbol) {
                return false;
            }
        }
    }
    true
}

/// Check whether a module path from a "Cannot find module" error matches one
/// of the editable_paths — the files bob will create. Handles relative paths
/// (../packages/worldgen/src/bandits → packages/worldgen/src/bandits) and
/// missing extensions (bandits → bandits.ts).
fn is_editable_module(module_path: &str, editable_paths: &[String]) -> bool {
    // Normalize: strip leading ./ and ../ segments, take the tail path.
    let normalized: String = module_path
        .split('/')
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .collect::<Vec<_>>()
        .join("/");
    let normalized_basename = Path::new(&normalized)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&normalized);
    for editable in editable_paths {
        let edit_normalized: String = editable
            .split('/')
            .filter(|s| !s.is_empty() && *s != "." && *s != "..")
            .collect::<Vec<_>>()
            .join("/");
        // Direct match (with or without extension).
        if edit_normalized == normalized
            || edit_normalized == format!("{normalized}.ts")
            || edit_normalized == format!("{normalized}.js")
            || edit_normalized == format!("{normalized}.tsx")
            || edit_normalized == format!("{normalized}.jsx")
        {
            return true;
        }
        // Basename match (the test imported a different relative path but the
        // target file is the same editable_path).
        let edit_basename = Path::new(&edit_normalized)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&edit_normalized);
        if edit_basename == normalized_basename && !normalized_basename.is_empty() {
            return true;
        }
    }
    false
}

/// #12 backstop: after the classifier says "not infra" (legitimate red), check
/// that the output actually looks like a test failure — not pure runtime error
/// noise that slipped through a classifier gap. A real TDD red has assertion
/// vocabulary or a missing-symbol-under-test marker. Pure error output with
/// none of these is suspicious — better to retry than accept a broken test.
/// ponytail: keyword scan, not a parser; add patterns as false-negatives surface.
fn looks_like_test_failure(output: &str) -> bool {
    let lower = output.to_lowercase();
    const MARKERS: &[&str] = &[
        "expected:",       // vitest/jest assertion output
        "received:",       // vitest/jest assertion output
        "assertionerror",  // JS AssertionError class
        "tests:",          // test runner summary ("Tests: 1 failed, 3 passed")
        "test suites:",    // jest summary
        "fail",            // "FAIL" / "failed" / "failing"
        "✓",               // vitest pass marker (at least some tests ran)
        "✗",               // vitest fail marker
        "passed",          // "3 passed"
        "expect(",         // vitest/jest expect() call in output
        "tobe(",           // common matcher in output
        "toequal(",        // common matcher in output
        "is not a function", // missing-symbol-under-test (TDD red for new function)
        "is not defined",    // missing-symbol-under-test
        "is not a constructor", // missing-symbol-under-test
        "has no exported member", // TS missing export (TDD red for new export)
        "cannot find module", // new-module-under-test (TDD red for new module)
        "error:",           // generic error that still indicates a test ran
    ];
    MARKERS.iter().any(|m| lower.contains(m))
}

#[cfg(test)]
mod infra_tests {
    use super::is_infrastructure_error;

    #[test]
    fn detects_import_error() {
        let out = "TypeError: Class extends value #<Object> is not a constructor or null\n\
                   at Object.<anonymous> (tests/player.test.js:1:18)";
        assert!(is_infrastructure_error(out, "", &[]));
    }

    #[test]
    fn detects_module_not_found() {
        let out = "Error: Cannot find module '../src/body'\nRequire stack:";
        assert!(is_infrastructure_error(out, "", &[]));
    }

    #[test]
    fn detects_syntax_error() {
        let out = "SyntaxError: Unexpected token }";
        assert!(is_infrastructure_error(out, "", &[]));
    }

    #[test]
    fn does_not_flag_assertion_failure() {
        let out = "expect(received).toBe(expected)\nExpected: 28\nReceived: 27";
        assert!(!is_infrastructure_error(out, "", &[]));
    }

    #[test]
    fn does_not_flag_test_failure_summary() {
        let out = "FAIL tests/body.test.js\nTests: 1 failed, 7 passed";
        assert!(!is_infrastructure_error(out, "", &[]));
    }

    // --- new-symbol-under-test vs. broken-import disambiguation ---

    #[test]
    fn new_function_under_test_is_legitimate_red_not_infra() {
        // TDD red state: warWearinessPenalty is the function the spec asked
        // for and doesn't exist yet — the frozen test is correct, not broken.
        let base_prompt = "## TASK\nAdd a pure function warWearinessPenalty(turns) \
                            to core/moraleUtils.ts.\n\n\
                            ## SPEC\nwarWearinessPenalty(0) must return 0.";
        let out = "TypeError: warWearinessPenalty is not a function\n\
                   at Object.<anonymous> (core/moraleUtils.test.ts:5:18)";
        assert!(!is_infrastructure_error(out, base_prompt, &[]));
    }

    #[test]
    fn new_function_under_test_babel_cjs_interop_form_is_not_infra() {
        // Jest's CJS interop wraps the reference: "(0 , _mod.fn) is not a function".
        let base_prompt = "## TASK\nAdd warWearinessPenalty to core/moraleUtils.\n\n\
                            ## SPEC\nwarWearinessPenalty(3) must return 6.";
        let out = "TypeError: (0 , _core_moraleUtils.warWearinessPenalty) is not a function";
        assert!(!is_infrastructure_error(out, base_prompt, &[]));
    }

    // #19: a truncated / reasoning-only model response must classify as EOF so
    // the plan loop rotates models instead of wasting a re-ask. A complete but
    // malformed object stays non-EOF and keeps the one re-ask.
    #[test]
    fn truncated_or_reasoning_only_response_is_eof_class() {
        use serde_json::error::Category;
        let parse = |s: &str| {
            serde_json::from_str::<super::ModelPlanResponse>(s)
                .unwrap_err()
                .classify()
        };
        // Unclosed <think>: nothing survives stripping → empty → EOF.
        let raw = "<think>Let me plan. The path from core/incorporation";
        assert_eq!(parse(&crate::model::extract_json(raw)), Category::Eof);
        // Truncated mid-object (token ceiling) → unterminated → EOF.
        assert_eq!(parse(r#"{"test_code": "half a fi"#), Category::Eof);
        // Complete object, wrong shape → Data, NOT EOF → re-ask path survives.
        assert_ne!(parse(r#"{"unexpected": true}"#), Category::Eof);
    }

    #[test]
    fn unrelated_broken_import_is_not_a_function_is_still_infra() {
        // The spec is about warWearinessPenalty; someOtherHelper never appears
        // in it — this is a broken/typo'd import in the test file, not TDD red.
        let base_prompt = "## TASK\nAdd a pure function warWearinessPenalty(turns).\n\n\
                            ## SPEC\nwarWearinessPenalty(0) must return 0.";
        let out = "TypeError: someOtherHelper is not a function\n\
                   at Object.<anonymous> (core/moraleUtils.test.ts:5:18)";
        assert!(is_infrastructure_error(out, base_prompt, &[]));
    }

    #[test]
    fn broken_import_still_infra_even_when_prompt_mentions_the_module() {
        // "Cannot find module" isn't a missing-symbol pattern — the exemption
        // never applies, even if the module name happens to appear in the
        // prompt (e.g. it's a legitimate reference path the test typo'd).
        let base_prompt = "## REFERENCE FILES\ncore/body.ts";
        let out = "Error: Cannot find module '../core/body'\nRequire stack:";
        assert!(is_infrastructure_error(out, base_prompt, &[]));
    }

    #[test]
    fn new_symbol_is_not_defined_is_not_infra() {
        let base_prompt = "## SPEC\nCall warWearinessPenalty(turns) directly (no import).";
        let out = "ReferenceError: warWearinessPenalty is not defined";
        assert!(!is_infrastructure_error(out, base_prompt, &[]));
    }

    #[test]
    fn new_symbol_has_no_exported_member_is_not_infra() {
        let base_prompt = "## SPEC\nExport warWearinessPenalty from core/moraleUtils.";
        let out = "TS2305: Module '\"../core/moraleUtils\"' has no exported member 'warWearinessPenalty'.";
        assert!(!is_infrastructure_error(out, base_prompt, &[]));
    }

    #[test]
    fn unrelated_has_no_exported_member_is_still_infra() {
        let base_prompt = "## SPEC\nwarWearinessPenalty(0) must return 0.";
        let out = "TS2305: Module '\"../core/moraleUtils\"' has no exported member 'someOtherHelper'.";
        assert!(is_infrastructure_error(out, base_prompt, &[]));
    }

    // --- #24: brand-new module under test (Cannot find module) ---

    #[test]
    fn new_module_under_test_is_legitimate_red_when_in_editable_paths() {
        // The test imports packages/worldgen/src/bandits which doesn't exist
        // yet — bob will create it. This is TDD red, not a broken import.
        let out = "Error: Cannot find module '../packages/worldgen/src/bandits'\nRequire stack:";
        let editable = vec!["packages/worldgen/src/bandits.ts".to_string()];
        assert!(!is_infrastructure_error(out, "", &editable));
    }

    #[test]
    fn new_module_under_test_basename_match_in_editable_paths() {
        // Test imports via a different relative path but the target file
        // basename matches an editable_path.
        let out = "Error: Cannot find module '../../bandits'\nRequire stack:";
        let editable = vec!["packages/worldgen/src/bandits.ts".to_string()];
        assert!(!is_infrastructure_error(out, "", &editable));
    }

    #[test]
    fn new_module_under_test_prompt_mentions_basename() {
        // Module path matches an editable_path even though the import path in
        // the error differs in relative depth — it's being created, not a typo.
        let base_prompt = "## TASK\nCreate packages/worldgen/src/bandits.ts implementing the bandit AI.";
        let out = "Error: Cannot find module '../packages/worldgen/src/bandits'\nRequire stack:";
        let editable = vec!["packages/worldgen/src/bandits.ts".to_string()];
        assert!(!is_infrastructure_error(out, base_prompt, &editable));
    }

    #[test]
    fn broken_import_unrelated_to_editable_paths_still_infra() {
        // The missing module has nothing to do with the editable_paths —
        // it's a real typo or wrong path in the test.
        let out = "Error: Cannot find module './testHelpers'\nRequire stack:";
        let editable = vec!["packages/worldgen/src/bandits.ts".to_string()];
        assert!(is_infrastructure_error(out, "", &editable));
    }

    #[test]
    fn broken_import_still_infra_with_empty_editable_paths() {
        // No editable_paths provided — Cannot find module stays infra
        // (can't prove it's a new module under test).
        let base_prompt = "## REFERENCE FILES\ncore/body.ts";
        let out = "Error: Cannot find module '../core/body'\nRequire stack:";
        assert!(is_infrastructure_error(out, base_prompt, &[]));
    }

    // --- #12: tightened escape hatch (no more bare "assertion" false-negative) ---

    #[test]
    fn infra_error_with_word_assertion_in_stack_trace_still_infra() {
        // The bare word "assertion" in a stack trace or plugin name used to
        // bypass classification entirely. Tightened: need "expected:/received:"
        // pair or "assertionerror" — not just "assertion".
        let out = "Cannot find module './testHelpers'\n    at runAssertion (node:internal)";
        assert!(is_infrastructure_error(out, "", &[]));
    }

    #[test]
    fn real_assertion_failure_still_not_infra() {
        // Genuine vitest assertion output still correctly bypasses infra.
        let out = "AssertionError: expected 28 to be 27\nExpected: 28\nReceived: 27";
        assert!(!is_infrastructure_error(out, "", &[]));
    }

    // --- #12 backstop: looks_like_test_failure ---

    #[test]
    fn looks_like_test_failure_recognizes_assertion_output() {
        assert!(super::looks_like_test_failure(
            "Expected: 28\nReceived: 27"
        ));
        assert!(super::looks_like_test_failure(
            "Tests: 1 failed, 3 passed"
        ));
        assert!(super::looks_like_test_failure(
            "FAIL core/bandits.test.ts"
        ));
    }

    #[test]
    fn looks_like_test_failure_rejects_pure_error_noise() {
        // No test-failure vocabulary at all — likely a runtime crash the
        // classifier missed. Should NOT pass the backstop.
        assert!(!super::looks_like_test_failure(
            "thread 'main' panicked at 'index out of bounds'"
        ));
    }

    #[test]
    fn tail_str_is_utf8_safe_and_bounded() {
        use super::tail_str;
        // Multibyte input longer than max: byte-slicing would panic mid-char.
        let s = "é".repeat(500);
        let out = tail_str(&s, 100);
        assert_eq!(out.chars().count(), 100);
        assert!(out.chars().all(|c| c == 'é'));
        // Shorter than max: returned whole.
        assert_eq!(tail_str("hi", 100), "hi");
    }
}

const PLANNING_SYSTEM: &str = "\
You are a TDD test-writer for a coding agent. The frontier model (or human) has already written a detailed behavior spec. Your job is to translate that spec into ONE focused failing test.\n\
\n\
Rules:\n\
- Write ONLY the test. The spec is provided — do not change it.\n\
- The test MUST fail before implementation (red state).\n\
- Follow the repo's existing test conventions exactly.\n\
- Match the API signature in the spec — constructor args, method names, param order, return types. The spec is the contract; your test exercises it.\n\
- Write the MINIMAL test that proves the behavior — not a full suite. One test is usually enough.\n\
- editable_paths are production files the implementation will change — never include test files there.\n\
- Output ONLY valid JSON. No markdown fences. No prose before or after.\n\
\n\
Output schema:\n\
{\"test_code\": \"full file contents\", \"test_path\": \"relative/path\", \"verify_cmd\": \"command\", \"editable_paths\": [\"paths\"], \"reasoning\": \"one sentence\"}";

/// One model's attempt(s) at writing the focused test, red probe included.
/// On failure, every test file this model wrote is removed — a leftover
/// broken test from a failed planner run would poison the repo's suite.
async fn write_test_with(
    cfg: &ModelCfg,
    base_prompt: &str,
    repo_root: &Path,
    editable_paths: &[String],
) -> anyhow::Result<ModelPlanResponse> {
    let mut written: Vec<std::path::PathBuf> = Vec::new();
    let result = write_test_attempts(cfg, base_prompt, repo_root, &mut written, editable_paths).await;
    if result.is_err() {
        for p in &written {
            let _ = std::fs::remove_file(p);
        }
    }
    result
}

async fn write_test_attempts(
    cfg: &ModelCfg,
    base_prompt: &str,
    repo_root: &Path,
    written: &mut Vec<std::path::PathBuf>,
    editable_paths: &[String],
) -> anyhow::Result<ModelPlanResponse> {
    // Hard caps to prevent infinite loop of doom:
    //   max_retries  = infra-error retries (model wrote a test that can't load)
    //   parse retry  = ONE re-ask on invalid JSON (with the parse error shown)
    //   loop_budget  = absolute ceiling on total model calls
    let max_retries = 2;
    let loop_budget = 4;
    let mut infra_attempt = 0;
    let mut parse_retried = false;
    let mut total_calls = 0;
    let mut current_prompt = base_prompt.to_string();

    loop {
        total_calls += 1;
        if total_calls > loop_budget {
            anyhow::bail!("attempt budget exhausted ({loop_budget} model calls)");
        }
        let raw = model::complete(cfg, PLANNING_SYSTEM, &current_prompt).await?;

        // Degenerate output (repetition death spiral) is a model failure, not
        // something a re-ask fixes — rotate immediately.
        if model::is_looping_output(&raw) {
            anyhow::bail!("output is looping (same segment repeated) — likely truncated at the token ceiling");
        }

        let json_str = model::extract_json(&raw);
        let resp: ModelPlanResponse = match serde_json::from_str(&json_str) {
            Ok(r) => r,
            // EOF-class = the JSON ended before it finished: either nothing
            // survived <think> stripping (reasoning-only answer) or the model
            // truncated mid-object at the token ceiling. Re-asking a model that
            // does this just burns another call on the same failure, so bail and
            // let the infra-retry loop rotate models — same stance as the
            // looping-output guard above. (#19: this used to masquerade as a
            // generic "not valid JSON, EOF" and waste the one re-ask.)
            Err(e) if e.classify() == serde_json::error::Category::Eof => {
                anyhow::bail!(
                    "model returned no complete JSON object — only reasoning/prose, \
                     or a response truncated at the token ceiling before the answer. \
                     raw tail: {}",
                    tail_str(&raw, 400)
                );
            }
            Err(e) => {
                if !parse_retried {
                    parse_retried = true;
                    eprintln!("hector: response was not valid JSON ({e}), re-asking once...");
                    current_prompt = format!(
                        "{base_prompt}\n\n\
                         ## YOUR PREVIOUS RESPONSE WAS NOT VALID JSON\n\
                         Parse error: {e}\n\
                         Output ONLY the JSON object — no prose, no fences, no reasoning."
                    );
                    continue;
                }
                anyhow::bail!(
                    "response is not valid JSON after a retry: {e}\nraw tail: {}",
                    tail_str(&raw, 400)
                );
            }
        };

        eprintln!("hector: model wrote test → {}", resp.test_path);

        // Write the test file (hector owns test files; bob owns production code)
        let test_path_full = repo_root.join(&resp.test_path);
        if let Some(parent) = test_path_full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&test_path_full, &resp.test_code)?;
        if !written.contains(&test_path_full) {
            written.push(test_path_full.clone());
        }

        // Red probe — run the verify command, confirm the test FAILS
        eprintln!("hector: red probe — running '{}'", resp.verify_cmd);
        let red_output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&resp.verify_cmd)
            .current_dir(repo_root)
            .output()
            .await;

        let (is_red, output_text) = match red_output {
            Ok(out) => {
                let combined = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                (!out.status.success(), combined)
            }
            Err(e) => {
                eprintln!("hector: warning — could not run verify command: {e}");
                (false, e.to_string())
            }
        };

        if !is_red {
            eprintln!(
                "hector: warning — test passed immediately (green). Either the feature exists or the test is wrong."
            );
            return Ok(resp);
        }

        // Test failed (red). But WHY? Infrastructure error vs assertion failure.
        if is_infrastructure_error(&output_text, base_prompt, editable_paths) {
            if infra_attempt < max_retries {
                infra_attempt += 1;
                eprintln!(
                    "hector: test has infrastructure error, retrying ({infra_attempt}/{max_retries})..."
                );
                current_prompt = format!(
                    "{initial}\n\n\
                     ## PREVIOUS TEST HAD AN ERROR — FIX THIS\n\
                     The test you wrote failed to load due to:\n{err}\n\n\
                     Common fixes:\n\
                     - Check require/import statements match module exports\n\
                     - For CommonJS: use `const {{ X }} = require(...)` not `const X = require(...)`\n\
                     - Verify file paths are correct (../src/ vs ./)\n",
                    initial = base_prompt,
                    err = tail_str(&output_text, 800)
                );
                continue;
            }
            // Never "accept anyway": a test that can't load is a guaranteed
            // wasted bob run — the frozen verify gate could never pass.
            anyhow::bail!(
                "test still has infrastructure errors after {max_retries} retries: {}",
                tail_str(&output_text, 300)
            );
        }
        // #12 backstop: the classifier said "not infra" (legitimate red), but
        // verify the output actually looks like a test failure. A real TDD red
        // has assertion vocabulary (expected/received/fail/tests:) or a
        // missing-symbol-under-test marker. If the output is pure error noise
        // with NO test-failure vocabulary at all, the classifier was wrong —
        // treat as infra and retry/bail instead of accepting a broken test.
        if !looks_like_test_failure(&output_text) && infra_attempt < max_retries {
            infra_attempt += 1;
            eprintln!(
                "hector: warning — test failed but output has no test-failure vocabulary \
                 (possible classifier miss), retrying ({infra_attempt}/{max_retries})..."
            );
            current_prompt = format!(
                "{initial}\n\n\
                 ## PREVIOUS TEST FAILED IN AN UNEXPECTED WAY\n\
                 The test you wrote failed, but not with a normal assertion failure. Output:\n{err}\n\n\
                 Common fixes:\n\
                 - Check that imports resolve and the test file can load\n\
                 - Check that the test actually runs assertions (not just imports)\n",
                initial = base_prompt,
                err = tail_str(&output_text, 800)
            );
            continue;
        }
        eprintln!("hector: test fails as expected (red) ✓");
        return Ok(resp);
    }
}

/// Per-reference-file and total caps (chars) for reference content fed to the
/// test-writer. The LAN models degrade past ~16k input tokens; the spec +
/// conventions already take a share of that.
const REF_FILE_CAP: usize = 4_000;
const REF_TOTAL_CAP: usize = 12_000;

/// Read reference file bodies for the test-writer prompt. "Copy the fixture
/// style of core/economy.test.ts" is physically impossible when the model only
/// sees the path — that miss made qwen invent a `./testHelpers` module in the
/// expansion-empire pilot. Unreadable paths are skipped (they may be globs or
/// not-yet-written files); oversized content is truncated, never dropped.
fn reference_files_block(paths: &[String], root: &Path) -> String {
    let mut out = String::new();
    for p in paths {
        if out.chars().count() >= REF_TOTAL_CAP {
            out.push_str("(further reference files omitted — context budget)\n");
            break;
        }
        let Ok(text) = std::fs::read_to_string(root.join(p)) else {
            continue;
        };
        let head: String = text.chars().take(REF_FILE_CAP).collect();
        let marker = if text.chars().count() > REF_FILE_CAP {
            "\n… (truncated)"
        } else {
            ""
        };
        out.push_str(&format!("### {p}\n```\n{head}{marker}\n```\n"));
    }
    out
}

/// LLM-backed planning: the frontier model (or human) writes the spec, hector's
/// smaller model writes a focused test against that spec. The spec is the
/// contract; the test is the proof. Hector runs the red probe, freezes both
/// into a campaign for bob to implement.
///
/// `models` is the rotation order (default first): a model whose output is
/// unusable — looping, unparseable after a retry, or a test that can't even
/// load after infra retries — is dropped and the next one tried. Only when
/// every configured model fails does planning fail (with a non-zero exit; a
/// campaign that was never written must never look like success).
pub async fn plan_with_model(
    opts: PlanOptions,
    models: &[ModelCfg],
    conventions: &Conventions,
    repo_root: &Path,
) -> anyhow::Result<String> {
    if opts.task.trim().is_empty() {
        anyhow::bail!("task is required for model-backed planning");
    }
    if models.is_empty() {
        anyhow::bail!("no planner models configured");
    }
    // Spec is REQUIRED in the LLM path. The frontier model is the spec author;
    // hector only writes tests. Without a spec, we can't generate a meaningful
    // test — refuse and ask for one rather than inventing both.
    let spec = match opts.spec.as_deref() {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => anyhow::bail!(
            "--spec is required when using the LLM planning path. \
             The frontier model (or human) writes the behavior contract; \
             hector only writes the focused test against that contract. \
             Provide a detailed spec file with API signatures, expected behavior, \
             edge cases, and acceptance criteria."
        ),
    };

    let refs = reference_files_block(&opts.reference_paths, repo_root);
    let refs_section = if refs.is_empty() {
        String::new()
    } else {
        format!(
            "## REFERENCE FILES (read-only — copy their style, imports, and helpers; do NOT invent modules that aren't imported here)\n{refs}\n"
        )
    };

    // #13: conventions are path-aware. A repo-wide scan picks the first test
    // file alphabetically (core/ before packages/), but this slice's module
    // may live in a different subtree. Adjust conventions to the editable_paths.
    let conventions = conventions.for_editable_paths(&opts.editable_paths, repo_root);

    let user_prompt = format!(
        "## TASK\n{task}\n\n\
         ## SPEC (authoritative — write a test that exercises this exact behavior)\n{spec}\n\n\
         ## REPO CONVENTIONS\n{conv}\n\n\
         {refs_section}\
         Write ONE focused test that proves the spec. Output JSON only.",
        task = opts.task,
        spec = spec,
        conv = conventions.prompt_block(),
    );

    let mut failures: Vec<String> = Vec::new();
    let mut resp: Option<ModelPlanResponse> = None;
    for cfg in models {
        eprintln!(
            "hector: asking model '{}' to write a focused test against the spec...",
            cfg.name
        );
        match write_test_with(cfg, &user_prompt, repo_root, &opts.editable_paths).await {
            Ok(r) => {
                resp = Some(r);
                break;
            }
            Err(e) => {
                eprintln!("hector: model '{}' failed to produce a usable test: {e}", cfg.name);
                failures.push(format!("{}: {e}", cfg.name));
            }
        }
    }
    let Some(resp) = resp else {
        anyhow::bail!(
            "all {} configured planner model(s) failed to produce a usable test:\n  {}",
            models.len(),
            failures.join("\n  ")
        );
    };

    // Freeze into campaign — spec from upstream is authoritative, test from hector
    let name = opts.name.unwrap_or_else(|| slug(&opts.task));
    let campaign = Campaign {
        name: Some(name.clone()),
        auto_commit: opts.auto_commit,
        verify_cmds: None,
        slices: vec![Slice {
            name: Some(name),
            task: Some(opts.task),
            spec: apply_invariants(Some(spec), &opts.invariants),
            verify_cmds: Some(vec![resp.verify_cmd.clone()]),
            editable_paths: if resp.editable_paths.is_empty() {
                opts.editable_paths.clone()
            } else {
                resp.editable_paths
            },
            reference_paths: {
                let mut refs = opts.reference_paths.clone();
                let test_ref = resp.test_path.clone();
                if !refs.contains(&test_ref) {
                    refs.push(test_ref);
                }
                refs
            },
            judge_policy: Some(opts.judge_policy),
            max_iters: Some(opts.max_iters),
            max_changed_files: Some(opts.max_changed_files),
            max_changed_lines: Some(opts.max_changed_lines),
            tier: None,
            model: None,
            fallback_models: Vec::new(),
            depends_on: Vec::new(),
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
    final_diff: Option<String>,
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
    #[serde(default)]
    final_diff: Option<String>,
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

/// Tier 2 deep review: call abe validate with a frontier reviewer (codex/glm)
/// for a final quality gate before handing results to the frontier model.
/// Runs ONCE per slice — not per iteration. The cheap per-iteration review
/// (bob's judge) already caught obvious bugs; this is the "are we really sure?"
/// check with a strong model.
pub async fn deep_review(
    campaign_path: &Path,
    bob_result_path: &Path,
    reviewer: &str,
) -> anyhow::Result<String> {
    let campaign = std::fs::read_to_string(campaign_path)?;
    let bob_result = std::fs::read_to_string(bob_result_path)?;

    // Extract spec + diff from the campaign and bob result for the reviewer
    let campaign_yaml: Campaign = serde_yaml::from_str(&campaign)?;
    let result: BobResult = serde_json::from_str(&bob_result)?;

    let spec = campaign_yaml
        .slices
        .iter()
        .filter_map(|s| s.spec.as_deref())
        .next()
        .unwrap_or("(no spec)");

    // The actual code under review is bob's final_diff (top-level or per-slice),
    // NOT the status string. Fall back to a note if bob produced no diff.
    let diff = result
        .final_diff
        .as_deref()
        .filter(|d| !d.trim().is_empty())
        .or_else(|| {
            result
                .slices
                .iter()
                .filter_map(|s| s.final_diff.as_deref())
                .find(|d| !d.trim().is_empty())
        })
        .unwrap_or("(no diff in bob result)");

    let statement = format!(
        "Deep code review. This code passed automated tests and a cheap reviewer. \
         As a frontier reviewer, check for subtle issues the cheap reviewer might miss: \
         race conditions, security holes, performance traps, semantic errors, \
         and spec violations that tests don't cover.\n\n\
         ## SPEC\n{spec}\n\n## DIFF UNDER REVIEW\n{diff}"
    );

    eprintln!("hector: deep review with '{reviewer}'...");
    // --verdict: abe returns a structured {verdict, reasons, take} instead of
    // prose, so the gate reads a field rather than keyword-grepping the review.
    let output = tokio::process::Command::new("abe")
        .args([
            "validate",
            "--reviewer",
            reviewer,
            "--json",
            "--verdict",
            "--",
            &statement,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("spawning abe for deep review: {e}"))?;

    if !output.status.success() {
        anyhow::bail!(
            "abe deep review failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .map_err(|e| anyhow::anyhow!("abe returned non-JSON: {e}"))?;

    let take = v
        .get("take")
        .and_then(|t| t.as_str())
        .unwrap_or("(no review output)");
    let reasons: Vec<String> = v
        .get("reasons")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|i| i.as_str().map(String::from)).collect())
        .unwrap_or_default();

    // Gate on abe's structured verdict. A missing/unknown verdict reads as
    // "uncertain" (never a silent pass) → route to human. Only an explicit
    // "pass" auto-accepts.
    let reviewer_verdict = v.get("verdict").and_then(|x| x.as_str()).unwrap_or("uncertain");
    let decision = match reviewer_verdict {
        "pass" => "accept",
        _ => "accept_for_human_review", // fail or uncertain → a human looks
    };

    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "deep_verdict": decision,
        "reviewer_verdict": reviewer_verdict,
        "deep_reviewer": reviewer,
        "deep_reasons": reasons,
        "deep_take": take,
    }))?)
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

/// Test-file heuristic. MUST stay in sync with bob's `engine::is_test_path` —
/// a file one tool treats as a frozen test and the other as editable production
/// code is a scope hole across the seam. (Pinned by is_test_path_matches_bob.)
fn is_test_path(path: &str) -> bool {
    let p = Path::new(path);
    p.components().any(
        |c| matches!(c, Component::Normal(s) if s == "tests" || s == "test" || s == "__tests__"),
    ) || path.ends_with("_test.rs")
        || path.ends_with("_test.js")
        || path.ends_with("_test.py")
        || path.ends_with(".test.js")
        || path.ends_with(".test.ts")
        || path.ends_with(".spec.js")
        || path.ends_with(".spec.ts")
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

#[cfg(test)]
mod review_contract_tests {
    use super::review_text;

    // CROSS-REPO CONTRACT: bob's exact JSON strings (bob/src/report.rs::to_json)
    // that review_text routes on. If bob renames a RunStatus/NextAction/StopReason
    // variant, bob's cross_repo_status_string_contract test fails first; this side
    // pins that review_text still maps those strings to the right decision.
    const CAMPAIGN: &str = "name: c\nslices:\n  - task: t\n    verify_cmds: [x]\n    editable_paths: [src/a.rs]\n    max_changed_files: 1\n    max_changed_lines: 9\n";

    fn decision(bob_json: &str) -> String {
        review_text(CAMPAIGN, bob_json).unwrap()
    }

    #[test]
    fn next_action_split_task_routes_to_split() {
        let j = r#"{"status":"not_converged","next_action":"split_task","changed_files":[]}"#;
        assert!(decision(j).contains("split_task"), "{}", decision(j));
    }

    #[test]
    fn stop_reason_scopeexceeded_routes_to_split() {
        let j = r#"{"status":"not_converged","stop_reason":"ScopeExceeded","changed_files":[]}"#;
        assert!(decision(j).contains("split_task"), "{}", decision(j));
    }

    #[test]
    fn converged_clean_routes_to_accept() {
        let j = r#"{"status":"converged","next_action":"done","changed_files":["src/a.rs"]}"#;
        assert!(decision(j).contains("accept"), "{}", decision(j));
    }

    #[test]
    fn needs_review_routes_to_human() {
        let j = r#"{"status":"needs_review","next_action":"review_candidate","changed_files":["src/a.rs"]}"#;
        assert!(decision(j).contains("accept_for_human_review"), "{}", decision(j));
    }

    // CROSS-REPO CONTRACT: must match bob::engine::is_test_path exactly.
    #[test]
    fn is_test_path_matches_bob() {
        use super::is_test_path;
        for p in [
            "tests/a.rs", "test/a.rs", "__tests__/a.js", "a_test.rs", "a_test.js",
            "a_test.py", "a.test.js", "a.test.ts", "a.spec.js", "a.spec.ts",
        ] {
            assert!(is_test_path(p), "should be a test path: {p}");
        }
        for p in ["src/main.rs", "src/foo.js", "lib/util.py"] {
            assert!(!is_test_path(p), "should NOT be a test path: {p}");
        }
    }
}
