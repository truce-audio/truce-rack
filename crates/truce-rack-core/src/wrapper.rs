//! `catch_unwind` helpers for FFI boundaries between rack and
//! plugin code.
//!
//! Plugin libraries are written by third parties. A panic in a
//! plugin's `process` (or any other callback) would unwind
//! across the `extern "C"` boundary back into the host —
//! undefined behaviour on most toolchains, abort on others.
//! These helpers catch the unwind, log a short diagnostic, and
//! return a fallback value the wrapper can hand back to its
//! caller.
//!
//! Mirrors `truce_core::wrapper`. The plugin-side framework
//! catches panics going *out* of plugin code; the host-side
//! framework catches panics going *in* from plugin code. Same
//! helper shape, opposite direction.

use crate::error::Error;
use std::any::type_name;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Run a per-audio-block callback body under [`catch_unwind`]
/// with no fallback value — caller only cares whether the body
/// panicked.
///
/// Returns `true` on a clean exit, `false` on panic. Wrappers
/// should zero output buffers on `false` so the host doesn't
/// hear garbage from whatever was in those slots.
#[must_use]
pub fn run_audio_block<P>(format: &str, body: impl FnOnce()) -> bool {
    let result = catch_unwind(AssertUnwindSafe(body));
    if let Err(payload) = result {
        eprintln!(
            "[truce-rack {format}] panic in process() for plugin {}: {}",
            type_name::<P>(),
            extract_panic_msg(&payload),
        );
        return false;
    }
    true
}

/// Run a per-audio-block callback body under [`catch_unwind`]
/// with a fallback return value. Returns the body's value on
/// clean exit, `fallback` on panic.
pub fn run_audio_block_with<P, R>(format: &str, fallback: R, body: impl FnOnce() -> R) -> R {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(payload) => {
            eprintln!(
                "[truce-rack {format}] panic in process() for plugin {}: {}",
                type_name::<P>(),
                extract_panic_msg(&payload),
            );
            fallback
        }
    }
}

/// Run an `extern "C"` plugin callback body (state save / load,
/// parameter formatting, GUI handler) under [`catch_unwind`]
/// with a fallback return value. `action` is logged for
/// debuggability ("`save_state`", "`load_state`", "`format_value`", …).
pub fn run_extern_callback_with<P, R>(
    format: &str,
    action: &str,
    fallback: R,
    body: impl FnOnce() -> R,
) -> R {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(payload) => {
            eprintln!(
                "[truce-rack {format}] panic in {action} for plugin {}: {}",
                type_name::<P>(),
                extract_panic_msg(&payload),
            );
            fallback
        }
    }
}

/// Convenience: run a plugin callback under [`catch_unwind`]
/// and convert a panic into an [`Error::Panic`]. Used by the
/// `PluginCore` / `Plugin` impl shims when the callback's
/// natural error type is `crate::Result<T>`.
///
/// Distinct from [`run_extern_callback_with`] in that this one
/// is *inside* the rack-side Rust trait surface, not at the
/// raw C ABI edge — but the catch-and-log pattern is the same.
///
/// # Errors
/// `body`'s error propagates on a clean failure; a panic in
/// `body` returns [`Error::Panic`] with the supplied `action`
/// string.
pub fn run_callable<P, T>(
    action: &'static str,
    body: impl FnOnce() -> crate::Result<T>,
) -> crate::Result<T> {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(result) => result,
        Err(payload) => Err(Error::Panic {
            action,
            message: extract_panic_msg(&payload).to_string(),
        }),
    }
}

fn extract_panic_msg(payload: &Box<dyn std::any::Any + Send>) -> &str {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.as_str()
    } else {
        "<non-string panic payload>"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dummy;

    #[test]
    fn clean_audio_block_returns_true() {
        let ran = std::cell::Cell::new(false);
        let result = run_audio_block::<Dummy>("test", || ran.set(true));
        assert!(result);
        assert!(ran.get());
    }

    #[test]
    fn panicking_audio_block_returns_false() {
        let result = run_audio_block::<Dummy>("test", || panic!("boom"));
        assert!(!result);
    }

    #[test]
    fn callable_panic_becomes_error() {
        let result: crate::Result<()> = run_callable::<Dummy, ()>("save_state", || panic!("oops"));
        match result {
            Err(Error::Panic { action, .. }) => assert_eq!(action, "save_state"),
            _ => panic!("expected Error::Panic"),
        }
    }

    #[test]
    fn callable_error_propagates() {
        let result: crate::Result<()> =
            run_callable::<Dummy, ()>("save_state", || Err(Error::Other("plain error".into())));
        assert!(matches!(result, Err(Error::Other(_))));
    }
}
