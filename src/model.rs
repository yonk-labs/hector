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

/// Extract the outermost JSON object from text that may have markdown fences,
/// surrounding prose, or reasoning-model `<think>` blocks. Models don't always
/// follow "JSON only" instructions.
pub fn extract_json(text: &str) -> String {
    let cleaned = strip_think_blocks(text);
    let start = cleaned.find('{');
    let end = cleaned.rfind('}');
    match (start, end) {
        (Some(s), Some(e)) if s < e => cleaned[s..=e].to_string(),
        _ => cleaned,
    }
}

/// Remove `<think>...</think>` reasoning blocks (MiniMax-M3 and friends wrap
/// their answer in them, and the reasoning often contains `{`/`}` that fool
/// the brace scan). An unclosed `<think>` means the answer never arrived —
/// everything after it is reasoning, so it's dropped.
fn strip_think_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("<think>") {
        out.push_str(&rest[..open]);
        match rest[open..].find("</think>") {
            Some(close) => rest = &rest[open + close + "</think>".len()..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Degenerate-output guard: local models under context pressure loop the same
/// phrase dozens of times ("// I'll use the 'fed' case." x33) until the token
/// ceiling truncates the JSON mid-string. The loop usually lives INSIDE a JSON
/// string, so the separators are escaped `\n` two-char sequences, not real
/// newlines — split on both. Any non-trivial segment repeated ≥8 times means
/// the response is garbage — fail fast and rotate models instead of parsing a
/// truncated tail.
/// ponytail: delimiter-split repeat count; add n-gram scan if models loop without separators.
pub fn is_looping_output(text: &str) -> bool {
    let mut counts: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    let segments = text
        .lines()
        .flat_map(|l| l.split("\\n"))
        .map(str::trim)
        .filter(|l| l.len() >= 10);
    for seg in segments {
        let c = counts.entry(seg).or_insert(0);
        *c += 1;
        if *c >= 8 {
            return true;
        }
    }
    false
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
    fn extract_json_strips_think_blocks() {
        // Reasoning contains braces that would fool the outermost-brace scan.
        let input = "<think>Let me plan {this} carefully. {\"draft\": true}</think>\n{\"a\": 1}";
        assert_eq!(extract_json(input), r#"{"a": 1}"#);
        // Multiple blocks.
        let input = "<think>x</think>prefix {\"a\": 2} <think>y</think>";
        assert_eq!(extract_json(input), r#"{"a": 2}"#);
    }

    #[test]
    fn extract_json_drops_unclosed_think_block() {
        // Truncated mid-reasoning: no answer exists, nothing JSON-shaped survives.
        let input = "<think>still reasoning {\"partial\": ";
        assert_eq!(extract_json(input), "");
    }

    #[test]
    fn looping_output_detected() {
        let looped = "I'll use the 'fed' case here.\n".repeat(30);
        assert!(is_looping_output(&looped));
        // Normal JSON responses and short repeats don't trip the guard.
        assert!(!is_looping_output("{\"test_code\": \"ok\", \"test_path\": \"a.test.ts\"}"));
        assert!(!is_looping_output(&"distinct line one\n".repeat(3)));
        // Trivial short lines (blank, braces) repeat legitimately.
        assert!(!is_looping_output(&"}\n\n{\n".repeat(40)));
    }

    #[test]
    fn looping_inside_json_string_detected() {
        // The gemma-26B death spiral: the repeated phrase lives INSIDE a JSON
        // string, separated by escaped \n — the raw response is one long line.
        let looped = format!(
            "{{\"test_code\": \"{}",
            "// I'll use the 'fed' case.\\n ".repeat(33)
        );
        assert!(is_looping_output(&looped));
    }
}
