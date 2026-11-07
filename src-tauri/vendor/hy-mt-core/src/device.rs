//! Device selection. YiTerm ships CPU-only inference (Candle CPU backend);
//! Metal/CUDA paths were removed from the vendored copy.

use candle_core::{DType, Device};

/// High-level kind of device the model is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Cpu,
}

/// Bundle of [`DeviceKind`] and the underlying Candle [`Device`].
#[derive(Debug, Clone)]
pub struct DeviceCtx {
    pub kind: DeviceKind,
    pub device: Device,
}

impl DeviceCtx {
    pub fn cpu() -> Self {
        Self {
            kind: DeviceKind::Cpu,
            device: Device::Cpu,
        }
    }

    /// Whether the device can run the STQ1_0 custom matmul natively.
    #[inline]
    pub fn supports_stq1_0_native(&self) -> bool {
        matches!(self.kind, DeviceKind::Cpu)
    }

    /// Activation dtype: F32 on CPU (matches the STQ1_0 custom matmul kernel).
    #[inline]
    pub fn compute_dtype(&self) -> DType {
        DType::F32
    }
}
