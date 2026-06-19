//! LV2 host implementation for the truce-rack framework.
//!
//! Built on `lilv-sys` — the Rust FFI for **lilv**, the standard
//! LV2 host library. lilv handles URI resolution, TTL parsing, and
//! the Turtle world load that LV2 demands; truce-rack-lv2 is the layer
//! that turns those C-API queries into [`PluginInfo`] entries and
//! drives `lilv_plugin_instantiate` / `lilv_instance_run` from the
//! truce-rack-core trait surface.
//!
//! # Status
//!
//! - **Scan** via `lilv_world_load_all`.
//! - **Load** instantiates the plugin into a `LilvInstance` with
//!   the LV2 `urid#map` feature. Each plugin owns its own world
//!   (instance pointers reference world-owned data).
//! - **Process** connects audio / control / atom-sequence ports
//!   per block and calls `lilv_instance_run`. Audio ports point
//!   straight at the host's planes (zero-copy); control ports
//!   carry their declared default value; the MIDI input atom port
//!   is rebuilt each block from the truce-rack [`EventList`].
//! - **MIDI** in via atom sequence ports tagged `midi:MidiEvent`.
//!   MIDI out is not yet drained back to the truce-rack host.
//!
//! # Build dependency
//!
//! `lilv-sys` links against the system `lilv-0` library. On macOS,
//! `brew install lilv`. On Debian/Ubuntu, `apt install liblilv-dev`.
//! Without those, `cargo build -p truce-rack-lv2` will fail at link
//! time.

#![allow(
    // Atom struct sizes are well under u32::MAX; the casts are for
    // FFI fields that are themselves u32.
    clippy::cast_possible_truncation,
    // Vec<u8> allocations come from the global allocator with at
    // least 8-byte alignment; safe to cast to atom-header pointers.
    clippy::cast_ptr_alignment,
    // `&x as *const _` reads cleaner here than `&raw const x`.
    clippy::borrow_as_ptr,
    clippy::ref_as_ptr,
    clippy::ptr_as_ptr
)]

use truce_rack_core::buffer::AudioBuffer;
use truce_rack_core::bus::{BusLayout, ChannelConfig};
use truce_rack_core::editor::{PluginEditor, WindowHandle};
use truce_rack_core::error::{Error, Result};
use truce_rack_core::events::{Event, EventBody, EventList, MidiData};
use truce_rack_core::info::{ParameterInfo, PluginCategory, PluginInfo, PresetInfo};
use truce_rack_core::plugin::{Plugin, PluginCore, ProcessContext, ProcessStatus};
use truce_rack_core::scanner::PluginScanner;
use truce_rack_core::transport::TransportInfo;

use lv2_raw::atom::{
    LV2Atom, LV2AtomEvent, LV2AtomObjectBody, LV2AtomPropertyBody, LV2AtomSequence,
    LV2AtomSequenceBody,
};
use lv2_raw::core::LV2Feature;
use lv2_raw::ui::{LV2UIControllerRaw, LV2UIDescriptorRaw, LV2UIHandle, LV2UIWidget};
use lv2_raw::urid::{LV2Urid, LV2UridMap, LV2UridMapHandle};

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Format identifier used on returned [`PluginInfo`].
pub const FORMAT: &str = "lv2";

const LV2_URID_MAP_URI: &[u8] = b"http://lv2plug.in/ns/ext/urid#map\0";
const LV2_ATOM_SEQUENCE_URI: &[u8] = b"http://lv2plug.in/ns/ext/atom#Sequence\0";
const LV2_UI_PARENT_URI: &[u8] = b"http://lv2plug.in/ns/extensions/ui#parent\0";
const LV2_UI_RESIZE_URI: &[u8] = b"http://lv2plug.in/ns/extensions/ui#resize\0";
const LV2_UI_IDLE_INTERFACE_URI: &[u8] = b"http://lv2plug.in/ns/extensions/ui#idleInterface\0";
const LV2_UI_RESIZE_INTERFACE_URI: &[u8] = LV2_UI_RESIZE_URI;

#[cfg(target_os = "macos")]
const NATIVE_UI_CLASS_URI: &[u8] = b"http://lv2plug.in/ns/extensions/ui#CocoaUI\0";
#[cfg(target_os = "windows")]
const NATIVE_UI_CLASS_URI: &[u8] = b"http://lv2plug.in/ns/extensions/ui#WindowsUI\0";
#[cfg(all(unix, not(target_os = "macos")))]
const NATIVE_UI_CLASS_URI: &[u8] = b"http://lv2plug.in/ns/extensions/ui#X11UI\0";

/// LV2 scanner.
#[derive(Debug, Default)]
pub struct Lv2Scanner;

impl Lv2Scanner {
    /// Construct a default scanner.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl PluginScanner for Lv2Scanner {
    type Plugin = Lv2Plugin;

    fn scan(&self) -> Result<Vec<PluginInfo>> {
        let world = unsafe { World::new() }
            .ok_or_else(|| Error::Other("lilv_world_new returned NULL".into()))?;
        unsafe { lilv_sys::lilv_world_load_all(world.ptr) };
        Ok(unsafe { world.collect_plugin_infos() })
    }

    fn scan_path(&self, _path: &Path) -> Result<Vec<PluginInfo>> {
        // LV2's discovery model is URI-based, not path-based — the
        // host calls `lilv_world_load_all` and lilv consults
        // LV2_PATH / standard locations. A path-bounded scan would
        // need `lilv_world_load_bundle` against each subdirectory;
        // tracked as a follow-on.
        Err(Error::Other(
            "truce-rack-lv2 path-bounded scan not yet implemented".into(),
        ))
    }

    fn load(&self, info: &PluginInfo) -> Result<Self::Plugin> {
        Lv2Plugin::load_from(info)
    }
}

// ---------------------------------------------------------------------------
// World wrapper
// ---------------------------------------------------------------------------

/// RAII wrapper around `*mut LilvWorld`. Holds the world for the
/// lifetime of either a scan or a loaded plugin (instance pointers
/// reference world-owned data, so the world must outlive them).
struct World {
    ptr: *mut lilv_sys::LilvWorld,
}

impl World {
    unsafe fn new() -> Option<Self> {
        let ptr = unsafe { lilv_sys::lilv_world_new() };
        if ptr.is_null() {
            None
        } else {
            Some(Self { ptr })
        }
    }

    unsafe fn collect_plugin_infos(&self) -> Vec<PluginInfo> {
        // Stash this world for the per-plugin classifiers to reach
        // (lilv exposes no `plugin → world` accessor). Cleared
        // before we return so the TLS pointer doesn't outlive
        // the borrow.
        SCAN_WORLD.with(|w| w.set(self.ptr));
        let mut out = Vec::new();
        let plugins = unsafe { lilv_sys::lilv_world_get_all_plugins(self.ptr) };
        if plugins.is_null() {
            SCAN_WORLD.with(|w| w.set(std::ptr::null_mut()));
            return out;
        }
        let audio_uri = unsafe { self.new_uri(lilv_sys::LILV_URI_AUDIO_PORT.as_ptr().cast()) };
        let input_uri = unsafe { self.new_uri(lilv_sys::LILV_URI_INPUT_PORT.as_ptr().cast()) };
        let output_uri = unsafe { self.new_uri(lilv_sys::LILV_URI_OUTPUT_PORT.as_ptr().cast()) };
        let atom_uri = unsafe { self.new_uri(lilv_sys::LILV_URI_ATOM_PORT.as_ptr().cast()) };
        let mut it = unsafe { lilv_sys::lilv_plugins_begin(plugins) };
        loop {
            if unsafe { lilv_sys::lilv_plugins_is_end(plugins, it) } {
                break;
            }
            let plugin = unsafe { lilv_sys::lilv_plugins_get(plugins, it) };
            if !plugin.is_null() {
                out.push(unsafe {
                    plugin_to_info(plugin, audio_uri, input_uri, output_uri, atom_uri)
                });
            }
            it = unsafe { lilv_sys::lilv_plugins_next(plugins, it) };
        }
        unsafe {
            lilv_sys::lilv_node_free(audio_uri);
            lilv_sys::lilv_node_free(input_uri);
            lilv_sys::lilv_node_free(output_uri);
            lilv_sys::lilv_node_free(atom_uri);
        }
        SCAN_WORLD.with(|w| w.set(std::ptr::null_mut()));
        out
    }

    unsafe fn new_uri(&self, uri: *const c_char) -> *mut lilv_sys::LilvNode {
        unsafe { lilv_sys::lilv_new_uri(self.ptr, uri.cast()) }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        unsafe { lilv_sys::lilv_world_free(self.ptr) };
    }
}

// SAFETY: lilv's world is meant to be single-threaded for mutation
// but we hand the wrapper between activate/process on the same
// thread the host owns. We never share it.
unsafe impl Send for World {}

