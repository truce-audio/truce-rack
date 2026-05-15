# truce-rack-au

[![crates.io](https://img.shields.io/crates/v/truce-rack-au.svg)](https://crates.io/crates/truce-rack-au)
[![docs.rs](https://docs.rs/truce-rack-au/badge.svg)](https://docs.rs/truce-rack-au)

Audio Unit v2 host implementation for the [**truce-rack**][repo]
audio plugin framework. Apple-only. Pure-Rust on top of the
`objc2` ecosystem (`objc2-audio-toolbox`,
`objc2-core-audio-types`) — no C++ wrapper, no bridging header.
Implements scan, load (both `AudioComponentInstanceNew` and the
async `AudioComponentInstantiate` for v3-flagged components),
audio + MIDI processing, and the AUv2 Cocoa view behind the
format-agnostic [`truce-rack-core`][core] traits.

[repo]: https://github.com/truce-audio/truce-rack
[core]: https://crates.io/crates/truce-rack-core

## Usage

```toml
[target.'cfg(target_vendor = "apple")'.dependencies]
truce-rack-core = "0.9"
truce-rack-au = "0.9"
```

```rust
use truce_rack_core::scanner::PluginScanner;
use truce_rack_au::AuScanner;

let plugins = AuScanner::new().scan()?;
for info in &plugins {
    println!("{info}");
}
```

## See also

The [**truce-rack**][repo] workspace ships sibling wrapper crates
for [CLAP](https://crates.io/crates/truce-rack-clap),
[VST3](https://crates.io/crates/truce-rack-vst3),
[AU v3](https://crates.io/crates/truce-rack-au3),
[LV2](https://crates.io/crates/truce-rack-lv2),
plus a [standalone runner](https://crates.io/crates/truce-rack-standalone).

## License

MIT or Apache-2.0, at your option.
