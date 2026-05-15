//! QWERTY → MIDI note mapping for the windowed runner.
//!
//! Two rows map to a piano starting at C4 (MIDI 60), matching the
//! spec the truce-rack standalone host inherits from truce-standalone but
//! shifted up an octave so the default keys hit the synth's
//! comfortable range:
//!
//! ```text
//!  Upper: W E   T Y U   O P
//!        C#D#  F#G#A#  C#D#
//! Lower: A S D F G H J K L ;
//!        C D E F G A B C D E
//! ```
//!
//! Matched by physical `keyboard_types::Code` so AZERTY / Dvorak /
//! etc. all land on the same physical keys.

use keyboard_types::Code;

/// Map a physical QWERTY key to a MIDI note number, shifted by
/// `octave_offset` octaves. Returns `None` for keys not on the
/// piano layout.
#[must_use]
pub fn code_to_midi_note(code: Code, octave_offset: i8) -> Option<u8> {
    // C4 = 60 is the default per the task spec.
    let base: i16 = 60 + (i16::from(octave_offset) * 12);

    let offset: i16 = match code {
        // Lower row: white keys C D E F G A B C D E
        Code::KeyA => 0,
        Code::KeyS => 2,
        Code::KeyD => 4,
        Code::KeyF => 5,
        Code::KeyG => 7,
        Code::KeyH => 9,
        Code::KeyJ => 11,
        Code::KeyK => 12,
        Code::KeyL => 14,
        Code::Semicolon => 16,
        // Upper row: black keys
        Code::KeyW => 1,
        Code::KeyE => 3,
        Code::KeyT => 6,
        Code::KeyY => 8,
        Code::KeyU => 10,
        Code::KeyO => 13,
        Code::KeyP => 15,
        _ => return None,
    };

    let note = base + offset;
    if (0..=127).contains(&note) {
        // Bounded above by the guard, so the cast is exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = note as u8;
        Some(n)
    } else {
        None
    }
}

/// Map `Z` / `X` to `-1` / `+1` octave shift, the rest return `None`.
#[must_use]
pub fn code_to_octave_shift(code: Code) -> Option<i8> {
    match code {
        Code::KeyZ => Some(-1),
        Code::KeyX => Some(1),
        _ => None,
    }
}
