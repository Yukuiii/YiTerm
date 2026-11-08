//! DeepL translation (Free and Pro API hosts).
//!
//! DeepL has no streaming API: each chunk is one request/response, and the
//! restored chunk is emitted as a single delta. Text is protected the same way
//! as the other providers so code blocks survive verbatim.

use std::time::Duration;

use serde_json::{Value, json};
use tauri::ipc::Channel;

use super::{Direction, TranslateEvent, chunk, chunk_utf16, protect_code, restore_code};

const CHUNK_MAX: usize = 4000;
/// DeepLX-style gateways cap the whole text array at 1500 UTF-16 code units
/// (DLX returns 413 above it); stay below that.
const DEEPLX_CHUNK_MAX: usize = 1400;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const FREE_URL: &str = "https://api-free.deepl.com/v2/translate";
const PRO_URL: &str = "https://api.deepl.com/v2/translate";

/// A custom URL (proxy, mirror, or self-hosted gateway) wins. Otherwise
/// DeepL free keys carry a `:fx` suffix and only work on the free host;
/// paid keys must use the pro host.
pub fn endpoint(api_key: &str, custom: Option<&str>) -> String {
    if let Some(url) = custom.map(str::trim).filter(|u| !u.is_empty()) {
        return url.to_string();
    }
    if api_key.trim().to_ascii_lowercase().ends_with(":fx") {
        FREE_URL.to_string()
    } else {
        PRO_URL.to_string()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Protocol {
    /// Official DeepL API, authenticated with an API key.
    DeepL,
    /// DeepLX-compatible gateway (`{text, target_lang}` → `{code, data}`).
    DeepLx,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Auth {
    DeepLAuthKey,
    Bearer,
    None,
}

pub fn protocol(value: Option<&str>) -> Protocol {
    match value.map(str::trim) {
        Some("deeplx") => Protocol::DeepLx,
        _ => Protocol::DeepL,
    }
}

/// The protocol decides the auth scheme: official DeepL uses the
/// `DeepL-Auth-Key` scheme, DeepLX-style gateways use a Bearer token (their
/// URLs usually carry the key as well). An empty key means no header.
fn auth(protocol: Protocol, api_key: &str) -> Auth {
    if api_key.trim().is_empty() {
        return Auth::None;
    }
    match protocol {
        Protocol::DeepL => Auth::DeepLAuthKey,
        // DLX-style gateways use a Bearer token (often embedded in the URL).
        Protocol::DeepLx => Auth::Bearer,
    }
}

fn target_lang(direction: Direction) -> &'static str {
    match direction {
        Direction::ZhToEn => "EN",
        Direction::EnToZh => "ZH",
    }
}

/// Pin the source language instead of letting DeepL auto-detect: short mixed
/// text such as "conversations.json 的作用？" can be detected as English and
/// returned unchanged.
fn source_lang(direction: Direction) -> &'static str {
    match direction {
        Direction::ZhToEn => "ZH",
        Direction::EnToZh => "EN",
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn translate(
    http: &reqwest::Client,
    api_key: &str,
    api_url: Option<&str>,
    protocol: Protocol,
    direction: Direction,
    text: &str,
    request_id: &str,
    channel: &Channel<TranslateEvent>,
) -> Result<String, String> {
    let custom = api_url.map(str::trim).filter(|u| !u.is_empty());
    if protocol == Protocol::DeepLx && custom.is_none() {
        return Err(
            "DeepLX 需要填写自定义 API 地址（例如 https://api.deeplx.org/<key>/translate）".into(),
        );
    }
    if protocol == Protocol::DeepL && api_key.trim().is_empty() {
        return Err("DeepL 官方模式需要 API Key（设置 → 翻译方式 → DeepL）".into());
    }
    let auth = auth(protocol, api_key);

    let url = endpoint(api_key, custom);
    let (protected, blocks) = protect_code(text);
    let mut acc = String::new();

    // DeepLX gateways enforce the 1500 UTF-16-unit anonymous cap; the official
    // API accepts much larger bodies.
    let parts = match protocol {
        Protocol::DeepL => chunk(&protected, CHUNK_MAX),
        _ => chunk_utf16(&protected, DEEPLX_CHUNK_MAX),
    };

    for part in parts {
        if part.trim().is_empty() {
            acc.push_str(&part);
            continue;
        }
        // Official DeepL wants an array of texts; DeepLX-style gateways take a
        // plain string.
        let body = if protocol == Protocol::DeepL {
            json!({
                "text": [part],
                "source_lang": source_lang(direction),
                "target_lang": target_lang(direction),
            })
        } else {
            json!({
                "text": part,
                "source_lang": source_lang(direction),
                "target_lang": target_lang(direction),
            })
        };
        let mut req = http.post(&url).timeout(REQUEST_TIMEOUT).json(&body);
        req = match auth {
            Auth::DeepLAuthKey => req.header("Authorization", format!("DeepL-Auth-Key {api_key}")),
            Auth::Bearer => req.header("Authorization", format!("Bearer {api_key}")),
            Auth::None => req,
        };
        let resp = req
            .send()
            .await
            .map_err(|e| format!("DeepL 请求失败: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let hint = match status.as_u16() {
                403 => "（认证失败，请检查 API Key）",
                429 => "（请求过于频繁，请稍后重试）",
                456 => "（本月免费额度已用完）",
                _ => "",
            };
            let body = resp
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(512)
                .collect::<String>();
            return Err(format!("DeepL HTTP {status}{hint}: {body}"));
        }

        let out: Value = resp
            .json()
            .await
            .map_err(|e| format!("DeepL 返回解析失败: {e}"))?;
        let translated = parse_translation(&out)?;
        let restored = restore_code(&translated, &blocks);
        acc.push_str(&restored);
        let _ = channel.send(TranslateEvent::Delta {
            request_id: request_id.to_string(),
            text: restored,
        });
    }
    Ok(acc)
}

/// Official DeepL returns `translations[0].text`; DeepLX-style gateways
/// return `{ code, data }` (or `{ code, message }` on errors).
fn parse_translation(v: &Value) -> Result<String, String> {
    if let Some(text) = v.pointer("/translations/0/text").and_then(Value::as_str) {
        return Ok(text.to_string());
    }
    if let Some(code) = v.get("code").and_then(Value::as_i64) {
        if code != 200 {
            let message = v.get("message").and_then(Value::as_str).unwrap_or("");
            return Err(format!("DeepLX 返回错误 {code}: {message}"));
        }
    }
    if let Some(data) = v.get("data").and_then(Value::as_str) {
        return Ok(data.to_string());
    }
    let body: String = v.to_string().chars().take(200).collect();
    Err(format!("DeepL 返回格式异常: {body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_keys_use_the_free_host() {
        assert_eq!(endpoint("abc:fx", None), FREE_URL);
        assert_eq!(endpoint("ABC:FX", None), FREE_URL);
        assert_eq!(endpoint("abc", None), PRO_URL);
    }

    #[test]
    fn a_custom_url_wins() {
        assert_eq!(
            endpoint("abc:fx", Some("https://proxy.example.com/v2/translate")),
            "https://proxy.example.com/v2/translate"
        );
        // Blank custom values fall back to the automatic host.
        assert_eq!(endpoint("abc:fx", Some("  ")), FREE_URL);
    }

    #[test]
    fn protocol_and_auth_defaults() {
        assert_eq!(protocol(None), Protocol::DeepL);
        assert_eq!(protocol(Some("deeplx")), Protocol::DeepLx);
        assert_eq!(auth(Protocol::DeepL, "key"), Auth::DeepLAuthKey);
        assert_eq!(auth(Protocol::DeepLx, "key"), Auth::Bearer);
        assert_eq!(auth(Protocol::DeepLx, "  "), Auth::None);
    }

    #[test]
    fn parses_official_and_deeplx_responses() {
        let official = json!({"translations":[{"detected_source_language":"EN","text":"你好"}]});
        assert_eq!(parse_translation(&official).unwrap(), "你好");
        let deeplx = json!({"code":200,"data":"你好"});
        assert_eq!(parse_translation(&deeplx).unwrap(), "你好");
        let err = json!({"code":401,"message":"status code: 401"});
        assert!(parse_translation(&err).unwrap_err().contains("401"));
    }

    #[test]
    fn language_mapping() {
        assert_eq!(source_lang(Direction::ZhToEn), "ZH");
        assert_eq!(source_lang(Direction::EnToZh), "EN");
        assert_eq!(target_lang(Direction::ZhToEn), "EN");
        assert_eq!(target_lang(Direction::EnToZh), "ZH");
    }
}
