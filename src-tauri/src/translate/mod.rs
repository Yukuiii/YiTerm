pub mod chat;
pub mod deepl;
pub mod google;
pub mod local;
pub mod oneshot;

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
    Delta {
        request_id: String,
        text: String,
    },
    /// Terminal state; carries the full translation.
    Done {
        request_id: String,
        text: String,
    },
    Error {
        request_id: String,
        message: String,
    },
}

/// Streams one translated fragment to the frontend. A closed channel only means
/// nobody is listening any more, which is not a translation failure.
pub(crate) fn emit_delta(
    channel: &Channel<TranslateEvent>,
    request_id: &str,
    text: impl Into<String>,
) {
    let _ = channel.send(TranslateEvent::Delta {
        request_id: request_id.to_string(),
        text: text.into(),
    });
}

/// Reports a failed translation on the channel and returns it as the command's
/// error, so both the stream and the awaited `invoke` carry the message.
pub(crate) fn emit_error(
    channel: &Channel<TranslateEvent>,
    request_id: &str,
    message: String,
) -> Result<String, String> {
    let _ = channel.send(TranslateEvent::Error {
        request_id: request_id.to_string(),
        message: message.clone(),
    });
    Err(message)
}

/// Start of a failed response body, for provider error messages.
pub(crate) async fn error_body(resp: reqwest::Response, max_chars: usize) -> String {
    resp.text()
        .await
        .unwrap_or_default()
        .chars()
        .take(max_chars)
        .collect()
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

/// Splits text into chunks of at most `max` runes. Cuts prefer paragraph or
/// sentence boundaries, so a sentence is not split when a boundary is
/// available; clause punctuation and whitespace are fallbacks, and only an
/// unpunctuated run is cut mid-text.
pub fn chunk(text: &str, max: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let runes: Vec<char> = text.chars().collect();
    let mut remaining = runes.as_slice();
    while remaining.len() > max {
        let cut = chunk_cut(remaining, max);
        chunks.push(remaining[..cut].iter().collect());
        remaining = &remaining[cut..];
    }
    if !remaining.is_empty() {
        chunks.push(remaining.iter().collect());
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

/// Like [`chunk`], but `max` is measured in UTF-16 code units — the limit the
/// anonymous DeepL endpoint and DeepLX-style gateways enforce (an astral
/// character counts as two). Cuts still prefer sentence boundaries, but never
/// overshoot the cap: a rejected request would fail the whole translation.
pub fn chunk_utf16(text: &str, max: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut units = 0;
        let mut end = start;
        while end < chars.len() {
            let next = chars[end].len_utf16();
            if units + next > max {
                break;
            }
            units += next;
            end += 1;
        }
        if end == chars.len() {
            chunks.push(chars[start..].iter().collect());
            break;
        }
        // Best boundary before the cap; `end` itself when the window holds no
        // punctuation or whitespace. Unlike `chunk_cut` this never overshoots.
        // Always advance to make progress.
        let cut = best_boundary(&chars, start, end)
            .map_or(end, |(_, cut)| cut)
            .max(start + 1);
        chunks.push(chars[start..cut].iter().collect());
        start = cut;
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

/// Rightmost boundary of the best available priority in `[start, end)`, as
/// `(priority, cut)`; the cut never passes `end`. The first few characters are
/// skipped to avoid pathological one-word chunks.
fn best_boundary(chars: &[char], start: usize, end: usize) -> Option<(u8, usize)> {
    let floor = 16.min(end - start);
    let mut best: Option<(u8, usize)> = None; // lower priority wins
    for i in (start + floor..end).rev() {
        let Some((prio, cut)) = boundary_at(chars, i, end) else {
            continue;
        };
        if best.is_none_or(|(p, _)| prio < p) {
            best = Some((prio, cut));
            if prio == 1 {
                break; // nothing better exists
            }
        }
    }
    best
}

/// Cut index for one chunk: the rightmost newline or sentence end in the
/// window; failing that, a sentence end a little past the limit (so a long
/// sentence still ends cleanly), then a clause boundary or whitespace.
fn chunk_cut(chars: &[char], max: usize) -> usize {
    let window = max.min(chars.len());
    let best = best_boundary(chars, 0, window);
    if best.is_none_or(|(p, _)| p > 1) {
        // No sentence boundary fits: allow the chunk to overshoot by up to
        // half a window to finish the sentence, rather than breaking it at
        // a clause or a space.
        let slack_end = (window + max / 2).min(chars.len());
        for i in window..slack_end {
            if let Some((1, cut)) = boundary_at(chars, i, slack_end) {
                return cut;
            }
        }
    }
    best.map_or(window, |(_, cut)| cut)
}

/// Classifies one boundary character. Priorities: 1 = newline / sentence
/// end, 2 = clause punctuation, 3 = whitespace. The cut consumes trailing
/// closing quotes and spaces so the next chunk starts clean.
fn boundary_at(chars: &[char], i: usize, window: usize) -> Option<(u8, usize)> {
    let c = chars[i];
    if c == '\n' {
        return Some((1, i + 1));
    }
    if is_sentence_end(c) {
        let after = skip_closing(chars, i + 1);
        // ASCII terminators only count when the sentence really ends here
        // (not inside "3.14" or "a.b").
        if c.is_ascii() && !chars.get(after).is_none_or(|n| n.is_whitespace()) {
            return None;
        }
        return Some((1, skip_spaces(chars, after).min(window)));
    }
    if is_clause(c) {
        let after = skip_closing(chars, i + 1);
        // Likewise for "12:30" and "a,b".
        if c.is_ascii() && !chars.get(after).is_none_or(|n| n.is_whitespace()) {
            return None;
        }
        return Some((2, skip_spaces(chars, after).min(window)));
    }
    if c == ' ' || c == '\t' {
        return Some((3, i + 1));
    }
    None
}

fn is_sentence_end(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '…' | '.' | '!' | '?')
}

fn is_clause(c: char) -> bool {
    matches!(c, '，' | '、' | '；' | '：' | ',' | ';' | ':')
}

fn skip_closing(chars: &[char], mut j: usize) -> usize {
    while j < chars.len()
        && matches!(
            chars[j],
            '"' | '”' | '’' | '」' | '』' | '）' | ')' | ']' | '】' | '》'
        )
    {
        j += 1;
    }
    j
}

fn skip_spaces(chars: &[char], mut j: usize) -> usize {
    while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
        j += 1;
    }
    j
}

// ---------- code protection ----------
//
// Machine translation mangles code (identifiers, punctuation) and the
// translateHtml endpoint flattens newlines. Before translating we swap every
// fenced block and inline code span for a `@@C<n>@@` sentinel (Google passes
// those through verbatim), then restore them in the output.

/// What a sentinel stood in for; drives how it is restored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// Inline code span — restored verbatim in place.
    Inline,
    /// Fenced code block — fence lines must land on line boundaries (a fence
    /// glued to surrounding text is no longer a fence).
    Fenced,
    /// A paragraph break (blank-line run). Translation models collapse these,
    /// so the break is swapped out like code and re-anchored on restore.
    Break,
}

/// A span swapped out for a `@@C<n>@@` sentinel.
pub struct ProtectedBlock {
    pub text: String,
    pub kind: BlockKind,
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
                    kind: BlockKind::Fenced,
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

/// Swaps blank-line runs (paragraph breaks) for sentinels, appending the
/// originals to `blocks`. Sequence: call after [`protect_code`] so blank
/// lines inside fenced blocks are already hidden.
///
/// Local MT models collapse paragraph breaks into a single newline, merging
/// markdown paragraphs; the sentinel survives translation and [`restore_code`]
/// re-anchors one blank line.
pub fn protect_breaks(input: &str, blocks: &mut Vec<ProtectedBlock>) -> String {
    let b = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut last = 0; // start of the slice not yet flushed to `out`
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\n' {
            i += 1;
            continue;
        }
        // Match a run of two or more newlines separated only by blanks.
        // The indentation of the following line is left untouched.
        let mut newlines = 1;
        let mut end = i + 1;
        let mut j = i + 1;
        loop {
            let mut k = j;
            while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
                k += 1;
            }
            if k < b.len() && b[k] == b'\n' {
                newlines += 1;
                end = k + 1;
                j = k + 1;
            } else {
                break;
            }
        }
        if newlines >= 2 {
            out.push_str(&input[last..i]);
            out.push(' ');
            out.push_str(&format!("@@C{}@@", blocks.len()));
            out.push(' ');
            blocks.push(ProtectedBlock {
                text: "\n\n".to_string(),
                kind: BlockKind::Break,
            });
            last = end;
            i = end;
        } else {
            i += 1;
        }
    }
    out.push_str(&input[last..]);
    out
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
                    kind: BlockKind::Inline,
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
                match block.kind {
                    BlockKind::Fenced => {
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
                    }
                    BlockKind::Break => {
                        // The model pads sentinels with spaces on both sides
                        // and turns "\n\n" into its own line break; re-anchor
                        // one blank line and drop the padding. Always emit
                        // the full blank line: during streaming the text
                        // before the sentinel may already have been flushed
                        // in an earlier delta (leaving `out` empty here).
                        while out.ends_with(' ') || out.ends_with('\t') {
                            out.pop();
                        }
                        if !out.ends_with('\n') {
                            out.push('\n');
                        }
                        out.push('\n');
                        rest = &after[digits + 2..];
                        rest = rest.trim_start_matches([' ', '\t']);
                        continue;
                    }
                    BlockKind::Inline => {
                        out.push_str(&block.text);
                    }
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

/// Restores sentinels in a translation that arrives as a stream of raw deltas.
/// Text that may still be an unfinished sentinel is held back until it either
/// completes or the stream ends.
pub(crate) struct StreamRestorer<'a> {
    blocks: &'a [ProtectedBlock],
    raw: String,
    pending: String,
}

