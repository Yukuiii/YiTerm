//! Thin wrapper over [`tokenizers::Tokenizer`] adding Hunyuan-MT special
//! tokens and a translate-oriented chat template.
//!
//! Two tokenizer / chat-template lineages are supported, gated by
//! [`crate::model::TokenizerFamily`]:
//!
//! - **V1** — Hy-MT 1.5 and Hy-MT2 1.8B. Special tokens use **full-width
//!   vertical bars** `｜` (U+FF5C). Chat template embeds explicit user /
//!   assistant prefix tokens:
//!
//!   ```text
//!   <｜hy_begin▁of▁sentence｜><｜hy_User｜>{user}<｜hy_Assistant｜>
//!   ```
//!
//! - **V2** — Hy-MT2 7B. Special tokens use ASCII `|`. The chat template is
//!   suffix-based: no explicit user / assistant prefix markers, just a
//!   user-end separator after the input:
//!
//!   ```text
//!   <|startoftext|>{user}<|extra_0|>
//!   ```
//!
//! Because the `tokenizers` crate's `encode(...)` byte-pair-merges the input
//! string and would split `<｜hy_User｜>` or `<|extra_0|>` into many tokens,
//! we look the special tokens up by name and concatenate IDs directly.

use std::path::Path;

use tokenizers::models::bpe::{Vocab, BPE};
use tokenizers::pre_tokenizers::byte_level::ByteLevel;
use tokenizers::{AddedToken, Tokenizer};

use crate::gguf::meta::Meta;
use crate::gguf::vendored::{Content, Value};
use crate::model::TokenizerFamily;
use crate::{Error, Result};

/// Conventional special-token IDs for Hunyuan-MT 1.5 (cross-checked against
/// the GGUF metadata and tokenizer.json `added_tokens` list). Used as
/// fallbacks when the tokenizer doesn't expose the canonical names.
pub const BOS_ID: u32 = 120_000;
pub const EOS_ID: u32 = 120_001;
/// Marker emitted by the assistant turn — also what the V1 GGUF declares as
/// "EOS" and what we use to terminate generation.
pub const ASSISTANT_TURN_END: u32 = 120_020;
pub const PAD_ID: u32 = 120_002;

// V1 (Hy-MT 1.5 + Hy-MT2-1.8B) — full-width pipes.
const V1_TOK_BOS: &str = "<｜hy_begin▁of▁sentence｜>";
const V1_TOK_USER: &str = "<｜hy_User｜>";
const V1_TOK_ASSISTANT: &str = "<｜hy_Assistant｜>";
const V1_TOK_SYSTEM_END: &str = "<｜hy_place▁holder▁no▁3｜>";
const V1_TOK_TURN_END: &str = "<｜hy_place▁holder▁no▁2｜>";

// V2 (Hy-MT2-7B) — ASCII pipes. Names mirror the `hy_dense_7b` template
// registered in train/llama_factory_support/hy_dense_template.py.
const V2_TOK_BOS: &str = "<|startoftext|>";
const V2_TOK_USER_END: &str = "<|extra_0|>"; // closes the user turn
const V2_TOK_SYSTEM_END: &str = "<|extra_4|>";
const V2_TOK_EOS: &str = "<|eos|>";

/// Per-family chat-template state — strings are looked up at construction
/// time and resolved IDs are stored alongside so the hot path doesn't need
/// to ask the tokenizer again.
#[derive(Debug, Clone)]
enum ChatTemplate {
    /// `<bos>[{system}<sys_end>]<user_pre>{user}<asst_pre>` — Hy-MT 1.5 / 2-1.8B.
    V1 {
        bos: u32,
        user_pre: u32,
        assistant_pre: u32,
        system_end: u32,
        turn_end: u32,
    },
    /// `<bos>[{system}<sys_end>]{user}<user_end>` — Hy-MT2-7B. Assistant
    /// content follows directly with no leading marker.
    V2 {
        bos: u32,
        user_end: u32,
        system_end: u32,
        eos: u32,
    },
}

