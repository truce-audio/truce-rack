//! VST3 host implementation for the truce-rack framework.
//!
//! Built on the community `vst3` Rust bindings — no Steinberg SDK
//! submodule, no cmake. A fresh checkout builds in seconds.
//!
//! # Lifecycle
//!
//! Each `.vst3` bundle exports `GetPluginFactory` (and on macOS
//! `bundleEntry` / `bundleExit`). Loading walks:
//!
//! 1. `dlopen` the binary inside the bundle.
//! 2. `bundleEntry()` on macOS (no-op elsewhere).
//! 3. `GetPluginFactory()` → `IPluginFactory`.
//! 4. `createInstance(cid, IComponent::IID)` → `IComponent`.
//! 5. `IPluginBase::initialize(host_context)` on the component.
//! 6. Cast to `IAudioProcessor`.
//! 7. `IComponent::getControllerClassId(&mut cid2)` — if separate,
//!    `factory.createInstance(cid2, IEditController::IID)` and
//!    initialize it. Otherwise cast the component itself.
//! 8. Connect the component and controller `IConnectionPoint`s
//!    (separate-controller case only).
//!
//! Activate calls `setBusArrangements`, `setupProcessing`,
//! `setActive(true)`, `setProcessing(true)`. Deactivate reverses.

use truce_rack_core::buffer::AudioBuffer;
use truce_rack_core::bus::{Bus, BusKind, BusLayout, ChannelConfig};
use truce_rack_core::error::{Error, Result};
use truce_rack_core::events::{Event as RackEvent, EventBody, EventList, MidiData};
use truce_rack_core::info::{ParameterInfo, PluginCategory, PluginInfo, PresetInfo};
use truce_rack_core::plugin::{Plugin, PluginCore, ProcessContext, ProcessStatus};
use truce_rack_core::scanner::PluginScanner;
use truce_rack_core::transport::TransportInfo;
use truce_rack_core::wrapper::run_audio_block_with;

use std::ptr;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
};

use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0, BusDirections_, BusInfo, BusTypes_, ControllerNumbers,
    ControllerNumbers_, CtrlNumber, Event, Event_::EventTypes_, Event__type0, IAudioProcessor,
    IAudioProcessorTrait, IComponent, IComponentHandler, IComponentHandlerTrait, IComponentTrait,
    IConnectionPoint, IConnectionPointTrait, IEditController, IEditControllerTrait, IEventList,
    IEventListTrait, IHostApplication, IHostApplicationTrait, IMidiMapping, IMidiMappingTrait,
    IParamValueQueue, IParamValueQueueTrait, IParameterChanges, IParameterChangesTrait,
    MediaTypes_, NoteOffEvent, NoteOnEvent, ParamID, ParamValue,
    ParameterInfo as Vst3ParameterInfo, ParameterInfo_::ParameterFlags_, PolyPressureEvent,
    ProcessContext as Vst3ProcessContext, ProcessContext_::StatesAndFlags_, ProcessData,
    ProcessModes_, ProcessSetup, RestartFlags_, String128, SymbolicSampleSizes_, ViewType,
};
use vst3::Steinberg::{
    FUnknown, IBStream, IBStreamTrait, IPlugView, IPlugViewTrait, IPluginBaseTrait, IPluginFactory,
    IPluginFactoryTrait, PClassInfo, PClassInfo_, TUID, ViewRect, kNotImplemented,
    kPlatformTypeHWND, kPlatformTypeNSView, kPlatformTypeX11EmbedWindowID, kResultOk, kResultTrue,
};
use vst3::{Class, ComPtr, ComWrapper};

/// Format identifier used on returned [`PluginInfo`].
pub const FORMAT: &str = "vst3";

/// Bundle directory suffix every VST3 plugin uses.
pub const VST3_EXTENSION: &str = ".vst3";

/// Symbol name `GetPluginFactory` plugins export. Same on every OS.
const GET_FACTORY_SYMBOL: &[u8] = b"GetPluginFactory\0";

/// macOS-only bundle entry point.
#[cfg(target_os = "macos")]
const BUNDLE_ENTRY_SYMBOL: &[u8] = b"bundleEntry\0";

/// macOS-only bundle exit point.
#[cfg(target_os = "macos")]
const BUNDLE_EXIT_SYMBOL: &[u8] = b"bundleExit\0";

const CTRL_AFTERTOUCH: u8 = 128;
const CTRL_PITCH_BEND: u8 = 129;
const CTRL_PROGRAM_CHANGE: u8 = 130;

/// VST3 scanner.
#[derive(Debug, Default)]
pub struct Vst3Scanner;

impl Vst3Scanner {
    /// Construct a default scanner.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl PluginScanner for Vst3Scanner {
    type Plugin = Vst3Plugin;

    fn scan(&self) -> Result<Vec<PluginInfo>> {
        let mut out = Vec::new();
        for dir in default_vst3_paths() {
            if dir.exists() {
                scan_dir_into(&dir, &mut out);
            }
        }
        Ok(out)
    }

    fn scan_path(&self, path: &Path) -> Result<Vec<PluginInfo>> {
        let mut out = Vec::new();
        if path.exists() {
            if is_vst3_bundle_path(path) {
                scan_bundle_into(path, &mut out)?;
            } else {
                scan_dir_into(path, &mut out);
            }
        }
        Ok(out)
    }

    fn load(&self, info: &PluginInfo) -> Result<Self::Plugin> {
        Vst3Plugin::load_from(info)
    }
}

/// Standard VST3 install locations for the current OS.
#[must_use]
pub fn default_vst3_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let mut user = PathBuf::from(home);
        #[cfg(target_os = "macos")]
        user.push("Library/Audio/Plug-Ins/VST3");
        #[cfg(target_os = "linux")]
        user.push(".vst3");
        #[cfg(target_os = "windows")]
        user.push("AppData/Local/Programs/Common/VST3");
        out.push(user);
    }
    #[cfg(target_os = "macos")]
    out.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
    #[cfg(target_os = "linux")]
    out.push(PathBuf::from("/usr/lib/vst3"));
    #[cfg(target_os = "windows")]
    {
        if let Some(pf) = std::env::var_os("CommonProgramFiles") {
            let mut p = PathBuf::from(pf);
            p.push("VST3");
            out.push(p);
        }
    }
    out
}

fn scan_dir_into(dir: &Path, out: &mut Vec<PluginInfo>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if !name.ends_with(VST3_EXTENSION) {
            continue;
        }
        if let Err(err) = scan_bundle_into(&path, out) {
            eprintln!("[truce-rack-vst3] skipping {}: {err}", path.display());
        }
    }
}

fn is_vst3_bundle_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(VST3_EXTENSION))
}

fn scan_bundle_into(bundle: &Path, out: &mut Vec<PluginInfo>) -> Result<()> {
    let module = unsafe { LoadedModule::open(bundle) }?;
    let factory = module.factory()?;
    let count = unsafe { factory.countClasses() };
    let mut info = empty_pclass_info();
    for idx in 0..count {
        if unsafe { factory.getClassInfo(idx, &raw mut info) } != kResultOk {
            continue;
        }
        let category = char8_array_to_string(&info.category);
        if category != "Audio Module Class" {
            continue;
        }
        let name = char8_array_to_string(&info.name);
        out.push(PluginInfo {
            name,
            vendor: String::new(),
            version: 0,
            category: PluginCategory::Effect,
            path: bundle.to_path_buf(),
            unique_id: tuid_to_hex(&info.cid),
            format: FORMAT,
            has_editor: false,
            accepts_midi: false,
        });
    }
    Ok(())
}

fn empty_pclass_info() -> PClassInfo {
    PClassInfo {
        cid: [0; 16],
        cardinality: 0,
        category: [0; PClassInfo_::kCategorySize as usize],
        name: [0; PClassInfo_::kNameSize as usize],
    }
}