unsafe fn plugin_to_info(
    plugin: *const lilv_sys::LilvPlugin,
    audio_uri: *mut lilv_sys::LilvNode,
    input_uri: *mut lilv_sys::LilvNode,
    output_uri: *mut lilv_sys::LilvNode,
    atom_uri: *mut lilv_sys::LilvNode,
) -> PluginInfo {
    let uri_node = unsafe { lilv_sys::lilv_plugin_get_uri(plugin) };
    let uri = unsafe { node_to_uri_string(uri_node) };

    let name_node = unsafe { lilv_sys::lilv_plugin_get_name(plugin) };
    let name = unsafe { node_to_string_owned(name_node) };
    unsafe { lilv_sys::lilv_node_free(name_node) };

    let author_node = unsafe { lilv_sys::lilv_plugin_get_author_name(plugin) };
    let vendor = if author_node.is_null() {
        String::new()
    } else {
        let v = unsafe { node_to_string_owned(author_node) };
        unsafe { lilv_sys::lilv_node_free(author_node) };
        v
    };

    let mut audio_in = 0u32;
    let mut audio_out = 0u32;
    let mut accepts_midi = false;
    let count = unsafe { lilv_sys::lilv_plugin_get_num_ports(plugin) };
    for idx in 0..count {
        let port = unsafe { lilv_sys::lilv_plugin_get_port_by_index(plugin, idx) };
        if port.is_null() {
            continue;
        }
        let is_input = unsafe { lilv_sys::lilv_port_is_a(plugin, port, input_uri) };
        if unsafe { lilv_sys::lilv_port_is_a(plugin, port, audio_uri) } {
            if is_input {
                audio_in += 1;
            } else if unsafe { lilv_sys::lilv_port_is_a(plugin, port, output_uri) } {
                audio_out += 1;
            }
        } else if is_input && unsafe { lilv_sys::lilv_port_is_a(plugin, port, atom_uri) } {
            // Best-effort: any atom-port input is treated as a MIDI
            // sink for the catalog. The actual MIDI-vs-not check
            // requires walking the port's `atom:supports` triples,
            // which we do at load time.
            accepts_midi = true;
        }
    }
    let category = if audio_in == 0 && audio_out > 0 {
        PluginCategory::Instrument
    } else {
        PluginCategory::Effect
    };

    let has_editor = unsafe { has_native_ui(plugin) };

    PluginInfo {
        name,
        vendor,
        version: 0,
        category,
        path: std::path::PathBuf::new(),
        unique_id: uri,
        format: FORMAT,
        has_editor,
        accepts_midi,
    }
}

/// True if the plugin advertises a UI of this platform's native class
/// (`CocoaUI` / `WindowsUI` / `X11UI`).
unsafe fn has_native_ui(plugin: *const lilv_sys::LilvPlugin) -> bool {
    let uis = unsafe { lilv_sys::lilv_plugin_get_uis(plugin) };
    if uis.is_null() {
        return false;
    }
    let world_ptr = unsafe { lilv_plugin_world(plugin) };
    if world_ptr.is_null() {
        unsafe { lilv_sys::lilv_uis_free(uis) };
        return false;
    }
    let class_node =
        unsafe { lilv_sys::lilv_new_uri(world_ptr, NATIVE_UI_CLASS_URI.as_ptr().cast()) };
    let mut found = false;
    let mut it = unsafe { lilv_sys::lilv_uis_begin(uis) };
    while !unsafe { lilv_sys::lilv_uis_is_end(uis, it) } {
        let ui = unsafe { lilv_sys::lilv_uis_get(uis, it) };
        if !ui.is_null() && unsafe { lilv_sys::lilv_ui_is_a(ui, class_node) } {
            found = true;
            break;
        }
        it = unsafe { lilv_sys::lilv_uis_next(uis, it) };
    }
    unsafe {
        lilv_sys::lilv_node_free(class_node);
        lilv_sys::lilv_uis_free(uis);
    }
    found
}

/// Recover the world pointer from a plugin pointer. lilv stores it
/// internally but doesn't expose it directly; we cheat by stashing
/// it into a thread-local during `collect_plugin_infos`.
unsafe fn lilv_plugin_world(_plugin: *const lilv_sys::LilvPlugin) -> *mut lilv_sys::LilvWorld {
    SCAN_WORLD.with(std::cell::Cell::get)
}

thread_local! {
    /// Set by `collect_plugin_infos` for the duration of one scan
    /// pass so the per-plugin classifiers can `lilv_new_uri`
    /// against the same world. Cleared on Drop of the World guard.
    static SCAN_WORLD: std::cell::Cell<*mut lilv_sys::LilvWorld> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
}

unsafe fn node_to_uri_string(node: *const lilv_sys::LilvNode) -> String {
    if node.is_null() {
        return String::new();
    }
    let p = unsafe { lilv_sys::lilv_node_as_uri(node) };
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

unsafe fn node_to_string_owned(node: *mut lilv_sys::LilvNode) -> String {
    if node.is_null() {
        return String::new();
    }
    let p = unsafe { lilv_sys::lilv_node_as_string(node) };
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// URID map + features
// ---------------------------------------------------------------------------

/// Bidirectional URI ↔ u32 map handed to LV2 plugins as the
/// standard `urid#map` feature.
struct UridMap {
    entries: Mutex<HashMap<CString, LV2Urid>>,
    next_id: Mutex<LV2Urid>,
}

impl UridMap {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            // Reserve 0 as the LV2-spec "couldn't map" sentinel.
            next_id: Mutex::new(1),
        }
    }

    fn map_uri(&self, uri: &CStr) -> LV2Urid {
        let mut entries = self.entries.lock().expect("urid map mutex");
        if let Some(&id) = entries.get(uri) {
            return id;
        }
        let mut next = self.next_id.lock().expect("urid next mutex");
        let id = *next;
        *next += 1;
        entries.insert(uri.to_owned(), id);
        id
    }
}

extern "C" fn map_callback(handle: LV2UridMapHandle, uri: *const c_char) -> LV2Urid {
    if handle.is_null() || uri.is_null() {
        return 0;
    }
    // SAFETY: `handle` is the `&UridMap` we set into the feature
    // struct at instantiate time; lilv passes it back unchanged.
    let map = unsafe { &*(handle as *const UridMap) };
    let cstr = unsafe { CStr::from_ptr(uri) };
    map.map_uri(cstr)
}

/// Self-contained LV2 feature list. Heap-allocated so the pointers
/// fed to `lilv_plugin_instantiate` stay valid for the instance's
/// lifetime even if `Lv2Plugin` is moved.
struct Features {
    urid_map: UridMap,
    // `LV2UridMap.handle` points at `urid_map` — the box's pinned
    // address. `LV2UridMap.map` is the static callback. Held inline
    // so its address can be taken for `feature.data`.
    map_struct: LV2UridMap,
    // The features array proper. Each `data` field points at a
    // sibling field of the same `Features` allocation.
    feature_storage: Vec<LV2Feature>,
    // Null-terminated pointer array — what `lilv_plugin_instantiate`
    // actually consumes.
    feature_ptrs: Vec<*const LV2Feature>,
}

impl Features {
    fn new() -> Box<Self> {
        // Build in two passes so the `Box`'s heap address is stable
        // before we record interior pointers.
        let mut boxed = Box::new(Features {
            urid_map: UridMap::new(),
            map_struct: LV2UridMap {
                handle: std::ptr::null_mut(),
                map: map_callback,
            },
            feature_storage: Vec::with_capacity(1),
            feature_ptrs: Vec::with_capacity(2),
        });
        let urid_map_handle: *mut UridMap = &mut boxed.urid_map as *mut UridMap;
        boxed.map_struct.handle = urid_map_handle.cast::<c_void>();
        boxed.feature_storage.push(LV2Feature {
            uri: LV2_URID_MAP_URI.as_ptr().cast::<c_char>(),
            data: (&boxed.map_struct) as *const LV2UridMap as *mut c_void,
        });
        boxed.feature_ptrs.push(&boxed.feature_storage[0]);
        boxed.feature_ptrs.push(std::ptr::null());
        boxed
    }
}

// ---------------------------------------------------------------------------
// UI plumbing
// ---------------------------------------------------------------------------

/// Metadata about a plugin's native UI bundle, captured at load
/// time. `binary_path` is the absolute filesystem path to the
/// `.so`/`.dylib`/`.dll` that exports `lv2ui_descriptor`.
#[derive(Debug, Clone)]
struct UiBundle {
    ui_uri: CString,
    bundle_path: PathBuf,
    binary_path: PathBuf,
}

/// Live editor state. Created on `PluginEditor::open`; torn down on
/// `close` or `Drop`. Kept on `Lv2Plugin` so the dropped instance's
/// `lv2ui_descriptor.cleanup` runs before the audio instance is
/// freed.
struct EditorState {
    /// The dlopen'd UI bundle. Held so the descriptor / function
    /// pointers stay valid; dropped after `cleanup` returns.
    _library: libloading::Library,
    descriptor: *const LV2UIDescriptorRaw,
    handle: LV2UIHandle,
    widget: LV2UIWidget,
    /// Self-contained features array passed to `instantiate`.
    /// Boxed so the pointers we recorded into the LV2 plugin stay
    /// valid even if `Lv2Plugin` itself is moved. Held for ownership
    /// only — the UI side reaches into it through the raw feature
    /// pointers it was given at instantiate time.
    #[allow(dead_code)]
    ui_features: Box<UiFeatureStorage>,
    /// Optional `LV2_UI__idleInterface` — non-null if the UI exports
    /// it via `extension_data`. Driven by `on_idle` once per host
    /// frame.
    idle_iface: *const lv2_raw::ui::LV2UIIdleInterface,
    /// Optional `LV2_UI__resize` interface — non-null if the UI
    /// exports it via `extension_data`. Used by `set_size` to push
    /// host-driven resizes to the UI.
    resize_iface: *const Lv2UiResizeInterface,
    /// Snapshot of `control_values` taken at open time and refreshed
    /// every `on_idle` so we can fire `port_event` for any value the
    /// audio thread (or another UI write) has changed since the last
    /// idle tick.
    last_pushed_values: Vec<f32>,
}

