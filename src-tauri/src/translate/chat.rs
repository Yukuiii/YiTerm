use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tauri::ipc::Channel;

use super::{protect_code, restore_code, safe_stream_prefix, Direction, TranslateEvent};

/// OpenAI-compatible chat translation with SSE streaming. Falls back to a
/// single non-streamed JSON response when the server doesn't speak SSE.
#[allow(clippy::too_many_arguments)]
pub async fn translate(
    http: &reqwest::Client,
    base_url: &str,
    model: &str,
    api_key: Option<&str>,
    direction: Direction,
    text: &str,
    request_id: &str,
    channel: &Channel<TranslateEvent>,
) -> Result<String, String> {
    let url = normalize_chat_url(base_url)?;
    let system = match direction {
        Direction::ZhToEn => {
            "You are a translation engine. Translate the user's text into English. \
             Preserve markdown formatting. Tokens of the form @@C0@@ are protected \
             code; reproduce them exactly where they appear. \
             Output ONLY the translation, with no explanations."
        }
        Direction::EnToZh => {
            "You are a translation engine. Translate the user's text into Simplified \
             Chinese (zh-CN). Preserve markdown formatting. Tokens of the form @@C0@@ \
             are protected code; reproduce them exactly where they appear. \
             Output ONLY the translation, with no explanations."
        }
    };

    // Swap code blocks / inline code for sentinels so they survive verbatim.
    let (protected, blocks) = protect_code(text);

    let mut req = http
        .post(&url)
        // Total cap for the streamed translation (covers SSE body reads).
        .timeout(std::time::Duration::from_secs(90))
        .json(&json!({
            "model": model,
            "stream": true,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": protected }
            ]
        }));
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("chat 请求失败: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body = body.chars().take(512).collect::<String>();
        return Err(format!("chat HTTP {status}: {body}"));
    }

    let is_sse = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("text/event-stream"));

    if is_sse {
        let mut stream = resp.bytes_stream().eventsource();
        let mut acc = String::new();
        let mut pending = String::new(); // may end inside a sentinel
        while let Some(event) = stream.next().await {
            let event = event.map_err(|e| format!("SSE: {e}"))?;
            if event.data.trim() == "[DONE]" {
                break;
            }
            let Ok(v) = serde_json::from_str::<Value>(&event.data) else {
                continue;
            };
            if let Some(delta) = v
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
            {
                if delta.is_empty() {
                    continue;
                }
                acc.push_str(delta);
                pending.push_str(delta);
                // Emit everything except a possibly-unfinished sentinel.
                let safe = safe_stream_prefix(&pending);
                if safe > 0 {
                    let out = restore_code(&pending[..safe], &blocks);
                    pending.drain(..safe);
                    if !out.is_empty() {
                        let _ = channel.send(TranslateEvent::Delta {
                            request_id: request_id.to_string(),
                            text: out,
                        });
                    }
                }
            }
        }
        if !pending.is_empty() {
            let out = restore_code(&pending, &blocks);
            if !out.is_empty() {
                let _ = channel.send(TranslateEvent::Delta {
                    request_id: request_id.to_string(),
                    text: out,
                });
            }
        }
        if acc.is_empty() {
            return Err("chat 返回为空".into());
        }
        Ok(restore_code(&acc, &blocks))
    } else {
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("chat decode: {e}"))?;
        let content = v
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or("chat 响应缺少 choices[0].message.content")?
            .to_string();
        let full = restore_code(&content, &blocks);
        if !full.is_empty() {
            let _ = channel.send(TranslateEvent::Delta {
                request_id: request_id.to_string(),
                text: full.clone(),
            });
        }
        Ok(full)
    }
}

/// `https://api.example.com[/v1]` → `https://api.example.com/v1/chat/completions`;
/// a full `.../chat/completions` URL is used as-is.
pub fn normalize_chat_url(base: &str) -> Result<String, String> {
    let mut url = base.trim().trim_end_matches('/').to_string();
    if url.is_empty() {
        return Err("Base URL 为空".into());
    }
    if url.ends_with("/chat/completions") {
        return Ok(url);
    }
    if let Some(stripped) = url.strip_suffix("/v1") {
        url = stripped.to_string();
    }
    Ok(format!("{url}/v1/chat/completions"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_normalization() {
        assert_eq!(
            normalize_chat_url("https://api.example.com").unwrap(),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            normalize_chat_url("https://api.example.com/").unwrap(),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            normalize_chat_url("https://api.example.com/v1").unwrap(),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            normalize_chat_url("https://api.example.com/v1/").unwrap(),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            normalize_chat_url("https://api.example.com/v1/chat/completions").unwrap(),
            "https://api.example.com/v1/chat/completions"
        );
        assert!(normalize_chat_url("").is_err());
    }
}