#[allow(clippy::cast_sign_loss)]
fn char8_array_to_string(array: &[i8]) -> String {
    // VST3 char8 is signed on Apple, unsigned elsewhere; the cast
    // preserves bit pattern.
    let bytes: Vec<u8> = array
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[allow(clippy::cast_sign_loss)]
fn tuid_to_hex(cid: &[i8; 16]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(32);
    for &b in cid {
        let _ = write!(s, "{:02x}", b as u8);
    }
    s
}

#[allow(clippy::cast_possible_wrap, clippy::unnecessary_cast)]
fn hex_to_tuid(hex: &str) -> Option<TUID> {
    if hex.len() != 32 {
        return None;
    }
    let mut out: TUID = [0; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()? as i8;
    }
    Some(out)
}

#[cfg(target_os = "macos")]
mod mac {
    //! `CFBundle`-backed loader for VST3 bundles on macOS.
    //!
    //! Plugins like Surge XT Effects call into `CFPlugin` during
    //! `bundleEntry` (specifically `CFPlugInRegisterFactories`,
    //! which lives behind the `AddInstanceForFactory` log line).
    //! Those APIs only work when the dylib was loaded through a
    //! registered `CFBundle` — raw `dlopen` leaves the bundle
    //! unknown to CoreFoundation and the plugin dereferences
    //! garbage. Going through `CFBundleLoadExecutable` gives the
    //! plugin the context it expects.

    use std::path::{Path, PathBuf};

    use core_foundation::base::TCFType;
    use core_foundation::bundle::CFBundle;
    use core_foundation::string::CFString;
    use core_foundation::url::{CFURL, kCFURLPOSIXPathStyle};

    use truce_rack_core::error::{Error, Result};

    pub(super) struct MacBundle {
        bundle: CFBundle,
        path: PathBuf,
    }

    impl MacBundle {
        pub(super) fn open(bundle_path: &Path) -> Result<Self> {
            let path_str = bundle_path.to_str().ok_or_else(|| Error::LoadFailed {
                path: bundle_path.to_path_buf(),
                reason: "bundle path is not valid UTF-8".into(),
            })?;
            let cf_path = CFString::new(path_str);
            // CFURLCreateWithFileSystemPath with `isDirectory = true`
            // is what every reference VST3 host uses on macOS — a
            // bundle URL has to be flagged as a directory or
            // CFBundleCreate silently picks the wrong layout.
            let url = unsafe {
                use core_foundation_sys::base::kCFAllocatorDefault;
                use core_foundation_sys::url::CFURLCreateWithFileSystemPath;
                let raw = CFURLCreateWithFileSystemPath(
                    kCFAllocatorDefault,
                    cf_path.as_concrete_TypeRef(),
                    kCFURLPOSIXPathStyle,
                    1,
                );
                if raw.is_null() {
                    return Err(Error::LoadFailed {
                        path: bundle_path.to_path_buf(),
                        reason: "CFURLCreateWithFileSystemPath returned NULL".into(),
                    });
                }
                CFURL::wrap_under_create_rule(raw)
            };

            let bundle = CFBundle::new(url).ok_or_else(|| Error::LoadFailed {
                path: bundle_path.to_path_buf(),
                reason: "CFBundleCreate returned NULL".into(),
            })?;

            // CFBundleLoadExecutable must run before any function
            // pointer lookup. Returns true on success.
            let loaded = unsafe {
                use core_foundation_sys::bundle::CFBundleLoadExecutable;
                CFBundleLoadExecutable(bundle.as_concrete_TypeRef()) != 0
            };
            if !loaded {
                return Err(Error::LoadFailed {
                    path: bundle_path.to_path_buf(),
                    reason: "CFBundleLoadExecutable failed".into(),
                });
            }

            Ok(Self {
                bundle,
                path: bundle_path.to_path_buf(),
            })
        }

        pub(super) fn path(&self) -> &Path {
            &self.path
        }

        /// The raw `CFBundleRef`, type-erased to `*mut c_void` so
        /// the call site doesn't need to depend on
        /// `core-foundation-sys`. Hand this to `bundleEntry` —
        /// it's the host's identity to the plugin.
        pub(super) fn raw(&self) -> *mut std::ffi::c_void {
            self.bundle.as_concrete_TypeRef().cast::<std::ffi::c_void>()
        }

        /// Look up an exported symbol. `name` may end in a trailing
        /// NUL (we strip it before handing the text to `CFString`).
        pub(super) unsafe fn function_ptr(&self, name: &[u8]) -> Option<*mut std::ffi::c_void> {
            let name = match name.split_last() {
                Some((0, rest)) => rest,
                _ => name,
            };
            let name_str = std::str::from_utf8(name).ok()?;
            let cf_name = CFString::new(name_str);
            let ptr = unsafe {
                use core_foundation_sys::bundle::CFBundleGetFunctionPointerForName;
                CFBundleGetFunctionPointerForName(
                    self.bundle.as_concrete_TypeRef(),
                    cf_name.as_concrete_TypeRef(),
                )
            };
            if ptr.is_null() {
                None
            } else {
                Some(ptr.cast_mut().cast::<std::ffi::c_void>())
            }
        }
    }

    // SAFETY: CFBundle is reference-counted by CoreFoundation; we
    // hold one owned reference and CoreFoundation itself is
    // thread-safe for read access. The Drop impl releases the
    // CFBundle but deliberately never calls
    // `CFBundleUnloadExecutable` — VST3 plugins leave Objective-C
    // class registrations and runloop callbacks pointing into the
    // dylib, and unloading invalidates them. Same "don't dlclose"
    // discipline truce-loader follows on the plugin side.
    unsafe impl Send for MacBundle {}
}

/// Per-platform VST3 bundle layout. Linux uses
/// `Contents/<arch>-linux/<stem>.so`; Windows uses
/// `Contents/<arch>-win/<stem>.vst3`. macOS goes through `CFBundle`
/// instead (see [`mac::MacBundle`]) so its binary lookup lives
/// there.
#[cfg(not(target_os = "macos"))]
fn bundle_binary_path(bundle: &Path) -> PathBuf {
    let stem = bundle
        .file_stem()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    #[cfg(target_os = "linux")]
    {
        if bundle.is_dir() {
            let arch_dir = format!("{}-linux", std::env::consts::ARCH);
            let mut binary = stem.clone();
            binary.push(".so");
            return bundle.join("Contents").join(arch_dir).join(binary);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if bundle.is_dir() {
            let arch_dir = format!("{}-win", std::env::consts::ARCH);
            let mut binary = stem.clone();
            binary.push(".vst3");
            return bundle.join("Contents").join(arch_dir).join(binary);
        }
    }
    let _ = stem;
    bundle.to_path_buf()
}

/// RAII wrapper around the loaded module. On macOS this is a real
/// `CFBundle` so the plugin's `bundleEntry` sees the `CFPlugin` /
/// `CFBundleGetIdentifier` context it expects (raw dlopen crashes
/// some bundles — Surge XT Effects calls into `CFPlugin`'s
/// `AddInstanceForFactory` during init). On Linux / Windows the
/// underlying file is a plain dynamic library; `libloading` is
/// enough.
#[cfg(not(target_os = "macos"))]
struct LoadedModule {
    library: libloading::Library,
}

#[cfg(target_os = "macos")]
struct LoadedModule {
    bundle: mac::MacBundle,
    entered: bool,
}

#[cfg(not(target_os = "macos"))]
impl LoadedModule {
    unsafe fn open(bundle: &Path) -> Result<Self> {
        let binary = bundle_binary_path(bundle);
        let library =
            unsafe { libloading::Library::new(&binary) }.map_err(|e| Error::LoadFailed {
                path: bundle.to_path_buf(),
                reason: format!("dlopen: {e}"),
            })?;
        Ok(Self { library })
    }

    fn factory(&self) -> Result<ComPtr<IPluginFactory>> {
        let get_factory: libloading::Symbol<'_, unsafe extern "C" fn() -> *mut IPluginFactory> =
            unsafe { self.library.get(GET_FACTORY_SYMBOL) }.map_err(|e| Error::LoadFailed {
                path: PathBuf::new(),
                reason: format!("missing GetPluginFactory: {e}"),
            })?;
        let ptr = unsafe { get_factory() };
        let factory = unsafe { ComPtr::<IPluginFactory>::from_raw(ptr) }.ok_or_else(|| {
            Error::LoadFailed {
                path: PathBuf::new(),
                reason: "GetPluginFactory returned NULL".into(),
            }
        })?;
        Ok(factory)
    }
}

#[cfg(target_os = "macos")]
impl LoadedModule {
    unsafe fn open(bundle: &Path) -> Result<Self> {
        let mac_bundle = mac::MacBundle::open(bundle)?;

        // VST3 macOS spec: bundleEntry takes the CFBundleRef the
        // host loaded the plugin from. Surge XT Effects (and any
        // bundle that touches CFPlugin/AU registration in init)
        // dereferences that argument; passing nothing crashes
        // inside CFRetain on a register that happened to be
        // non-zero. Reference impl: Steinberg's `module_mac.mm`.
        let entered = unsafe {
            match mac_bundle.function_ptr(BUNDLE_ENTRY_SYMBOL) {
                Some(ptr) => {
                    let entry: unsafe extern "C" fn(*mut std::ffi::c_void) -> bool =
                        std::mem::transmute(ptr);
                    entry(mac_bundle.raw())
                }
                None => false,
            }
        };

        Ok(Self {
            bundle: mac_bundle,
            entered,
        })
    }

    fn factory(&self) -> Result<ComPtr<IPluginFactory>> {
        let raw = unsafe {
            self.bundle
                .function_ptr(GET_FACTORY_SYMBOL)
                .ok_or_else(|| Error::LoadFailed {
                    path: self.bundle.path().to_path_buf(),
                    reason: "missing GetPluginFactory".into(),
                })?
        };
        let get_factory: unsafe extern "C" fn() -> *mut IPluginFactory =
            unsafe { std::mem::transmute(raw) };
        let ptr = unsafe { get_factory() };
        let factory = unsafe { ComPtr::<IPluginFactory>::from_raw(ptr) }.ok_or_else(|| {
            Error::LoadFailed {
                path: self.bundle.path().to_path_buf(),
                reason: "GetPluginFactory returned NULL".into(),
            }
        })?;
        Ok(factory)
    }
}

#[cfg(target_os = "macos")]
impl Drop for LoadedModule {
    fn drop(&mut self) {
        // bundleExit is the symmetric partner to bundleEntry and
        // takes no arguments per the VST3 macOS spec — only
        // bundleEntry sees the CFBundleRef. Most plugins are
        // no-ops; some unregister CFPlugin factories here.
        if self.entered
            && let Some(ptr) = unsafe { self.bundle.function_ptr(BUNDLE_EXIT_SYMBOL) }
        {
            let exit: unsafe extern "C" fn() -> bool = unsafe { std::mem::transmute(ptr) };
            unsafe {
                exit();
            }
        }
    }
}

/// One loaded VST3 plugin instance.
///
/// Holds three COM smart pointers — component, audio processor,
/// edit controller — plus the dlopen handle that keeps the
/// underlying dylib mapped. When `Drop`s, the COM pointers
/// release their objects which triggers `terminate()` and
/// component disposal.
pub struct Vst3Plugin {
    info: PluginInfo,
    layouts: Vec<BusLayout>,
    active_layout: Option<BusLayout>,

    // Hold the module open for the lifetime of the instance.
    _module: LoadedModule,
    _host_context: ComWrapper<HostContext>,
    host_restart_flags: Arc<AtomicI32>,
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: ComPtr<IEditController>,
    /// `true` when controller and component are *different* COM
    /// objects (separate-controller architecture) and we've wired
    /// their connection points.
    separate_controller: bool,
    component_cp: Option<ComPtr<IConnectionPoint>>,
    controller_cp: Option<ComPtr<IConnectionPoint>>,
    /// `IMidiMapping` from the edit controller, when the plugin
    /// exposes one. Lets us turn MIDI CC / channel-pressure /
    /// pitch-bend input into the parameter changes the processor
    /// actually reads (VST3 delivers controller-style MIDI through
    /// `IParameterChanges`, not `IEventList`).
    midi_mapping: Option<ComPtr<IMidiMapping>>,

    /// Host-thread `set_parameter` writes queued for delivery to
    /// the processor via `inputParameterChanges` on the next
    /// `process` call. Each entry is a `(param id, normalized
    /// value)` pair; drained every block.
    pending_param_changes: Vec<(ParamID, f64)>,

    param_count: usize,
    processing: bool,

    /// Cached `IPlugView` for the plugin's editor. Created on
    /// `open()`, released on `close()` / Drop.
    view: Option<ComPtr<IPlugView>>,
    editor_open: bool,
}

impl Vst3Plugin {
    fn load_from(info: &PluginInfo) -> Result<Self> {
        let module = unsafe { LoadedModule::open(&info.path) }?;
        let factory = module.factory()?;
        let host_restart_flags = Arc::new(AtomicI32::new(0));
        let host_context = ComWrapper::new(HostContext {
            restart_flags: Arc::clone(&host_restart_flags),
        });
        let host_application = host_context
            .to_com_ptr::<IHostApplication>()
            .ok_or_else(|| Error::Other("HostContext missing IHostApplication IID".into()))?;

        let cid = hex_to_tuid(&info.unique_id).ok_or_else(|| Error::LoadFailed {
            path: info.path.clone(),
            reason: format!("could not parse VST3 unique_id {:?}", info.unique_id),
        })?;

        // Create IComponent.
        let component_ptr =
            unsafe { create_instance::<IComponent>(&factory, &cid) }.ok_or_else(|| {
                Error::LoadFailed {
                    path: info.path.clone(),
                    reason: "factory.createInstance(IComponent) returned NULL".into(),
                }
            })?;
        let component = component_ptr;

        if unsafe { component.initialize(host_application.as_ptr().cast::<FUnknown>()) }
            != kResultOk
        {
            return Err(Error::LoadFailed {
                path: info.path.clone(),
                reason: "IComponent::initialize returned non-OK".into(),
            });
        }

        // Cast to IAudioProcessor.
        let processor = component
            .as_com_ref()
            .cast::<IAudioProcessor>()
            .ok_or_else(|| Error::LoadFailed {
                path: info.path.clone(),
                reason: "component does not implement IAudioProcessor".into(),
            })?;

        // Look up the separate-controller class id; if present we
        // create a separate controller and connect to it. Otherwise
        // the component is itself the controller.
        let mut controller_cid: TUID = [0; 16];
        let cls_id_status = unsafe { component.getControllerClassId(&raw mut controller_cid) };
        let (controller, separate_controller) = if cls_id_status == kResultTrue {
            let ctrl_ptr = unsafe { create_instance::<IEditController>(&factory, &controller_cid) }
                .ok_or_else(|| Error::LoadFailed {
                    path: info.path.clone(),
                    reason: "factory.createInstance(IEditController) returned NULL".into(),
                })?;
            if unsafe { ctrl_ptr.initialize(host_application.as_ptr().cast::<FUnknown>()) }
                != kResultOk
            {
                return Err(Error::LoadFailed {
                    path: info.path.clone(),
                    reason: "IEditController::initialize returned non-OK".into(),
                });
            }
            (ctrl_ptr, true)
        } else {
            // Single-component plugin — controller and component
            // share an object. Cast through queryInterface so the
            // refcount is correct.
            let ctrl = component
                .as_com_ref()
                .cast::<IEditController>()
                .ok_or_else(|| Error::LoadFailed {
                    path: info.path.clone(),
                    reason:
                        "component is not its own controller and didn't report a controller cid"
                            .into(),
                })?;
            (ctrl, false)
        };

        let component_handler = host_context
            .to_com_ptr::<IComponentHandler>()
            .ok_or_else(|| Error::Other("HostContext missing IComponentHandler IID".into()))?;
        let handler_status = unsafe { controller.setComponentHandler(component_handler.as_ptr()) };
        if handler_status != kResultOk && handler_status != kNotImplemented {
            return Err(Error::LoadFailed {
                path: info.path.clone(),
                reason: format!("IEditController::setComponentHandler returned {handler_status}"),
            });
        }

        // Optional: connect the two connection points so the
        // plugin's component and controller can talk to each
        // other (param/audio synchronisation). Best-effort —
        // some plugins skip these even when the controller is
        // separate.
        let (component_cp, controller_cp) = if separate_controller {
            let cp_a = component.as_com_ref().cast::<IConnectionPoint>();
            let cp_b = controller.as_com_ref().cast::<IConnectionPoint>();
            if let (Some(a), Some(b)) = (&cp_a, &cp_b) {
                unsafe {
                    a.connect(b.as_com_ref().as_ptr().cast());
                    b.connect(a.as_com_ref().as_ptr().cast());
                }
            }
            (cp_a, cp_b)
        } else {
            (None, None)
        };

        // IMidiMapping is optional — synths usually expose it,
        // pure effects often don't. Absence just means CC input has
        // nowhere to map to.
        let midi_mapping = controller.as_com_ref().cast::<IMidiMapping>();

        let param_count_raw = unsafe { controller.getParameterCount() }.max(0);
        #[allow(clippy::cast_sign_loss)]
        let param_count = param_count_raw as usize;

        let mut info = info.clone();
        // The editor exists if `createView("editor")` returns a
        // non-null view. We probe by creating + releasing it once
        // at load time. (Some plugins are slow to create the view;
        // a heavier-weight host would defer this to first open.)
        info.has_editor = unsafe { create_editor_view(&controller) }.is_some();
        let layouts = vec![discover_audio_layout(&component)];

        Ok(Self {
            info,
            layouts,
            active_layout: None,
            _module: module,
            _host_context: host_context,
            host_restart_flags,
            component,
            processor,
            controller,
            separate_controller,
            component_cp,
            controller_cp,
            midi_mapping,
            pending_param_changes: Vec::new(),
            param_count,
            processing: false,
            view: None,
            editor_open: false,
        })
    }

    /// Restore VST3 preset state from separate component and
    /// controller chunks.
    ///
    /// VST3 `.vstpreset` files can store the audio component state
    /// (`Comp`) and edit-controller state (`Cont`) separately. The
    /// generic [`PluginCore::load_state`] method restores only a
    /// single opaque blob, so hosts that parse `.vstpreset` files can
    /// use this method to restore both chunks.
    ///
    /// # Errors
    ///
    /// Returns an error when the component rejects `component_state`,
    /// when the controller rejects `component_state` via
    /// `setComponentState`, or when a supplied `controller_state` is
    /// rejected by `IEditController::setState`.
    pub fn load_vst3_preset_state(
        &mut self,
        component_state: &[u8],
        controller_state: Option<&[u8]>,
    ) -> Result<()> {
        self.load_component_state(component_state)?;
        if let Some(controller_state) = controller_state {
            self.load_controller_state(controller_state)?;
        }
        self.queue_changed_parameter_values()?;
        Ok(())
    }

    fn load_component_state(&mut self, bytes: &[u8]) -> Result<()> {
        let component_stream = stream_from_bytes(bytes)?;
        let status = unsafe { self.component.setState(component_stream.as_ptr()) };
        if status != kResultOk {
            return Err(Error::Other(format!(
                "IComponent::setState returned {status}"
            )));
        }

        let controller_stream = stream_from_bytes(bytes)?;
        let status = unsafe {
            self.controller
                .setComponentState(controller_stream.as_ptr())
        };
        if status != kResultOk && status != kNotImplemented {
            return Err(Error::Other(format!(
                "IEditController::setComponentState returned {status}"
            )));
        }
        Ok(())
    }

    fn load_controller_state(&mut self, bytes: &[u8]) -> Result<()> {
        let stream = stream_from_bytes(bytes)?;
        let status = unsafe { self.controller.setState(stream.as_ptr()) };
        if status != kResultOk && status != kNotImplemented {
            return Err(Error::Other(format!(
                "IEditController::setState returned {status}"
            )));
        }
        Ok(())
    }

    fn queue_changed_parameter_values(&mut self) -> Result<()> {
        let flags = self.host_restart_flags.swap(0, Ordering::AcqRel);
        if flags & RestartFlags_::kParamValuesChanged == 0 {
            return Ok(());
        }
        self.queue_all_parameter_values()
    }

    fn queue_all_parameter_values(&mut self) -> Result<()> {
        self.pending_param_changes.clear();
        self.pending_param_changes.reserve(self.param_count);
        for index in 0..self.param_count {
            let mut info = empty_parameter_info();
            let i32_index = i32::try_from(index).map_err(|_| Error::InvalidParameter(index))?;
            if unsafe { self.controller.getParameterInfo(i32_index, &raw mut info) } != kResultOk {
                return Err(Error::InvalidParameter(index));
            }
            let value = unsafe { self.controller.getParamNormalized(info.id) };
            if value.is_finite() {
                // Reuse the host-thread setter queue; process() drains
                // it into inputParameterChanges, delivering the restored
                // values to the processor on the next block.
                self.pending_param_changes.push((info.id, value));
            }
        }
        Ok(())
    }
}

/// Call `IEditController::createView("editor")` and wrap the raw
/// pointer. Returns `None` when the plugin has no editor.
unsafe fn create_editor_view(controller: &ComPtr<IEditController>) -> Option<ComPtr<IPlugView>> {
    let raw = unsafe { controller.createView(ViewType::kEditor) };
    if raw.is_null() {
        return None;
    }
    unsafe { ComPtr::<IPlugView>::from_raw(raw) }
}

fn platform_type_for_handle(
    handle: truce_rack_core::editor::WindowHandle,
) -> (*const i8, *mut std::ffi::c_void) {
    use truce_rack_core::editor::WindowHandle;
    match handle {
        WindowHandle::NSView(p) => (kPlatformTypeNSView, p),
        WindowHandle::HWND(p) => (kPlatformTypeHWND, p),
        WindowHandle::X11(id) => (kPlatformTypeX11EmbedWindowID, id as *mut std::ffi::c_void),
    }
}

/// Run `factory.createInstance` for `I` and wrap the result as a
/// `ComPtr<I>`. The factory call uses the interface's `IID`
/// directly so the plugin returns the correct `*mut c_void`.
unsafe fn create_instance<I>(factory: &ComPtr<IPluginFactory>, cid: &TUID) -> Option<ComPtr<I>>
where
    I: vst3::Interface,
{
    let mut obj: *mut std::ffi::c_void = ptr::null_mut();
    // `com_scrape_types::Guid` and `TUID` share the same
    // `[i8; 16]` layout — reinterpret to avoid an extra copy.
    let iid_bytes: &TUID =
        unsafe { &*(std::ptr::from_ref::<vst3::com_scrape_types::Guid>(&I::IID).cast::<TUID>()) };
    let cid_ptr = cid.as_ptr();
    let iid_ptr = iid_bytes.as_ptr();
    if unsafe { factory.createInstance(cid_ptr, iid_ptr, &raw mut obj) } != kResultOk
        || obj.is_null()
    {
        return None;
    }
    unsafe { ComPtr::<I>::from_raw(obj.cast()) }
}

// VST3 enum constants are `c_int`/`c_uint` depending on platform, so
// the `as i32` casts are only a potential wrap on some targets.
#[allow(clippy::cast_possible_wrap, clippy::unnecessary_cast)]
fn discover_audio_layout(component: &ComPtr<IComponent>) -> BusLayout {
    let mut layout = BusLayout::new();
    append_audio_buses(component, BusDirections_::kInput as i32, &mut layout);
    append_audio_buses(component, BusDirections_::kOutput as i32, &mut layout);

    if layout.outputs.is_empty() {
        layout
            .outputs
            .push(Bus::main("Output", ChannelConfig::Stereo));
    }

    layout
}

#[allow(clippy::cast_possible_wrap, clippy::unnecessary_cast)]
fn append_audio_buses(component: &ComPtr<IComponent>, direction: i32, layout: &mut BusLayout) {
    let count = unsafe { component.getBusCount(MediaTypes_::kAudio as i32, direction) }.max(0);
    for index in 0..count {
        let Some(bus) = query_audio_bus(component, direction, index) else {
            continue;
        };
        if direction == BusDirections_::kInput as i32 {
            layout.inputs.push(bus);
        } else {
            layout.outputs.push(bus);
        }
    }
}

#[allow(clippy::cast_possible_wrap, clippy::unnecessary_cast)]
fn query_audio_bus(component: &ComPtr<IComponent>, direction: i32, index: i32) -> Option<Bus> {
    let mut info = empty_bus_info();
    if unsafe { component.getBusInfo(MediaTypes_::kAudio as i32, direction, index, &raw mut info) }
        != kResultOk
    {
        return None;
    }

    let channels = channel_config_for_count(info.channelCount)?;
    let name = {
        let raw = string128_to_string(&info.name);
        if raw.is_empty() {
            if direction == BusDirections_::kInput as i32 {
                format!("Input {}", index + 1)
            } else {
                format!("Output {}", index + 1)
            }
        } else {
            raw
        }
    };
    let kind = if info.busType == BusTypes_::kMain as i32 || index == 0 {
        BusKind::Main
    } else if direction == BusDirections_::kInput as i32 {
        BusKind::Sidechain
    } else {
        BusKind::Auxiliary
    };

    Some(Bus {
        name,
        kind,
        channels,
    })
}

fn empty_bus_info() -> BusInfo {
    BusInfo {
        mediaType: 0,
        direction: 0,
        channelCount: 0,
        name: [0; 128],
        busType: 0,
        flags: 0,
    }
}

fn channel_config_for_count(count: i32) -> Option<ChannelConfig> {
    match count {
        1 => Some(ChannelConfig::Mono),
        2 => Some(ChannelConfig::Stereo),
        n if n > 0 => Some(ChannelConfig::Discrete(u32::try_from(n).ok()?)),
        _ => None,
    }
}

fn speaker_arrangement_for_channels(channels: u32) -> u64 {
    match channels {
        0 => 0,
        n if n >= u64::BITS => u64::MAX,
        n => (1u64 << n) - 1,
    }
}

impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        if self.processing {
            unsafe { self.processor.setProcessing(0) };
        }
        if self.active_layout.is_some() {
            unsafe { self.component.setActive(0) };
        }
        if let (Some(a), Some(b)) = (&self.component_cp, &self.controller_cp) {
            unsafe {
                a.disconnect(b.as_com_ref().as_ptr().cast());
                b.disconnect(a.as_com_ref().as_ptr().cast());
            }
        }
        if self.separate_controller {
            unsafe { self.controller.terminate() };
        }
        unsafe { self.component.terminate() };
    }
}

