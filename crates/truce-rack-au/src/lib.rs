//! Audio Unit v2 host for the truce-rack framework. Apple-only.
//!
//! Built on `objc2-audio-toolbox` — the modern Rust bindings
//! that replace the C++ `AudioToolbox` scanner the legacy rack 0.4
//! relied on. No cmake, no bridging-header step.
//!
//! # Status
//!
//! - **Scan** via `AudioComponentFindNext` across the AU v2 type
//!   families (Effect, `MusicDevice`, Generator, Mixer, `MusicEffect`,
//!   `MIDIProcessor`). Each match yields one [`PluginInfo`] with the
//!   AU's four-CC packed as a `"type:sub:mfr"` `unique_id`.
//! - **Load** via `AudioComponentInstanceNew`.
//! - **Process** via `AudioUnitRender` into a per-block
//!   `AudioBufferList` whose buffers point directly at the host's
//!   output planes (see [`AuPlugin::process`]).
//! - **MIDI** via `MusicDeviceMIDIEvent`. Routes channel-voice
//!   messages with sample-accurate offsets; raw bytes pass through
//!   the same call. Non-MIDI-accepting effects no-op silently.
//! - **Editor** via the AUv2 Cocoa view (`kAudioUnitProperty_CocoaUI`).
//!
//! AU v3 plugins that set `kAudioComponentFlag_RequiresAsyncInstantiation`
//! still need the async path and are not yet loadable through this
//! crate; truce-rack-au3 forwards them here regardless.

#![cfg(target_vendor = "apple")]

use truce_rack_core::buffer::AudioBuffer;
use truce_rack_core::bus::BusLayout;
use truce_rack_core::error::{Error, Result};
use truce_rack_core::events::EventList;
use truce_rack_core::info::{ParameterInfo, PluginCategory, PluginInfo, PresetInfo};
use truce_rack_core::plugin::{Plugin, PluginCore, ProcessContext, ProcessStatus};
use truce_rack_core::scanner::PluginScanner;

use objc2_audio_toolbox::{
    AURenderCallbackStruct, AudioComponent, AudioComponentCopyName, AudioComponentDescription,
    AudioComponentFindNext, AudioComponentFlags, AudioComponentInstance,
    AudioComponentInstanceDispose, AudioComponentInstanceNew, AudioUnitGetParameter,
    AudioUnitGetProperty, AudioUnitGetPropertyInfo, AudioUnitInitialize, AudioUnitParameterID,
    AudioUnitParameterInfo, AudioUnitRender, AudioUnitRenderActionFlags, AudioUnitSetParameter,
    AudioUnitSetProperty, AudioUnitUninitialize, AUPreset, MusicDeviceMIDIEvent,
    kAudioUnitProperty_ClassInfo,
    kAudioUnitProperty_FactoryPresets, kAudioUnitProperty_MaximumFramesPerSlice,
    kAudioUnitProperty_ParameterInfo, kAudioUnitProperty_ParameterList,
    kAudioUnitProperty_PresentPreset, kAudioUnitProperty_SetRenderCallback,
    kAudioUnitProperty_StreamFormat, kAudioUnitScope_Global, kAudioUnitScope_Input,
    kAudioUnitScope_Output, kAudioUnitType_Effect, kAudioUnitType_Generator,
    kAudioUnitType_MIDIProcessor, kAudioUnitType_Mixer, kAudioUnitType_MusicDevice,
    kAudioUnitType_MusicEffect,
};
use objc2_core_audio_types::{
    AudioBuffer as CAAudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp,
    AudioTimeStampFlags, kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved,
    kAudioFormatFlagIsPacked, kAudioFormatLinearPCM,
};
use objc2_core_foundation::CFString;

use std::path::Path;
use std::ptr;

/// Format identifier used on returned [`PluginInfo`].
pub const FORMAT: &str = "au";

/// AU type families we surface to hosts. Output / `FormatConverter`
/// are intentionally skipped — they're system plumbing, not
/// effects.
const SCAN_TYPES: &[u32] = &[
    kAudioUnitType_Effect,
    kAudioUnitType_MusicDevice,
    kAudioUnitType_Generator,
    kAudioUnitType_MusicEffect,
    kAudioUnitType_MIDIProcessor,
    kAudioUnitType_Mixer,
];

/// AU v2 scanner.
#[derive(Debug, Default)]
pub struct AuScanner;

impl AuScanner {
    /// Construct a default scanner.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl PluginScanner for AuScanner {
    type Plugin = AuPlugin;

    fn scan(&self) -> Result<Vec<PluginInfo>> {
        let mut out = Vec::new();
        for &type_code in SCAN_TYPES {
            unsafe { scan_family(type_code, &mut out) };
        }
        Ok(out)
    }

    fn scan_path(&self, _path: &Path) -> Result<Vec<PluginInfo>> {
        // AU discovery is registry-based, not path-based.
        Err(Error::Other(
            "truce-rack-au path-bounded scan is not meaningful (AU uses a registry)".into(),
        ))
    }

    fn load(&self, info: &PluginInfo) -> Result<Self::Plugin> {
        AuPlugin::load_from(info)
    }
}

unsafe fn scan_family(component_type: u32, out: &mut Vec<PluginInfo>) {
    let mut desc = AudioComponentDescription {
        componentType: component_type,
        componentSubType: 0,
        componentManufacturer: 0,
        componentFlags: 0,
        componentFlagsMask: 0,
    };
    let mut component: AudioComponent = ptr::null_mut();
    let category = type_category(component_type);
    let accepts_midi =
        component_type != kAudioUnitType_Effect && component_type != kAudioUnitType_Generator;
    loop {
        let next = unsafe {
            AudioComponentFindNext(component, ptr::NonNull::new_unchecked(&raw mut desc))
        };
        if next.is_null() {
            break;
        }
        component = next;
        let mut comp_desc = AudioComponentDescription {
            componentType: 0,
            componentSubType: 0,
            componentManufacturer: 0,
            componentFlags: 0,
            componentFlagsMask: 0,
        };
        let _ = unsafe {
            objc2_audio_toolbox::AudioComponentGetDescription(
                component,
                ptr::NonNull::new_unchecked(&raw mut comp_desc),
            )
        };
        // Skip AUv3 (app-extension) components — they require
        // AudioComponentInstantiate (async / sandboxed) and are
        // owned by truce-rack-au3. AUv3 components advertise themselves
        // via the IsV3AudioUnit flag in componentFlags.
        if AudioComponentFlags::from_bits_retain(comp_desc.componentFlags)
            .contains(AudioComponentFlags::IsV3AudioUnit)
        {
            continue;
        }
        let name = unsafe { component_name(component) };
        let (vendor, display) = split_name(&name);
        out.push(PluginInfo {
            name: display,
            vendor,
            version: unsafe { component_version(component) },
            category,
            path: std::path::PathBuf::new(),
            unique_id: format_unique_id(&comp_desc),
            format: FORMAT,
            has_editor: false,
            accepts_midi,
        });
    }
}

