use std::{collections::HashMap, path::PathBuf};

use serde::Serialize;
use tauri::Emitter;

use super::{parse_line, ChatMsg};

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdate {
    pub conversation_id: String,
    pub session_id: String,
    pub upserts: Vec<ChatMsg>,
}

/// Tails the session JSONL for one conversation and emits `session-update`
/// events. Polling (not FSEvents) on purpose: files are local, append-only,
/// may not exist until the first message, and a 300 ms stat loop over a
/// handful of files is negligible.
pub fn spawn_follower(
    app: tauri::AppHandle,
    conv_id: String,
    folder: String,
    session_id: String,
) -> tokio::task::JoinHandle<()> {
    let Some(home) = dirs::home_dir() else {
        tracing::warn!("no home dir, cannot follow sessions");
        return tokio::spawn(async {});
    };
    let path: PathBuf = super::jsonl_path(&home, &folder, &session_id);
    tracing::info!(?path, conv_id = %conv_id, "follower started");
    tokio::spawn(async move {
        let mut last_pos: u64 = 0;
        let mut partial: Vec<u8> = Vec::new();
        // key → last emitted JSON; dedupes streaming snapshot re-emissions.
        let mut emitted: HashMap<String, String> = HashMap::new();

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
                for msg in parse_line(&v) {
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
                    upserts,
                };
                if let Err(e) = app.emit("session-update", &update) {
                    tracing::warn!(error = %e, "emit session-update failed");
                }
            }
        }
    })
}

async fn read_range(path: &std::path::Path, start: u64) -> Result<Vec<u8>, std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut f = tokio::fs::File::open(path).await?;
    f.seek(std::io::SeekFrom::Start(start)).await?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).await?;
    Ok(buf)
}