impl PluginCore for Vst3Plugin {
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
        self.param_count
    }

    fn parameter_info(&self, index: usize) -> Result<ParameterInfo> {
        if index >= self.param_count {
            return Err(Error::InvalidParameter(index));
        }
        let mut info = empty_parameter_info();
        let i32_index = i32::try_from(index).map_err(|_| Error::InvalidParameter(index))?;
        if unsafe { self.controller.getParameterInfo(i32_index, &raw mut info) } != kResultOk {
            return Err(Error::InvalidParameter(index));
        }
        Ok(vst3_param_info_to_rack(&info))
    }

    fn parameter_value(&self, index: usize) -> Result<f64> {
        if index >= self.param_count {
            return Err(Error::InvalidParameter(index));
        }
        let mut info = empty_parameter_info();
        let i32_index = i32::try_from(index).map_err(|_| Error::InvalidParameter(index))?;
        if unsafe { self.controller.getParameterInfo(i32_index, &raw mut info) } != kResultOk {
            return Err(Error::InvalidParameter(index));
        }
        Ok(unsafe { self.controller.getParamNormalized(info.id) })
    }

    fn parameter_value_string(&self, index: usize, _value: f64) -> Result<String> {
        // VST3 exposes IEditController::getParamStringByValue —
        // wiring it requires a TChar (UTF-16) buffer round-trip.
        // Tracked as a follow-on to avoid pulling in a wide-char
        // dep here.
        let _ = index;
        Err(Error::Other("vst3 parameter_value_string TODO".into()))
    }

    fn set_parameter(&mut self, index: usize, value: f64) -> Result<()> {
        if index >= self.param_count {
            return Err(Error::InvalidParameter(index));
        }
        let mut info = empty_parameter_info();
        let i32_index = i32::try_from(index).map_err(|_| Error::InvalidParameter(index))?;
        if unsafe { self.controller.getParameterInfo(i32_index, &raw mut info) } != kResultOk {
            return Err(Error::InvalidParameter(index));
        }
        let clamped = value.clamp(0.0, 1.0);
        // Update the controller so any open editor reflects the new
        // value...
        if unsafe { self.controller.setParamNormalized(info.id, clamped) } != kResultOk {
            return Err(Error::Other(
                "IEditController::setParamNormalized failed".into(),
            ));
        }
        // ...and queue the same change for the processor. For
        // separate controller/processor plugins, setParamNormalized
        // alone never reaches the audio side — the processor only
        // observes parameter changes that arrive through
        // inputParameterChanges during process().
        self.pending_param_changes.push((info.id, clamped));
        Ok(())
    }

    fn preset_count(&self) -> usize {
        // VST3 exposes presets via the `IUnitInfo` interface,
        // wired in a follow-on. Treat as zero for now.
        0
    }
    fn preset_info(&self, index: usize) -> Result<PresetInfo> {
        Err(Error::InvalidParameter(index))
    }
    fn load_preset(&mut self, _preset_number: i32) -> Result<()> {
        Err(Error::Other("vst3 preset loading not yet wired".into()))
    }

    fn save_state(&self) -> Result<Vec<u8>> {
        let stream = ComWrapper::new(MemoryStream::default());
        let stream_ptr = stream
            .to_com_ptr::<IBStream>()
            .ok_or_else(|| Error::Other("MemoryStream missing IBStream IID".into()))?;
        let status = unsafe { self.component.getState(stream_ptr.as_ptr()) };
        if status != kResultOk {
            return Err(Error::Other(format!(
                "IComponent::getState returned {status}"
            )));
        }
        Ok(stream.data.borrow().clone())
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<()> {
        self.load_component_state(bytes)?;
        self.queue_changed_parameter_values()
    }

    fn activate(
        &mut self,
        layout: BusLayout,
        sample_rate: f64,
        max_block_size: usize,
    ) -> Result<()> {
        let input_channels = layout.total_input_channels();
        let output_channels = layout.total_output_channels();
        let mut input_arr = speaker_arrangement_for_channels(input_channels);
        let mut output_arr = speaker_arrangement_for_channels(output_channels);
        let input_arr_ptr = if input_channels == 0 {
            ptr::null_mut()
        } else {
            &raw mut input_arr
        };
        let output_arr_ptr = if output_channels == 0 {
            ptr::null_mut()
        } else {
            &raw mut output_arr
        };
        let _ = unsafe {
            self.processor.setBusArrangements(
                input_arr_ptr,
                i32::from(input_channels > 0),
                output_arr_ptr,
                i32::from(output_channels > 0),
            )
        };

        let mut setup = ProcessSetup {
            #[allow(clippy::cast_possible_wrap, clippy::unnecessary_cast)]
            processMode: ProcessModes_::kRealtime as i32,
            #[allow(clippy::cast_possible_wrap, clippy::unnecessary_cast)]
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            maxSamplesPerBlock: i32::try_from(max_block_size).unwrap_or(i32::MAX),
            sampleRate: sample_rate,
        };
        if unsafe { self.processor.setupProcessing(&raw mut setup) } != kResultOk {
            return Err(Error::Other(
                "IAudioProcessor::setupProcessing failed".into(),
            ));
        }
        self.activate_buses(true);
        if unsafe { self.component.setActive(1) } != kResultOk {
            self.activate_buses(false);
            return Err(Error::Other("IComponent::setActive(true) failed".into()));
        }
        if unsafe { self.processor.setProcessing(1) } != kResultOk {
            unsafe { self.component.setActive(0) };
            self.activate_buses(false);
            return Err(Error::Other(
                "IAudioProcessor::setProcessing(true) failed".into(),
            ));
        }
        self.processing = true;
        self.active_layout = Some(layout);
        Ok(())
    }

    fn deactivate(&mut self) {
        if self.processing {
            unsafe { self.processor.setProcessing(0) };
            self.processing = false;
        }
        if self.active_layout.is_some() {
            unsafe { self.component.setActive(0) };
        }
        self.activate_buses(false);
        self.active_layout = None;
    }
    fn is_active(&self) -> bool {
        self.active_layout.is_some()
    }

    fn editor(&mut self) -> Option<&mut dyn truce_rack_core::editor::PluginEditor> {
        if !self.info.has_editor {
            return None;
        }
        Some(self)
    }
}

