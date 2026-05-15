//! Tell the linker where to find lilv at link time.
//!
//! `lilv-sys` links against `lilv-0` but doesn't probe for the
//! library — on macOS Homebrew installs to `/opt/homebrew/opt/lilv/lib`
//! (Apple Silicon) or `/usr/local/opt/lilv/lib` (Intel), neither
//! of which clang searches by default. We emit a `rustc-link-search`
//! hint per platform.

fn main() {
    #[cfg(target_os = "macos")]
    {
        // Honour brew --prefix lilv if set, otherwise fall back
        // to the two standard locations.
        let candidates = [
            "/opt/homebrew/opt/lilv/lib", // Apple Silicon Homebrew
            "/usr/local/opt/lilv/lib",    // Intel Homebrew
            "/opt/local/lib",             // MacPorts
        ];
        for path in candidates {
            if std::path::Path::new(path).exists() {
                println!("cargo:rustc-link-search=native={path}");
            }
        }
    }
    // Linux + Windows: lilv-sys's link directive plus the standard
    // ld search path already cover the common install locations.
}
