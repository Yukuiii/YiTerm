//! Vendored, CPU-only Hunyuan-MT GGUF inference for YiTerm.
//!
//! Derived from `n0madic/hy-mt-rs` (`hy-mt-core`, master @ 9f31dcf), pruned:
//! - removed `hub` (HuggingFace download) and `safetensors_loader` paths;
//! - removed `metal`/`cuda`/`mkl`/`accelerate` features (CPU only);
//! - kept the GGUF parser, K-quant loading via Candle, the `HunyuanDense`
//!   decoder, BPE tokenizer, sampler, and the token-streaming generator.
//!
//! The STQ1_0 codec is retained because the GGUF dtype enum references it,
//! though the Q3_K_M model never exercises those paths.

pub mod device;
pub mod error;
pub mod generate;
pub mod gguf;
pub mod model;
pub mod quant;
pub mod rope;
pub mod sampling;
pub mod source;
pub mod tokenizer;
pub(crate) mod util;
pub mod weights;

pub use error::{Error, LoadingKind, Result};
pub use source::ModelSource;
