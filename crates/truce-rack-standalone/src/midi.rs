//! Hardware MIDI input via midir.
//!
//! Opens every available MIDI input port at startup and forwards
//! channel-voice messages onto the shared [`crate::midi_queue`]. The
//! audio callback drains that queue at the top of each block, so
//! hardware notes flow into the plugin alongside QWERTY-keyboard
//! notes from the windowed handler with no extra wiring.
//!
//! Cross-platform via midir's three backends: `CoreMIDI` on macOS,
//! `WinMM` on Windows, ALSA on Linux.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use midir::{MidiInput, MidiInputConnection, MidiInputPort};

use truce_rack_core::events::{EventBody, MidiData};

use crate::midi_queue;

/// Which MIDI channel(s) reach the plugin.
///
/// CLI specs are 1-based (`1`..=`16`) to match hardware labels; the
/// `Only` variant stores the 0-based channel that appears in a
/// status byte's low nibble.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MidiChannel {
    /// Accept every channel (the default).
    #[default]
    Omni,
    /// Accept only this 0-based channel.
    Only(u8),
}

impl MidiChannel {
    /// Parse a CLI / env spec: `omni` / `all` → [`Self::Omni`], or a
    /// 1-based channel `1`..=`16` → [`Self::Only`]. `None` if
    /// malformed or out of range.
    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        let s = spec.trim().to_ascii_lowercase();
        if s == "omni" || s == "all" {
            return Some(Self::Omni);
        }
        let n: u8 = s.parse().ok()?;
        (1..=16).contains(&n).then(|| Self::Only(n - 1))
    }

    /// Whether a message with this status byte passes the filter.
    /// Only channel-voice messages (`0x80`..=`0xEF`) carry a channel;
    /// system messages (clock, sysex, …) always pass.
    #[must_use]
    fn accepts(self, status: u8) -> bool {
        match self {
            Self::Omni => true,
            Self::Only(c) => !(0x80..=0xEF).contains(&status) || (status & 0x0F) == c,
        }
    }

    /// Pack into the live-channel atomic. `Only(0..=15)` stores the
    /// channel; `Omni` uses the `0x10` sentinel (out of channel
    /// range) so a zeroed atomic is *not* mistaken for omni.
    #[must_use]
    fn encode(self) -> u8 {
        match self {
            Self::Omni => OMNI,
            Self::Only(c) => c,
        }
    }

    /// Inverse of [`Self::encode`]; anything outside `0..16` is omni.
    #[must_use]
    fn decode(v: u8) -> Self {
        if v < 16 { Self::Only(v) } else { Self::Omni }
    }
}

/// `live_channel` sentinel meaning "accept every channel".
const OMNI: u8 = 0x10;

/// MIDI input selection resolved from the CLI, read once when the
/// input thread starts.
#[derive(Clone, Debug, Default)]
pub struct MidiConfig {
    /// Input port name (substring, case-insensitive). `None` → open
    /// every visible port.
    pub input: Option<String>,
    /// Channel filter applied to incoming channel-voice messages.
    pub channel: MidiChannel,
}

static CONFIG: OnceLock<MidiConfig> = OnceLock::new();

/// Live channel filter, encoded per [`MidiChannel::encode`]. Read in
/// the midir callback for every message and written by the macOS
/// "MIDI Channel" submenu, so the filter changes without reopening
/// ports. Seeded from the CLI in [`set_config`].
static LIVE_CHANNEL: AtomicU8 = AtomicU8::new(OMNI);

/// Install the process-wide MIDI config. Called once from the CLI;
/// ignored if called twice. Also seeds the live channel filter.
pub fn set_config(config: MidiConfig) {
    LIVE_CHANNEL.store(config.channel.encode(), Ordering::Relaxed);
    let _ = CONFIG.set(config);
}

/// The installed MIDI config, or the default (all ports, omni).
#[must_use]
pub fn config() -> MidiConfig {
    CONFIG.get().cloned().unwrap_or_default()
}

/// The current (live) channel filter applied to incoming messages.
#[must_use]
pub fn live_channel() -> MidiChannel {
    MidiChannel::decode(LIVE_CHANNEL.load(Ordering::Relaxed))
}

