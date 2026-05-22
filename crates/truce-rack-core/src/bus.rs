//! Bus topology declaration.
//!
//! A [`BusLayout`] is the host-side description of one of the
//! audio bus configurations a plugin can operate in. Plugins
//! advertise multiple layouts (mono, stereo, stereo + sidechain,
//! 5.1, …); the host picks one before [`crate::PluginCore::activate`].
//!
//! Mirrors `truce_core::bus::BusLayout`. Repeated rather than
//! shared so a rack consumer doesn't transitively pull in any
//! truce-plugin-side code.

use smallvec::{SmallVec, smallvec};

/// Channel count and grouping for one bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelConfig {
    /// 1 channel.
    Mono,
    /// 2 channels (L, R).
    Stereo,
    /// 6 channels (L, R, C, LFE, Ls, Rs).
    Surround5_1,
    /// 8 channels (L, R, C, LFE, Ls, Rs, Lb, Rb).
    Surround7_1,
    /// Arbitrary channel count for hosts that don't fit the
    /// canonical configs.
    Discrete(u32),
}

impl ChannelConfig {
    /// Number of channels this config carries.
    #[must_use]
    pub const fn count(self) -> u32 {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::Surround5_1 => 6,
            Self::Surround7_1 => 8,
            Self::Discrete(n) => n,
        }
    }
}

/// What a bus carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusKind {
    /// Main audio path. Every layout has exactly one main input
    /// bus (or zero for instruments) and one main output bus.
    Main,
    /// Sidechain or additional auxiliary input — driven by the
    /// host's routing UI, fed independently from the main bus.
    Sidechain,
    /// Auxiliary output beyond the main bus (multi-out
    /// instruments, split-out compressor diagnostics, etc.).
    Auxiliary,
}

/// One bus's declaration: name, kind, channel config.
#[derive(Debug, Clone)]
pub struct Bus {
    /// Display name for the host UI.
    pub name: String,
    /// Bus role.
    pub kind: BusKind,
    /// Channel grouping / count.
    pub channels: ChannelConfig,
}

/// One complete I/O topology the plugin supports.
///
/// Hosts iterate over a plugin's declared layouts and pick one
/// before activation. After activation the layout is fixed until
/// [`crate::PluginCore::deactivate`] is called.
#[derive(Debug, Clone)]
pub struct BusLayout {
    /// Input buses in declaration order. Index 0 is the main
    /// input (when present); later indices are sidechains /
    /// auxiliaries.
    pub inputs: SmallVec<[Bus; 2]>,
    /// Output buses in declaration order. Index 0 is the main
    /// output; later indices are auxiliaries.
    pub outputs: SmallVec<[Bus; 2]>,
}

impl BusLayout {
    /// An empty layout — no audio buses. Useful for MIDI-only
    /// plugins.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inputs: SmallVec::new(),
            outputs: SmallVec::new(),
        }
    }

    /// Mono-in, mono-out, no sidechains. The simplest effect
    /// layout.
    #[must_use]
    pub fn mono() -> Self {
        Self {
            inputs: smallvec![Bus::main("Input", ChannelConfig::Mono)],
            outputs: smallvec![Bus::main("Output", ChannelConfig::Mono)],
        }
    }

    /// Stereo-in, stereo-out, no sidechains.
    #[must_use]
    pub fn stereo() -> Self {
        Self {
            inputs: smallvec![Bus::main("Input", ChannelConfig::Stereo)],
            outputs: smallvec![Bus::main("Output", ChannelConfig::Stereo)],
        }
    }

    /// Stereo + sidechain input, stereo output. Compressors,
    /// gates, vocoders.
    #[must_use]
    pub fn stereo_with_sidechain(sidechain_name: &str) -> Self {
        Self {
            inputs: smallvec![
                Bus::main("Input", ChannelConfig::Stereo),
                Bus::sidechain(sidechain_name, ChannelConfig::Stereo),
            ],
            outputs: smallvec![Bus::main("Output", ChannelConfig::Stereo)],
        }
    }

    /// Total input channels across every bus.
    #[must_use]
    pub fn total_input_channels(&self) -> u32 {
        self.inputs.iter().map(|b| b.channels.count()).sum()
    }

    /// Total output channels across every bus.
    #[must_use]
    pub fn total_output_channels(&self) -> u32 {
        self.outputs.iter().map(|b| b.channels.count()).sum()
    }
}

impl Default for BusLayout {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus {
    /// A main-bus shorthand.
    #[must_use]
    pub fn main(name: &str, channels: ChannelConfig) -> Self {
        Self {
            name: name.to_string(),
            kind: BusKind::Main,
            channels,
        }
    }

    /// A sidechain-bus shorthand.
    #[must_use]
    pub fn sidechain(name: &str, channels: ChannelConfig) -> Self {
        Self {
            name: name.to_string(),
            kind: BusKind::Sidechain,
            channels,
        }
    }

    /// An auxiliary-bus shorthand.
    #[must_use]
    pub fn auxiliary(name: &str, channels: ChannelConfig) -> Self {
        Self {
            name: name.to_string(),
            kind: BusKind::Auxiliary,
            channels,
        }
    }
}
