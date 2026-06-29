//! Audio output device + channel-routing selection for the
//! standalone runner.
//!
//! The host has no settings UI, so the device, channel routing,
//! sample rate, and buffer size come from the CLI (`--output`,
//! `--output-channels`, `--sample-rate`, `--buffer`) via
//! [`set_config`] — the same affordances truce's own
//! `truce-standalone` exposes. Choices are resolved once at startup;
//! there's no runtime device switching.
//!
//! The rack host is output-only (it feeds plugins silence on the
//! input bus), so only the *output* side is selectable here.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use cpal::traits::{DeviceTrait, HostTrait};

use truce_rack_core::error::{Error, Result};

/// How the plugin's output channels map onto the device's channels.
///
/// 1-based channel numbers in CLI specs match what a user reads off
/// their interface; the variants store 0-based `base` indices.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ChannelRoute {
    /// Plugin channel N → device channel N (1:1). The default, and
    /// what every non-multichannel-aware setup wants.
    #[default]
    Direct,
    /// Plugin channels 0 and 1 → device channels `base` and
    /// `base + 1`; every other device channel is silenced.
    Stereo { base: usize },
    /// Plugin channels 0 and 1 fold (sum) into device channel
    /// `base`; every other device channel is silenced.
    Mono { base: usize },
}

impl ChannelRoute {
    /// Parse a CLI / env spec. `direct` / `all` → [`Self::Direct`],
    /// `N` → [`Self::Mono`] on (1-based) channel N, `N-M` (with
    /// `M == N + 1`) → [`Self::Stereo`] starting at N. Returns `None`
    /// for anything malformed.
    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        let s = spec.trim().to_ascii_lowercase();
        if s == "direct" || s == "all" {
            return Some(Self::Direct);
        }
        if let Some((a, b)) = s.split_once('-') {
            let a: usize = a.trim().parse().ok()?;
            let b: usize = b.trim().parse().ok()?;
            // `.then(||)` keeps `a - 1` lazy so it never underflows
            // when the guard rejects (e.g. `0-1`).
            return (a >= 1 && b == a + 1).then(|| Self::Stereo { base: a - 1 });
        }
        let c: usize = s.parse().ok()?;
        (c >= 1).then(|| Self::Mono { base: c - 1 })
    }

    /// Pack into the `usize` the live-route atomic stores. `Direct`
    /// is 0 so a zero-initialized atomic decodes to the default.
    #[must_use]
    pub fn encode(self) -> usize {
        match self {
            Self::Direct => 0,
            Self::Stereo { base } => 1 + base * 2,
            Self::Mono { base } => 2 + base * 2,
        }
    }

    /// Inverse of [`Self::encode`].
    #[must_use]
    pub fn decode(v: usize) -> Self {
        if v == 0 {
            return Self::Direct;
        }
        let k = v - 1;
        if k.is_multiple_of(2) {
            Self::Stereo { base: k / 2 }
        } else {
            Self::Mono { base: (k - 1) / 2 }
        }
    }

    /// Write one block of plugin output into the interleaved device
    /// buffer `out` per this route. `bufs` holds one plane per plugin
    /// output channel; `channels` is the device's channel count.
    /// Allocation-free — safe to call from the audio thread.
    pub fn write(self, out: &mut [f32], bufs: &[Vec<f32>], channels: usize, frames: usize) {
        let n_buf = bufs.len();
        if n_buf == 0 || channels == 0 {
            out.fill(0.0);
            return;
        }
        match self {
            Self::Direct => {
                for frame in 0..frames {
                    for ch in 0..channels {
                        out[frame * channels + ch] = bufs[ch.min(n_buf - 1)][frame];
                    }
                }
            }
            Self::Stereo { base } => {
                out.fill(0.0);
                for frame in 0..frames {
                    if base < channels {
                        out[frame * channels + base] = bufs[0][frame];
                    }
                    if n_buf > 1 && base + 1 < channels {
                        out[frame * channels + base + 1] = bufs[1][frame];
                    }
                }
            }
            Self::Mono { base } if base < channels => {
                out.fill(0.0);
                for frame in 0..frames {
                    let mut v = bufs[0][frame];
                    if n_buf > 1 {
                        v += bufs[1][frame];
                    }
                    out[frame * channels + base] = v;
                }
            }
            Self::Mono { .. } => out.fill(0.0),
        }
    }
}