impl<'a> StreamRestorer<'a> {
    pub fn new(blocks: &'a [ProtectedBlock]) -> Self {
        Self {
            blocks,
            raw: String::new(),
            pending: String::new(),
        }
    }

    /// Feeds one raw delta; returns the restored text that is safe to show.
    pub fn push(&mut self, delta: &str) -> Option<String> {
        self.raw.push_str(delta);
        self.pending.push_str(delta);
        let safe = safe_stream_prefix(&self.pending);
        if safe == 0 {
            return None;
        }
        let out = restore_code(&self.pending[..safe], self.blocks);
        self.pending.drain(..safe);
        (!out.is_empty()).then_some(out)
    }

    /// Releases whatever was held back once the stream has ended.
    pub fn flush(&mut self) -> Option<String> {
        let out = restore_code(&std::mem::take(&mut self.pending), self.blocks);
        (!out.is_empty()).then_some(out)
    }

    /// True when the provider produced no text at all.
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// The whole translation restored in one pass. Unlike the concatenated
    /// deltas this is exact, so it is what callers return and persist.
    pub fn full(&self) -> String {
        restore_code(&self.raw, self.blocks)
    }
}

/// Dispatches one translation request to the configured provider, streaming
/// events into `channel` and returning the final full text.
pub async fn run(
    http: &reqwest::Client,
    settings: &TranslationSettings,
    api_key: Option<&str>,
    direction: Direction,
    text: &str,
    request_id: &str,
    channel: &Channel<TranslateEvent>,
) -> Result<String, String> {
    if text.trim().is_empty() {
        return Ok(String::new());
    }

    let result = match settings.mode.as_str() {
        "chat" => {
            let base = settings.chat_base_url.as_deref().unwrap_or_default().trim();
            let model = settings.chat_model_id.as_deref().unwrap_or_default().trim();
            if base.is_empty() || model.is_empty() {
                return emit_error(
                    channel,
                    request_id,
                    "Chat 模式未配置完整（需要 Base URL 和模型 ID）".into(),
                );
            }
            chat::translate(
                http,
                base,
                model,
                api_key.filter(|key| !key.is_empty()),
                direction,
                text,
                request_id,
                channel,
            )
            .await
        }
        "dlx" => oneshot::translate(http, direction, text, request_id, channel).await,
        "deepl" => {
            let protocol = deepl::protocol(settings.deepl_protocol.as_deref());
            deepl::translate(
                http,
                api_key.unwrap_or_default(),
                settings.deepl_api_url.as_deref(),
                protocol,
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
            emit_error(channel, request_id, e)
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
        assert_eq!(
            chunks.iter().map(|c| c.chars().count()).sum::<usize>(),
            2000
        );
    }

    #[test]
    fn chunk_preserves_unicode_and_newlines_across_multiple_chunks() {
        let paragraphs: Vec<String> = ["😀", "🦀", "🚀", "🙂"]
            .iter()
            .map(|emoji| format!("{} {emoji}\n", "段".repeat(20)))
            .collect();
        let text = paragraphs.concat();
        assert_eq!(chunk(&text, 24), paragraphs);
    }

    #[test]
    fn chunk_small_and_empty() {
        assert_eq!(chunk("hi", 800), vec!["hi".to_string()]);
        assert_eq!(chunk("", 800), vec![String::new()]);
    }

    #[test]
    fn chunk_does_not_split_sentences() {
        // CJK: every cut lands right after a sentence terminator.
        let zh = "这是第一句话。这是第二句话！这是第三句话？".repeat(40);
        let chunks = chunk(&zh, 50);
        assert!(chunks.len() > 2);
        for c in &chunks[..chunks.len() - 1] {
            let last = c.chars().last().unwrap();
            assert!(
                matches!(last, '。' | '！' | '？'),
                "chunk ends mid-sentence: {c:?}"
            );
        }

        // English: same, accounting for the trailing space the cut may include.
        let en =
            "This is one sentence. This is another sentence! Is this a third? Yes. ".repeat(30);
        let chunks = chunk(&en, 60);
        assert!(chunks.len() > 2);
        for c in &chunks[..chunks.len() - 1] {
            let end = c.trim_end();
            assert!(
                matches!(end.chars().last(), Some('.' | '!' | '?')),
                "chunk ends mid-sentence: {c:?}"
            );
        }
    }

    #[test]
    fn chunk_falls_back_to_clauses_then_whitespace() {
        // No sentence terminators: clause punctuation is the best cut.
        let clauses = "这是一个很长的句子，它没有句号只有逗号，而且还在继续，".repeat(30);
        let chunks = chunk(&clauses, 60);
        for c in &chunks[..chunks.len() - 1] {
            assert!(c.trim_end().ends_with('，'), "expected a clause cut: {c:?}");
        }

        // Only spaces: cut at a space rather than mid-word.
        let words = "word ".repeat(40);
        let chunks = chunk(&words, 60);
        for c in &chunks[..chunks.len() - 1] {
            assert!(c.ends_with(' '), "expected a whitespace cut: {c:?}");
        }
    }

    #[test]
    fn chunk_extends_past_the_limit_to_finish_a_long_sentence() {
        // One sentence longer than the 800-char limit: cut at its end, even
        // though that overshoots, instead of breaking at a space.
        let text = format!("{} tail.", "word ".repeat(200));
        let chunks = chunk(&text, 800);
        assert!(chunks[0].chars().count() > 800);
        assert!(chunks[0].trim_end().ends_with('.'));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn chunk_ignores_periods_inside_words() {
        // "1.2.3" must not look like a sentence end.
        let text = "1.2.3 ".repeat(300);
        let chunks = chunk(&text, 800);
        for c in &chunks[..chunks.len() - 1] {
            assert!(c.ends_with(' '), "cut inside a dotted token: {c:?}");
        }
    }

    #[test]
    fn chunk_utf16_counts_surrogate_pairs() {
        // 1000 emoji = 2000 UTF-16 units, but only 1000 chars.
        let text = "😀".repeat(1000);
        let chunks = chunk_utf16(&text, 1400);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks.concat(), text);
        for c in &chunks {
            assert!(c.encode_utf16().count() <= 1400, "chunk over the cap");
        }
    }

    #[test]
    fn chunk_utf16_never_overshoots_and_prefers_sentences() {
        let zh = "这是第一句话。这是第二句话！这是第三句话？".repeat(40);
        let chunks = chunk_utf16(&zh, 50);
        assert!(chunks.len() > 2);
        for c in &chunks {
            assert!(c.encode_utf16().count() <= 50, "chunk over the cap: {c:?}");
        }
        assert_eq!(chunks.concat(), zh);
        for c in &chunks[..chunks.len() - 1] {
            let last = c.chars().last().unwrap();
            assert!(
                matches!(last, '。' | '！' | '？'),
                "chunk ends mid-sentence: {c:?}"
            );
        }
    }

    #[test]
    fn chunk_utf16_handles_empty_and_short_text() {
        assert_eq!(chunk_utf16("", 1400), vec![String::new()]);
        assert_eq!(chunk_utf16("hi", 1400), vec!["hi".to_string()]);
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
        assert_eq!(blocks[0].kind, BlockKind::Fenced);
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
    fn break_sentinels_roundtrip_paragraphs() {
        let input = "第一段。\n\n第二段。\n\n```js\ncode\n```\n\n第三段。";
        let (protected, mut blocks) = protect_code(input);
        let protected = protect_breaks(&protected, &mut blocks);
        // No raw blank lines survive for the model to collapse.
        assert!(!protected.contains("\n\n"));
        assert_eq!(restore_code(&protected, &blocks), input);
    }

    #[test]
    fn break_sentinels_tolerate_model_padding() {
        let input = "Start.\n\nEnd.";
        let (protected, mut blocks) = protect_code(input);
        let protected = protect_breaks(&protected, &mut blocks);
        // The model likes to pad sentinels with spaces and may split text
        // across deltas; restoring must still yield exactly one blank line.
        let model_out = protected.replace("@@C0@@", "  @@C0@@  ");
        assert_eq!(restore_code(&model_out, &blocks), "Start.\n\nEnd.");

        // When the text before the sentinel was already flushed in an
        // earlier delta, its trailing model padding can't be trimmed any
        // more — the streamed text keeps a cosmetic space, while the final
        // full restore (what gets persisted) is exact.
        let (before, after) = model_out.split_once("@@C0@@").unwrap();
        let mut streamed = restore_code(before, &blocks);
        streamed.push_str(&restore_code(&format!("@@C0@@{after}"), &blocks));
        let words: Vec<&str> = streamed.split_whitespace().collect();
        assert_eq!(words, ["Start.", "End."]);
    }

    #[test]
    fn protect_breaks_keeps_single_newlines_and_indentation() {
        let input = "line one\nline two\n\n  indented\n";
        let mut blocks = Vec::new();
        let out = protect_breaks(input, &mut blocks);
        assert_eq!(blocks.len(), 1);
        assert!(out.contains("line one\nline two"));
        assert!(out.contains("  indented"));
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

    #[test]
    fn stream_restorer_holds_a_split_sentinel_until_it_completes() {
        let (protected, blocks) = protect_code("use `cargo test` now");
        assert_eq!(protected, "use @@C0@@ now");
        let mut restorer = StreamRestorer::new(&blocks);
        assert!(restorer.is_empty());

        // The sentinel arrives across three deltas; nothing partial is shown.
        let mut shown = String::new();
        for (delta, expected) in [
            ("使用 @@", Some("使用 ")),
            ("C0", None),
            ("@@ 现在 @", Some("`cargo test` 现在 ")),
        ] {
            let out = restorer.push(delta);
            assert_eq!(out.as_deref(), expected, "after {delta:?}");
            shown.push_str(&out.unwrap_or_default());
        }

        // The trailing '@' could have opened a sentinel; it is released at the end.
        assert_eq!(restorer.flush().as_deref(), Some("@"));
        assert_eq!(restorer.flush(), None);
        assert!(!restorer.is_empty());
        assert_eq!(restorer.full(), "使用 `cargo test` 现在 @");
        assert_eq!(format!("{shown}@"), restorer.full());
    }
}