/// LV2 host-side `ui:resize` interface — the same shape both the
/// host implements (passed via the feature) and the UI implements
/// (returned from `extension_data`). Non-zero `ui_resize` returns
/// indicate failure.
#[repr(C)]
struct Lv2UiResizeInterface {
    /// Opaque pointer the UI passes back. For host → UI, this is
    /// the UI's own handle.
    handle: *mut c_void,
    ui_resize: extern "C" fn(handle: *mut c_void, width: i32, height: i32) -> i32,
}

/// Heap-stable storage for the LV2 UI feature array. Mirrors the
/// shape of [`Features`] for the audio side.
struct UiFeatureStorage {
    /// Same URID map the audio instance uses — the UI talks to the
    /// host through the same map.
    map_struct: LV2UridMap,
    /// `ui#parent` feature. `data` is a raw pointer to the parent
    /// widget (`NSView`* / HWND / X11 Window).
    parent_feature_data: *mut c_void,
    /// Host-side `ui:resize` callback. The UI calls
    /// `resize_struct.ui_resize(resize_struct.handle, w, h)` to ask
    /// the host to resize. The handle is a pointer to a
    /// `HostResizeNotifier` heap-allocated alongside us; the host
    /// reads the latest requested size out of it via `take_request`.
    resize_struct: Lv2UiResizeInterface,
    resize_notifier: Box<HostResizeNotifier>,
    feature_storage: Vec<LV2Feature>,
    feature_ptrs: Vec<*const LV2Feature>,
}

/// Heap-stable cell the UI's `ui_resize` callback writes its
/// requested dimensions into. The standalone polls this every
/// `on_idle` and applies any pending request.
struct HostResizeNotifier {
    /// `(width, height)` requested by the UI; `None` means no
    /// pending request. Written by the UI thread, read by the host
    /// — both run on the same main thread, but the field is
    /// touched from C code outside Rust's borrow tracking, so we
    /// use `Cell` to make the interior mutability explicit.
    pending: std::cell::Cell<Option<(i32, i32)>>,
}

extern "C" fn host_resize_callback(handle: *mut c_void, width: i32, height: i32) -> i32 {
    if handle.is_null() {
        return 1;
    }
    // SAFETY: `handle` is the boxed `HostResizeNotifier` we stored
    // in the resize_struct at construction. lilv passes it back
    // unchanged.
    let notifier = unsafe { &*(handle as *const HostResizeNotifier) };
    notifier.pending.set(Some((width, height)));
    0
}

/// Heap-allocated controller passed back to the UI as the opaque
/// `controller` argument of `instantiate` and `write_function`. The
/// UI hands the same pointer back unchanged; we cast it to
/// `&Controller` to route the write into our `control_values` vec.
// Field names share the `control_` prefix because they all describe
// the control-port shuttle — the prefix is meaningful, not noise.
#[allow(clippy::struct_field_names)]
struct Controller {
    /// Pointer to the head of `Lv2Plugin::control_values`. Stable
    /// for the lifetime of the plugin (we never reallocate after
    /// `load_from`). The audio thread reads through the same
    /// pointer via `lilv_instance_connect_port` — a benign data
    /// race on aligned f32 stores, accepted by every LV2 host in
    /// the wild.
    control_values_base: *mut f32,
    control_values_len: usize,
    /// Map from LV2 port index to the `control_values` offset, or
    /// `usize::MAX` if the port isn't a control port.
    control_value_offset: Vec<usize>,
}

extern "C" fn ui_write_callback(
    controller: LV2UIControllerRaw,
    port_index: libc::c_uint,
    buffer_size: libc::c_uint,
    port_protocol: libc::c_uint,
    buffer: *const c_void,
) {
    if controller.is_null() || buffer.is_null() {
        return;
    }
    // Only the implicit float protocol (port_protocol == 0) updates
    // a control port; richer protocols (atom event transfer, etc.)
    // are not yet plumbed back into the audio side.
    if port_protocol != 0 || buffer_size != 4 {
        return;
    }
    // SAFETY: `controller` is the boxed `Controller` we set on
    // `instantiate`. lilv passes it back unchanged. Const-cast is
    // safe because we hold the only writer.
    let ctrl = unsafe { &*(controller as *const Controller) };
    let Some(&off) = ctrl.control_value_offset.get(port_index as usize) else {
        return;
    };
    if off == usize::MAX || off >= ctrl.control_values_len {
        return;
    }
    let value = unsafe { *(buffer as *const f32) };
    // SAFETY: writing to a u32-sized aligned slot inside the
    // control_values Vec. See doc comment on `control_values_base`.
    unsafe {
        *ctrl.control_values_base.add(off) = value;
    }
}

impl UiFeatureStorage {
    // The `Box` return is load-bearing: the caller stores raw
    // pointers into this struct's interior fields, so the heap
    // allocation must outlive `Lv2Plugin` moves.
    #[allow(clippy::unnecessary_box_returns)]
    fn new(map_handle: *mut UridMap, parent: *mut c_void) -> Box<Self> {
        let mut boxed = Box::new(UiFeatureStorage {
            map_struct: LV2UridMap {
                handle: std::ptr::null_mut(),
                map: map_callback,
            },
            parent_feature_data: parent,
            resize_struct: Lv2UiResizeInterface {
                handle: std::ptr::null_mut(),
                ui_resize: host_resize_callback,
            },
            resize_notifier: Box::new(HostResizeNotifier {
                pending: std::cell::Cell::new(None),
            }),
            feature_storage: Vec::with_capacity(3),
            feature_ptrs: Vec::with_capacity(4),
        });
        boxed.map_struct.handle = map_handle.cast::<c_void>();
        boxed.resize_struct.handle =
            (&*boxed.resize_notifier) as *const HostResizeNotifier as *mut c_void;
        boxed.feature_storage.push(LV2Feature {
            uri: LV2_URID_MAP_URI.as_ptr().cast::<c_char>(),
            data: (&boxed.map_struct) as *const LV2UridMap as *mut c_void,
        });
        boxed.feature_storage.push(LV2Feature {
            uri: LV2_UI_PARENT_URI.as_ptr().cast::<c_char>(),
            data: boxed.parent_feature_data,
        });
        boxed.feature_storage.push(LV2Feature {
            uri: LV2_UI_RESIZE_URI.as_ptr().cast::<c_char>(),
            data: (&boxed.resize_struct) as *const Lv2UiResizeInterface as *mut c_void,
        });
        for i in 0..boxed.feature_storage.len() {
            boxed.feature_ptrs.push(&boxed.feature_storage[i]);
        }
        boxed.feature_ptrs.push(std::ptr::null());
        boxed
    }
}

/// Walk `plugin`'s UIs for one matching the host's native UI class.
/// Returns the first match's metadata or `None`.
unsafe fn discover_ui(
    world: *mut lilv_sys::LilvWorld,
    plugin: *const lilv_sys::LilvPlugin,
) -> Option<UiBundle> {
    let uis = unsafe { lilv_sys::lilv_plugin_get_uis(plugin) };
    if uis.is_null() {
        return None;
    }
    let class_node = unsafe { lilv_sys::lilv_new_uri(world, NATIVE_UI_CLASS_URI.as_ptr().cast()) };
    let mut chosen: Option<UiBundle> = None;
    let mut it = unsafe { lilv_sys::lilv_uis_begin(uis) };
    while !unsafe { lilv_sys::lilv_uis_is_end(uis, it) } {
        let ui = unsafe { lilv_sys::lilv_uis_get(uis, it) };
        if !ui.is_null() && unsafe { lilv_sys::lilv_ui_is_a(ui, class_node) } {
            let uri_str = unsafe {
                let n = lilv_sys::lilv_ui_get_uri(ui);
                node_to_uri_string(n)
            };
            let bundle_path = unsafe { node_uri_to_path(lilv_sys::lilv_ui_get_bundle_uri(ui)) };
            let binary_path = unsafe { node_uri_to_path(lilv_sys::lilv_ui_get_binary_uri(ui)) };
            if let (Ok(uri_c), Some(bundle), Some(binary)) =
                (CString::new(uri_str), bundle_path, binary_path)
            {
                chosen = Some(UiBundle {
                    ui_uri: uri_c,
                    bundle_path: bundle,
                    binary_path: binary,
                });
                break;
            }
        }
        it = unsafe { lilv_sys::lilv_uis_next(uis, it) };
    }
    unsafe {
        lilv_sys::lilv_node_free(class_node);
        lilv_sys::lilv_uis_free(uis);
    }
    chosen
}

