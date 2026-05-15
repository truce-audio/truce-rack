//! macOS native menu bar for the windowed runner.
//!
//! Minimal install: an autopopulated App menu (with Quit) plus a
//! plugin-name top-level item. The keyboard handler in
//! [`crate::windowed`] owns Save / Load — the menu doesn't surface
//! those today; the goal is to get the right app name in the menu
//! bar and a real Quit item so ⌘Q works.

#![cfg(all(target_os = "macos", feature = "gui"))]

use objc2::msg_send;
use objc2::runtime::AnyObject;
use objc2_foundation::NSString;

/// Build and install the menu bar. Must run on the main thread
/// after `NSApp` is wired up (i.e. inside baseview's window-build
/// closure).
pub fn install(plugin_name: &str) {
    unsafe {
        // [NSApplication sharedApplication] → NSApplication*
        let app_cls = objc2::class!(NSApplication);
        let app: *mut AnyObject = msg_send![app_cls, sharedApplication];

        let app_menu_item = make_menu_item(plugin_name);
        let app_menu = make_menu(plugin_name);
        let quit = make_quit_item(plugin_name);
        let _: () = msg_send![app_menu, addItem: quit];
        let _: () = msg_send![app_menu_item, setSubmenu: app_menu];

        let main_menu = make_menu("");
        let _: () = msg_send![main_menu, addItem: app_menu_item];
        let _: () = msg_send![app, setMainMenu: main_menu];
    }
}

unsafe fn make_menu(title: &str) -> *mut AnyObject {
    unsafe {
        let title_ns = NSString::from_str(title);
        let menu_cls = objc2::class!(NSMenu);
        let menu: *mut AnyObject = msg_send![menu_cls, alloc];
        let title_ref: &NSString = &title_ns;
        let menu: *mut AnyObject = msg_send![menu, initWithTitle: title_ref];
        menu
    }
}

unsafe fn make_menu_item(title: &str) -> *mut AnyObject {
    unsafe {
        let title_ns = NSString::from_str(title);
        let empty_ns = NSString::from_str("");
        let cls = objc2::class!(NSMenuItem);
        let item: *mut AnyObject = msg_send![cls, alloc];
        let title_ref: &NSString = &title_ns;
        let empty_ref: &NSString = &empty_ns;
        let item: *mut AnyObject = msg_send![
            item,
            initWithTitle: title_ref,
            action: objc2::sel!(noopAction:),
            keyEquivalent: empty_ref,
        ];
        item
    }
}

unsafe fn make_quit_item(plugin_name: &str) -> *mut AnyObject {
    unsafe {
        let title_ns = NSString::from_str(&format!("Quit {plugin_name}"));
        let key_ns = NSString::from_str("q");
        let cls = objc2::class!(NSMenuItem);
        let item: *mut AnyObject = msg_send![cls, alloc];
        let title_ref: &NSString = &title_ns;
        let key_ref: &NSString = &key_ns;
        let item: *mut AnyObject = msg_send![
            item,
            initWithTitle: title_ref,
            action: objc2::sel!(terminate:),
            keyEquivalent: key_ref,
        ];
        item
    }
}
