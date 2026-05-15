//! AU v3 (Audio Unit App Extension) host for the truce-rack framework.
//! macOS / iOS only.
//!
//! # How v3 differs from v2
//!
//! AU v3 plugins ship as sandboxed App Extensions discovered via
//! `NSExtension` rather than as dylibs in `/Library/Audio/Plug-Ins/Components`.
//! The host communicates with the extension over XPC inside the
//! per-plugin sandbox. From the audio-rendering perspective, once
//! an `AudioComponentInstance` is in hand the interface is
//! identical to AU v2 — so truce-rack-au3's scanner filters the same
//! `AudioComponentFindNext` walk by the
//! `kAudioComponentFlag_IsV3AudioUnit` flag, and `AuPlugin` from
//! `truce-rack-au` is re-used to hold the resulting handle.
//!
//! # Status
//!
//! Scanning is implemented. Loading is forwarded to `truce-rack-au`'s
//! [`truce_rack_au::AuScanner::load`], which currently scaffolds the
//! instance but leaves the AU v3 specific async instantiation
//! (`AudioComponentInstantiate` with a completion block) as a
//! TODO. v3 plugins flagged `kAudioComponentFlag_RequiresAsyncInstantiation`
//! will fail synchronous load until that lands.

#![cfg(target_vendor = "apple")]

use truce_rack_core::error::{Error, Result};
use truce_rack_core::info::PluginInfo;
use truce_rack_core::scanner::PluginScanner;

use objc2_audio_toolbox::{
    AudioComponent, AudioComponentDescription, AudioComponentFindNext, AudioComponentFlags,
    kAudioUnitType_Effect, kAudioUnitType_Generator, kAudioUnitType_MIDIProcessor,
    kAudioUnitType_Mixer, kAudioUnitType_MusicDevice, kAudioUnitType_MusicEffect,
};

use std::path::Path;
use std::ptr;

/// Format identifier used on returned [`PluginInfo`].
pub const FORMAT: &str = "au3";

const SCAN_TYPES: &[u32] = &[
    kAudioUnitType_Effect,
    kAudioUnitType_MusicDevice,
    kAudioUnitType_Generator,
    kAudioUnitType_MusicEffect,
    kAudioUnitType_MIDIProcessor,
    kAudioUnitType_Mixer,
];

/// AU v3 scanner.
#[derive(Debug, Default)]
pub struct Au3Scanner;

impl Au3Scanner {
    /// Construct a default scanner.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl PluginScanner for Au3Scanner {
    type Plugin = truce_rack_au::AuPlugin;

    fn scan(&self) -> Result<Vec<PluginInfo>> {
        let mut out = Vec::new();
        for &type_code in SCAN_TYPES {
            unsafe { scan_family_v3(type_code, &mut out) };
        }
        // Re-stamp the format so consumers can tell v2 vs v3 in
        // their browser even though both paths route through the
        // same `AuPlugin` type.
        for info in &mut out {
            info.format = FORMAT;
        }
        Ok(out)
    }

    fn scan_path(&self, _path: &Path) -> Result<Vec<PluginInfo>> {
        Err(Error::Other(
            "truce-rack-au3 path-bounded scan is not meaningful (AU uses a registry)".into(),
        ))
    }

    fn load(&self, info: &PluginInfo) -> Result<Self::Plugin> {
        // Once the truce-rack-au loader switches to the async
        // instantiation path for `RequiresAsyncInstantiation`
        // components, this re-dispatches without further change.
        truce_rack_au::AuScanner::new().load(info)
    }
}

unsafe fn scan_family_v3(component_type: u32, out: &mut Vec<PluginInfo>) {
    let mut desc = AudioComponentDescription {
        componentType: component_type,
        componentSubType: 0,
        componentManufacturer: 0,
        componentFlags: 0,
        componentFlagsMask: 0,
    };
    let mut component: AudioComponent = ptr::null_mut();
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
        let status = unsafe {
            objc2_audio_toolbox::AudioComponentGetDescription(
                component,
                ptr::NonNull::new_unchecked(&raw mut comp_desc),
            )
        };
        if status != 0 {
            continue;
        }

        // Filter to AU v3 only — v2 components are surfaced by
        // truce-rack-au's scanner instead.
        let flags = AudioComponentFlags::from_bits_truncate(comp_desc.componentFlags);
        if !flags.contains(AudioComponentFlags::IsV3AudioUnit) {
            continue;
        }

        // Funnel into truce-rack-au's PluginInfo builder by faking the
        // walk it does — the only difference is the v3 flag check
        // above. We rebuild here rather than calling into truce-rack-au's
        // internals to keep the v3 / v2 paths independent.
        out.push(unsafe { component_to_info(component, &comp_desc) });
    }
}

unsafe fn component_to_info(
    component: AudioComponent,
    comp_desc: &AudioComponentDescription,
) -> PluginInfo {
    let name = unsafe { component_name(component) };
    let (vendor, display) = name.split_once(": ").map_or_else(
        || (String::new(), name.clone()),
        |(v, n)| (v.to_string(), n.to_string()),
    );
    let category = match comp_desc.componentType {
        t if t == kAudioUnitType_MusicDevice => truce_rack_core::info::PluginCategory::Instrument,
        t if t == kAudioUnitType_MIDIProcessor => truce_rack_core::info::PluginCategory::NoteEffect,
        t if t == kAudioUnitType_Mixer => truce_rack_core::info::PluginCategory::Tool,
        _ => truce_rack_core::info::PluginCategory::Effect,
    };
    let accepts_midi = comp_desc.componentType != kAudioUnitType_Effect
        && comp_desc.componentType != kAudioUnitType_Generator;
    PluginInfo {
        name: display,
        vendor,
        version: unsafe { component_version(component) },
        category,
        path: std::path::PathBuf::new(),
        unique_id: format!(
            "{}:{}:{}",
            four_cc(comp_desc.componentType),
            four_cc(comp_desc.componentSubType),
            four_cc(comp_desc.componentManufacturer),
        ),
        format: FORMAT,
        has_editor: false,
        accepts_midi,
    }
}

unsafe fn component_name(component: AudioComponent) -> String {
    use objc2_core_foundation::CFString;
    let mut cf_str: *const CFString = ptr::null();
    let status = unsafe {
        objc2_audio_toolbox::AudioComponentCopyName(
            component,
            ptr::NonNull::new_unchecked(&raw mut cf_str),
        )
    };
    if status != 0 || cf_str.is_null() {
        return String::new();
    }
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

fn four_cc(code: u32) -> String {
    let bytes = code.to_be_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() && *b != b':') {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        format!("{code:08x}")
    }
}
