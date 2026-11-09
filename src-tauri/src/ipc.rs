use std::sync::{Arc, atomic::Ordering};

use serde::Serialize;
use tauri::{Emitter, State, ipc::Channel};

use crate::{
    sessions::{self, Conversation, follower},
    state::AppState,
    terminal::{
        TerminalHandle,
        plan_approval::{PlanApprovalStatus, PlanExecutionMode, TranscriptBoundary},
    },
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
    /// Conversations whose terminals were reclaimed to stay under the cap.
    /// The frontend drops their entries so the next visit starts `--resume`.
    pub evicted_conversation_ids: Vec<String>,
}

/// Keep at most this many live PTY/claude processes. When a new conversation
/// opens, the least-recently-used idle terminals are reclaimed; busy
/// conversations are never reclaimed, so the cap is soft while work runs.
const MAX_LIVE_TERMINALS: usize = 4;
/// A terminal that printed within this window counts as running even before
/// the transcript records a turn (startup banner, prompt echo, TUI spinner).
const OUTPUT_GRACE_MS: i64 = 5_000;

/// Kills the least-recently-used idle terminals until `terms` is below the
/// cap and returns the evicted conversation ids. A terminal is idle when no
/// turn is running in its transcript and its PTY has been quiet for a while.
fn reclaim_idle_terminals(
    app: &tauri::AppHandle,
    state: &AppState,
    keep_conv: &str,
) -> Vec<String> {
    let mut evicted = Vec::new();
    loop {
        let now = unix_millis();
        let busy = state.busy.lock().unwrap().clone();
        let snapshot: Vec<TerminalSnapshot> = state
            .terms
            .read()
            .unwrap()
            .iter()
            .map(|(id, t)| TerminalSnapshot {
                id: *id,
                conv_id: t.conv_id.clone(),
                last_output_ms: t.last_output_ms(),
                exited: t.is_exited(),
            })
            .collect();
        if snapshot.len() < MAX_LIVE_TERMINALS {
            break;
        }
        let Some((id, conv)) = pick_eviction(&snapshot, &busy, keep_conv, now) else {
            break; // everything left is busy: allow a temporary overshoot
        };
        {
            let mut terms = state.terms.write().unwrap();
            if let Some(handle) = terms.get(&id) {
                handle.retire();
            }
            if let Some(handle) = terms.remove(&id) {
                handle.kill();
            }
        }
        if let Some(c) = state.convs.write().unwrap().get_mut(&conv) {
            c.terminal_id = None;
        }
        if let Some(f) = state.followers.lock().unwrap().remove(&conv) {
            f.abort();
        }
        state.busy.lock().unwrap().remove(&conv);
        let _ = app.emit(
            "conversation-busy",
            follower::ConversationBusy {
                conversation_id: conv.clone(),
                busy: false,
            },
        );
        state.persist_conversations(app);
        tracing::info!(terminal = id, conv = %conv, "reclaimed idle terminal");
        evicted.push(conv);
    }
    evicted
}

/// What the reclaimer needs to know about one live terminal.
struct TerminalSnapshot {
    id: u64,
    conv_id: String,
    last_output_ms: i64,
    exited: bool,
}

