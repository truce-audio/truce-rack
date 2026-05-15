//! Host transport snapshot passed into [`crate::Plugin::process`].
//!
//! Mirrors `truce_core::TransportInfo`. Plugin-side and host-side
//! types are intentionally separate (per the v2 design decision)
//! but field-compatible — copying a value across is a struct
//! literal, not a conversion.

/// One block's worth of host transport state.
///
/// `Option`s express "host doesn't report this field" — a
/// CLAP host without a transport extension, or VST3
/// `ProcessContext` flags not set, leave the corresponding
/// field as `None`. Plugins should fall back gracefully (default
/// 120 BPM, free-running phase) when fields are missing.
#[derive(Debug, Clone, Copy, Default)]
pub struct TransportInfo {
    /// Tempo in BPM. `None` = host did not report.
    pub tempo_bpm: Option<f64>,
    /// `(numerator, denominator)` time signature. `None` = host
    /// did not report (single hosts that strictly report only
    /// numerator + denominator separately would still pack both
    /// fields here once available).
    pub time_signature: Option<(u32, u32)>,
    /// Continuous song position in beats. `None` = host did not
    /// report or is not playing back a timeline.
    pub song_position_beats: Option<f64>,
    /// Continuous song position in samples since the host's
    /// timeline origin. `None` = host did not report.
    pub song_position_samples: Option<i64>,
    /// First-sample-of-current-bar offset in beats.
    pub bar_start_beats: Option<f64>,
    /// Host is currently playing back / rendering — `false` for
    /// armed-but-paused or stopped state.
    pub playing: bool,
    /// Host is in record-arm + transport-running state.
    pub recording: bool,
    /// Host loop is active.
    pub loop_active: bool,
}
