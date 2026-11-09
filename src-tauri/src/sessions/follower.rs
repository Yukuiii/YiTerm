use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::Serialize;
use tauri::Emitter;

use super::{
    is_interrupt_marker, is_synthetic_no_response, parse_line, ChatMsg, Role, SKIP_USER_PREFIXES,
};

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

/// Emitted whenever a conversation's turn starts or finishes, so the sidebar
/// can show a running indicator.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationBusy {
    pub conversation_id: String,
    pub busy: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdate {
    pub conversation_id: String,
    pub session_id: String,
    /// The conversation came from Claude's records, not YiTerm.
    pub imported: bool,
    /// This batch is the transcript that already existed when the follower
    /// attached, not something written just now.
    pub historical: bool,
    pub upserts: Vec<ChatMsg>,
}

/// Tails the session JSONL for one conversation and emits `session-update`
/// events. Polling (not FSEvents) on purpose: files are local, append-only,
/// may not exist until the first message, and a 300 ms stat loop over a
/// handful of files is negligible.
///
/// The caller resolves `path` (see `claude::resolve_jsonl`): imported sessions
/// may live under a project directory whose name does not match the
/// canonicalized workspace folder.
pub fn spawn_follower(
    app: tauri::AppHandle,
    conv_id: String,
    path: PathBuf,
    session_id: String,
    imported: bool,
    busy: Arc<Mutex<HashMap<String, bool>>>,
    assistant_since_prompt: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tracing::info!(?path, conv_id = %conv_id, "follower started");
    tokio::spawn(async move {
        let mut last_pos: u64 = 0;
        let mut partial: Vec<u8> = Vec::new();
        // The first batch read from an existing file is history, not live
        // activity; imported conversations use this to skip translation.
        let mut historical = true;
        // key → last emitted JSON; dedupes streaming snapshot re-emissions.
        let mut emitted: HashMap<String, String> = HashMap::new();
        // Busy events are suppressed while replaying history; only the final
        // state of the first batch is emitted, so resuming an idle session
        // doesn't flash a running spinner through its old turns.
        let mut live = false;
        // Whether the last turn-changing line was a user prompt. A transcript
        // that ends on one has no assistant reply, usually because the request
        // was aborted before the CLI could write an interrupt marker.
        let mut last_turn_was_user = false;
        // The last user prompt, kept so an interrupt that arrives before any
        // assistant reply can mark that bubble as interrupted.
        let mut last_user: Option<ChatMsg> = None;
        let mut marked_interrupted = false;

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            let Ok(meta) = tokio::fs::metadata(&path).await else {
                continue; // normal until the first message
            };
            let len = meta.len();
            if len < last_pos {
                // Truncated/replaced → resnapshot everything.
                tracing::info!(?path, "session file shrank, resnapshotting");
                last_pos = 0;
                partial.clear();
                emitted.clear();
                historical = true;
                last_turn_was_user = false;
                last_user = None;
                assistant_since_prompt.store(false, Ordering::Relaxed);
                marked_interrupted = false;
            }
            if len <= last_pos && partial.is_empty() {
                continue;
            }

            let read = match read_range(&path, last_pos).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::warn!(?path, error = %e, "session read failed");
                    continue;
                }
            };
            last_pos += read.len() as u64;
            if read.is_empty() {
                continue;
            }
            partial.extend_from_slice(&read);

            let mut upserts: Vec<ChatMsg> = Vec::new();
            while let Some(pos) = partial.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = partial.drain(..=pos).collect();
                let line = &line[..line.len() - 1]; // strip \n
                if line.is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
                    tracing::debug!(?path, "unparseable jsonl line skipped");
                    continue;
                };
                let state = turn_state(&v);
                let parsed = parse_line(&v);
                let is_user_prompt = state == Some(true)
                    && v.get("type").and_then(serde_json::Value::as_str) == Some("user");
                if is_user_prompt {
                    last_user = parsed.iter().find(|m| m.role == Role::User).cloned();
                    marked_interrupted = false;
                }
                record_assistant_progress(&v, &assistant_since_prompt);

                // An interrupt before any assistant line leaves no reply: mark
                // the prompt so the chat can say "已中断" instead of showing a
                // silently dangling user bubble.
                if !marked_interrupted
                    && !assistant_since_prompt.load(Ordering::Relaxed)
                    && (has_interrupt_marker(&v)
                        || v.get("message").is_some_and(is_synthetic_no_response))
                {
                    if let Some(mut msg) = last_user.clone() {
                        msg.interrupted = true;
                        let fingerprint = serde_json::to_string(&msg).unwrap_or_default();
                        if emitted.get(&msg.key) != Some(&fingerprint) {
                            emitted.insert(msg.key.clone(), fingerprint);
                            upserts.push(msg);
                        }
                        marked_interrupted = true;
                    }
                }

                if let Some(running) = state {
                    last_turn_was_user = running
                        && v.get("type").and_then(serde_json::Value::as_str) == Some("user");
                    let changed = {
                        let mut map = busy.lock().unwrap();
                        let prev = map.insert(conv_id.clone(), running);
                        prev != Some(running)
                    };
                    if changed && live {
                        let _ = app.emit(
                            "conversation-busy",
                            ConversationBusy {
                                conversation_id: conv_id.clone(),
                                busy: running,
                            },
                        );
                    }
                }
                for msg in parsed {
                    let fingerprint = serde_json::to_string(&msg).unwrap_or_default();
                    match emitted.get(&msg.key) {
                        Some(prev) if *prev == fingerprint => {}
                        _ => {
                            emitted.insert(msg.key.clone(), fingerprint);
                            upserts.push(msg);
                        }
                    }
                }
            }

            if !live {
                // A replayed turn that ends on a user prompt with no assistant
                // reply was most likely aborted before the CLI could write an
                // interrupt marker; don't resurrect a running spinner.
                let (had_state, running) = {
                    let mut map = busy.lock().unwrap();
                    let had_state = map.contains_key(&conv_id);
                    let running = replay_busy(
                        map.get(&conv_id).copied().unwrap_or(false),
                        last_turn_was_user,
                    );
                    if had_state {
                        map.insert(conv_id.clone(), running);
                    }
                    (had_state, running)
                };
                if had_state {
                    let _ = app.emit(
                        "conversation-busy",
                        ConversationBusy {
                            conversation_id: conv_id.clone(),
                            busy: running,
                        },
                    );
                }
                live = true;
            }

            if !upserts.is_empty() {
                tracing::info!(
                    conv = %conv_id,
                    n = upserts.len(),
                    keys = ?upserts.iter().map(|m| &m.key).collect::<Vec<_>>(),
                    lenses = ?upserts.iter().map(|m| m.text.chars().count()).collect::<Vec<_>>(),
                    "emit session-update"
                );
                let update = SessionUpdate {
                    conversation_id: conv_id.clone(),
                    session_id: session_id.clone(),
                    imported,
                    historical,
                    upserts,
                };
                if let Err(e) = app.emit("session-update", &update) {
                    tracing::warn!(error = %e, "emit session-update failed");
                }
                historical = false;
            }
        }
    })
}