/// Picks the least-recently-used idle terminal, if any. Exited terminals are
/// always eligible; a live one must have no running turn in its transcript and
/// a quiet PTY. Returns `None` when every candidate is busy.
fn pick_eviction(
    terminals: &[TerminalSnapshot],
    busy: &std::collections::HashMap<String, bool>,
    keep_conv: &str,
    now: i64,
) -> Option<(u64, String)> {
    let mut candidates: Vec<&TerminalSnapshot> = terminals
        .iter()
        // Dead terminals of the conversation being opened are stale links.
        .filter(|t| t.conv_id != keep_conv || t.exited)
        .filter(|t| {
            t.exited
                || (!busy.get(&t.conv_id).copied().unwrap_or(false)
                    && now - t.last_output_ms >= OUTPUT_GRACE_MS)
        })
        .collect();
    candidates.sort_by_key(|t| t.last_output_ms);
    candidates.first().map(|t| (t.id, t.conv_id.clone()))
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
        let alive = terms
            .values()
            .find(|t| t.conv_id == req.conversation_id && !*t.exited.lock().unwrap());
        if let Some(t) = alive {
            return Ok(TerminalStartInfo {
                conversation_id: t.conv_id.clone(),
                terminal_id: t.id,
                session_id: t.session_id.clone(),
                command: t.command.clone(),
                resumed: true,
                evicted_conversation_ids: Vec::new(),
            });
        }
    }

    // A conversation not created in YiTerm comes from Claude's own records;
    // adopt it on first open so the rest of the lifecycle is unchanged.
    let existing = {
        state
            .convs
            .read()
            .unwrap()
            .get(&req.conversation_id)
            .cloned()
    };
    let conv = match existing {
        Some(conv) => conv,
        None => {
            let home = dirs::home_dir().ok_or("no home dir")?;
            let found = sessions::claude::find_session(&home, &req.conversation_id)
                .ok_or("conversation not found")?;
            let conv = Conversation {
                id: found.session_id.clone(),
                folder: found.folder,
                session_id: found.session_id,
                terminal_id: None,
                title: found.title,
                created_at: found.modified_ms.to_string(),
                imported: true,
            };
            state
                .convs
                .write()
                .unwrap()
                .insert(conv.id.clone(), conv.clone());
            conv
        }
    };
    // Imported sessions must run in the exact cwd Claude recorded: that string
    // is what its project directory name is derived from, so canonicalizing it
    // can make `--resume` look in a different project.
    let folder = if conv.imported {
        if !std::path::Path::new(&conv.folder).is_dir() {
            return Err(format!("project folder {:?} no longer exists", conv.folder));
        }
        conv.folder.clone()
    } else {
        std::fs::canonicalize(&conv.folder)
            .map_err(|e| format!("folder {:?}: {e}", conv.folder))?
            .to_string_lossy()
            .into_owned()
    };

    let claude_path = state.settings.read().unwrap().resolve_claude_path()?;
    let session_id = conv.session_id.clone();

    // Resume if the transcript exists anywhere, otherwise seed the path where
    // a new session will appear. Resolving by session id avoids mismatches
    // between the recorded cwd and its canonicalized equivalent.
    let home = dirs::home_dir().ok_or("no home dir")?;
    let jsonl = sessions::claude::resolve_jsonl(&home, &folder, &session_id);
    let resumed = jsonl.is_file();
    if conv.imported && !resumed {
        return Err("session transcript not found".into());
    }

    // Keep the number of live Claude processes bounded: reclaim the oldest
    // idle terminals before adding another one.
    let evicted = reclaim_idle_terminals(&app, &state, &conv.id);

    let id = state.new_terminal_id();
    let transcript_boundary = TranscriptBoundary::capture(jsonl.clone())?;
    let handle = TerminalHandle::spawn(
        id,
        conv.id.clone(),
        folder.clone(),
        session_id.clone(),
        &claude_path,
        resumed,
        100,
        30,
        transcript_boundary,
    )?;
    let command = handle.command.clone();
    state.terms.write().unwrap().insert(id, Arc::clone(&handle));
    {
        let mut convs = state.convs.write().unwrap();
        if let Some(c) = convs.get_mut(&conv.id) {
            if !c.imported {
                c.folder = folder;
            }
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
            follower::spawn_follower(
                app.clone(),
                conv.id.clone(),
                jsonl,
                session_id.clone(),
                conv.imported,
                state.busy.clone(),
                handle.assistant_since_prompt.clone(),
                handle.plan_approval.clone(),
            ),
        );
    }

    Ok(TerminalStartInfo {
        conversation_id: conv.id,
        terminal_id: id,
        session_id,
        command,
        resumed,
        evicted_conversation_ids: evicted,
    })
}

