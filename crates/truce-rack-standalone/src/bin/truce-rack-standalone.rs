//! CLI runner for [`truce_rack_standalone`]. One plugin in, default
//! audio device out, optionally with the plugin's editor in a
//! window.

// When zero format features are enabled (`--no-default-features`)
// every match arm becomes `process::exit(2)` and the variables
// gathered above are never read. We still want the bin to compile
// so packagers can verify the "feature-less" surface.
#![cfg_attr(
    not(any(feature = "clap", feature = "vst3", feature = "au")),
    allow(unused_variables, unreachable_code)
)]
//!
//! Flags:
//!
//! - `--list` — print every plugin in every enabled format and exit.
//! - `--format <clap|vst3|au>` — which scanner to use (default
//!   `clap`).
//! - `--id <unique-id>` — exact unique-id (CLAP plugin id, VST3 CID
//!   hex, AU 4cc triplet).
//! - `--name <substring>` — case-insensitive substring against the
//!   plugin name. One of `--id` / `--name` is required (unless
//!   `--list` is set).
//! - `--seconds <n>` — run for n seconds then exit (headless only).
//! - `--gui` — open the plugin's editor in a window (requires the
//!   `gui` feature).

use truce_rack_standalone::transport::TransportConfig;
use truce_rack_standalone::{Format, PluginSelector, RunMode, list_plugins};

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
    truce_rack_standalone::transport::set_config(config);
}

fn main() {
    let mut pargs = pico_args::Arguments::from_env();

    if pargs.contains(["-h", "--help"]) {
        print_help();
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

    let format_str: String = pargs
        .opt_value_from_str("--format")
        .unwrap_or(None)
        .unwrap_or_else(|| "clap".into());
    let Some(format) = Format::parse(&format_str) else {
        eprintln!("error: unknown --format {format_str:?} (clap, vst3, au)");
        std::process::exit(2);
    };

    let id: Option<String> = pargs.opt_value_from_str("--id").unwrap_or(None);
    let name: Option<String> = pargs.opt_value_from_str("--name").unwrap_or(None);
    let seconds: Option<f32> = pargs.opt_value_from_str("--seconds").unwrap_or(None);
    let gui = pargs.contains("--gui");

    // Transport flags. The standalone host has no DAW timeline, so
    // it synthesizes one (see `truce_rack_standalone::transport`).
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
                "usage: truce-rack-standalone [--list]\n\
                 truce-rack-standalone --format <clap|vst3|au|lv2> --id <unique-id> [--gui] [--seconds N]\n\
                 truce-rack-standalone --format <clap|vst3|au|lv2> --name <substring> [--gui] [--seconds N]"
            );
            std::process::exit(2);
        }
    };

    let mode = seconds.map_or(RunMode::UntilSignal, RunMode::Seconds);

    // Explicit type lets the match infer when every arm is a
    // `process::exit(2)` (every format disabled at compile time).
    let result: truce_rack_core::error::Result<()> = match format {
        #[cfg(feature = "clap")]
        Format::Clap => truce_rack_standalone::run_clap(&selector, mode, gui),
        #[cfg(not(feature = "clap"))]
        Format::Clap => {
            eprintln!("format clap not enabled in this build");
            std::process::exit(2);
        }
        #[cfg(feature = "vst3")]
        Format::Vst3 => truce_rack_standalone::run_vst3(&selector, mode, gui),
        #[cfg(not(feature = "vst3"))]
        Format::Vst3 => {
            eprintln!("format vst3 not enabled in this build");
            std::process::exit(2);
        }
        #[cfg(all(feature = "au", target_vendor = "apple"))]
        Format::Au => truce_rack_standalone::run_au(&selector, mode, gui),
        #[cfg(not(all(feature = "au", target_vendor = "apple")))]
        Format::Au => {
            eprintln!("format au not enabled in this build");
            std::process::exit(2);
        }
        #[cfg(feature = "lv2")]
        Format::Lv2 => truce_rack_standalone::run_lv2(&selector, mode, gui),
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

fn print_help() {
    println!(
        "truce-rack-standalone — host one plugin against the default audio device

USAGE:
  truce-rack-standalone --list
  truce-rack-standalone --format <clap|vst3|au|lv2> --id <id> [--gui] [--seconds N]
  truce-rack-standalone --format <clap|vst3|au|lv2> --name <substring> [--gui] [--seconds N]

OPTIONS:
  --list              Print every plugin in every enabled format
  --format <fmt>      Format to scan (clap, vst3, au, lv2). Default: clap
  --id <id>           Exact unique-id match
  --name <substring>  Case-insensitive substring match against name
  --gui               Open the plugin's editor in a window (gui feature)
  --seconds <n>       Run for n seconds then exit (headless only)
  --tempo <bpm>       Transport tempo in BPM (default: 120)
  --time-sig <n/d>    Transport time signature (default: 4/4)
  --paused            Report transport as stopped (position frozen)
  --no-transport      Report no transport at all to the plugin
  -h, --help          Print this help"
    );
}
