use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod claude;
pub mod follower;

/// Claude Code munges the cwd into a directory name where every character
/// outside `[a-zA-Z0-9]` becomes `-` (verified against 2.1.316 on this machine:
/// `/Users/admin/code/claude-g` → `-Users-admin-code-claude-g`).
pub fn munge_dir(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub fn jsonl_path(home: &std::path::Path, cwd: &str, session_id: &str) -> PathBuf {
    home.join(".claude")
        .join("projects")
        .join(munge_dir(cwd))
        .join(format!("{session_id}.jsonl"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    User,
    Assistant,
}

/// One tool invocation from an assistant `tool_use` block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Human-readable one-line label (the tool's `description` when present).
    pub label: String,
    /// Short technical summary of the input (command, path, pattern, …).
    pub detail: String,
}

/// A `tool_result` block from a user line, attached to its call by id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub call_id: String,
    pub is_error: bool,
    pub text: String,
}

/// A chat message extracted from a session JSONL line. `key` is stable:
/// user lines use the line `uuid`, assistant lines use `message.id` because
/// claude writes 1–3 snapshot lines per message id while streaming,
/// tool results use `tr-<tool_use_id>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatMsg {
    pub key: String,
    pub role: Role,
    pub text: String,
    pub thinking: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub tool_result: Option<ToolResult>,
    pub ts: Option<String>,
    pub done: bool,
}

const TOOL_DETAIL_MAX: usize = 200;
const TOOL_LABEL_MAX: usize = 80;
const TOOL_RESULT_MAX: usize = 4000;