/// Wrapper around `tokenizers::Tokenizer` for Hunyuan-MT models.
pub struct HyTokenizer {
    inner: Tokenizer,
    family: TokenizerFamily,
    template: ChatTemplate,
}

impl HyTokenizer {
    /// Load a `tokenizer.json` (HuggingFace fast-tokenizer format). The
    /// tokenizer family is detected by inspecting which special-token names
    /// the vocabulary contains.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let inner = Tokenizer::from_file(path.as_ref())
            .map_err(|e| Error::Tokenizer(format!("from_file failed: {e}")))?;
        let family = detect_family_from_inner(&inner);
        Ok(Self::with_inner(inner, family))
    }

    /// Load a `tokenizer.json` and assert that it belongs to the expected
    /// family. Use this from loaders that know the model family from
    /// elsewhere (e.g. parsed config) — guards against silently pairing a
    /// V2 model with a V1 tokenizer, which would produce garbage output.
    pub fn from_file_with_family(path: impl AsRef<Path>, family: TokenizerFamily) -> Result<Self> {
        let inner = Tokenizer::from_file(path.as_ref())
            .map_err(|e| Error::Tokenizer(format!("from_file failed: {e}")))?;
        let detected = detect_family_from_inner(&inner);
        if detected != family {
            return Err(Error::Tokenizer(format!(
                "tokenizer family mismatch: model expects {family:?}, \
                 tokenizer.json belongs to {detected:?}"
            )));
        }
        Ok(Self::with_inner(inner, family))
    }

    /// Build the tokenizer directly from data embedded in a GGUF file's
    /// metadata (no external `tokenizer.json` required). Supports the
    /// `gpt2` BPE flavour, which is what `tokenizer.ggml.model = "gpt2"`
    /// covers — the convention used by Hunyuan-MT 1.5 / 2 and most modern
    /// HF-derived GGUFs.
    pub fn from_gguf(content: &Content) -> Result<Self> {
        let meta = Meta::new(content);
        let model = meta.string("tokenizer.ggml.model")?;
        if model != "gpt2" {
            return Err(Error::Tokenizer(format!(
                "GGUF tokenizer model `{model}` not supported (expected `gpt2`)"
            )));
        }

        let tokens = read_string_array(&meta, "tokenizer.ggml.tokens")?;
        let merges_raw = read_string_array(&meta, "tokenizer.ggml.merges")?;
        let token_types = read_i32_array(&meta, "tokenizer.ggml.token_type")?;

        if tokens.len() != token_types.len() {
            return Err(Error::Tokenizer(format!(
                "tokens len {} != token_type len {}",
                tokens.len(),
                token_types.len()
            )));
        }

        // Parse "a b" merge lines into tuples.
        let mut merges: Vec<(String, String)> = Vec::with_capacity(merges_raw.len());
        for line in &merges_raw {
            let mut parts = line.splitn(2, ' ');
            match (parts.next(), parts.next()) {
                (Some(a), Some(b)) => merges.push((a.to_string(), b.to_string())),
                _ => return Err(Error::Tokenizer(format!("malformed merge `{line}`"))),
            }
        }

        let vocab: Vocab = tokens
            .iter()
            .enumerate()
            .map(|(i, t)| (t.clone(), i as u32))
            .collect();

        let bpe = BPE::builder()
            .vocab_and_merges(vocab, merges)
            .build()
            .map_err(|e| Error::Tokenizer(format!("BPE build failed: {e}")))?;

        let mut inner = Tokenizer::new(bpe);
        // Standard GPT-2 setup: byte-level pre-tokenizer / decoder /
        // post-processor. The same ByteLevel struct serves all three roles.
        inner.with_pre_tokenizer(Some(ByteLevel::default()));
        inner.with_decoder(Some(ByteLevel::default()));
        inner.with_post_processor(Some(ByteLevel::default()));

        // GGUF token-type values (mirroring llama.cpp's enum):
        //   1 NORMAL  2 UNKNOWN  3 CONTROL  4 USER_DEFINED  5 UNUSED  6 BYTE
        // Add CONTROL and USER_DEFINED entries as added tokens so the
        // tokenizer recognises them in raw input without splitting; CONTROL
        // ones get `special=true` so `decode(skip_special=true)` hides them.
        let added: Vec<AddedToken> = tokens
            .iter()
            .zip(token_types.iter())
            .filter_map(|(t, ty)| match *ty {
                3 => Some(AddedToken::from(t.clone(), true)),
                4 => Some(AddedToken::from(t.clone(), false)),
                _ => None,
            })
            .collect();
        if !added.is_empty() {
            inner.add_special_tokens(&added);
        }

        // Prefer the family declared in the GGUF metadata's bos id; fall
        // back to inspecting the just-built vocab for V2 markers if the
        // metadata key is missing.
        let family = match meta.opt_u32("tokenizer.ggml.bos_token_id")? {
            Some(bos) => TokenizerFamily::detect_from_bos(bos),
            None => detect_family_from_inner(&inner),
        };

        Ok(Self::with_inner(inner, family))
    }

    fn with_inner(inner: Tokenizer, family: TokenizerFamily) -> Self {
        let lookup =
            |name: &str, fallback: u32| -> u32 { inner.token_to_id(name).unwrap_or(fallback) };
        let template = match family {
            TokenizerFamily::V1 => ChatTemplate::V1 {
                bos: lookup(V1_TOK_BOS, BOS_ID),
                user_pre: lookup(V1_TOK_USER, 120_006),
                assistant_pre: lookup(V1_TOK_ASSISTANT, 120_007),
                system_end: lookup(V1_TOK_SYSTEM_END, 120_021),
                turn_end: lookup(V1_TOK_TURN_END, ASSISTANT_TURN_END),
            },
            TokenizerFamily::V2 => ChatTemplate::V2 {
                bos: lookup(V2_TOK_BOS, 127_958),
                user_end: lookup(V2_TOK_USER_END, 127_962),
                system_end: lookup(V2_TOK_SYSTEM_END, 127_966),
                eos: lookup(V2_TOK_EOS, 127_960),
            },
        };
        Self {
            inner,
            family,
            template,
        }
    }

    pub fn inner(&self) -> &Tokenizer {
        &self.inner
    }

    pub fn vocab_size(&self) -> usize {
        self.inner.get_vocab_size(true)
    }

    /// Which tokenizer lineage this instance was built for.
    pub fn family(&self) -> TokenizerFamily {
        self.family
    }

    /// Token ID conventionally used to terminate generation in this model.
    /// For V1 it's the `<｜hy_place▁holder▁no▁2｜>` turn-end placeholder
    /// (also declared as EOS in the GGUF); for V2 it's `<|eos|>`.
    pub fn turn_end_id(&self) -> u32 {
        match self.template {
            ChatTemplate::V1 { turn_end, .. } => turn_end,
            ChatTemplate::V2 { eos, .. } => eos,
        }
    }

    pub fn bos_id(&self) -> u32 {
        match self.template {
            ChatTemplate::V1 { bos, .. } | ChatTemplate::V2 { bos, .. } => bos,
        }
    }

    /// The first token emitted by the assistant after the user turn ends —
    /// useful for debug tooling that wants to seed assistant generation
    /// directly. V1 returns the explicit `<｜hy_Assistant｜>` marker; V2 has
    /// no such marker (assistant text starts immediately after `<|extra_0|>`),
    /// so we return that separator instead.
    pub fn assistant_id(&self) -> u32 {
        match self.template {
            ChatTemplate::V1 { assistant_pre, .. } => assistant_pre,
            ChatTemplate::V2 { user_end, .. } => user_end,
        }
    }

    /// Encode a raw string with no special-token wrapping.
    pub fn encode_raw(&self, text: &str) -> Result<Vec<u32>> {
        let enc = self
            .inner
            .encode(text, false)
            .map_err(|e| Error::Tokenizer(format!("encode failed: {e}")))?;
        Ok(enc.get_ids().to_vec())
    }

    /// Decode token IDs back to a string, hiding special tokens.
    pub fn decode(&self, ids: &[u32]) -> Result<String> {
        self.inner
            .decode(ids, true)
            .map_err(|e| Error::Tokenizer(format!("decode failed: {e}")))
    }

    pub fn decode_with_special(&self, ids: &[u32]) -> Result<String> {
        self.inner
            .decode(ids, false)
            .map_err(|e| Error::Tokenizer(format!("decode failed: {e}")))
    }

    /// Build the prompt token sequence for a single-turn user message using
    /// the family's chat template:
    ///
    /// - V1: `<BOS>[{system}<SYS_END>]<USER>{user}<ASSISTANT>`
    /// - V2: `<BOS>[{system}<SYS_END>]{user}<USER_END>`
    ///
    /// In both cases the model is expected to continue with assistant text
    /// up to its respective stop token.
    pub fn apply_chat_template(&self, system: Option<&str>, user: &str) -> Result<Vec<u32>> {
        let mut ids = Vec::with_capacity(8 + user.len() / 2);
        match self.template {
            ChatTemplate::V1 {
                bos,
                user_pre,
                assistant_pre,
                system_end,
                ..
            } => {
                ids.push(bos);
                if let Some(sys) = system {
                    ids.extend(self.encode_raw(sys)?);
                    ids.push(system_end);
                }
                ids.push(user_pre);
                ids.extend(self.encode_raw(user)?);
                ids.push(assistant_pre);
            }
            ChatTemplate::V2 {
                bos,
                user_end,
                system_end,
                ..
            } => {
                ids.push(bos);
                if let Some(sys) = system {
                    ids.extend(self.encode_raw(sys)?);
                    ids.push(system_end);
                }
                ids.extend(self.encode_raw(user)?);
                ids.push(user_end);
            }
        }
        Ok(ids)
    }

    /// Build the standard "translate into <tgt>" prompt. The model
    /// auto-detects the source language, so no `src_lang` parameter is
    /// required.
    pub fn build_translate_prompt(&self, tgt_lang: &str, text: &str) -> Result<Vec<u32>> {
        let user = format!(
            "Translate the following segment into {tgt_lang}, without additional explanation.\n\n{text}",
        );
        self.apply_chat_template(None, &user)
    }

    pub fn token_id(&self, text: &str) -> Option<u32> {
        self.inner.token_to_id(text)
    }
}

