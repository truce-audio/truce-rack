//! Host-side core for the rack audio-plugin framework.
//!
//! `truce-rack-core` defines the contracts every format wrapper
//! (`truce-rack-clap`, `truce-rack-vst3`, `truce-rack-au`, …) implements: the
//! [`Plugin`] trait, the [`AudioBuffer`] / [`EventList`] /
//! [`ProcessContext`] passed into `process`, the [`BusLayout`]
//! that declares bus topology, and the FFI-edge `catch_unwind`
//! helpers in [`wrapper`].
//!
//! No FFI lives here. The crate compiles on every platform with
//! no system dependencies — a consumer that depends on `truce-rack-core`
//! and on a single format wrapper picks up exactly that format's
//! transitive deps.
//!
//! # Mapping from truce
//!
//! `truce-rack-core` mirrors `truce-core` on the host side. The names
//! parallel deliberately:
//!
//! | truce (plugin)        | rack (host)            |
//! | ---                   | ---                    |
//! | `PluginLogic`         | [`Plugin`]             |
//! | `PluginExport`        | [`PluginScanner`]      |
//! | `AudioBuffer`         | [`AudioBuffer`]        |
//! | `EventList`           | [`EventList`]          |
//! | `ProcessContext`      | [`ProcessContext`]     |
//! | `BusLayout`           | [`BusLayout`]          |
//! | `PluginInfo`          | [`PluginInfo`]         |
//! | `wrapper::run_*`      | [`wrapper`]`::run_*`   |
//!
//! A host targeting one format depends on `truce-rack-core` plus that
//! format's wrapper crate; a multi-format host depends on
//! `truce-rack-core` plus each wrapper it needs.

#![warn(missing_docs)]

pub mod buffer;
pub mod bus;
pub mod editor;
pub mod error;
pub mod events;
pub mod info;
pub mod plugin;
pub mod sample;
pub mod scanner;
pub mod state;
pub mod transport;
pub mod wrapper;

pub use buffer::AudioBuffer;
pub use bus::{BusKind, BusLayout, ChannelConfig};
pub use editor::{PluginEditor, WindowHandle};
pub use error::{Error, Result};
pub use events::{Event, EventBody, EventList, MidiData};
pub use info::{ParameterInfo, PluginCategory, PluginInfo, PresetInfo};
pub use plugin::{Plugin, PluginCore, ProcessContext, ProcessStatus};
pub use sample::Sample;
pub use scanner::PluginScanner;
pub use state::{StateEnvelope, StateLoadError};
pub use transport::TransportInfo;
