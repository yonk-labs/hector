//! Single source of truth for the Bob campaign format that hector reads,
//! writes, and dispatches. Previously this struct was duplicated in `planner`
//! and `dispatch`, which let fields (e.g. `tier`) drift between the two paths.
//!
//! This is hector's *view* of the shared YAML contract with bob. It is
//! intentionally a superset of optional fields: hector emits a subset when
//! planning, and bob ignores fields it doesn't consume. Field names here MUST
//! match bob's `campaign::Slice` (see bob/src/campaign.rs).

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Campaign {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub auto_commit: bool,
    /// Campaign-level integration gates (e.g. `npm run test:all`, typecheck)
    /// not tied to any single slice. Consumed only by `hector dispatch`, which
    /// runs them against the merged tree after all slices apply; `bob campaign`
    /// ignores unknown top-level fields, so this stays contract-safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_cmds: Option<Vec<String>>,
    #[serde(default)]
    pub slices: Vec<Slice>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Slice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_cmds: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub editable_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iters: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_changed_files: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_changed_lines: Option<u64>,
    /// Tier override (cheap | medium | large | frontier). Honored by both
    /// `hector dispatch` (passes `--tier`) and `bob campaign`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Explicit builder model pin (a name from bob's builder.models or a raw
    /// provider/model id). Matches bob's campaign::Slice. A pinned slice is
    /// excluded from dispatch's endpoint round-robin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Fallback model chain, tried in order if the builder errors or stalls.
    /// Matches bob's campaign::Slice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_models: Vec<String>,
    /// Slice names this slice depends on. Consumed only by `hector dispatch`,
    /// which runs dependency-ordered batches and commits between them so later
    /// batches build on earlier results; `bob campaign` (sequential anyway)
    /// ignores it. Requires campaign `auto_commit: true`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards against drift with bob's campaign::Slice — these field names are
    /// the contract. If a rename here isn't mirrored in bob, the YAML silently
    /// stops round-tripping.
    #[test]
    fn emits_bob_field_names() {
        let c = Campaign {
            name: Some("c".into()),
            auto_commit: false,
            verify_cmds: None,
            slices: vec![Slice {
                name: Some("s".into()),
                task: Some("do x".into()),
                verify_cmds: Some(vec!["cargo test".into()]),
                editable_paths: vec!["src/x.rs".into()],
                tier: Some("medium".into()),
                max_changed_files: Some(2),
                max_changed_lines: Some(50),
                ..Default::default()
            }],
        };
        let yaml = serde_yaml::to_string(&c).unwrap();
        for field in ["task:", "verify_cmds:", "editable_paths:", "tier:", "max_changed_files:"] {
            assert!(yaml.contains(field), "missing `{field}` in:\n{yaml}");
        }
        // Unset optionals are omitted, not emitted as null.
        assert!(!yaml.contains("null"), "should not emit nulls:\n{yaml}");
    }

    /// Campaign-level verify_cmds (hector-dispatch-only) round-trips and is
    /// omitted from YAML when unset — bob never sees a spurious field.
    #[test]
    fn campaign_verify_cmds_round_trips_and_omits_when_none() {
        let yaml = "name: c\nverify_cmds: [npm run test:all]\nslices:\n  - task: t\n";
        let c: Campaign = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(c.verify_cmds.as_deref(), Some(&["npm run test:all".to_string()][..]));

        let none = Campaign { name: Some("c".into()), ..Default::default() };
        let emitted = serde_yaml::to_string(&none).unwrap();
        assert!(!emitted.contains("verify_cmds"), "unset field must be omitted:\n{emitted}");
    }

    #[test]
    fn round_trips() {
        let yaml = "name: c\nslices:\n  - task: t\n    verify_cmds: [echo ok]\n    editable_paths: [src/a.rs]\n    tier: cheap\n";
        let c: Campaign = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(c.slices.len(), 1);
        assert_eq!(c.slices[0].tier.as_deref(), Some("cheap"));
    }

    /// CROSS-REPO CONTRACT: the SAME campaign YAML asserted by bob's
    /// campaign::campaign_field_name_contract. Every field must keep parsing on
    /// this side too — a rename in hector's Slice silently drops a field bob
    /// would otherwise read. If this fails, the field name diverged from bob.
    #[test]
    fn parses_full_contract() {
        let yaml = r#"
name: c
auto_commit: true
slices:
  - name: s
    task: do x
    spec: the spec
    verify_cmds: [cargo test x]
    editable_paths: [src/x.rs]
    reference_paths: [tests/x_test.rs]
    judge_policy: blocking
    max_iters: 3
    max_changed_files: 5
    max_changed_lines: 50
    tier: medium
    model: qwen-193
    fallback_models: [gemma-133]
"#;
        let c: Campaign = serde_yaml::from_str(yaml).unwrap();
        assert!(c.auto_commit);
        let s = &c.slices[0];
        assert_eq!(s.model.as_deref(), Some("qwen-193"));
        assert_eq!(s.fallback_models, vec!["gemma-133"]);
        assert_eq!(s.task.as_deref(), Some("do x"));
        assert_eq!(s.spec.as_deref(), Some("the spec"));
        assert_eq!(s.verify_cmds.as_deref(), Some(&["cargo test x".to_string()][..]));
        assert_eq!(s.editable_paths, vec!["src/x.rs"]);
        assert_eq!(s.reference_paths, vec!["tests/x_test.rs"]);
        assert_eq!(s.judge_policy.as_deref(), Some("blocking"));
        assert_eq!(s.max_iters, Some(3));
        assert_eq!(s.max_changed_files, Some(5));
        assert_eq!(s.max_changed_lines, Some(50));
        assert_eq!(s.tier.as_deref(), Some("medium"));
    }
}
