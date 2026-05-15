# truce-rack

[![crates.io](https://img.shields.io/crates/v/truce-rack.svg)](https://crates.io/crates/truce-rack)
[docs.rs](https://docs.rs/truce-rack)

Cross-format audio plugin host. Umbrella crate for the
[**truce-rack**][repo] workspace — re-exports the per-format
wrapper crates behind features so a downstream user can opt into
exactly the formats they need without juggling version pins
across multiple direct dependencies.

[repo]: https://github.com/truce-audio/truce-rack

## Defaults

CLAP and VST3 are the cross-platform mainstream formats and ship
enabled by default. Audio Unit and LV2 are platform-specific
enough to be opt-in.

| Feature | Default | Crate it enables                                           | Platforms |
| ------- | ------- | ---------------------------------------------------------- | --------- |
| `clap`  | ✓       | [`truce-rack-clap`](https://crates.io/crates/truce-rack-clap)   | all       |
| `vst3`  | ✓       | [`truce-rack-vst3`](https://crates.io/crates/truce-rack-vst3)   | all       |
| `au`    |         | [`truce-rack-au`](https://crates.io/crates/truce-rack-au)       | Apple     |
| `au3`   |         | [`truce-rack-au3`](https://crates.io/crates/truce-rack-au3)     | Apple     |
| `lv2`   |         | [`truce-rack-lv2`](https://crates.io/crates/truce-rack-lv2) (needs system `lilv-0`) | all       |

## Usage

```toml
[dependencies]
truce-rack = "1.0"   # CLAP + VST3
```

```rust
use truce_rack::core::scanner::PluginScanner;
use truce_rack::clap::ClapScanner;
use truce_rack::vst3::Vst3Scanner;

let clap_plugins = ClapScanner::new().scan()?;
let vst3_plugins = Vst3Scanner::new().scan()?;
```

To opt into AU + LV2 too:

```toml
[dependencies]
truce-rack = { version = "1.0", features = ["au", "au3", "lv2"] }
```

## When to depend on `truce-rack` vs the per-format crates

- **`truce-rack`** — you want the convenience of one dependency
  line and aren't sensitive to the umbrella's resolver churn when
  unrelated wrappers bump.
- **Per-format crates** — you only need one or two formats, want
  the minimum possible build graph, or want to pin per-format
  versions independently. See
  [`truce-rack-core`](https://crates.io/crates/truce-rack-core)
  for the trait surface and pick wrappers à la carte.

## License

MIT or Apache-2.0, at your option.
