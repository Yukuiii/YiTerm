use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// A chat message extracted from a session JSONL line. `key` is stable:
/// user lines use the line `uuid`, assistant lines use `message.id` because
/// claude writes 1–3 snapshot lines per message id while streaming.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatMsg {
    pub key: String,
    pub role: Role,
    pub text: String,
    pub thinking: Option<String>,
    pub tools: Vec<String>,
    pub ts: Option<String>,
    pub done: bool,
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

/// Tolerant extraction from one JSONL line. Returns `None` for anything that
/// is not a displayable chat message (tool_result arrays, meta lines, the 12+
/// non-conversational line types claude writes).
pub fn parse_line(v: &Value) -> Option<ChatMsg> {
    let line_type = v.get("type")?.as_str()?;
    let message = v.get("message")?;
    let ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());

    match line_type {
        "user" => {
            if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            // String content = a real prompt; array content = tool_result.
            let text = message.get("content").and_then(Value::as_str)?.trim();
            if text.is_empty() {
                return None;
            }
            if SKIP_USER_PREFIXES.iter().any(|p| text.starts_with(p)) {
                return None;
            }
            let key = v.get("uuid").and_then(Value::as_str)?.to_string();
            Some(ChatMsg {
                key,
                role: Role::User,
                text: text.to_string(),
                thinking: None,
                tools: Vec::new(),
                ts,
                done: true,
            })
        }
        "assistant" => {
            if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            let key = message
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| v.get("uuid").and_then(Value::as_str).map(str::to_string))?;
            let mut text = String::new();
            let mut thinking = None;
            let mut tools = Vec::new();
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
                            if let Some(name) = block.get("name").and_then(Value::as_str) {
                                tools.push(name.to_string());
                            }
                        }
                        _ => {}
                    }
                }
            }
            if text.is_empty() && thinking.is_none() && tools.is_empty() {
                return None;
            }
            let done = message.get("stop_reason").is_some();
            Some(ChatMsg {
                key,
                role: Role::Assistant,
                text,
                thinking,
                tools,
                ts,
                done,
            })
        }
        _ => None, // system / mode / api-request / attachment / ... — ignored
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
        let m = parse_line(&v).unwrap();
        assert_eq!(m.role, Role::User);
        assert_eq!(m.key, "u1");
        assert_eq!(m.text, "你好，请看一下这个目录");
        assert!(m.done);
    }

    #[test]
    fn skips_tool_result_and_meta_and_commands() {
        let tool_result = json!({
            "type": "user", "uuid": "u2",
            "message": { "role": "user", "content": [ { "type": "tool_result", "content": "ok" } ] }
        });
        assert!(parse_line(&tool_result).is_none());

        let meta = json!({
            "type": "user", "uuid": "u3", "isMeta": true,
            "message": { "role": "user", "content": "meta text" }
        });
        assert!(parse_line(&meta).is_none());

        let command = json!({
            "type": "user", "uuid": "u4",
            "message": { "role": "user", "content": "<command-name>/clear</command-name>" }
        });
        assert!(parse_line(&command).is_none());
    }

    #[test]
    fn assistant_snapshot_upsert_by_message_id() {
        let first = json!({
            "type": "assistant", "uuid": "a1",
            "message": { "id": "msg_1", "content": [ { "type": "text", "text": "partial" } ] }
        });
        let m = parse_line(&first).unwrap();
        assert_eq!(m.key, "msg_1");
        assert!(!m.done);

        let final_line = json!({
            "type": "assistant", "uuid": "a1",
            "message": {
                "id": "msg_1",
                "stop_reason": "end_turn",
                "content": [
                    { "type": "thinking", "thinking": "hmm" },
                    { "type": "text", "text": "final text" },
                    { "type": "tool_use", "name": "Bash" }
                ]
            }
        });
        let m = parse_line(&final_line).unwrap();
        assert_eq!(m.key, "msg_1");
        assert_eq!(m.text, "final text");
        assert_eq!(m.thinking.as_deref(), Some("hmm"));
        assert_eq!(m.tools, vec!["Bash"]);
        assert!(m.done);
    }

    #[test]
    fn ignores_non_conversational_types() {
        for t in ["system", "mode", "permission-mode", "api-request", "file-history-snapshot"] {
            let v = json!({ "type": t, "message": { "role": "user", "content": "x" } });
            assert!(parse_line(&v).is_none(), "{t} should be ignored");
        }
    }
}
