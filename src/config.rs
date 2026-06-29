use serde::Deserialize;

use crate::model::ModelCfg;

pub const DEFAULT_CONFIG: &str = "# Hector config — TDD planning defaults + model endpoints.
# Models are used for test-writing and PRD splitting when --verify is omitted.
verify:
  prefer_focused: true
scope:
  forbid_dependency_churn: true
  default_max_changed_files: 2
  default_max_changed_lines: 160
judge:
  default_policy: retry_on_fail
bob:
  campaign_auto_commit: true
# models:
#   - { name: qwen,    model: \"Intel/Qwen3-Coder-Next-int4-AutoRound\", base_url: \"http://192.168.1.193:8000/v1\" }
#   - { name: gemma,   model: \"cyankiwi/gemma-4-26B-A4B-it-AWQ-4bit\", base_url: \"http://192.168.1.133:8000/v1\" }
#   - { name: minimax, model: \"MiniMax-M3\", base_url: \"https://api.minimax.io/v1\", api_key_env: MINIMAX_API_KEY }
# default_model: qwen
";

#[derive(Debug)]
pub struct PlanDefaults {
    pub max_changed_files: u64,
    pub max_changed_lines: u64,
    pub judge_policy: String,
    pub auto_commit: bool,
}

impl PlanDefaults {
    fn merge(self, cfg: HectorConfig) -> Self {
        Self {
            max_changed_files: cfg
                .scope
                .default_max_changed_files
                .unwrap_or(self.max_changed_files),
            max_changed_lines: cfg
                .scope
                .default_max_changed_lines
                .unwrap_or(self.max_changed_lines),
            judge_policy: cfg.judge.default_policy.unwrap_or(self.judge_policy),
            auto_commit: cfg.bob.campaign_auto_commit.unwrap_or(self.auto_commit),
        }
    }
}

impl Default for PlanDefaults {
    fn default() -> Self {
        Self {
            max_changed_files: 2,
            max_changed_lines: 160,
            judge_policy: "retry_on_fail".to_string(),
            auto_commit: true,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct HectorConfig {
    #[serde(default)]
    scope: ScopeConfig,
    #[serde(default)]
    judge: JudgeConfig,
    #[serde(default)]
    bob: BobConfig,
    #[serde(default)]
    models: Vec<ModelCfg>,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    review: ReviewConfig,
}

#[derive(Debug, Default, Deserialize)]
struct ScopeConfig {
    default_max_changed_files: Option<u64>,
    default_max_changed_lines: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct JudgeConfig {
    default_policy: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct BobConfig {
    campaign_auto_commit: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ReviewConfig {
    /// Abe reviewer name for the deep (expensive) review. e.g. "codex".
    /// When set, hector runs `abe validate --reviewer <deep_reviewer>` on accepted
    /// slices before handing results to the frontier model.
    #[serde(default)]
    pub deep_reviewer: Option<String>,
    /// Run deep review automatically when deterministic review says "accept".
    #[serde(default)]
    pub deep_on_accept: bool,
}

impl ReviewConfig {
    pub fn deep_enabled(&self) -> bool {
        self.deep_reviewer.is_some()
    }
}

/// Load review config for the deep-review tier.
pub fn load_review_config() -> anyhow::Result<ReviewConfig> {
    let text = match std::fs::read_to_string("hector.yaml") {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ReviewConfig::default()),
        Err(e) => anyhow::bail!("failed to read hector.yaml: {e}"),
    };
    let cfg: HectorConfig =
        serde_yaml::from_str(&text).map_err(|e| anyhow::anyhow!("invalid hector.yaml: {e}"))?;
    Ok(cfg.review)
}

pub fn load_plan_defaults() -> anyhow::Result<PlanDefaults> {
    let text = match std::fs::read_to_string("hector.yaml") {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(PlanDefaults::default()),
        Err(e) => anyhow::bail!("failed to read hector.yaml: {e}"),
    };
    let cfg: HectorConfig =
        serde_yaml::from_str(&text).map_err(|e| anyhow::anyhow!("invalid hector.yaml: {e}"))?;
    Ok(PlanDefaults::default().merge(cfg))
}

/// Load the default model config (if any). Returns None if no models configured.
pub fn load_default_model() -> anyhow::Result<Option<ModelCfg>> {
    let text = match std::fs::read_to_string("hector.yaml") {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => anyhow::bail!("failed to read hector.yaml: {e}"),
    };
    let cfg: HectorConfig =
        serde_yaml::from_str(&text).map_err(|e| anyhow::anyhow!("invalid hector.yaml: {e}"))?;
    if cfg.models.is_empty() {
        return Ok(None);
    }
    let pick = cfg
        .default_model
        .as_deref()
        .unwrap_or(&cfg.models[0].name)
        .to_string();
    Ok(cfg.models.into_iter().find(|m| m.name == pick))
}