unsafe fn node_uri_to_path(node: *const lilv_sys::LilvNode) -> Option<PathBuf> {
    if node.is_null() {
        return None;
    }
    let uri = unsafe { lilv_sys::lilv_node_as_uri(node) };
    if uri.is_null() {
        return None;
    }
    let raw = unsafe { lilv_sys::lilv_file_uri_parse(uri, std::ptr::null_mut()) };
    if raw.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned();
    unsafe { lilv_sys::lilv_free(raw.cast()) };
    Some(PathBuf::from(s))
}

// ---------------------------------------------------------------------------
// Lv2Plugin
// ---------------------------------------------------------------------------

/// Cached metadata about one LV2 port.
#[derive(Debug, Clone, Copy)]
struct PortInfo {
    index: u32,
    kind: PortKind,
    is_input: bool,
}

#[derive(Debug, Clone, Copy)]
enum PortKind {
    Audio,
    Control { default: f32 },
    AtomSequence,
    Other,
}

/// URIDs needed to build a `time:Position` object atom, mapped once
/// at load. The `atom_*` entries are the value types each property
/// carries; the rest are the `time:` properties themselves.
#[derive(Clone, Copy)]
struct TimeUrids {
    position: LV2Urid,
    frame: LV2Urid,
    speed: LV2Urid,
    bar: LV2Urid,
    bar_beat: LV2Urid,
    beats_per_bar: LV2Urid,
    beat_unit: LV2Urid,
    bpm: LV2Urid,
    atom_long: LV2Urid,
    atom_float: LV2Urid,
    atom_int: LV2Urid,
    atom_object: LV2Urid,
}

/// One loaded LV2 plugin.
///
/// Owns its `World` (lilv data is referenced by pointer from the
/// instance), its features, its per-port metadata, and the per-port
/// scratch buffers connected on every block.
pub struct Lv2Plugin {
    info: PluginInfo,
    layouts: Vec<BusLayout>,
    active_layout: Option<BusLayout>,

    /// Held for ownership: the `LilvPlugin` pointer and the instance
    /// both reference world-owned data, so the world must outlive
    /// them. Read directly only at load.
    #[allow(dead_code)]
    world: Box<World>,
    plugin: *const lilv_sys::LilvPlugin,
    instance: *mut lilv_sys::LilvInstance,

    sample_rate: f64,

    ports: Vec<PortInfo>,
    /// Backing storage for control ports. `connect_port` points the
    /// plugin at one f32 per control port; the index here matches
    /// the position in `ports` filtered to controls.
    control_values: Vec<f32>,
    /// Map from `ports` index to the offset in `control_values`.
    control_value_offset: Vec<usize>,

    /// Backing storage for the (single) MIDI input atom-sequence
    /// port, if the plugin has one. Rebuilt each block from the
    /// truce-rack `EventList`.
    midi_in_buf: Vec<u8>,
    /// Backing storage for the MIDI output atom-sequence port. We
    /// connect a chunk-typed buffer so the plugin has somewhere to
    /// write; we don't currently drain it back into the host.
    midi_out_buf: Vec<u8>,

    features: Box<Features>,
    midi_event_urid: LV2Urid,
    atom_sequence_urid: LV2Urid,
    time_urids: TimeUrids,

    /// UI metadata captured at load if the plugin advertises a
    /// native UI for this platform. `None` means no editor — the
    /// `PluginEditor` impl will refuse `open`.
    ui_bundle: Option<UiBundle>,
    /// Live editor — `Some` between `open` and `close`.
    editor: Option<EditorState>,
    /// Heap-stable controller passed to the LV2 UI as the opaque
    /// callback context. Built lazily on first `open`. Boxed so its
    /// address survives moves of `Lv2Plugin`.
    controller: Option<Box<Controller>>,
}

// SAFETY: We hand the plugin between the audio and main threads
// behind an `Arc<Mutex<_>>` exactly like every other truce-rack format.
unsafe impl Send for Lv2Plugin {}

impl Lv2Plugin {
    fn load_from(info: &PluginInfo) -> Result<Self> {
        let world = Box::new(unsafe { World::new() }.ok_or_else(|| Error::LoadFailed {
            path: info.path.clone(),
            reason: "lilv_world_new returned NULL".into(),
        })?);
        unsafe { lilv_sys::lilv_world_load_all(world.ptr) };

        let uri = CString::new(info.unique_id.clone()).map_err(|_| Error::LoadFailed {
            path: info.path.clone(),
            reason: format!("lv2 unique_id contains NUL: {:?}", info.unique_id),
        })?;
        let uri_node = unsafe { lilv_sys::lilv_new_uri(world.ptr, uri.as_ptr()) };
        if uri_node.is_null() {
            return Err(Error::LoadFailed {
                path: info.path.clone(),
                reason: format!("lilv_new_uri failed for {:?}", info.unique_id),
            });
        }
        let plugins = unsafe { lilv_sys::lilv_world_get_all_plugins(world.ptr) };
        let plugin = unsafe { lilv_sys::lilv_plugins_get_by_uri(plugins, uri_node) };
        unsafe { lilv_sys::lilv_node_free(uri_node) };
        if plugin.is_null() {
            return Err(Error::LoadFailed {
                path: info.path.clone(),
                reason: format!("no LV2 plugin matching URI {:?}", info.unique_id),
            });
        }

        // Walk the ports once, classify each, and stash
        // direction + control defaults.
        let (ports, audio_in_count, audio_out_count, control_value_offset, control_values) =
            unsafe { classify_ports(world.ptr, plugin) };

        // Pre-map the URIs we'll need at process time.
        let features = Features::new();
        let midi_event_urid = features
            .urid_map
            .map_uri(unsafe { CStr::from_ptr(lilv_sys::LILV_URI_MIDI_EVENT.as_ptr().cast()) });
        let atom_sequence_urid = features
            .urid_map
            .map_uri(unsafe { CStr::from_ptr(LV2_ATOM_SEQUENCE_URI.as_ptr().cast()) });

        // URIDs for the host transport (`time:Position`) atom we
        // inject each block. Mapped once here so process() stays
        // allocation-free.
        let map_uri =
            |uri: &[u8]| features.urid_map.map_uri(unsafe { CStr::from_ptr(uri.as_ptr().cast()) });
        let time_urids = TimeUrids {
            position: map_uri(lv2_raw::time::LV2_TIME__POSITION),
            frame: map_uri(lv2_raw::time::LV2_TIME__FRAME),
            speed: map_uri(lv2_raw::time::LV2_TIME__SPEED),
            bar: map_uri(lv2_raw::time::LV2_TIME__BAR),
            bar_beat: map_uri(lv2_raw::time::LV2_TIME__BARBEAT),
            beats_per_bar: map_uri(lv2_raw::time::LV2_TIME__BEATSPERBAR),
            beat_unit: map_uri(lv2_raw::time::LV2_TIME__BEATUNIT),
            bpm: map_uri(lv2_raw::time::LV2_TIME__BEATSPERMINUTE),
            atom_long: map_uri(lv2_raw::atom::LV2_ATOM__LONG),
            atom_float: map_uri(lv2_raw::atom::LV2_ATOM__FLOAT),
            atom_int: map_uri(lv2_raw::atom::LV2_ATOM__INT),
            atom_object: map_uri(lv2_raw::atom::LV2_ATOM__OBJECT),
        };

        let layout = audio_layout(audio_in_count, audio_out_count);

        let ui_bundle = unsafe { discover_ui(world.ptr, plugin) };
        let mut info = info.clone();
        info.has_editor = ui_bundle.is_some();

        Ok(Self {
            info,
            layouts: vec![layout],
            active_layout: None,
            world,
            plugin,
            instance: std::ptr::null_mut(),
            sample_rate: 0.0,
            ports,
            control_values,
            control_value_offset,
            midi_in_buf: Vec::new(),
            midi_out_buf: Vec::new(),
            features,
            midi_event_urid,
            atom_sequence_urid,
            time_urids,
            ui_bundle,
            editor: None,
            controller: None,
        })
    }

    /// Build (or rebuild) the heap-stable Controller that the UI
    /// uses as its callback context. Must be called only when
    /// `control_values` is no longer going to be reallocated —
    /// after `load_from` completes that's true for the plugin's
    /// lifetime.
    fn ensure_controller(&mut self) {
        if self.controller.is_some() {
            return;
        }
        self.controller = Some(Box::new(Controller {
            control_values_base: self.control_values.as_mut_ptr(),
            control_values_len: self.control_values.len(),
            control_value_offset: self.control_value_offset.clone(),
        }));
    }