/// Text fragments of a user message: a plain string counts as one fragment,
/// an array contributes the `text` of its content blocks, and anything else
/// contributes none. Claude Code writes interrupt markers both ways.
fn message_texts(content: &serde_json::Value) -> Vec<&str> {
    match content {
        serde_json::Value::String(s) => vec![s.as_str()],
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(serde_json::Value::as_str))
            .collect(),
        _ => Vec::new(),
    }
}

/// True when the line is Claude Code's interrupt marker for the turn.
fn has_interrupt_marker(v: &serde_json::Value) -> bool {
    v.get("type").and_then(serde_json::Value::as_str) == Some("user")
        && v.pointer("/message/content")
            .is_some_and(|content| message_texts(content).iter().any(|t| is_interrupt_marker(t)))
}

/// Track the first real assistant record in each main-session turn. Tool
/// results, local commands and synthetic replies must not reset this progress.
fn record_assistant_progress(v: &serde_json::Value, assistant_since_prompt: &AtomicBool) {
    match v.get("type").and_then(serde_json::Value::as_str) {
        Some("user") if turn_state(v) == Some(true) => {
            assistant_since_prompt.store(false, Ordering::Relaxed);
        }
        Some("assistant")
            if v.get("isSidechain").and_then(serde_json::Value::as_bool) != Some(true)
                && !v.get("message").is_some_and(is_synthetic_no_response) =>
        {
            assistant_since_prompt.store(true, Ordering::Relaxed);
        }
        _ => {}
    }
}

