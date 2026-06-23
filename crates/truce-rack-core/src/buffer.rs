//! Planar audio buffer passed into [`crate::Plugin::process`].
//!
//! `AudioBuffer<S>` is the host-side view of the slices the
//! plugin reads from and writes into. Channels are organised
//! by bus to match how host SDKs natively expose audio — no
//! interleave / deinterleave on the hot path.
//!
//! The lifetime `'a` is the block lifetime: every slice borrows
//! from buffers the host owns. Plugins receive `&mut AudioBuffer`
//! and may write through the output slices but cannot extend
//! the borrows past the `process` call.

use crate::sample::Sample;

/// One channel of input audio.
pub type InputChannel<'a, S> = &'a [S];

/// One channel of output audio.
pub type OutputChannel<'a, S> = &'a mut [S];

/// Per-bus channel range into the buffer's flat channel arrays.
///
/// Format wrappers construct slices of these and hand them to
/// [`AudioBuffer::new`]; plugin code does not need to look at
/// them directly (use [`AudioBuffer::bus_inputs`] /
/// [`AudioBuffer::bus_outputs`]).
#[derive(Debug, Clone, Copy)]
pub struct BusRange {
    /// Inclusive start index into the flat channel array.
    start: usize,
    /// Number of channels in this bus.
    len: usize,
}

/// Mutable, planar audio buffer for one `process` block.
///
/// Channels are flat across buses internally; `bus_inputs(0)` /
/// `bus_outputs(0)` slice into the main bus, higher indices into
/// sidechains and auxiliaries. The flat-then-sliced shape matches
/// host SDK conventions (CLAP's `clap_audio_buffer`, VST3's
/// `ProcessData`, AU's `AudioBufferList`).
///
/// `S` is the sample precision — `f32` for CLAP / VST2 / LV2 /
/// AAX, `f32` or `f64` for VST3 / AU at the host's choice.
pub struct AudioBuffer<'a, S: Sample> {
    /// One slice per input channel, in bus-order.
    inputs: &'a [&'a [S]],
    /// One slice per output channel, in bus-order. The outer slice
    /// is mutable so the plugin can write through it; the inner
    /// `[S]` borrows are independent so two output buses don't
    /// alias.
    outputs: &'a mut [&'a mut [S]],
    /// Number of frames in this block. All channel slices are
    /// exactly this long.
    num_frames: usize,
    /// Per-input-bus channel ranges. `bus_inputs[k]` gives the
    /// `start..start+len` range into `inputs`.
    bus_inputs: &'a [BusRange],
    /// Per-output-bus channel ranges into `outputs`.
    bus_outputs: &'a [BusRange],
}

impl<'a, S: Sample> AudioBuffer<'a, S> {
    /// Build a buffer from raw slices.
    ///
    /// Format wrappers call this once per block from their
    /// `process` callback. Plugin code receives the buffer; only
    /// wrappers construct one.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if any channel slice is shorter
    /// than `num_frames` or the bus ranges don't cover the
    /// channel arrays exactly. Release builds elide the checks —
    /// wrappers are expected to maintain the invariants.
    #[must_use]
    pub fn new(
        inputs: &'a [&'a [S]],
        outputs: &'a mut [&'a mut [S]],
        num_frames: usize,
        bus_inputs: &'a [BusRange],
        bus_outputs: &'a [BusRange],
    ) -> Self {
        debug_assert!(
            inputs.iter().all(|c| c.len() >= num_frames),
            "all input channels must have at least num_frames samples"
        );
        debug_assert!(
            outputs.iter().all(|c| c.len() >= num_frames),
            "all output channels must have at least num_frames samples"
        );
        debug_assert_eq!(
            bus_inputs.iter().map(|r| r.len).sum::<usize>(),
            inputs.len(),
            "input bus ranges must partition the channel array"
        );
        debug_assert_eq!(
            bus_outputs.iter().map(|r| r.len).sum::<usize>(),
            outputs.len(),
            "output bus ranges must partition the channel array"
        );
        Self {
            inputs,
            outputs,
            num_frames,
            bus_inputs,
            bus_outputs,
        }
    }

    /// Block length in samples (frames). Every channel slice is
    /// exactly this long.
    #[must_use]
    pub fn num_frames(&self) -> usize {
        self.num_frames
    }

    /// Number of input buses (including main + sidechains).
    #[must_use]
    pub fn num_input_buses(&self) -> usize {
        self.bus_inputs.len()
    }

    /// Number of output buses.
    #[must_use]
    pub fn num_output_buses(&self) -> usize {
        self.bus_outputs.len()
    }

    /// Total input channels across every bus. Useful for the
    /// "loop over flat channels" pattern when bus layout doesn't
    /// matter for the operation.
    #[must_use]
    pub fn total_input_channels(&self) -> usize {
        self.inputs.len()
    }

    /// Total output channels across every bus.
    #[must_use]
    pub fn total_output_channels(&self) -> usize {
        self.outputs.len()
    }

    /// Flat input channels across every bus, in bus declaration order.
    #[must_use]
    pub fn all_inputs(&self) -> &[InputChannel<'a, S>] {
        self.inputs
    }

    /// Flat mutable output channels across every bus, in bus declaration order.
    pub fn all_outputs(&mut self) -> &mut [&'a mut [S]] {
        self.outputs
    }

    /// Input channels for one bus. `bus_index` is 0 for the main
    /// input; higher indices for sidechains / auxiliaries.
    ///
    /// # Panics
    ///
    /// Panics if `bus_index >= num_input_buses()`.
    #[must_use]
    pub fn bus_inputs(&self, bus_index: usize) -> &[InputChannel<'a, S>] {
        let range = self.bus_inputs[bus_index];
        &self.inputs[range.start..range.start + range.len]
    }

    /// Mutable output channels for one bus.
    ///
    /// # Panics
    ///
    /// Panics if `bus_index >= num_output_buses()`.
    pub fn bus_outputs(&mut self, bus_index: usize) -> &mut [&'a mut [S]] {
        let range = self.bus_outputs[bus_index];
        &mut self.outputs[range.start..range.start + range.len]
    }

    /// Shortcut: input channels of the main bus (index 0). Most
    /// effects use this; reach for [`Self::bus_inputs`] when you
    /// need sidechains too.
    #[must_use]
    pub fn main_inputs(&self) -> &[InputChannel<'a, S>] {
        if self.bus_inputs.is_empty() {
            &[]
        } else {
            self.bus_inputs(0)
        }
    }

    /// Shortcut: mutable output channels of the main bus.
    pub fn main_outputs(&mut self) -> &mut [&'a mut [S]] {
        debug_assert!(
            !self.bus_outputs.is_empty(),
            "main_outputs called on a buffer with no output buses"
        );
        self.bus_outputs(0)
    }
}

impl BusRange {
    /// Construct a range from a `(start, len)` pair. Format
    /// wrappers call this once per bus when building the slice
    /// they pass to [`AudioBuffer::new`].
    #[must_use]
    pub const fn new(start: usize, len: usize) -> Self {
        Self { start, len }
    }
}