    fn build_midi_sequence(&mut self, events: &EventList, transport: Option<TransportInfo>) {
        let header_size = std::mem::size_of::<LV2AtomSequence>();
        let cap = self.midi_in_buf.len();
        if cap < header_size {
            return;
        }
        let buf = self.midi_in_buf.as_mut_ptr();

        // Header: capacity-bounded sequence whose `atom.size` only
        // counts the body + events (the LV2 atom convention).
        let body_size = std::mem::size_of::<LV2AtomSequenceBody>();
        let seq = unsafe { &mut *buf.cast::<LV2AtomSequence>() };
        seq.atom = LV2Atom {
            size: body_size as u32,
            mytype: self.atom_sequence_urid,
        };
        seq.body = LV2AtomSequenceBody { unit: 0, pad: 0 };

        let mut write_off = header_size;

        // Inject a `time:Position` object as the first event (frame
        // 0) so transport-aware plugins see tempo / grid before any
        // MIDI. MIDI events follow, also at frame >= 0.
        if let Some(t) = transport {
            write_off = unsafe { write_time_position(&self.time_urids, buf, write_off, cap, &t) };
        }

        let event_header = std::mem::size_of::<LV2AtomEvent>();
        for ev in events {
            let Some((status, d1, d2, len)) = midi_bytes(&ev.body) else {
                continue;
            };
            let total = event_header + len;
            let padded = (total + 7) & !7; // 8-byte alignment per atom spec
            if write_off + padded > cap {
                break;
            }
            let event_ptr = unsafe { buf.add(write_off) }.cast::<LV2AtomEvent>();
            unsafe {
                (*event_ptr).time_in_frames = i64::from(ev.sample_offset);
                (*event_ptr).body = LV2Atom {
                    size: len as u32,
                    mytype: self.midi_event_urid,
                };
                let data_ptr = (event_ptr as *mut u8).add(event_header);
                if len >= 1 {
                    *data_ptr = status;
                }
                if len >= 2 {
                    *data_ptr.add(1) = d1;
                }
                if len >= 3 {
                    *data_ptr.add(2) = d2;
                }
            }
            write_off += padded;
        }
        // sequence body size is everything past the atom header
        seq.atom.size = (write_off - std::mem::size_of::<LV2Atom>()) as u32;
    }

    fn prep_midi_out(&mut self) {
        // Reset to an empty Chunk-typed atom so the plugin has a
        // defined buffer to write into. We don't drain output yet.
        if self.midi_out_buf.len() < std::mem::size_of::<LV2AtomSequence>() {
            return;
        }
        let buf = self.midi_out_buf.as_mut_ptr();
        let seq = unsafe { &mut *buf.cast::<LV2AtomSequence>() };
        seq.atom = LV2Atom {
            size: std::mem::size_of::<LV2AtomSequenceBody>() as u32,
            mytype: self.atom_sequence_urid,
        };
        seq.body = LV2AtomSequenceBody { unit: 0, pad: 0 };
    }
}

/// Upper bound on the bytes one `time:Position` event consumes:
/// event + object header plus eight 8-byte-aligned properties. Used
/// to bounds-check once instead of per-property.
const TIME_POSITION_MAX_BYTES: usize =
    std::mem::size_of::<LV2AtomEvent>() + std::mem::size_of::<LV2AtomObjectBody>() + 8 * 32;

/// Write one `time:` property (key + typed scalar value) into an
/// object body at `off`, returning the next 8-byte-aligned offset.
///
/// # Safety
/// `buf + off` must have room for the property header plus `value`
/// rounded up to 8 bytes; callers guarantee this via a single
/// up-front [`TIME_POSITION_MAX_BYTES`] check.
unsafe fn write_property(buf: *mut u8, off: usize, key: LV2Urid, value_type: LV2Urid, value: &[u8]) -> usize {
    let prop = unsafe { &mut *buf.add(off).cast::<LV2AtomPropertyBody>() };
    prop.key = key;
    prop.context = 0;
    prop.value = LV2Atom {
        size: u32::try_from(value.len()).unwrap_or(0),
        mytype: value_type,
    };
    let data = unsafe { buf.add(off + std::mem::size_of::<LV2AtomPropertyBody>()) };
    unsafe { std::ptr::copy_nonoverlapping(value.as_ptr(), data, value.len()) };
    let total = std::mem::size_of::<LV2AtomPropertyBody>() + value.len();
    off + ((total + 7) & !7)
}

/// Write a `time:Position` object event (at frame 0) into the atom
/// sequence buffer starting at `base`, returning the offset just
/// past it. Returns `base` unchanged if there isn't room.
///
/// # Safety
/// `buf` must point at a buffer of at least `cap` bytes that is
/// valid for writes in `[base, cap)` and aligned for atom structs.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss
)]
unsafe fn write_time_position(
    urids: &TimeUrids,
    buf: *mut u8,
    base: usize,
    cap: usize,
    t: &TransportInfo,
) -> usize {
    if base + TIME_POSITION_MAX_BYTES > cap {
        return base;
    }
    let event_header = std::mem::size_of::<LV2AtomEvent>();
    let obj_body_off = base + event_header;

    // Object body header (id = 0 blank, otype = time:Position).
    let obj_body = unsafe { &mut *buf.add(obj_body_off).cast::<LV2AtomObjectBody>() };
    obj_body.id = 0;
    obj_body.otype = urids.position;

    let mut off = obj_body_off + std::mem::size_of::<LV2AtomObjectBody>();

    // speed: 1.0 while rolling, 0.0 when stopped.
    let speed: f32 = if t.playing { 1.0 } else { 0.0 };
    off = unsafe { write_property(buf, off, urids.speed, urids.atom_float, &speed.to_ne_bytes()) };

    if let Some(samples) = t.song_position_samples {
        off = unsafe { write_property(buf, off, urids.frame, urids.atom_long, &samples.to_ne_bytes()) };
    }
    if let Some(tempo) = t.tempo_bpm {
        let bpm = tempo as f32;
        off = unsafe { write_property(buf, off, urids.bpm, urids.atom_float, &bpm.to_ne_bytes()) };
    }
    if let Some((num, den)) = t.time_signature {
        let beats_per_bar = num as f32;
        off = unsafe {
            write_property(buf, off, urids.beats_per_bar, urids.atom_float, &beats_per_bar.to_ne_bytes())
        };
        let beat_unit = den as i32;
        off = unsafe {
            write_property(buf, off, urids.beat_unit, urids.atom_int, &beat_unit.to_ne_bytes())
        };

        // Bar / barBeat need the musical position too.
        let beats_per_bar_qn = f64::from(num) * 4.0 / f64::from(den.max(1));
        if let Some(bar_start) = t.bar_start_beats {
            let bar = (bar_start / beats_per_bar_qn.max(f64::EPSILON)).round() as i64;
            off = unsafe { write_property(buf, off, urids.bar, urids.atom_long, &bar.to_ne_bytes()) };

            if let Some(beats) = t.song_position_beats {
                // Quarter notes since the bar, expressed in this
                // signature's beats (beatUnit per whole note).
                let bar_beat = ((beats - bar_start) * f64::from(den) / 4.0) as f32;
                off = unsafe {
                    write_property(buf, off, urids.bar_beat, urids.atom_float, &bar_beat.to_ne_bytes())
                };
            }
        }
    }

    // Back-patch the event header now that the object size is known.
    let object_body_size = off - obj_body_off;
    let event = unsafe { &mut *buf.add(base).cast::<LV2AtomEvent>() };
    event.time_in_frames = 0;
    event.body = LV2Atom {
        size: object_body_size as u32,
        mytype: urids.atom_object,
    };

    // Pad the whole event to the sequence's 8-byte event alignment.
    (off + 7) & !7
}

fn audio_layout(in_count: usize, out_count: usize) -> BusLayout {
    let mut layout = BusLayout::new();
    if in_count > 0 {
        layout.inputs.push(truce_rack_core::bus::Bus::main(
            "Input",
            channel_config(in_count),
        ));
    }
    if out_count > 0 {
        layout.outputs.push(truce_rack_core::bus::Bus::main(
            "Output",
            channel_config(out_count),
        ));
    }
    layout
}

fn channel_config(n: usize) -> ChannelConfig {
    match n {
        1 => ChannelConfig::Mono,
        2 => ChannelConfig::Stereo,
        6 => ChannelConfig::Surround5_1,
        8 => ChannelConfig::Surround7_1,
        n => ChannelConfig::Discrete(u32::try_from(n).unwrap_or(0)),
    }
}