impl Vst3Plugin {
    // VST3 enum constants are `c_int`/`c_uint` depending on platform,
    // so the `as i32` casts are only a potential wrap on some targets.
    #[allow(clippy::cast_possible_wrap, clippy::unnecessary_cast)]
    fn activate_buses(&self, active: bool) {
        let state = u8::from(active);
        for media_type in [MediaTypes_::kAudio, MediaTypes_::kEvent] {
            for direction in [BusDirections_::kInput, BusDirections_::kOutput] {
                let count = unsafe {
                    self.component
                        .getBusCount(media_type as i32, direction as i32)
                }
                .max(0);
                for index in 0..count {
                    unsafe {
                        self.component.activateBus(
                            media_type as i32,
                            direction as i32,
                            index,
                            state,
                        );
                    }
                }
            }
        }
    }
}

impl truce_rack_core::editor::PluginEditor for Vst3Plugin {
    fn open(
        &mut self,
        parent: truce_rack_core::editor::WindowHandle,
        _scale: f64,
    ) -> truce_rack_core::error::Result<()> {
        if self.editor_open {
            return Ok(());
        }
        let view = unsafe { create_editor_view(&self.controller) }
            .ok_or_else(|| Error::Other("IEditController::createView returned NULL".into()))?;
        let (type_str, parent_ptr) = platform_type_for_handle(parent);
        if unsafe { view.isPlatformTypeSupported(type_str) } != kResultOk {
            return Err(Error::Other(
                "IPlugView::isPlatformTypeSupported returned false".into(),
            ));
        }
        if unsafe { view.attached(parent_ptr, type_str) } != kResultOk {
            return Err(Error::Other("IPlugView::attached returned non-OK".into()));
        }
        self.view = Some(view);
        self.editor_open = true;
        Ok(())
    }