/// Device + stream choices resolved from the CLI, shared with the
/// audio setup. Read once when the stream opens.
#[derive(Clone, Debug, Default)]
pub struct DeviceConfig {
    /// Output device name (substring, case-insensitive). `None` →
    /// cpal's default output device.
    pub output_device: Option<String>,
    /// How plugin output maps onto device channels.
    pub output_route: ChannelRoute,
    /// Requested sample rate in Hz. `None` → device default; an
    /// unsupported rate falls back to the default with a warning.
    pub sample_rate: Option<u32>,
    /// Requested buffer size in frames. `None` → device default.
    pub buffer_size: Option<u32>,
}

static CONFIG: OnceLock<DeviceConfig> = OnceLock::new();

/// Live output channel route, encoded per [`ChannelRoute::encode`].
/// Read by the audio callback every block and written by the macOS
/// menu's "Output Channels" submenu, so routing can change without
/// restarting the stream. Seeded from the CLI in [`set_config`].
static LIVE_ROUTE: AtomicUsize = AtomicUsize::new(0);

/// Install the process-wide device config. Called once from the CLI
/// after parsing flags; ignored if called twice. Also seeds the
/// live route the audio callback reads.
pub fn set_config(config: DeviceConfig) {
    LIVE_ROUTE.store(config.output_route.encode(), Ordering::Relaxed);
    let _ = CONFIG.set(config);
}

/// The installed device config, or the default (cpal-picked device,
/// `Direct` routing, device-default rate / buffer).
#[must_use]
pub fn config() -> DeviceConfig {
    CONFIG.get().cloned().unwrap_or_default()
}

/// The current (live) output channel route — what the audio
/// callback applies this block.
#[must_use]
pub fn live_route() -> ChannelRoute {
    ChannelRoute::decode(LIVE_ROUTE.load(Ordering::Relaxed))
}

/// Set the live output channel route. Takes effect on the next
/// audio block; used by the macOS "Output Channels" menu.
pub fn set_live_route(route: ChannelRoute) {
    LIVE_ROUTE.store(route.encode(), Ordering::Relaxed);
}

/// Print available output and input devices (default marked), then
/// return. Backs `--list-devices`.
pub fn list_devices() {
    let host = cpal::default_host();
    let default_out = host.default_output_device().and_then(|d| d.name().ok());
    let default_in = host.default_input_device().and_then(|d| d.name().ok());
    println!("Output:");
    print_device_list(default_out.as_deref(), host.output_devices().ok());
    println!("Input:");
    print_device_list(default_in.as_deref(), host.input_devices().ok());
}

fn print_device_list(
    default_name: Option<&str>,
    devices: Option<impl Iterator<Item = cpal::Device>>,
) {
    let Some(devices) = devices else {
        return;
    };
    for device in devices {
        let Ok(name) = device.name() else { continue };
        let marker = if default_name == Some(name.as_str()) {
            " (default)"
        } else {
            ""
        };
        println!("  {name}{marker}");
    }
}

/// Open the configured output device (or cpal's default) and return
/// it alongside its default stream config.
///
/// # Errors
/// When no device matches `--output`, or no default output device
/// exists, or the device's default config can't be queried.
pub fn open_output_device() -> Result<(cpal::Device, cpal::SupportedStreamConfig)> {
    open_output(config().output_device.as_deref())
}

/// Open an output device by `name` (case-insensitive substring), or
/// cpal's default when `name` is `None`, with its default config.
/// Used both at startup and by the menu's live device switch.
///
/// # Errors
/// When no device matches `name`, no default output device exists,
/// or the device's default config can't be queried.
pub fn open_output(name: Option<&str>) -> Result<(cpal::Device, cpal::SupportedStreamConfig)> {
    let host = cpal::default_host();
    let device = match name {
        Some(name) => find_output_device(&host, name)
            .ok_or_else(|| Error::Other(format!("no output device matching {name:?}")))?,
        None => host
            .default_output_device()
            .ok_or_else(|| Error::Other("no default output device".into()))?,
    };
    let supported = device
        .default_output_config()
        .map_err(|e| Error::Other(format!("default_output_config: {e}")))?;
    Ok((device, supported))
}

