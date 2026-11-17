//! Built-in anonymous Duck.ai channel.
//!
//! Duck.ai has no public API. Its SPA asks `/duckchat/v1/status` for a JS
//! challenge (`x-vqd-hash-1`), runs that challenge in a same-origin iframe,
//! SHA-256 hashes the returned `client_hashes`, and sends the result back as
//! `X-Vqd-Hash-1` on `/duckchat/v1/chat`. The challenge measures DOM layout,
//! so it only works in a real browser: the frontend runs it in a sandboxed
//! `data:` iframe (opaque origin, no Tauri IPC) and this module does the
//! hashing plus both HTTP calls.
//!
//! The profile is fragile by nature: Duck.ai rate-limits anonymous clients
//! aggressively and can answer 418 (`ERR_BN_LIMIT`) for a whole network. The
//! response is a plain SSE stream; a static RSA public key satisfies the
//! `durableStream` envelope until Duck.ai starts encrypting chunks.

use std::{
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tauri::ipc::Channel;

use super::{
    Direction, ProtectedBlock, StreamRestorer, TranslateEvent, emit_delta, error_body, protect_code,
};

const PAGE_URL: &str = "https://duck.ai/";
const STATUS_URL: &str = "https://duck.ai/duckchat/v1/status";
const CHAT_URL: &str = "https://duck.ai/duckchat/v1/chat";
const WEB_ORIGIN: &str = "https://duck.ai";
/// The model the SPA currently selects by default. Model ids are part of the
/// unofficial protocol; update this when Duck.ai renames them.
const MODEL: &str = "gpt-6-luna";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const PAGE_TIMEOUT: Duration = Duration::from_secs(15);
const VERSION_TTL: Duration = Duration::from_secs(60 * 60);

/// Public half of a locally generated RSA-2048 key. Duck.ai only needs a
/// well-formed JWK in `durableStream`; observed responses are plain JSON.
const DURABLE_KEY_N: &str = "padNUOdXi1QYMbfUpULOIdHadZTslxD4xAy2bk7nUDObvjMtQeffbsheBLm9TUoYtbGoZhxncJMRWpwQpROMnR5cNz1x_Egzb8w1bNRXVcI72edaavz-7o7lZwlcEZMkXvpcQz4UmDYBPuPF_4sXTRh7G_FfoONwMIdfVa7QuSMfA1sAhxieJmgwd8W5c2l2uEDqPfSe4DdXkJsmnRsGCpxRiP10JS9kNitOurIKocU5Ra9EIFii8bcESWOgHjnmMdbKlRdf23CtrT2k6_0BdmRn7hMTGkO9eM8-L8uFxxLeTxgHJ8a5jlNAjlr04iSEuWLvIpKf6SE6YuVl6iVzrw";

struct VersionCache {
    value: String,
    at: Instant,
}

static VERSION_CACHE: LazyLock<Mutex<Option<VersionCache>>> = LazyLock::new(|| Mutex::new(None));

/// The raw challenge the frontend executes. `client_hashes` are pre-hash
/// strings; `meta` carries the challenge bookkeeping the SPA echoes back.
#[derive(Debug, Deserialize)]
pub struct ChallengeResult {
    server_hashes: Vec<String>,
    client_hashes: Vec<String>,
    #[serde(default = "empty_object")]
    signals: Value,
    meta: ChallengeMeta,
}

#[derive(Debug, Deserialize)]
struct ChallengeMeta {
    v: String,
    challenge_id: String,
    timestamp: String,
    debug: String,
}

fn empty_object() -> Value {
    json!({})
}

/// Serialized in the same field order as the SPA so the payload base64 is
/// byte-compatible with what Duck.ai expects.
#[derive(Serialize)]
struct VqdPayload<'a> {
    server_hashes: &'a [String],
    client_hashes: Vec<String>,
    signals: &'a Value,
    meta: VqdMeta<'a>,
}

