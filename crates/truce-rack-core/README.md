# truce-rack-core

[![crates.io](https://img.shields.io/crates/v/truce-rack-core.svg)](https://crates.io/crates/truce-rack-core)
[docs.rs](https://docs.rs/truce-rack-core)

Core traits and types for the [**truce-rack**][repo] audio plugin
host framework. Defines the format-agnostic `PluginScanner`,
`PluginCore`, `Plugin<S>`, and `PluginEditor` trait surface every
per-format wrapper crate adapts plugins into. No FFI — depends on
nothing platform-specific.

[repo]: https://github.com/truce-audio/truce-rack

## Companion crates

Per-format wrappers all live in the same workspace:

- [`truce-rack-clap`](https://crates.io/crates/truce-rack-clap) — CLAP
- [`truce-rack-vst3`](https://crates.io/crates/truce-rack-vst3) — VST3
- [`truce-rack-au`](https://crates.io/crates/truce-rack-au) — Audio Unit v2 (Apple)
- [`truce-rack-au3`](https://crates.io/crates/truce-rack-au3) — Audio Unit v3 (Apple)
- [`truce-rack-lv2`](https://crates.io/crates/truce-rack-lv2) — LV2
- [`truce-rack-standalone`](https://crates.io/crates/truce-rack-standalone) — reference cpal + baseview host
- [`truce-rack-test`](https://crates.io/crates/truce-rack-test) — assertion helpers

## Usage

```toml
[dependencies]
truce-rack-core = "1.0"
truce-rack-clap = "1.0"   # add one wrapper per format you need
```

```rust
use truce_rack_core::scanner::PluginScanner;
use truce_rack_clap::ClapScanner;

let plugins = ClapScanner::new().scan()?;
for plugin in &plugins {
    println!("{plugin}");
}
```

## License

MIT or Apache-2.0, at your option.
