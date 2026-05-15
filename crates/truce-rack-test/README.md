# truce-rack-test

[![crates.io](https://img.shields.io/crates/v/truce-rack-test.svg)](https://crates.io/crates/truce-rack-test)
[![docs.rs](https://docs.rs/truce-rack-test/badge.svg)](https://docs.rs/truce-rack-test)

Assertion helpers for [**truce-rack**][repo] host integration
tests — render N frames of silence (or a generated input) through
a [`Plugin<f32>`][core], assert no NaN / no clipping / state
round-trips. Use these from per-format integration suites that
exercise a [`PluginScanner`][core] impl across a corpus.

[repo]: https://github.com/truce-audio/truce-rack
[core]: https://crates.io/crates/truce-rack-core

## Usage

```toml
[dev-dependencies]
truce-rack-core = "0.9"
truce-rack-test = "0.9"
```

```rust
use truce_rack_core::scanner::PluginScanner;
use truce_rack_test::{render_silence, assert_no_nans, assert_state_round_trip};

let scanner = MyScanner::new();
for info in scanner.scan()? {
    let mut plugin = scanner.load(&info)?;
    let rendered = render_silence(&mut plugin, 48_000.0, 1024)?;
    assert_no_nans(&rendered);
    assert_state_round_trip(&mut plugin)?;
}
```

## See also

The [**truce-rack**][repo] workspace ships per-format wrapper
crates this helper exercises — see
[`truce-rack-clap`](https://crates.io/crates/truce-rack-clap),
[`truce-rack-vst3`](https://crates.io/crates/truce-rack-vst3),
[`truce-rack-au`](https://crates.io/crates/truce-rack-au),
[`truce-rack-au3`](https://crates.io/crates/truce-rack-au3),
[`truce-rack-lv2`](https://crates.io/crates/truce-rack-lv2).

## License

MIT or Apache-2.0, at your option.
