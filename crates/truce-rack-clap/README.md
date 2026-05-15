# truce-rack-clap

[![crates.io](https://img.shields.io/crates/v/truce-rack-clap.svg)](https://crates.io/crates/truce-rack-clap)
[![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/truce-rack-clap)

CLAP host implementation for the [**truce-rack**][repo] audio
plugin framework. Pure-Rust on top of `clap-sys`. Implements scan,
load, audio + MIDI processing, and the CLAP GUI extension behind
the format-agnostic [`truce-rack-core`][core] traits.

[repo]: https://github.com/truce-audio/truce-rack
[core]: https://crates.io/crates/truce-rack-core

## Usage

```toml
[dependencies]
truce-rack-core = "1.0"
truce-rack-clap = "1.0"
```

```rust
use truce_rack_core::scanner::PluginScanner;
use truce_rack_clap::ClapScanner;

let plugins = ClapScanner::new().scan()?;
for info in &plugins {
    println!("{info}");
}
```

## See also

The [**truce-rack**][repo] workspace ships sibling wrapper crates
for [VST3](https://crates.io/crates/truce-rack-vst3),
[AU v2](https://crates.io/crates/truce-rack-au),
[AU v3](https://crates.io/crates/truce-rack-au3),
[LV2](https://crates.io/crates/truce-rack-lv2),
plus a [standalone runner](https://crates.io/crates/truce-rack-standalone).

## License

MIT or Apache-2.0, at your option.
