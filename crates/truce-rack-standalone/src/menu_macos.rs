//! macOS native menu bar for the windowed runner.
//!
//! Two top-level menus:
//!
//! - **App menu** — the standard "Quit <plugin>" (⌘Q).
//! - **Settings menu** — live audio / MIDI selection, mirroring the
//!   CLI flags but switchable without relaunching:
//!     - **Output Device** — every cpal output, repopulated on open.
//!     - **Output Channels** — `Direct` / stereo pairs / mono, per
//!       the device's channel count.
//!     - **MIDI Input** — every midir port, repopulated on open.
//!     - **MIDI Channel** — `Omni` or channel 1-16.
//!
//! Action wiring uses a custom `TruceRackMenuTarget` Objective-C
//! class registered at runtime (objc2 `ClassBuilder`). Its action
//! methods read the process-global [`MenuState`] — which holds raw
//! pointers to the [`AudioController`] / [`MidiController`] that
//! [`crate::windowed::run`] keeps on its stack for the window's
//! lifetime — and drive the matching controller. The same object is
//! each submenu's delegate, so `menuWillOpen:` repopulates device
//! lists and refreshes checkmarks just before display.

#![cfg(all(target_os = "macos", feature = "gui"))]

use std::ptr;
use std::sync::Once;
use std::sync::atomic::{AtomicPtr, Ordering};

use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::NSString;

use crate::device::{self, ChannelRoute};
use crate::midi::{self, MidiChannel, MidiController};
use crate::windowed::AudioController;

const STATE_ON: isize = 1;
const STATE_OFF: isize = 0;
/// `tag` sentinel for the "use the system default / all ports" item.
const TAG_DEFAULT: isize = -1;

/// Process-global the Obj-C action handlers read. Holds *borrowed*
/// pointers (not ownership) to the controllers and the four submenus.
struct MenuState {
    audio: *mut AudioController,
    midi: *mut MidiController,
    target: *mut AnyObject,
    output_device_menu: *mut AnyObject,
    output_channels_menu: *mut AnyObject,
    midi_input_menu: *mut AnyObject,
    midi_channel_menu: *mut AnyObject,
}

static MENU: AtomicPtr<MenuState> = AtomicPtr::new(ptr::null_mut());

/// Borrow the global menu state, or `None` once [`clear`] has run.
/// Safe to alias because every caller runs serially on the main
/// (`AppKit`) thread.
unsafe fn menu_state() -> Option<&'static mut MenuState> {
    unsafe { MENU.load(Ordering::Acquire).as_mut() }
}

/// Register the audio + MIDI controllers the menu will drive. Called
/// (main thread) before the window opens; the pointers must stay
/// valid until [`clear`]. Menu pointers are filled in by [`install`].
pub(crate) fn set_controllers(
    audio: *mut AudioController,
    midi: *mut MidiController,
    _channels: usize,
) {
    let state = Box::new(MenuState {
        audio,
        midi,
        target: ptr::null_mut(),
        output_device_menu: ptr::null_mut(),
        output_channels_menu: ptr::null_mut(),
        midi_input_menu: ptr::null_mut(),
        midi_channel_menu: ptr::null_mut(),
    });
    let prev = MENU.swap(Box::into_raw(state), Ordering::AcqRel);
    if !prev.is_null() {
        // SAFETY: prev came from an earlier Box::into_raw here.
        drop(unsafe { Box::from_raw(prev) });
    }
}

/// Drop the global menu state so a late menu event can't deref the
/// controllers after they leave [`crate::windowed::run`]'s scope.
pub fn clear() {
    let prev = MENU.swap(ptr::null_mut(), Ordering::AcqRel);
    if !prev.is_null() {
        // SAFETY: prev came from Box::into_raw in set_controllers.
        drop(unsafe { Box::from_raw(prev) });
    }
}