    fn close(&mut self) {
        if let Some(view) = self.view.take() {
            unsafe { view.removed() };
        }
        self.editor_open = false;
    }

    fn is_open(&self) -> bool {
        self.editor_open
    }

    fn size(&self) -> Option<(u32, u32)> {
        let view = self.view.as_ref()?;
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if unsafe { view.getSize(&raw mut rect) } != kResultOk {
            return None;
        }
        let w = u32::try_from(rect.right - rect.left).ok()?;
        let h = u32::try_from(rect.bottom - rect.top).ok()?;
        Some((w, h))
    }

    fn is_resizable(&self) -> bool {
        let Some(view) = self.view.as_ref() else {
            return false;
        };
        unsafe { view.canResize() == kResultOk }
    }

    fn set_size(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let view = self.view.as_ref()?;
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: i32::try_from(width).ok()?,
            bottom: i32::try_from(height).ok()?,
        };
        // Plugins may snap to a constraint via checkSizeConstraint.
        let _ = unsafe { view.checkSizeConstraint(&raw mut rect) };
        if unsafe { view.onSize(&raw mut rect) } != kResultOk {
            return None;
        }
        let w = u32::try_from(rect.right - rect.left).ok()?;
        let h = u32::try_from(rect.bottom - rect.top).ok()?;
        Some((w, h))
    }

    fn show(&mut self) {
        // VST3 attaches once and stays visible — no show/hide
        // separate from attached/removed.
    }

    fn hide(&mut self) {
        // Same as show — VST3 has no distinct hide.
    }
}

fn empty_parameter_info() -> Vst3ParameterInfo {
    Vst3ParameterInfo {
        id: 0,
        title: [0; 128],
        shortTitle: [0; 128],
        units: [0; 128],
        stepCount: 0,
        defaultNormalizedValue: 0.0,
        unitId: 0,
        flags: 0,
    }
}