/// A lone Escape byte is Claude Code's interrupt key; escape sequences such as
/// arrow keys (`\x1b[A`) are not.
fn is_interrupt_key(text: &str) -> bool {
    text == "\x1b"
}

/// Emitted when Esc aborts a turn, so the chat can mark the prompt that never
/// received a reply.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConversationInterrupted {
    conversation_id: String,
}

#[tauri::command]
pub fn terminal_write(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: u64,
    text: String,
    submit: bool,
) -> Result<(), String> {
    let handle = state
        .terms
        .read()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or("terminal not found")?;
    handle.write(&text, submit)?;

    // Esc is Claude Code's interrupt key. When the abort happens before the
    // first assistant message, the transcript contains no interrupt marker,
    // so the follower would keep the turn running forever. Clear it here; the
    // next transcript line restores the state if the turn really continues.
    if is_interrupt_key(&text) {
        let was_busy = {
            let mut map = state.busy.lock().unwrap();
            map.insert(handle.conv_id.clone(), false)
        };
        if was_busy == Some(true) {
            let _ = app.emit(
                "conversation-busy",
                follower::ConversationBusy {
                    conversation_id: handle.conv_id.clone(),
                    busy: false,
                },
            );
        }
        // Emit even when the busy flag has not caught up yet (Esc can be
        // pressed within the follower's 300 ms poll); the chat only marks a
        // trailing user bubble that has no assistant reply after it.
        let _ = app.emit(
            "conversation-interrupted",
            ConversationInterrupted {
                conversation_id: handle.conv_id.clone(),
            },
        );

        // Before the first assistant record, Claude Code restores the aborted
        // prompt into its input; later interruptions leave the input empty.
        // Only the former needs an Esc double-tap. Write directly to the PTY so
        // the clearing Escs don't re-enter this interrupt path.
        if was_busy == Some(true) && !handle.assistant_since_prompt.load(Ordering::Relaxed) {
            let clear = handle.clone();
            // Reserve before scheduling: an approval cannot cross either of
            // these delayed writes. Subsequent direct input cancels the revision.
            if let Ok(operation) = clear.reserve_input_operation() {
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    if clear.assistant_since_prompt.load(Ordering::Relaxed) {
                        return;
                    }
                    tracing::info!(id = clear.id, "interrupt clear: esc double tap");
                    if clear.write_operation(&operation, "\x1b", false).is_err() {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let _ = clear.write_operation(&operation, "\x1b", false);
                });
            }
        }
    }
    Ok(())
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

#[tauri::command]
pub fn plan_approval_status(
    state: State<'_, AppState>,
    conversation_id: String,
    terminal_id: u64,
    call_id: String,
) -> Result<PlanApprovalStatus, String> {
    with_plan_approval_terminal(&state, &conversation_id, terminal_id, |terminal| {
        terminal.plan_approval_status(&conversation_id, &call_id)
    })
}

#[tauri::command]
pub fn plan_approval_decide(
    state: State<'_, AppState>,
    conversation_id: String,
    terminal_id: u64,
    call_id: String,
    token: u64,
    mode: PlanExecutionMode,
) -> Result<(), String> {
    with_plan_approval_terminal(&state, &conversation_id, terminal_id, |terminal| {
        terminal.plan_approval_decide(&conversation_id, &call_id, token, mode)
    })
}

fn plan_owner_matches(
    conv: &Conversation,
    terminal: &TerminalHandle,
    conversation_id: &str,
    terminal_id: u64,
) -> bool {
    conv.id == conversation_id
        && conv.terminal_id == Some(terminal_id)
        && conv.session_id == terminal.session_id
        && terminal.conv_id == conversation_id
        && terminal.id == terminal_id
}