/// Build and install the menu bar. Must run on the main thread after
/// `NSApp` is wired up (inside baseview's window-build closure).
pub fn install(plugin_name: &str) {
    unsafe {
        let app_cls = class!(NSApplication);
        let app: *mut AnyObject = msg_send![app_cls, sharedApplication];

        // App menu: just Quit for now.
        let app_menu_item = make_menu_item(plugin_name);
        let app_menu = make_menu(plugin_name);
        let quit = make_quit_item(plugin_name);
        let _: () = msg_send![app_menu, addItem: quit];
        let _: () = msg_send![app_menu_item, setSubmenu: app_menu];

        let main_menu = make_menu("");
        let _: () = msg_send![main_menu, addItem: app_menu_item];

        // Settings menu — only when the controllers are registered.
        if let Some(state) = menu_state() {
            let target: *mut AnyObject = msg_send![target_class(), new];
            state.target = target;

            let settings_item = make_menu_item("Settings");
            let settings_menu = make_menu("Settings");

            state.output_device_menu =
                add_submenu(settings_menu, "Output Device", target);
            state.output_channels_menu =
                add_submenu(settings_menu, "Output Channels", target);
            add_separator(settings_menu);
            state.midi_input_menu = add_submenu(settings_menu, "MIDI Input", target);
            state.midi_channel_menu = add_submenu(settings_menu, "MIDI Channel", target);

            let _: () = msg_send![settings_item, setSubmenu: settings_menu];
            let _: () = msg_send![main_menu, addItem: settings_item];

            // Populate now so the menus are correct before first open.
            refresh_all(state);
        }

        let _: () = msg_send![app, setMainMenu: main_menu];
    }
}

// ---------------------------------------------------------------------------
// Objective-C target class
// ---------------------------------------------------------------------------

fn target_class() -> &'static AnyClass {
    static REGISTER: Once = Once::new();
    static CLASS: AtomicPtr<AnyClass> = AtomicPtr::new(ptr::null_mut());
    REGISTER.call_once(|| {
        let superclass = class!(NSObject);
        let mut builder = ClassBuilder::new(c"TruceRackMenuTarget", superclass)
            .expect("TruceRackMenuTarget already registered");
        // SAFETY: each selector matches the registered fn's signature
        // (`-(void)foo:(id)sender` / `-(void)menuWillOpen:(NSMenu*)`).
        unsafe {
            builder.add_method(
                sel!(selectOutputDeviceAction:),
                select_output_device as extern "C-unwind" fn(_, _, _),
            );
            builder.add_method(
                sel!(selectOutputChannelsAction:),
                select_output_channels as extern "C-unwind" fn(_, _, _),
            );
            builder.add_method(
                sel!(selectMidiInputAction:),
                select_midi_input as extern "C-unwind" fn(_, _, _),
            );
            builder.add_method(
                sel!(selectMidiChannelAction:),
                select_midi_channel as extern "C-unwind" fn(_, _, _),
            );
            builder.add_method(
                sel!(menuWillOpen:),
                menu_will_open as extern "C-unwind" fn(_, _, _),
            );
        }
        CLASS.store(ptr::from_ref(builder.register()).cast_mut(), Ordering::Release);
    });
    // SAFETY: set inside the Once above before any use.
    unsafe { &*CLASS.load(Ordering::Acquire).cast_const() }
}

extern "C-unwind" fn select_output_device(_this: &AnyObject, _cmd: Sel, sender: *mut AnyObject) {
    unsafe {
        let Some(state) = menu_state() else { return };
        let name = item_device_name(sender);
        (*state.audio).set_output_device(name);
        check_only(sender);
    }
}

extern "C-unwind" fn select_output_channels(_this: &AnyObject, _cmd: Sel, sender: *mut AnyObject) {
    unsafe {
        let tag: isize = msg_send![sender, tag];
        device::set_live_route(ChannelRoute::decode(usize::try_from(tag).unwrap_or(0)));
        check_only(sender);
    }
}

extern "C-unwind" fn select_midi_input(_this: &AnyObject, _cmd: Sel, sender: *mut AnyObject) {
    unsafe {
        let Some(state) = menu_state() else { return };
        let name = item_device_name(sender);
        (*state.midi).set_input(name);
        check_only(sender);
    }
}

extern "C-unwind" fn select_midi_channel(_this: &AnyObject, _cmd: Sel, sender: *mut AnyObject) {
    unsafe {
        let Some(state) = menu_state() else { return };
        let tag: isize = msg_send![sender, tag];
        let channel = decode_channel_tag(tag);
        (*state.midi).set_channel(channel);
        check_only(sender);
    }
}

extern "C-unwind" fn menu_will_open(_this: &AnyObject, _cmd: Sel, menu: *mut AnyObject) {
    unsafe {
        let Some(state) = menu_state() else { return };
        if menu == state.output_device_menu {
            populate_output_devices(state);
        } else if menu == state.output_channels_menu {
            populate_output_channels(state);
        } else if menu == state.midi_input_menu {
            populate_midi_inputs(state);
        } else if menu == state.midi_channel_menu {
            populate_midi_channels(state);
        }
    }
}

// ---------------------------------------------------------------------------
// Submenu population (also the menuWillOpen refresh)
// ---------------------------------------------------------------------------