/// Set the live channel filter. Takes effect on the next incoming
/// message; used by the macOS "MIDI Channel" menu.
pub fn set_live_channel(channel: MidiChannel) {
    LIVE_CHANNEL.store(channel.encode(), Ordering::Relaxed);
}

/// Print available MIDI input ports, then return. Backs
/// `--list-midi`.
pub fn list_midi() {
    println!("MIDI inputs:");
    let names = list_midi_devices();
    if names.is_empty() {
        println!("  (none)");
    }
    for name in names {
        println!("  {name}");
    }
}

/// MIDI input port names in midir order. Empty if MIDI is
/// unavailable.
#[must_use]
pub fn list_midi_devices() -> Vec<String> {
    let Ok(input) = MidiInput::new("truce-rack-standalone-enum") else {
        return Vec::new();
    };
    input
        .ports()
        .iter()
        .filter_map(|p| input.port_name(p).ok())
        .collect()
}

/// Holds the open midir connections so they outlive the audio
/// stream. Drop closes all ports.
pub struct MidiInputThread {
    // midir's connections are `Send` and own their callback closure;
    // dropping disconnects.
    _connections: Vec<MidiInputConnection<()>>,
}

impl MidiInputThread {
    /// Open MIDI input ports per the installed [`config`] — every
    /// visible port, or only those matching `--midi-input`. Returns
    /// `None` if midir can't initialize, no ports are present, or
    /// none match — the caller keeps going without hardware MIDI.
    #[must_use]
    pub fn start() -> Option<Self> {
        Self::open(config().input.as_deref())
    }

    /// Open every port whose name contains `input` (or every port
    /// when `input` is `None`). The channel filter is read live from
    /// [`live_channel`] in each callback, so changing the channel
    /// needs no reopen — only a device change rebuilds connections.
    #[must_use]
    pub fn open(input: Option<&str>) -> Option<Self> {
        let probe = match MidiInput::new("truce-rack-standalone") {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[truce-rack-standalone] midi init: {e}");
                return None;
            }
        };
        let mut ports = probe.ports();
        if let Some(want) = input {
            let needle = want.to_ascii_lowercase();
            ports.retain(|p| {
                probe
                    .port_name(p)
                    .is_ok_and(|n| n.to_ascii_lowercase().contains(&needle))
            });
            if ports.is_empty() {
                eprintln!("[truce-rack-standalone] no MIDI input matching {want:?}");
                return None;
            }
        }
        if ports.is_empty() {
            return None;
        }

        let mut connections = Vec::with_capacity(ports.len());
        for port in ports {
            // Each connection consumes its `MidiInput`, so we open a
            // fresh one per port. The label is for the system port
            // browser (e.g. macOS Audio MIDI Setup).
            let input = match MidiInput::new("truce-rack-standalone") {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("[truce-rack-standalone] midi reinit: {e}");
                    continue;
                }
            };
            let label = input.port_name(&port).unwrap_or_else(|_| "?".into());
            match open_port(input, &port) {
                Ok(conn) => {
                    eprintln!("[truce-rack-standalone] midi in: {label}");
                    connections.push(conn);
                }
                Err(e) => eprintln!("[truce-rack-standalone] midi open '{label}': {e}"),
            }
        }
        if connections.is_empty() {
            None
        } else {
            Some(Self {
                _connections: connections,
            })
        }
    }
}

/// Live handle for the macOS menu to switch the MIDI input device
/// and channel filter at runtime. Holds the open connections so they
/// outlive the window; rebuilds them on a device change.
pub struct MidiController {
    thread: Option<MidiInputThread>,
    /// Current input-device substring (`None` = all ports).
    input: Option<String>,
}

impl MidiController {
    /// Open MIDI input per the CLI config and return a controller.
    #[must_use]
    pub fn start() -> Self {
        let input = config().input;
        Self {
            thread: MidiInputThread::open(input.as_deref()),
            input,
        }
    }

    /// The current input-device filter (`None` = all ports).
    #[must_use]
    pub fn input(&self) -> Option<&str> {
        self.input.as_deref()
    }

