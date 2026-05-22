//! Capture every installed plugin's editor to a PNG.
//!
//! Walks the scanners enabled at compile time (CLAP / VST3 / AU),
//! opens each plugin's editor offscreen, snaps the `NSView`'s
//! contents, and writes `~/truce-rack-screenshots/<format>-<slug>.png`.
//! Supports `--format` (limit to one format) and `--output-dir`.
//!
//! # Per-plugin subprocess isolation
//!
//! Some plugins SIGSEGV inside their own view-factory code when the
//! editor opens. Doing all captures in one process means one bad
//! plugin kills the whole run. The parent walk re-execs itself with
//! `--single <format>:<unique_id>` for each capture so a crash only
//! loses that screenshot — every other plugin still gets one. The
//! `--single` mode skips the walk and captures just that one.

// With zero format features enabled the per-format walk blocks
// below all `cfg` away and several locals / imports go unused.
#![cfg_attr(
    not(any(feature = "clap", feature = "vst3", feature = "au")),
    allow(unused_variables, unused_imports)
)]

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("truce-rack-screenshot currently supports macOS only");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
use truce_rack_core::plugin::PluginCore;
#[cfg(target_os = "macos")]
use truce_rack_standalone::Format;
#[cfg(target_os = "macos")]
use truce_rack_standalone::screenshot;

#[cfg(target_os = "macos")]
#[cfg_attr(
    not(any(feature = "clap", feature = "vst3", feature = "au")),
    allow(dead_code)
)]
fn capture_one<P: PluginCore>(
    plugin: &mut P,
    format: Format,
    name: &str,
    label: &str,
    output_dir: &std::path::Path,
) -> i32 {
    let tag = format.tag();
    let Some(editor) = plugin.editor() else {
        println!("[skip] {tag} — {label} (no editor)");
        return 0;
    };
    let (w, h) = editor.size().unwrap_or((640, 480));
    let slug = screenshot::sanitize(name);
    let out = output_dir.join(format!("{tag}-{slug}.png"));
    match screenshot::capture_editor(editor, w, h, &out) {
        Ok(()) => {
            println!("[ok] {tag} — {label} -> {}", out.display());
            0
        }
        Err(e) => {
            println!("[err] {tag} — {label}: {e}");
            1
        }
    }
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_lines)]
fn main() {
    let mut pargs = pico_args::Arguments::from_env();

    if pargs.contains(["-h", "--help"]) {
        print_help();
        return;
    }

    let only_format: Option<String> = match pargs.opt_value_from_str("--format") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: --format: {e}");
            std::process::exit(2);
        }
    };
    let output_dir: std::path::PathBuf = match pargs.opt_value_from_str("--output-dir") {
        Ok(Some(p)) => p,
        Ok(None) => screenshot::default_output_dir(),
        Err(e) => {
            eprintln!("error: --output-dir: {e}");
            std::process::exit(2);
        }
    };
    let single: Option<String> = match pargs.opt_value_from_str("--single") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: --single: {e}");
            std::process::exit(2);
        }
    };
    let no_isolation = pargs.contains("--no-isolation");

    let leftover = pargs.finish();
    if !leftover.is_empty() {
        eprintln!("unknown arguments: {leftover:?}");
        std::process::exit(2);
    }

    if let Err(e) = std::fs::create_dir_all(&output_dir) {
        eprintln!("error: create_dir_all {}: {e}", output_dir.display());
        std::process::exit(1);
    }

    let only = only_format.as_deref().and_then(Format::parse);

    if let Some(spec) = single {
        single_mode(&spec, &output_dir);
        return;
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: current_exe: {e}");
            std::process::exit(1);
        }
    };

    walk_and_capture(only, &exe, &output_dir, no_isolation);
}