/// Query `kAudioUnitProperty_ParameterInfo` for one parameter.
unsafe fn fetch_parameter_info(
    unit: AudioComponentInstance,
    id: AudioUnitParameterID,
) -> Result<AudioUnitParameterInfo> {
    let mut info = AudioUnitParameterInfo {
        name: [0; 52],
        unitName: ptr::null(),
        clumpID: 0,
        cfNameString: ptr::null(),
        unit: objc2_audio_toolbox::AudioUnitParameterUnit::Generic,
        minValue: 0.0,
        maxValue: 0.0,
        defaultValue: 0.0,
        flags: objc2_audio_toolbox::AudioUnitParameterOptions::empty(),
    };
    #[allow(clippy::cast_possible_truncation)]
    let mut size = std::mem::size_of::<AudioUnitParameterInfo>() as u32;
    let status = unsafe {
        AudioUnitGetProperty(
            unit,
            kAudioUnitProperty_ParameterInfo,
            kAudioUnitScope_Global,
            id,
            ptr::NonNull::new_unchecked((&raw mut info).cast()),
            ptr::NonNull::new_unchecked(&raw mut size),
        )
    };
    if status != 0 {
        return Err(Error::Other(format!(
            "kAudioUnitProperty_ParameterInfo failed: OSStatus {status}"
        )));
    }
    Ok(info)
}

#[allow(clippy::cast_sign_loss)]
fn au_parameter_to_rack(id: AudioUnitParameterID, info: &AudioUnitParameterInfo) -> ParameterInfo {
    // CFNameString takes precedence over the 52-byte ASCII name
    // field — AU plugins set the latter to empty more often than
    // not since the introduction of CFString-based names.
    let name = if info.cfNameString.is_null() {
        // SAFETY: `info.name` is a NUL-terminated 52-byte ASCII
        // buffer per AU spec.
        let bytes: Vec<u8> = info
            .name
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        // SAFETY: `info.cfNameString` is a +0 reference per AU
        // spec — we don't release it.
        let s = unsafe { &*info.cfNameString };
        s.to_string()
    };
    let unit_name = if info.unitName.is_null() {
        String::new()
    } else {
        let s = unsafe { &*info.unitName };
        s.to_string()
    };
    let flags = au_param_flags_to_rack(info);
    ParameterInfo {
        id,
        name: name.clone(),
        short_name: name,
        unit: unit_name,
        min: f64::from(info.minValue),
        max: f64::from(info.maxValue),
        default: f64::from(info.defaultValue),
        step_count: 0,
        flags,
    }
}

fn au_param_flags_to_rack(
    info: &AudioUnitParameterInfo,
) -> truce_rack_core::info::ParameterFlags {
    use objc2_audio_toolbox::AudioUnitParameterOptions as Opt;
    let mut flags = truce_rack_core::info::ParameterFlags::empty();
    // AU treats most parameters as automatable by default; only
    // explicit MeterReadOnly takes that away.
    if info.flags.contains(Opt::Flag_MeterReadOnly) {
        flags |= truce_rack_core::info::ParameterFlags::READ_ONLY;
    } else {
        flags |= truce_rack_core::info::ParameterFlags::AUTOMATABLE;
    }
    if info.flags.contains(Opt::Flag_OmitFromPresets) {
        flags |= truce_rack_core::info::ParameterFlags::HIDDEN;
    }
    flags
}

/// Query `kAudioUnitProperty_FactoryPresets` and return one
/// [`PresetInfo`] per preset. The property hands us a `CFArrayRef`
/// of `AUPreset` structs — we copy the fields out and release the
/// array.
unsafe fn fetch_factory_presets(unit: AudioComponentInstance) -> Option<Vec<PresetInfo>> {
    use objc2_core_foundation::{CFArray, CFRetained};
    let mut presets: *const CFArray = ptr::null();
    let mut size =
        u32::try_from(std::mem::size_of::<*const CFArray>()).unwrap_or(0);
    let status = unsafe {
        AudioUnitGetProperty(
            unit,
            kAudioUnitProperty_FactoryPresets,
            kAudioUnitScope_Global,
            0,
            ptr::NonNull::new_unchecked((&raw mut presets).cast()),
            ptr::NonNull::new_unchecked(&raw mut size),
        )
    };
    if status != 0 || presets.is_null() {
        return None;
    }
    // CFArrayRef from FactoryPresets is owned per Apple docs —
    // we wrap in CFRetained so it's released on drop.
    let array = unsafe {
        CFRetained::from_raw(ptr::NonNull::new_unchecked(presets.cast_mut()))
    };
    let count = array.count();
    if count <= 0 {
        return Some(Vec::new());
    }
    #[allow(clippy::cast_sign_loss)]
    let count_usize = count as usize;
    let mut out = Vec::with_capacity(count_usize);
    for i in 0..count {
        // CFArrayGetValueAtIndex returns *const c_void — points
        // at an `AUPreset` struct living inside the CFArray.
        let value = unsafe { array.value_at_index(i) };
        if value.is_null() {
            continue;
        }
        let preset = unsafe { &*value.cast::<AUPreset>() };
        let name = if preset.presetName.is_null() {
            format!("Preset {}", preset.presetNumber)
        } else {
            unsafe { &*preset.presetName }.to_string()
        };
        #[allow(clippy::cast_sign_loss)]
        out.push(PresetInfo {
            index: i as usize,
            name,
            preset_number: preset.presetNumber,
        });
    }
    Some(out)
}

/// Route one truce-rack [`truce_rack_core::events::Event`] into the AU via
/// `MusicDeviceMIDIEvent`. Non-MIDI events (param automation,
/// transport flags) are skipped — AU parameter automation goes
/// through `AudioUnitSetParameter` with a sample offset, and
/// transport flips are surfaced via the render-callback context.
fn send_midi(unit: AudioComponentInstance, event: &truce_rack_core::events::Event) {
    use truce_rack_core::events::{EventBody, MidiData};
    let offset = event.sample_offset;
    let (status, d1, d2) = match event.body {
        EventBody::Midi(MidiData::NoteOn { channel, note, velocity }) => (
            0x90 | u32::from(channel & 0x0F),
            u32::from(note & 0x7F),
            u32::from(velocity & 0x7F),
        ),
        EventBody::Midi(MidiData::NoteOff { channel, note, velocity }) => (
            0x80 | u32::from(channel & 0x0F),
            u32::from(note & 0x7F),
            u32::from(velocity & 0x7F),
        ),
        EventBody::Midi(MidiData::ControlChange { channel, controller, value }) => (
            0xB0 | u32::from(channel & 0x0F),
            u32::from(controller & 0x7F),
            u32::from(value & 0x7F),
        ),
        EventBody::Midi(MidiData::ProgramChange { channel, program }) => {
            (0xC0 | u32::from(channel & 0x0F), u32::from(program & 0x7F), 0)
        }
        EventBody::Midi(MidiData::ChannelAftertouch { channel, pressure }) => {
            (0xD0 | u32::from(channel & 0x0F), u32::from(pressure & 0x7F), 0)
        }
        EventBody::Midi(MidiData::PolyAftertouch { channel, note, pressure }) => (
            0xA0 | u32::from(channel & 0x0F),
            u32::from(note & 0x7F),
            u32::from(pressure & 0x7F),
        ),
        EventBody::Midi(MidiData::PitchBend { channel, value }) => (
            0xE0 | u32::from(channel & 0x0F),
            u32::from(value & 0x7F),
            u32::from((value >> 7) & 0x7F),
        ),
        EventBody::Midi(MidiData::Raw { len, data }) if len >= 1 => {
            let s = u32::from(data[0]);
            let d1 = if len >= 2 { u32::from(data[1]) } else { 0 };
            let d2 = if len >= 3 { u32::from(data[2]) } else { 0 };
            (s, d1, d2)
        }
        _ => return,
    };
    // Non-MusicDevice / non-MIDIFX components return
    // kAudioUnitErr_InvalidProperty here; ignore.
    let _ = unsafe { MusicDeviceMIDIEvent(unit, status, d1, d2, offset) };
}