fn vst3_param_info_to_rack(info: &Vst3ParameterInfo) -> ParameterInfo {
    let name = string128_to_string(&info.title);
    let short_name = string128_to_string(&info.shortTitle);
    let unit = string128_to_string(&info.units);
    let mut flags = truce_rack_core::info::ParameterFlags::empty();
    if info.flags & ParameterFlags_::kIsBypass != 0 {
        flags |= truce_rack_core::info::ParameterFlags::BYPASS;
    }
    if info.flags & ParameterFlags_::kCanAutomate != 0 {
        flags |= truce_rack_core::info::ParameterFlags::AUTOMATABLE;
    }
    if info.flags & ParameterFlags_::kIsHidden != 0 {
        flags |= truce_rack_core::info::ParameterFlags::HIDDEN;
    }
    if info.flags & ParameterFlags_::kIsReadOnly != 0 {
        flags |= truce_rack_core::info::ParameterFlags::READ_ONLY;
    }
    if info.flags & ParameterFlags_::kIsList != 0 {
        flags |= truce_rack_core::info::ParameterFlags::ENUMERATED;
    }
    ParameterInfo {
        id: info.id,
        name,
        short_name,
        unit,
        min: 0.0,
        max: 1.0,
        default: info.defaultNormalizedValue,
        step_count: u32::try_from(info.stepCount).unwrap_or(0),
        flags,
    }
}

/// VST3 `String128` is `[char16; 128]` (UTF-16). Walk the slice
/// until the first NUL and decode to UTF-8.
fn string128_to_string(buf: &[u16; 128]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Translate a host [`TransportInfo`] snapshot into Steinberg's
/// `Vst::ProcessContext`. Only the fields the host reported get
/// their `k…Valid` flag set; everything else stays zero so the
/// plugin treats it as absent.
// `StatesAndFlags` is `c_int` or `c_uint` depending on platform, so
// the `as u32` casts are only redundant on some targets.
#[allow(clippy::unnecessary_cast)]
fn build_vst3_context(t: &TransportInfo, sample_rate: f64) -> Vst3ProcessContext {
    // SAFETY: ProcessContext is a repr(C) aggregate of integers,
    // floats, and small POD sub-structs (Chord, FrameRate). An
    // all-zero value is the valid "no flags set" state; we then
    // fill in the fields the host actually reported.
    let mut ctx: Vst3ProcessContext = unsafe { std::mem::zeroed() };
    ctx.sampleRate = sample_rate;

    let mut state: u32 = 0;
    if let Some(tempo) = t.tempo_bpm {
        ctx.tempo = tempo;
        state |= StatesAndFlags_::kTempoValid as u32;
    }
    if let Some((num, den)) = t.time_signature {
        ctx.timeSigNumerator = i32::try_from(num).unwrap_or(4);
        ctx.timeSigDenominator = i32::try_from(den).unwrap_or(4);
        state |= StatesAndFlags_::kTimeSigValid as u32;
    }
    if let Some(beats) = t.song_position_beats {
        // projectTimeMusic is in quarter notes (TQuarterNotes = f64).
        ctx.projectTimeMusic = beats;
        state |= StatesAndFlags_::kProjectTimeMusicValid as u32;
    }
    if let Some(samples) = t.song_position_samples {
        ctx.projectTimeSamples = samples;
    }
    if let Some(bar) = t.bar_start_beats {
        ctx.barPositionMusic = bar;
        state |= StatesAndFlags_::kBarPositionValid as u32;
    }
    if t.playing {
        state |= StatesAndFlags_::kPlaying as u32;
    }
    if t.recording {
        state |= StatesAndFlags_::kRecording as u32;
    }
    if t.loop_active {
        state |= StatesAndFlags_::kCycleActive as u32;
    }
    ctx.state = state;
    ctx
}

impl Plugin<f32> for Vst3Plugin {
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

        // Build the input parameter changes the processor reads:
        // host-thread set_parameter() writes drained from the queue,
        // plus any MIDI controller events the plugin maps through
        // IMidiMapping (CC / channel pressure / pitch bend). Both
        // arrive on the same VST3 path — inputParameterChanges —
        // rather than the event list.
        let input_param_changes = ComWrapper::new(ParameterChanges::default());
        for (param_id, value) in self.pending_param_changes.drain(..) {
            input_param_changes.push_point(param_id, 0, value);
        }

        // Build an IEventList for the plugin's inputEvents slot.
        // Note-style MIDI goes here; controller-style MIDI is peeled
        // off into the parameter changes above.
        let mut translated = EventBuffer::default();
        for event in events {
            if let Some((ctrl_number, channel, normalized)) = midi_controller_assignment(event) {
                // Controller-style MIDI only reaches the processor
                // when the plugin maps it. With no IMidiMapping (or
                // no assignment for this controller) there's no VST3
                // path for it, so it's dropped rather than sent as a
                // note event.
                if let Some(mapping) = &self.midi_mapping {
                    let mut param_id: ParamID = 0;
                    let assigned = unsafe {
                        mapping.getMidiControllerAssignment(
                            0,
                            channel,
                            ctrl_number,
                            &raw mut param_id,
                        )
                    } == kResultOk;
                    if assigned {
                        let offset = i32::try_from(event.sample_offset).unwrap_or(i32::MAX);
                        input_param_changes.push_point(param_id, offset, normalized);
                    }
                }
                continue;
            }
            translated.push_rack(event);
        }
        let input_param_changes_ptr = input_param_changes
            .to_com_ptr::<IParameterChanges>()
            .ok_or_else(|| Error::Other("ParameterChanges missing IParameterChanges IID".into()))?;
        let output_param_changes = ComWrapper::new(ParameterChanges::default());
        let output_param_changes_ptr = output_param_changes
            .to_com_ptr::<IParameterChanges>()
            .ok_or_else(|| Error::Other("ParameterChanges missing IParameterChanges IID".into()))?;

        let input_events_wrapper = ComWrapper::new(EventList3 {
            events: std::cell::RefCell::new(translated.events),
        });
        let input_events_ptr = input_events_wrapper
            .to_com_ptr::<IEventList>()
            .ok_or_else(|| Error::Other("EventList3 missing IEventList IID".into()))?;
        let output_events_wrapper = ComWrapper::new(EventList3::default());
        let output_events_ptr = output_events_wrapper
            .to_com_ptr::<IEventList>()
            .ok_or_else(|| Error::Other("EventList3 missing IEventList IID".into()))?;

        let mut input_ptrs: Vec<*mut f32> = buffer
            .all_inputs()
            .iter()
            .map(|c| c.as_ptr().cast_mut())
            .collect();
        let mut input_bus = AudioBusBuffers {
            numChannels: i32::try_from(input_ptrs.len()).unwrap_or(0),
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: input_ptrs.as_mut_ptr(),
            },
        };

        let mut output_ptrs: Vec<*mut f32> = buffer
            .all_outputs()
            .iter_mut()
            .map(|c| c.as_mut_ptr())
            .collect();
        let mut output_bus = AudioBusBuffers {
            numChannels: i32::try_from(output_ptrs.len()).unwrap_or(0),
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: output_ptrs.as_mut_ptr(),
            },
        };

        // Build the transport context up front so its backing
        // struct outlives the process call.
        let mut process_context = context
            .transport
            .map(|t| build_vst3_context(&t, context.sample_rate));
        let mut data = ProcessData {
            #[allow(clippy::cast_possible_wrap, clippy::unnecessary_cast)]
            processMode: ProcessModes_::kRealtime as i32,
            #[allow(clippy::cast_possible_wrap, clippy::unnecessary_cast)]
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples: i32::try_from(frames).unwrap_or(i32::MAX),
            numInputs: i32::from(!input_ptrs.is_empty()),
            numOutputs: i32::from(!output_ptrs.is_empty()),
            inputs: if input_ptrs.is_empty() {
                ptr::null_mut()
            } else {
                &raw mut input_bus
            },
            outputs: if output_ptrs.is_empty() {
                ptr::null_mut()
            } else {
                &raw mut output_bus
            },
            inputParameterChanges: input_param_changes_ptr.as_ptr(),
            outputParameterChanges: output_param_changes_ptr.as_ptr(),
            inputEvents: input_events_ptr.as_ptr(),
            outputEvents: output_events_ptr.as_ptr(),
            processContext: process_context
                .as_mut()
                .map_or(ptr::null_mut(), std::ptr::from_mut),
        };

        let processor_ptr = self.processor.as_ptr();
        let status = run_audio_block_with::<Vst3Plugin, i32>(FORMAT, -1, || unsafe {
            ((*(*processor_ptr).vtbl).process)(processor_ptr, &raw mut data)
        });
        if status == kResultOk {
            drain_vst3_output_events(&output_events_wrapper, context.output_events);
            Ok(ProcessStatus::Continue)
        } else {
            Ok(ProcessStatus::Error)
        }
    }
}

// ---------------------------------------------------------------------------
// EventList3 — in-memory IEventList impl for MIDI in/out.
// ---------------------------------------------------------------------------

/// Translation buffer for truce-rack-core `EventList` → VST3 `Event[]`.
#[derive(Default)]
struct EventBuffer {
    events: Vec<Event>,
}

