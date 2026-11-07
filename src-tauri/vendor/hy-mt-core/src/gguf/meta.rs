//! Typed accessors over `vendored::Value` for the metadata keys produced by
//! the Hunyuan-MT GGUF writer in llama.cpp (`hunyuan-dense.*` and variants).

use super::vendored::{Content, Value};
use crate::{Error, Result};

const ARCH_KEY: &str = "general.architecture";
/// Conventional architecture string used by both Hy-MT 1.5 and Hy-MT2 GGUFs.
/// Kept as a constant for tests; production code paths read the actual string
/// from `general.architecture` via [`Meta::arch_key`].
pub const ARCH_NAME: &str = "hunyuan-dense";

/// Thin wrapper around the parsed metadata HashMap with typed lookups and
/// architecture-aware key formatting.
pub struct Meta<'a> {
    content: &'a Content,
}

impl<'a> Meta<'a> {
    pub fn new(content: &'a Content) -> Self {
        Self { content }
    }

    pub fn architecture(&self) -> Result<&str> {
        Ok(self.string(ARCH_KEY)?.as_str())
    }

    /// Build the fully-qualified key `<arch>.<suffix>` using whichever
    /// architecture name this GGUF declares. Returns an error if the
    /// `general.architecture` field is missing (which would make all
    /// downstream lookups meaningless anyway).
    pub fn arch_key(&self, suffix: &str) -> Result<String> {
        Ok(format!("{}.{suffix}", self.architecture()?))
    }

    fn get(&self, key: &str) -> Result<&Value> {
        self.content
            .metadata
            .get(key)
            .ok_or_else(|| Error::MissingMeta(key.to_string()))
    }

    pub fn u32(&self, key: &str) -> Result<u32> {
        let v = self.get(key)?.to_u64()?;
        u32::try_from(v).map_err(|_| Error::Gguf(format!("{key}={v} does not fit in u32")))
    }

    pub fn u64(&self, key: &str) -> Result<u64> {
        self.get(key)?.to_u64()
    }

    pub fn usize(&self, key: &str) -> Result<usize> {
        Ok(self.u64(key)? as usize)
    }

    pub fn f32(&self, key: &str) -> Result<f32> {
        self.get(key)?.to_f32()
    }

    pub fn bool(&self, key: &str) -> Result<bool> {
        self.get(key)?.to_bool()
    }

    pub fn string(&self, key: &str) -> Result<&String> {
        self.get(key)?.to_string()
    }

    pub fn opt_u32(&self, key: &str) -> Result<Option<u32>> {
        match self.content.metadata.get(key) {
            None => Ok(None),
            Some(_) => Ok(Some(self.u32(key)?)),
        }
    }

    pub fn opt_f32(&self, key: &str) -> Result<Option<f32>> {
        match self.content.metadata.get(key) {
            None => Ok(None),
            Some(_) => Ok(Some(self.f32(key)?)),
        }
    }

    pub fn opt_bool(&self, key: &str) -> Result<Option<bool>> {
        match self.content.metadata.get(key) {
            None => Ok(None),
            Some(_) => Ok(Some(self.bool(key)?)),
        }
    }

    pub fn array(&self, key: &str) -> Result<&Vec<Value>> {
        self.get(key)?.to_array()
    }

    pub fn opt_array_len(&self, key: &str) -> Result<Option<usize>> {
        match self.content.metadata.get(key) {
            None => Ok(None),
            Some(v) => Ok(Some(v.to_array()?.len())),
        }
    }
}
