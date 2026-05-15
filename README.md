# truce-rack

A Rust library for hosting audio plugins.

`truce-rack` is a from-scratch rewrite of the original [`rack`
0.4.x](https://crates.io/crates/rack) crate — same goal (a clean
host-side Rust API for VST3 / AU / CLAP / LV2 / …), different
shape. Where the old crate was one big package with a heavy C++
`rack-sys` glue layer (cmake, the Steinberg VST3 SDK as a
submodule, an Objective-C++ AU shim), truce-rack is a layered Cargo
workspace of small per-format wrapper crates that talk to native
plugin APIs through pure Rust bindings (`objc2`, `clap-sys`,
`vst3`, `lilv-sys`).

**`truce-rack` doesn't depend on `truce`** and isn't a runtime
extension of it — it's a standalone host library that hosts
plugins of any format (truce-built or otherwise). The shared name
is naming-convention only.

## Workspace layout

```
truce-rack/
├── Cargo.toml              # workspace root
└── crates/
    ├── truce-rack-core/         # Plugin / Scanner / Editor traits, no FFI
    ├── truce-rack-clap/         # CLAP host (clap-sys, pure Rust)
    ├── truce-rack-vst3/         # VST3 host (vst3 community crate)
    ├── truce-rack-au/           # AU v2 host (objc2)
    ├── truce-rack-au3/          # AU v3 host
    ├── truce-rack-lv2/          # LV2 host (lilv-sys)
    ├── truce-rack-standalone/   # cpal + baseview reference host
    └── truce-rack-test/         # assertion helpers for the per-format crates
```

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

## Quick start

```toml
[dependencies]
truce-rack-core = "0.9"
truce-rack-clap = "0.9"   # add one wrapper per format you need
```

```rust
use truce_rack_core::scanner::PluginScanner;
use truce_rack_clap::ClapScanner;

let plugins = ClapScanner::new().scan()?;
for plugin in &plugins {
    println!("{plugin}");
}
```

## Try it standalone

`truce-rack-standalone` is the reference host: scan every enabled
format, load one plugin by id or name, drive it through cpal,
and (with `--gui`) embed its editor in a baseview window.
Hardware MIDI input is always on (CoreMIDI / WinMM / ALSA via
midir).

```bash
# List every plugin every enabled scanner can find.
cargo run -p truce-rack-standalone --features "gui,vst3,au,au3" -- --list

# Load one and play it from a MIDI controller + the QWERTY
# keyboard inside the editor window.
cargo run -p truce-rack-standalone --features "gui,vst3,au" -- \
    --format vst3 --name "Surge XT" --gui

# Headless, exit after N seconds — useful for smoke-testing render.
cargo run -p truce-rack-standalone --features "au" -- \
    --format au --name "AUMIDISynth" --seconds 5
```

Default features are `clap` only; opt in to `vst3`, `au`, `au3`
as needed. The `gui` feature adds the baseview editor host and
the QWERTY-keyboard MIDI handler. Linux gates `gui` off (baseview
drags `wayland-sys`); the headless path works there.

## License

MIT or Apache-2.0, at your option.
