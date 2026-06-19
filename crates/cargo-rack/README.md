# cargo-rack

[![crates.io](https://img.shields.io/crates/v/cargo-rack.svg)](https://crates.io/crates/cargo-rack)
[![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/cargo-rack)

`cargo rack` — a [cargo subcommand][subcmd] that runs the
[**truce-rack**][repo] standalone plugin host. Scan every installed
plugin, load one by id or name, drive it through the default audio
device, and (with `--gui`) embed its editor in a window — without
cloning the workspace or remembering the `cargo run --bin …`
incantation.

It's a thin wrapper: the entire flag surface is shared with the
[`truce-rack-standalone`][standalone] binary via its `cli` module.

[repo]: https://github.com/truce-audio/truce-rack
[standalone]: https://crates.io/crates/truce-rack-standalone
[subcmd]: https://doc.rust-lang.org/cargo/reference/external-tools.html#custom-subcommands

## Install

```bash
# Every format that builds out-of-the-box on your OS.
cargo install cargo-rack

# Add LV2 (needs the system `lilv` library installed).
cargo install cargo-rack --features lv2
```

By default `cargo install cargo-rack` enables every format that
compiles on the host with no extra system libraries — picked per
OS so the install never breaks:

| OS      | Formats on by default | Editor GUI |
| ------- | --------------------- | ---------- |
| macOS   | CLAP, VST3, AU, AU v3 | ✓          |
| Windows | CLAP, VST3            | ✓          |
| Linux   | CLAP, VST3            | — ¹        |

¹ Linux gates the GUI off — baseview's Linux backend drags in
`wayland-sys`. The headless host still runs.

**LV2** is the one opt-in: it links the system `lilv` library
(`brew install lilv`, `apt install liblilv-dev`, …), so it's left
out of the default to keep `cargo install` working everywhere. Add
it with `--features lv2` once `lilv` is present.

## Usage

The plugin's editor opens by default (where the GUI is available);
pass `--headless` to run without a window.

```bash
# List every plugin every enabled scanner can find.
cargo rack --list

# Load one — its editor opens automatically.
cargo rack --format vst3 --name "Surge XT"

# Headless smoke test: render 5 seconds and exit.
cargo rack --format vst3 --name "Surge XT" --headless --seconds 5

# Drive a tempo/grid-synced plugin with a synthesized transport.
cargo rack --format vst3 --name "Surge XT" --tempo 140 --time-sig 7/8

# Pick an output device + channel pair on a multichannel interface.
cargo rack --list-devices
cargo rack --format vst3 --name "Surge XT" --output "Scarlett" --output-channels 3-4
```

| Option              | Meaning                                            |
| ------------------- | -------------------------------------------------- |
| `--list`            | Print every plugin in every enabled format         |
| `--list-devices`    | List audio output + input devices and exit         |
| `--format <fmt>`    | Format to scan (`clap`, `vst3`, `au`, `lv2`)       |
| `--id <id>`         | Exact unique-id match                              |
| `--name <substr>`   | Case-insensitive substring match against the name  |
| `--output <name>`   | Output device (case-insensitive substring)         |
| `--output-channels <spec>` | `direct` (all), a channel `3`, or a pair `3-4` |
| `--sample-rate <hz>`| Output sample rate (falls back if unsupported)     |
| `--buffer <frames>` | Audio buffer size in frames                        |
| `--list-midi`       | List MIDI input devices and exit                   |
| `--midi-input <name>` | MIDI input device (substring; default: all ports) |
| `--midi-channel <spec>` | `omni`/`all` (default) or a channel `1`-`16`   |
| `--headless`        | Run without the editor window                      |
| `--gui`             | Force the editor window on (default where available) |
| `--seconds <n>`     | Run headless for n seconds, then exit              |
| `--tempo <bpm>`     | Transport tempo in BPM (default: 120)              |
| `--time-sig <n/d>`  | Time signature, e.g. `7/8` (default: 4/4)          |
| `--paused`          | Report transport stopped (song position frozen)    |
| `--no-transport`    | Report no transport at all to the plugin           |

## License

MIT or Apache-2.0, at your option.