#[derive(Serialize)]
struct VqdMeta<'a> {
    v: &'a str,
    challenge_id: &'a str,
    timestamp: &'a str,
    debug: &'a str,
    origin: &'static str,
    stack: &'a str,
    duration: String,
}

/// `GET /duckchat/v1/status` returns the challenge in a response header and
/// nothing useful in the body.
pub async fn challenge(http: &reqwest::Client, user_agent: &str) -> Result<String, String> {
    let resp = http
        .get(STATUS_URL)
        .timeout(PAGE_TIMEOUT)
        .header("x-vqd-accept", "1")
        .header("user-agent", user_agent)
        .header("origin", WEB_ORIGIN)
        .header("referer", PAGE_URL)
        .send()
        .await
        .map_err(|e| format!("Duck.ai 挑战请求失败: {e}"))?;
    let status = resp.status();
    let challenge = resp
        .headers()
        .get("x-vqd-hash-1")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    match challenge {
        Some(value) => Ok(value),
        None => {
            let body = error_body(resp, 300).await;
            Err(format!("Duck.ai 未返回挑战（HTTP {status}）: {body}"))
        }
    }
}

/// Hashes the challenge's client values and wraps everything into the
/// `X-Vqd-Hash-1` request header value.
pub fn build_vqd(result: ChallengeResult, stack: &str, duration_ms: u64) -> Result<String, String> {
    let client_hashes = result
        .client_hashes
        .iter()
        .map(|raw| STANDARD.encode(Sha256::digest(raw.as_bytes())))
        .collect();
    let payload = VqdPayload {
        server_hashes: &result.server_hashes,
        client_hashes,
        signals: &result.signals,
        meta: VqdMeta {
            v: &result.meta.v,
            challenge_id: &result.meta.challenge_id,
            timestamp: &result.meta.timestamp,
            debug: &result.meta.debug,
            origin: WEB_ORIGIN,
            stack,
            duration: duration_ms.to_string(),
        },
    };
    serde_json::to_vec(&payload)
        .map(|bytes| STANDARD.encode(bytes))
        .map_err(|e| format!("Duck.ai 挑战编码失败: {e}"))
}

