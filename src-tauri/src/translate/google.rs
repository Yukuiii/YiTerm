use serde_json::{json, Value};
use tauri::ipc::Channel;

use super::{chunk, TranslateEvent};

/// Chrome's built-in translate key (same one the proven proxy uses).
const CHROME_KEY: &str = "AIzaSyATBXajvzQLTDHEQbcpq0Ihe0vWDHmO520";
const TRANSLATE_HTML_URL: &str = "https://translate-pa.googleapis.com/v1/translateHtml";
const GTX_URL: &str = "https://translate.googleapis.com/translate_a/single";
const CHUNK_MAX: usize = 800;
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/153 Safari/537.36";

/// Chunked Google translation; per chunk: primary endpoint, retry once on
/// 429/5xx after 1 s, then the translate_a fallback. Emits one Delta per
/// completed chunk so long texts appear progressively.
pub async fn translate(
    http: &reqwest::Client,
    target: &str,
    text: &str,
    request_id: &str,
    channel: &Channel<TranslateEvent>,
) -> Result<String, String> {
    let mut acc = String::new();
    for part in chunk(text, CHUNK_MAX) {
        let out = match chrome_translate(http, &part, &target).await {
            Ok(out) => out,
            Err(first_err) => {
                tracing::warn!(error = %first_err, "translateHtml failed, retrying once");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                match chrome_translate(http, &part, &target).await {
                    Ok(out) => out,
                    Err(_) => gtx_translate(http, &part, &target)
                        .await
                        .map_err(|e| format!("Google 翻译失败: {e}"))?,
                }
            }
        };
        acc.push_str(&out);
        let _ = channel.send(TranslateEvent::Delta {
            request_id: request_id.to_string(),
            text: out,
        });
    }
    Ok(acc)
}

async fn chrome_translate(
    http: &reqwest::Client,
    text: &str,
    target: &str,
) -> Result<String, String> {
    let payload = json!([[text, "auto", target], "te_lib"]);
    let resp = http
        .post(TRANSLATE_HTML_URL)
        .header("Content-Type", "application/json+protobuf")
        .header("x-goog-api-key", CHROME_KEY)
        .header("referer", "https://google.com/")
        .header("User-Agent", UA)
        .body(payload.to_string())
        .send()
        .await
        .map_err(|e| format!("translateHtml request: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(256)
            .collect::<String>();
        return Err(format!("translateHtml HTTP {status}: {body}"));
    }
    let out: Value = resp
        .json()
        .await
        .map_err(|e| format!("translateHtml decode: {e}"))?;
    let parts = out
        .as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_array)
        .ok_or("unexpected translateHtml response shape")?;
    let mut joined = String::new();
    for p in parts {
        if let Some(s) = p.as_str() {
            joined.push_str(s);
        }
    }
    if joined.is_empty() {
        return Err("no translated text".into());
    }
    Ok(html_unescape(&joined))
}

async fn gtx_translate(
    http: &reqwest::Client,
    text: &str,
    target: &str,
) -> Result<String, String> {
    let resp = http
        .get(GTX_URL)
        .query(&[
            ("client", "at"),
            ("sl", "auto"),
            ("tl", target),
            ("dt", "t"),
            ("q", text),
        ])
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await
        .map_err(|e| format!("gtx request: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("gtx HTTP {status}"));
    }
    let out: Value = resp
        .json()
        .await
        .map_err(|e| format!("gtx decode: {e}"))?;
    let segments = out
        .as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_array)
        .ok_or("unexpected gtx response shape")?;
    let mut joined = String::new();
    for seg in segments {
        if let Some(s) = seg
            .as_array()
            .and_then(|a| a.first())
            .and_then(Value::as_str)
        {
            joined.push_str(s);
        }
    }
    if joined.is_empty() {
        return Err("no translated text".into());
    }
    Ok(joined)
}

/// Minimal HTML entity unescape (Google returns escaped fragments).
fn html_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find('&') {
        out.push_str(&rest[..idx]);
        rest = &rest[idx..];
        let end = rest.find(';').unwrap_or(0);
        if end > 1 && end <= 10 {
            let entity = &rest[1..end];
            let replacement = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" | "#x27" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => {
                    if let Some(num) = entity.strip_prefix('#').and_then(|n| {
                        n.parse::<u32>().ok().or_else(|| {
                            n.strip_prefix('x').and_then(|h| u32::from_str_radix(h, 16).ok())
                        })
                    }) {
                        char::from_u32(num)
                    } else {
                        None
                    }
                }
            };
            match replacement {
                Some(c) => {
                    out.push(c);
                    rest = &rest[end + 1..];
                }
                None => {
                    out.push('&');
                    rest = &rest[1..];
                }
            }
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_common_entities() {
        assert_eq!(html_unescape("a &amp; b"), "a & b");
        assert_eq!(html_unescape("&lt;div&gt;"), "<div>");
        assert_eq!(html_unescape("&#39;quote&#x27;"), "'quote'");
        assert_eq!(html_unescape("no entities"), "no entities");
        assert_eq!(html_unescape("a & b &amp; c"), "a & b & c");
        assert_eq!(html_unescape("&nbsp;"), "\u{a0}");
    }
}