unsafe fn refresh_all(state: &mut MenuState) {
    unsafe {
        populate_output_devices(state);
        populate_output_channels(state);
        populate_midi_inputs(state);
        populate_midi_channels(state);
    }
}

unsafe fn populate_output_devices(state: &mut MenuState) {
    unsafe {
        let menu = state.output_device_menu;
        let _: () = msg_send![menu, removeAllItems];
        let current = (*state.audio).device_name();

        let default_item = make_action_item(
            "System Default",
            sel!(selectOutputDeviceAction:),
            state.target,
            TAG_DEFAULT,
            current.is_none(),
        );
        let _: () = msg_send![menu, addItem: default_item];

        for name in device::output_device_names() {
            let checked = current == Some(name.as_str());
            let item =
                make_action_item(&name, sel!(selectOutputDeviceAction:), state.target, 0, checked);
            let _: () = msg_send![menu, addItem: item];
        }
    }
}

unsafe fn populate_output_channels(state: &mut MenuState) {
    unsafe {
        let menu = state.output_channels_menu;
        let _: () = msg_send![menu, removeAllItems];
        let channels = (*state.audio).channels();
        let current = device::live_route().encode();

        let add = |label: &str, route: ChannelRoute| {
            let tag = isize::try_from(route.encode()).unwrap_or(0);
            let item = make_action_item(
                label,
                sel!(selectOutputChannelsAction:),
                state.target,
                tag,
                route.encode() == current,
            );
            let _: () = msg_send![menu, addItem: item];
        };

        add("Direct (all channels)", ChannelRoute::Direct);
        // Stereo pairs: 1-2, 3-4, …
        let mut base = 0;
        while base + 1 < channels {
            add(&format!("{}-{}", base + 1, base + 2), ChannelRoute::Stereo { base });
            base += 2;
        }
        // Mono fold-downs onto each single channel.
        for base in 0..channels {
            add(&format!("Channel {} (mono)", base + 1), ChannelRoute::Mono { base });
        }
    }
}

unsafe fn populate_midi_inputs(state: &mut MenuState) {
    unsafe {
        let menu = state.midi_input_menu;
        let _: () = msg_send![menu, removeAllItems];
        let current = (*state.midi).input();

        let all_item = make_action_item(
            "All Ports",
            sel!(selectMidiInputAction:),
            state.target,
            TAG_DEFAULT,
            current.is_none(),
        );
        let _: () = msg_send![menu, addItem: all_item];

        let names = midi::list_midi_devices();
        if names.is_empty() {
            let none = make_disabled_item("(no MIDI inputs)");
            let _: () = msg_send![menu, addItem: none];
        }
        for name in names {
            let checked = current == Some(name.as_str());
            let item =
                make_action_item(&name, sel!(selectMidiInputAction:), state.target, 0, checked);
            let _: () = msg_send![menu, addItem: item];
        }
    }
}

unsafe fn populate_midi_channels(state: &mut MenuState) {
    unsafe {
        let menu = state.midi_channel_menu;
        let _: () = msg_send![menu, removeAllItems];
        let current = midi::live_channel();

        let omni = make_action_item(
            "Omni (all channels)",
            sel!(selectMidiChannelAction:),
            state.target,
            channel_tag(MidiChannel::Omni),
            current == MidiChannel::Omni,
        );
        let _: () = msg_send![menu, addItem: omni];

        for n in 0u8..16 {
            let ch = MidiChannel::Only(n);
            let item = make_action_item(
                &format!("Channel {}", n + 1),
                sel!(selectMidiChannelAction:),
                state.target,
                channel_tag(ch),
                current == ch,
            );
            let _: () = msg_send![menu, addItem: item];
        }
    }
}

// ---------------------------------------------------------------------------
// MIDI channel <-> menu tag
// ---------------------------------------------------------------------------

/// `Omni` → 16 (out of channel range), `Only(n)` → n.
fn channel_tag(channel: MidiChannel) -> isize {
    match channel {
        MidiChannel::Omni => 16,
        MidiChannel::Only(n) => isize::from(n),
    }
}

fn decode_channel_tag(tag: isize) -> MidiChannel {
    u8::try_from(tag)
        .ok()
        .filter(|&n| n < 16)
        .map_or(MidiChannel::Omni, MidiChannel::Only)
}

// ---------------------------------------------------------------------------
// Obj-C helpers
// ---------------------------------------------------------------------------

/// Read a device/port item's name: `None` for the default/all
/// sentinel (`tag == TAG_DEFAULT`), otherwise the item's title.
unsafe fn item_device_name(item: *mut AnyObject) -> Option<String> {
    unsafe {
        let tag: isize = msg_send![item, tag];
        if tag == TAG_DEFAULT {
            return None;
        }
        let title: *mut NSString = msg_send![item, title];
        title.as_ref().map(ToString::to_string)
    }
}

