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
    state.persist_conversations(&app);
    Ok(())
}

// ---------- chat history persistence (opaque JSON owned by the frontend) ----------

fn chat_path(app: &tauri::AppHandle, state: &AppState, conv_id: &str) -> Result<std::path::PathBuf, String> {
    // conv ids are UUIDs we generate; reject anything path-ish defensively.
    if !conv_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
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
