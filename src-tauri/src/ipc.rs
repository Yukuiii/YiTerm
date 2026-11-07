use std::sync::Arc;

use serde::Serialize;
use tauri::{ipc::Channel, State};

use crate::{
    sessions::{self, follower, Conversation},
    state::AppState,
    terminal::TerminalHandle,
    translate::{self, Direction, TranslateEvent},
};

// ---------- terminal ----------

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalStartRequest {
    pub conversation_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalStartInfo {
    pub conversation_id: String,
    pub terminal_id: u64,
    pub session_id: String,
    pub command: String,
    pub resumed: bool,
}

#[tauri::command]
pub async fn terminal_start(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: TerminalStartRequest,
) -> Result<TerminalStartInfo, String> {
    // Existing live terminal bound to this conversation? Return it.
    // Scanning `terms` (not just conv.terminal_id) also self-heals stale
    // links and guards against concurrent duplicate starts.
    {
        let terms = state.terms.read().unwrap();
        let alive = terms.values().find(|t| {
            t.conv_id == req.conversation_id && !*t.exited.lock().unwrap()
        });
        if let Some(t) = alive {
            return Ok(TerminalStartInfo {
                conversation_id: t.conv_id.clone(),
                terminal_id: t.id,
                session_id: t.session_id.clone(),
                command: t.command.clone(),
                resumed: true,
            });
        }
    }

    let conv = {
        state
            .convs
            .read()
            .unwrap()
            .get(&req.conversation_id)
            .cloned()
            .ok_or("conversation not found")?
    };
    let folder = std::fs::canonicalize(&conv.folder)
        .map_err(|e| format!("folder {:?}: {e}", conv.folder))?
        .to_string_lossy()
        .into_owned();

    let claude_path = state.settings.read().unwrap().resolve_claude_path()?;
    let session_id = conv.session_id.clone();

    // Resume if the session file already exists, otherwise seed a new session
    // id path. The JSONL is created only after the first message, so a
    // never-used session still takes the `--session-id` branch idempotently.
    let home = dirs::home_dir().ok_or("no home dir")?;
    let jsonl = sessions::jsonl_path(&home, &folder, &session_id);
    let resumed = jsonl.is_file();

    let id = state.new_terminal_id();
    let handle = TerminalHandle::spawn(
        id,
        conv.id.clone(),
        folder.clone(),
        session_id.clone(),
        &claude_path,
        resumed,
        100,
        30,
    )?;
    let command = handle.command.clone();
    state.terms.write().unwrap().insert(id, Arc::clone(&handle));
    {
        let mut convs = state.convs.write().unwrap();
        if let Some(c) = convs.get_mut(&conv.id) {
            c.folder = folder.clone();
            c.terminal_id = Some(id);
        }
    }
    state.persist_conversations(&app);

    // Replace any previous follower for this conversation.
    {
        let mut followers = state.followers.lock().unwrap();
        if let Some(old) = followers.remove(&conv.id) {
            old.abort();
        }
        followers.insert(
            conv.id.clone(),
            follower::spawn_follower(app.clone(), conv.id.clone(), folder, session_id.clone()),
        );
    }

    Ok(TerminalStartInfo {
        conversation_id: conv.id,
        terminal_id: id,
        session_id,
        command,
        resumed,
    })
}

#[tauri::command]
pub fn terminal_write(
    state: State<'_, AppState>,
    id: u64,
    text: String,
    submit: bool,
) -> Result<(), String> {
    with_terminal(&state, id, |t| t.write(&text, submit))
}

/// One composer turn: text plus any pasted image paths.
///
/// Async because an image paste has to settle inside Claude Code before Enter
/// goes out — see `TerminalHandle::send_prompt`. The terminal handle is cloned
/// out of the lock so no std guard is held across an await.
#[tauri::command]
pub async fn terminal_send_prompt(
    state: State<'_, AppState>,
    id: u64,
    text: String,
    image_paths: Vec<String>,
) -> Result<(), String> {
    let handle = state
        .terms
        .read()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or("terminal not found")?;
    handle.send_prompt(&text, &image_paths).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachAck {
    pub terminal_id: u64,
}

#[tauri::command]
pub fn terminal_attach(
    state: State<'_, AppState>,
    id: u64,
    seq: u64,
    channel: Channel<crate::terminal::TerminalEvent>,
) -> Result<AttachAck, String> {
    with_terminal(&state, id, |t| {
        t.attach(seq, channel);
        Ok(())
    })?;
    Ok(AttachAck { terminal_id: id })
}

#[tauri::command]
pub fn terminal_resize(
    state: State<'_, AppState>,
    id: u64,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    with_terminal(&state, id, |t| t.resize(cols, rows))
}

#[tauri::command]
pub fn terminal_kill(state: State<'_, AppState>, id: u64) -> Result<(), String> {
    with_terminal(&state, id, |t| {
        t.kill();
        Ok(())
    })
}

fn with_terminal(
    state: &AppState,
    id: u64,
    f: impl FnOnce(&TerminalHandle) -> Result<(), String>,
) -> Result<(), String> {
    let handle = state
        .terms
        .read()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or("terminal not found")?;
    f(&handle)
}

// ---------- conversations ----------

#[tauri::command]
pub fn conversations_list(state: State<'_, AppState>) -> Vec<Conversation> {
    let mut list: Vec<Conversation> = state.convs.read().unwrap().values().cloned().collect();
    list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    list
}

#[tauri::command]
pub fn conversations_create(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    folder: String,
    title: Option<String>,
) -> Result<Conversation, String> {
    let folder = std::fs::canonicalize(&folder)
        .map_err(|e| format!("folder {:?}: {e}", folder))?
        .to_string_lossy()
        .into_owned();
    let conv = Conversation {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: uuid::Uuid::new_v4().to_string(),
        folder: folder.clone(),
        terminal_id: None,
        title: title.unwrap_or_else(|| {
            std::path::Path::new(&folder)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| folder.clone())
        }),
        created_at: now_iso(),
    };
    state.convs.write().unwrap().insert(conv.id.clone(), conv.clone());
    state.persist_conversations(&app);
    Ok(conv)
}

#[tauri::command]
pub fn conversations_delete(app: tauri::AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    let conv = state.convs.write().unwrap().remove(&id);
    if let Some(conv) = &conv {
        if let Some(tid) = conv.terminal_id {
            if let Some(t) = state.terms.read().unwrap().get(&tid) {
                t.kill();
            }
            state.terms.write().unwrap().remove(&tid);
        }
    }
    if let Some(handle) = state.followers.lock().unwrap().remove(&id) {
        handle.abort();
    }
    // Pasted screenshots are conversation-scoped; don't leave them in the cache.
    if valid_conv_id(&id) {
        let images = images_dir(&state, &app, &id);
        if let Err(e) = std::fs::remove_dir_all(&images)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(target: "yiterm", conv = %id, "cleanup images: {e}");
        }
    }
    state.persist_conversations(&app);
    Ok(())
}