fn type_category(type_code: u32) -> PluginCategory {
    match type_code {
        t if t == kAudioUnitType_MusicDevice => PluginCategory::Instrument,
        t if t == kAudioUnitType_MIDIProcessor => PluginCategory::NoteEffect,
        t if t == kAudioUnitType_Mixer => PluginCategory::Tool,
        _ => PluginCategory::Effect,
    }
}

/// AU `AudioComponentCopyName` returns `"<vendor>: <name>"`. Split
/// it so the host's browser shows them in separate columns.
fn split_name(full: &str) -> (String, String) {
    full.split_once(": ").map_or_else(
        || (String::new(), full.to_string()),
        |(v, n)| (v.to_string(), n.to_string()),
    )
}

unsafe fn component_name(component: AudioComponent) -> String {
    let mut cf_str: *const CFString = ptr::null();
    let status =
        unsafe { AudioComponentCopyName(component, ptr::NonNull::new_unchecked(&raw mut cf_str)) };
    if status != 0 || cf_str.is_null() {
        return String::new();
    }
    // SAFETY: AudioComponentCopyName follows the "Copy" rule —
    // we own the +1 retain on cf_str. Wrap it in CFRetained so it
    // gets released on drop.
    let retained = unsafe {
        objc2_core_foundation::CFRetained::from_raw(ptr::NonNull::new_unchecked(cf_str.cast_mut()))
    };
    retained.to_string()
}

unsafe fn component_version(component: AudioComponent) -> u32 {
    let mut version: u32 = 0;
    let _ = unsafe {
        objc2_audio_toolbox::AudioComponentGetVersion(
            component,
            ptr::NonNull::new_unchecked(&raw mut version),
        )
    };
    version
}

fn format_unique_id(desc: &AudioComponentDescription) -> String {
    format!(
        "{}:{}:{}",
        four_cc(desc.componentType),
        four_cc(desc.componentSubType),
        four_cc(desc.componentManufacturer),
    )
}

/// Render a four-CC as its printable ASCII representation if all
/// bytes are printable, otherwise its hex. AU manufacturer codes
/// are conventionally ASCII (`"appl"`, `"vlse"`, …).
fn four_cc(code: u32) -> String {
    let bytes = code.to_be_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() && *b != b':') {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        format!("{code:08x}")
    }
}

/// Parse a `"type:sub:mfr"` unique id back into a description for
/// `AudioComponentFindNext`.
fn parse_unique_id(id: &str) -> Option<AudioComponentDescription> {
    let mut parts = id.split(':');
    let t = parse_four_cc(parts.next()?)?;
    let s = parse_four_cc(parts.next()?)?;
    let m = parse_four_cc(parts.next()?)?;
    Some(AudioComponentDescription {
        componentType: t,
        componentSubType: s,
        componentManufacturer: m,
        componentFlags: 0,
        componentFlagsMask: 0,
    })
}

fn parse_four_cc(s: &str) -> Option<u32> {
    if s.len() == 4 && s.bytes().all(|b| b.is_ascii_graphic()) {
        let b = s.as_bytes();
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    } else if s.len() == 8 {
        u32::from_str_radix(s, 16).ok()
    } else {
        None
    }
}

/// One loaded AU v2 instance. Holds the `AudioComponentInstance`
/// returned by `AudioComponentInstanceNew`. Disposes on `Drop`.
pub struct AuPlugin {
    info: PluginInfo,
    layouts: Vec<BusLayout>,
    active_layout: Option<BusLayout>,
    instance: AudioComponentInstance,
    /// Parameter id list, cached at load. AU exposes parameters
    /// by `AudioUnitParameterID` (u32); truce-rack-core's trait surface
    /// uses zero-based indices. This vec is the index → id map.
    param_ids: Vec<AudioUnitParameterID>,
    /// Stable-address render plumbing — the AU calls our render
    /// callback during `AudioUnitRender` with this Box's pointer as
    /// `inRefCon`. Kept boxed so its address survives moves of
    /// `AuPlugin` itself.
    render_ctx: Option<Box<RenderContext>>,
    /// Storage for the variable-length output `AudioBufferList`
    /// we pass to `AudioUnitRender`. Re-sized on activate based on
    /// channel count.
    output_buffer_storage: Vec<u8>,
    /// Running output sample counter for `AudioTimeStamp.mSampleTime`.
    sample_time: f64,
    /// Bus channel counts as set during activate (effects need
    /// matching input/output; instruments may have 0 input).
    input_channels: u32,
    output_channels: u32,
    /// Live editor state, populated by `PluginEditor::open`. The
    /// view-factory object (`AUCocoaUIBase`) and the resulting
    /// `NSView` are kept alive here; dropping the plugin tears them
    /// down via objc2's automatic retain/release.
    editor: AuEditorState,
}

/// Per-plugin Cocoa-view editor state. Lives on the main thread
/// only — never touched from the audio thread.
#[derive(Default)]
struct AuEditorState {
    factory: Option<objc2::rc::Retained<objc2::runtime::AnyObject>>,
    view: Option<objc2::rc::Retained<objc2_app_kit::NSView>>,
}

/// Audio-thread state shared with the AU render callback. The
/// callback fills the AU's input buffer from `input_planes` —
/// `process()` refreshes these pointers each block.
struct RenderContext {
    /// One pointer per channel into the host's input data for the
    /// current block. NULL when the plugin doesn't take input
    /// (instrument case).
    input_planes: Vec<*const f32>,
    /// Frames per block — must match `inNumberFrames` the AU
    /// passes to the callback. We assert and zero-fill on mismatch.
    frames: usize,
}

