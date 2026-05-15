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

use truce_rack_standalone::{Format, PluginSelector, RunMode, list_plugins};

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
  -h, --help          Print this help"
    );
}
