//! Hardware MIDI input via midir.
//!
//! Opens every available MIDI input port at startup and forwards
//! channel-voice messages onto the shared [`crate::midi_queue`]. The
//! audio callback drains that queue at the top of each block, so
//! hardware notes flow into the plugin alongside QWERTY-keyboard
//! notes from the windowed handler with no extra wiring.
//!
//! Cross-platform via midir's three backends: CoreMIDI on macOS,
//! WinMM on Windows, ALSA on Linux.

use midir::{MidiInput, MidiInputConnection, MidiInputPort};

use truce_rack_core::events::{EventBody, MidiData};

use crate::midi_queue;

/// Holds the open midir connections so they outlive the audio
/// stream. Drop closes all ports.
pub struct MidiInputThread {
    // midir's connections are `Send` and own their callback closure;
    // dropping disconnects.
    _connections: Vec<MidiInputConnection<()>>,
}

impl MidiInputThread {
    /// Open every visible MIDI input port. Returns `None` if midir
    /// can't initialize (no backend available, missing permission)
    /// or no ports are present — the caller can keep going without
    /// hardware MIDI in either case.
    #[must_use]
    pub fn start() -> Option<Self> {
        let probe = match MidiInput::new("truce-rack-standalone") {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[truce-rack-standalone] midi init: {e}");
                return None;
            }
        };
        let ports = probe.ports();
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

fn open_port(
    input: MidiInput,
    port: &MidiInputPort,
) -> std::result::Result<MidiInputConnection<()>, midir::ConnectError<MidiInput>> {
    input.connect(
        port,
        "truce-rack-standalone-in",
        |_timestamp_us, bytes, ()| {
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
            MidiData::Raw {
                len: bytes.len() as u8,
                data,
            }
        }
        _ => return None,
    };
    Some(EventBody::Midi(body))
}
