//! Built-in anonymous DeepL channel.
//!
//! DeepL's interactive clients (notably the iOS app) talk to a stateless
//! "oneshot" endpoint that also serves anonymous traffic, with no API key.
//! This module reproduces that request profile (app headers, app_information,
//! a stable instance/session id, and a warm cookie jar) so YiTerm can offer
//! DeepL without a key or a self-hosted DeepLX gateway.
//!
//! The profile is fragile by nature: DeepL rate-limits clients whose
//! User-Agent, advertised app version and TLS story disagree. A stale app
//! version is answered with HTTP 429 for every request, so the version is
//! refreshed from the App Store once a day instead of being pinned at build
//! time.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tauri::ipc::Channel;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{Direction, TranslateEvent, chunk_utf16, protect_code, restore_code};

/// Anonymous endpoint; the Pro one needs an OAuth token and is unused here.
const URL: &str = "https://oneshot-free.www.deepl.com/v1/translate";
const APP_LOOKUP_URL: &str = "https://itunes.apple.com/lookup?id=1552407475&country=us";
/// Fallback used when the App Store lookup fails. Keep it tracking DeepL's
/// current release: a stale version is answered with 429 from any IP.
const FALLBACK_APP_VERSION: &str = "26.56";
const APP_BUILD: &str = "5443737";
/// Must be a shipping iOS major: an impossible value combined with the
/// iOS-style User-Agent is a cheap ban signal.
const IOS_OS_VERSION: &str = "26.0";
const CF_NETWORK_VERSION: &str = "3826.600.41";
const DARWIN_VERSION: &str = "25.0.0";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);
const COOKIE_TIMEOUT: Duration = Duration::from_secs(5);
const APP_VERSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const APP_VERSION_RETRY: Duration = Duration::from_secs(15 * 60);

/// Anonymous oneshot caps the whole `text` array at 1500 UTF-16 code units.
/// Stay below it so a chunk is never rejected with 413.
const CHUNK_MAX: usize = 1400;

fn instance_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| Uuid::new_v4().to_string())
}

fn session_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| Uuid::new_v4().to_string())
}

/// The oneshot endpoint expects localized target codes, not the official
/// API's EN/ZH: EN resolves to en-US and Chinese to Simplified script.
fn target_lang(direction: Direction) -> &'static str {
    match direction {
        Direction::ZhToEn => "en-US",
        Direction::EnToZh => "zh-Hans",
    }
}

/// Pin the source language: short mixed text such as
/// "conversations.json 的作用？" is otherwise detected as English and
/// returned unchanged.
fn source_lang(direction: Direction) -> &'static str {
    match direction {
        Direction::ZhToEn => "zh",
        Direction::EnToZh => "en",
    }
}

struct VersionCache {
    version: Option<String>,
    fetched_at: Option<Instant>,
    attempted_at: Option<Instant>,
}

fn version_cache() -> &'static Mutex<VersionCache> {
    static CACHE: OnceLock<Mutex<VersionCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(VersionCache {
            version: None,
            fetched_at: None,
            attempted_at: None,
        })
    })
}

/// The iOS marketing version to advertise. A successful lookup is cached for
/// a day; a failed one is not retried for 15 minutes. Any failure degrades to
/// the last resolved version, or the pinned fallback.
async fn app_version(http: &reqwest::Client) -> String {
    // Holding the lock across the lookup lets concurrent translations share a
    // single App Store query.
    let mut cache = version_cache().lock().await;
    let now = Instant::now();
    if let (Some(version), Some(fetched_at)) = (&cache.version, cache.fetched_at) {
        if now.duration_since(fetched_at) < APP_VERSION_TTL {
            return version.clone();
        }
    }
    if let Some(attempted_at) = cache.attempted_at {
        if now.duration_since(attempted_at) < APP_VERSION_RETRY {
            return cache
                .version
                .clone()
                .unwrap_or_else(|| FALLBACK_APP_VERSION.to_string());
        }
    }
    cache.attempted_at = Some(now);
    match fetch_app_version(http).await {
        Ok(version) => {
            cache.version = Some(version.clone());
            cache.fetched_at = Some(now);
            version
        }
        Err(error) => {
            tracing::warn!(error = %error, "DeepL iOS version lookup failed; using fallback");
            cache
                .version
                .clone()
                .unwrap_or_else(|| FALLBACK_APP_VERSION.to_string())
        }
    }
}

