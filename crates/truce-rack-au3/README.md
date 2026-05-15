# truce-rack-au3

[![crates.io](https://img.shields.io/crates/v/truce-rack-au3.svg)](https://crates.io/crates/truce-rack-au3)
[docs.rs](https://docs.rs/truce-rack-au3)

Audio Unit v3 (App Extension) host implementation for the
[**truce-rack**][repo] audio plugin framework. Apple-only. Filters
the registry walk to v3-flagged components and forwards loading
to [`truce-rack-au`][au], which handles both synchronous and
async (`AudioComponentInstantiate` with a completion block)
instantiation paths. Once the instance handle is in hand the
audio + MIDI + editor surface is identical to AUv2.

[repo]: https://github.com/truce-audio/truce-rack
[au]: https://crates.io/crates/truce-rack-au

## Usage

```toml
[target.'cfg(target_vendor = "apple")'.dependencies]
truce-rack-core = "1.0"
truce-rack-au3 = "1.0"
```

```rust
use truce_rack_core::scanner::PluginScanner;
use truce_rack_au3::Au3Scanner;

let plugins = Au3Scanner::new().scan()?;
for info in &plugins {
    println!("{info}");
}
```

## See also

The [**truce-rack**][repo] workspace ships sibling wrapper crates
for [CLAP](https://crates.io/crates/truce-rack-clap),
[VST3](https://crates.io/crates/truce-rack-vst3),
[AU v2](https://crates.io/crates/truce-rack-au),
[LV2](https://crates.io/crates/truce-rack-lv2),
plus a [standalone runner](https://crates.io/crates/truce-rack-standalone).

## License

MIT or Apache-2.0, at your option.
