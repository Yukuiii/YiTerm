use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
};

use tauri::Manager;

use crate::{sessions::Conversation, settings::Settings};

/// Shared application state managed by Tauri.
pub struct AppState {
    pub terms: RwLock<HashMap<u64, Arc<crate::terminal::TerminalHandle>>>,
    pub convs: RwLock<HashMap<String, Conversation>>,
    /// Follower tasks per conversation id, so they can be aborted on delete.
    pub followers: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    pub settings: RwLock<Settings>,
    pub http: reqwest::Client,
    next_terminal_id: AtomicU64,
}

impl AppState {
    pub fn init(app: tauri::AppHandle) -> Self {
        let config_dir = app
            .path()
            .app_config_dir()
            .expect("app config dir unavailable");
        std::fs::create_dir_all(&config_dir).ok();
        let settings = Settings::load(&config_dir);
        let convs = crate::sessions::load_conversations(&config_dir);
        Self {
            terms: RwLock::new(HashMap::new()),
            convs: RwLock::new(convs),
            followers: Mutex::new(HashMap::new()),
            settings: RwLock::new(settings),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(35))
                .build()
                .expect("reqwest client"),
            next_terminal_id: AtomicU64::new(1),
        }
    }

    pub fn new_terminal_id(&self) -> u64 {
        self.next_terminal_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn config_dir(&self, app: &tauri::AppHandle) -> std::path::PathBuf {
        app.path()
            .app_config_dir()
            .expect("app config dir unavailable")
    }

    pub fn persist_conversations(&self, app: &tauri::AppHandle) {
        let dir = self.config_dir(app);
        let convs = self.convs.read().unwrap().clone();
        crate::sessions::save_conversations(&dir, &convs);
    }
}
