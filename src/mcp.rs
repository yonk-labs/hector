//! MCP stdio server exposing Hector's planning, checking, review, and brief tools.

use crate::{guidance, planner};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::{schemars, tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct HectorServer {
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PlanCampaignParams {
    /// One self-contained behavior to implement.
    pub task: String,
    /// Optional campaign and slice name.
    #[serde(default)]
    pub name: Option<String>,
    /// Exact rules, edge cases, and acceptance details.
    #[serde(default)]
    pub spec: Option<String>,
    /// Deterministic verification commands.
    #[serde(default)]
    pub verify_cmds: Option<Vec<String>>,
    /// Production files or directories Bob may edit.
    #[serde(default)]
    pub editable_paths: Option<Vec<String>>,
    /// Files Bob may read but not edit.
    #[serde(default)]
    pub reference_paths: Option<Vec<String>>,
    /// Max changed files cap.
    #[serde(default)]
    pub max_changed_files: Option<u64>,
    /// Max changed lines cap.
    #[serde(default)]
    pub max_changed_lines: Option<u64>,
    /// Max Bob iterations.
    #[serde(default)]
    pub max_iters: Option<u32>,
    /// Bob judge policy.
    #[serde(default)]
    pub judge_policy: Option<String>,
    /// Whether Bob may auto-commit a passing result.
    #[serde(default)]
    pub auto_commit: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CheckCampaignParams {
    /// Bob campaign YAML or JSON text.
    pub campaign: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReviewResultParams {
    /// Original Bob campaign YAML or JSON text.
    pub campaign: String,
    /// Bob result JSON text.
    pub bob_result: String,
}

#[derive(Serialize)]
struct PlannedOutput {
    status: &'static str,
    campaign_yaml: String,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct CheckOutput {
    status: &'static str,
    findings: Vec<String>,
}

#[tool_router]
impl HectorServer {
    #[tool(description = "Print the frontier-model instructions for writing Hector-ready slices.")]
    pub async fn frontier_brief(&self) -> String {
        guidance::FRONTIER_BRIEF.to_string()
    }

    #[tool(description = "Draft a Bob campaign from a Hector-ready task/spec and guardrails.")]
    pub async fn plan_campaign(&self, Parameters(p): Parameters<PlanCampaignParams>) -> String {
        json_or_error(plan_campaign(p))
    }

    #[tool(description = "Validate a Bob campaign before handing it to Bob.")]
    pub async fn check_campaign(&self, Parameters(p): Parameters<CheckCampaignParams>) -> String {
        json_or_error(check_campaign(p))
    }

    #[tool(description = "Review Bob's result against the original Hector campaign.")]
    pub async fn review_result(&self, Parameters(p): Parameters<ReviewResultParams>) -> String {
        json_or_error(planner::review_text(&p.campaign, &p.bob_result))
    }
}

impl HectorServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

fn plan_campaign(p: PlanCampaignParams) -> anyhow::Result<String> {
    let planned = planner::plan(planner::PlanOptions {
        task: p.task,
        name: p.name,
        spec: p.spec,
        verify_cmds: p.verify_cmds.unwrap_or_default(),
        editable_paths: p.editable_paths.unwrap_or_default(),
        reference_paths: p.reference_paths.unwrap_or_default(),
        max_changed_files: p.max_changed_files.unwrap_or(2),
        max_changed_lines: p.max_changed_lines.unwrap_or(160),
        max_iters: p.max_iters.unwrap_or(4),
        judge_policy: p
            .judge_policy
            .unwrap_or_else(|| "retry_on_fail".to_string()),
        auto_commit: p.auto_commit.unwrap_or(true),
        // Same standing invariants the CLI path injects — MCP-planned
        // campaigns must not silently skip the house rules.
        invariants: crate::config::load_plan_defaults()
            .map(|d| d.invariants)
            .unwrap_or_default(),
    })?;

    if planned.trim_start().starts_with('{') {
        return Ok(planned);
    }

    Ok(serde_json::to_string_pretty(&PlannedOutput {
        status: "planned",
        campaign_yaml: planned,
        warnings: Vec::new(),
    })?)
}

fn check_campaign(p: CheckCampaignParams) -> anyhow::Result<String> {
    planner::check_text(&p.campaign)?;
    Ok(serde_json::to_string_pretty(&CheckOutput {
        status: "pass",
        findings: Vec::new(),
    })?)
}

#[tool_handler]
impl ServerHandler for HectorServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "hector: TDD/spec planner that turns frontier intent into Bob campaigns.".into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

fn json_or_error(r: anyhow::Result<String>) -> String {
    match r {
        Ok(s) => s,
        Err(e) => format!(
            "{{\"error\":{}}}",
            serde_json::to_string(&e.to_string())
                .unwrap_or_else(|_| "\"internal serialization error\"".to_string())
        ),
    }
}

pub async fn serve() -> anyhow::Result<()> {
    let server = HectorServer::new();
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frontier_brief_tool_returns_handoff_contract() {
        let server = HectorServer::new();
        let out = server.frontier_brief().await;
        assert!(out.contains("Hector Frontier Brief"));
        assert!(out.contains("editable_paths"));
    }

    #[test]
    fn plan_campaign_tool_wraps_valid_yaml() {
        let out = plan_campaign(PlanCampaignParams {
            task: "Implement focused behavior.".into(),
            name: Some("focused-behavior".into()),
            spec: None,
            verify_cmds: Some(vec!["cargo test focused_behavior".into()]),
            editable_paths: Some(vec!["src/lib.rs".into()]),
            reference_paths: Some(vec!["tests/focused_behavior.rs".into()]),
            max_changed_files: Some(1),
            max_changed_lines: Some(80),
            max_iters: Some(4),
            judge_policy: Some("retry_on_fail".into()),
            auto_commit: Some(true),
        })
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["status"], "planned");
        assert!(json["campaign_yaml"].as_str().unwrap().contains("slices:"));
    }

    #[test]
    fn check_campaign_tool_reports_pass() {
        let out = check_campaign(CheckCampaignParams {
            campaign: r#"
name: ok
slices:
  - task: Implement focused behavior.
    verify_cmds: ["cargo test focused_behavior"]
    editable_paths: ["src/lib.rs"]
    max_changed_files: 1
    max_changed_lines: 80
"#
            .into(),
        })
        .unwrap();
        assert!(out.contains("\"status\": \"pass\""));
    }
}
