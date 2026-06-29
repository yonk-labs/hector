//! Minimal OpenAI-compatible completion client via curl. Zero new deps — same
//! pattern as bob shelling out to opencode. Hector uses this for test-writing
//! and PRD splitting; one-off calls, not high-throughput.

use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[derive(Debug, Clone, Deserialize)]
pub struct ModelCfg {
    pub name: String,
    pub model: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

/// Call an OpenAI-compatible chat endpoint. Returns the assistant message content.
pub async fn complete(cfg: &ModelCfg, system: &str, user: &str) -> anyhow::Result<String> {
    let api_key = cfg
        .api_key_env
        .as_ref()
        .and_then(|env| std::env::var(env).ok());

    let body = serde_json::to_string(&ChatRequest {
        model: &cfg.model,
        messages: vec![
            ChatMessage { role: "system", content: system },
            ChatMessage { role: "user", content: user },
        ],
        temperature: 0.2,
        max_tokens: 4096,
    })?;

    let url = format!(
        "{}/chat/completions",
        cfg.base_url.trim_end_matches('/')
    );

    let mut cmd = Command::new("curl");
    cmd.arg("-s")
        .arg("--max-time")
        .arg("120")
        .arg("-X")
        .arg("POST")
        .arg(&url)
        .arg("-H")
        .arg("Content-Type: application/json");

    if let Some(key) = &api_key {
        cmd.arg("-H").arg(format!("Authorization: Bearer {key}"));
    }

    cmd.arg("-d")
        .arg("@-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(body.as_bytes()).await?;
    }
    let output = child.wait_with_output().await?;

    if !output.status.success() {
        anyhow::bail!(
            "model call failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let resp: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| anyhow::anyhow!("parse response: {e}; {stdout}"))?;

    if let Some(err) = resp.get("error") {
        anyhow::bail!("model API error: {err}");
    }

    resp["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("no content in model response"))
}

/// Extract the outermost JSON object from text that may have markdown fences
/// or surrounding prose. Models don't always follow "JSON only" instructions.
pub fn extract_json(text: &str) -> &str {
    let start = text.find('{');
    let end = text.rfind('}');
    match (start, end) {
        (Some(s), Some(e)) if s < e => &text[s..=e],
        _ => text,
    }
}

/// Rough token estimate (~4 chars/token). Same heuristic abe uses.
pub fn est_tokens(s: &str) -> usize {
    (s.len() / 4).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_strips_fences() {
        let input = "```json\n{\"a\": 1}\n```";
        assert_eq!(extract_json(input), r#"{"a": 1}"#);
    }

    #[test]
    fn extract_json_finds_object_in_prose() {
        let input = "Here is the plan:\n{\"a\": 1, \"b\": [2]}\nThat's it.";
        assert_eq!(extract_json(input), r#"{"a": 1, "b": [2]}"#);
    }

    #[test]
    fn est_tokens_basic() {
        assert!(est_tokens("hello world this is a test") >= 6);
        assert_eq!(est_tokens(""), 1);
    }
}
