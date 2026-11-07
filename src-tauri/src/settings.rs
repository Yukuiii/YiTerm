use std::path::Path;

use serde::{Deserialize, Serialize};

const KEYCHAIN_SERVICE: &str = "com.yiterm.app";
const KEYCHAIN_USER: &str = "chat-api-key";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationSettings {
    /// "google" (built-in free endpoint), "chat" (OpenAI-compatible), or
    /// "local" (embedded Hy-MT2-1.8B, offline).
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Target language for the en→native direction.
    #[serde(default = "default_target_zh")]
    pub target_zh: String,
    pub chat_base_url: Option<String>,
    pub chat_model_id: Option<String>,
    /// Custom GGUF path for local mode; None → managed download location.
    pub local_model_path: Option<String>,
}

fn default_mode() -> String {
    "google".to_string()
}

fn default_target_zh() -> String {
    "zh-CN".to_string()
}

impl Default for TranslationSettings {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            target_zh: default_target_zh(),
            chat_base_url: None,
            chat_model_id: None,
            local_model_path: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub translation: TranslationSettings,
    /// None → auto-detect: $HOME/.local/bin/claude, then PATH.
    pub claude_path: Option<String>,
}

impl Settings {
    pub fn load(config_dir: &Path) -> Self {
        let path = config_dir.join("settings.json");
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(?path, error = %e, "corrupt settings.json, using defaults");
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self, config_dir: &Path) -> Result<(), String> {
        let path = config_dir.join("settings.json");
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let tmp = config_dir.join("settings.json.tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn resolve_claude_path(&self) -> Result<String, String> {
        if let Some(p) = &self.claude_path {
            if !p.trim().is_empty() {
                return Ok(p.clone());
            }
        }
        if let Some(home) = dirs::home_dir() {
            let candidate = home.join(".local/bin/claude");
            if candidate.is_file() {
                return Ok(candidate.to_string_lossy().into_owned());
            }
        }
        which_claude().ok_or_else(|| {
            "claude CLI not found (looked in ~/.local/bin and PATH); set it in Settings".to_string()
        })
    }
}

fn which_claude() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("claude");
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

// ---------- API key (macOS Keychain) ----------

pub fn api_key_get() -> Option<String> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER).ok()?;
    match entry.get_password() {
        Ok(k) => Some(k),
        Err(keyring::Error::NoEntry) => None,
        Err(e) => {
            tracing::warn!(error = %e, "keychain read failed");
            None
        }
    }
}

pub fn api_key_set(key: Option<String>) -> Result<(), String> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER).map_err(|e| e.to_string())?;
    match key {
        Some(k) if !k.trim().is_empty() => entry
            .set_password(k.trim())
            .map_err(|e| e.to_string()),
        _ => match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        },
    }
}
