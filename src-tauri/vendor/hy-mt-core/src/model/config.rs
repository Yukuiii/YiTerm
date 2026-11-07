//! Hyperparameters for the Hunyuan-MT dense decoder (1.5 and 2.0 families).
//!
//! When loading from GGUF we read the `<arch>.*` keys (typically
//! `hunyuan-dense.*`); the [`Self::hy_mt_15_18b`], [`Self::hy_mt_2_18b`], and
//! [`Self::hy_mt_2_7b`] constructors produce reference values hardcoded for
//! each supported variant (useful for tests and as a sanity reference).

use crate::gguf::meta::Meta;
use crate::Result;

/// First id in the special-token range shared by both tokenizer families
/// (V1 BOS = 120 000, V2 BOS = 127 958). Stop ids below this are byte-level
/// tokens, never the chat turn-end marker.
const SPECIAL_TOKEN_MIN: u32 = 120_000;

/// Which tokenizer / chat-template lineage a checkpoint uses.
///
/// - `V1` is the original Hunyuan-MT 1.5 tokenizer (vocab 120 818, full-width
///   `｜` pipes, `<｜hy_User｜>` style turn markers). Used by both
///   `tencent/HY-MT1.5-1.8B` and `tencent/Hy-MT2-1.8B` — Hy-MT2-1.8B is
///   architecturally a re-trained 1.5 and inherits the same vocabulary.
/// - `V2` is the new Hy-MT2-7B tokenizer (vocab 128 167, ASCII `|` pipes,
///   `<|startoftext|>` / `<|extra_N|>` markers). Used only by
///   `tencent/Hy-MT2-7B*` so far.
///
/// Detected by inspecting `bos_token_id` once at config-load time — V1 BOS
/// lives in the 120 000 range, V2 BOS at 127 958.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenizerFamily {
    V1,
    V2,
}

impl TokenizerFamily {
    /// Pick the family based on the canonical BOS id. The boundary is a
    /// generous midpoint between the two known ranges so future minor reshuffles
    /// inside either family don't flip the classification.
    pub fn detect_from_bos(bos: u32) -> Self {
        if bos < 121_000 {
            Self::V1
        } else {
            Self::V2
        }
    }