async fn fetch_app_version(http: &reqwest::Client) -> Result<String, String> {
    let body: Value = http
        .get(APP_LOOKUP_URL)
        .timeout(LOOKUP_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("App Store lookup failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("App Store lookup failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("App Store lookup returned invalid JSON: {e}"))?;
    let result = body
        .pointer("/results/0")
        .ok_or("App Store lookup returned no result")?;
    let expected = result.get("trackId").and_then(Value::as_i64) == Some(1552407475)
        && result.get("bundleId").and_then(Value::as_str)
            == Some("com.linguee.DeepLMobileTranslator")
        && result.get("artistName").and_then(Value::as_str) == Some("DeepL SE");
    if !expected {
        return Err("App Store lookup returned an unexpected app".into());
    }
    let version = result
        .get("version")
        .and_then(Value::as_str)
        .ok_or("App Store lookup returned no version")?;
    if !is_valid_version(version) {
        return Err(format!("implausible DeepL iOS version {version:?}"));
    }
    Ok(version.to_string())
}

fn is_valid_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    (2..=4).contains(&parts.len())
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// Cookies a cold visit to deepl.com sets (userCountry, verifiedBot). They are
/// best-effort: a failed warmup is cached as "no cookies" and translation
/// proceeds without them.
async fn cookie_header(http: &reqwest::Client) -> String {
    static COOKIE: OnceLock<tokio::sync::OnceCell<String>> = OnceLock::new();
    let cell = COOKIE.get_or_init(tokio::sync::OnceCell::new);
    cell.get_or_init(|| async {
        let Ok(resp) = http
            .get("https://www.deepl.com/translator")
            .timeout(COOKIE_TIMEOUT)
            .send()
            .await
        else {
            return String::new();
        };
        let mut pairs = Vec::new();
        for value in resp.headers().get_all("set-cookie") {
            let Ok(value) = value.to_str() else { continue };
            let pair = value.split(';').next().unwrap_or("").trim();
            if !pair.is_empty() {
                pairs.push(pair.to_string());
            }
        }
        pairs.join("; ")
    })
    .await
    .clone()
}

/// One chunk through the oneshot endpoint. Text protection and streaming
/// deltas are handled by [`translate`].
async fn request(
    http: &reqwest::Client,
    direction: Direction,
    version: &str,
    text: &str,
) -> Result<String, String> {
    let body = json!({
        "text": [text],
        "source_lang": source_lang(direction),
        "target_lang": target_lang(direction),
        "usage_type": "translate",
        "app_information": {
            "os": "iOS",
            "os_version": IOS_OS_VERSION,
            "app_version": version,
            "app_build": APP_BUILD,
            "instance_id": instance_id(),
        },
    });
    let user_agent =
        format!("DeepL/{version} CFNetwork/{CF_NETWORK_VERSION} Darwin/{DARWIN_VERSION}");
    let mut req = http
        .post(URL)
        .timeout(REQUEST_TIMEOUT)
        .header("Content-Type", "application/json")
        // The iOS client's anonymous branch literally sends "None".
        .header("Authorization", "None")
        .header("User-Agent", user_agent)
        .header("x-app-os-version", IOS_OS_VERSION)
        .header("x-app-instance-id", instance_id())
        .header("x-app-session-id", session_id())
        .header("Accept", "*/*")
        .header("Accept-Language", "en-US,en;q=0.9")
        .json(&body);
    let cookie = cookie_header(http).await;
    if !cookie.is_empty() {
        req = req.header("Cookie", cookie);
    }

    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            "DeepL 免费通道请求超时".to_string()
        } else {
            format!("DeepL 免费通道请求失败: {e}")
        }
    })?;
    let status = resp.status();
    if !status.is_success() {
        let hint = match status.as_u16() {
            403 => "（DeepL 拒绝了请求：客户端配置、认证或 IP 可能被临时限制）",
            429 => "（请求过于频繁，IP 被 DeepL 临时限制，请稍后重试）",
            _ => "",
        };
        let body = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(512)
            .collect::<String>();
        return Err(format!("DeepL 免费通道 HTTP {status}{hint}: {body}"));
    }
    let out: Value = resp
        .json()
        .await
        .map_err(|e| format!("DeepL 免费通道返回解析失败: {e}"))?;
    parse_translation(&out)
}