unsafe extern "C-unwind" fn render_input_callback(
    in_ref_con: std::ptr::NonNull<std::ffi::c_void>,
    _io_action_flags: std::ptr::NonNull<AudioUnitRenderActionFlags>,
    _in_time_stamp: std::ptr::NonNull<AudioTimeStamp>,
    _in_bus_number: u32,
    in_number_frames: u32,
    io_data: *mut AudioBufferList,
) -> i32 {
    if io_data.is_null() {
        return -1;
    }
    let ctx_ptr = in_ref_con.as_ptr().cast::<RenderContext>();
    let ctx = unsafe { &*ctx_ptr };
    let list = unsafe { &mut *io_data };
    let frames = in_number_frames as usize;
    let buffer_count = list.mNumberBuffers as usize;
    // The variable-length tail is reached via mBuffers as a [_; 1]
    // declared slice we extend via pointer arithmetic.
    let buffers_ptr: *mut CAAudioBuffer = list.mBuffers.as_mut_ptr();
    let bytes_u32 = u32::try_from(frames * std::mem::size_of::<f32>()).unwrap_or(0);
    let bytes_usize = bytes_u32 as usize;
    for i in 0..buffer_count {
        let buffer = unsafe { &mut *buffers_ptr.add(i) };
        buffer.mDataByteSize = bytes_u32;
        if let Some(&plane) = ctx.input_planes.get(i)
            && !plane.is_null()
            && frames <= ctx.frames
        {
            buffer.mData = plane.cast::<std::ffi::c_void>().cast_mut();
            continue;
        }
        // No input available for this channel — leave mData where
        // the caller put it and zero whatever bytes were there.
        if !buffer.mData.is_null() {
            unsafe {
                std::ptr::write_bytes(buffer.mData.cast::<u8>(), 0, bytes_usize);
            }
        }
    }
    0
}

// SAFETY: AU v2 instances are not movable across audio rendering
// threads while active, but we serialize all access through
// `&mut self` and never share the pointer.
unsafe impl Send for AuPlugin {}

impl AuPlugin {
    fn load_from(info: &PluginInfo) -> Result<Self> {
        let mut desc = parse_unique_id(&info.unique_id).ok_or_else(|| Error::LoadFailed {
            path: info.path.clone(),
            reason: format!("could not parse AU unique_id {:?}", info.unique_id),
        })?;
        let component = unsafe {
            AudioComponentFindNext(ptr::null_mut(), ptr::NonNull::new_unchecked(&raw mut desc))
        };
        if component.is_null() {
            return Err(Error::LoadFailed {
                path: info.path.clone(),
                reason: format!("no AU matching {}", info.unique_id),
            });
        }
        let mut instance: AudioComponentInstance = ptr::null_mut();
        let status = unsafe {
            AudioComponentInstanceNew(component, ptr::NonNull::new_unchecked(&raw mut instance))
        };
        if status != 0 || instance.is_null() {
            return Err(Error::LoadFailed {
                path: info.path.clone(),
                reason: format!("AudioComponentInstanceNew failed with OSStatus {status}"),
            });
        }
        let param_ids = unsafe { fetch_parameter_ids(instance) };
        let has_editor = unsafe { has_cocoa_ui(instance) };
        let mut updated = info.clone();
        updated.has_editor = has_editor;
        Ok(Self {
            info: updated,
            layouts: vec![BusLayout::stereo()],
            active_layout: None,
            instance,
            param_ids,
            render_ctx: None,
            output_buffer_storage: Vec::new(),
            sample_time: 0.0,
            input_channels: 0,
            output_channels: 0,
            editor: AuEditorState::default(),
        })
    }
}

/// True if the AU advertises an `AUv2` Cocoa view (one or more
/// classes implementing `AUCocoaUIBase`).
unsafe fn has_cocoa_ui(unit: AudioComponentInstance) -> bool {
    use objc2_audio_toolbox::kAudioUnitProperty_CocoaUI;
    let mut size: u32 = 0;
    let info_status = unsafe {
        AudioUnitGetPropertyInfo(
            unit,
            kAudioUnitProperty_CocoaUI,
            kAudioUnitScope_Global,
            0,
            &raw mut size,
            ptr::null_mut(),
        )
    };
    info_status == 0 && size as usize >= std::mem::size_of::<usize>() * 2
}

/// Allocate enough bytes to hold an `AudioBufferList` with
/// `n_buffers` `AudioBuffer`s tail-included. Returns a `Vec<u8>`
/// large enough; the caller casts the data pointer.
fn alloc_audio_buffer_list(n_buffers: usize) -> Vec<u8> {
    let n = n_buffers.max(1);
    let size = std::mem::size_of::<AudioBufferList>()
        + (n - 1) * std::mem::size_of::<CAAudioBuffer>();
    vec![0u8; size]
}

/// Query `kAudioUnitProperty_ParameterList` and return the
/// parameter ids in declaration order. Returns an empty vec on
/// any failure — a plugin with zero parameters is valid.
unsafe fn fetch_parameter_ids(unit: AudioComponentInstance) -> Vec<AudioUnitParameterID> {
    let mut size: u32 = 0;
    let info_status = unsafe {
        AudioUnitGetPropertyInfo(
            unit,
            kAudioUnitProperty_ParameterList,
            kAudioUnitScope_Global,
            0,
            &raw mut size,
            ptr::null_mut(),
        )
    };
    if info_status != 0 || size == 0 {
        return Vec::new();
    }
    let count = (size as usize) / std::mem::size_of::<AudioUnitParameterID>();
    let mut ids: Vec<AudioUnitParameterID> = vec![0; count];
    let mut io_size = size;
    let get_status = unsafe {
        AudioUnitGetProperty(
            unit,
            kAudioUnitProperty_ParameterList,
            kAudioUnitScope_Global,
            0,
            ptr::NonNull::new_unchecked(ids.as_mut_ptr().cast()),
            ptr::NonNull::new_unchecked(&raw mut io_size),
        )
    };
    if get_status != 0 {
        return Vec::new();
    }
    ids.truncate(io_size as usize / std::mem::size_of::<AudioUnitParameterID>());
    ids
}

impl Drop for AuPlugin {
    fn drop(&mut self) {
        if !self.instance.is_null() {
            unsafe { AudioComponentInstanceDispose(self.instance) };
        }
    }
}

