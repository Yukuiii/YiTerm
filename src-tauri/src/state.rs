use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
};

use tauri::Manager;

use crate::{
    sessions::Conversation,
    settings::Settings,
    translate::local::{DownloadHandle, TranslatorHandle},
};

/// Shared application state managed by Tauri.
pub struct AppState {
    pub terms: RwLock<HashMap<u64, Arc<crate::terminal::TerminalHandle>>>,
    pub convs: RwLock<HashMap<String, Conversation>>,
    /// Follower tasks per conversation id, so they can be aborted on delete.
    pub followers: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    /// conv id → a turn is running (derived from the JSONL transcript). The
    /// idle-terminal reclaimer never evicts a busy conversation.
    pub busy: Arc<Mutex<HashMap<String, bool>>>,
    pub settings: RwLock<Settings>,
    pub http: reqwest::Client,
    /// In-flight translation requests, keyed by request id, so the frontend
    /// can cancel one (e.g. Esc while the outbound prompt is translating).
    pub translate_cancels: Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>,
    /// Lazily loaded local translation model (Hy-MT2-1.8B).
    pub local: TranslatorHandle,
    /// Background download progress/state for the model file.
    pub model: DownloadHandle,
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
            busy: Arc::new(Mutex::new(HashMap::new())),
            settings: RwLock::new(settings),
            http: build_http_client(),
            translate_cancels: Mutex::new(HashMap::new()),
            local: Arc::new(tokio::sync::Mutex::new(None)),
            model: Arc::new(Mutex::new(crate::translate::local::ModelDownload::default())),
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

    /// `~/Library/Caches/<identifier>` — paste attachments live here. They are
    /// disposable by definition (the OS may purge them), which is why history
    /// rendering tolerates a missing file.
    pub fn cache_dir(&self, app: &tauri::AppHandle) -> std::path::PathBuf {
        app.path()
            .app_cache_dir()
            .expect("app cache dir unavailable")
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
    let parsed = parse_scutil_proxy(&text);

    // SOCKS proxy would need reqwest's "socks" feature; HTTP/HTTPS proxies
    // (Clash/Surge mixed ports serve both) are sufficient here.
    let mut proxies = Vec::new();
    if let Some(addr) = parsed.http {
        if let Ok(p) = reqwest::Proxy::http(&addr) {
            proxies.push(p);
        }
    }
    if let Some(addr) = parsed.https {
        if let Ok(p) = reqwest::Proxy::https(&addr) {
            proxies.push(p);
        }
    }
    (!proxies.is_empty()).then_some(proxies)
}

#[derive(Debug, Default, PartialEq)]
struct ScutilProxies {
    http: Option<String>,
    https: Option<String>,
}

/// Parses `scutil --proxy` output. Lines are indented, e.g.
/// `  HTTPEnable : 1` / `  HTTPSProxy : 127.0.0.1` — matches must trim first.
fn parse_scutil_proxy(text: &str) -> ScutilProxies {
    let get = |key: &str| -> Option<String> {
        text.lines()
            .map(str::trim_start)
            .find(|l| l.starts_with(key))?
            .split(':')
            .nth(1)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let enabled = |key: &str| get(key).as_deref() == Some("1");
    let addr = |en: &str, host_key: &str, port_key: &str| -> Option<String> {
        if !enabled(en) {
            return None;
        }
        let host = get(host_key)?;
        let port = get(port_key)?;
        Some(format!("http://{host}:{port}"))
    };
    ScutilProxies {
        http: addr("HTTPEnable", "HTTPProxy", "HTTPPort"),
        https: addr("HTTPSEnable", "HTTPSProxy", "HTTPSPort"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exact shape produced by `scutil --proxy` on macOS 26 (indented lines).
    const SAMPLE: &str = r#"<dictionary> {
  ExceptionsList : <array> {
    0 : 127.0.0.1
    1 : localhost
  }
  HTTPEnable : 1
  HTTPPort : 7890
  HTTPProxy : 127.0.0.1
  HTTPSEnable : 1
  HTTPSPort : 7890
  HTTPSProxy : 127.0.0.1
  ProxyAutoConfigEnable : 0
}"#;

    #[test]
    fn parses_indented_scutil_output() {
        let p = parse_scutil_proxy(SAMPLE);
        assert_eq!(p.http.as_deref(), Some("http://127.0.0.1:7890"));
        assert_eq!(p.https.as_deref(), Some("http://127.0.0.1:7890"));
    }

    #[test]
    fn disabled_proxies_yield_none() {
        let text = "  HTTPEnable : 0\n  HTTPProxy : 127.0.0.1\n  HTTPPort : 7890\n";
        let p = parse_scutil_proxy(text);
        assert_eq!(p.http, None);
        assert_eq!(p.https, None);
    }

    #[test]
    fn missing_keys_yield_none() {
        let p = parse_scutil_proxy("<dictionary> {\n  FTPPassive : 1\n}");
        assert_eq!(p, ScutilProxies::default());
    }
}