fn truncate_label(s: &str, max: usize) -> String {
    let one_line = s.replace('\n', " ");
    let one_line = one_line.trim();
    if one_line.chars().count() <= max {
        one_line.to_string()
    } else {
        let mut out: String = one_line.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// One-line label for the step list: prefer the tool's human `description`,
/// then the most meaningful input (command/path/pattern), then the tool name.
fn summarize_tool_label(input: Option<&Value>, name: &str) -> String {
    if let Some(obj) = input.and_then(Value::as_object) {
        if let Some(desc) = obj.get("description").and_then(Value::as_str) {
            let desc = desc.trim();
            if !desc.is_empty() {
                return truncate_label(desc, TOOL_LABEL_MAX);
            }
        }
    }
    let detail = summarize_tool_input(input);
    if detail.trim().is_empty() {
        name.to_string()
    } else {
        truncate_label(&detail, TOOL_LABEL_MAX)
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n…（输出已截断）");
    out
}

/// Compact one-line summary of a tool_use `input` object.
fn summarize_tool_input(input: Option<&Value>) -> String {
    let Some(input) = input else {
        return String::new();
    };
    if let Some(obj) = input.as_object() {
        for key in [
            "command",
            "file_path",
            "path",
            "pattern",
            "url",
            "query",
            "description",
            "prompt",
        ] {
            if let Some(v) = obj.get(key).and_then(Value::as_str) {
                let v = v.trim();
                if !v.is_empty() {
                    let one_line = v.replace('\n', " ⏎ ");
                    return truncate_chars(&one_line, TOOL_DETAIL_MAX);
                }
            }
        }
        // Fall back to a compact JSON dump of the whole input.
        let compact = input.to_string().replace('\n', " ");
        return truncate_chars(&compact, TOOL_DETAIL_MAX);
    }
    truncate_chars(&input.to_string().replace('\n', " "), TOOL_DETAIL_MAX)
}

/// tool_result content is either a plain string or an array of text blocks.
fn extract_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => {
            let mut out = String::new();
            for item in items {
                if let Some(t) = item.get("text").and_then(Value::as_str) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(t);
                }
            }
            out
        }
        Some(Value::Object(o)) => o
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// Line prefixes that mark synthetic/system echoes inside string-type user
/// content (slash-command machinery, harness notifications) — never shown.
const SKIP_USER_PREFIXES: [&str; 6] = [
    "<command-name>",
    "<command-message>",
    "<local-command-caveat>",
    "<local-command-stdout>",
    "<system-reminder>",
    "<task-summary>",
];

/// Tolerant extraction from one JSONL line. Returns zero or more displayable
/// chat messages (a user line can carry several tool_result blocks). Unknown
/// line types are ignored silently.
pub fn parse_line(v: &Value) -> Vec<ChatMsg> {
    let Some(line_type) = v.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Some(message) = v.get("message") else {
        return Vec::new();
    };
    let ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());

    match line_type {
        "user" => {
            if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
                return Vec::new();
            }
            if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
                return Vec::new();
            }
            // Array content = tool results, or a prompt that carried images.
            if let Some(blocks) = message.get("content").and_then(Value::as_array) {
                let mut out = Vec::new();
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let call_id = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if call_id.is_empty() {
                        continue;
                    }
                    let is_error = block
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let text = extract_result_text(block.get("content"));
                    out.push(ChatMsg {
                        key: format!("tr-{call_id}"),
                        role: Role::User,
                        text: String::new(),
                        thinking: None,
                        tool_calls: Vec::new(),
                        tool_result: Some(ToolResult {
                            call_id,
                            is_error,
                            text: truncate_chars(&text, TOOL_RESULT_MAX),
                        }),
                        ts: ts.clone(),
                        done: true,
                    });
                }
                if !out.is_empty() {
                    return out;
                }
                // A prompt with pasted screenshots is stored as an array of
                // text + image blocks (e.g. "[Image #1] Take a look"). It must
                // surface as a user turn: unlike a plain-text prompt, the
                // frontend's optimistic bubble is the only other copy, and it
                // is not recreated from anywhere else. Claude Code also writes
                // a model-facing echo of the same turn ("[Image: source: …]")
                // that has no image block — that one is skipped.
                let has_image = blocks
                    .iter()
                    .any(|b| b.get("type").and_then(Value::as_str) == Some("image"));
                if !has_image {
                    return Vec::new();
                }
                let text = blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                let mut text = text.trim().to_string();
                // An image-only prompt may carry no text block at all; keep a
                // marker so the turn still reaches the mirror.
                if text.is_empty() {
                    text = "[Image #1]".to_string();
                }
                let Some(key) = v.get("uuid").and_then(Value::as_str) else {
                    return Vec::new();
                };
                if SKIP_USER_PREFIXES.iter().any(|p| text.starts_with(p)) {
                    return Vec::new();
                }
                return vec![ChatMsg {
                    key: key.to_string(),
                    role: Role::User,
                    text: text.to_string(),
                    thinking: None,
                    tool_calls: Vec::new(),
                    tool_result: None,
                    ts,
                    done: true,
                }];
            }
            // String content = a real prompt.
            let Some(raw) = message.get("content").and_then(Value::as_str) else {
                return Vec::new();
            };
            let text = raw.trim();
            if text.is_empty() || SKIP_USER_PREFIXES.iter().any(|p| text.starts_with(p)) {
                return Vec::new();
            }
            let Some(key) = v.get("uuid").and_then(Value::as_str) else {
                return Vec::new();
            };
            vec![ChatMsg {
                key: key.to_string(),
                role: Role::User,
                text: text.to_string(),
                thinking: None,
                tool_calls: Vec::new(),
                tool_result: None,
                ts,
                done: true,
            }]
        }
        "assistant" => {
            if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
                return Vec::new();
            }
            let Some(key) = message
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| v.get("uuid").and_then(Value::as_str).map(str::to_string))
            else {
                return Vec::new();
            };
            let mut text = String::new();
            let mut thinking = None;
            let mut tool_calls = Vec::new();
            if let Some(blocks) = message.get("content").and_then(Value::as_array) {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = block.get("text").and_then(Value::as_str) {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(t);
                            }
                        }
                        Some("thinking") => {
                            if let Some(t) = block.get("thinking").and_then(Value::as_str) {
                                thinking = Some(t.to_string());
                            }
                        }
                        Some("tool_use") => {
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string();
                            let id = block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            tool_calls.push(ToolCall {
                                id,
                                label: summarize_tool_label(block.get("input"), &name),
                                detail: summarize_tool_input(block.get("input")),
                                name,
                            });
                        }
                        _ => {}
                    }
                }
            }
            if text.is_empty() && thinking.is_none() && tool_calls.is_empty() {
                return Vec::new();
            }
            let done = message.get("stop_reason").is_some();
            vec![ChatMsg {
                key,
                role: Role::Assistant,
                text,
                thinking,
                tool_calls,
                tool_result: None,
                ts,
                done,
            }]
        }
        _ => Vec::new(), // system / mode / api-request / attachment / ... — ignored
    }
}

