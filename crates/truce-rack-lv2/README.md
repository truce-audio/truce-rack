# truce-rack-lv2

[![crates.io](https://img.shields.io/crates/v/truce-rack-lv2.svg)](https://crates.io/crates/truce-rack-lv2)
[![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/truce-rack-lv2)

LV2 host implementation for the [**truce-rack**][repo] audio
plugin framework. Built on `lilv-sys` (host-side LV2 discovery /
state library). Implements scan, load, audio + MIDI atom-sequence
processing, and the LV2 UI extension (`CocoaUI` on macOS,
`WindowsUI` on Windows, `X11UI` on Linux) behind the
format-agnostic [`truce-rack-core`][core] traits.

[repo]: https://github.com/truce-audio/truce-rack
[core]: https://crates.io/crates/truce-rack-core

## System dependency

`lilv-sys` links against the system `lilv-0` library:

- macOS: `brew install lilv`
- Debian/Ubuntu: `apt install liblilv-dev`

Without it, `cargo build -p truce-rack-lv2` fails at link time.

## Usage

```toml
[dependencies]
truce-rack-core = "1.0"
truce-rack-lv2 = "1.0"
```

```rust
use truce_rack_core::scanner::PluginScanner;
use truce_rack_lv2::Lv2Scanner;

let plugins = Lv2Scanner::new().scan()?;
for info in &plugins {
    println!("{info}");
}
```

## See also

The [**truce-rack**][repo] workspace ships sibling wrapper crates
for [CLAP](https://crates.io/crates/truce-rack-clap),
[VST3](https://crates.io/crates/truce-rack-vst3),
[AU v2](https://crates.io/crates/truce-rack-au),
[AU v3](https://crates.io/crates/truce-rack-au3),
plus a [standalone runner](https://crates.io/crates/truce-rack-standalone).

## License

MIT or Apache-2.0, at your option.
