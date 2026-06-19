//! cpal-driven standalone host for truce-rack.
//!
//! Loads a single plugin via the appropriate format wrapper,
//! opens the default cpal output device, and renders the plugin
//! into the device's audio stream. Optionally also opens a
//! baseview window and embeds the plugin's editor — see the `gui`
//! feature and the `windowed` module.
//!
//! # Layout
//!
//! - `run_clap` / `run_vst3` / `run_au` / `run_lv2` — one entry point
//!   per format wrapper. Each is feature-gated.
//! - [`run_with_plugin`] — common cpal plumbing every headless
//!   entry point uses.
//! - [`list_plugins`] — scan every enabled format and print one
//!   line per discovered plugin (powers `--list`).

use truce_rack_core::buffer::{AudioBuffer, BusRange};
use truce_rack_core::bus::BusLayout;
use truce_rack_core::error::{Error, Result};
use truce_rack_core::events::EventList;
use truce_rack_core::info::PluginInfo;
use truce_rack_core::plugin::{Plugin, PluginCore, ProcessContext};

pub mod cli;
pub mod device;
pub mod midi;
pub mod midi_queue;
pub mod transport;
#[cfg(any(
    feature = "clap",
    feature = "vst3",
    feature = "lv2",
    all(feature = "au", target_vendor = "apple"),
))]
use truce_rack_core::scanner::PluginScanner;

use cpal::Stream;
use cpal::traits::{DeviceTrait, StreamTrait};

#[cfg(feature = "gui")]
pub mod keyboard;
#[cfg(feature = "gui")]
pub mod windowed;

#[cfg(all(target_os = "macos", feature = "gui"))]
pub mod menu_macos;

#[cfg(target_os = "macos")]
pub mod screenshot;

/// Whether this build can open a plugin editor window — i.e. the
/// `gui` feature is compiled in. `false` on Linux (baseview's Linux
/// backend is gated off) and on any `--no-default-features` build.
/// Callers use it to decide whether `--gui` can be the default.
pub const GUI_AVAILABLE: bool = cfg!(feature = "gui");

/// What the standalone runner wants to do once it has a stream
/// open: stay alive for `seconds`, or block until a user sends
/// SIGINT.
#[derive(Debug, Clone, Copy)]
pub enum RunMode {
    /// Block this many seconds, then return.
    Seconds(f32),
    /// Block until SIGINT / `^C`.
    UntilSignal,
}

/// Which format the CLI picked. Used by the dispatcher so a single
/// `--name "Foo"` flag can resolve against the right scanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// CLAP.
    Clap,
    /// VST3.
    Vst3,
    /// Audio Unit v2.
    Au,
    /// LV2.
    Lv2,
}

impl Format {
    /// Parse `clap` / `vst3` / `au` / `lv2` (case-insensitive).
    /// Returns `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "clap" => Some(Self::Clap),
            "vst3" => Some(Self::Vst3),
            "au" => Some(Self::Au),
            "lv2" => Some(Self::Lv2),
            _ => None,
        }
    }

    /// Human-readable tag used in `--list` output and screenshot
    /// filenames.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Clap => "clap",
            Self::Vst3 => "vst3",
            Self::Au => "au",
            Self::Lv2 => "lv2",
        }
    }
}

/// Open a CLAP plugin by id or name and run it.
///
/// `gui` opens a windowed runner that embeds the plugin's editor;
/// without `gui` (or with `gui` disabled at compile time) the
/// runner is headless and only drives the audio thread.
///
/// # Errors
/// Propagates scanner / load / activate failures and cpal device
/// errors.
#[cfg(feature = "clap")]
pub fn run_clap(selector: &PluginSelector, mode: RunMode, gui: bool) -> Result<()> {
    let scanner = truce_rack_clap::ClapScanner::new();
    let plugin = load_by_selector(&scanner, selector)?;
    dispatch_run(plugin, mode, gui)
}

/// Open a VST3 plugin by id or name and run it.
///
/// # Errors
/// Same shape as [`run_clap`].
#[cfg(feature = "vst3")]
pub fn run_vst3(selector: &PluginSelector, mode: RunMode, gui: bool) -> Result<()> {
    let scanner = truce_rack_vst3::Vst3Scanner::new();
    let plugin = load_by_selector(&scanner, selector)?;
    dispatch_run(plugin, mode, gui)
}

