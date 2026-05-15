//! Cross-format audio plugin host.
//!
//! `truce-rack` is the umbrella crate. It re-exports the
//! per-format wrapper crates behind features so a downstream user
//! can opt into exactly the formats they need without juggling
//! version pins across multiple direct dependencies.
//!
//! # Default features
//!
//! `clap` + `vst3` — the two cross-platform mainstream plugin
//! formats. CLAP is pure-Rust, VST3 uses the community `vst3`
//! crate (no Steinberg SDK submodule).
//!
//! # Opt-in features
//!
//! - `au` — Audio Unit v2 host (Apple platforms only)
//! - `au3` — Audio Unit v3 (App Extension) host (Apple platforms only)
//! - `lv2` — LV2 host via `lilv-sys` (requires the system `lilv` library)
//!
//! # Layout
//!
//! Format wrappers are re-exported as nested modules so existing
//! `truce_rack_clap::*` paths can also be reached as
//! `truce_rack::clap::*`:
//!
//! ```ignore
//! use truce_rack::core::scanner::PluginScanner;
//! use truce_rack::clap::ClapScanner;
//!
//! let plugins = ClapScanner::new().scan()?;
//! ```
//!
//! For per-format granular dependency management, depend on the
//! individual `truce-rack-*` crates directly instead.

#![doc(html_root_url = "https://docs.rs/truce-rack/1.0.1")]
// Empty `cargo doc` build with no features enabled would emit
// dead-imports lints; suppress them at the crate level so the
// no-feature `cargo check` stays clean.
#![allow(unused_imports)]

/// Format-agnostic traits and types — `Plugin`, `PluginScanner`,
/// `PluginEditor`, the `EventList` / `MidiData` enums, and shared
/// error / info structs. Always available regardless of features.
pub use truce_rack_core as core;

/// CLAP host. Enabled by default; disable with `default-features = false`.
#[cfg(feature = "clap")]
pub use truce_rack_clap as clap;

/// VST3 host. Enabled by default; disable with `default-features = false`.
#[cfg(feature = "vst3")]
pub use truce_rack_vst3 as vst3;

/// Audio Unit v2 host (Apple platforms). Enable with the `au` feature.
#[cfg(all(feature = "au", target_vendor = "apple"))]
pub use truce_rack_au as au;

/// Audio Unit v3 (App Extension) host (Apple platforms).
/// Enable with the `au3` feature.
#[cfg(all(feature = "au3", target_vendor = "apple"))]
pub use truce_rack_au3 as au3;

/// LV2 host (requires the system `lilv-0` library).
/// Enable with the `lv2` feature.
#[cfg(feature = "lv2")]
pub use truce_rack_lv2 as lv2;