#[cfg(target_os = "macos")]
fn walk_and_capture(
    only: Option<Format>,
    exe: &std::path::Path,
    output_dir: &std::path::Path,
    no_isolation: bool,
) {
    use truce_rack_core::scanner::PluginScanner;

    let mut targets: Vec<(Format, String, String, String)> = Vec::new(); // (fmt, name, label, unique_id)

    #[cfg(feature = "clap")]
    if only.is_none_or(|f| f == Format::Clap) {
        let scanner = truce_rack_clap::ClapScanner::new();
        for info in scanner.scan().unwrap_or_default() {
            let label = format!("{} — {}", info.vendor, info.name);
            targets.push((
                Format::Clap,
                info.name.clone(),
                label,
                info.unique_id.clone(),
            ));
        }
    }
    #[cfg(feature = "vst3")]
    if only.is_none_or(|f| f == Format::Vst3) {
        let scanner = truce_rack_vst3::Vst3Scanner::new();
        for info in scanner.scan().unwrap_or_default() {
            let label = format!("{} — {}", info.vendor, info.name);
            targets.push((
                Format::Vst3,
                info.name.clone(),
                label,
                info.unique_id.clone(),
            ));
        }
    }
    #[cfg(all(feature = "au", target_vendor = "apple"))]
    if only.is_none_or(|f| f == Format::Au) {
        let scanner = truce_rack_au::AuScanner::new();
        for info in scanner.scan().unwrap_or_default() {
            let label = format!("{} — {}", info.vendor, info.name);
            targets.push((Format::Au, info.name.clone(), label, info.unique_id.clone()));
        }
    }

    eprintln!("[walk] {} plugins across formats", targets.len());

    for (idx, (fmt, _name, label, uid)) in targets.iter().enumerate() {
        let tag = fmt.tag();
        eprintln!("[{}/{}] {tag} {label}", idx + 1, targets.len());

        if no_isolation {
            // Direct capture without subprocess isolation — a crash
            // here kills the walk. Use `--no-isolation` only when
            // debugging.
            direct_capture(*fmt, uid, output_dir);
            continue;
        }

        let spec = format!("{tag}:{uid}");
        let child = std::process::Command::new(exe)
            .arg("--single")
            .arg(&spec)
            .arg("--output-dir")
            .arg(output_dir)
            .stdin(std::process::Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            eprintln!("[err] spawn: {}", child.err().unwrap());
            continue;
        };
        // Cap each capture at ~15s — long enough for heavy plugins
        // (Melodyne, license-server-backed) to lay out, short
        // enough that one hang doesn't stall the whole walk.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(Some(status)) => {
                    eprintln!("[crash] {tag} {label}: {status}");
                    break;
                }
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        eprintln!("[timeout] {tag} {label}");
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => {
                    eprintln!("[err] wait: {e}");
                    break;
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn single_mode(spec: &str, output_dir: &std::path::Path) {
    let Some((tag, uid)) = spec.split_once(':') else {
        eprintln!("error: --single expects <format>:<unique_id>, got {spec:?}");
        std::process::exit(2);
    };
    let Some(format) = Format::parse(tag) else {
        eprintln!("error: --single: unknown format tag {tag:?}");
        std::process::exit(2);
    };
    direct_capture(format, uid, output_dir);
}

#[cfg(target_os = "macos")]
fn direct_capture(format: Format, uid: &str, output_dir: &std::path::Path) {
    use truce_rack_core::scanner::PluginScanner;
    let tag = format.tag();
    let code = match format {
        #[cfg(feature = "clap")]
        Format::Clap => {
            let scanner = truce_rack_clap::ClapScanner::new();
            scanner
                .scan()
                .unwrap_or_default()
                .iter()
                .find(|p| p.unique_id == uid)
                .map_or_else(
                    || {
                        eprintln!("[err] {tag} no match for {uid:?}");
                        2
                    },
                    |info| match scanner.load(info) {
                        Ok(mut p) => {
                            let label = format!("{} — {}", info.vendor, info.name);
                            capture_one(&mut p, Format::Clap, &info.name, &label, output_dir)
                        }
                        Err(e) => {
                            eprintln!("[err] {tag} load: {e}");
                            1
                        }
                    },
                )
        }
        #[cfg(feature = "vst3")]
        Format::Vst3 => {
            let scanner = truce_rack_vst3::Vst3Scanner::new();
            scanner
                .scan()
                .unwrap_or_default()
                .iter()
                .find(|p| p.unique_id == uid)
                .map_or_else(
                    || {
                        eprintln!("[err] {tag} no match for {uid:?}");
                        2
                    },
                    |info| match scanner.load(info) {
                        Ok(mut p) => {
                            let label = format!("{} — {}", info.vendor, info.name);
                            capture_one(&mut p, Format::Vst3, &info.name, &label, output_dir)
                        }
                        Err(e) => {
                            eprintln!("[err] {tag} load: {e}");
                            1
                        }
                    },
                )
        }
        #[cfg(all(feature = "au", target_vendor = "apple"))]
        Format::Au => {
            let scanner = truce_rack_au::AuScanner::new();
            scanner
                .scan()
                .unwrap_or_default()
                .iter()
                .find(|p| p.unique_id == uid)
                .map_or_else(
                    || {
                        eprintln!("[err] {tag} no match for {uid:?}");
                        2
                    },
                    |info| match scanner.load(info) {
                        Ok(mut p) => {
                            let label = format!("{} — {}", info.vendor, info.name);
                            capture_one(&mut p, Format::Au, &info.name, &label, output_dir)
                        }
                        Err(e) => {
                            eprintln!("[err] {tag} load: {e}");
                            1
                        }
                    },
                )
        }
        #[allow(unreachable_patterns)]
        _ => {
            eprintln!("[err] {tag} format not enabled in this build");
            2
        }
    };
    std::process::exit(code);
}

#[cfg(target_os = "macos")]
fn print_help() {
    println!(
        "truce-rack-screenshot — capture every installed plugin's editor to PNG

USAGE:
  truce-rack-screenshot [--format <clap|vst3|au>] [--output-dir <path>]

OPTIONS:
  --format <fmt>          Only scan this format (clap, vst3, or au)
  --output-dir <path>     Override the default ~/truce-rack-screenshots
  --no-isolation          Capture without per-plugin subprocess isolation
                          (one crashing plugin kills the whole run)
  --single <fmt:uid>      Capture exactly one plugin and exit; used
                          internally to spawn isolated workers
  -h, --help              Print this help"
    );
}
