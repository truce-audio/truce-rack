# truce-rack-standalone

[![crates.io](https://img.shields.io/crates/v/truce-rack-standalone.svg)](https://crates.io/crates/truce-rack-standalone)
[![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/truce-rack-standalone)

Reference standalone host for the [**truce-rack**][repo] audio
plugin framework. cpal for the audio device, `midir` for hardware
MIDI input, optional baseview window with the plugin's editor
embedded as a child. Ships two binaries:

- `truce-rack-standalone` — load one plugin, render through the
  default output device. With `--gui` opens the plugin's editor;
  headless otherwise.
- `truce-rack-screenshot` — walk every installed plugin and
  capture its editor to a PNG (macOS only).

[repo]: https://github.com/truce-audio/truce-rack

## Quick start

```bash
# Scan everything every enabled format scanner can find.
cargo run --bin truce-rack-standalone --features "gui,vst3,au,au3,lv2" -- --list

# Load a plugin by name. On a `gui` build the editor opens by
# default; pass --headless to run without a window (or --seconds N).
cargo run --bin truce-rack-standalone --features "gui,vst3,au" -- --format vst3 --name "Surge XT"

# Drive a tempo/grid-synced plugin with a synthesized transport.
cargo run --bin truce-rack-standalone --features "gui,vst3" -- --format vst3 --name "Surge XT" --tempo 140 --time-sig 7/8
```

Default features are `clap` only; opt in to `vst3`, `au`, `au3`,
`lv2` as needed. The `gui` feature pulls baseview + the QWERTY
keyboard MIDI handler. Linux gates `gui` off (baseview drags
`wayland-sys`); the headless path works there. On a `gui` build the
editor opens by default — pass `--headless` to suppress it.

## Audio device & channels

Playback goes to cpal's default output device at its native rate
unless you override it:

| Flag                      | Meaning                                          |
| ------------------------- | ------------------------------------------------ |
| `--list-devices`          | List audio output + input devices and exit       |
| `--output <name>`         | Output device (case-insensitive substring)       |
| `--output-channels <spec>`| `direct` (all, default), a channel like `3`, or a pair `3-4` |
| `--sample-rate <hz>`      | Output sample rate (falls back if unsupported)   |
| `--buffer <frames>`       | Audio buffer size in frames                      |

`--output-channels` lets a stereo plugin land on, say, outputs 3-4
of a multichannel interface (`3` folds it down to a single output).
The rack host is output-only, so there's no input-device selection.

## Host transport

There's no DAW timeline behind the runner, so it synthesizes one
and passes it to the plugin every block — each format wrapper maps
it to its native transport (VST3 `ProcessContext`, CLAP
`clap_event_transport`, LV2 `time:Position`, AU host callbacks).
Control it with:

| Flag                | Default | Meaning                                          |
| ------------------- | ------- | ------------------------------------------------ |
| `--tempo <bpm>`     | `120`   | Transport tempo in BPM                           |
| `--time-sig <n/d>`  | `4/4`   | Time signature, e.g. `7/8`                       |
| `--paused`          | rolling | Report transport stopped (song position frozen)  |
| `--no-transport`    | off     | Report no transport at all (`transport == None`) |
| `--seconds <n>`     | —       | Run headless for n seconds, then exit            |

## Companion crates

Per-format wrapper crates this binary dispatches to:

- [`truce-rack-core`](https://crates.io/crates/truce-rack-core) — traits + types
- [`truce-rack-clap`](https://crates.io/crates/truce-rack-clap) — CLAP
- [`truce-rack-vst3`](https://crates.io/crates/truce-rack-vst3) — VST3
- [`truce-rack-au`](https://crates.io/crates/truce-rack-au) — Audio Unit v2
- [`truce-rack-au3`](https://crates.io/crates/truce-rack-au3) — Audio Unit v3
- [`truce-rack-lv2`](https://crates.io/crates/truce-rack-lv2) — LV2

## License

MIT or Apache-2.0, at your option.
