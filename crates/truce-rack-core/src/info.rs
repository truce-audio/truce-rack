//! Metadata types — what scanners hand back, what plugins
//! report about themselves.

use std::path::PathBuf;

/// What kind of plugin this is. Hosts use this to filter their
/// browser (instruments separately from effects, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCategory {
    /// Audio effect — takes audio in, produces audio out.
    Effect,
    /// Instrument — takes MIDI in, produces audio out.
    Instrument,
    /// MIDI effect — takes MIDI in, produces MIDI out.
    NoteEffect,
    /// Analyzer / metering — observes audio, may not produce
    /// audio output.
    Analyzer,
    /// Other tool category (utility, format converter, etc.).
    Tool,
}

/// Scanner-side info about a discovered plugin. This is what
/// hosts hand back from [`crate::PluginScanner::scan`]; the
/// caller picks one and hands it back to
/// [`crate::PluginScanner::load`] to materialise an instance.
#[derive(Debug, Clone)]
pub struct PluginInfo {
    /// Display name.
    pub name: String,
    /// Vendor / manufacturer name.
    pub vendor: String,
    /// Plugin version, packed as host saw it.
    pub version: u32,
    /// Category for browser UIs.
    pub category: PluginCategory,
    /// Filesystem path to the bundle / dylib (where applicable).
    pub path: PathBuf,
    /// Format-specific stable id. CLAP uses the plugin id string,
    /// VST3 uses the 16-byte CID rendered as hex, AU uses the
    /// `type/subtype/manufacturer` 4ccs packed as
    /// `"type:subtype:mfr"`. Hosts pass this back into `load`.
    pub unique_id: String,
    /// Which format wrapper produced this entry — `"clap"`,
    /// `"vst3"`, `"au"`, etc. Lets a multi-format host that
    /// aggregates scans tell entries apart in its browser.
    pub format: &'static str,
    /// Whether the plugin reports a GUI (a custom editor view).
    pub has_editor: bool,
    /// Whether the plugin handles MIDI input. Used by hosts that
    /// only want to enumerate instruments / note effects.
    pub accepts_midi: bool,
}

impl std::fmt::Display for PluginInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}] {} — {} (v{}, {:?})",
            self.format, self.name, self.vendor, self.version, self.category
        )
    }
}

/// Per-parameter metadata. The host needs all of this *before*
/// activation so it can build a parameter UI and wire automation.
#[derive(Debug, Clone)]
pub struct ParameterInfo {
    /// Stable id for this parameter (format-specific — CLAP is a
    /// 32-bit hash, VST3 is the `ParamID`, AU is the address).
    pub id: u32,
    /// Display name for the host UI.
    pub name: String,
    /// Short name for tight UI cells (truncated form of `name`).
    pub short_name: String,
    /// Unit label (`"dB"`, `"Hz"`, `"%"`, empty for unitless).
    pub unit: String,
    /// Minimum value in the parameter's native unit.
    pub min: f64,
    /// Maximum value in the parameter's native unit.
    pub max: f64,
    /// Default value in the parameter's native unit.
    pub default: f64,
    /// For stepped/integer parameters, the number of distinct
    /// values (`0` for continuous).
    pub step_count: u32,
    /// Flag bits — bypass, automatable, hidden, etc.
    pub flags: ParameterFlags,
}

bitflags::bitflags! {
    /// Bitset for parameter capabilities and host hints. Format
    /// wrappers map their native flags into this set.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ParameterFlags: u32 {
        /// This parameter is the plugin's master bypass switch.
        const BYPASS = 1 << 0;
        /// Parameter can be automated (recorded by host
        /// automation lanes).
        const AUTOMATABLE = 1 << 1;
        /// Host shouldn't show in its parameter list (plugin
        /// uses it internally for state, but it's not user-facing).
        const HIDDEN = 1 << 2;
        /// Value is read-only (meter-style — plugin writes,
        /// host reads, no UI mutation).
        const READ_ONLY = 1 << 3;
        /// Values are an enumeration with named entries — host
        /// should call `parameter_value_string` to format
        /// instead of formatting numerically.
        const ENUMERATED = 1 << 4;
    }
}

/// A factory preset entry.
#[derive(Debug, Clone)]
pub struct PresetInfo {
    /// Zero-based index in the plugin's preset list.
    pub index: usize,
    /// Preset display name.
    pub name: String,
    /// Format-specific id passed back to `load_preset`. Stored
    /// as `i32` because AU uses signed preset numbers (negative
    /// values are reserved). CLAP / VST3 use ids comfortably
    /// inside the `i32` range.
    pub preset_number: i32,
}