/// Turn off every sibling item's checkmark and turn on `sender`'s.
unsafe fn check_only(sender: *mut AnyObject) {
    unsafe {
        let menu: *mut AnyObject = msg_send![sender, menu];
        if menu.is_null() {
            return;
        }
        let count: isize = msg_send![menu, numberOfItems];
        for i in 0..count {
            let item: *mut AnyObject = msg_send![menu, itemAtIndex: i];
            let state = if item == sender { STATE_ON } else { STATE_OFF };
            let _: () = msg_send![item, setState: state];
        }
    }
}

unsafe fn add_submenu(parent: *mut AnyObject, title: &str, target: *mut AnyObject) -> *mut AnyObject {
    unsafe {
        let item = make_menu_item(title);
        let submenu = make_menu(title);
        // The delegate gets `menuWillOpen:` so we can repopulate /
        // refresh checkmarks right before display.
        let _: () = msg_send![submenu, setDelegate: target];
        let _: () = msg_send![item, setSubmenu: submenu];
        let _: () = msg_send![parent, addItem: item];
        submenu
    }
}

unsafe fn add_separator(menu: *mut AnyObject) {
    unsafe {
        let cls = class!(NSMenuItem);
        let sep: *mut AnyObject = msg_send![cls, separatorItem];
        let _: () = msg_send![menu, addItem: sep];
    }
}

unsafe fn make_menu(title: &str) -> *mut AnyObject {
    unsafe {
        let title_ns = NSString::from_str(title);
        let menu: *mut AnyObject = msg_send![class!(NSMenu), alloc];
        let title_ref: &NSString = &title_ns;
        msg_send![menu, initWithTitle: title_ref]
    }
}

unsafe fn make_menu_item(title: &str) -> *mut AnyObject {
    unsafe {
        let title_ns = NSString::from_str(title);
        let empty_ns = NSString::from_str("");
        let item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let title_ref: &NSString = &title_ns;
        let empty_ref: &NSString = &empty_ns;
        msg_send![
            item,
            initWithTitle: title_ref,
            action: sel!(noopAction:),
            keyEquivalent: empty_ref,
        ]
    }
}

/// A clickable item targeting our action object, carrying `tag` and
/// an initial checkmark.
unsafe fn make_action_item(
    title: &str,
    action: Sel,
    target: *mut AnyObject,
    tag: isize,
    checked: bool,
) -> *mut AnyObject {
    unsafe {
        let title_ns = NSString::from_str(title);
        let empty_ns = NSString::from_str("");
        let item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let title_ref: &NSString = &title_ns;
        let empty_ref: &NSString = &empty_ns;
        let item: *mut AnyObject = msg_send![
            item,
            initWithTitle: title_ref,
            action: action,
            keyEquivalent: empty_ref,
        ];
        let _: () = msg_send![item, setTarget: target];
        let _: () = msg_send![item, setTag: tag];
        let state = if checked { STATE_ON } else { STATE_OFF };
        let _: () = msg_send![item, setState: state];
        item
    }
}

unsafe fn make_disabled_item(title: &str) -> *mut AnyObject {
    unsafe {
        let item = make_menu_item(title);
        let _: () = msg_send![item, setEnabled: false];
        item
    }
}

unsafe fn make_quit_item(plugin_name: &str) -> *mut AnyObject {
    unsafe {
        let title_ns = NSString::from_str(&format!("Quit {plugin_name}"));
        let key_ns = NSString::from_str("q");
        let item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
        let title_ref: &NSString = &title_ns;
        let key_ref: &NSString = &key_ns;
        msg_send![
            item,
            initWithTitle: title_ref,
            action: sel!(terminate:),
            keyEquivalent: key_ref,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::{TAG_DEFAULT, channel_tag, decode_channel_tag};
    use crate::midi::MidiChannel;

    #[test]
    fn channel_tag_roundtrips() {
        assert_eq!(
            decode_channel_tag(channel_tag(MidiChannel::Omni)),
            MidiChannel::Omni
        );
        for n in 0u8..16 {
            let ch = MidiChannel::Only(n);
            assert_eq!(decode_channel_tag(channel_tag(ch)), ch);
        }
        // Out-of-range tags (and the default sentinel) fall back to omni.
        assert_eq!(decode_channel_tag(99), MidiChannel::Omni);
        assert_eq!(decode_channel_tag(TAG_DEFAULT), MidiChannel::Omni);
    }
}