unsafe fn classify_ports(
    world: *mut lilv_sys::LilvWorld,
    plugin: *const lilv_sys::LilvPlugin,
) -> (Vec<PortInfo>, usize, usize, Vec<usize>, Vec<f32>) {
    let count = unsafe { lilv_sys::lilv_plugin_get_num_ports(plugin) };
    let audio_uri =
        unsafe { lilv_sys::lilv_new_uri(world, lilv_sys::LILV_URI_AUDIO_PORT.as_ptr().cast()) };
    let control_uri =
        unsafe { lilv_sys::lilv_new_uri(world, lilv_sys::LILV_URI_CONTROL_PORT.as_ptr().cast()) };
    let atom_uri =
        unsafe { lilv_sys::lilv_new_uri(world, lilv_sys::LILV_URI_ATOM_PORT.as_ptr().cast()) };
    let input_uri =
        unsafe { lilv_sys::lilv_new_uri(world, lilv_sys::LILV_URI_INPUT_PORT.as_ptr().cast()) };
    let output_uri =
        unsafe { lilv_sys::lilv_new_uri(world, lilv_sys::LILV_URI_OUTPUT_PORT.as_ptr().cast()) };

    let mut ports = Vec::with_capacity(count as usize);
    let mut audio_in_count: usize = 0;
    let mut audio_out_count: usize = 0;
    let mut control_value_offset = Vec::with_capacity(count as usize);
    let mut control_values = Vec::new();

    let mut defaults: Vec<f32> = vec![0.0; count as usize];
    unsafe {
        // Pull every control port's declared default in one call.
        let defaults_ptr = defaults.as_mut_ptr();
        lilv_sys::lilv_plugin_get_port_ranges_float(
            plugin,
            std::ptr::null_mut(), // mins (don't care)
            std::ptr::null_mut(), // maxes (don't care)
            defaults_ptr,
        );
    }

    for idx in 0..count {
        let port = unsafe { lilv_sys::lilv_plugin_get_port_by_index(plugin, idx) };
        if port.is_null() {
            ports.push(PortInfo {
                index: idx,
                kind: PortKind::Other,
                is_input: false,
            });
            control_value_offset.push(usize::MAX);
            continue;
        }
        let is_input = unsafe { lilv_sys::lilv_port_is_a(plugin, port, input_uri) };
        let is_output = unsafe { lilv_sys::lilv_port_is_a(plugin, port, output_uri) };
        let kind = if unsafe { lilv_sys::lilv_port_is_a(plugin, port, audio_uri) } {
            if is_input {
                audio_in_count += 1;
            } else if is_output {
                audio_out_count += 1;
            }
            PortKind::Audio
        } else if unsafe { lilv_sys::lilv_port_is_a(plugin, port, control_uri) } {
            let default = if defaults[idx as usize].is_finite() {
                defaults[idx as usize]
            } else {
                0.0
            };
            PortKind::Control { default }
        } else if unsafe { lilv_sys::lilv_port_is_a(plugin, port, atom_uri) } {
            // We don't currently introspect the port's
            // `atom:supports` triples; assume any atom-port can
            // carry MIDI. Plugins with non-MIDI atom expectations
            // will see an unrecognised event type and ignore it,
            // which is the LV2 spec's required behaviour.
            PortKind::AtomSequence
        } else {
            PortKind::Other
        };
        if let PortKind::Control { default } = kind {
            control_value_offset.push(control_values.len());
            control_values.push(default);
        } else {
            control_value_offset.push(usize::MAX);
        }
        ports.push(PortInfo {
            index: idx,
            kind,
            is_input,
        });
    }

    unsafe {
        lilv_sys::lilv_node_free(audio_uri);
        lilv_sys::lilv_node_free(control_uri);
        lilv_sys::lilv_node_free(atom_uri);
        lilv_sys::lilv_node_free(input_uri);
        lilv_sys::lilv_node_free(output_uri);
    }

    (
        ports,
        audio_in_count,
        audio_out_count,
        control_value_offset,
        control_values,
    )
}

/// Encode one truce-rack MIDI event into the (status, d1, d2, len)
/// triple LV2 atom MIDI events expect. Returns `None` for
/// non-MIDI events or unsupported MIDI variants.
fn midi_bytes(body: &EventBody) -> Option<(u8, u8, u8, usize)> {
    match body {
        EventBody::Midi(MidiData::NoteOn {
            channel,
            note,
            velocity,
        }) => Some((0x90 | (channel & 0x0F), note & 0x7F, velocity & 0x7F, 3)),
        EventBody::Midi(MidiData::NoteOff {
            channel,
            note,
            velocity,
        }) => Some((0x80 | (channel & 0x0F), note & 0x7F, velocity & 0x7F, 3)),
        EventBody::Midi(MidiData::PolyAftertouch {
            channel,
            note,
            pressure,
        }) => Some((0xA0 | (channel & 0x0F), note & 0x7F, pressure & 0x7F, 3)),
        EventBody::Midi(MidiData::ControlChange {
            channel,
            controller,
            value,
        }) => Some((0xB0 | (channel & 0x0F), controller & 0x7F, value & 0x7F, 3)),
        EventBody::Midi(MidiData::ProgramChange { channel, program }) => {
            Some((0xC0 | (channel & 0x0F), program & 0x7F, 0, 2))
        }
        EventBody::Midi(MidiData::ChannelAftertouch { channel, pressure }) => {
            Some((0xD0 | (channel & 0x0F), pressure & 0x7F, 0, 2))
        }
        EventBody::Midi(MidiData::PitchBend { channel, value }) => Some((
            0xE0 | (channel & 0x0F),
            (value & 0x7F) as u8,
            ((value >> 7) & 0x7F) as u8,
            3,
        )),
        EventBody::Midi(MidiData::Raw { len, data }) if *len >= 1 && *len <= 3 => Some((
            data[0],
            *data.get(1).unwrap_or(&0),
            *data.get(2).unwrap_or(&0),
            *len as usize,
        )),
        _ => None,
    }
}

/// Suppress unused-import warning for `Event`.
#[allow(dead_code)]
fn _event_marker(_: &Event) {}

impl Drop for Lv2Plugin {
    fn drop(&mut self) {
        // Tear down the editor first so the UI's `cleanup` runs
        // while the audio instance is still alive (some UIs talk
        // to the instance via the `instance-access` extension).
        self.close_editor();
        if !self.instance.is_null() {
            unsafe {
                lilv_sys::lilv_instance_deactivate(self.instance);
                lilv_sys::lilv_instance_free(self.instance);
            }
            self.instance = std::ptr::null_mut();
        }
        // `world` and `features` drop in struct order after this.
    }
}

impl Lv2Plugin {
    /// Free the live UI state (descriptor cleanup + libloading
    /// drop). Safe to call when no editor is open.
    fn close_editor(&mut self) {
        if let Some(state) = self.editor.take() {
            unsafe {
                ((*state.descriptor).cleanup)(state.handle);
            }
            // Library drops here, which closes the dlopen handle.
            drop(state);
        }
    }
}

impl PluginCore for Lv2Plugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn active_layout(&self) -> Option<&BusLayout> {
        self.active_layout.as_ref()
    }
    fn supported_layouts(&self) -> &[BusLayout] {
        &self.layouts
    }
    fn parameter_count(&self) -> usize {
        // Control ports are LV2's parameter analogue, but exposing
        // them through the param API requires names + ranges, which
        // we don't yet read. Hosts can still read /set the value via
        // the trait once we add ParameterInfo enumeration.
        0
    }
    fn parameter_info(&self, index: usize) -> Result<ParameterInfo> {
        Err(Error::InvalidParameter(index))
    }
    fn parameter_value(&self, index: usize) -> Result<f64> {
        Err(Error::InvalidParameter(index))
    }
    fn parameter_value_string(&self, index: usize, _value: f64) -> Result<String> {
        Err(Error::InvalidParameter(index))
    }
    fn set_parameter(&mut self, index: usize, _value: f64) -> Result<()> {
        Err(Error::InvalidParameter(index))
    }
    fn preset_count(&self) -> usize {
        0
    }
    fn preset_info(&self, index: usize) -> Result<PresetInfo> {
        Err(Error::InvalidParameter(index))
    }
    fn load_preset(&mut self, _preset_number: i32) -> Result<()> {
        Err(Error::Other("lv2 preset loading not yet wired".into()))
    }
    fn save_state(&self) -> Result<Vec<u8>> {
        Err(Error::Other("lv2 state save not yet wired".into()))
    }
    fn load_state(&mut self, _bytes: &[u8]) -> Result<()> {
        Err(Error::Other("lv2 state load not yet wired".into()))
    }

    fn activate(
        &mut self,
        layout: BusLayout,
        sample_rate: f64,
        max_block_size: usize,
    ) -> Result<()> {
        // (Re)instantiate if this is the first activate or the
        // sample rate has changed — LV2 bakes the rate into the
        // instance at `lilv_plugin_instantiate` time.
        let needs_reinstantiate =
            self.instance.is_null() || (self.sample_rate - sample_rate).abs() > f64::EPSILON;
        if needs_reinstantiate {
            if !self.instance.is_null() {
                unsafe {
                    lilv_sys::lilv_instance_deactivate(self.instance);
                    lilv_sys::lilv_instance_free(self.instance);
                }
                self.instance = std::ptr::null_mut();
            }
            let inst = unsafe {
                lilv_sys::lilv_plugin_instantiate(
                    self.plugin,
                    sample_rate,
                    self.features.feature_ptrs.as_ptr(),
                )
            };
            if inst.is_null() {
                return Err(Error::Other("lilv_plugin_instantiate returned NULL".into()));
            }
            self.instance = inst;
            self.sample_rate = sample_rate;
        }

        // Rough sizing: room for a sequence header plus one max-len
        // MIDI event per frame. Plenty for any sane block.
        let event_slot = std::mem::size_of::<LV2AtomEvent>() + 8;
        let cap = std::mem::size_of::<LV2AtomSequence>()
            + TIME_POSITION_MAX_BYTES
            + max_block_size * event_slot;
        self.midi_in_buf.resize(cap.max(64), 0);
        self.midi_out_buf.resize(cap.max(64), 0);

        unsafe { lilv_sys::lilv_instance_activate(self.instance) };
        self.active_layout = Some(layout);
        Ok(())
    }
    fn deactivate(&mut self) {
        if !self.instance.is_null() {
            unsafe { lilv_sys::lilv_instance_deactivate(self.instance) };
        }
        self.active_layout = None;
    }
    fn is_active(&self) -> bool {
        self.active_layout.is_some()
    }

    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        if self.ui_bundle.is_some() {
            Some(self)
        } else {
            None
        }
    }
}

