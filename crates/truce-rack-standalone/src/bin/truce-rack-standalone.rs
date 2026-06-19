//! CLI runner for [`truce_rack_standalone`]. One plugin in, default
//! audio device out, optionally with the plugin's editor in a
//! window.
//!
//! All the flag handling lives in [`truce_rack_standalone::cli`] so
//! the `cargo rack` subcommand (the `cargo-rack` crate) shares it.
//!
//! Flags:
//!
//! - `--list` — print every plugin in every enabled format and exit.
//! - `--format <clap|vst3|au|lv2>` — which scanner to use (default
//!   `clap`).
//! - `--id <unique-id>` — exact unique-id (CLAP plugin id, VST3 CID
//!   hex, AU 4cc triplet).
//! - `--name <substring>` — case-insensitive substring against the
//!   plugin name. One of `--id` / `--name` is required (unless
//!   `--list` is set).
//! - `--seconds <n>` — run for n seconds then exit (headless only).
//! - `--gui` — open the plugin's editor in a window (requires the
//!   `gui` feature).
//! - `--tempo` / `--time-sig` / `--paused` / `--no-transport` —
//!   shape the synthesized host transport.

fn main() {
    let args = std::env::args_os().skip(1).collect();
    truce_rack_standalone::cli::run("truce-rack-standalone", args);
}