/// Frontend diagnostic breadcrumbs → backend log (helps debug IPC/state issues
/// without devtools attached to the webview).
#[tauri::command]
pub fn debug_log(message: String) {
    tracing::info!(target: "yiterm_frontend", "{message}");
}

/// Login name for the sidebar identity row.
#[tauri::command]
pub fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".to_string())
}

// ---------- chat history persistence (opaque JSON owned by the frontend) ----------

/// conv ids are UUIDs we generate; reject anything path-ish defensively.
fn valid_conv_id(conv_id: &str) -> bool {
    !conv_id.is_empty() && conv_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn chat_path(app: &tauri::AppHandle, state: &AppState, conv_id: &str) -> Result<std::path::PathBuf, String> {
    if !valid_conv_id(conv_id) {
        return Err("invalid conversation id".into());
    }
    let dir = state.config_dir(app).join("chats");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(format!("{conv_id}.json")))
}

#[tauri::command]
pub fn chat_save(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    conv_id: String,
    data: String,
) -> Result<(), String> {
    let path = chat_path(&app, &state, &conv_id)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn chat_load(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    conv_id: String,
) -> Result<Option<String>, String> {
    let path = chat_path(&app, &state, &conv_id)?;
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

// ---------- pasted images ----------

/// Decoded size cap. Retina screenshots are a few MB; Claude Code resizes anyway.
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// Claude Code only auto-attaches pasted paths with one of these extensions.
fn image_extension(mime: &str) -> Option<&'static str> {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// Sniff the real content rather than trusting the clipboard's label: WKWebView
/// can hand over a file with an empty `type`, and if an unsupported file ever
/// reached the PTY Claude Code would insert the literal path as text, which the
/// mirror would then show as a stray path bubble.
fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Validates a pasted image and returns the extension to save it under.
/// Split out from the command so it is testable without an `AppHandle`.
fn validate_image(conv_id: &str, mime: &str, bytes: &[u8]) -> Result<&'static str, String> {
    if !valid_conv_id(conv_id) {
        return Err("invalid conversation id".into());
    }
    if bytes.is_empty() {
        return Err("图片为空".into());
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "图片过大（上限 {} MB）",
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    let sniffed = sniff_image_mime(bytes)
        .ok_or_else(|| format!("不支持的图片格式 {mime}（仅支持 PNG/JPEG/GIF/WebP）"))?;
    // Content wins over the label; `mime` only shapes the error message.
    Ok(image_extension(sniffed).expect("sniffed type is always supported"))
}

/// Pasted screenshots live in the app cache dir, scoped to their conversation.
/// History keeps the absolute path so thumbnails survive a restart; the OS may
/// still reclaim the cache, which the UI tolerates as a missing thumbnail.
fn images_dir(state: &AppState, app: &tauri::AppHandle, conv_id: &str) -> std::path::PathBuf {
    state.cache_dir(app).join("images").join(conv_id)
}

/// Persists a pasted image and returns its absolute path, which is what gets
/// written into the PTY (Claude Code reads the file and keeps its own copy).
#[tauri::command]
pub fn chat_image_save(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    conv_id: String,
    mime: String,
    data: String,
) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|e| format!("bad image data: {e}"))?;
    let ext = validate_image(&conv_id, &mime, &bytes)?;
    let dir = images_dir(&state, &app, &conv_id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.{ext}", uuid::Uuid::new_v4()));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

fn now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    // Unix millis — enough for stable newest-first ordering.
    format!("{}", now.as_millis())
}

// ---------- settings ----------

#[tauri::command]
pub fn settings_get(state: State<'_, AppState>) -> crate::settings::Settings {
    state.settings.read().unwrap().clone()
}

#[tauri::command]
pub fn settings_set(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    settings: crate::settings::Settings,
) -> Result<(), String> {
    let dir = state.config_dir(&app);
    settings.save(&dir)?;
    *state.settings.write().unwrap() = settings;
    Ok(())
}

#[tauri::command]
pub fn api_key_get() -> Option<String> {
    crate::settings::api_key_get()
}

#[tauri::command]
pub fn api_key_set(key: Option<String>) -> Result<(), String> {
    crate::settings::api_key_set(key)
}

// ---------- translate ----------

#[tauri::command]
pub async fn translate(
    state: State<'_, AppState>,
    request_id: String,
    direction: Direction,
    text: String,
    channel: Channel<TranslateEvent>,
) -> Result<String, String> {
    let http = state.http.clone();
    let settings = state.settings.read().unwrap().clone();
    translate::run(&http, &settings.translation, direction, &text, &request_id, &channel).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n0000";
    const JPEG: &[u8] = b"\xff\xd8\xff\xe0jfif";
    const GIF: &[u8] = b"GIF89a0000";
    const WEBP: &[u8] = b"RIFF\x00\x00\x00\x00WEBPVP8 ";

    #[test]
    fn extensions_cover_exactly_what_claude_attaches() {
        assert_eq!(image_extension("image/png"), Some("png"));
        assert_eq!(image_extension("IMAGE/JPEG"), Some("jpg"));
        assert_eq!(image_extension("image/jpg"), Some("jpg"));
        assert_eq!(image_extension("image/gif"), Some("gif"));
        assert_eq!(image_extension("image/webp"), Some("webp"));
        assert_eq!(image_extension("image/tiff"), None);
        assert_eq!(image_extension("image/heic"), None);
        assert_eq!(image_extension("text/plain"), None);
        assert_eq!(image_extension(""), None);
    }

    #[test]
    fn sniffing_detects_supported_magic_bytes() {
        assert_eq!(sniff_image_mime(PNG), Some("image/png"));
        assert_eq!(sniff_image_mime(JPEG), Some("image/jpeg"));
        assert_eq!(sniff_image_mime(GIF), Some("image/gif"));
        assert_eq!(sniff_image_mime(WEBP), Some("image/webp"));
        assert_eq!(sniff_image_mime(b"not an image"), None);
        assert_eq!(sniff_image_mime(b"RIFF0000WAVE"), None);
        assert_eq!(sniff_image_mime(b""), None);
    }

    #[test]
    fn validate_uses_content_over_a_mislabelled_type() {
        // Empty `type` is what WKWebView can hand over; sniffing still works.
        assert_eq!(validate_image("abc-123", "", PNG), Ok("png"));
        // A wrong label still saves under the real format.
        assert_eq!(validate_image("abc-123", "image/gif", JPEG), Ok("jpg"));
        assert!(validate_image("abc-123", "image/png", b"nope").is_err());
    }

    #[test]
    fn validate_rejects_pathish_ids_empty_and_oversized() {
        assert!(validate_image("../etc", "image/png", PNG).is_err());
        assert!(validate_image("a/b", "image/png", PNG).is_err());
        assert!(validate_image("", "image/png", PNG).is_err());
        assert!(validate_image("ok", "image/png", b"").is_err());

        let mut big = vec![0u8; MAX_IMAGE_BYTES + 1];
        big[..8].copy_from_slice(&PNG[..8]);
        assert!(validate_image("ok", "image/png", &big).is_err());

        let mut at_cap = vec![0u8; MAX_IMAGE_BYTES];
        at_cap[..8].copy_from_slice(&PNG[..8]);
        assert_eq!(validate_image("ok", "image/png", &at_cap), Ok("png"));
    }
}