/// Busy state to report after replaying history. A turn that ends on a user
/// prompt has no assistant reply: it was most likely aborted before the CLI
/// could write an interrupt marker, so do not resurrect a running spinner.
fn replay_busy(running: bool, last_turn_was_user: bool) -> bool {
    running && !last_turn_was_user
}

/// Whether one JSONL line says a turn is running. `None` = the line carries
/// no turn state (tool results, meta/system noise).
///
/// The transcript writes a user line when a turn starts, streaming assistant
/// snapshots without a `stop_reason`, a `tool_use` snapshot before each tool,
/// and finally an assistant line whose stop reason is `end_turn` (or another
/// terminal reason).
fn turn_state(v: &serde_json::Value) -> Option<bool> {
    match v.get("type").and_then(serde_json::Value::as_str)? {
        "assistant" => {
            let stop = v
                .pointer("/message/stop_reason")
                .and_then(serde_json::Value::as_str);
            Some(match stop {
                // Still streaming, or waiting on a tool: the turn continues.
                None | Some("tool_use") => true,
                Some(_) => false,
            })
        }
        "user" => {
            if v.get("isMeta").and_then(serde_json::Value::as_bool) == Some(true)
                || v.get("isSidechain").and_then(serde_json::Value::as_bool) == Some(true)
            {
                return None;
            }
            let content = v.pointer("/message/content")?;
            for text in message_texts(content) {
                let text = text.trim_start();
                // Pressing Esc writes this marker instead of an assistant
                // `end_turn`, so it must close the running turn.
                if is_interrupt_marker(text) {
                    return Some(false);
                }
                // Slash-command machinery (/list-agents, /model, …) is a
                // synthetic user line with no assistant turn behind it.
                if SKIP_USER_PREFIXES.iter().any(|p| text.starts_with(p)) {
                    return None;
                }
            }
            let is_prompt = match content {
                serde_json::Value::String(_) => true,
                // Array prompts carry image blocks; arrays of tool_result do
                // not start a turn.
                serde_json::Value::Array(blocks) => blocks.iter().any(|b| {
                    b.get("type").and_then(serde_json::Value::as_str) == Some("image")
                }),
                _ => false,
            };
            is_prompt.then_some(true)
        }
        _ => None,
    }
}

