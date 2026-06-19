//! Host-side transport clock for the standalone runner.
//!
//! The standalone host has no real DAW timeline, so it synthesizes
//! one: a free-running clock that advances a song position by the
//! block size every audio callback and reports a fixed tempo and
//! time signature. Tempo / time signature / play state are taken
//! from the CLI (`--tempo`, `--time-sig`, `--paused`,
//! `--no-transport`) via [`set_config`].
//!
//! Each format wrapper translates the resulting [`TransportInfo`]
//! into its backend's native transport struct (VST3
//! `ProcessContext`, CLAP `clap_event_transport`, LV2
//! `time:Position`, AU host callbacks), so tempo / grid-synced
//! plugins get a usable transport even with no DAW driving them.

use std::sync::OnceLock;

use truce_rack_core::transport::TransportInfo;

/// Transport parameters chosen at startup, shared with the audio
/// thread. Read-only once the stream is running.
#[derive(Debug, Clone, Copy)]
pub struct TransportConfig {
    /// When `false`, the runner reports no transport at all
    /// (`ProcessContext::transport == None`) — matches a host that
    /// doesn't expose a timeline.
    pub enabled: bool,
    /// Tempo in quarter-notes-per-minute.
    pub tempo_bpm: f64,
    /// `(numerator, denominator)` time signature.
    pub time_sig: (u32, u32),
    /// Whether the synthesized transport is rolling. When `false`
    /// the song position is frozen and `playing` is reported false.
    pub playing: bool,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tempo_bpm: 120.0,
            time_sig: (4, 4),
            playing: true,
        }
    }
}

static CONFIG: OnceLock<TransportConfig> = OnceLock::new();

/// Install the process-wide transport config. Called once from
/// `main` after parsing CLI flags; ignored if called twice.
pub fn set_config(config: TransportConfig) {
    let _ = CONFIG.set(config);
}

/// The installed transport config, or the default (120 BPM, 4/4,
/// playing) when `main` never set one.
#[must_use]
pub fn config() -> TransportConfig {
    CONFIG.get().copied().unwrap_or_default()
}

/// Free-running transport clock owned by one audio stream. Tracks
/// the continuous song position in samples and derives the musical
/// position each block from the configured tempo.
pub struct TransportClock {
    config: TransportConfig,
    /// Continuous song position in samples since the stream started.
    sample_pos: i64,
}

impl TransportClock {
    /// Build a clock from the process-wide [`config`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: config(),
            sample_pos: 0,
        }
    }

    /// Snapshot the transport for a block of `frames` at
    /// `sample_rate`, then advance the clock (only while playing).
    ///
    /// Returns `None` when transport is disabled, so the wrapper
    /// passes `ProcessContext::transport == None` straight through.
    #[allow(clippy::cast_precision_loss)]
    pub fn next_block(&mut self, frames: usize, sample_rate: f64) -> Option<TransportInfo> {
        if !self.config.enabled {
            return None;
        }
        let (num, den) = self.config.time_sig;
        let beats_per_sec = self.config.tempo_bpm / 60.0;
        let pos_seconds = self.sample_pos as f64 / sample_rate.max(1.0);
        // Beats are quarter notes — the convention VST3 / CLAP use
        // for projectTimeMusic / song_pos_beats.
        let song_position_beats = pos_seconds * beats_per_sec;
        let beats_per_bar = f64::from(num) * 4.0 / f64::from(den.max(1));
        let bar_index = (song_position_beats / beats_per_bar.max(f64::EPSILON)).floor();
        let bar_start_beats = bar_index * beats_per_bar;

        let info = TransportInfo {
            tempo_bpm: Some(self.config.tempo_bpm),
            time_signature: Some((num, den)),
            song_position_beats: Some(song_position_beats),
            song_position_samples: Some(self.sample_pos),
            bar_start_beats: Some(bar_start_beats),
            playing: self.config.playing,
            recording: false,
            loop_active: false,
        };

        if self.config.playing {
            self.sample_pos = self
                .sample_pos
                .saturating_add(i64::try_from(frames).unwrap_or(0));
        }
        Some(info)
    }
}

impl Default for TransportClock {
    fn default() -> Self {
        Self::new()
    }
}