impl PluginCore for AuPlugin {
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
        self.param_ids.len()
    }

    fn parameter_info(&self, index: usize) -> Result<ParameterInfo> {
        let id = *self
            .param_ids
            .get(index)
            .ok_or(Error::InvalidParameter(index))?;
        let info = unsafe { fetch_parameter_info(self.instance, id)? };
        Ok(au_parameter_to_rack(id, &info))
    }

    fn parameter_value(&self, index: usize) -> Result<f64> {
        let id = *self
            .param_ids
            .get(index)
            .ok_or(Error::InvalidParameter(index))?;
        let mut value: f32 = 0.0;
        let status = unsafe {
            AudioUnitGetParameter(
                self.instance,
                id,
                kAudioUnitScope_Global,
                0,
                ptr::NonNull::new_unchecked(&raw mut value),
            )
        };
        if status != 0 {
            return Err(Error::Other(format!(
                "AudioUnitGetParameter failed: OSStatus {status}"
            )));
        }
        Ok(f64::from(value))
    }

    fn parameter_value_string(&self, _index: usize, _value: f64) -> Result<String> {
        // AU exposes a parameter-value-to-string property via
        // kAudioUnitProperty_ParameterValueStrings (for indexed
        // params) and AUParameterStringFromValue (for everything
        // else). Neither maps cleanly to the truce-rack-core surface
        // yet; tracked as a follow-on. For now, fall back to the
        // unit-stripped numeric form so callers get *something*
        // they can display.
        Err(Error::Other(
            "au parameter_value_string not yet wired".into(),
        ))
    }

    fn set_parameter(&mut self, index: usize, value: f64) -> Result<()> {
        let id = *self
            .param_ids
            .get(index)
            .ok_or(Error::InvalidParameter(index))?;
        #[allow(clippy::cast_possible_truncation)]
        let v = value as f32;
        let status = unsafe {
            AudioUnitSetParameter(self.instance, id, kAudioUnitScope_Global, 0, v, 0)
        };
        if status != 0 {
            return Err(Error::Other(format!(
                "AudioUnitSetParameter failed: OSStatus {status}"
            )));
        }
        Ok(())
    }
    fn preset_count(&self) -> usize {
        unsafe { fetch_factory_presets(self.instance) }.map_or(0, |v| v.len())
    }

    fn preset_info(&self, index: usize) -> Result<PresetInfo> {
        let presets = unsafe { fetch_factory_presets(self.instance) }
            .ok_or_else(|| Error::Other("kAudioUnitProperty_FactoryPresets failed".into()))?;
        let preset = presets
            .get(index)
            .ok_or(Error::InvalidParameter(index))?;
        Ok(preset.clone())
    }

    fn load_preset(&mut self, preset_number: i32) -> Result<()> {
        let preset = AUPreset {
            presetNumber: preset_number,
            presetName: ptr::null(),
        };
        let status = unsafe {
            AudioUnitSetProperty(
                self.instance,
                kAudioUnitProperty_PresentPreset,
                kAudioUnitScope_Global,
                0,
                (&raw const preset).cast(),
                u32::try_from(std::mem::size_of::<AUPreset>()).unwrap_or(0),
            )
        };
        if status != 0 {
            return Err(Error::Other(format!(
                "AudioUnitSetProperty(PresentPreset) failed: OSStatus {status}"
            )));
        }
        Ok(())
    }
    fn save_state(&self) -> Result<Vec<u8>> {
        use objc2_core_foundation::{
            CFData, CFPropertyList, CFPropertyListCreateData, CFPropertyListFormat, CFRetained,
        };
        let mut class_info: *const CFPropertyList = ptr::null();
        let mut size = u32::try_from(std::mem::size_of::<*const CFPropertyList>())
            .unwrap_or(0);
        let status = unsafe {
            AudioUnitGetProperty(
                self.instance,
                kAudioUnitProperty_ClassInfo,
                kAudioUnitScope_Global,
                0,
                ptr::NonNull::new_unchecked((&raw mut class_info).cast()),
                ptr::NonNull::new_unchecked(&raw mut size),
            )
        };
        if status != 0 || class_info.is_null() {
            return Err(Error::Other(format!(
                "kAudioUnitProperty_ClassInfo failed: OSStatus {status}"
            )));
        }
        let plist = unsafe {
            CFRetained::<CFPropertyList>::from_raw(ptr::NonNull::new_unchecked(
                class_info.cast_mut(),
            ))
        };
        let mut error: *mut objc2_core_foundation::CFError = ptr::null_mut();
        let data: Option<CFRetained<CFData>> = unsafe {
            CFPropertyListCreateData(
                None,
                Some(&plist),
                CFPropertyListFormat::BinaryFormat_v1_0,
                0,
                &raw mut error,
            )
        };
        let data = data.ok_or_else(|| {
            Error::Other("CFPropertyListCreateData returned null".into())
        })?;
        let len = data.length();
        if len < 0 {
            return Err(Error::Other("CFData length negative".into()));
        }
        #[allow(clippy::cast_sign_loss)]
        let len_usize = len as usize;
        let mut out = vec![0u8; len_usize];
        unsafe {
            data.bytes(
                objc2_core_foundation::CFRange {
                    location: 0,
                    length: len,
                },
                out.as_mut_ptr(),
            );
        }
        Ok(out)
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<()> {
        use objc2_core_foundation::{
            CFData, CFPropertyList, CFPropertyListCreateWithData, CFPropertyListFormat,
            CFRetained,
        };
        if bytes.is_empty() {
            return Err(Error::Other("empty AU state".into()));
        }
        #[allow(clippy::cast_possible_wrap)]
        let cf_data: CFRetained<CFData> = unsafe {
            CFData::new(None, bytes.as_ptr(), bytes.len() as isize)
        }
        .ok_or_else(|| Error::Other("CFData::new returned null".into()))?;
        let mut error: *mut objc2_core_foundation::CFError = ptr::null_mut();
        let mut format = CFPropertyListFormat::BinaryFormat_v1_0;
        let plist: Option<CFRetained<CFPropertyList>> = unsafe {
            CFPropertyListCreateWithData(None, Some(&cf_data), 0, &raw mut format, &raw mut error)
        };
        let plist = plist.ok_or_else(|| {
            Error::Other("CFPropertyListCreateWithData returned null".into())
        })?;
        let plist_ptr: *const CFPropertyList = CFRetained::as_ptr(&plist).as_ptr().cast_const();
        let status = unsafe {
            AudioUnitSetProperty(
                self.instance,
                kAudioUnitProperty_ClassInfo,
                kAudioUnitScope_Global,
                0,
                (&raw const plist_ptr).cast(),
                u32::try_from(std::mem::size_of::<*const CFPropertyList>()).unwrap_or(0),
            )
        };
        if status != 0 {
            return Err(Error::Other(format!(
                "AudioUnitSetProperty(ClassInfo) failed: OSStatus {status}"
            )));
        }
        Ok(())
    }
    fn activate(
        &mut self,
        layout: BusLayout,
        sample_rate: f64,
        max_block_size: usize,
    ) -> Result<()> {
        // Match legacy rack 0.4's sequence: set planar f32 stream
        // format on both scopes (effects need input, instruments
        // don't), set MaximumFramesPerSlice, then
        // AudioUnitInitialize. Sample-rate-dependent parameter
        // ranges (e.g. Apple AUBandpass's Center Frequency cap at
        // Nyquist) only become correct after this point — we
        // refresh the parameter id cache as the last step.
        let channels: u32 = 2;
        #[allow(clippy::cast_possible_truncation)]
        let format = AudioStreamBasicDescription {
            mSampleRate: sample_rate,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat
                | kAudioFormatFlagIsPacked
                | kAudioFormatFlagIsNonInterleaved,
            mBytesPerPacket: 4,
            mFramesPerPacket: 1,
            mBytesPerFrame: 4,
            mChannelsPerFrame: channels,
            mBitsPerChannel: 32,
            mReserved: 0,
        };
        let _ = unsafe {
            AudioUnitSetProperty(
                self.instance,
                kAudioUnitProperty_StreamFormat,
                kAudioUnitScope_Input,
                0,
                (&raw const format).cast(),
                u32::try_from(std::mem::size_of::<AudioStreamBasicDescription>()).unwrap_or(0),
            )
        };
        let _ = unsafe {
            AudioUnitSetProperty(
                self.instance,
                kAudioUnitProperty_StreamFormat,
                kAudioUnitScope_Output,
                0,
                (&raw const format).cast(),
                u32::try_from(std::mem::size_of::<AudioStreamBasicDescription>()).unwrap_or(0),
            )
        };
        let max_frames = u32::try_from(max_block_size).unwrap_or(u32::MAX);
        let _ = unsafe {
            AudioUnitSetProperty(
                self.instance,
                kAudioUnitProperty_MaximumFramesPerSlice,
                kAudioUnitScope_Global,
                0,
                (&raw const max_frames).cast(),
                u32::try_from(std::mem::size_of::<u32>()).unwrap_or(0),
            )
        };
        // Set the render callback before AudioUnitInitialize so
        // the AU sees its input source from frame 0.
        let mut render_ctx = Box::new(RenderContext {
            input_planes: vec![ptr::null(); channels as usize],
            frames: max_block_size,
        });
        let render_ctx_ptr: *mut RenderContext = render_ctx.as_mut();
        let callback_struct = AURenderCallbackStruct {
            inputProc: Some(render_input_callback),
            inputProcRefCon: render_ctx_ptr.cast(),
        };
        // Best-effort: instruments don't accept SetRenderCallback
        // on the input scope. We ignore the OSStatus here for
        // exactly the legacy reasons.
        let _ = unsafe {
            AudioUnitSetProperty(
                self.instance,
                kAudioUnitProperty_SetRenderCallback,
                kAudioUnitScope_Input,
                0,
                (&raw const callback_struct).cast(),
                u32::try_from(std::mem::size_of::<AURenderCallbackStruct>()).unwrap_or(0),
            )
        };

        let status = unsafe { AudioUnitInitialize(self.instance) };
        if status != 0 {
            return Err(Error::Other(format!(
                "AudioUnitInitialize failed: OSStatus {status}"
            )));
        }

        // Refresh the parameter id cache now that initialize has
        // settled the AU's sample-rate-dependent state.
        self.param_ids = unsafe { fetch_parameter_ids(self.instance) };
        self.render_ctx = Some(render_ctx);
        self.output_buffer_storage = alloc_audio_buffer_list(channels as usize);
        self.input_channels = channels;
        self.output_channels = channels;
        self.sample_time = 0.0;
        self.active_layout = Some(layout);
        Ok(())
    }
    fn deactivate(&mut self) {
        if self.is_active() {
            let _ = unsafe { AudioUnitUninitialize(self.instance) };
        }
        self.render_ctx = None;
        self.active_layout = None;
    }
    fn is_active(&self) -> bool {
        self.active_layout.is_some()
    }

    fn editor(&mut self) -> Option<&mut dyn truce_rack_core::editor::PluginEditor> {
        if self.info.has_editor {
            Some(self)
        } else {
            None
        }
    }
}