impl EventBuffer {
    fn push_rack(&mut self, event: &truce_rack_core::events::Event) {
        use truce_rack_core::events::{EventBody, MidiData};
        let offset = i32::try_from(event.sample_offset).unwrap_or(i32::MAX);
        let EventBody::Midi(body) = event.body else {
            return;
        };
        let header = |type_: u16| Event {
            busIndex: 0,
            sampleOffset: offset,
            ppqPosition: 0.0,
            flags: 0,
            r#type: type_,
            __field0: Event__type0 {
                noteOn: NoteOnEvent {
                    channel: 0,
                    pitch: 0,
                    tuning: 0.0,
                    velocity: 0.0,
                    length: -1,
                    noteId: -1,
                },
            },
        };
        match body {
            MidiData::NoteOn {
                channel,
                note,
                velocity,
            } => {
                let mut ev = header(u16::try_from(EventTypes_::kNoteOnEvent).unwrap_or(0));
                ev.__field0 = Event__type0 {
                    noteOn: NoteOnEvent {
                        channel: i16::from(channel),
                        pitch: i16::from(note),
                        tuning: 0.0,
                        velocity: f32::from(velocity) / 127.0,
                        length: -1,
                        noteId: -1,
                    },
                };
                self.events.push(ev);
            }
            MidiData::NoteOff {
                channel,
                note,
                velocity,
            } => {
                let mut ev = header(u16::try_from(EventTypes_::kNoteOffEvent).unwrap_or(0));
                ev.__field0 = Event__type0 {
                    noteOff: NoteOffEvent {
                        channel: i16::from(channel),
                        pitch: i16::from(note),
                        velocity: f32::from(velocity) / 127.0,
                        noteId: -1,
                        tuning: 0.0,
                    },
                };
                self.events.push(ev);
            }
            MidiData::PolyAftertouch {
                channel,
                note,
                pressure,
            } => {
                let mut ev = header(u16::try_from(EventTypes_::kPolyPressureEvent).unwrap_or(0));
                ev.__field0 = Event__type0 {
                    polyPressure: PolyPressureEvent {
                        channel: i16::from(channel),
                        pitch: i16::from(note),
                        pressure: f32::from(pressure) / 127.0,
                        noteId: -1,
                    },
                };
                self.events.push(ev);
            }
            // CC / channel pressure / pitch bend are peeled off in
            // `process` and delivered through IParameterChanges via
            // IMidiMapping (see `midi_controller_assignment`), so
            // they never reach this translator. ProgramChange and
            // Sysex have no IEventList representation and are
            // dropped.
            _ => {}
        }
    }
}

/// Classify a rack MIDI event as a VST3 *controller-style* message
/// and return `(controller number, channel, normalized value)`.
///
/// VST3 has no `IEventList` representation for CC, channel pressure,
/// or pitch bend — they reach the processor as parameter changes,
/// keyed by the `IMidiMapping` the plugin publishes. Note on/off and
/// poly pressure are real `Event`s and return `None` here so they
/// stay on the event-list path.
fn midi_controller_assignment(
    event: &truce_rack_core::events::Event,
) -> Option<(CtrlNumber, i16, ParamValue)> {
    use truce_rack_core::events::{EventBody, MidiData};
    // `ControllerNumbers` is a C enum (c_int/c_uint); every value we
    // use is well under i16::MAX, so the cast to CtrlNumber is exact.
    #[allow(clippy::cast_possible_truncation)]
    let ctrl = |n: ControllerNumbers| n as CtrlNumber;
    let EventBody::Midi(midi) = event.body else {
        return None;
    };
    match midi {
        MidiData::ControlChange {
            channel,
            controller,
            value,
        } => Some((
            CtrlNumber::from(controller & 0x7F),
            i16::from(channel & 0x0F),
            ParamValue::from(value & 0x7F) / 127.0,
        )),
        MidiData::ChannelAftertouch { channel, pressure } => Some((
            ctrl(ControllerNumbers_::kAfterTouch),
            i16::from(channel & 0x0F),
            ParamValue::from(pressure & 0x7F) / 127.0,
        )),
        MidiData::PitchBend { channel, value } => Some((
            ctrl(ControllerNumbers_::kPitchBend),
            i16::from(channel & 0x0F),
            ParamValue::from(value & 0x3FFF) / 16383.0,
        )),
        _ => None,
    }
}

/// Host-side `IParameterChanges` — a flat list of per-parameter
/// value queues. We populate the input instance before `process`
/// (`set_parameter` writes + mapped MIDI controllers) and hand the
/// plugin an empty output instance to record automation into.
#[derive(Default)]
struct ParameterChanges {
    queues: std::cell::RefCell<Vec<ComWrapper<ParamValueQueue>>>,
}

impl ParameterChanges {
    /// Append a `(sample offset, value)` point for `param_id`,
    /// reusing the parameter's existing queue when one is already
    /// present. Points must stay sorted by offset within a queue;
    /// callers feed events in block order so appends preserve that.
    fn push_point(&self, param_id: ParamID, sample_offset: i32, value: ParamValue) {
        let mut queues = self.queues.borrow_mut();
        if let Some(queue) = queues.iter().find(|q| q.param_id == param_id) {
            queue.points.borrow_mut().push((sample_offset, value));
            return;
        }
        queues.push(ComWrapper::new(ParamValueQueue {
            param_id,
            points: std::cell::RefCell::new(vec![(sample_offset, value)]),
        }));
    }
}

impl Class for ParameterChanges {
    type Interfaces = (IParameterChanges,);
}

#[allow(clippy::cast_sign_loss)]
impl IParameterChangesTrait for ParameterChanges {
    unsafe fn getParameterCount(&self) -> i32 {
        i32::try_from(self.queues.borrow().len()).unwrap_or(i32::MAX)
    }

    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        if index < 0 {
            return ptr::null_mut();
        }
        let queues = self.queues.borrow();
        queues.get(index as usize).map_or(ptr::null_mut(), |queue| {
            queue
                .as_com_ref::<IParamValueQueue>()
                .map_or(ptr::null_mut(), |r| r.as_ptr())
        })
    }

    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut i32,
    ) -> *mut IParamValueQueue {
        if id.is_null() {
            return ptr::null_mut();
        }
        let param_id = unsafe { *id };
        let mut queues = self.queues.borrow_mut();
        let pos = queues
            .iter()
            .position(|q| q.param_id == param_id)
            .unwrap_or_else(|| {
                queues.push(ComWrapper::new(ParamValueQueue {
                    param_id,
                    points: std::cell::RefCell::new(Vec::new()),
                }));
                queues.len() - 1
            });
        if !index.is_null() {
            unsafe { *index = i32::try_from(pos).unwrap_or(i32::MAX) };
        }
        queues[pos]
            .as_com_ref::<IParamValueQueue>()
            .map_or(ptr::null_mut(), |r| r.as_ptr())
    }
}

/// One parameter's ordered list of automation points, exposed to
/// the plugin as `IParamValueQueue`.
#[derive(Default)]
struct ParamValueQueue {
    param_id: ParamID,
    points: std::cell::RefCell<Vec<(i32, ParamValue)>>,
}

impl Class for ParamValueQueue {
    type Interfaces = (IParamValueQueue,);
}

#[allow(clippy::cast_sign_loss)]
impl IParamValueQueueTrait for ParamValueQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.param_id
    }

    unsafe fn getPointCount(&self) -> i32 {
        i32::try_from(self.points.borrow().len()).unwrap_or(i32::MAX)
    }

    unsafe fn getPoint(&self, index: i32, sample_offset: *mut i32, value: *mut ParamValue) -> i32 {
        if index < 0 || sample_offset.is_null() || value.is_null() {
            return -1;
        }
        let points = self.points.borrow();
        let Some(&(offset, val)) = points.get(index as usize) else {
            return -1;
        };
        unsafe {
            *sample_offset = offset;
            *value = val;
        }
        kResultOk
    }

    unsafe fn addPoint(&self, sample_offset: i32, value: ParamValue, index: *mut i32) -> i32 {
        let mut points = self.points.borrow_mut();
        points.push((sample_offset, value));
        if !index.is_null() {
            unsafe { *index = i32::try_from(points.len() - 1).unwrap_or(i32::MAX) };
        }
        kResultOk
    }
}

fn drain_vst3_output_events(events: &EventList3, output_events: &mut EventList) {
    for event in events.events.borrow().iter() {
        if let Some(event) = rack_event_from_vst3(event) {
            output_events.push(event);
        }
    }
}