async fn read_range(path: &std::path::Path, start: u64) -> Result<Vec<u8>, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut f = tokio::fs::File::open(path).await?;
    f.seek(std::io::SeekFrom::Start(start)).await?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn turn_state_tracks_transcript_markers() {
        // A plain user prompt starts a turn.
        assert_eq!(
            turn_state(&json!({"type":"user","message":{"content":"hello"}})),
            Some(true)
        );
        // Tool results and meta lines say nothing about the turn.
        assert_eq!(
            turn_state(&json!({"type":"user","message":{"content":[{"type":"tool_result"}]}})),
            None
        );
        assert_eq!(
            turn_state(&json!({"type":"user","isMeta":true,"message":{"content":"x"}})),
            None
        );
        // Streaming snapshots and tool calls keep it running.
        assert_eq!(
            turn_state(&json!({"type":"assistant","message":{"stop_reason":null}})),
            Some(true)
        );
        assert_eq!(
            turn_state(&json!({"type":"assistant","message":{"stop_reason":"tool_use"}})),
            Some(true)
        );
        // A terminal stop reason ends it.
        assert_eq!(
            turn_state(&json!({"type":"assistant","message":{"stop_reason":"end_turn"}})),
            Some(false)
        );
        // Unrelated line types are ignored.
        assert_eq!(turn_state(&json!({"type":"system"})), None);
        // Interrupting a turn closes it without an assistant end_turn; the
        // marker can arrive as a string or as an array of text blocks.
        assert_eq!(
            turn_state(&json!({
                "type":"user",
                "message":{"content":"[Request interrupted by user]"}
            })),
            Some(false)
        );
        assert_eq!(
            turn_state(&json!({
                "type":"user",
                "message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}
            })),
            Some(false)
        );
        // Slash commands are local machinery, not Claude turns.
        assert_eq!(
            turn_state(&json!({
                "type":"user",
                "message":{"content":"<command-name>/list-agents</command-name>"}
            })),
            None
        );
        assert_eq!(
            turn_state(&json!({
                "type":"user",
                "message":{"content":"<local-command-stdout>ok</local-command-stdout>"}
            })),
            None
        );
    }

    #[test]
    fn assistant_progress_survives_tools_and_resets_for_new_prompts() {
        let progress = AtomicBool::new(false);
        let prompt = json!({"type":"user","message":{"content":"start a task"}});
        let tool_result = json!({
            "type":"user",
            "message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","content":"ok"}]}
        });
        let interrupt = json!({
            "type":"user",
            "message":{"content":"[Request interrupted by user]"}
        });
        for content in [
            json!([{"type":"text","text":"Working on it"}]),
            json!([{"type":"thinking","thinking":"Consider the next step"}]),
            json!([{"type":"tool_use","id":"tool-1","name":"Read","input":{}}]),
        ] {
            record_assistant_progress(&prompt, &progress);
            assert!(!progress.load(Ordering::Relaxed));

            record_assistant_progress(
                &json!({"type":"assistant","message":{"content":content,"stop_reason":null}}),
                &progress,
            );
            assert!(progress.load(Ordering::Relaxed));

            // Tool results and interruption markers belong to the same turn;
            // neither makes an already-started task an unanswered prompt again.
            record_assistant_progress(&tool_result, &progress);
            record_assistant_progress(&interrupt, &progress);
            assert!(progress.load(Ordering::Relaxed));
        }

        // Image prompts start a fresh turn too, including after history replay.
        record_assistant_progress(
            &json!({"type":"user","message":{"content":[{"type":"image","source":{}}]}}),
            &progress,
        );
        assert!(!progress.load(Ordering::Relaxed));
    }

    #[test]
    fn internal_records_do_not_change_assistant_progress() {
        let ignored = [
            json!({"type":"system"}),
            json!({"type":"user","isMeta":true,"message":{"content":"context"}}),
            json!({"type":"user","isSidechain":true,"message":{"content":"subtask"}}),
            json!({"type":"assistant","isSidechain":true,"message":{"content":"subtask reply"}}),
            json!({"type":"user","message":{"content":"<command-name>/model</command-name>"}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result"}]}}),
            json!({"type":"user","message":{"content":"[Request interrupted by user]"}}),
            json!({
                "type":"assistant",
                "message":{
                    "model":"<synthetic>",
                    "content":[{"type":"text","text":"No response requested."}],
                    "stop_reason":"end_turn"
                }
            }),
        ];
        for initial in [false, true] {
            let progress = AtomicBool::new(initial);
            for record in &ignored {
                record_assistant_progress(record, &progress);
                assert_eq!(progress.load(Ordering::Relaxed), initial, "{record}");
            }
        }
    }

    #[test]
    fn replay_does_not_revive_a_dangling_prompt() {
        // User prompt, no assistant reply → aborted before the interrupt marker.
        assert!(!replay_busy(true, true));
        // Assistant streaming or waiting on a tool → genuinely running.
        assert!(replay_busy(true, false));
        assert!(!replay_busy(false, false));
    }
}