impl truce_rack_core::editor::PluginEditor for AuPlugin {
    fn open(&mut self, parent: truce_rack_core::editor::WindowHandle, _scale: f64) -> Result<()> {
        use truce_rack_core::editor::WindowHandle;
        let WindowHandle::NSView(parent_ptr) = parent else {
            return Err(Error::Other("AU editor requires an NSView parent".into()));
        };
        if parent_ptr.is_null() {
            return Err(Error::Other("AU editor: parent NSView is null".into()));
        }
        if self.editor.view.is_some() {
            return Ok(());
        }
        unsafe { open_cocoa_editor(self, parent_ptr) }
    }

    fn close(&mut self) {
        if let Some(view) = self.editor.view.take() {
            unsafe { remove_from_superview(&view) };
        }
        self.editor.factory = None;
    }

    fn is_open(&self) -> bool {
        self.editor.view.is_some()
    }

    fn size(&self) -> Option<(u32, u32)> {
        let view = self.editor.view.as_ref()?;
        let frame = unsafe { view_frame(view) };
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        Some((frame.2.max(0.0) as u32, frame.3.max(0.0) as u32))
    }

    fn is_resizable(&self) -> bool {
        // AUv2 Cocoa views are typically fixed-size: the factory
        // returns an NSView with a baked layout. Resizing is
        // accepted by setFrame: but most hosts treat AUv2 views as
        // non-resizable. We mirror that.
        false
    }

    fn set_size(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let view = self.editor.view.as_ref()?;
        unsafe { set_view_frame(view, f64::from(width), f64::from(height)) };
        Some((width, height))
    }

    fn show(&mut self) {
        if let Some(view) = self.editor.view.as_ref() {
            unsafe { set_hidden(view, false) };
        }
    }

    fn hide(&mut self) {
        if let Some(view) = self.editor.view.as_ref() {
            unsafe { set_hidden(view, true) };
        }
    }
}

/// Signature of `[AUCocoaUIBase uiViewForAudioUnit:withSize:]`.
/// Used to dispatch directly through the method's raw IMP, side-
/// stepping objc2's encoding check (Apple's AUs use one type
/// encoding for the `AudioUnit` argument, third-party AUs use the
/// older Carbon-era one — same ABI, but objc2 panics on mismatch).
type UiViewFn = unsafe extern "C-unwind" fn(
    *mut objc2::runtime::AnyObject,
    objc2::runtime::Sel,
    *mut std::ffi::c_void,
    objc2_foundation::NSSize,
) -> *mut objc2_app_kit::NSView;

