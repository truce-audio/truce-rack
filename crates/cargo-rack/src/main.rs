//! `cargo rack` — a cargo subcommand that runs the truce-rack
//! standalone plugin host.
//!
//! Installed as the `cargo-rack` binary, which cargo surfaces as
//! `cargo rack <args>`. All flag handling is shared with the
//! `truce-rack-standalone` binary via
//! [`truce_rack_standalone::cli`].

fn main() {
    // Skip argv[0]. When cargo invokes us as `cargo rack …` it
    // injects the subcommand name `rack` as the first argument;
    // drop it so the shared CLI sees only the real flags. (Running
    // the binary directly as `cargo-rack …` has no such prefix.)
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "rack") {
        args.remove(0);
    }
    truce_rack_standalone::cli::run("cargo rack", args);
}