/// One Duck.ai chat per translation; SSE deltas stream as they arrive. The
/// solved VQD is reused across translations until it expires.
#[allow(clippy::too_many_arguments)]
pub async fn translate(
    http: &reqwest::Client,
    direction: Direction,
    text: &str,
    vqd: &str,
    user_agent: &str,
    request_id: &str,
    channel: &Channel<TranslateEvent>,
) -> Result<String, String> {
    let (protected, blocks) = protect_code(text);
    let version = fe_version(http, user_agent).await?;
    request(
        http, direction, &protected, vqd, user_agent, &version, &blocks, request_id, channel,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn request(
    http: &reqwest::Client,
    direction: Direction,
    text: &str,
    vqd: &str,
    user_agent: &str,
    fe_version: &str,
    blocks: &[ProtectedBlock],
    request_id: &str,
    channel: &Channel<TranslateEvent>,
) -> Result<String, String> {
    let conversation_id = compact_uuid();
    let response_id = uuid::Uuid::new_v4().to_string();
    let signals = STANDARD.encode(b"{}");
    let body = json!({
        "model": MODEL,
        "messages": [{ "role": "user", "content": prompt(direction, text) }],
        "canUseTools": false,
        "reasoningEffort": "none",
        "canUseApproxLocation": null,
        "canDelegateImageGeneration": null,
        "canShowGreeting": false,
        "durableStream": {
            "messageId": compact_uuid(),
            "conversationId": conversation_id,
            "publicKey": {
                "alg": "RSA-OAEP-256",
                "e": "AQAB",
                "ext": true,
                "key_ops": ["encrypt"],
                "kty": "RSA",
                "n": DURABLE_KEY_N,
                "use": "enc"
            }
        }
    });

    let resp = http
        .post(CHAT_URL)
        .timeout(REQUEST_TIMEOUT)
        .header("user-agent", user_agent)
        .header("accept", "text/event-stream")
        .header("accept-language", "en-US,en;q=0.9")
        .header("content-type", "application/json")
        .header("origin", WEB_ORIGIN)
        .header("referer", PAGE_URL)
        .header("x-fe-version", fe_version)
        .header("x-fe-signals", signals)
        .header("x-ddg-journey-id", &conversation_id)
        .header("x-duckai-response-id", response_id)
        .header("x-vqd-hash-1", vqd)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Duck.ai 请求失败: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body = error_body(resp, 400).await;
        return Err(upstream_error(status.as_u16(), &body));
    }
    let is_sse = resp
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"));
    if !is_sse {
        return Err(format!("Duck.ai 未返回事件流（HTTP {status}）"));
    }

    let mut stream = resp.bytes_stream().eventsource();
    let mut restorer = StreamRestorer::new(blocks);
    while let Some(event) = stream.next().await {
        let event = event.map_err(|e| format!("Duck.ai SSE: {e}"))?;
        let data = event.data.trim();
        if data == "[DONE]" {
            break;
        }
        if let Some(delta) = assistant_delta(data)
            && let Some(out) = restorer.push(&delta)
        {
            emit_delta(channel, request_id, out);
        }
    }
    if let Some(out) = restorer.flush() {
        emit_delta(channel, request_id, out);
    }
    if restorer.is_empty() {
        return Err("Duck.ai 返回为空".into());
    }
    Ok(restorer.full())
}

/// One SSE payload → assistant text. Duck.ai also emits `[PING]`,
/// `[CHAT_TITLE:…]`, tool, and source events; none of them are translation.
fn assistant_delta(data: &str) -> Option<String> {
    if data.is_empty() || data == "[PING]" || data.starts_with("[CHAT_TITLE:") {
        return None;
    }
    let value: Value = serde_json::from_str(data).ok()?;
    if value.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    value
        .get("message")
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty())
        .map(str::to_owned)
}

fn prompt(direction: Direction, text: &str) -> String {
    let (source, target) = match direction {
        Direction::ZhToEn => ("Chinese", "English"),
        Direction::EnToZh => ("English", "Simplified Chinese (zh-CN)"),
    };
    format!(
        "Translate the following {source} text into {target}. \
         Preserve markdown formatting. Tokens of the form @@C0@@ are protected \
         code; reproduce them exactly where they appear. \
         Output ONLY the translation, with no explanations.\n\n{text}"
    )
}

/// 418 is what Duck.ai returns when the challenge is stale or the network is
/// limited. The frontend uses this marker to refresh the VQD and retry once.
fn upstream_error(status: u16, body: &str) -> String {
    if body.contains("ERR_CHALLENGE") || body.contains("ERR_BN_LIMIT") {
        format!("DUCKAI_VQD_INVALID: Duck.ai 挑战失效或当前网络被限流（HTTP {status}）")
    } else if body.contains("ERR_INPUT_LIMIT") {
        "Duck.ai 拒绝了过长的输入".to_string()
    } else {
        format!("Duck.ai HTTP {status}: {body}")
    }
}

/// `x-fe-version` is `<data-version-tag>-<data-version-sha>` from the page.
/// Fetch it hourly; a stale cached value is still a better identity than a
/// failed translation when the HTML request itself fails.
async fn fe_version(http: &reqwest::Client, user_agent: &str) -> Result<String, String> {
    if let Some(cached) = VERSION_CACHE.lock().unwrap().as_ref()
        && cached.at.elapsed() < VERSION_TTL
    {
        return Ok(cached.value.clone());
    }
    match fetch_fe_version(http, user_agent).await {
        Ok(value) => {
            *VERSION_CACHE.lock().unwrap() = Some(VersionCache {
                value: value.clone(),
                at: Instant::now(),
            });
            Ok(value)
        }
        Err(err) => match VERSION_CACHE.lock().unwrap().as_ref() {
            Some(cached) => {
                tracing::warn!(error = %err, "duckai version refresh failed; using cached");
                Ok(cached.value.clone())
            }
            None => Err(err),
        },
    }
}