/// The oneshot response is the official v2 shape: `translations[].text`.
fn parse_translation(v: &Value) -> Result<String, String> {
    let translations = v
        .get("translations")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            let body: String = v.to_string().chars().take(200).collect();
            format!("DeepL 免费通道返回格式异常: {body}")
        })?;
    let text = translations
        .first()
        .and_then(|t| t.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        return Err("DeepL 免费通道返回了空译文".into());
    }
    Ok(text)
}

pub async fn translate(
    http: &reqwest::Client,
    direction: Direction,
    text: &str,
    request_id: &str,
    channel: &Channel<TranslateEvent>,
) -> Result<String, String> {
    let version = app_version(http).await;
    let (protected, blocks) = protect_code(text);
    let mut acc = String::new();

    for part in chunk_utf16(&protected, CHUNK_MAX) {
        if part.trim().is_empty() {
            acc.push_str(&part);
            continue;
        }
        let translated = request(http, direction, &version, &part).await?;
        let restored = restore_code(&translated, &blocks);
        acc.push_str(&restored);
        let _ = channel.send(TranslateEvent::Delta {
            request_id: request_id.to_string(),
            text: restored,
        });
    }
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_mapping_uses_localized_codes() {
        assert_eq!(source_lang(Direction::ZhToEn), "zh");
        assert_eq!(source_lang(Direction::EnToZh), "en");
        assert_eq!(target_lang(Direction::ZhToEn), "en-US");
        assert_eq!(target_lang(Direction::EnToZh), "zh-Hans");
    }

    #[test]
    fn version_validation() {
        assert!(is_valid_version("26.56"));
        assert!(is_valid_version("26.52.1"));
        assert!(is_valid_version("1.2.3.4"));
        assert!(!is_valid_version("26"));
        assert!(!is_valid_version("v26.1"));
        assert!(!is_valid_version("26..1"));
        assert!(!is_valid_version("26.1a"));
    }

    #[test]
    fn parses_oneshot_response() {
        let v = json!({"translations":[{"detected_source_language":"zh","text":"Hello, world!"}]});
        assert_eq!(parse_translation(&v).unwrap(), "Hello, world!");
        assert!(parse_translation(&json!({"translations":[]})).is_err());
        assert!(parse_translation(&json!({"error":"nope"})).is_err());
    }

    #[test]
    fn the_pinned_fallback_is_a_plausible_version() {
        assert!(is_valid_version(FALLBACK_APP_VERSION));
    }

    /// Live check of the request profile (network, not part of `cargo test`).
    #[tokio::test]
    #[ignore = "hits the live DeepL oneshot endpoint"]
    async fn live_oneshot_roundtrip() {
        let http = reqwest::Client::new();
        let version = app_version(&http).await;
        // Mixed Chinese/English must still translate; without a source hint
        // DeepL can detect this as English and echo it unchanged.
        let out = request(
            &http,
            Direction::ZhToEn,
            &version,
            "conversations.json 的作用？",
        )
        .await
        .expect("oneshot request");
        eprintln!("DeepL iOS {version} → {out}");
        assert!(out.to_ascii_lowercase().contains("conversations.json"));
        assert!(!out.contains('的'), "source text was echoed: {out}");
    }
}
