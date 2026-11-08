use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationSettings {
    /// "google" (free endpoint), "dlx" (built-in DeepL anonymous channel),
    /// "deepl" (official API or a DeepLX-compatible gateway), "chat"
    /// (OpenAI-compatible), or "local" (embedded Hy-MT2-1.8B, offline).
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
    /// Custom DeepL-compatible endpoint; empty/None → free/pro host chosen
    /// from the key suffix.
    #[serde(default)]
    pub deepl_api_url: Option<String>,
    /// "deepl" (official API) or "deeplx" (DeepLX-compatible gateway).
    #[serde(default)]
    pub deepl_protocol: Option<String>,
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
            deepl_api_url: None,
            deepl_protocol: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSettings {
    /// Custom font family; None/empty → the bundled Ioskeley Mono stack.
    #[serde(default)]
    pub font_family: Option<String>,
    #[serde(default = "default_term_font_size")]
    pub font_size: f32,
    #[serde(default = "default_term_line_height")]
    pub line_height: f32,
}

fn default_term_font_size() -> f32 {
    13.0
}

fn default_term_line_height() -> f32 {
    1.1
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: None,
            font_size: default_term_font_size(),
            line_height: default_term_line_height(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub translation: TranslationSettings,
    #[serde(default)]
    pub terminal: TerminalSettings,
    /// None → auto-detect: $HOME/.local/bin/claude, then PATH.
    pub claude_path: Option<String>,
}

impl Settings {
    pub fn load(config_dir: &Path) -> Self {
        let path = config_dir.join("settings.json");
        let mut settings = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(?path, error = %e, "corrupt settings.json, using defaults");
                Settings::default()
            }),
            Err(_) => Settings::default(),
        };
        settings.normalize();
        settings
    }

    /// Migrates settings written by older builds.
    ///
    /// The built-in DeepL free channel used to be stored as mode "deepl"
    /// with protocol "oneshot"; it is now a mode of its own, "dlx".
    pub fn normalize(&mut self) {
        if self.translation.mode == "deepl"
            && self.translation.deepl_protocol.as_deref() == Some("oneshot")
        {
            self.translation.mode = "dlx".to_string();
            self.translation.deepl_protocol = None;
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

// ---------- API keys (local file) ----------
//
// The app is unsigned and rebuilt often; talking to the macOS Keychain
// prompts or fails whenever the binary changes. Keys therefore live in a
// small JSON file next to settings.json, readable only by the user (0600).

fn keys_path(config_dir: &Path) -> PathBuf {
    config_dir.join("keys.json")
}

fn read_keys(config_dir: &Path) -> HashMap<String, String> {
    let path = keys_path(config_dir);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(?path, error = %e, "corrupt keys.json, starting empty");
            HashMap::new()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => {
            tracing::warn!(?path, error = %e, "keys.json read failed");
            HashMap::new()
        }
    }
}

fn write_keys(config_dir: &Path, keys: &HashMap<String, String>) -> Result<(), String> {
    std::fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
    let path = keys_path(config_dir);
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(keys).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// One key per chat provider: preset keys must not overwrite each other.
pub fn api_key_get(config_dir: &Path, provider: &str) -> Option<String> {
    read_keys(config_dir)
        .get(provider.trim())
        .cloned()
        .filter(|k| !k.trim().is_empty())
}

pub fn api_key_set(config_dir: &Path, provider: &str, key: Option<String>) -> Result<(), String> {
    let mut keys = read_keys(config_dir);
    let provider = provider.trim();
    match key {
        Some(k) if !k.trim().is_empty() => {
            keys.insert(provider.to_string(), k.trim().to_string());
        }
        _ => {
            keys.remove(provider);
        }
    }
    write_keys(config_dir, &keys)
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
    fn api_keys_live_in_a_file_and_are_per_provider() {
        let dir = std::env::temp_dir().join(format!("yiterm-keys-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(api_key_get(&dir, "custom"), None);
        api_key_set(&dir, "siliconflow", Some(" sk-sf ".into())).unwrap();
        api_key_set(&dir, "openrouter", Some("sk-or".into())).unwrap();
        assert_eq!(api_key_get(&dir, "siliconflow").as_deref(), Some("sk-sf"));
        assert_eq!(api_key_get(&dir, "openrouter").as_deref(), Some("sk-or"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("keys.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "keys.json must be user-only");
        }
        api_key_set(&dir, "siliconflow", None).unwrap();
        assert_eq!(api_key_get(&dir, "siliconflow"), None);
        assert_eq!(api_key_get(&dir, "openrouter").as_deref(), Some("sk-or"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn terminal_settings_default_when_missing() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.terminal.font_family, None);
        assert_eq!(s.terminal.font_size, 13.0);
        assert_eq!(s.terminal.line_height, 1.1);
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

    #[test]
    fn legacy_oneshot_protocol_migrates_to_dlx_mode() {
        let mut s: Settings =
            serde_json::from_str(r#"{"translation":{"mode":"deepl","deeplProtocol":"oneshot"}}"#)
                .unwrap();
        s.normalize();
        assert_eq!(s.translation.mode, "dlx");
        assert_eq!(s.translation.deepl_protocol, None);

        // The gateway protocol is left alone.
        let mut gateway: Settings =
            serde_json::from_str(r#"{"translation":{"mode":"deepl","deeplProtocol":"deeplx"}}"#)
                .unwrap();
        gateway.normalize();
        assert_eq!(gateway.translation.mode, "deepl");
        assert_eq!(
            gateway.translation.deepl_protocol.as_deref(),
            Some("deeplx")
        );
    }
}