/// Heuristic family detection from a built tokenizer: V2 if the vocabulary
/// contains the canonical V2 BOS marker, else V1. Used by `from_file` and as
/// a fallback in `from_gguf` when metadata is missing.
fn detect_family_from_inner(inner: &Tokenizer) -> TokenizerFamily {
    if inner.token_to_id(V2_TOK_BOS).is_some() {
        TokenizerFamily::V2
    } else {
        TokenizerFamily::V1
    }
}

/// Read a typed homogeneous GGUF array, applying `extract` to each element.
/// Returns a clear, key-attributed error if any element has the wrong tag.
fn read_array<T, F>(
    meta: &Meta<'_>,
    key: &str,
    expected: &'static str,
    extract: F,
) -> Result<Vec<T>>
where
    F: Fn(&Value) -> Option<T>,
{
    let arr = meta.array(key)?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        match extract(v) {
            Some(t) => out.push(t),
            None => {
                return Err(Error::Tokenizer(format!(
                    "{key}: expected {expected} element, got {v:?}"
                )))
            }
        }
    }
    Ok(out)
}

fn read_string_array(meta: &Meta<'_>, key: &str) -> Result<Vec<String>> {
    read_array(meta, key, "String", |v| match v {
        Value::String(s) => Some(s.clone()),
        _ => None,
    })
}

