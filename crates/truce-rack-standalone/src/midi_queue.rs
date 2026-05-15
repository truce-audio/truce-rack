//! Cross-thread MIDI queue.
//!
//! One static `Mutex<Vec<RackEvent>>` shared by every MIDI producer
//! (QWERTY keyboard inside the windowed handler, midir input ports,
//! anything else added later) and drained by the audio callback once
//! per block. The standalone runs exactly one plugin / one audio
//! stream at a time, so a single global is the simplest correct
//! shape.

use std::sync::Mutex;

use truce_rack_core::events::{Event as RackEvent, EventBody, EventList};

static QUEUE: Mutex<Vec<RackEvent>> = Mutex::new(Vec::new());

/// Push one event body onto the queue at sample offset 0 (delivered
/// at the start of the next audio block).
pub fn enqueue(body: EventBody) {
    if let Ok(mut q) = QUEUE.lock() {
        q.push(RackEvent {
            sample_offset: 0,
            body,
        });
    }
}

/// Drain everything pending into `events`. Called by the audio
/// callback at the top of each block.
pub fn drain_into(events: &mut EventList) {
    if let Ok(mut q) = QUEUE.lock() {
        for ev in q.drain(..) {
            events.push(ev);
        }
    }
}