    /// Defense-in-depth secondary stop token used by `Generator`. V1 reserves
    /// id 120_001 as a raw EOS that the chat template's `turn_end` (120_020)
    /// is preferred over — but degenerate completions occasionally emit it
    /// directly. V2 has no such legacy variant; its `<|eos|>` (127_960) is
    /// already the canonical stop token.
    pub fn legacy_eos(self) -> Option<u32> {
        match self {
            Self::V1 => Some(120_001),
            Self::V2 => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HunyuanConfig {
    pub n_layers: usize,
    pub hidden_size: usize,
    pub intermediate_size: usize,
    pub n_heads: usize,
    pub n_kv_heads: usize,
    pub head_dim: usize,
    pub vocab_size: usize,
    pub max_position_embeddings: usize,
    pub rms_norm_eps: f32,
    pub rope_theta: f32,
    /// Dynamic-NTK alpha (only kicks in beyond `max_position_embeddings`).
    pub rope_scaling_alpha: f32,
    pub use_qk_norm: bool,
    pub tie_word_embeddings: bool,
    pub bos_id: u32,
    pub eos_id: u32,
    pub pad_id: u32,
    pub tokenizer_family: TokenizerFamily,
}

impl HunyuanConfig {
    /// Reference values for the Hy-MT 1.5 1.8B base model — useful for tests
    /// where no GGUF file is available.
    pub fn hy_mt_15_18b() -> Self {
        Self {
            n_layers: 32,
            hidden_size: 2048,
            intermediate_size: 6144,
            n_heads: 16,
            n_kv_heads: 4,
            head_dim: 128,
            vocab_size: 120818,
            max_position_embeddings: 262_144,
            rms_norm_eps: 1e-5,
            rope_theta: 10_000.0,
            rope_scaling_alpha: 1000.0,
            use_qk_norm: true,
            tie_word_embeddings: true,
            bos_id: 120_000,
            eos_id: 120_020,
            pad_id: 120_002,
            tokenizer_family: TokenizerFamily::V1,
        }
    }

    /// Reference values for the Hy-MT2 1.8B base model. Architecturally
    /// identical to Hy-MT 1.5 1.8B (same hidden_size / n_layers / vocab /
    /// special-token ids); the differences live in the training data and
    /// fine-tuning recipe, not the weights' shape.
    pub fn hy_mt_2_18b() -> Self {
        // Tencent's config.json for tencent/Hy-MT2-1.8B matches Hy-MT 1.5 1.8B
        // byte-for-byte on architecture and token ids, so we can reuse those
        // values directly.
        Self::hy_mt_15_18b()
    }

    /// Reference values for the Hy-MT2 7B base model. Larger transformer plus
    /// the V2 tokenizer (vocab 128 167, ASCII-pipe special tokens).
    pub fn hy_mt_2_7b() -> Self {
        Self {
            n_layers: 32,
            hidden_size: 4096,
            intermediate_size: 14_336,
            n_heads: 32,
            n_kv_heads: 8,
            head_dim: 128,
            vocab_size: 128_167,
            max_position_embeddings: 262_144,
            rms_norm_eps: 1e-5,
            rope_theta: 10_000.0,
            rope_scaling_alpha: 1000.0,
            use_qk_norm: true,
            tie_word_embeddings: true,
            bos_id: 127_958,
            eos_id: 127_960,
            pad_id: 127_961,
            tokenizer_family: TokenizerFamily::V2,
        }
    }

    /// Read the configuration from GGUF metadata.
    ///
    /// All `<arch>.*` keys are looked up via the dynamic [`Meta::arch_key`]
    /// helper, so the loader works for any architecture string the converter
    /// emits (`hunyuan-dense` for both Hy-MT 1.5 and Hy-MT2; future variants
    /// pick up automatically). We still reject architectures we don't
    /// recognise — see [`is_supported_arch`].
    pub fn from_gguf(meta: &Meta<'_>) -> Result<Self> {
        let arch = meta.architecture()?.to_string();
        if !is_supported_arch(&arch) {
            return Err(crate::Error::Gguf(format!(
                "unsupported architecture `{arch}` (expected `hunyuan-dense` \
                 or another Hunyuan-MT dense variant)"
            )));
        }

        let n_layers = meta.usize(&meta.arch_key("block_count")?)?;
        let hidden_size = meta.usize(&meta.arch_key("embedding_length")?)?;
        let intermediate_size = meta.usize(&meta.arch_key("feed_forward_length")?)?;
        let n_heads = meta.usize(&meta.arch_key("attention.head_count")?)?;
        let n_kv_heads = meta.usize(&meta.arch_key("attention.head_count_kv")?)?;
        let max_position_embeddings = meta.usize(&meta.arch_key("context_length")?)?;
        let rms_norm_eps = meta
            .opt_f32(&meta.arch_key("attention.layer_norm_rms_epsilon")?)?
            .unwrap_or(1e-5);
        let rope_theta = meta
            .opt_f32(&meta.arch_key("rope.freq_base")?)?
            .unwrap_or(10_000.0);
        let rope_scaling_alpha = meta
            .opt_f32(&meta.arch_key("rope.scaling.alpha")?)?
            .unwrap_or(1.0);

        // head_dim may be present explicitly or inferred from hidden_size / n_heads.
        let head_dim = match meta.opt_u32(&meta.arch_key("attention.key_length")?)? {
            Some(v) => v as usize,
            None => hidden_size / n_heads,
        };
        let vocab_size = match meta.usize(&meta.arch_key("vocab_size")?) {
            Ok(v) => v,
            Err(_) => meta
                .opt_array_len("tokenizer.ggml.tokens")?
                .ok_or_else(|| crate::Error::Gguf("cannot determine vocab size".into()))?,
        };

        let use_qk_norm = meta
            .opt_bool(&meta.arch_key("attention.use_qk_norm")?)?
            .unwrap_or(true);
        let tie_word_embeddings = meta
            .opt_bool(&meta.arch_key("tie_word_embeddings")?)?
            .unwrap_or(true);

        // BOS detection drives the tokenizer-family choice — fall back to the
        // V1 default (120_000) only when the GGUF omits the field entirely,
        // which matches existing Hy-MT 1.5 behaviour.
        let bos_id = meta
            .opt_u32("tokenizer.ggml.bos_token_id")?
            .unwrap_or(120_000);
        let tokenizer_family = TokenizerFamily::detect_from_bos(bos_id);
        let (default_eos, default_pad) = match tokenizer_family {
            TokenizerFamily::V1 => (120_020_u32, 120_002_u32),
            TokenizerFamily::V2 => (127_960_u32, 127_961_u32),
        };
        // Chat-tuned GGUFs frequently set `eos_token_id` to a low byte-token
        // id (e.g. 3 in Hy-MT2-7B-GGUF) and put the actual end-of-turn marker
        // in `eot_token_id` instead. Prefer `eot_token_id` whenever it's
        // present — that's what llama.cpp's own runtime stops on. Fall back
        // to `eos_token_id` for converters that don't emit `eot_token_id`
        // (true for both Hy-MT 1.5 and Hy-MT2-1.8B GGUFs, where the bare
        // `eos_token_id` already names the correct turn-end token).
        let eos_id = match meta.opt_u32("tokenizer.ggml.eot_token_id")? {
            Some(v) => v,
            None => {
                let declared = meta
                    .opt_u32("tokenizer.ggml.eos_token_id")?
                    .unwrap_or(default_eos);
                // Some converters (e.g. mradermacher's Hy-MT2-1.8B-GGUF)
                // copy the base tokenizer's raw byte-level EOS (id 3) while
                // the chat template actually ends turns on the special
                // turn-end token. A stop id below the special-token range
                // would never be emitted, so generation would never stop —
                // prefer the family default in that case.
                if declared >= SPECIAL_TOKEN_MIN {
                    declared
                } else {
                    default_eos
                }
            }
        };
        let pad_id = meta
            .opt_u32("tokenizer.ggml.padding_token_id")?
            .unwrap_or(default_pad);

        Ok(Self {
            n_layers,
            hidden_size,
            intermediate_size,
            n_heads,
            n_kv_heads,
            head_dim,
            vocab_size,
            max_position_embeddings,
            rms_norm_eps,
            rope_theta,
            rope_scaling_alpha,
            use_qk_norm,
            tie_word_embeddings,
            bos_id,
            eos_id,
            pad_id,
            tokenizer_family,
        })
    }

    /// `n_heads / n_kv_heads`, the GQA repetition factor.
    pub fn kv_groups(&self) -> usize {
        self.n_heads / self.n_kv_heads
    }

    /// Parse a HuggingFace `config.json` (e.g. as shipped with
    /// `tencent/HY-MT1.5-1.8B`, `tencent/Hy-MT2-1.8B`, `tencent/Hy-MT2-7B`,
    /// or any of the AngelSlim quantized variants).
    ///
    /// Missing fields fall back to architecture defaults from
    /// [`Self::hy_mt_15_18b`] where reasonable.
    pub fn from_hf_config(json: &serde_json::Value) -> Result<Self> {
        let arch = json
            .get("architectures")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !arch.contains("HunYuan") {
            return Err(crate::Error::Gguf(format!(
                "expected HunYuan* architecture, got `{arch}`"
            )));
        }

        let defaults = Self::hy_mt_15_18b();
        let usize_or = |key: &str, fallback: usize| -> usize {
            json.get(key)
                .and_then(|v| v.as_u64())
                .map(|x| x as usize)
                .unwrap_or(fallback)
        };
        let f32_or = |key: &str, fallback: f32| -> f32 {
            json.get(key)
                .and_then(|v| v.as_f64())
                .map(|x| x as f32)
                .unwrap_or(fallback)
        };
        let u32_or = |key: &str, fallback: u32| -> u32 {
            json.get(key)
                .and_then(|v| v.as_u64())
                .map(|x| x as u32)
                .unwrap_or(fallback)
        };
        let bool_or = |key: &str, fallback: bool| -> bool {
            json.get(key).and_then(|v| v.as_bool()).unwrap_or(fallback)
        };

        let n_heads = usize_or("num_attention_heads", defaults.n_heads);
        let hidden_size = usize_or("hidden_size", defaults.hidden_size);
        let head_dim = json
            .get("head_dim")
            .and_then(|v| v.as_u64())
            .map(|x| x as usize)
            .unwrap_or_else(|| {
                json.get("attention_head_dim")
                    .and_then(|v| v.as_u64())
                    .map(|x| x as usize)
                    .unwrap_or(hidden_size / n_heads.max(1))
            });
        // The HF checkpoint stores the *unscaled* RoPE base (10_000) plus a
        // separate `rope_scaling.alpha` factor. Hunyuan was trained with the
        // alpha-NTK base scaling **always** applied (not only beyond
        // `max_position_embeddings`), so the model expects every position —
        // including the first — to use the rescaled base. The production
        // STQ1_0 GGUF stores the post-scaling base (`11_158_840`) directly,
        // which is why GGUF inference matches the HF reference output and
        // raw-base safetensors inference doesn't. Apply the scaling here so
        // both paths feed `RopeCache::new` an already-effective base.
        //
        //   effective_base = base * alpha^(d / (d - 2))
        //
        // Default alpha = 1.0 (i.e. no scaling) when the field is absent.
        let rope_alpha = json
            .get("rope_scaling")
            .and_then(|v| v.get("alpha"))
            .and_then(|v| v.as_f64())
            .map(|x| x as f32)
            .unwrap_or(1.0);
        let raw_rope_theta = f32_or("rope_theta", defaults.rope_theta);
        let effective_rope_theta =
            if rope_alpha > 0.0 && (rope_alpha - 1.0).abs() > f32::EPSILON && head_dim > 2 {
                let exp = head_dim as f32 / (head_dim as f32 - 2.0);
                raw_rope_theta * rope_alpha.powf(exp)
            } else {
                raw_rope_theta
            };

        let bos_id = u32_or("bos_token_id", defaults.bos_id);
        let tokenizer_family = TokenizerFamily::detect_from_bos(bos_id);
        let (default_eos, default_pad) = match tokenizer_family {
            TokenizerFamily::V1 => (defaults.eos_id, defaults.pad_id),
            TokenizerFamily::V2 => (127_960_u32, 127_961_u32),
        };

        Ok(Self {
            n_layers: usize_or("num_hidden_layers", defaults.n_layers),
            hidden_size,
            intermediate_size: usize_or("intermediate_size", defaults.intermediate_size),
            n_heads,
            n_kv_heads: usize_or("num_key_value_heads", defaults.n_kv_heads),
            head_dim,
            vocab_size: usize_or("vocab_size", defaults.vocab_size),
            max_position_embeddings: usize_or(
                "max_position_embeddings",
                defaults.max_position_embeddings,
            ),
            rms_norm_eps: f32_or("rms_norm_eps", defaults.rms_norm_eps),
            rope_theta: effective_rope_theta,
            rope_scaling_alpha: rope_alpha,
            use_qk_norm: bool_or("use_qk_norm", defaults.use_qk_norm),
            tie_word_embeddings: bool_or("tie_word_embeddings", defaults.tie_word_embeddings),
            bos_id,
            eos_id: u32_or("eos_token_id", default_eos),
            pad_id: u32_or("pad_token_id", default_pad),
            tokenizer_family,
        })
    }
}

/// Architectures we recognise from the GGUF `general.architecture` field.
///
/// Both Hy-MT 1.5 and Hy-MT2 GGUFs ship as `hunyuan-dense` (the upstream
/// model_type `hunyuan_v1_dense` survives intact across the family rename).
/// New aliases are gated through this single helper so future relaxation
/// (e.g. a `hunyuan-v2-dense` rename) is one literal change.
fn is_supported_arch(arch: &str) -> bool {
    matches!(arch, "hunyuan-dense" | "hunyuan-v1-dense")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_detect_from_bos_classifies_both_lineages() {
        // V1: Hy-MT 1.5 / Hy-MT2-1.8B canonical BOS plus its neighbours.
        assert_eq!(
            TokenizerFamily::detect_from_bos(120_000),
            TokenizerFamily::V1
        );
        assert_eq!(
            TokenizerFamily::detect_from_bos(120_020),
            TokenizerFamily::V1
        );
        // V2: Hy-MT2-7B canonical BOS plus the surrounding extra_N range.
        assert_eq!(
            TokenizerFamily::detect_from_bos(127_958),
            TokenizerFamily::V2
        );
        assert_eq!(
            TokenizerFamily::detect_from_bos(127_961),
            TokenizerFamily::V2
        );
        // Midpoint of the gap stays on the V1 side per the cutoff.
        assert_eq!(
            TokenizerFamily::detect_from_bos(120_999),
            TokenizerFamily::V1
        );
        assert_eq!(
            TokenizerFamily::detect_from_bos(121_000),
            TokenizerFamily::V2
        );
    }

    #[test]
    fn legacy_eos_only_for_v1() {
        assert_eq!(TokenizerFamily::V1.legacy_eos(), Some(120_001));
        assert_eq!(TokenizerFamily::V2.legacy_eos(), None);
    }

    #[test]
    fn from_hf_config_detects_hy_mt_2_7b() {
        // Fabricated minimal config matching tencent/Hy-MT2-7B.
        let json = serde_json::json!({
            "architectures": ["HunYuanDenseV1ForCausalLM"],
            "model_type": "hunyuan_v1_dense",
            "hidden_size": 4096,
            "num_hidden_layers": 32,
            "num_attention_heads": 32,
            "num_key_value_heads": 8,
            "head_dim": 128,
            "intermediate_size": 14336,
            "vocab_size": 128167,
            "max_position_embeddings": 262144,
            "rope_theta": 10000.0,
            "rope_scaling": {"alpha": 1000.0},
            "rms_norm_eps": 1e-5,
            "use_qk_norm": true,
            "tie_word_embeddings": true,
            "bos_token_id": 127958,
            "eos_token_id": 127960,
            "pad_token_id": 127961,
        });
        let cfg = HunyuanConfig::from_hf_config(&json).expect("parse Hy-MT2-7B config");
        assert_eq!(cfg.tokenizer_family, TokenizerFamily::V2);
        assert_eq!(cfg.hidden_size, 4096);
        assert_eq!(cfg.n_heads, 32);
        assert_eq!(cfg.n_kv_heads, 8);
        assert_eq!(cfg.intermediate_size, 14336);
        assert_eq!(cfg.vocab_size, 128167);
        assert_eq!(cfg.bos_id, 127958);
        // alpha-NTK rescaling stays the same formula for head_dim=128, alpha=1000.
        assert!((cfg.rope_theta - 11_158_844.0).abs() < 10.0);
    }

    #[test]
    fn from_hf_config_keeps_v1_for_hy_mt_15() {
        let json = serde_json::json!({
            "architectures": ["HunYuanDenseV1ForCausalLM"],
            "hidden_size": 2048,
            "num_hidden_layers": 32,
            "num_attention_heads": 16,
            "num_key_value_heads": 4,
            "head_dim": 128,
            "intermediate_size": 6144,
            "vocab_size": 120818,
            "bos_token_id": 120000,
            "eos_token_id": 120020,
            "pad_token_id": 120002,
            "rope_scaling": {"alpha": 1000.0},
        });
        let cfg = HunyuanConfig::from_hf_config(&json).expect("parse Hy-MT 1.5 config");
        assert_eq!(cfg.tokenizer_family, TokenizerFamily::V1);
        assert_eq!(cfg.vocab_size, 120818);
        assert_eq!(cfg.bos_id, 120000);
    }

    #[test]
    fn rejects_unsupported_architecture() {
        // architectures=[] would trip the `contains("HunYuan")` check.
        let json = serde_json::json!({
            "architectures": ["LlamaForCausalLM"],
            "hidden_size": 2048,
        });
        assert!(HunyuanConfig::from_hf_config(&json).is_err());
    }
}