async fn fetch_fe_version(http: &reqwest::Client, user_agent: &str) -> Result<String, String> {
    let resp = http
        .get(PAGE_URL)
        .timeout(PAGE_TIMEOUT)
        .header("user-agent", user_agent)
        .send()
        .await
        .map_err(|e| format!("Duck.ai 版本请求失败: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("Duck.ai 版本请求 HTTP {status}"));
    }
    let html = resp
        .text()
        .await
        .map_err(|e| format!("Duck.ai 版本读取失败: {e}"))?;
    parse_fe_version(&html)
}

fn parse_fe_version(html: &str) -> Result<String, String> {
    let tag = attr(html, "data-version-tag").ok_or("Duck.ai 页面缺少 data-version-tag")?;
    let sha = attr(html, "data-version-sha").ok_or("Duck.ai 页面缺少 data-version-sha")?;
    Ok(format!("{tag}-{sha}"))
}

fn attr<'a>(html: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{name}=\"");
    let start = html.find(&prefix)? + prefix.len();
    let end = html[start..].find('"')? + start;
    Some(&html[start..end])
}

fn compact_uuid() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_frontend_version_attributes() {
        let html = r#"<html data-version-tag="serp_20261117_034225_ET"
            data-version-sha="2db9209917ca37a1ea076dc62e25eff43d2aba2a">"#;
        assert_eq!(
            parse_fe_version(html).unwrap(),
            "serp_20261117_034225_ET-2db9209917ca37a1ea076dc62e25eff43d2aba2a"
        );
        assert!(parse_fe_version("<html>").is_err());
    }

    #[test]
    fn hashes_client_values_into_the_vqd_header() {
        let result = ChallengeResult {
            server_hashes: vec!["server".into()],
            client_hashes: vec!["abc".into()],
            signals: json!({}),
            meta: ChallengeMeta {
                v: "4".into(),
                challenge_id: "challenge".into(),
                timestamp: "123".into(),
                debug: "ND".into(),
            },
        };
        let vqd = build_vqd(result, "stack", 42).unwrap();
        let payload: Value =
            serde_json::from_slice(&STANDARD.decode(vqd).unwrap()).expect("payload json");
        assert_eq!(
            payload["client_hashes"][0],
            "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0="
        );
        assert_eq!(payload["meta"]["origin"], WEB_ORIGIN);
        assert_eq!(payload["meta"]["duration"], "42");
        assert_eq!(payload["meta"]["stack"], "stack");
    }

    #[test]
    fn extracts_only_assistant_text_from_sse() {
        assert_eq!(
            assistant_delta(r#"{"role":"assistant","message":"你好"}"#).as_deref(),
            Some("你好")
        );
        assert_eq!(assistant_delta("[PING]"), None);
        assert_eq!(assistant_delta("[DONE]"), None);
        assert_eq!(assistant_delta("[CHAT_TITLE:标题]"), None);
        assert_eq!(
            assistant_delta(r#"{"role":"tool-invocation","state":"call"}"#),
            None
        );
        assert_eq!(assistant_delta("not json"), None);
    }

    #[test]
    fn marks_rate_limits_as_refreshable() {
        assert!(
            upstream_error(418, r#"{"type":"ERR_BN_LIMIT"}"#).starts_with("DUCKAI_VQD_INVALID")
        );
        assert!(
            upstream_error(418, r#"{"type":"ERR_CHALLENGE"}"#).starts_with("DUCKAI_VQD_INVALID")
        );
        assert_eq!(
            upstream_error(429, r#"{"type":"ERR_INPUT_LIMIT"}"#),
            "Duck.ai 拒绝了过长的输入"
        );
    }
}
