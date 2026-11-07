//! Hunyuan-MT dense decoder transformer (1.5 and 2.0 families).
//!
//! Architecture summary (`HunYuanDenseV1ForCausalLM`):
//! - Transformer layers with pre-norm residual scheme
//! - GQA (heads / kv-heads ratio fixed at 4:1 in the released variants)
//! - RMSNorm (eps 1e-5) including QK-norm on the per-head Q and K
//! - SwiGLU FFN
//! - Rotary positional embedding, base 10000, dynamic-NTK scaling
//! - Tied input/output embeddings (`tie_word_embeddings = true`)
//!
//! Sizes per variant:
//! - Hy-MT 1.5 / Hy-MT2 1.8B: 32 layers, hidden 2048, 16/4 heads, FFN 6144,
//!   vocab 120 818 (V1 tokenizer).
//! - Hy-MT2 7B: 32 layers, hidden 4096, 32/8 heads, FFN 14 336, vocab
//!   128 167 (V2 tokenizer).

mod attention;
pub mod config;
mod ffn;
mod kv_cache;
mod layer;
pub mod layout;
mod linear;
mod rms_norm;
mod transformer;

pub use config::{HunyuanConfig, TokenizerFamily};
pub use kv_cache::KvCache;
pub use layout::{BlockSlot, TensorRole};
pub use linear::QuantLinear;
pub use rms_norm::{RmsNorm, RmsNormPerHead};
pub use transformer::HunyuanDense;
