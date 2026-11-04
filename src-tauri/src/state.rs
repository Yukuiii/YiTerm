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
            http: build_http_client(),
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

/// reqwest reads HTTP(S)_PROXY env vars by default, but a GUI app launched
/// from Finder/Dock has no shell environment — read the macOS system proxy
/// (scutil) and wire it in explicitly so Google Translate works for users
/// whose only route to Google is a local proxy (e.g. 127.0.0.1:7890).
fn build_http_client() -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(35))
        .connect_timeout(std::time::Duration::from_secs(8));
    match macos_system_proxies() {
        Some(proxies) => {
            for p in proxies {
                tracing::info!(proxy = ?p, "using macOS system proxy for translation traffic");
                builder = builder.proxy(p);
            }
        }
        None => tracing::info!("no macOS system proxy configured"),
    }
    builder.build().expect("reqwest client")
}

fn macos_system_proxies() -> Option<Vec<reqwest::Proxy>> {
    // GUI processes may have a minimal PATH without /usr/sbin — use the
    // absolute path first, bare name as fallback.
    let output = std::process::Command::new("/usr/sbin/scutil")
        .arg("--proxy")
        .output()
        .or_else(|_| std::process::Command::new("scutil").arg("--proxy").output())
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let get = |key: &str| -> Option<String> {
        text.lines()
            .find(|l| l.starts_with(key))?
            .split(':')
            .nth(1)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let enabled = |key: &str| get(key).as_deref() == Some("1");

    // SOCKS proxy would need reqwest's "socks" feature; HTTP/HTTPS proxies
    // (Clash/Surge mixed ports serve both) are sufficient here.
    let mut proxies = Vec::new();
    if enabled("HTTPEnable") {
        if let (Some(host), Some(port)) = (get("HTTPProxy"), get("HTTPPort")) {
            if let Ok(p) = reqwest::Proxy::http(&format!("http://{host}:{port}")) {
                proxies.push(p);
            }
        }
    }
    if enabled("HTTPSEnable") {
        if let (Some(host), Some(port)) = (get("HTTPSProxy"), get("HTTPSPort")) {
            if let Ok(p) = reqwest::Proxy::https(&format!("http://{host}:{port}")) {
                proxies.push(p);
            }
        }
    }
    (!proxies.is_empty()).then_some(proxies)
}
