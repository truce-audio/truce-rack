# truce-rack-vst3

[![crates.io](https://img.shields.io/crates/v/truce-rack-vst3.svg)](https://crates.io/crates/truce-rack-vst3)
[![docs.rs](https://img.shields.io/badge/docs.rs-blue)](https://docs.rs/truce-rack-vst3)

VST3 host implementation for the [**truce-rack**][repo] audio
plugin framework. Built on the community [`vst3`][vst3] crate —
no Steinberg SDK submodule, no cmake. Implements scan, load,
audio + MIDI processing, and the VST3 `IPlugView` GUI behind the
format-agnostic [`truce-rack-core`][core] traits.

Host transport from `ProcessContext::transport` is translated into
Steinberg's `Vst::ProcessContext` (tempo, time signature, project
time in quarter notes / samples, bar position, and the playing /
recording / cycle flags), so tempo- and grid-synced plugins get a
usable timeline.

[repo]: https://github.com/truce-audio/truce-rack
[core]: https://crates.io/crates/truce-rack-core
[vst3]: https://crates.io/crates/vst3

## Usage

```toml
[dependencies]
truce-rack-core = "1.0"
truce-rack-vst3 = "1.0"
```

```rust
use truce_rack_core::scanner::PluginScanner;
use truce_rack_vst3::Vst3Scanner;

let plugins = Vst3Scanner::new().scan()?;
for info in &plugins {
    println!("{info}");
}
```

## See also

The [**truce-rack**][repo] workspace ships sibling wrapper crates
for [CLAP](https://crates.io/crates/truce-rack-clap),
[AU v2](https://crates.io/crates/truce-rack-au),
[AU v3](https://crates.io/crates/truce-rack-au3),
[LV2](https://crates.io/crates/truce-rack-lv2),
plus a [standalone runner](https://crates.io/crates/truce-rack-standalone).

## License

MIT or Apache-2.0, at your option.
