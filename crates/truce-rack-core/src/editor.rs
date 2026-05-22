//! Host-side editor (GUI) hosting interface.
//!
//! Plugins that ship a custom editor expose it through their
//! format's GUI API: `clap.gui`, `kAudioUnitProperty_CocoaUI`,
//! `IEditController::createView`, LV2 `ui:UI`. truce-rack-core wraps
//! these behind a single [`PluginEditor`] trait so a host doesn't
//! care which format produced the editor — it only needs a
//! native parent window handle and a place to put the resulting
//! view.
//!
//! # Threading
//!
//! Editor methods run on the **main (UI) thread**. Audio
//! processing (the `Plugin::process` path) runs on the audio
//! thread. The host application is responsible for serialising
//! the two — never invoke editor methods while the audio thread
//! holds a `&mut PluginCore`. Rust's borrow rules enforce this
//! because [`crate::PluginCore::editor`] borrows `&mut self`.
//!
//! # Platform handles
//!
//! Editors attach to a native parent window via a
//! [`WindowHandle`]. The variant tells the wrapper which API to
//! use:
//!
//! - macOS: [`WindowHandle::NSView`] — pointer to an `NSView*`.
//! - Windows: [`WindowHandle::HWND`] — `HWND`.
//! - Linux X11: [`WindowHandle::X11`] — the X11 window ID.
//!
//! Wayland support is currently unwired; CLAP also defines
//! a Wayland API but few hosts implement it.

use crate::error::Result;
use std::ffi::c_void;

/// Native parent window the plugin's editor view attaches to.
///
/// The host opens its own window, picks the appropriate variant
/// for the platform, and hands it to [`PluginEditor::open`]. The
/// plugin embeds its view inside that parent — the host stays in
/// charge of the outer window's lifecycle.
#[derive(Debug, Clone, Copy)]
pub enum WindowHandle {
    /// macOS / iOS / visionOS — pointer to a parent `NSView*`.
    NSView(*mut c_void),
    /// Windows — parent `HWND` (cast through `*mut c_void` to
    /// avoid pulling in the windows-sys dep at this layer).
    HWND(*mut c_void),
    /// X11 — the X11 `Window` id (`unsigned long` on most
    /// platforms, here widened to `u64`).
    X11(u64),
}

/// Editor-side view of a hosted plugin's UI.
///
/// Created by [`crate::PluginCore::editor`] when a plugin reports a
/// custom editor (its format-specific GUI extension is present
/// and `is_api_supported` returns true for the platform's API).
/// Methods correspond to the union of `clap.gui`, AU's
/// `kAudioUnitProperty_CocoaUI`, and VST3's `IPlugView`.
///
/// All methods run on the main (UI) thread; see the module
/// docs.
pub trait PluginEditor {
    /// Open the editor inside `parent`. After this returns `Ok`
    /// the editor is visible (or ready to be shown via
    /// [`Self::show`] for formats that distinguish the two
    /// phases). `scale` is the host's UI scale factor — 1.0 for
    /// non-Retina, 2.0 for typical Retina, etc.
    ///
    /// # Errors
    /// Returns [`crate::Error::Other`] when the underlying format
    /// API returns failure, e.g. the plugin's editor doesn't
    /// support the platform's window API.
    fn open(&mut self, parent: WindowHandle, scale: f64) -> Result<()>;

    /// Close the editor. Releases the plugin's view but does not
    /// invalidate the editor itself — calling [`Self::open`]
    /// again is legal.
    fn close(&mut self);

    /// `true` while [`Self::open`] has succeeded and
    /// [`Self::close`] hasn't been called.
    fn is_open(&self) -> bool;

    /// Editor's current logical size in pixels. `None` if the
    /// plugin doesn't expose a size (rare but legal — some AU
    /// editors leave the host to query the `NSView`'s `frame`).
    fn size(&self) -> Option<(u32, u32)>;

    /// `true` if the editor lets the host resize it.
    fn is_resizable(&self) -> bool;

    /// Request a new size. Plugins with aspect-ratio or
    /// minimum-size constraints may pick a nearby size — the
    /// return value is what the plugin actually adopted. Returns
    /// `None` on failure.
    fn set_size(&mut self, width: u32, height: u32) -> Option<(u32, u32)>;

    /// Show the editor view. Only meaningful for CLAP (which
    /// separates `create` and `show`); other formats no-op.
    fn show(&mut self);

    /// Hide the editor view without destroying it.
    fn hide(&mut self);

    /// Per-frame hook the host calls on the UI thread (typically
    /// at the parent window's frame rate). Formats whose plugins
    /// expose an idle / animation callback (LV2 `ui:idleInterface`,
    /// CLAP `gui.suggest_title`, future AU `GestureKit` ticks)
    /// override this to drive their plugin. Default is a no-op
    /// for formats that don't need it.
    fn on_idle(&mut self) {}
}
