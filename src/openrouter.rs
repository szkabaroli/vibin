//! Optional OpenRouter integration — currently just one job: summarize a
//! conversation into a short title, the way Zed does, when the agent
//! itself didn't provide one. Off unless `OPENROUTER_API_KEY` is set.
//!
//! No HTTP dependency: the request goes through `curl` (already a hard
//! requirement, see `update.rs`) and the reply is small, so a single
//! blocking call on a background thread is plenty.

use serde_json::{Value, json};
use std::process::Command;

/// The default title model — a free reasoning model. Overridable via the
/// `title_model` config key.
pub const DEFAULT_MODEL: &str = "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free";

/// Zed's thread-title prompt, verbatim
/// (crates/agent_settings/src/prompts/summarize_thread_prompt.txt).
const SUMMARIZE_PROMPT: &str = "Generate a concise 3-7 word title for this conversation, omitting punctuation.\nGo straight to the title, without any preamble and prefix like `Here's a concise suggestion:...` or `Title:`.\nIf the conversation is about a specific subject, include it in the title.\nBe descriptive. DO NOT speak in the first person.";

/// The OpenRouter key from the environment, if configured.
pub fn api_key() -> Option<String> {
    std::env::var("OPENROUTER_API_KEY").ok().filter(|k| !k.trim().is_empty())
}

/// Ask the model for a short title for `transcript`. Blocking (call it on a
/// thread); None on any network/parse failure.
pub fn generate_title(key: &str, model: &str, transcript: &str) -> Option<String> {
    let body = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": SUMMARIZE_PROMPT },
            { "role": "user", "content": transcript },
        ],
        "max_tokens": 32,
        "temperature": 0.3,
    })
    .to_string();
    let out = Command::new("curl")
        .args([
            "-s",
            "--max-time",
            "30",
            "-X",
            "POST",
            "https://openrouter.ai/api/v1/chat/completions",
            "-H",
            &format!("Authorization: Bearer {key}"),
            "-H",
            "Content-Type: application/json",
            "-d",
            &body,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let reply: Value = serde_json::from_slice(&out.stdout).ok()?;
    let content = reply.pointer("/choices/0/message/content").and_then(Value::as_str)?;
    let title = clean_title(content);
    (!title.is_empty()).then_some(title)
}

/// Tidy a model's reply into a title: first non-empty line, a leading
/// `Title:` / quotes / trailing punctuation stripped, capped at 7 words.
fn clean_title(raw: &str) -> String {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    // reasoning models sometimes prefix with a label
    let line = line.strip_prefix("Title:").map(str::trim).unwrap_or(line);
    let line = line.trim_matches(|c: char| c == '"' || c == '\'' || c == '`' || c == '.');
    line.split_whitespace().take(7).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_title_strips_noise_and_caps_words() {
        assert_eq!(clean_title("Fix the parser bug"), "Fix the parser bug");
        assert_eq!(clean_title("  \"Debug authentication flow\"  "), "Debug authentication flow");
        assert_eq!(clean_title("Title: Rewrite the config loader."), "Rewrite the config loader");
        assert_eq!(clean_title("one line\nsecond ignored"), "one line");
        let long = clean_title("a b c d e f g h i j");
        assert_eq!(long.split_whitespace().count(), 7);
        assert_eq!(clean_title("   "), "");
    }
}