/// Open an Audio Unit v2 plugin by id or name and run it.
///
/// # Errors
/// Same shape as [`run_clap`].
#[cfg(all(feature = "au", target_vendor = "apple"))]
pub fn run_au(selector: &PluginSelector, mode: RunMode, gui: bool) -> Result<()> {
    let scanner = truce_rack_au::AuScanner::new();
    let plugin = load_by_selector(&scanner, selector)?;
    dispatch_run(plugin, mode, gui)
}

/// Open an LV2 plugin by URI or name and run it.
///
/// # Errors
/// Same shape as [`run_clap`].
#[cfg(feature = "lv2")]
pub fn run_lv2(selector: &PluginSelector, mode: RunMode, gui: bool) -> Result<()> {
    let scanner = truce_rack_lv2::Lv2Scanner::new();
    let plugin = load_by_selector(&scanner, selector)?;
    dispatch_run(plugin, mode, gui)
}

/// How the CLI identified the plugin it wants to load. `Id` is an
/// exact unique-id match (CLAP plugin id, VST3 CID hex, AU 4cc
/// triplet). `Name` is a case-insensitive substring against the
/// plugin's display name.
#[derive(Debug, Clone)]
pub enum PluginSelector {
    /// Exact unique-id match.
    Id(String),
    /// Case-insensitive substring of the display name.
    Name(String),
}

// Used only by the per-format `run_*` entry points, all of which
// are feature-gated.
#[cfg(any(
    feature = "clap",
    feature = "vst3",
    feature = "lv2",
    all(feature = "au", target_vendor = "apple"),
))]
fn load_by_selector<S>(scanner: &S, selector: &PluginSelector) -> Result<S::Plugin>
where
    S: PluginScanner,
{
    let entries = scanner.scan()?;
    let info = match selector {
        PluginSelector::Id(id) => entries
            .into_iter()
            .find(|p| p.unique_id == *id || p.name == *id),
        PluginSelector::Name(name) => {
            let needle = name.to_ascii_lowercase();
            entries
                .into_iter()
                .find(|p| p.name.to_ascii_lowercase().contains(&needle))
        }
    };
    let info = info.ok_or_else(|| {
        let label = match selector {
            PluginSelector::Id(s) | PluginSelector::Name(s) => s.clone(),
        };
        Error::PluginNotFound(label)
    })?;
    scanner.load(&info)
}

// Only referenced by the per-format `run_clap` / `run_vst3` /
// `run_au` entry points, all of which are themselves feature-gated.
#[cfg(any(
    feature = "clap",
    feature = "vst3",
    feature = "lv2",
    all(feature = "au", target_vendor = "apple"),
))]
#[cfg(feature = "gui")]
fn dispatch_run<P>(plugin: P, mode: RunMode, gui: bool) -> Result<()>
where
    P: PluginCore + Plugin<f32> + Send + 'static,
{
    if gui {
        windowed::run(plugin)
    } else {
        run_with_plugin(plugin, mode)
    }
}

#[cfg(any(
    feature = "clap",
    feature = "vst3",
    feature = "lv2",
    all(feature = "au", target_vendor = "apple"),
))]
#[cfg(not(feature = "gui"))]
fn dispatch_run<P>(plugin: P, mode: RunMode, gui: bool) -> Result<()>
where
    P: PluginCore + Plugin<f32> + Send + 'static,
{
    if gui {
        return Err(Error::Other(
            "--gui requires the `gui` feature to be enabled at build time".into(),
        ));
    }
    run_with_plugin(plugin, mode)
}

/// Open the default cpal output device and pump `plugin` into
/// it, running for `mode`.
///
/// # Errors
/// Activation, cpal device opening, or stream-start failures.
pub fn run_with_plugin<P>(plugin: P, mode: RunMode) -> Result<()>
where
    P: PluginCore + Plugin<f32> + Send + 'static,
{
    let (device, supported) = device::open_output_device()?;
    let config = device::resolve_stream_config(&device, &supported);
    let sample_rate = f64::from(config.sample_rate.0);
    let channels = usize::from(config.channels.max(1));
    let max_block = 1024usize;

    let stream = build_audio_stream(plugin, &device, &config, sample_rate, channels, max_block)?;
    stream
        .play()
        .map_err(|e| Error::Other(format!("stream.play: {e}")))?;

    // Hardware MIDI in: held for the lifetime of the run so the
    // headless mode is also playable from a connected controller.
    let _midi_in = midi::MidiInputThread::start();

    match mode {
        RunMode::Seconds(secs) => {
            std::thread::sleep(std::time::Duration::from_secs_f32(secs));
        }
        RunMode::UntilSignal => loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        },
    }
    drop(stream);
    Ok(())
}