/// Fetch the AU's Cocoa view factory descriptor, load the bundle,
/// instantiate the factory, ask it for an `NSView`, and add it as a
/// subview of `parent_ptr`. Stores the live factory + view on
/// `self.editor` so they survive `open` returning.
unsafe fn open_cocoa_editor(plugin: &mut AuPlugin, parent_ptr: *mut std::ffi::c_void) -> Result<()> {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_audio_toolbox::{AudioUnitCocoaViewInfo, kAudioUnitProperty_CocoaUI};
    use objc2_core_foundation::{CFRetained, CFString, CFURL};

    let mut size: u32 = 0;
    let info_status = unsafe {
        AudioUnitGetPropertyInfo(
            plugin.instance,
            kAudioUnitProperty_CocoaUI,
            kAudioUnitScope_Global,
            0,
            &raw mut size,
            ptr::null_mut(),
        )
    };
    if info_status != 0 || (size as usize) < std::mem::size_of::<AudioUnitCocoaViewInfo>() {
        return Err(Error::Other(format!(
            "kAudioUnitProperty_CocoaUI not advertised (status {info_status}, size {size})"
        )));
    }
    // The CocoaUI struct is variable-length: 1 bundle URL +
    // N class names. We get the static-sized struct here and only
    // use the first class name (mCocoaAUViewClass[0]) — the
    // overwhelming majority of AUs publish exactly one view class.
    let mut storage = vec![0u8; size as usize];
    let mut io_size = size;
    let get_status = unsafe {
        AudioUnitGetProperty(
            plugin.instance,
            kAudioUnitProperty_CocoaUI,
            kAudioUnitScope_Global,
            0,
            ptr::NonNull::new_unchecked(storage.as_mut_ptr().cast()),
            ptr::NonNull::new_unchecked(&raw mut io_size),
        )
    };
    if get_status != 0 {
        return Err(Error::Other(format!(
            "kAudioUnitProperty_CocoaUI get failed: OSStatus {get_status}"
        )));
    }
    // SAFETY: The first sizeof(AudioUnitCocoaViewInfo) bytes form
    // the descriptor; we've verified size at the property-info
    // step. The pointers inside are +1 references owned by us —
    // we wrap them in CFRetained to release on drop.
    #[allow(clippy::cast_ptr_alignment)]
    let view_info = unsafe { &*storage.as_ptr().cast::<AudioUnitCocoaViewInfo>() };
    let bundle_url = unsafe { CFRetained::<CFURL>::from_raw(view_info.mCocoaAUViewBundleLocation) };
    let class_name = unsafe { CFRetained::<CFString>::from_raw(view_info.mCocoaAUViewClass[0]) };

    // CFURL ↔ NSURL and CFString ↔ NSString are toll-free
    // bridged, but objc2's msg_send! checks the type encoding —
    // raw CFType pointers register as struct pointers, not '@'.
    // Cast through the matching NSObject type so the encoding
    // matches `id` (and any future runtime type checks pass).
    // SAFETY: toll-free bridging guarantees identical layout.
    let bundle_url_ns: &objc2_foundation::NSURL = unsafe {
        &*CFRetained::as_ptr(&bundle_url).as_ptr().cast::<objc2_foundation::NSURL>()
    };
    let class_name_ns: &objc2_foundation::NSString = unsafe {
        &*CFRetained::as_ptr(&class_name).as_ptr().cast::<objc2_foundation::NSString>()
    };

    // [NSBundle bundleWithURL:url] → NSBundle*
    let ns_bundle_class = objc2::class!(NSBundle);
    let bundle: *mut AnyObject = unsafe {
        msg_send![ns_bundle_class, bundleWithURL: bundle_url_ns]
    };
    if bundle.is_null() {
        return Err(Error::Other("NSBundle bundleWithURL: returned nil".into()));
    }
    // [bundle classNamed:name] → Class
    let view_factory_class: *mut objc2::runtime::AnyClass = unsafe {
        msg_send![bundle, classNamed: class_name_ns]
    };
    if view_factory_class.is_null() {
        return Err(Error::Other(format!(
            "view factory class not found in bundle: {}",
            unsafe { (*CFRetained::as_ptr(&class_name).as_ptr()).to_string() }
        )));
    }
    // [[class alloc] init]
    let factory_alloc: *mut AnyObject = unsafe { msg_send![view_factory_class, alloc] };
    let factory: *mut AnyObject = unsafe { msg_send![factory_alloc, init] };
    if factory.is_null() {
        return Err(Error::Other("AU view factory init returned nil".into()));
    }
    let factory_retained = unsafe { objc2::rc::Retained::from_raw(factory) }
        .ok_or_else(|| Error::Other("could not retain AU view factory".into()))?;

    // Default preferred size — most factories ignore this and pick
    // their own.
    let pref = objc2_foundation::NSSize::new(0.0, 0.0);
    // `uiViewForAudioUnit:withSize:` was historically declared with
    // the Carbon-era `^{ComponentInstanceRecord=[1q]}` encoding;
    // Apple's own AU factories migrated to the newer
    // `^{OpaqueAudioComponentInstance=}` form. objc2's msg_send!
    // panics on encoding mismatch. Skip the check by dispatching
    // through the method's raw IMP directly — same ABI, no encoding
    // verification.
    let sel = objc2::sel!(uiViewForAudioUnit:withSize:);
    let factory_class = (*factory_retained).class();
    let Some(method) = factory_class.instance_method(sel) else {
        return Err(Error::Other(
            "AU view factory has no uiViewForAudioUnit:withSize: method".into(),
        ));
    };
    let imp = method.implementation();
    // SAFETY: AUv2 view factories all expose this exact signature;
    // we matched the selector above before transmuting the IMP.
    let typed: UiViewFn = unsafe { std::mem::transmute(imp) };
    let factory_obj: *mut objc2::runtime::AnyObject =
        objc2::rc::Retained::as_ptr(&factory_retained).cast_mut();
    let view_raw: *mut objc2_app_kit::NSView =
        unsafe { typed(factory_obj, sel, plugin.instance.cast(), pref) };
    if view_raw.is_null() {
        return Err(Error::Other(
            "uiViewForAudioUnit:withSize: returned nil".into(),
        ));
    }
    // The returned view is autoreleased; retain it.
    let view = unsafe { objc2::rc::Retained::retain(view_raw) }
        .ok_or_else(|| Error::Other("could not retain AU NSView".into()))?;

    // [parent addSubview:view]
    let parent_view: *mut objc2_app_kit::NSView = parent_ptr.cast();
    let _: () = unsafe { msg_send![parent_view, addSubview: &*view] };

    plugin.editor.factory = Some(factory_retained);
    plugin.editor.view = Some(view);
    Ok(())
}

unsafe fn remove_from_superview(view: &objc2_app_kit::NSView) {
    use objc2::msg_send;
    let _: () = unsafe { msg_send![view, removeFromSuperview] };
}

unsafe fn set_hidden(view: &objc2_app_kit::NSView, hidden: bool) {
    use objc2::msg_send;
    let flag: objc2::runtime::Bool = objc2::runtime::Bool::new(hidden);
    let _: () = unsafe { msg_send![view, setHidden: flag] };
}

unsafe fn view_frame(view: &objc2_app_kit::NSView) -> (f64, f64, f64, f64) {
    use objc2::msg_send;
    use objc2_foundation::NSRect;

    // Force any pending Auto Layout pass so the descendant frames
    // we're about to walk are post-layout. Apple's stock AUv2 views
    // (AULowpass, AUMultibandCompressor, …) only finalize their
    // child-view positions on the first layout pass.
    let _: () = unsafe { msg_send![view, layoutSubtreeIfNeeded] };

    let r: NSRect = unsafe { msg_send![view, frame] };

    // AUGenericView and friends report a `frame` that's smaller
    // than the union of their descendant subviews — macOS doesn't
    // clip children to a parent's bounds by default, so the visible
    // UI extends past the parent's frame. Walk the subtree and use
    // the bounding box of every descendant frame as the real
    // preferred size.
    let (extent_w, extent_h) = unsafe { subtree_extent(view) };
    let w = extent_w.max(r.size.width);
    let h = extent_h.max(r.size.height);
    (r.origin.x, r.origin.y, w, h)
}

