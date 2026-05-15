//! Audio sample type abstraction.
//!
//! Host code is generic over `f32` (the wire format for CLAP /
//! VST2 / LV2 / AAX) and `f64` (supported by VST3, AU v2, AU v3).
//! Each format wrapper picks the sample type at load time based
//! on what the plugin asks for and what the host configured.

/// Audio sample scalar — `f32` or `f64`.
///
/// Implemented by `f32` and `f64` only. The bound exists so the
/// [`crate::Plugin`] trait can be parameterised over precision
/// without leaking the full numeric trait surface.
pub trait Sample: Copy + Send + Sync + 'static + private::Sealed {
    /// Zero value of this precision.
    const ZERO: Self;

    /// Whether this sample type is `f64`. Lets format wrappers
    /// pick the `processSetup::symbolicSampleSize` field
    /// (VST3) or equivalent without runtime branching past the
    /// constant-fold.
    const IS_F64: bool;

    /// Widen a `f32` to this precision (`f32 → f32` is identity).
    fn from_f32(value: f32) -> Self;

    /// Narrow to `f32` for handing back to the host.
    fn to_f32(self) -> f32;
}

impl Sample for f32 {
    const ZERO: Self = 0.0;
    const IS_F64: bool = false;
    #[inline]
    fn from_f32(value: f32) -> Self {
        value
    }
    #[inline]
    fn to_f32(self) -> f32 {
        self
    }
}

impl Sample for f64 {
    const ZERO: Self = 0.0;
    const IS_F64: bool = true;
    #[inline]
    fn from_f32(value: f32) -> Self {
        f64::from(value)
    }
    #[inline]
    #[allow(clippy::cast_possible_truncation)]
    fn to_f32(self) -> f32 {
        self as f32
    }
}

mod private {
    pub trait Sealed {}
    impl Sealed for f32 {}
    impl Sealed for f64 {}
}
