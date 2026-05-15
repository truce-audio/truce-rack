//! Sample-accurate event list shared between MIDI / parameter
//! automation / transport flags.
//!
//! An [`EventList`] arrives sorted by `sample_offset` and gets
//! consumed by the plugin during one block of `process`. The
//! output `EventList` on [`crate::ProcessContext`] is the
//! plugin's path back to the host for outbound MIDI and
//! parameter touches.

use smallvec::SmallVec;

/// One event with sample-accurate timing.
#[derive(Debug, Clone, Copy)]
pub struct Event {
    /// Sample offset within the current `process` block.
    pub sample_offset: u32,
    /// Event payload.
    pub body: EventBody,
}

/// What this event carries.
#[derive(Debug, Clone, Copy)]
pub enum EventBody {
    /// MIDI 1.0 / 2.0 message.
    Midi(MidiData),
    /// Host-driven parameter automation point.
    ParamValue {
        /// Parameter id from [`crate::ParameterInfo::id`].
        param_id: u32,
        /// New value in the parameter's native range.
        value: f64,
    },
    /// Plugin-emitted "user touched this parameter" notification.
    /// Hosts use the touch / release pair to delimit a gesture
    /// for undo grouping and automation.
    ParamGesture {
        /// Parameter id.
        param_id: u32,
        /// `true` = begin gesture, `false` = end.
        active: bool,
    },
    /// Host transport state changed mid-block (e.g. user hit
    /// play between samples 256 and 257). Plugins that care
    /// about exact transport flip points read these out of the
    /// input event list.
    TransportFlag(TransportFlag),
}

/// Sub-flags describing transport state transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportFlag {
    /// Playback started.
    PlayStart,
    /// Playback stopped.
    PlayStop,
    /// Recording armed → engaged.
    RecordStart,
    /// Recording stopped.
    RecordStop,
    /// Loop boundary crossed (host jumped from end to start).
    Looped,
}

/// MIDI message body.
///
/// MIDI 1.0 channel-voice messages are first-class; system
/// real-time and `SysEx` ride in [`MidiData::Raw`] as raw bytes
/// for the rare hosts that care.
#[derive(Debug, Clone, Copy)]
pub enum MidiData {
    /// Note On — velocity 0 is treated as Note Off per the
    /// MIDI 1.0 spec, but we represent that explicitly via
    /// [`MidiData::NoteOff`] when known.
    NoteOn {
        /// MIDI channel, 0-15.
        channel: u8,
        /// Note number, 0-127.
        note: u8,
        /// Velocity, 0-127.
        velocity: u8,
    },
    /// Note Off.
    NoteOff {
        /// MIDI channel, 0-15.
        channel: u8,
        /// Note number, 0-127.
        note: u8,
        /// Release velocity, 0-127.
        velocity: u8,
    },
    /// Polyphonic key pressure.
    PolyAftertouch {
        /// MIDI channel, 0-15.
        channel: u8,
        /// Note number, 0-127.
        note: u8,
        /// Pressure, 0-127.
        pressure: u8,
    },
    /// Control change.
    ControlChange {
        /// MIDI channel, 0-15.
        channel: u8,
        /// Controller number, 0-127.
        controller: u8,
        /// Value, 0-127.
        value: u8,
    },
    /// Program change.
    ProgramChange {
        /// MIDI channel, 0-15.
        channel: u8,
        /// Program number, 0-127.
        program: u8,
    },
    /// Channel pressure.
    ChannelAftertouch {
        /// MIDI channel, 0-15.
        channel: u8,
        /// Pressure, 0-127.
        pressure: u8,
    },
    /// Pitch bend, 14-bit (0-16383, 8192 = center).
    PitchBend {
        /// MIDI channel, 0-15.
        channel: u8,
        /// Bend value, 0-16383.
        value: u16,
    },
    /// Raw MIDI bytes — system real-time, `SysEx` fragments,
    /// anything the channel-voice variants don't cover.
    /// `len` bytes of `data` are meaningful; trailing bytes
    /// are undefined. Cap of 8 covers MIDI 2.0 UMP 64-bit and
    /// most system messages without spilling to the heap.
    Raw {
        /// Number of meaningful bytes in `data`.
        len: u8,
        /// Message bytes, big-endian.
        data: [u8; 8],
    },
}

/// Reasonable inline capacity for the per-block event list.
/// Few hosts produce more than ~16 events per audio block; sizing
/// the inline buffer this way keeps the audio thread out of the
/// allocator for the vast majority of blocks.
const EVENT_LIST_INLINE: usize = 32;

/// Sample-ordered event buffer used for one `process` block.
///
/// Backed by `SmallVec<[Event; 32]>`: 32 inline entries cover
/// almost every block without heap allocation; bursts spill to
/// the heap rather than getting dropped. Cleared between blocks
/// by [`EventList::clear`] (keeps the heap allocation when one
/// was forced).
#[derive(Debug, Default, Clone)]
pub struct EventList {
    events: SmallVec<[Event; EVENT_LIST_INLINE]>,
}

impl EventList {
    /// An empty list with inline capacity for [`EVENT_LIST_INLINE`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from an existing slice.
    #[must_use]
    pub fn from_slice(events: &[Event]) -> Self {
        Self {
            events: SmallVec::from_slice(events),
        }
    }

    /// Append an event. Caller is responsible for keeping the
    /// list sample-offset-sorted.
    pub fn push(&mut self, event: Event) {
        self.events.push(event);
    }

    /// Reset to empty without dropping any heap allocation.
    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Number of events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// `true` when the list contains no events.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Borrow the events as a slice.
    #[must_use]
    pub fn as_slice(&self) -> &[Event] {
        &self.events
    }

    /// Iterate over the events.
    pub fn iter(&self) -> std::slice::Iter<'_, Event> {
        self.events.iter()
    }
}

impl<'a> IntoIterator for &'a EventList {
    type Item = &'a Event;
    type IntoIter = std::slice::Iter<'a, Event>;
    fn into_iter(self) -> Self::IntoIter {
        self.events.iter()
    }
}
