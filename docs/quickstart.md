# Quickstart

Build `truce-rack` from a fresh checkout on macOS, Linux, or
Windows. After installing the prerequisites for your platform,
`cargo build` at the workspace root should succeed.

## Prerequisites (all platforms)

- Rust stable (1.75 or newer) — install via [rustup](https://rustup.rs).
- A C toolchain (clang on macOS, gcc on Linux, MSVC on Windows).
  `rustup` will prompt for it if missing.

## System libraries by crate

`truce-rack` is a workspace; each per-format crate links to the
native host SDK for that format. A `cargo build` at the workspace
root builds every member, so you need every dependency installed.
To skip the ones you don't care about, build a single crate with
`cargo build -p <crate>` instead.

| Crate                  | Links against                       | Needed on              |
| ---------------------- | ----------------------------------- | ---------------------- |
| `truce-rack-core`      | —                                   | —                      |
| `truce-rack-clap`      | — (CLAP is header-only, vendored)   | —                      |
| `truce-rack-vst3`      | — (VST3 community crate, pure Rust) | —                      |
| `truce-rack-au`        | system AU frameworks                | macOS only             |
| `truce-rack-au3`       | system AU frameworks                | macOS only             |
| `truce-rack-lv2`       | `lilv-0`                            | all platforms          |
| `truce-rack-standalone`| ALSA (Linux), CoreAudio (macOS), WASAPI (Windows) — via cpal/midir | all platforms |

## macOS

```bash
# Xcode Command Line Tools (clang, AU frameworks)
xcode-select --install

# LV2 host library
brew install lilv

# Build everything
cargo build
```

Apple Silicon Homebrew installs lilv to `/opt/homebrew/opt/lilv`;
Intel Homebrew to `/usr/local/opt/lilv`. `truce-rack-lv2/build.rs`
already adds both to the linker search path.

## Linux (Debian / Ubuntu)

```bash
sudo apt install build-essential pkg-config \
    libasound2-dev \
    liblilv-dev

# Build everything
cargo build
```

- `libasound2-dev` — ALSA, pulled in by cpal (audio out) and midir
  (MIDI in).
- `liblilv-dev` — LV2 host library. The error
  `rust-lld: error: unable to find library -llilv-0` means this is
  missing.

The `gui` feature on `truce-rack-standalone` is gated off on Linux
(baseview's Linux backend drags `wayland-sys`); the headless path
works. To build standalone without LV2 / without lilv installed:

```bash
cargo build -p truce-rack-standalone --no-default-features --features clap
```

### Fedora / RHEL

```bash
sudo dnf install alsa-lib-devel lilv-devel
```

### Arch

```bash
sudo pacman -S alsa-lib lilv
```

## Windows

Audio (WASAPI) and MIDI (WinMM) come from the system — no extra
install. Only LV2 needs a system library.

```powershell
# Install vcpkg if you don't have it
git clone https://github.com/microsoft/vcpkg
.\vcpkg\bootstrap-vcpkg.bat

# LV2 host library
.\vcpkg\vcpkg install lilv:x64-windows

# Point the linker at vcpkg's lib dir, then build
$env:LIB = "$PWD\vcpkg\installed\x64-windows\lib;$env:LIB"
cargo build
```

If you don't need LV2, skip vcpkg and build only the crates you
want:

```powershell
cargo build -p truce-rack-standalone --no-default-features --features clap,vst3
```

## Try it

```bash
# List every plugin every enabled scanner can find.
cargo run -p truce-rack-standalone --features "vst3,lv2" -- --list

# Load one and run it headlessly for 5 seconds.
cargo run -p truce-rack-standalone --features "vst3" -- \
    --format vst3 --name "Surge XT" --seconds 5
```

On macOS, add `gui`, `au`, `au3` to the feature list to embed the
plugin's editor and to scan AU plugins.

## Troubleshooting

- **`unable to find library -llilv-0`** — install `liblilv-dev`
  (Linux) / `brew install lilv` (macOS) / `vcpkg install lilv`
  (Windows). See platform sections above.
- **`ALSA lib ... cannot find card`** on Linux — no audio device
  is selected. Pass `--seconds N` to run without opening one, or
  check `aplay -l`.
- **`error: linker 'cc' not found`** — install your platform's C
  toolchain (Xcode CLT, `build-essential`, MSVC Build Tools).
