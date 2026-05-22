//! Assertion helpers for truce-rack host integration tests.
//!
//! Use these from host-side test suites that want to verify a
//! [`truce_rack_core::scanner::PluginScanner`] impl can scan a corpus, load each result,
//! activate it, and render a known input without NaN / clipping.
//!
//! The helpers operate on `Plugin<f32>` instances by default;
//! a parallel `_f64` variant for `Plugin<f64>` can be added when
//! the first VST3 / AU 64-bit consumer needs it.
//!
//! # Example
//!
//! ```ignore
//! use truce_rack_core::scanner::PluginScanner;
//! use truce_rack_test::{render_silence, assert_no_nans};
//!
//! let scanner = MyScanner::new();
//! for info in scanner.scan()? {
//!     let mut plugin = scanner.load(&info)?;
//!     let rendered = render_silence(&mut plugin, 48_000.0, 1024)?;
//!     assert_no_nans(&rendered);
//! }
//! ```

use truce_rack_core::buffer::{AudioBuffer, BusRange};
use truce_rack_core::bus::BusLayout;
use truce_rack_core::error::{Error, Result};
use truce_rack_core::events::EventList;
use truce_rack_core::plugin::{Plugin, PluginCore, ProcessContext};

/// Rendered audio block — what every helper hands back so callers
/// can run their own assertions.
#[derive(Debug, Clone)]
pub struct Rendered {
    /// Output channels, planar.
    pub output: Vec<Vec<f32>>,
}

impl Rendered {
    /// Maximum absolute sample across all channels.
    #[must_use]
    pub fn peak(&self) -> f32 {
        self.output
            .iter()
            .flat_map(|c| c.iter())
            .map(|s| s.abs())
            .fold(0.0f32, f32::max)
    }

    /// `true` if any sample in any channel is NaN.
    #[must_use]
    pub fn any_nan(&self) -> bool {
        self.output
            .iter()
            .flat_map(|c| c.iter())
            .any(|s| s.is_nan())
    }
}

/// Render `num_frames` of silence through `plugin` at
/// `sample_rate`. Useful for "plugin doesn't crash on empty
/// input" smoke tests.
///
/// # Errors
/// Propagates `activate` and `process` failures.
pub fn render_silence<P>(plugin: &mut P, sample_rate: f64, num_frames: usize) -> Result<Rendered>
where
    P: PluginCore + Plugin<f32>,
{
    render(plugin, sample_rate, num_frames, |_ch, _frame| 0.0)
}

/// Render `num_frames` of a generated input through `plugin`.
/// `generator` is called per `(channel, frame)` and returns the
/// input sample at that position.
///
/// # Errors
/// Propagates `activate` and `process` failures.
pub fn render<P, F>(
    plugin: &mut P,
    sample_rate: f64,
    num_frames: usize,
    mut generator: F,
) -> Result<Rendered>
where
    P: PluginCore + Plugin<f32>,
    F: FnMut(usize, usize) -> f32,
{
    let channels = 2usize;
    if !plugin.is_active() {
        plugin.activate(BusLayout::stereo(), sample_rate, num_frames)?;
    }
    let mut input_buf = vec![vec![0.0f32; num_frames]; channels];
    for (ch_idx, ch) in input_buf.iter_mut().enumerate() {
        for (frame, sample) in ch.iter_mut().enumerate() {
            *sample = generator(ch_idx, frame);
        }
    }
    let mut output_buf = vec![vec![0.0f32; num_frames]; channels];
    let bus_in = [BusRange::new(0, channels)];
    let bus_out = [BusRange::new(0, channels)];

    {
        let inputs: Vec<&[f32]> = input_buf.iter().map(Vec::as_slice).collect();
        let mut outputs: Vec<&mut [f32]> = output_buf.iter_mut().map(Vec::as_mut_slice).collect();
        let mut buffer = AudioBuffer::new(&inputs, &mut outputs, num_frames, &bus_in, &bus_out);
        let events = EventList::default();
        let mut out_events = EventList::default();
        let mut ctx = ProcessContext {
            sample_rate,
            max_block_size: num_frames,
            transport: None,
            output_events: &mut out_events,
        };
        plugin.process(&mut buffer, &events, &mut ctx)?;
    }

    Ok(Rendered { output: output_buf })
}

/// Assert that no sample in `rendered` is NaN. Panics if any is.
///
/// # Panics
/// If any sample in any output channel is NaN.
pub fn assert_no_nans(rendered: &Rendered) {
    assert!(!rendered.any_nan(), "rendered audio contained NaN samples");
}

/// Assert that the peak sample is at or below `bound`.
///
/// # Panics
/// If the peak exceeds `bound`.
pub fn assert_peak_below(rendered: &Rendered, bound: f32) {
    let peak = rendered.peak();
    assert!(peak <= bound, "rendered peak {peak} exceeds bound {bound}",);
}

/// Round-trip the plugin's saved state: dump → load → dump,
/// and assert the second dump equals the first. Catches
/// non-deterministic state encoding.
///
/// # Errors
/// Returns whatever `save_state` / `load_state` produces. Returns
/// [`Error::Other`] if the round-tripped bytes differ.
pub fn assert_state_round_trip<P>(plugin: &mut P) -> Result<()>
where
    P: PluginCore,
{
    let first = plugin.save_state()?;
    plugin.load_state(&first)?;
    let second = plugin.save_state()?;
    if first != second {
        return Err(Error::Other(format!(
            "state round-trip mismatch: first {} bytes, second {} bytes",
            first.len(),
            second.len(),
        )));
    }
    Ok(())
}