    /// Switch the MIDI input device by substring (`None` = all
    /// ports), reopening connections. The channel filter is
    /// unaffected — it lives in [`live_channel`].
    pub fn set_input(&mut self, input: Option<String>) {
        // Drop the old connections before opening new ones.
        self.thread = None;
        self.thread = MidiInputThread::open(input.as_deref());
        self.input = input;
    }

    /// Set the live channel filter (no reopen needed).
    pub fn set_channel(&self, channel: MidiChannel) {
        set_live_channel(channel);
    }
}

fn open_port(
    input: MidiInput,
    port: &MidiInputPort,
) -> std::result::Result<MidiInputConnection<()>, midir::ConnectError<MidiInput>> {
    input.connect(
        port,
        "truce-rack-standalone-in",
        |_timestamp_us, bytes, ()| {
            // Drop channel-voice messages on filtered-out channels
            // before they reach the queue. Read live so the menu's
            // channel choice applies without reopening the port.
            if bytes
                .first()
                .is_some_and(|&status| !live_channel().accepts(status))
            {
                return;
            }
            if let Some(body) = parse_midi(bytes) {
                midi_queue::enqueue(body);
            }
        },
        (),
    )
}

/// Decode one channel-voice / system message from the wire bytes
/// midir hands us. Anything we don't recognise (sysex, real-time,
/// unknown status) gets stuffed into [`MidiData::Raw`] up to the
/// 8-byte cap; longer messages are dropped.
fn parse_midi(bytes: &[u8]) -> Option<EventBody> {
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
            // The arm guard caps `bytes.len()` at 8, so the cast is
            // lossless. `try_from` would force an unwrap that's
            // dead code by construction.
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

#[cfg(test)]
mod tests {
    use super::MidiChannel;

    #[test]
    fn parse_channels() {
        assert_eq!(MidiChannel::parse("omni"), Some(MidiChannel::Omni));
        assert_eq!(MidiChannel::parse("all"), Some(MidiChannel::Omni));
        assert_eq!(MidiChannel::parse("1"), Some(MidiChannel::Only(0)));
        assert_eq!(MidiChannel::parse("16"), Some(MidiChannel::Only(15)));
        // Out of range / garbage.
        assert_eq!(MidiChannel::parse("0"), None);
        assert_eq!(MidiChannel::parse("17"), None);
        assert_eq!(MidiChannel::parse("ch1"), None);
    }

    #[test]
    fn accepts_filters_channel_voice_only() {
        let only_3 = MidiChannel::Only(2); // 1-based channel 3
        // Note-on (0x90) on channel 3 passes; channel 1 is dropped.
        assert!(only_3.accepts(0x92));
        assert!(!only_3.accepts(0x90));
        // System messages (clock 0xF8, sysex 0xF0) always pass.
        assert!(only_3.accepts(0xF8));
        assert!(only_3.accepts(0xF0));
        // Omni accepts everything.
        assert!(MidiChannel::Omni.accepts(0x90));
        assert!(MidiChannel::Omni.accepts(0xF8));
    }

    #[test]
    fn encode_decode_roundtrips() {
        assert_eq!(MidiChannel::decode(MidiChannel::Omni.encode()), MidiChannel::Omni);
        for c in 0u8..16 {
            let ch = MidiChannel::Only(c);
            assert_eq!(MidiChannel::decode(ch.encode()), ch);
        }
    }

    #[test]
    fn live_channel_set_get() {
        super::set_live_channel(MidiChannel::Only(4));
        assert_eq!(super::live_channel(), MidiChannel::Only(4));
        super::set_live_channel(MidiChannel::Omni);
        assert_eq!(super::live_channel(), MidiChannel::Omni);
    }

    #[test]
    fn controller_tracks_input_selection() {
        // No live MIDI hardware is needed: a non-matching name opens
        // zero ports, but the controller still records the selection
        // the menu would show as checked.
        let mut controller = super::MidiController::start();
        controller.set_input(Some("nonexistent-port".into()));
        assert_eq!(controller.input(), Some("nonexistent-port"));
        controller.set_input(None);
        assert_eq!(controller.input(), None);
    }
}