/// Output device names in cpal order, for the menu's device submenu.
/// Empty if enumeration fails.
#[must_use]
pub fn output_device_names() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

fn find_output_device(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    let needle = name.to_lowercase();
    host.output_devices()
        .ok()?
        .find(|d| d.name().is_ok_and(|n| n.to_lowercase().contains(&needle)))
}

/// Build the cpal `StreamConfig` from the device default, overriding
/// the sample rate / buffer size from the CLI where supported. An
/// unsupported sample rate falls back to the default with a warning.
#[must_use]
pub fn resolve_stream_config(
    device: &cpal::Device,
    default: &cpal::SupportedStreamConfig,
) -> cpal::StreamConfig {
    let cfg = config();
    let mut stream: cpal::StreamConfig = default.config();

    if let Some(sr) = cfg.sample_rate {
        let supported = device.supported_output_configs().is_ok_and(|mut ranges| {
            ranges.any(|r| r.min_sample_rate().0 <= sr && r.max_sample_rate().0 >= sr)
        });
        if supported {
            stream.sample_rate = cpal::SampleRate(sr);
        } else {
            eprintln!(
                "[truce-rack-standalone] sample rate {sr} Hz not supported by this device; \
                 using default {} Hz",
                default.sample_rate().0
            );
        }
    }
    if let Some(frames) = cfg.buffer_size {
        stream.buffer_size = cpal::BufferSize::Fixed(frames);
    }
    stream
}

#[cfg(test)]
mod tests {
    use super::ChannelRoute;

    #[test]
    fn parse_routes() {
        assert_eq!(ChannelRoute::parse("direct"), Some(ChannelRoute::Direct));
        assert_eq!(ChannelRoute::parse("all"), Some(ChannelRoute::Direct));
        assert_eq!(
            ChannelRoute::parse("3"),
            Some(ChannelRoute::Mono { base: 2 })
        );
        assert_eq!(
            ChannelRoute::parse("3-4"),
            Some(ChannelRoute::Stereo { base: 2 })
        );
        // Malformed: non-adjacent pair, zero channel, garbage.
        assert_eq!(ChannelRoute::parse("3-5"), None);
        assert_eq!(ChannelRoute::parse("0"), None);
        assert_eq!(ChannelRoute::parse("left"), None);
    }

    #[test]
    fn stereo_route_writes_chosen_pair() {
        let bufs = vec![vec![1.0f32; 2], vec![2.0f32; 2]];
        let mut out = vec![0.0f32; 8]; // 2 frames * 4 device channels
        ChannelRoute::Stereo { base: 2 }.write(&mut out, &bufs, 4, 2);
        // Frame 0: channels 2,3 carry the plugin pair; 0,1 silent.
        assert_eq!(&out[0..4], &[0.0, 0.0, 1.0, 2.0]);
        assert_eq!(&out[4..8], &[0.0, 0.0, 1.0, 2.0]);
    }

    #[test]
    fn mono_route_folds_down() {
        let bufs = vec![vec![1.0f32; 1], vec![3.0f32; 1]];
        let mut out = vec![0.0f32; 2]; // 1 frame * 2 device channels
        ChannelRoute::Mono { base: 0 }.write(&mut out, &bufs, 2, 1);
        assert_eq!(out, vec![4.0, 0.0]);
    }

    #[test]
    fn encode_decode_roundtrips() {
        for route in [
            ChannelRoute::Direct,
            ChannelRoute::Stereo { base: 0 },
            ChannelRoute::Stereo { base: 3 },
            ChannelRoute::Mono { base: 0 },
            ChannelRoute::Mono { base: 5 },
        ] {
            assert_eq!(ChannelRoute::decode(route.encode()), route);
        }
        // Zero is the default, so a freshly-zeroed atomic is `Direct`.
        assert_eq!(ChannelRoute::decode(0), ChannelRoute::Direct);
    }

    #[test]
    fn live_route_set_get() {
        super::set_live_route(ChannelRoute::Stereo { base: 2 });
        assert_eq!(super::live_route(), ChannelRoute::Stereo { base: 2 });
        super::set_live_route(ChannelRoute::Direct);
        assert_eq!(super::live_route(), ChannelRoute::Direct);
    }
}