fn with_plan_approval_terminal<T>(
    state: &AppState,
    conversation_id: &str,
    terminal_id: u64,
    f: impl FnOnce(&TerminalHandle) -> Result<T, String>,
) -> Result<T, String> {
    // Keep the registry and binding stable through status/decision and its
    // single write. Reclamation retires before removing this exact handle.
    let terms = state.terms.read().map_err(|e| e.to_string())?;
    let terminal = terms
        .get(&terminal_id)
        .ok_or("终端不可用，请重新打开会话。")?;
    let convs = state.convs.read().map_err(|e| e.to_string())?;
    let conv = convs
        .get(conversation_id)
        .ok_or("会话已不存在，无法确认计划。")?;
    if !plan_owner_matches(conv, terminal, conversation_id, terminal_id) {
        return Err("计划所属的终端已变化，请在终端确认当前状态。".into());
    }
    f(terminal)
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationsOverview {
    pub conversations: Vec<Conversation>,
    pub projects: Vec<String>,
}

/// Stored conversations plus everything Claude Code has on disk: the sidebar
/// lists both, and empty registered projects are shown as groups too.
#[tauri::command]
pub async fn conversations_list(
    state: State<'_, AppState>,
) -> Result<ConversationsOverview, String> {
    let stored: Vec<Conversation> = {
        let convs = state.convs.read().unwrap();
        convs.values().cloned().collect()
    };
    let fallback = stored.clone();
    let Some(home) = dirs::home_dir() else {
        return Ok(ConversationsOverview {
            conversations: fallback,
            projects: Vec::new(),
        });
    };
    match tauri::async_runtime::spawn_blocking(move || {
        let (conversations, projects) = sessions::claude::overview(&home, stored);
        ConversationsOverview {
            conversations,
            projects,
        }
    })
    .await
    {
        Ok(overview) => Ok(overview),
        Err(e) => {
            tracing::warn!(error = %e, "claude project scan failed");
            Ok(ConversationsOverview {
                conversations: fallback,
                projects: Vec::new(),
            })
        }
    }
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
        imported: false,
    };
    state
        .convs
        .write()
        .unwrap()
        .insert(conv.id.clone(), conv.clone());
    state.persist_conversations(&app);
    Ok(conv)
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
    !conv_id.is_empty()
        && conv_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn chat_path(
    app: &tauri::AppHandle,
    state: &AppState,
    conv_id: &str,
) -> Result<std::path::PathBuf, String> {
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

/// Result of attaching a file chosen from disk.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedAttachment {
    /// "image" — copied into the conversation cache and pasted into the PTY
    /// like a screenshot; "file" — referenced by its original path.
    pub kind: String,
    pub path: String,
    pub name: String,
}

/// Attaches a file picked in the system dialog. Real images go through the
/// same cache + paste path as clipboard screenshots; everything else keeps its
/// original path, which the composer appends to the prompt so Claude can read
/// it (no copy, no translation of the path).
#[tauri::command]
pub fn chat_attachment_import(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    conv_id: String,
    path: String,
) -> Result<ImportedAttachment, String> {
    use std::io::Read;

    if !valid_conv_id(&conv_id) {
        return Err("invalid conversation id".into());
    }
    let src = std::path::PathBuf::from(&path);
    let meta = std::fs::metadata(&src).map_err(|e| format!("读取附件失败: {e}"))?;
    if !meta.is_file() {
        return Err("只能选择文件".into());
    }
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());

    // Read the magic bytes (a single `read` may return fewer than requested).
    let mut head = [0u8; 12];
    let mut filled = 0;
    let mut file = std::fs::File::open(&src).map_err(|e| format!("读取附件失败: {e}"))?;
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return Err(format!("读取附件失败: {e}")),
        }
    }
    if let Some(mime) = sniff_image_mime(&head[..filled]) {
        if meta.len() > MAX_IMAGE_BYTES as u64 {
            return Err(format!(
                "图片过大（上限 {} MB）",
                MAX_IMAGE_BYTES / (1024 * 1024)
            ));
        }
        let ext = image_extension(mime).expect("sniffed type is always supported");
        let dir = images_dir(&state, &app, &conv_id);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let dest = dir.join(format!("{}.{ext}", uuid::Uuid::new_v4()));
        std::fs::copy(&src, &dest).map_err(|e| format!("复制图片失败: {e}"))?;
        return Ok(ImportedAttachment {
            kind: "image".into(),
            path: dest.to_string_lossy().into_owned(),
            name,
        });
    }
    Ok(ImportedAttachment {
        kind: "file".into(),
        path,
        name,
    })
}

