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
/// the brace scan). Two asymmetric cases matter:
///   - unclosed `<think>`: the answer never arrived — everything after the
///     tag is reasoning, dropped;
///   - orphan `</think>` with no opening tag: some chat templates put the
///     opening `<think>` in the PROMPT, so the completion is reasoning that
///     just ends with `</think>` — everything before it is reasoning, dropped.
fn strip_think_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let open = rest.find("<think>");
        let close = rest.find("</think>");
        match (open, close) {
            // Closing tag first (orphan): what precedes it is reasoning.
            (Some(o), Some(c)) if c < o => rest = &rest[c + "</think>".len()..],
            (None, Some(c)) => rest = &rest[c + "</think>".len()..],
            // Opening tag first: keep what precedes it, drop the block.
            (Some(o), _) => {
                out.push_str(&rest[..o]);
                match rest[o..].find("</think>") {
                    Some(c) => rest = &rest[o + c + "</think>".len()..],
                    None => rest = "",
                }
            }
            (None, None) => break,
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
///
/// Judged on the ANSWER, not the reasoning: think blocks are stripped first,
/// because a reasoning model legitimately re-quotes the same line while
/// ruminating. A loop entirely inside an unclosed think block strips to
/// nothing and fails as a parse error instead — same rotation, right label.
/// ponytail: delimiter-split repeat count; add n-gram scan if models loop without separators.
pub fn is_looping_output(raw: &str) -> bool {
    let text = strip_think_blocks(raw);
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
    fn extract_json_handles_real_minimax_m3_response() {
        // Verbatim MiniMax-M3 completion captured live 2026-07-03 (trimmed
        // mid-reasoning): quoted JSON keys and paths inside the think block,
        // answer after it.
        let real = "<think>The user wants a JSON object with two keys: \"test_path\" and \"verify_cmd\". This is for a vitest test of a function `clamp(x, lo, hi)`.\n\nLet me think about reasonable values:\n- test_path: a path to a test file, e.g., \"./tests/clamp.test.js\" or \"src/__tests__/clamp.test.js\"\n\nLet me provide a reasonable, common convention.</think>\n\n{\"test_path\":\"tests/clamp.test.js\",\"verify_cmd\":\"npx vitest run tests/clamp.test.js\"}";
        let json = extract_json(real);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["test_path"], "tests/clamp.test.js");
    }

    #[test]
    fn extract_json_handles_orphan_closing_tag() {
        // R1-style templates put the opening <think> in the PROMPT: the
        // completion is reasoning (with braces!) ending in </think>, then
        // the answer — no opening tag anywhere in the response.
        let input = "Let me weigh {option: 1} vs {option: 2}...</think>\n{\"a\": 1}";
        assert_eq!(extract_json(input), r#"{"a": 1}"#);
        // Orphan close then a normal closed pair later.
        let input = "reasoning {x}</think>{\"a\": 2}<think>post-hoc {y}</think>";
        assert_eq!(extract_json(input), r#"{"a": 2}"#);
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
    fn looping_guard_judges_answer_not_reasoning() {
        // A reasoning model re-quoting the same line while ruminating is a
        // GOOD response — the guard must not rotate away from it.
        let ruminating = format!(
            "<think>{}</think>{{\"test_code\": \"ok\"}}",
            "maybe: import {{ createGameState }} from './helpers';\n".repeat(12)
        );
        assert!(!is_looping_output(&ruminating));
        // But a loop in the ANSWER (after the think block) still trips it.
        let bad = format!("<think>brief</think>{}", "// the 'fed' case again\n".repeat(30));
        assert!(is_looping_output(&bad));
        // Loop inside an UNCLOSED think block: strips to nothing → not flagged
        // as looping; the empty answer fails the JSON parse path instead.
        let truncated = format!("<think>{}", "I'll use the 'fed' case.\n".repeat(30));
        assert!(!is_looping_output(&truncated));
        assert_eq!(extract_json(&truncated), "");
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