// ---------- Conversations ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub folder: String,
    pub session_id: String,
    pub terminal_id: Option<u64>,
    pub title: String,
    pub created_at: String,
    /// True for sessions discovered from Claude Code's own records rather than
    /// created in YiTerm. They are virtual until first opened (then persisted
    /// with this flag set) and have no delete action in the UI.
    #[serde(default)]
    pub imported: bool,
}

pub fn load_conversations(config_dir: &std::path::Path) -> HashMap<String, Conversation> {
    let path = config_dir.join("conversations.json");
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<Vec<Conversation>>(&bytes)
            .unwrap_or_else(|e| {
                tracing::warn!(?path, error = %e, "corrupt conversations.json");
                Vec::new()
            })
            .into_iter()
            .map(|c| (c.id.clone(), c))
            .collect(),
        Err(_) => HashMap::new(),
    }
}

pub fn save_conversations(config_dir: &std::path::Path, convs: &HashMap<String, Conversation>) {
    let path = config_dir.join("conversations.json");
    let list: Vec<&Conversation> = convs.values().collect();
    let mut sorted = list;
    sorted.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if let Ok(bytes) = serde_json::to_vec_pretty(&sorted) {
        let tmp = config_dir.join("conversations.json.tmp");
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn munge_rules() {
        assert_eq!(
            munge_dir("/Users/admin/code/claude-g"),
            "-Users-admin-code-claude-g"
        );
        assert_eq!(munge_dir("/private/tmp"), "-private-tmp");
        assert_eq!(munge_dir("/Users/中文/项目"), "-Users------");
    }

    #[test]
    fn parses_real_user_line() {
        let v = json!({
            "type": "user",
            "uuid": "u1",
            "timestamp": "2026-11-04T00:00:00Z",
            "message": { "role": "user", "content": "你好，请看一下这个目录" }
        });
        let msgs = parse_line(&v);
        assert_eq!(msgs.len(), 1);
        let m = &msgs[0];
        assert_eq!(m.role, Role::User);
        assert_eq!(m.key, "u1");
        assert_eq!(m.text, "你好，请看一下这个目录");
        assert!(m.done);
    }

    #[test]
    fn parses_image_prompt_array_content() {
        // A prompt with a pasted screenshot: text + image blocks, not a string.
        let v = json!({
            "type": "user", "uuid": "u-img",
            "timestamp": "2026-11-07T20:22:13Z",
            "promptSource": "typed",
            "imagePasteIds": [1],
            "message": { "role": "user", "content": [
                { "type": "text", "text": "[Image #1] Take a look" },
                { "type": "image", "source": { "type": "base64",
                  "media_type": "image/png", "data": "AAAA" } }
            ] }
        });
        let msgs = parse_line(&v);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[0].key, "u-img");
        assert_eq!(msgs[0].text, "[Image #1] Take a look");
        assert!(msgs[0].tool_result.is_none());
    }

    #[test]
    fn image_only_prompt_still_surfaces() {
        let v = json!({
            "type": "user", "uuid": "u-img2",
            "message": { "role": "user", "content": [
                { "type": "image", "source": { "type": "base64",
                  "media_type": "image/png", "data": "AAAA" } }
            ] }
        });
        let msgs = parse_line(&v);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "[Image #1]");
    }

    #[test]
    fn skips_model_facing_image_echo() {
        // The same turn is written again in model-facing form, with no image
        // block. Parsing it too would duplicate the user bubble.
        let v = json!({
            "type": "user", "uuid": "u-echo",
            "message": { "role": "user", "content": [
                { "type": "text", "text": "[Image: source: /tmp/a.png]" }
            ] }
        });
        assert!(parse_line(&v).is_empty());
    }

    #[test]
    fn parses_tool_results() {
        let v = json!({
            "type": "user", "uuid": "u2",
            "message": { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "call_1", "content": "file listing\n…" },
                { "type": "tool_result", "tool_use_id": "call_2", "is_error": true,
                  "content": [ { "type": "text", "text": "boom" } ] }
            ] }
        });
        let msgs = parse_line(&v);
        assert_eq!(msgs.len(), 2);
        let tr0 = msgs[0].tool_result.as_ref().unwrap();
        assert_eq!(tr0.call_id, "call_1");
        assert!(!tr0.is_error);
        assert!(tr0.text.contains("file listing"));
        let tr1 = msgs[1].tool_result.as_ref().unwrap();
        assert!(tr1.is_error);
        assert_eq!(tr1.text, "boom");
        assert_eq!(msgs[1].key, "tr-call_2");
    }

    #[test]
    fn skips_meta_and_commands() {
        let meta = json!({
            "type": "user", "uuid": "u3", "isMeta": true,
            "message": { "role": "user", "content": "meta text" }
        });
        assert!(parse_line(&meta).is_empty());

        let command = json!({
            "type": "user", "uuid": "u4",
            "message": { "role": "user", "content": "<command-name>/clear</command-name>" }
        });
        assert!(parse_line(&command).is_empty());
    }

    #[test]
    fn assistant_snapshot_upsert_by_message_id() {
        let first = json!({
            "type": "assistant", "uuid": "a1",
            "message": { "id": "msg_1", "content": [ { "type": "text", "text": "partial" } ] }
        });
        let msgs = parse_line(&first);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].key, "msg_1");
        assert!(!msgs[0].done);

        let final_line = json!({
            "type": "assistant", "uuid": "a1",
            "message": {
                "id": "msg_1",
                "stop_reason": "end_turn",
                "content": [
                    { "type": "thinking", "thinking": "hmm" },
                    { "type": "text", "text": "final text" },
                    { "type": "tool_use", "id": "call_9", "name": "Bash",
                      "input": { "command": "ls -la && echo hi" } }
                ]
            }
        });
        let msgs = parse_line(&final_line);
        assert_eq!(msgs.len(), 1);
        let m = &msgs[0];
        assert_eq!(m.key, "msg_1");
        assert_eq!(m.text, "final text");
        assert_eq!(m.thinking.as_deref(), Some("hmm"));
        assert_eq!(m.tool_calls.len(), 1);
        assert_eq!(m.tool_calls[0].id, "call_9");
        assert_eq!(m.tool_calls[0].name, "Bash");
        assert_eq!(m.tool_calls[0].label, "ls -la && echo hi");
        assert_eq!(m.tool_calls[0].detail, "ls -la && echo hi");
        assert!(m.done);
    }

    #[test]
    fn tool_detail_from_various_inputs() {
        assert_eq!(
            summarize_tool_input(Some(&json!({ "file_path": "/tmp/a.rs" }))),
            "/tmp/a.rs"
        );
        assert!(summarize_tool_input(Some(&json!({ "weird_key": 42 }))).contains("weird_key"));
        assert_eq!(summarize_tool_input(None), "");
    }

    #[test]
    fn tool_label_prefers_description() {
        let v = json!({
            "type": "assistant", "uuid": "a2",
            "message": {
                "id": "msg_2",
                "content": [{
                    "type": "tool_use", "id": "call_x", "name": "Bash",
                    "input": {
                        "command": "git status --short",
                        "description": "检查仓库状态"
                    }
                }]
            }
        });
        let msgs = parse_line(&v);
        let t = &msgs[0].tool_calls[0];
        assert_eq!(t.label, "检查仓库状态");
        assert_eq!(t.detail, "git status --short");
        assert_eq!(summarize_tool_label(None, "Bash"), "Bash");
    }

    #[test]
    fn ignores_non_conversational_types() {
        for t in ["system", "mode", "permission-mode", "api-request", "file-history-snapshot"] {
            let v = json!({ "type": t, "message": { "role": "user", "content": "x" } });
            assert!(parse_line(&v).is_empty(), "{t} should be ignored");
        }
    }

    #[test]
    fn old_conversations_json_defaults_to_not_imported() {
        // Existing conversations.json predates the `imported` field.
        let v = json!({
            "id": "app-id",
            "folder": "/p",
            "sessionId": "session-id",
            "terminalId": null,
            "title": "t",
            "createdAt": "1"
        });
        let conv: Conversation = serde_json::from_value(v).unwrap();
        assert!(!conv.imported);
    }
}