/// Unix millis — enough for stable newest-first ordering.
fn unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn now_iso() -> String {
    format!("{}", unix_millis())
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
    mut settings: crate::settings::Settings,
) -> Result<(), String> {
    let dir = state.config_dir(&app);
    settings.normalize();
    settings.save(&dir)?;
    *state.settings.write().unwrap() = settings;
    Ok(())
}

#[tauri::command]
pub fn api_key_get(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    provider: String,
) -> Option<String> {
    crate::settings::api_key_get(&state.config_dir(&app), &provider)
}

#[tauri::command]
pub fn api_key_set(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    provider: String,
    key: Option<String>,
) -> Result<(), String> {
    crate::settings::api_key_set(&state.config_dir(&app), &provider, key)
}

// ---------- translate ----------

#[tauri::command]
pub async fn translate(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request_id: String,
    direction: Direction,
    text: String,
    channel: Channel<TranslateEvent>,
) -> Result<String, String> {
    let settings = state.settings.read().unwrap().clone();
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    state
        .translate_cancels
        .lock()
        .unwrap()
        .insert(request_id.clone(), cancel_tx);

    let result = tokio::select! {
        result = async {
            if settings.translation.mode == "local" {
                return translate::local::translate(&app, &state, direction, &text, &request_id, &channel).await;
            }
            let http = state.http.clone();
            let key_provider = if settings.translation.mode == "deepl" {
                "deepl".to_string()
            } else {
                settings.translation.chat_provider.clone()
            };
            let api_key = if matches!(settings.translation.mode.as_str(), "chat" | "deepl") {
                crate::settings::api_key_get(&state.config_dir(&app), &key_provider)
            } else {
                None
            };
            translate::run(
                &http,
                &settings.translation,
                api_key.as_deref(),
                direction,
                &text,
                &request_id,
                &channel,
            )
            .await
        } => result,
        _ = cancel_rx => Err("翻译已取消".into()),
    };

    state.translate_cancels.lock().unwrap().remove(&request_id);
    result
}

/// Aborts an in-flight `translate` request (dropping its HTTP request and
/// stream). Unknown ids are ignored.
#[tauri::command]
pub fn translate_cancel(state: State<'_, AppState>, request_id: String) {
    if let Some(tx) = state.translate_cancels.lock().unwrap().remove(&request_id) {
        let _ = tx.send(());
    }
}

// ---------- local model management ----------

