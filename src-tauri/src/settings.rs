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
    /// Chat-mode provider preset: "custom", "siliconflow", or "openrouter".
    /// Informational only — requests use `chat_base_url` + `chat_model_id`.
    #[serde(default = "default_chat_provider")]
    pub chat_provider: String,
    pub chat_base_url: Option<String>,
    pub chat_model_id: Option<String>,
}

fn default_chat_provider() -> String {
    "custom".to_string()
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
            chat_provider: default_chat_provider(),
            chat_base_url: None,
            chat_model_id: None,
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

/// One key per chat provider: preset keys must not overwrite each other.
/// `custom` keeps the original account name so existing keys continue to work.
fn keychain_user(provider: &str) -> String {
    let provider = provider.trim();
    if provider.is_empty() || provider == "custom" {
        KEYCHAIN_USER.to_string()
    } else {
        format!("{KEYCHAIN_USER}:{provider}")
    }
}

pub fn api_key_get(provider: &str) -> Option<String> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, &keychain_user(provider)).ok()?;
    match entry.get_password() {
        Ok(k) => Some(k),
        Err(keyring::Error::NoEntry) => None,
        Err(e) => {
            tracing::warn!(error = %e, "keychain read failed");
            None
        }
    }
}

pub fn api_key_set(provider: &str, key: Option<String>) -> Result<(), String> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, &keychain_user(provider))
        .map_err(|e| e.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_without_chat_provider_default_to_custom() {
        // Settings written before provider presets existed must still load.
        let s: Settings = serde_json::from_str(
            r#"{"translation":{"mode":"chat","chatBaseUrl":"https://example.com/v1","chatModelId":"m"}}"#,
        )
        .unwrap();
        assert_eq!(s.translation.chat_provider, "custom");
        assert_eq!(s.translation.mode, "chat");
    }

    #[test]
    fn chat_provider_roundtrips() {
        let json = serde_json::to_string(&TranslationSettings {
            chat_provider: "openrouter".into(),
            ..TranslationSettings::default()
        })
        .unwrap();
        let back: TranslationSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.chat_provider, "openrouter");
    }
}
