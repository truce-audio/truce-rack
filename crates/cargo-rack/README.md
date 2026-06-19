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
# CLAP only (the default).
cargo install cargo-rack

# Pick the formats you want; add `gui` to embed plugin editors.
cargo install cargo-rack --features "gui,vst3,au,au3,lv2"
```

Default features are `clap` only; opt into `vst3`, `au`, `au3`,
`lv2`, and `gui` as needed (mirroring `truce-rack-standalone`).
Linux gates `gui` off — the headless path works there.

## Usage

```bash
# List every plugin every enabled scanner can find.
cargo rack --list

# Load one and open its editor.
cargo rack --format vst3 --name "Surge XT" --gui

# Headless smoke test: render 5 seconds and exit.
cargo rack --format au --name "AUMIDISynth" --seconds 5

# Drive a tempo/grid-synced plugin with a synthesized transport.
cargo rack --format vst3 --name "Surge XT" --tempo 140 --time-sig 7/8 --gui
```

| Option              | Meaning                                            |
| ------------------- | -------------------------------------------------- |
| `--list`            | Print every plugin in every enabled format         |
| `--format <fmt>`    | Format to scan (`clap`, `vst3`, `au`, `lv2`)       |
| `--id <id>`         | Exact unique-id match                              |
| `--name <substr>`   | Case-insensitive substring match against the name  |
| `--gui`             | Open the plugin's editor in a window (`gui` feature) |
| `--seconds <n>`     | Run headless for n seconds, then exit              |
| `--tempo <bpm>`     | Transport tempo in BPM (default: 120)              |
| `--time-sig <n/d>`  | Time signature, e.g. `7/8` (default: 4/4)          |
| `--paused`          | Report transport stopped (song position frozen)    |
| `--no-transport`    | Report no transport at all to the plugin           |

## License

MIT or Apache-2.0, at your option.
