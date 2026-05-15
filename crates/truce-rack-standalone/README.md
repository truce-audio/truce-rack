# truce-rack-standalone

[![crates.io](https://img.shields.io/crates/v/truce-rack-standalone.svg)](https://crates.io/crates/truce-rack-standalone)
[docs.rs](https://docs.rs/truce-rack-standalone)

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
cargo run -p truce-rack-standalone --features "gui,vst3,au,au3,lv2" -- --list

# Load a plugin by name. With --gui, embeds its editor in a window;
# without --gui, runs headless until SIGINT (or --seconds N).
cargo run -p truce-rack-standalone --features "gui,vst3,au" -- \
    --format vst3 --name "Surge XT" --gui
```

Default features are `clap` only; opt in to `vst3`, `au`, `au3`,
`lv2` as needed. The `gui` feature pulls baseview + the QWERTY
keyboard MIDI handler. Linux gates `gui` off (baseview drags
`wayland-sys`); the headless path works there.

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
