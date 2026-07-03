use serde::Deserialize;
use std::path::{Path, PathBuf};

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
# Standing constraints appended to every generated slice spec.
# invariants:
#   - never weaken or delete an existing assertion
#   - no new dependencies
#   - pure functions stay pure; state stays JSON-plain
# Models are tried in order (default_model first) — a model whose output is
# unusable rotates to the next. Coder models (qwen) make better test-writers
# than general chat models on long specs.
# models:
#   - { name: qwen,    model: \"Intel/Qwen3-Coder-Next-int4-AutoRound\", base_url: \"http://192.168.1.193:8000/v1\" }
#   - { name: gemma,   model: \"cyankiwi/gemma-4-26B-A4B-it-AWQ-4bit\", base_url: \"http://192.168.1.133:8000/v1\" }
#   - { name: minimax, model: \"MiniMax-M3\", base_url: \"https://api.minimax.io/v1\", api_key_env: MINIMAX_API_KEY }
# default_model: qwen
# This file is also honored at ~/.config/hector/config.yaml (./hector.yaml wins).
# maple:
#   budget: 12000   # max approx context tokens when scoping via --symbol
";

#[derive(Debug)]
pub struct PlanDefaults {
    pub max_changed_files: u64,
    pub max_changed_lines: u64,
    pub judge_policy: String,
    pub auto_commit: bool,
    /// Standing constraints ("never weaken an assertion", "no new deps")
    /// appended to every generated slice spec.
    pub invariants: Vec<String>,
    /// Context-token budget for maple-derived scope (`hector plan --symbol`).
    pub maple_budget: u64,
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
            invariants: cfg.invariants,
            maple_budget: cfg.maple.budget.unwrap_or(self.maple_budget),
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
            invariants: Vec::new(),
            maple_budget: 12_000,
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
    #[serde(default)]
    invariants: Vec<String>,
    #[serde(default)]
    maple: MapleConfig,
}

#[derive(Debug, Default, Deserialize)]
struct MapleConfig {
    budget: Option<u64>,
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

/// Where hector looks for config, in order. Shown in error/doctor output so a
/// missing config is never a silent mystery.
pub const CONFIG_SEARCH_HINT: &str = "./hector.yaml, then ~/.config/hector/config.yaml";

/// Locate the config file: project-local `./hector.yaml` first, then the
/// per-user `~/.config/hector/config.yaml` (same fallback shape bob has) so
/// LAN model endpoints don't need copying into every repo.
pub fn find_config_file(cwd: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let local = cwd.join("hector.yaml");
    if local.exists() {
        return Some(local);
    }
    let global = home?.join(".config").join("hector").join("config.yaml");
    global.exists().then_some(global)
}

fn config_path() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    find_config_file(&cwd, home.as_deref())
}

fn load_config() -> anyhow::Result<Option<(PathBuf, HectorConfig)>> {
    let Some(path) = config_path() else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
    let cfg: HectorConfig = serde_yaml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("invalid {}: {e}", path.display()))?;
    Ok(Some((path, cfg)))
}

/// Load review config for the deep-review tier.
pub fn load_review_config() -> anyhow::Result<ReviewConfig> {
    Ok(load_config()?.map(|(_, c)| c.review).unwrap_or_default())
}

pub fn load_plan_defaults() -> anyhow::Result<PlanDefaults> {
    match load_config()? {
        Some((_, cfg)) => Ok(PlanDefaults::default().merge(cfg)),
        None => Ok(PlanDefaults::default()),
    }
}

/// All configured models, default first — this is the planner's rotation
/// order when a model produces unusable output. Empty = no models configured.
pub fn load_models() -> anyhow::Result<Vec<ModelCfg>> {
    let Some((_, cfg)) = load_config()? else {
        return Ok(Vec::new());
    };
    let mut models = cfg.models;
    if let Some(default) = cfg.default_model.as_deref() {
        if let Some(pos) = models.iter().position(|m| m.name == default) {
            let m = models.remove(pos);
            models.insert(0, m);
        }
    }
    Ok(models)
}

/// Config summary for `hector doctor`: which file was found and the models in
/// rotation order.
pub struct DoctorView {
    pub path: PathBuf,
    pub models: Vec<ModelCfg>,
    pub default_model: Option<String>,
}

pub fn doctor_view() -> anyhow::Result<Option<DoctorView>> {
    let Some((path, cfg)) = load_config()? else {
        return Ok(None);
    };
    Ok(Some(DoctorView {
        path,
        default_model: cfg.default_model.clone(),
        models: cfg.models,
    }))
}

#[cfg(test)]
mod tests {
    use super::find_config_file;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hector-cfg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn local_config_wins_then_global_then_none() {
        let cwd = tmp("cwd");
        let home = tmp("home");
        // Nothing anywhere → None (the caller reports the search paths).
        assert_eq!(find_config_file(&cwd, Some(&home)), None);
        // Global fallback found.
        let global_dir = home.join(".config").join("hector");
        std::fs::create_dir_all(&global_dir).unwrap();
        std::fs::write(global_dir.join("config.yaml"), "{}").unwrap();
        assert_eq!(
            find_config_file(&cwd, Some(&home)),
            Some(global_dir.join("config.yaml"))
        );
        // Local hector.yaml beats global.
        std::fs::write(cwd.join("hector.yaml"), "{}").unwrap();
        assert_eq!(find_config_file(&cwd, Some(&home)), Some(cwd.join("hector.yaml")));
        // No HOME → only local is considered.
        assert_eq!(find_config_file(&cwd, None), Some(cwd.join("hector.yaml")));
    }
}
