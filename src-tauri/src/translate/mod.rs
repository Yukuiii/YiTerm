pub mod chat;
pub mod google;

use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;

use crate::settings::TranslationSettings;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Direction {
    #[serde(rename = "zh-en")]
    ZhToEn,
    #[serde(rename = "en-zh")]
    EnToZh,
}

impl Direction {
    pub fn target(&self, settings: &TranslationSettings) -> String {
        match self {
            Direction::ZhToEn => "en".to_string(),
            Direction::EnToZh => settings.target_zh.clone(),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TranslateEvent {
    /// Incremental text appended since the previous delta.
    Delta { request_id: String, text: String },
    /// Terminal state; carries the full translation.
    Done { request_id: String, text: String },
    Error { request_id: String, message: String },
}

/// True if the string contains Han / Hiragana / Katakana / Hangul.
/// (The frontend does its own detection; kept for server-side use and tests.)
#[allow(dead_code)]
pub fn has_cjk(s: &str) -> bool {
    s.chars().any(is_cjk_char)
}

#[allow(dead_code)]
pub fn is_cjk_char(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{309F}'   // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
        | '\u{3400}'..='\u{4DBF}' // CJK Extension A
        | '\u{4E00}'..='\u{9FFF}' // CJK Unified
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility
        | '\u{AC00}'..='\u{D7AF}' // Hangul Syllables
        | '\u{1100}'..='\u{11FF}' // Hangul Jamo
    )
}

/// Splits text into chunks of at most `max` runes, preferring whitespace /
/// newline boundaries (ported from the proven Go proxy implementation).
pub fn chunk(text: &str, max: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut runes: Vec<char> = text.chars().collect();
    while runes.len() > max {
        let mut cut = max;
        for i in (0..max).rev() {
            if runes[i] == '\n' || runes[i] == ' ' {
                cut = i + 1; // boundary char stays with this chunk
                break;
            }
        }
        chunks.push(runes[..cut].iter().collect());
        runes = runes[cut..].to_vec();
    }
    if !runes.is_empty() {
        chunks.push(runes.iter().collect());
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

// ---------- code protection ----------
//
// Machine translation mangles code (identifiers, punctuation) and the
// translateHtml endpoint flattens newlines. Before translating we swap every
// fenced block and inline code span for a `@@C<n>@@` sentinel (Google passes
// those through verbatim), then restore them in the output.

/// A span swapped out for a `@@C<n>@@` sentinel. `fenced` marks multi-line
/// fenced code blocks, whose fence lines must land on line boundaries when
/// restored (a fence glued to surrounding text is no longer a fence).
pub struct ProtectedBlock {
    pub text: String,
    pub fenced: bool,
}

/// Replaces fenced code blocks and inline code spans with sentinels.
/// Returns the protected text plus the original spans (index = sentinel id).
pub fn protect_code(input: &str) -> (String, Vec<ProtectedBlock>) {
    let mut blocks: Vec<ProtectedBlock> = Vec::new();
    let mut out = String::with_capacity(input.len());
    // Opening fence: character and run length. A fence closes only on a line
    // with the same character and at least as long a run (CommonMark), so a
    // ``` line inside a ```` block stays content.
    let mut fence: Option<(u8, usize)> = None;
    let mut fence_buf = String::new();

    for line in input.split_inclusive('\n') {
        if let Some((ch, len)) = fence {
            let closes = fence_run(line)
                .is_some_and(|(c, n, rest)| c == ch && n >= len && rest.trim().is_empty());
            if closes {
                fence_buf.push_str(line);
                out.push_str(&format!("@@C{}@@\n", blocks.len()));
                blocks.push(ProtectedBlock {
                    text: fence_buf.trim_end_matches('\n').to_string(),
                    fenced: true,
                });
                fence_buf.clear();
                fence = None;
            } else {
                fence_buf.push_str(line);
            }
        } else if let Some((ch, len, _)) = fence_run(line) {
            fence = Some((ch, len));
            fence_buf.clear();
            fence_buf.push_str(line);
        } else {
            out.push_str(&protect_inline_code(line, &mut blocks));
        }
    }
    if fence.is_some() {
        // Unterminated fence: keep verbatim rather than translate code as prose.
        out.push_str(&fence_buf);
    }
    (out, blocks)
}

/// Returns `(character, run length, rest of line)` when `line` starts with an
/// opening fence: three or more backticks or tildes, optionally indented.
fn fence_run(line: &str) -> Option<(u8, usize, &str)> {
    let trimmed = line.trim_start();
    let ch = *trimmed.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = trimmed.bytes().take_while(|&b| b == ch).count();
    (len >= 3).then(|| (ch, len, &trimmed[len..]))
}

fn protect_inline_code(line: &str, blocks: &mut Vec<ProtectedBlock>) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('`') {
            Some(end) => {
                let span = &rest[start..start + 1 + end + 1];
                out.push_str(&format!("@@C{}@@", blocks.len()));
                blocks.push(ProtectedBlock {
                    text: span.to_string(),
                    fenced: false,
                });
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(rest);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Restores sentinels in a translated fragment. Translators (notably Google's)
/// collapse the whitespace around sentinels, so fenced blocks are re-anchored
/// to their own lines; otherwise a fence merges into the surrounding prose and
/// markdown stops recognising it (a trailing lone fence renders as an empty
/// code block).
pub fn restore_code(translated: &str, blocks: &[ProtectedBlock]) -> String {
    let mut out = String::with_capacity(translated.len());
    let mut rest = translated;
    while let Some(start) = rest.find("@@C") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !after[digits..].starts_with("@@") {
            // A literal "@@C" that is not one of our sentinels.
            out.push_str("@@C");
            rest = after;
            continue;
        }
        let block = after[..digits]
            .parse::<usize>()
            .ok()
            .and_then(|i| blocks.get(i));
        match block {
            Some(block) => {
                if block.fenced {
                    while out.ends_with(' ') || out.ends_with('\t') {
                        out.pop();
                    }
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&block.text);
                    if !block.text.ends_with('\n') {
                        out.push('\n');
                    }
                } else {
                    out.push_str(&block.text);
                }
                rest = &after[digits + 2..];
            }
            None => {
                // Unknown id: leave the token untouched.
                out.push_str(&rest[start..start + 3 + digits + 2]);
                rest = &after[digits + 2..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Position up to which `s` can be flushed during streaming without risking a
/// split sentinel (a `@@C<digits>@@` token that may still complete next delta).
pub fn safe_stream_prefix(s: &str) -> usize {
    if let Some(rel) = s.rfind("@@") {
        let tail = &s[rel..];
        if tail == "@@" {
            // Either a complete sentinel's closing pair, or a fresh opening.
            let before = &s[..rel];
            if let Some(rel2) = before.rfind("@@C") {
                let mid = &before[rel2 + 3..];
                let digits = mid.bytes().take_while(u8::is_ascii_digit).count();
                if digits > 0 && digits == mid.len() {
                    return s.len(); // complete "@@C<n>@@" — flush everything
                }
            }
            return rel; // opening "@@" — hold
        }
        if let Some(rest) = tail.strip_prefix("@@C") {
            if rest.is_empty() {
                return rel; // "@@C" — may still grow
            }
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return s.len(); // "@@C" + non-digit → literal text
            }
            let after_digits = &rest[digits..];
            if after_digits.is_empty() {
                return rel; // "@@C123" — closing may come next
            }
            if after_digits.starts_with("@@") {
                return s.len(); // complete sentinel
            }
            return rel; // malformed — hold to be safe
        }
    }
    if s.ends_with('@') {
        return s.len() - 1;
    }
    s.len()
}

/// Dispatches one translation request to the configured provider, streaming
/// events into `channel` and returning the final full text.
pub async fn run(
    http: &reqwest::Client,
    settings: &TranslationSettings,
    direction: Direction,
    text: &str,
    request_id: &str,
    channel: &Channel<TranslateEvent>,
) -> Result<String, String> {
    let emit_error = |message: String| -> Result<String, String> {
        let _ = channel.send(TranslateEvent::Error {
            request_id: request_id.to_string(),
            message: message.clone(),
        });
        Err(message)
    };

    if text.trim().is_empty() {
        return Ok(String::new());
    }

    let result = match settings.mode.as_str() {
        "chat" => {
            let api_key = crate::settings::api_key_get().unwrap_or_default();
            let base = settings
                .chat_base_url
                .clone()
                .unwrap_or_default()
                .trim()
                .to_string();
            let model = settings
                .chat_model_id
                .clone()
                .unwrap_or_default()
                .trim()
                .to_string();
            if base.is_empty() || model.is_empty() {
                return emit_error("Chat 模式未配置完整（需要 Base URL 和模型 ID）".into());
            }
            chat::translate(
                http,
                &base,
                &model,
                if api_key.is_empty() { None } else { Some(&api_key) },
                direction,
                text,
                request_id,
                channel,
            )
            .await
        }
        _ => {
            let target = direction.target(settings);
            google::translate(http, &target, text, request_id, channel).await
        }
    };

    match result {
        Ok(full) => {
            tracing::info!(
                mode = %settings.mode,
                direction = ?direction,
                in_chars = text.chars().count(),
                out_chars = full.chars().count(),
                "translate ok"
            );
            let _ = channel.send(TranslateEvent::Done {
                request_id: request_id.to_string(),
                text: full.clone(),
            });
            Ok(full)
        }
        Err(e) => {
            tracing::warn!(mode = %settings.mode, direction = ?direction, error = %e, "translate failed");
            emit_error(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_detection() {
        assert!(has_cjk("你好"));
        assert!(has_cjk("こんにちは"));
        assert!(has_cjk("안녕"));
        assert!(has_cjk("mixed 中文 text"));
        assert!(!has_cjk("plain english 123 !@#"));
        assert!(!has_cjk(""));
    }

    #[test]
    fn chunk_respects_boundaries() {
        let text = "a".repeat(1000);
        let chunks = chunk(&text, 800);
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|c| c.chars().count() <= 800));

        let words = "word ".repeat(300); // 1500 chars, boundaries every 5
        let chunks = chunk(&words, 800);
        assert!(chunks.iter().all(|c| c.chars().count() <= 800));
        assert!(chunks.len() >= 2);
        // Every chunk except the last should end at a space boundary.
        for c in &chunks[..chunks.len() - 1] {
            assert!(c.ends_with(' '));
        }

        let cjk = "你".repeat(2000);
        let chunks = chunk(&cjk, 800);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.iter().map(|c| c.chars().count()).sum::<usize>(), 2000);
    }

    #[test]
    fn chunk_small_and_empty() {
        assert_eq!(chunk("hi", 800), vec!["hi".to_string()]);
        assert_eq!(chunk("", 800), vec![String::new()]);
    }

    #[test]
    fn protects_fenced_and_inline_code() {
        let input = "## 标题\n\n使用 `npm run build`：\n\n```js\nconst x = 1;\n```\n\n完成。";
        let (protected, blocks) = protect_code(input);
        assert!(!protected.contains("const x"));
        assert!(!protected.contains("npm run build"));
        assert!(protected.contains("@@C0@@"));
        assert!(protected.contains("@@C1@@"));
        assert_eq!(blocks.len(), 2); // inline first, then fence (scan order)
        assert!(blocks.iter().any(|b| b.text.contains("npm run build")));
        assert!(blocks.iter().any(|b| b.text.contains("const x = 1;")));
        assert!(protected.contains("## 标题"));

        let restored = restore_code(&restored_probe(&protected), &blocks);
        assert!(restored.contains("const x = 1;"));
        assert!(restored.contains("`npm run build`"));
    }

    #[test]
    fn longer_fence_contains_shorter_fences() {
        let input =
            "intro\n\n````markdown\n# Title\n\n```text\ndiagram\n```\n\n```bash\nls\n```\n````\n";
        let (protected, blocks) = protect_code(input);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].fenced);
        assert!(blocks[0].text.starts_with("````markdown"));
        assert!(blocks[0].text.ends_with("````"));
        assert!(!protected.contains("diagram"));
        assert!(!protected.contains("ls"));
        assert_eq!(protected, "intro\n\n@@C0@@\n");
    }

    #[test]
    fn closing_fence_needs_same_char_and_length() {
        // A tilde fence does not close a backtick fence.
        let (_, blocks) = protect_code("```\na\n~~~\nb\n```\n");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].text.contains("~~~"));

        // Three backticks do not close a four-backtick fence.
        let (_, blocks) = protect_code("````\na\n```\nb\n````\n");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].text.contains("```\nb"));
    }

    #[test]
    fn restore_anchors_fences_to_their_own_lines() {
        // Google collapses the newline before a sentinel. Without re-anchoring,
        // the opening fence merges into the intro and the trailing fence opens
        // an empty code block in the rendered markdown.
        let input = "以下是内容：\n\n````markdown\n# hi\n````";
        let (protected, blocks) = protect_code(input);
        assert_eq!(protected, "以下是内容：\n\n@@C0@@\n");
        let restored = restore_code("以下是内容： @@C0@@", &blocks);
        assert_eq!(restored, "以下是内容：\n````markdown\n# hi\n````\n");
    }

    #[test]
    fn restore_keeps_inline_code_inline() {
        let (_, blocks) = protect_code("use `cargo test` now");
        let restored = restore_code("使用 @@C0@@ 现在", &blocks);
        assert_eq!(restored, "使用 `cargo test` 现在");
    }

    fn restored_probe(s: &str) -> String {
        // pretend the translator returned the protected text unchanged
        s.to_string()
    }

    #[test]
    fn unterminated_fence_kept_verbatim() {
        let input = "before\n```js\nconst a = 1;";
        let (protected, blocks) = protect_code(input);
        assert!(protected.contains("const a = 1;"));
        assert!(blocks.is_empty());
    }

    #[test]
    fn stream_prefix_holds_partial_sentinels() {
        // Nothing pending → flush all.
        assert_eq!(safe_stream_prefix("hello"), 5);
        // Opening characters are held back.
        assert_eq!(safe_stream_prefix("hi @@"), 3);
        assert_eq!(safe_stream_prefix("hi @@C"), 3);
        assert_eq!(safe_stream_prefix("hi @@C12"), 3);
        assert_eq!(safe_stream_prefix("hi @@C12@"), 3);
        // Complete sentinel flushes through.
        assert_eq!(safe_stream_prefix("hi @@C12@@"), 10);
        assert_eq!(safe_stream_prefix("hi @@C12@@ tail"), 15);
        // Lone trailing '@' is held.
        assert_eq!(safe_stream_prefix("hi @"), 3);
    }
}
