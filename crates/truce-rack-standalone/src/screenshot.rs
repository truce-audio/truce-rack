//! macOS-only `NSView` → PNG capture for the truce-rack-screenshot bin.
//!
//! Allocates an offscreen `NSView`, hands it to the plugin's editor
//! via [`truce_rack_core::editor::WindowHandle::NSView`], pumps the
//! `AppKit` event loop for ~500 ms so the plugin's view can lay out
//! and render, then captures the view's contents through
//! `bitmapImageRepForCachingDisplayInRect:` /
//! `cacheDisplayInRect:toBitmapImageRep:` and writes a PNG.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use objc2::msg_send;
use objc2::runtime::{AnyObject, Bool};
use objc2_foundation::{NSData, NSDate, NSPoint, NSRect, NSRunLoop, NSSize, NSString};

use truce_rack_core::editor::{PluginEditor, WindowHandle};
use truce_rack_core::error::{Error, Result};

/// `NSBitmapImageFileType::Png` — value 4 per
/// `<AppKit/NSBitmapImageRep.h>`. Hard-coded rather than pulled
/// from the `AppKit` binding crate so we don't drag a new feature
/// in for one constant.
const NS_BITMAP_FILE_TYPE_PNG: usize = 4;

/// Open `editor` against an offscreen `NSView` sized to `(width,
/// height)`, pump the event loop briefly so it renders, capture to
/// PNG at `out_path`, then close the editor and release the
/// offscreen view.
///
/// # Errors
/// Returns [`Error::Other`] if `AppKit` refuses any of the cocoa
/// allocs, if the editor's `open` fails, or if PNG encoding /
/// `writeToFile:atomically:` fails.
pub fn capture_editor(
    editor: &mut dyn PluginEditor,
    width: u32,
    height: u32,
    out_path: &Path,
) -> Result<()> {
    // Make sure NSApplication is initialized — without
    // `sharedApplication` the run loop is dormant and
    // `runUntilDate:` returns immediately.
    unsafe {
        let app_cls = objc2::class!(NSApplication);
        let _: *mut AnyObject = msg_send![app_cls, sharedApplication];
    }

    let rect = NSRect {
        origin: NSPoint { x: 0.0, y: 0.0 },
        size: NSSize {
            width: f64::from(width),
            height: f64::from(height),
        },
    };

    let parent: *mut AnyObject = unsafe {
        let cls = objc2::class!(NSView);
        let alloc: *mut AnyObject = msg_send![cls, alloc];
        let view: *mut AnyObject = msg_send![alloc, initWithFrame: rect];
        view
    };
    if parent.is_null() {
        return Err(Error::Other("NSView alloc/init returned nil".into()));
    }

    let result = capture_inner(editor, parent, rect, out_path);

    // Tear down the offscreen NSView; close the editor (so the
    // plugin removes its subview / releases retained state) before
    // we release our retained `parent`.
    if editor.is_open() {
        editor.close();
    }
    unsafe {
        let _: () = msg_send![parent, release];
    }
    result
}

fn capture_inner(
    editor: &mut dyn PluginEditor,
    parent: *mut AnyObject,
    rect: NSRect,
    out_path: &Path,
) -> Result<()> {
    editor.open(WindowHandle::NSView(parent.cast::<c_void>()), 1.0)?;
    editor.show();

    // Re-query the editor's preferred size and re-size the parent
    // NSView accordingly. Some plugins ignore the parent's frame
    // and size their own subview; capturing the editor's actual
    // size avoids cropping or padding.
    let capture_rect = if let Some((w, h)) = editor.size() {
        let new_rect = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSSize {
                width: f64::from(w),
                height: f64::from(h),
            },
        };
        unsafe {
            let _: () = msg_send![parent, setFrame: new_rect];
        }
        new_rect
    } else {
        rect
    };

    // Pump the run loop briefly so the editor's subview can lay
    // out, dispatch async draws, and create any GPU surfaces it
    // needs. `[[NSRunLoop currentRunLoop] runUntilDate:limit]`
    // drives timer / source-0 callbacks until `limit` — not a full
    // AppKit dispatch (that would need `nextEventMatchingMask:`
    // inside a loop) but enough for the editor's layout / autosize
    // callbacks to settle, which is all we need before the capture.
    let run_loop = NSRunLoop::currentRunLoop();
    let date = NSDate::dateWithTimeIntervalSinceNow(0.5);
    run_loop.runUntilDate(&date);

    // [parent bitmapImageRepForCachingDisplayInRect:rect] →
    //   NSBitmapImageRep* (autoreleased)
    let rep: *mut AnyObject =
        unsafe { msg_send![parent, bitmapImageRepForCachingDisplayInRect: capture_rect] };
    if rep.is_null() {
        return Err(Error::Other(
            "bitmapImageRepForCachingDisplayInRect: returned nil".into(),
        ));
    }
    // [parent cacheDisplayInRect:rect toBitmapImageRep:rep]
    unsafe {
        let _: () = msg_send![parent, cacheDisplayInRect: capture_rect, toBitmapImageRep: rep];
    }

    // [rep representationUsingType:NSBitmapImageFileTypePNG properties:nil] → NSData*
    let png_data: *mut NSData = unsafe {
        msg_send![
            rep,
            representationUsingType: NS_BITMAP_FILE_TYPE_PNG,
            properties: std::ptr::null::<AnyObject>(),
        ]
    };
    if png_data.is_null() {
        return Err(Error::Other(
            "representationUsingType:NSBitmapImageFileTypePNG returned nil".into(),
        ));
    }

    if let Some(parent_dir) = out_path.parent()
        && !parent_dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent_dir)
            .map_err(|e| Error::Other(format!("create_dir_all {}: {e}", parent_dir.display())))?;
    }

    let path_str = out_path
        .to_str()
        .ok_or_else(|| Error::Other(format!("non-utf8 path: {}", out_path.display())))?;
    let path_ns = NSString::from_str(path_str);

    let wrote: Bool = unsafe {
        let path_ref: &NSString = &path_ns;
        msg_send![png_data, writeToFile: path_ref, atomically: Bool::YES]
    };
    if !wrote.as_bool() {
        return Err(Error::Other(format!(
            "writeToFile: returned NO for {}",
            out_path.display()
        )));
    }
    Ok(())
}

/// Sanitize a plugin name into a filesystem-friendly stem:
/// lowercase ASCII alnum + `-`; spaces / slashes / dots / `_`
/// collapse to a single `-`; everything else is dropped.
#[must_use]
pub fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_dash = false;
    for c in name.chars() {
        let mapped = if c.is_ascii_alphanumeric() {
            Some(c.to_ascii_lowercase())
        } else if matches!(c, ' ' | '/' | '\\' | '.' | '_') {
            Some('-')
        } else {
            None
        };
        if let Some(ch) = mapped {
            if ch == '-' && last_was_dash {
                continue;
            }
            out.push(ch);
            last_was_dash = ch == '-';
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("unnamed");
    }
    out
}

/// Resolve the default output directory: `~/truce-rack-screenshots`,
/// falling back to `./truce-rack-screenshots` when the home dir is
/// unavailable.
#[must_use]
pub fn default_output_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("truce-rack-screenshots")
}
