//! Shared command-line entry point for the standalone runner.
//!
//! Both the `truce-rack-standalone` binary and the `cargo rack`
//! cargo subcommand (the `cargo-rack` crate) call [`run`] so the
//! flag surface, dispatch, and help text stay in one place. The
//! only difference is the program name shown in usage / help, which
//! the caller passes as `prog`.

use std::ffi::OsString;

use crate::device::{ChannelRoute, DeviceConfig};
use crate::transport::TransportConfig;
use crate::{Format, PluginSelector, RunMode, list_plugins};

/// Parse the CLI args and run the host. `prog` is the program name
/// used in help / usage strings (`truce-rack-standalone` or
/// `cargo rack`); `args` is argv with the program name (and, for
/// the cargo subcommand, the leading `rack`) already stripped.
///
/// The editor window opens by default whenever this build can open
/// one ([`GUI_AVAILABLE`]); pass `--headless` to suppress it, or
/// `--gui` to request it explicitly (an error on a non-`gui` build).
///
/// Exits the process with status 1 on a load/run error and 2 on a
/// usage error, mirroring the historic binary behavior.
// With zero format features enabled every match arm becomes
// `process::exit(2)` and the gathered variables are never read; keep
// the feature-less surface compiling for packagers.
#[cfg_attr(
    not(any(feature = "clap", feature = "vst3", feature = "au")),
    allow(unused_variables, unreachable_code)
)]
pub fn run(prog: &str, args: Vec<OsString>) {
    let mut pargs = pico_args::Arguments::from_vec(args);

    if pargs.contains(["-h", "--help"]) {
        print_help(prog);
        return;
    }

    if pargs.contains("--list") {
        for (fmt, info) in list_plugins() {
            let editor_tag = if info.has_editor { "editor" } else { "-" };
            println!(
                "{} {} {} {} {}",
                fmt.tag(),
                info.vendor,
                info.name,
                info.unique_id,
                editor_tag
            );
        }
        return;
    }

    if pargs.contains("--list-devices") {
        crate::device::list_devices();
        return;
    }

    let format_str: String = pargs
        .opt_value_from_str("--format")
        .unwrap_or(None)
        .unwrap_or_else(|| "clap".into());
    let Some(format) = Format::parse(&format_str) else {
        eprintln!("error: unknown --format {format_str:?} (clap, vst3, au, lv2)");
        std::process::exit(2);
    };

    let id: Option<String> = pargs.opt_value_from_str("--id").unwrap_or(None);
    let name: Option<String> = pargs.opt_value_from_str("--name").unwrap_or(None);
    let seconds: Option<f32> = pargs.opt_value_from_str("--seconds").unwrap_or(None);
    // The editor opens by default on builds that can open one;
    // `--headless` forces it off, `--gui` forces it on (and errors
    // on a non-`gui` build).
    let gui = if pargs.contains("--headless") {
        false
    } else {
        pargs.contains("--gui") || crate::GUI_AVAILABLE
    };

    // Audio device / channel selection (see `crate::device`).
    install_device_config(&mut pargs);

    // Transport flags. The standalone host has no DAW timeline, so
    // it synthesizes one (see `crate::transport`).
    install_transport_config(&mut pargs);

    let leftover = pargs.finish();
    if !leftover.is_empty() {
        eprintln!("unknown arguments: {leftover:?}");
        std::process::exit(2);
    }

    let selector = match (id, name) {
        (Some(id), _) => PluginSelector::Id(id),
        (None, Some(name)) => PluginSelector::Name(name),
        (None, None) => {
            eprintln!(
                "usage: {prog} [--list]\n  \
                 {prog} --format <clap|vst3|au|lv2> --id <unique-id> [--headless] [--seconds N]\n  \
                 {prog} --format <clap|vst3|au|lv2> --name <substring> [--headless] [--seconds N]"
            );
            std::process::exit(2);
        }
    };

    let mode = seconds.map_or(RunMode::UntilSignal, RunMode::Seconds);

    // Explicit type lets the match infer when every arm is a
    // `process::exit(2)` (every format disabled at compile time).
    let result: truce_rack_core::error::Result<()> = match format {
        #[cfg(feature = "clap")]
        Format::Clap => crate::run_clap(&selector, mode, gui),
        #[cfg(not(feature = "clap"))]
        Format::Clap => {
            eprintln!("format clap not enabled in this build");
            std::process::exit(2);
        }
        #[cfg(feature = "vst3")]
        Format::Vst3 => crate::run_vst3(&selector, mode, gui),
        #[cfg(not(feature = "vst3"))]
        Format::Vst3 => {
            eprintln!("format vst3 not enabled in this build");
            std::process::exit(2);
        }
        #[cfg(all(feature = "au", target_vendor = "apple"))]
        Format::Au => crate::run_au(&selector, mode, gui),
        #[cfg(not(all(feature = "au", target_vendor = "apple")))]
        Format::Au => {
            eprintln!("format au not enabled in this build");
            std::process::exit(2);
        }
        #[cfg(feature = "lv2")]
        Format::Lv2 => crate::run_lv2(&selector, mode, gui),
        #[cfg(not(feature = "lv2"))]
        Format::Lv2 => {
            eprintln!("format lv2 not enabled in this build");
            std::process::exit(2);
        }
    };
    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

/// Parse the audio device / channel flags (`--output`,
/// `--output-channels`, `--sample-rate`, `--buffer`) and install the
/// process-wide device config the audio setup reads. Exits with
/// status 2 on a malformed `--output-channels`.
fn install_device_config(pargs: &mut pico_args::Arguments) {
    let output_device: Option<String> = pargs.opt_value_from_str("--output").unwrap_or(None);
    let output_channels: Option<String> =
        pargs.opt_value_from_str("--output-channels").unwrap_or(None);
    let sample_rate: Option<u32> = pargs.opt_value_from_str("--sample-rate").unwrap_or(None);
    let buffer_size: Option<u32> = pargs.opt_value_from_str("--buffer").unwrap_or(None);

    let output_route = match output_channels {
        Some(spec) => {
            let Some(route) = ChannelRoute::parse(&spec) else {
                eprintln!(
                    "error: --output-channels expects `direct`, a channel like `3`, \
                     or a pair like `3-4`, got {spec:?}"
                );
                std::process::exit(2);
            };
            route
        }
        None => ChannelRoute::Direct,
    };

    crate::device::set_config(DeviceConfig {
        output_device,
        output_route,
        sample_rate,
        buffer_size,
    });
}

/// Parse a `<numerator>/<denominator>` time signature like `7/8`.
fn parse_time_sig(s: &str) -> Option<(u32, u32)> {
    let (num, den) = s.split_once('/')?;
    let num: u32 = num.trim().parse().ok()?;
    let den: u32 = den.trim().parse().ok()?;
    if num == 0 || den == 0 {
        return None;
    }
    Some((num, den))
}

/// Parse the `--tempo` / `--time-sig` / `--paused` / `--no-transport`
/// flags and install the process-wide transport config the audio
/// thread reads. Exits with status 2 on a malformed `--time-sig`.
fn install_transport_config(pargs: &mut pico_args::Arguments) {
    let tempo: Option<f64> = pargs.opt_value_from_str("--tempo").unwrap_or(None);
    let time_sig: Option<String> = pargs.opt_value_from_str("--time-sig").unwrap_or(None);
    let paused = pargs.contains("--paused");
    let no_transport = pargs.contains("--no-transport");

    let mut config = TransportConfig {
        enabled: !no_transport,
        playing: !paused,
        ..TransportConfig::default()
    };
    if let Some(bpm) = tempo {
        config.tempo_bpm = bpm;
    }
    if let Some(ts) = time_sig {
        let Some(sig) = parse_time_sig(&ts) else {
            eprintln!("error: --time-sig expects <numerator>/<denominator>, got {ts:?}");
            std::process::exit(2);
        };
        config.time_sig = sig;
    }
    crate::transport::set_config(config);
}

fn print_help(prog: &str) {
    println!(
        "{prog} — host one plugin against the default audio device

USAGE:
  {prog} --list
  {prog} --list-devices
  {prog} --format <clap|vst3|au|lv2> --id <id> [--headless] [--seconds N]
  {prog} --format <clap|vst3|au|lv2> --name <substring> [--headless] [--seconds N]

OPTIONS:
  --list              Print every plugin in every enabled format
  --list-devices      List audio output + input devices and exit
  --format <fmt>      Format to scan (clap, vst3, au, lv2). Default: clap
  --id <id>           Exact unique-id match
  --name <substring>  Case-insensitive substring match against name
  --output <name>     Output device (substring match; default: system)
  --output-channels <spec>
                      Route output to device channels: `direct` (all,
                      default), a channel like `3`, or a pair `3-4`
  --sample-rate <hz>  Output sample rate, e.g. 48000 (default: device)
  --buffer <frames>   Audio buffer size in frames (default: device)
  --gui               Open the plugin's editor in a window (default
                      on `gui` builds; errors on a non-`gui` build)
  --headless          Run without the editor window
  --seconds <n>       Run for n seconds then exit (headless only)
  --tempo <bpm>       Transport tempo in BPM (default: 120)
  --time-sig <n/d>    Transport time signature (default: 4/4)
  --paused            Report transport as stopped (position frozen)
  --no-transport      Report no transport at all to the plugin
  -h, --help          Print this help"
    );
}