/// Current state of the embedded Hy-MT2-1.8B model file.
#[tauri::command]
pub fn model_status(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> translate::local::ModelStatus {
    translate::local::status(&app, &state)
}

/// Starts (or resumes) the background model download; progress arrives as
/// `model-progress` events.
#[tauri::command]
pub fn model_download(app: tauri::AppHandle) -> Result<(), String> {
    translate::local::start_download(&app)
}

/// Deletes the model file and unloads the in-memory translator.
#[tauri::command]
pub async fn model_delete(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    translate::local::delete_model(&app, &state).await
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

    #[test]
    fn eviction_prefers_the_oldest_idle_terminal() {
        let now = 1_000_000;
        let terminals = vec![
            TerminalSnapshot {
                id: 1,
                conv_id: "a".into(),
                last_output_ms: now - 60_000,
                exited: false,
            },
            TerminalSnapshot {
                id: 2,
                conv_id: "b".into(),
                last_output_ms: now - 10_000,
                exited: false,
            },
            TerminalSnapshot {
                id: 3,
                conv_id: "c".into(),
                last_output_ms: now - 1_000,
                exited: false,
            },
        ];
        let busy = std::collections::HashMap::new();
        assert_eq!(
            pick_eviction(&terminals, &busy, "new", now),
            Some((1, "a".to_string()))
        );
    }

    #[test]
    fn eviction_skips_busy_and_recently_printing_terminals() {
        let now = 1_000_000;
        let terminals = vec![
            TerminalSnapshot {
                id: 1,
                conv_id: "busy".into(),
                last_output_ms: now - 60_000,
                exited: false,
            },
            TerminalSnapshot {
                id: 2,
                conv_id: "recent".into(),
                last_output_ms: now - 1_000,
                exited: false,
            },
            TerminalSnapshot {
                id: 3,
                conv_id: "idle".into(),
                last_output_ms: now - 30_000,
                exited: false,
            },
        ];
        let busy = std::collections::HashMap::from([("busy".to_string(), true)]);
        assert_eq!(
            pick_eviction(&terminals, &busy, "new", now),
            Some((3, "idle".to_string()))
        );
    }

    #[test]
    fn eviction_never_touches_the_active_conversation() {
        let now = 1_000_000;
        let terminals = vec![
            TerminalSnapshot {
                id: 1,
                conv_id: "active".into(),
                last_output_ms: now - 60_000,
                exited: false,
            },
            TerminalSnapshot {
                id: 2,
                conv_id: "dead".into(),
                last_output_ms: now - 1_000,
                exited: true,
            },
        ];
        let busy = std::collections::HashMap::new();
        // The active live terminal is skipped; the dead one can go.
        assert_eq!(
            pick_eviction(&terminals, &busy, "active", now),
            Some((2, "dead".to_string()))
        );

        // Nothing evictable → the caller allows a temporary overshoot.
        let all_busy = std::collections::HashMap::from([("a".to_string(), true)]);
        let terms = vec![TerminalSnapshot {
            id: 1,
            conv_id: "a".into(),
            last_output_ms: now - 60_000,
            exited: false,
        }];
        assert_eq!(pick_eviction(&terms, &all_busy, "new", now), None);
    }

    #[test]
    fn plan_owner_requires_exact_registry_binding_session_and_incarnation() {
        let (mut terminal, _) = crate::terminal::tests::mock_terminal(false);
        let mut conv = Conversation {
            id: "conv".into(),
            session_id: "sess".into(),
            terminal_id: Some(7),
            folder: "/tmp".into(),
            title: "mock".into(),
            created_at: "0".into(),
            imported: false,
        };
        assert!(plan_owner_matches(&conv, &terminal, "conv", 7));
        assert!(!plan_owner_matches(&conv, &terminal, "other", 7));
        assert!(!plan_owner_matches(&conv, &terminal, "conv", 8));
        conv.terminal_id = Some(8);
        assert!(!plan_owner_matches(&conv, &terminal, "conv", 7));
        conv.terminal_id = Some(7);
        conv.session_id = "replacement".into();
        assert!(!plan_owner_matches(&conv, &terminal, "conv", 7));
        conv.session_id = "sess".into();
        terminal.conv_id = "other".into();
        assert!(!plan_owner_matches(&conv, &terminal, "conv", 7));
    }

    #[test]
    fn only_a_lone_escape_is_an_interrupt() {
        assert!(is_interrupt_key("\x1b"));
        assert!(!is_interrupt_key("\x1b[A")); // up arrow
        assert!(!is_interrupt_key("\x1b[200~")); // bracketed paste start
        assert!(!is_interrupt_key("\x03")); // Ctrl-C
        assert!(!is_interrupt_key(""));
    }
}