impl PluginEditor for Lv2Plugin {
    // `type DescFn = …` lives in the function body next to its single
    // call site so the LV2 UI signature is one read away — hoisting it
    // would orphan the comment from the use.
    #[allow(clippy::items_after_statements)]
    fn open(&mut self, parent: WindowHandle, _scale: f64) -> Result<()> {
        let bundle = self
            .ui_bundle
            .as_ref()
            .ok_or_else(|| Error::Other("lv2 plugin has no UI".into()))?
            .clone();
        if self.editor.is_some() {
            return Ok(());
        }

        let parent_ptr: *mut c_void = match parent {
            WindowHandle::NSView(p) | WindowHandle::HWND(p) => p,
            // X11 hands us a u64 XID; LV2 X11UI expects it cast
            // to a pointer (lv2 spec: "an X11 Window with the
            // proper visual"). usize cast is identity on 64-bit
            // and zero-extends on 32-bit, both fine.
            WindowHandle::X11(xid) => xid as usize as *mut c_void,
        };

        // dlopen the UI binary. libloading retains an OS handle
        // we keep in EditorState for the lifetime of the editor.
        let lib = unsafe { libloading::Library::new(&bundle.binary_path) }.map_err(|e| {
            Error::Other(format!(
                "lv2 ui dlopen {}: {}",
                bundle.binary_path.display(),
                e
            ))
        })?;
        type DescFn = unsafe extern "C" fn(u32) -> *const LV2UIDescriptorRaw;
        let descriptor = unsafe {
            let sym: libloading::Symbol<DescFn> = lib
                .get(b"lv2ui_descriptor")
                .map_err(|e| Error::Other(format!("lv2 ui_descriptor symbol: {e}")))?;
            // Walk indices until we find the descriptor whose URI
            // matches our chosen UI's URI, or the function returns
            // NULL.
            let mut chosen: *const LV2UIDescriptorRaw = std::ptr::null();
            for idx in 0u32..64 {
                let d = sym(idx);
                if d.is_null() {
                    break;
                }
                let uri = (*d).uri;
                if !uri.is_null() && CStr::from_ptr(uri) == bundle.ui_uri.as_c_str() {
                    chosen = d;
                    break;
                }
            }
            chosen
        };
        if descriptor.is_null() {
            return Err(Error::Other(format!(
                "no lv2ui_descriptor matching {:?}",
                bundle.ui_uri
            )));
        }

        // Build the UI feature list. URID map shares the same
        // backing UridMap as the audio side so URIs interned by
        // either path stay consistent.
        let map_handle: *mut UridMap = (&mut self.features.urid_map) as *mut UridMap;
        let ui_features = UiFeatureStorage::new(map_handle, parent_ptr);

        // Heap-stable controller for write_function callbacks. Built
        // lazily because `control_values` only stops moving after
        // load completes.
        self.ensure_controller();
        let controller_ptr: *const Controller = self
            .controller
            .as_deref()
            .map_or(std::ptr::null(), |c| c as *const Controller);

        let plugin_uri = CString::new(self.info.unique_id.clone())
            .map_err(|_| Error::Other("lv2 plugin uri contains NUL".into()))?;
        let bundle_path_c = path_to_cstring_with_trailing_sep(&bundle.bundle_path)?;

        let mut widget: LV2UIWidget = std::ptr::null_mut();
        let handle = unsafe {
            ((*descriptor).instantiate_raw)(
                descriptor,
                plugin_uri.as_ptr(),
                bundle_path_c.as_ptr(),
                Some(ui_write_callback),
                controller_ptr.cast::<c_void>(),
                &raw mut widget,
                ui_features.feature_ptrs.as_ptr(),
            )
        };
        if handle.is_null() {
            return Err(Error::Other("lv2 ui instantiate returned NULL".into()));
        }

        // For embedded UI types (CocoaUI / WindowsUI / X11UI) the
        // "widget" is the toolkit-native parent-of-the-UI handle
        // (NSView / HWND / X11 Window). The host's parent already
        // contains it as a child after instantiate; we just keep
        // the pointer for size queries.

        // Look up the optional idle / resize extension interfaces
        // the UI may have published. `extension_data` returns a
        // pointer to a shared static struct of function pointers.
        let idle_iface = unsafe {
            if let Some(ext) = (*descriptor).extension_data {
                ext(LV2_UI_IDLE_INTERFACE_URI.as_ptr().cast::<c_char>())
                    as *const lv2_raw::ui::LV2UIIdleInterface
            } else {
                std::ptr::null()
            }
        };
        let resize_iface = unsafe {
            if let Some(ext) = (*descriptor).extension_data {
                ext(LV2_UI_RESIZE_INTERFACE_URI.as_ptr().cast::<c_char>())
                    as *const Lv2UiResizeInterface
            } else {
                std::ptr::null()
            }
        };

        let last_pushed_values = self.control_values.clone();

        self.editor = Some(EditorState {
            _library: lib,
            descriptor,
            handle,
            widget,
            ui_features,
            idle_iface,
            resize_iface,
            last_pushed_values,
        });
        Ok(())
    }

    fn close(&mut self) {
        self.close_editor();
    }

    fn is_open(&self) -> bool {
        self.editor.is_some()
    }

    fn size(&self) -> Option<(u32, u32)> {
        let state = self.editor.as_ref()?;
        native_widget_size(state.widget)
    }

    fn is_resizable(&self) -> bool {
        // Resizable iff the UI exported a `ui:resize` extension —
        // that's the only way for the host to push a new size.
        self.editor
            .as_ref()
            .is_some_and(|e| !e.resize_iface.is_null())
    }

    fn set_size(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let state = self.editor.as_ref()?;
        if state.resize_iface.is_null() {
            return None;
        }
        // SAFETY: `resize_iface` was returned by the UI's
        // `extension_data(ui:resize)` and is a static struct of
        // function pointers owned by the UI bundle (still loaded
        // because `EditorState._library` is alive).
        let r = unsafe {
            ((*state.resize_iface).ui_resize)(
                (*state.resize_iface).handle,
                i32::try_from(width).unwrap_or(i32::MAX),
                i32::try_from(height).unwrap_or(i32::MAX),
            )
        };
        if r == 0 { Some((width, height)) } else { None }
    }

    fn show(&mut self) {
        // Embedded LV2 UIs are visible immediately after
        // instantiate; nothing to do.
    }

    fn hide(&mut self) {
        // No-op for embedded UIs — closing the editor or the host
        // window is the visibility primitive.
    }

    fn on_idle(&mut self) {
        let Some(state) = self.editor.as_mut() else {
            return;
        };
        // 1. Drive the optional `ui:idleInterface` first — animations
        //    and the UI's own event pump live here. Non-zero return
        //    means the UI closed itself; we bail and let the next
        //    on_idle skip cleanly.
        if !state.idle_iface.is_null() {
            let rc = unsafe { ((*state.idle_iface).idle)(state.handle) };
            if rc != 0 {
                // UI asked to be torn down. close_editor walks the
                // descriptor's cleanup and drops the library.
                self.close_editor();
                return;
            }
        }

        // 2. Push host-side parameter changes to the UI via
        //    `port_event`. Compare current control_values against
        //    the snapshot we took last tick; for any port whose value
        //    differs, fire a float-protocol port_event so the UI
        //    redraws. Snapshot is then refreshed.
        let Some(state) = self.editor.as_mut() else {
            return;
        };
        // SAFETY: `descriptor` is the same one we instantiated; its
        // `port_event` field is non-null per the LV2 spec.
        let port_event_fn = unsafe { (*state.descriptor).port_event };
        for (port_index, &offset) in self.control_value_offset.iter().enumerate() {
            if offset == usize::MAX || offset >= self.control_values.len() {
                continue;
            }
            let cur = self.control_values[offset];
            // First idle tick after a re-open may have a shorter
            // snapshot than control_values — guard via .get.
            let prev = state
                .last_pushed_values
                .get(offset)
                .copied()
                .unwrap_or(cur + 1.0);
            // f32 inequality is fine here — we only push when the
            // bit pattern actually changed, NaNs included (NaN != NaN
            // is the right answer; both sides will have NaN if the
            // plugin keeps writing NaN, no spurious push).
            #[allow(clippy::float_cmp)]
            let changed = cur != prev;
            if changed {
                let value = cur;
                let port_index_u32 = u32::try_from(port_index).unwrap_or(u32::MAX);
                port_event_fn(
                    state.handle,
                    port_index_u32,
                    4,
                    0, // float protocol
                    (&raw const value).cast::<c_void>(),
                );
                if offset < state.last_pushed_values.len() {
                    state.last_pushed_values[offset] = cur;
                }
            }
        }
        // suppress unused-warning if no port matched
        let _ = port_event_fn;

        // 3. Apply any UI-requested resize. The host-side ui:resize
        //    callback writes into the notifier; on_idle is the host's
        //    chance to act on it. We can't actually resize the
        //    baseview window from inside this trait method (no window
        //    handle), so we just stash the request — windowed.rs polls
        //    `size()` next frame and resizes accordingly.
        // Currently no-op past the notifier write; future host-driven
        // window resize would consume `notifier.pending.take()` here.
    }
}