/// Recursively walk `view`'s subviews, computing the bounding box
/// (in `view`'s coordinate system) of every descendant's frame.
/// Returns `(max_x, max_y)` — i.e. the max width/height the view
/// would need so no descendant overflows.
unsafe fn subtree_extent(view: &objc2_app_kit::NSView) -> (f64, f64) {
    use objc2::msg_send;
    use objc2_foundation::{NSArray, NSRect};

    let subviews: *mut NSArray<objc2_app_kit::NSView> =
        unsafe { msg_send![view, subviews] };
    if subviews.is_null() {
        return (0.0, 0.0);
    }
    let count: usize = unsafe { msg_send![subviews, count] };
    let mut max_x = 0.0_f64;
    let mut max_y = 0.0_f64;
    for i in 0..count {
        let sub: *mut objc2_app_kit::NSView =
            unsafe { msg_send![subviews, objectAtIndex: i] };
        if sub.is_null() {
            continue;
        }
        let frame: NSRect = unsafe { msg_send![sub, frame] };
        max_x = max_x.max(frame.origin.x + frame.size.width);
        max_y = max_y.max(frame.origin.y + frame.size.height);
        // Recurse into the descendant, translating its extent into
        // `view`'s coordinate space.
        let (sub_w, sub_h) = unsafe { subtree_extent(&*sub) };
        max_x = max_x.max(frame.origin.x + sub_w);
        max_y = max_y.max(frame.origin.y + sub_h);
    }
    (max_x, max_y)
}

unsafe fn set_view_frame(view: &objc2_app_kit::NSView, w: f64, h: f64) {
    use objc2::msg_send;
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    let r = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
    let _: () = unsafe { msg_send![view, setFrame: r] };
}

impl Plugin<f32> for AuPlugin {
    fn process(
        &mut self,
        buffer: &mut AudioBuffer<'_, f32>,
        events: &EventList,
        _context: &mut ProcessContext<'_>,
    ) -> Result<ProcessStatus> {
        if !self.is_active() {
            return Err(Error::NotActivated);
        }
        let frames = buffer.num_frames();
        let frames_u32 = u32::try_from(frames).unwrap_or(u32::MAX);

        // Feed MIDI to the AU before AudioUnitRender so the
        // plugin sees the events at the correct sample offset
        // within this block. MusicDeviceMIDIEvent works for both
        // MusicDevice (instruments) and AUMIDIFX effects that
        // accept MIDI; for plain audio effects the call no-ops
        // with `kAudioUnitErr_InvalidProperty`, which we ignore.
        for event in events {
            send_midi(self.instance, event);
        }

        // Refresh the render context's input pointers so the
        // callback hands the AU the right plane addresses for this
        // block.
        let main_inputs = buffer.main_inputs();
        if let Some(ctx) = self.render_ctx.as_deref_mut() {
            ctx.frames = frames;
            ctx.input_planes.clear();
            for chan in main_inputs.iter().take(self.input_channels as usize) {
                ctx.input_planes.push(chan.as_ptr());
            }
            while ctx.input_planes.len() < self.input_channels as usize {
                ctx.input_planes.push(ptr::null());
            }
        }

        // Build the output AudioBufferList in-place over our
        // pre-allocated bytes. Each entry's `mData` points directly
        // at the corresponding truce-rack-core output channel.
        let main_outputs = buffer.main_outputs();
        // SAFETY: alloc_audio_buffer_list returned a Vec<u8>
        // sized to hold an AudioBufferList plus its variable
        // tail; the Vec is heap-allocated to 8-byte alignment via
        // the global allocator, which suffices for AudioBufferList.
        #[allow(clippy::cast_ptr_alignment)]
        let list_ptr = self.output_buffer_storage.as_mut_ptr().cast::<AudioBufferList>();
        unsafe {
            (*list_ptr).mNumberBuffers = self.output_channels;
            let buffers_start: *mut CAAudioBuffer = (*list_ptr).mBuffers.as_mut_ptr();
            for ch in 0..self.output_channels as usize {
                let buf = &mut *buffers_start.add(ch);
                buf.mNumberChannels = 1;
                buf.mDataByteSize =
                    u32::try_from(frames * std::mem::size_of::<f32>()).unwrap_or(0);
                buf.mData = main_outputs
                    .get_mut(ch)
                    .map_or(ptr::null_mut(), |c| c.as_mut_ptr().cast());
            }
        }

        let mut flags = AudioUnitRenderActionFlags::empty();
        let timestamp = AudioTimeStamp {
            mSampleTime: self.sample_time,
            mHostTime: 0,
            mRateScalar: 1.0,
            mWordClockTime: 0,
            mSMPTETime: objc2_core_audio_types::SMPTETime {
                mSubframes: 0,
                mSubframeDivisor: 0,
                mCounter: 0,
                mType: objc2_core_audio_types::SMPTETimeType(0),
                mFlags: objc2_core_audio_types::SMPTETimeFlags(0),
                mHours: 0,
                mMinutes: 0,
                mSeconds: 0,
                mFrames: 0,
            },
            mFlags: AudioTimeStampFlags::SampleTimeValid,
            mReserved: 0,
        };
        let status = unsafe {
            AudioUnitRender(
                self.instance,
                &raw mut flags,
                ptr::NonNull::new_unchecked((&raw const timestamp).cast_mut()),
                0, // output bus 0
                frames_u32,
                ptr::NonNull::new_unchecked(list_ptr),
            )
        };
        #[allow(clippy::cast_precision_loss)]
        let frames_f = frames as f64;
        self.sample_time += frames_f;
        if status != 0 {
            return Ok(ProcessStatus::Error);
        }
        Ok(ProcessStatus::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_cc_roundtrip() {
        let aufx = u32::from_be_bytes(*b"aufx");
        assert_eq!(four_cc(aufx), "aufx");
        assert_eq!(parse_four_cc("aufx"), Some(aufx));
    }

    #[test]
    fn unique_id_roundtrip() {
        let desc = AudioComponentDescription {
            componentType: u32::from_be_bytes(*b"aufx"),
            componentSubType: u32::from_be_bytes(*b"dely"),
            componentManufacturer: u32::from_be_bytes(*b"appl"),
            componentFlags: 0,
            componentFlagsMask: 0,
        };
        let id = format_unique_id(&desc);
        assert_eq!(id, "aufx:dely:appl");
        let parsed = parse_unique_id(&id).unwrap();
        assert_eq!(parsed.componentType, desc.componentType);
        assert_eq!(parsed.componentSubType, desc.componentSubType);
        assert_eq!(parsed.componentManufacturer, desc.componentManufacturer);
    }
}