fn read_i32_array(meta: &Meta<'_>, key: &str) -> Result<Vec<i32>> {
    read_array(meta, key, "I32", |v| match v {
        Value::I32(i) => Some(*i),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokenizers::models::bpe::BPE;

    /// Build a minimal in-memory tokenizer that contains only the given set
    /// of special-token strings. Lets the family-detection tests assert
    /// against `Tokenizer::token_to_id` lookups without touching the file
    /// system or a real GGUF.
    fn tok_with_specials(names: &[&str]) -> Tokenizer {
        let bpe = BPE::builder().build().unwrap();
        let mut t = Tokenizer::new(bpe);
        let added: Vec<AddedToken> = names
            .iter()
            .map(|n| AddedToken::from((*n).to_string(), true))
            .collect();
        t.add_special_tokens(&added);
        t
    }

    #[test]
    fn detect_family_from_inner_picks_v1_when_v2_bos_absent() {
        let t = tok_with_specials(&[V1_TOK_BOS, V1_TOK_USER]);
        assert_eq!(detect_family_from_inner(&t), TokenizerFamily::V1);
    }

    #[test]
    fn detect_family_from_inner_picks_v2_when_marker_present() {
        let t = tok_with_specials(&[V2_TOK_BOS, V2_TOK_USER_END, V2_TOK_EOS]);
        assert_eq!(detect_family_from_inner(&t), TokenizerFamily::V2);
    }

    #[test]
    fn chat_template_v1_uses_explicit_user_assistant_markers() {
        let t = tok_with_specials(&[
            V1_TOK_BOS,
            V1_TOK_USER,
            V1_TOK_ASSISTANT,
            V1_TOK_SYSTEM_END,
            V1_TOK_TURN_END,
        ]);
        let bos = t.token_to_id(V1_TOK_BOS).unwrap();
        let user = t.token_to_id(V1_TOK_USER).unwrap();
        let assistant = t.token_to_id(V1_TOK_ASSISTANT).unwrap();
        let hy = HyTokenizer::with_inner(t, TokenizerFamily::V1);
        let ids = hy.apply_chat_template(None, "hi").unwrap();
        // <bos> ... <user_pre> {encoded "hi"} <assistant_pre>
        assert_eq!(ids.first().copied(), Some(bos));
        assert!(ids.contains(&user));
        assert_eq!(ids.last().copied(), Some(assistant));
    }

    #[test]
    fn chat_template_v2_uses_suffix_marker() {
        let t = tok_with_specials(&[V2_TOK_BOS, V2_TOK_USER_END, V2_TOK_SYSTEM_END, V2_TOK_EOS]);
        let bos = t.token_to_id(V2_TOK_BOS).unwrap();
        let user_end = t.token_to_id(V2_TOK_USER_END).unwrap();
        let eos = t.token_to_id(V2_TOK_EOS).unwrap();
        let assistant_pre_v1 = t.token_to_id(V1_TOK_ASSISTANT);
        assert!(
            assistant_pre_v1.is_none(),
            "V1 marker leaked into V2 tokenizer"
        );
        let hy = HyTokenizer::with_inner(t, TokenizerFamily::V2);
        let ids = hy.apply_chat_template(None, "hi").unwrap();
        // <bos> {encoded "hi"} <user_end>  — no explicit assistant prefix.
        assert_eq!(ids.first().copied(), Some(bos));
        assert_eq!(ids.last().copied(), Some(user_end));
        assert_eq!(hy.family(), TokenizerFamily::V2);
        assert_eq!(hy.turn_end_id(), eos);
    }

    #[test]
    fn chat_template_v1_with_system_prepends_system_block() {
        let t = tok_with_specials(&[
            V1_TOK_BOS,
            V1_TOK_USER,
            V1_TOK_ASSISTANT,
            V1_TOK_SYSTEM_END,
            V1_TOK_TURN_END,
        ]);
        let sys_end = t.token_to_id(V1_TOK_SYSTEM_END).unwrap();
        let hy = HyTokenizer::with_inner(t, TokenizerFamily::V1);
        let ids = hy.apply_chat_template(Some("sys"), "hi").unwrap();
        // Look for system_end after the bos but before user_pre.
        assert!(ids.contains(&sys_end));
    }
}
