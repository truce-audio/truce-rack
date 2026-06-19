# truce-rack

[![crates.io](https://img.shields.io/crates/v/truce-rack.svg)](https://crates.io/crates/truce-rack)
[![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/truce-rack)

A Rust library for hosting audio plugins. CLAP and VST3 by
default; Audio Unit and LV2 behind features.

`truce-rack` is a from-scratch rewrite of the original [`rack`
0.4.x](https://crates.io/crates/rack) crate — same goal (a clean
host-side Rust API for VST3 / AU / CLAP / LV2 / …), different
shape. Where the old crate was one big package with a heavy C++
`rack-sys` glue layer (cmake, the Steinberg VST3 SDK as a
submodule, an Objective-C++ AU shim), truce-rack talks to native
plugin APIs through pure Rust bindings (`objc2`, `clap-sys`,
`vst3`, `lilv-sys`).

**`truce-rack` doesn't depend on [`truce`][truce]** and isn't a
runtime extension of it — it's a standalone host library that
hosts plugins of any format (truce-built or otherwise). The
shared name is naming-convention only.

[truce]: https://github.com/truce-audio/truce

## Quick start

```toml
[dependencies]
truce-rack = "1.0"   # CLAP + VST3 enabled by default
```

```rust
use truce_rack::core::scanner::PluginScanner;
use truce_rack::clap::ClapScanner;

let plugins = ClapScanner::new().scan()?;
for info in &plugins {
    println!("{info}");
}
```

To enable additional formats, opt in via features:

```toml
[dependencies]
truce-rack = { version = "1.0", features = ["au", "au3", "lv2"] }
```

| Feature | Default | Crate it enables                                                       | Platforms      | System library                  |
| ------- | ------- | ---------------------------------------------------------------------- | -------------- | ------------------------------- |
| `clap`  | ✓       | [`truce-rack-clap`](crates/truce-rack-clap)             | all            | none (CLAP header is vendored)  |
| `vst3`  | ✓       | [`truce-rack-vst3`](crates/truce-rack-vst3)             | all            | none (pure-Rust `vst3` crate)   |
| `au`    |         | [`truce-rack-au`](crates/truce-rack-au)                 | Apple          | system AU frameworks            |
| `au3`   |         | [`truce-rack-au3`](crates/truce-rack-au3)               | Apple          | system AU frameworks            |
| `lv2`   |         | [`truce-rack-lv2`](crates/truce-rack-lv2)               | all            | `lilv-0` (see below)            |

For a granular dependency tree (e.g. you only ever want CLAP and
don't care about an umbrella crate's resolver churn), depend on
the per-format crates directly instead.

## Try it standalone

[`truce-rack-standalone`](crates/truce-rack-standalone) is the
reference host: scans every enabled format, loads one plugin by
id or name, drives it through cpal, and (with `--gui`) embeds
its editor in a baseview window. Hardware MIDI input is always
on (CoreMIDI / WinMM / ALSA via midir).

The quickest way in is [`cargo-rack`](crates/cargo-rack), which
installs it as a `cargo rack` subcommand — every format that builds
on your OS, no clone required:

```bash
cargo install cargo-rack          # add `--features lv2` for LV2
cargo rack --list
# Editor opens by default; add `--headless` to run without a window.
cargo rack --format vst3 --name "Surge XT" --tempo 140 --time-sig 7/8
```

Or run it straight from a workspace checkout, choosing formats with
`--features`:

```bash
# List every plugin every enabled scanner can find.
cargo run --bin truce-rack-standalone --features "gui,vst3,au,au3,lv2" -- --list

# List audio output + input devices and exit.
cargo run --bin truce-rack-standalone --features "vst3" -- --list-devices

# Load one and play it from a MIDI controller + the QWERTY keyboard.
# On a `gui` build the editor opens by default.
cargo run --bin truce-rack-standalone --features "gui,vst3,au" -- --format vst3 --name "Surge XT"

# Pick an output device + channel pair on a multichannel interface.
cargo run --bin truce-rack-standalone --features "vst3" -- --format vst3 --name "Surge XT" --output "Scarlett" --output-channels 3-4

# Headless, exit after N seconds — useful for smoke-testing render.
cargo run --bin truce-rack-standalone --features "gui,vst3" -- --format vst3 --name "Surge XT" --headless --seconds 5

# Drive tempo/grid-synced plugins with a synthesized transport
# (140 BPM, 7/8). Works in every format.
cargo run --bin truce-rack-standalone --features "gui,vst3" -- --format vst3 --name "Surge XT" --tempo 140 --time-sig 7/8
```

On macOS add `gui`, `au`, `au3` to the feature list to embed the
plugin's editor and scan AU plugins. On Linux `gui` is gated off;
the headless path works. On a `gui` build the editor opens by
default — pass `--headless` to run without a window.

### Audio device & channels

The host plays through cpal's default output device at the device's
native rate. Override any of that from the CLI: `--list-devices`
prints the available devices, `--output <name>` selects one by
substring, `--output-channels <spec>` routes the plugin onto
specific device channels (`direct` for all, `3` for mono on channel
3, `3-4` for a stereo pair), and `--sample-rate` / `--buffer` set
the stream format.

For MIDI: `--list-midi` prints input ports, `--midi-input <name>`
opens only the ports matching a substring (default: all), and
`--midi-channel <spec>` filters to one channel (`omni`/`all` or
`1`-`16`).

On macOS, the windowed runner (`--gui`) also exposes all of this as
a live **Settings** menu — Output Device, Output Channels, MIDI
Input, and MIDI Channel — so you can switch any of them while the
plugin is running, no restart required.

### Host transport

The runner has no DAW timeline, so it synthesizes one and feeds it
to the plugin every block as host transport (tempo, time signature,
song position, bar, play state). Each format wrapper translates it
into the backend's native representation — VST3 `ProcessContext`,
CLAP `clap_event_transport`, an LV2 `time:Position` atom, and the
AU host callbacks — so tempo delays, LFO sync, arpeggiators, and
sequencers behave as they would under a real host.

| Flag             | Default | Meaning                                            |
| ---------------- | ------- | -------------------------------------------------- |
| `--tempo <bpm>`  | `120`   | Transport tempo in BPM                             |
| `--time-sig <n/d>` | `4/4` | Time signature, e.g. `7/8`                         |
| `--paused`       | rolling | Report transport stopped (song position frozen)    |
| `--no-transport` | off     | Report no transport at all (`transport == None`)   |

## Format coverage

| Format | Crate              | Scan | Load | Process | MIDI | GUI |
| ------ | ------------------ | ---- | ---- | ------- | ---- | --- |
| CLAP   | `truce-rack-clap`       | ✓    | ✓    | ✓       | ✓    | ✓   |
| VST3   | `truce-rack-vst3`       | ✓    | ✓    | ✓       | ✓    | ✓   |
| AU v2  | `truce-rack-au`         | ✓    | ✓    | ✓       | ✓    | ✓   |
| AU v3  | `truce-rack-au3`        | ✓    | ✓    | ✓       | ✓    | ✓   |
| LV2    | `truce-rack-lv2`        | ✓    | ✓    | ✓       | ✓    | ✓ ¹ |
| VST2   | —                  | not planned (legacy SDK) |
| AAX    | —                  | deferred                 |

¹ LV2 UI: native-class only (`CocoaUI` on macOS, `WindowsUI`
on Windows, `X11UI` on Linux). The UI bundle binary is dlopen'd
and instantiated as a child of the host's parent window with
the `urid#map`, `ui#parent`, and `ui#resize` features.
`write_function` (UI → host) and `port_event` (host → UI) carry
control-port changes; `idle` and `resize` extension interfaces
are queried at open and driven by the host. Toolkit-specific UI
classes (`Gtk*UI`, `Qt*UI`) would still need a `suil`-style
wrapper — that's the only remaining gap.

## Building from source

Rust stable (1.75 or newer) via [rustup](https://rustup.rs), plus
a C toolchain (clang on macOS, gcc on Linux, MSVC on Windows).
Below covers the system libraries each format needs.

### macOS

```bash
xcode-select --install         # clang + AU frameworks
brew install lilv              # only if you want LV2
cargo build
```

Apple Silicon Homebrew installs `lilv` to `/opt/homebrew/opt/lilv`;
Intel Homebrew to `/usr/local/opt/lilv`. `truce-rack-lv2/build.rs`
adds both to the linker search path.

### Linux (Debian / Ubuntu)

```bash
sudo apt install build-essential pkg-config \
    libasound2-dev \
    liblilv-dev    # only if you want LV2
cargo build
```

- `libasound2-dev` — ALSA, pulled in by cpal (audio out) and
  midir (MIDI in).
- `liblilv-dev` — LV2 host library. The error
  `rust-lld: error: unable to find library -llilv-0` means it's
  missing.

The `gui` feature on `truce-rack-standalone` is gated off on
Linux (baseview's Linux backend drags `wayland-sys`); the
headless path works. To skip LV2 entirely:

```bash
cargo build --bin truce-rack-standalone --no-default-features --features clap
```

**Fedora / RHEL:**

```bash
sudo dnf install alsa-lib-devel lilv-devel
```

**Arch:**

```bash
sudo pacman -S alsa-lib lilv
```

### Windows

WASAPI (audio) and WinMM (MIDI) come from the system — no extra
install. Only LV2 needs a system library.

```powershell
git clone https://github.com/microsoft/vcpkg
.\vcpkg\bootstrap-vcpkg.bat
.\vcpkg\vcpkg install lilv:x64-windows
$env:LIB = "$PWD\vcpkg\installed\x64-windows\lib;$env:LIB"
cargo build
```

To skip LV2:

```powershell
cargo build --bin truce-rack-standalone --no-default-features --features clap,vst3
```

## Workspace layout

```
truce-rack/
├── Cargo.toml              # workspace root
└── crates/
    ├── truce-rack/         # umbrella — re-exports formats by feature
    ├── truce-rack-core/    # Plugin / Scanner / Editor traits, no FFI
    ├── truce-rack-clap/    # CLAP host (clap-sys, pure Rust)
    ├── truce-rack-vst3/    # VST3 host (vst3 community crate)
    ├── truce-rack-au/      # AU v2 host (objc2)
    ├── truce-rack-au3/     # AU v3 host
    ├── truce-rack-lv2/     # LV2 host (lilv-sys)
    ├── truce-rack-standalone/   # cpal + baseview reference host
    ├── cargo-rack/         # `cargo rack` subcommand wrapping the standalone host
    └── truce-rack-test/    # assertion helpers for the per-format crates
```

| Crate | crates.io | docs.rs |
| ----- | --------- | ------- |
| [`truce-rack`](crates/truce-rack) | [![crates.io](https://img.shields.io/crates/v/truce-rack.svg)](https://crates.io/crates/truce-rack) | [![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/truce-rack) |
| [`truce-rack-core`](crates/truce-rack-core) | [![crates.io](https://img.shields.io/crates/v/truce-rack-core.svg)](https://crates.io/crates/truce-rack-core) | [![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/truce-rack-core) |
| [`truce-rack-clap`](crates/truce-rack-clap) | [![crates.io](https://img.shields.io/crates/v/truce-rack-clap.svg)](https://crates.io/crates/truce-rack-clap) | [![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/truce-rack-clap) |
| [`truce-rack-vst3`](crates/truce-rack-vst3) | [![crates.io](https://img.shields.io/crates/v/truce-rack-vst3.svg)](https://crates.io/crates/truce-rack-vst3) | [![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/truce-rack-vst3) |
| [`truce-rack-au`](crates/truce-rack-au) | [![crates.io](https://img.shields.io/crates/v/truce-rack-au.svg)](https://crates.io/crates/truce-rack-au) | [![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/truce-rack-au) |
| [`truce-rack-au3`](crates/truce-rack-au3) | [![crates.io](https://img.shields.io/crates/v/truce-rack-au3.svg)](https://crates.io/crates/truce-rack-au3) | [![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/truce-rack-au3) |
| [`truce-rack-lv2`](crates/truce-rack-lv2) | [![crates.io](https://img.shields.io/crates/v/truce-rack-lv2.svg)](https://crates.io/crates/truce-rack-lv2) | [![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/truce-rack-lv2) |
| [`truce-rack-standalone`](crates/truce-rack-standalone) | [![crates.io](https://img.shields.io/crates/v/truce-rack-standalone.svg)](https://crates.io/crates/truce-rack-standalone) | [![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/truce-rack-standalone) |
| [`cargo-rack`](crates/cargo-rack) | [![crates.io](https://img.shields.io/crates/v/cargo-rack.svg)](https://crates.io/crates/cargo-rack) | [![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/cargo-rack) |
| [`truce-rack-test`](crates/truce-rack-test) | [![crates.io](https://img.shields.io/crates/v/truce-rack-test.svg)](https://crates.io/crates/truce-rack-test) | [![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/truce-rack-test) |

## Troubleshooting

- **`unable to find library -llilv-0`** — install the LV2 host
  library: `liblilv-dev` (Debian/Ubuntu), `lilv-devel`
  (Fedora/RHEL), `lilv` (Arch), `brew install lilv` (macOS),
  `vcpkg install lilv` (Windows). Or skip LV2 with
  `--no-default-features --features clap,vst3`.
- **`ALSA lib ... cannot find card`** on Linux — no audio device
  is selected. Pass `--seconds N` to run without opening one, or
  check `aplay -l`.
- **`error: linker 'cc' not found`** — install your platform's C
  toolchain (Xcode CLT, `build-essential`, MSVC Build Tools).

## License

MIT or Apache-2.0, at your option.