fn build_audio_stream<P>(
    mut plugin: P,
    device: &cpal::Device,
    stream_config: &cpal::StreamConfig,
    sample_rate: f64,
    channels: usize,
    max_block: usize,
) -> Result<Stream>
where
    P: PluginCore + Plugin<f32> + Send + 'static,
{
    plugin.activate(BusLayout::stereo(), sample_rate, max_block)?;
    let stream_config = stream_config.clone();

    let mut input_buf = vec![vec![0.0f32; max_block]; channels];
    let mut output_buf = vec![vec![0.0f32; max_block]; channels];
    let bus_in = vec![BusRange::new(0, channels)];
    let bus_out = vec![BusRange::new(0, channels)];
    let mut clock = transport::TransportClock::new();

    let stream = device
        .build_output_stream(
            &stream_config,
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let frames = out.len() / channels.max(1);

                for ch in &mut input_buf {
                    if ch.len() < frames {
                        ch.resize(frames, 0.0);
                    }
                    for v in &mut ch[..frames] {
                        *v = 0.0;
                    }
                }
                for ch in &mut output_buf {
                    if ch.len() < frames {
                        ch.resize(frames, 0.0);
                    }
                    for v in &mut ch[..frames] {
                        *v = 0.0;
                    }
                }

                {
                    let inputs: Vec<&[f32]> = input_buf.iter().map(|c| &c[..frames]).collect();
                    let mut outputs: Vec<&mut [f32]> =
                        output_buf.iter_mut().map(|c| &mut c[..frames]).collect();

                    let mut buffer =
                        AudioBuffer::new(&inputs, &mut outputs, frames, &bus_in, &bus_out);
                    let mut events = EventList::default();
                    midi_queue::drain_into(&mut events);
                    let mut out_events = EventList::default();
                    let mut ctx = ProcessContext {
                        sample_rate,
                        max_block_size: max_block,
                        transport: clock.next_block(frames, sample_rate),
                        output_events: &mut out_events,
                    };
                    let _ = plugin.process(&mut buffer, &events, &mut ctx);
                }

                device::live_route().write(out, &output_buf, channels, frames);
            },
            move |err| eprintln!("[truce-rack-standalone] stream error: {err}"),
            None,
        )
        .map_err(|e| Error::Other(format!("build_output_stream: {e}")))?;
    Ok(stream)
}

/// Scan every format compiled into this build and return a flat
/// list of `(format, info)` pairs. Used by `--list` and by the
/// screenshot bin's per-plugin walk.
///
/// CLAP / VST3 / AU `has_editor` is only known post-load — scanning
/// alone reports `false`. The caller is responsible for loading
/// each entry if they want truth on the editor field.
#[must_use]
pub fn list_plugins() -> Vec<(Format, PluginInfo)> {
    // `mut` is conditional — only the cfg-on branches push into
    // `out`. Suppress the warning for the all-features-off build.
    #[allow(unused_mut)]
    let mut out: Vec<(Format, PluginInfo)> = Vec::new();

    #[cfg(feature = "clap")]
    if let Ok(entries) = truce_rack_clap::ClapScanner::new().scan() {
        for e in entries {
            out.push((Format::Clap, e));
        }
    }

    #[cfg(feature = "vst3")]
    if let Ok(entries) = truce_rack_vst3::Vst3Scanner::new().scan() {
        for e in entries {
            out.push((Format::Vst3, e));
        }
    }

    #[cfg(all(feature = "au", target_vendor = "apple"))]
    if let Ok(entries) = truce_rack_au::AuScanner::new().scan() {
        for e in entries {
            out.push((Format::Au, e));
        }
    }

    #[cfg(feature = "lv2")]
    if let Ok(entries) = truce_rack_lv2::Lv2Scanner::new().scan() {
        for e in entries {
            out.push((Format::Lv2, e));
        }
    }

    out
}