fn path_to_cstring_with_trailing_sep(p: &Path) -> Result<CString> {
    let mut s = p.to_string_lossy().into_owned();
    if !s.ends_with(std::path::MAIN_SEPARATOR) {
        s.push(std::path::MAIN_SEPARATOR);
    }
    CString::new(s).map_err(|_| Error::Other("lv2 ui bundle path contains NUL".into()))
}

#[cfg(target_os = "macos")]
fn native_widget_size(widget: LV2UIWidget) -> Option<(u32, u32)> {
    use objc2::msg_send;
    use objc2_foundation::NSRect;
    if widget.is_null() {
        return None;
    }
    // SAFETY: For CocoaUI the widget is an NSView*. Reading
    // -frame is always safe on a live NSView; the LV2 spec
    // promises the widget stays valid until cleanup.
    let view = widget as *mut objc2::runtime::AnyObject;
    let frame: NSRect = unsafe { msg_send![view, frame] };
    // `.max(0.0)` clamps the negative branch the sign-loss lint
    // worries about; window dimensions can't reasonably overflow u32.
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    Some((
        frame.size.width.max(0.0) as u32,
        frame.size.height.max(0.0) as u32,
    ))
}

#[cfg(not(target_os = "macos"))]
fn native_widget_size(_widget: LV2UIWidget) -> Option<(u32, u32)> {
    // X11 / Win32 paths would query XGetWindowAttributes /
    // GetClientRect respectively; truce-rack-standalone falls back to
    // its INITIAL_WINDOW until that's wired.
    None
}

impl Plugin<f32> for Lv2Plugin {
    fn process(
        &mut self,
        buffer: &mut AudioBuffer<'_, f32>,
        events: &EventList,
        context: &mut ProcessContext<'_>,
    ) -> Result<ProcessStatus> {
        if !self.is_active() {
            return Err(Error::NotActivated);
        }
        let frames = buffer.num_frames();

        // Build the input atom sequence (host transport + MIDI) and
        // reset the output.
        self.build_midi_sequence(events, context.transport);
        self.prep_midi_out();

        // Take raw pointers for every audio plane up front so the
        // immutable + mutable borrows on `buffer` don't overlap
        // when we iterate ports below.
        let in_ptrs: Vec<*mut f32> = buffer
            .main_inputs()
            .iter()
            .map(|s| s.as_ptr().cast_mut())
            .collect();
        let out_ptrs: Vec<*mut f32> = buffer
            .main_outputs()
            .iter_mut()
            .map(|s| s.as_mut_ptr())
            .collect();
        let control_base = self.control_values.as_mut_ptr();
        let midi_in_ptr = self.midi_in_buf.as_mut_ptr().cast::<c_void>();
        let midi_out_ptr = self.midi_out_buf.as_mut_ptr().cast::<c_void>();

        // Connect each port. Audio + atom port directions are already
        // classified at load; we just count audio ports as we go to
        // pick the right channel of the host buffer.
        let mut next_audio_in = 0usize;
        let mut next_audio_out = 0usize;
        for port in &self.ports {
            let data: *mut c_void = match port.kind {
                PortKind::Audio => {
                    if port.is_input {
                        let plane = in_ptrs
                            .get(next_audio_in)
                            .copied()
                            .unwrap_or(std::ptr::null_mut());
                        next_audio_in += 1;
                        plane.cast::<c_void>()
                    } else {
                        let plane = out_ptrs
                            .get(next_audio_out)
                            .copied()
                            .unwrap_or(std::ptr::null_mut());
                        next_audio_out += 1;
                        plane.cast::<c_void>()
                    }
                }
                PortKind::Control { .. } => {
                    let off = self.control_value_offset[port.index as usize];
                    if off == usize::MAX {
                        std::ptr::null_mut()
                    } else {
                        // SAFETY: `control_base` is the head of the
                        // `control_values` Vec; `off` was computed
                        // from its push order at load time.
                        unsafe { control_base.add(off).cast::<c_void>() }
                    }
                }
                PortKind::AtomSequence => {
                    if port.is_input {
                        midi_in_ptr
                    } else {
                        midi_out_ptr
                    }
                }
                PortKind::Other => std::ptr::null_mut(),
            };
            unsafe {
                lilv_sys::lilv_instance_connect_port(self.instance, port.index, data);
            }
        }

        unsafe {
            lilv_sys::lilv_instance_run(self.instance, u32::try_from(frames).unwrap_or(u32::MAX));
        }

        // Drain MIDI from the output atom-sequence port (if the
        // plugin connected one) into context.output_events.
        self.drain_midi_out(context);

        Ok(ProcessStatus::Continue)
    }
}

impl Lv2Plugin {
    /// Walk the MIDI output atom-sequence buffer the plugin just
    /// wrote and translate every `MidiEvent`-typed event back into
    /// rack2-core `EventList` events on `context.output_events`.
    fn drain_midi_out(&mut self, context: &mut ProcessContext<'_>) {
        let header_size = std::mem::size_of::<LV2AtomSequence>();
        if self.midi_out_buf.len() < header_size {
            return;
        }
        let buf = self.midi_out_buf.as_ptr();
        // SAFETY: midi_out_buf is sized for an LV2AtomSequence header
        // plus events at activate. Vec<u8> is heap-aligned to >= 8.
        let seq = unsafe { &*buf.cast::<LV2AtomSequence>() };
        // Sequence body size (atom.size) excludes the LV2Atom header
        // itself; the body's events run from sequence_begin to
        // sequence_end.
        let body_size = seq.atom.size;
        let event_header = std::mem::size_of::<LV2AtomEvent>();
        let body_offset = std::mem::size_of::<LV2Atom>();
        let mut cursor = body_offset + std::mem::size_of::<LV2AtomSequenceBody>();
        let body_end = body_offset + body_size as usize;
        while cursor + event_header <= body_end && cursor + event_header <= self.midi_out_buf.len()
        {
            let ev_ptr = unsafe { buf.add(cursor) }.cast::<LV2AtomEvent>();
            // SAFETY: cursor + event_header <= midi_out_buf.len().
            let ev = unsafe { &*ev_ptr };
            let payload_size = ev.body.size as usize;
            let payload_total = event_header + payload_size;
            if cursor + payload_total > body_end {
                break;
            }
            if ev.body.mytype == self.midi_event_urid && (1..=8).contains(&payload_size) {
                let data_ptr = unsafe { (ev_ptr as *const u8).add(event_header) };
                // SAFETY: payload_size bytes immediately follow the
                // event header within the bounds we just checked.
                let bytes = unsafe { std::slice::from_raw_parts(data_ptr, payload_size) };
                if let Some(body) = decode_midi_atom(bytes) {
                    let offset = u32::try_from(ev.time_in_frames.max(0)).unwrap_or(0);
                    context.output_events.push(Event {
                        sample_offset: offset,
                        body,
                    });
                }
            }
            // Advance cursor past this event, padded to 8 bytes per
            // the atom alignment rule.
            cursor += (payload_total + 7) & !7;
        }
    }
}

/// Inverse of `midi_bytes` — turn 1-3 bytes of LV2 MIDI atom payload
/// into a typed `EventBody`. Anything longer (sysex etc.) is wrapped
/// in `MidiData::Raw` up to the 8-byte cap.
fn decode_midi_atom(bytes: &[u8]) -> Option<EventBody> {
    if bytes.is_empty() {
        return None;
    }
    let status = bytes[0];
    let channel = status & 0x0F;
    let kind = status & 0xF0;
    let body = match (kind, bytes) {
        (0x80, [_, note, vel]) => MidiData::NoteOff {
            channel,
            note: *note,
            velocity: *vel,
        },
        (0x90, [_, note, 0]) => MidiData::NoteOff {
            channel,
            note: *note,
            velocity: 0,
        },
        (0x90, [_, note, vel]) => MidiData::NoteOn {
            channel,
            note: *note,
            velocity: *vel,
        },
        (0xA0, [_, note, pressure]) => MidiData::PolyAftertouch {
            channel,
            note: *note,
            pressure: *pressure,
        },
        (0xB0, [_, controller, value]) => MidiData::ControlChange {
            channel,
            controller: *controller,
            value: *value,
        },
        (0xC0, [_, program]) => MidiData::ProgramChange {
            channel,
            program: *program,
        },
        (0xD0, [_, pressure]) => MidiData::ChannelAftertouch {
            channel,
            pressure: *pressure,
        },
        (0xE0, [_, lsb, msb]) => MidiData::PitchBend {
            channel,
            value: u16::from(*msb) << 7 | u16::from(*lsb),
        },
        _ if bytes.len() <= 8 => {
            let mut data = [0u8; 8];
            data[..bytes.len()].copy_from_slice(bytes);
            #[allow(clippy::cast_possible_truncation)]
            MidiData::Raw {
                len: bytes.len() as u8,
                data,
            }
        }
        _ => return None,
    };
    Some(EventBody::Midi(body))
}