fn rack_event_from_vst3(event: &Event) -> Option<RackEvent> {
    let sample_offset = u32::try_from(event.sampleOffset.max(0)).ok()?;
    // `EventTypes_` is a C enum whose repr is `i32` on MSVC and `u32`
    // elsewhere, while `Event::type` is `u16`. Normalize the tags to
    // the field's width up front so the comparisons compile on every
    // target — same `u16::try_from` idiom `EventBuffer::push_rack`
    // uses on the way out.
    let note_on = u16::try_from(EventTypes_::kNoteOnEvent).unwrap_or(u16::MAX);
    let note_off = u16::try_from(EventTypes_::kNoteOffEvent).unwrap_or(u16::MAX);
    let poly_pressure = u16::try_from(EventTypes_::kPolyPressureEvent).unwrap_or(u16::MAX);
    let legacy_cc = u16::try_from(EventTypes_::kLegacyMIDICCOutEvent).unwrap_or(u16::MAX);
    let body = match event.r#type {
        t if t == note_on => {
            let note = unsafe { event.__field0.noteOn };
            EventBody::Midi(MidiData::NoteOn {
                channel: midi_channel(note.channel),
                note: midi_note(note.pitch),
                velocity: normalized_to_midi(note.velocity),
            })
        }
        t if t == note_off => {
            let note = unsafe { event.__field0.noteOff };
            EventBody::Midi(MidiData::NoteOff {
                channel: midi_channel(note.channel),
                note: midi_note(note.pitch),
                velocity: normalized_to_midi(note.velocity),
            })
        }
        t if t == poly_pressure => {
            let pressure = unsafe { event.__field0.polyPressure };
            EventBody::Midi(MidiData::PolyAftertouch {
                channel: midi_channel(pressure.channel),
                note: midi_note(pressure.pitch),
                pressure: normalized_to_midi(pressure.pressure),
            })
        }
        t if t == legacy_cc => {
            let cc = unsafe { event.__field0.midiCCOut };
            legacy_midi_cc_to_rack(cc.controlNumber, cc.channel, cc.value, cc.value2)?
        }
        _ => return None,
    };

    Some(RackEvent {
        sample_offset,
        body,
    })
}

fn legacy_midi_cc_to_rack(control: u8, channel: i8, value: i8, value2: i8) -> Option<EventBody> {
    let channel = midi_channel(i16::from(channel));
    let value = midi_data_byte(value);
    let body = match control {
        0..=127 => EventBody::Midi(MidiData::ControlChange {
            channel,
            controller: control,
            value,
        }),
        CTRL_AFTERTOUCH => EventBody::Midi(MidiData::ChannelAftertouch {
            channel,
            pressure: value,
        }),
        CTRL_PITCH_BEND => {
            let lsb = u16::from(value & 0x7F);
            let msb = u16::from(midi_data_byte(value2) & 0x7F);
            EventBody::Midi(MidiData::PitchBend {
                channel,
                value: lsb | (msb << 7),
            })
        }
        CTRL_PROGRAM_CHANGE => EventBody::Midi(MidiData::ProgramChange {
            channel,
            program: value,
        }),
        _ => return None,
    };
    Some(body)
}

fn midi_channel(value: i16) -> u8 {
    u8::try_from(value).unwrap_or(0) & 0x0F
}

fn midi_note(value: i16) -> u8 {
    u8::try_from(value.clamp(0, 127)).unwrap_or(0)
}

#[allow(clippy::cast_sign_loss)]
fn midi_data_byte(value: i8) -> u8 {
    // Bit-pattern reinterpret; we only keep the low 7 data bits.
    (value as u8) & 0x7F
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn normalized_to_midi(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 127.0).round() as u8
}

#[derive(Default)]
struct EventList3 {
    events: std::cell::RefCell<Vec<Event>>,
}

impl Class for EventList3 {
    type Interfaces = (IEventList,);
}

#[allow(clippy::cast_sign_loss)]
impl IEventListTrait for EventList3 {
    unsafe fn getEventCount(&self) -> i32 {
        i32::try_from(self.events.borrow().len()).unwrap_or(i32::MAX)
    }

    unsafe fn getEvent(&self, index: i32, out: *mut Event) -> i32 {
        if out.is_null() || index < 0 {
            return -1;
        }
        let events = self.events.borrow();
        let Some(event) = events.get(index as usize) else {
            return -1;
        };
        unsafe { *out = *event };
        kResultOk
    }

    unsafe fn addEvent(&self, event: *mut Event) -> i32 {
        if event.is_null() {
            return -1;
        }
        self.events.borrow_mut().push(unsafe { *event });
        kResultOk
    }
}

// ---------------------------------------------------------------------------
// MemoryStream — in-memory IBStream impl for state save/load.
// ---------------------------------------------------------------------------

fn stream_from_bytes(bytes: &[u8]) -> Result<ComPtr<IBStream>> {
    ComWrapper::new(MemoryStream {
        data: std::cell::RefCell::new(bytes.to_vec()),
        position: std::cell::Cell::new(0),
    })
    .to_com_ptr::<IBStream>()
    .ok_or_else(|| Error::Other("MemoryStream missing IBStream IID".into()))
}

struct HostContext {
    restart_flags: Arc<AtomicI32>,
}

impl Class for HostContext {
    type Interfaces = (IHostApplication, IComponentHandler);
}

impl IHostApplicationTrait for HostContext {
    unsafe fn getName(&self, name: *mut String128) -> i32 {
        if name.is_null() {
            return -1;
        }
        let out = unsafe { &mut *name };
        out.fill(0);
        for (slot, unit) in out.iter_mut().zip("truce-rack".encode_utf16()) {
            *slot = unit as _;
        }
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        _cid: *mut TUID,
        _iid: *mut TUID,
        obj: *mut *mut std::ffi::c_void,
    ) -> i32 {
        if !obj.is_null() {
            unsafe {
                *obj = ptr::null_mut();
            }
        }
        kNotImplemented
    }
}

impl IComponentHandlerTrait for HostContext {
    unsafe fn beginEdit(&self, _id: ParamID) -> i32 {
        kResultOk
    }

    unsafe fn performEdit(&self, _id: ParamID, _value_normalized: ParamValue) -> i32 {
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> i32 {
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: i32) -> i32 {
        self.restart_flags.fetch_or(flags, Ordering::AcqRel);
        kResultOk
    }
}

/// Backing storage for the `IBStream` we hand to
/// `IComponent::setState` / `getState`. The plugin reads and
/// writes through `read`/`write`; the host inspects
/// `data` / `position` after the call.
#[derive(Default)]
struct MemoryStream {
    data: std::cell::RefCell<Vec<u8>>,
    position: std::cell::Cell<usize>,
}

impl Class for MemoryStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(
        &self,
        buffer: *mut std::ffi::c_void,
        num_bytes: i32,
        num_bytes_read: *mut i32,
    ) -> i32 {
        if buffer.is_null() || num_bytes < 0 {
            return -1;
        }
        let pos = self.position.get();
        #[allow(clippy::cast_sign_loss)]
        let want = num_bytes as usize;
        let data = self.data.borrow();
        let available = data.len().saturating_sub(pos);
        let take = want.min(available);
        if take > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr().add(pos), buffer.cast::<u8>(), take);
            }
        }
        self.position.set(pos + take);
        if !num_bytes_read.is_null() {
            unsafe {
                *num_bytes_read = i32::try_from(take).unwrap_or(i32::MAX);
            }
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut std::ffi::c_void,
        num_bytes: i32,
        num_bytes_written: *mut i32,
    ) -> i32 {
        if buffer.is_null() || num_bytes < 0 {
            return -1;
        }
        let pos = self.position.get();
        #[allow(clippy::cast_sign_loss)]
        let want = num_bytes as usize;
        let mut data = self.data.borrow_mut();
        if data.len() < pos + want {
            data.resize(pos + want, 0);
        }
        unsafe {
            std::ptr::copy_nonoverlapping(buffer.cast::<u8>(), data.as_mut_ptr().add(pos), want);
        }
        self.position.set(pos + want);
        if !num_bytes_written.is_null() {
            unsafe {
                *num_bytes_written = i32::try_from(want).unwrap_or(i32::MAX);
            }
        }
        kResultOk
    }

    #[allow(clippy::cast_possible_wrap, clippy::unnecessary_cast)]
    unsafe fn seek(&self, pos: i64, mode: i32, result: *mut i64) -> i32 {
        // VST3 SDK SeekMode: 0 = `SeekSet`, 1 = `SeekCur`, 2 = `SeekEnd`.
        let data_len = self.data.borrow().len() as i64;
        let current = self.position.get() as i64;
        let new_pos = match mode {
            0 => pos,
            1 => current + pos,
            2 => data_len + pos,
            _ => return -1,
        };
        if new_pos < 0 {
            return -1;
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        self.position.set(new_pos as usize);
        if !result.is_null() {
            unsafe {
                *result = new_pos;
            }
        }
        kResultOk
    }

    #[allow(clippy::cast_possible_wrap, clippy::unnecessary_cast)]
    unsafe fn tell(&self, pos: *mut i64) -> i32 {
        if pos.is_null() {
            return -1;
        }
        unsafe {
            *pos = self.position.get() as i64;
        }
        kResultOk
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuid_hex_roundtrip() {
        let cid: TUID = [
            0x01, 0x23, 0x45, 0x67, -0x10, -0x10, -0x10, -0x10, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let hex = tuid_to_hex(&cid);
        let parsed = hex_to_tuid(&hex).expect("parse");
        assert_eq!(parsed, cid);
    }

    #[test]
    fn char8_skips_trailing_nul() {
        let mut arr = [0i8; 16];
        for (i, &b) in b"Hello".iter().enumerate() {
            // ASCII byte → i8 always fits losslessly; the cast is
            // bit-pattern-identical for `b` < 128.
            arr[i] = i8::try_from(b).expect("ASCII byte fits in i8");
        }
        assert_eq!(char8_array_to_string(&arr), "Hello");
    }
}
