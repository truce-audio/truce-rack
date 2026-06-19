//! Windowed standalone host.
//!
//! Opens a baseview window, embeds the plugin's own editor as a
//! child of it, and pumps audio through cpal in the background.
//! Keystrokes inside the window translate to MIDI events the audio
//! callback drains every block. Cmd/Ctrl-S writes plugin state to
//! `~/<plugin-slug>.state`; Cmd/Ctrl-O reads it back.
//!
//! The plugin is shared between the UI and audio threads through
//! an `Arc<Mutex<_>>`. The UI thread holds the lock while it has a
//! borrow on the editor; the audio callback takes it for the
//! duration of each `process` call. cpal's stream sits on its own
//! worker thread, so the only contention is the audio block edge.

use std::sync::{Arc, Mutex};

use baseview::{
    Event, EventStatus, Size, Window, WindowHandler, WindowOpenOptions, WindowScalePolicy,
};
// Initial baseview window size before the editor opens. AU and VST3
// both report `editor.size() == None` until the editor's view exists,
// so we have to pick *something*; the real plugin size is applied via
// `Window::resize` once `editor.open()` returns.
const INITIAL_WINDOW: (u32, u32) = (320, 240);
use keyboard_types::{Code, KeyState, Modifiers};
use raw_window_handle::{HasRawWindowHandle, RawWindowHandle as RwhHandle};

use truce_rack_core::buffer::{AudioBuffer, BusRange};
use truce_rack_core::bus::BusLayout;
use truce_rack_core::editor::WindowHandle as PluginParent;
use truce_rack_core::error::{Error, Result};
use truce_rack_core::events::{EventBody, EventList, MidiData};
use truce_rack_core::plugin::{Plugin, PluginCore, ProcessContext};

use crate::keyboard;
use crate::midi_queue;

const MAX_BLOCK: usize = 1024;

/// Open a baseview window, embed `plugin`'s editor inside it, and
/// drive cpal in the background. Blocks until the window closes.
///
/// # Errors
/// Propagates activate / device errors. If the plugin reports no
/// editor we fall back to the headless [`crate::run_with_plugin`]
/// runner.
///
/// # Panics
/// Locks an `Arc<Mutex<P>>` we just created — `expect` only fires
/// if the mutex was somehow already poisoned, which is impossible
/// on a fresh allocation.
pub fn run<P>(plugin: P) -> Result<()>
where
    P: PluginCore + Plugin<f32> + Send + 'static,
{
    let plugin = Arc::new(Mutex::new(plugin));
    let plugin_name = plugin.lock().expect("fresh mutex").info().name.clone();

    // Probe editor presence — if the plugin can't open an editor
    // we bail to headless. We can't read the real editor size yet
    // (AU / VST3 require the editor be opened in a parent window
    // first), so the baseview window starts at a small default and
    // is resized once the editor reports its true frame.
    {
        let mut guard = plugin.lock().expect("fresh mutex");
        if guard.editor().is_none() {
            drop(guard);
            eprintln!(
                "[truce-rack-standalone] plugin '{plugin_name}' has no editor — running headless"
            );
            return match Arc::try_unwrap(plugin) {
                Ok(mutex) => crate::run_with_plugin(
                    mutex
                        .into_inner()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                    crate::RunMode::UntilSignal,
                ),
                Err(_) => Err(Error::Other(
                    "could not unwrap plugin Arc for headless fallback".into(),
                )),
            };
        }
    }
    let initial_size = INITIAL_WINDOW;

    // Start the audio stream first so the editor sees a live host
    // from the get-go. Owns the underlying cpal::Stream; we drop
    // it explicitly after the window closes.
    let stream_owner = StreamOwner::start(&plugin)?;

    // Open every visible hardware MIDI input. None means no ports
    // available or midir failed to initialise — the runner is still
    // playable from the QWERTY keyboard. Held for the lifetime of
    // the window; dropped after we close.
    let _midi_in = crate::midi::MidiInputThread::start();

    let window_opts = WindowOpenOptions {
        title: plugin_name.clone(),
        size: Size::new(f64::from(initial_size.0), f64::from(initial_size.1)),
        scale: WindowScalePolicy::SystemScaleFactor,
    };

    let plugin_for_handler = Arc::clone(&plugin);
    let plugin_name_for_handler = plugin_name.clone();

    Window::open_blocking(window_opts, move |window| {
        let parent = raw_handle_to_plugin_handle(window.raw_window_handle());

        // Install the macOS native menu bar — App + File menu.
        // Has to run on the main thread after baseview has wired up
        // NSApp, which it does as part of opening the window. The
        // closure builder runs there before the event loop starts.
        #[cfg(all(target_os = "macos", feature = "gui"))]
        crate::menu_macos::install(&plugin_name_for_handler);

        // Open editor under the lock. After it opens, ask it for its
        // real size and resize the baseview window to fit — we had
        // to pick an arbitrary `INITIAL_WINDOW` above because AU /
        // VST3 don't report a size until the editor view exists.
        // Drop the guard before handing the handler back to baseview
        // so the audio thread isn't blocked.
        let mut editor_size: Option<(u32, u32)> = None;
        {
            let mut guard = plugin_for_handler.lock().expect("plugin mutex");
            if let Some(editor) = guard.editor() {
                if let Err(e) = editor.open(parent, 1.0) {
                    eprintln!("[truce-rack-standalone] editor.open failed: {e}");
                }
                editor.show();
                editor_size = editor.size();
            }
        }
        if let Some((w, h)) = editor_size {
            window.resize(Size::new(f64::from(w), f64::from(h)));
            // The plugin's view stays at the position / size it was
            // added to the (still INITIAL_WINDOW-sized) parent. After
            // the parent resizes, push the view's frame back to
            // (0, 0, w, h) so it fills the new bounds — otherwise
            // shrunk parents leave the view in its old corner with
            // empty space, and grown parents leave the view too small.
            let mut guard = plugin_for_handler.lock().expect("plugin mutex");
            if let Some(editor) = guard.editor() {
                editor.set_size(w, h);
            }
        }

        StandaloneHandler {
            plugin: Arc::clone(&plugin_for_handler),
            plugin_name: plugin_name_for_handler,
            octave_offset: 0,
        }
    });

    // Window closed — close the editor before we drop the audio
    // stream so the editor sees the parent window tear down in the
    // right order.
    {
        let mut guard = plugin.lock().expect("plugin mutex");
        if let Some(editor) = guard.editor() {
            editor.close();
        }
    }
    drop(stream_owner);
    Ok(())
}

fn raw_handle_to_plugin_handle(handle: RwhHandle) -> PluginParent {
    match handle {
        RwhHandle::AppKit(h) => PluginParent::NSView(h.ns_view),
        RwhHandle::Win32(h) => PluginParent::HWND(h.hwnd),
        // `h.window` is `c_ulong` — u64 on 64-bit Linux, u32 on the
        // (theoretical) Windows path here. Widen explicitly so the
        // match arm type-checks on every platform.
        #[allow(clippy::useless_conversion)]
        RwhHandle::Xlib(h) => PluginParent::X11(h.window.into()),
        _ => panic!("[truce-rack-standalone] unsupported raw-window-handle variant"),
    }
}

struct StandaloneHandler<P>
where
    P: PluginCore + Plugin<f32> + Send + 'static,
{
    plugin: Arc<Mutex<P>>,
    plugin_name: String,
    octave_offset: i8,
}

impl<P> WindowHandler for StandaloneHandler<P>
where
    P: PluginCore + Plugin<f32> + Send + 'static,
{
    fn on_frame(&mut self, _window: &mut Window) {
        // Drive the editor's per-frame idle hook. LV2 uses this to
        // tick its `ui:idleInterface`, push host→UI parameter updates,
        // and animate; other formats no-op via the trait default.
        if let Ok(mut guard) = self.plugin.try_lock()
            && let Some(editor) = guard.editor()
        {
            editor.on_idle();
        }
    }

    fn on_event(&mut self, _window: &mut Window, event: Event) -> EventStatus {
        match event {
            Event::Keyboard(kb) => self.handle_keyboard(&kb),
            _ => EventStatus::Ignored,
        }
    }
}

impl<P> StandaloneHandler<P>
where
    P: PluginCore + Plugin<f32> + Send + 'static,
{
    fn handle_keyboard(&mut self, kb: &keyboard_types::KeyboardEvent) -> EventStatus {
        // Cmd/Ctrl + S → save state.
        if kb.state == KeyState::Down && kb.code == Code::KeyS && is_mod_pressed(kb.modifiers) {
            self.save_state();
            return EventStatus::Captured;
        }

        // Cmd/Ctrl + O → load state.
        if kb.state == KeyState::Down && kb.code == Code::KeyO && is_mod_pressed(kb.modifiers) {
            self.load_state();
            return EventStatus::Captured;
        }

        // SPACE → transport toggle (placeholder — log only).
        if kb.state == KeyState::Down && kb.code == Code::Space {
            eprintln!("[truce-rack-standalone] transport: toggle (placeholder)");
            return EventStatus::Captured;
        }

        // Z / X → octave shift (on key-down only; ignore repeats).
        if kb.state == KeyState::Down
            && let Some(shift) = keyboard::code_to_octave_shift(kb.code)
        {
            self.octave_offset = (self.octave_offset + shift).clamp(-3, 3);
            return EventStatus::Captured;
        }

        // QWERTY note row → MIDI note on/off pushed straight into
        // the plugin under the lock. Audio thread takes the same
        // lock once per block, so worst-case the keystroke is
        // observed at the next block boundary.
        if let Some(note) = keyboard::code_to_midi_note(kb.code, self.octave_offset) {
            let body = match kb.state {
                KeyState::Down => EventBody::Midi(MidiData::NoteOn {
                    channel: 0,
                    note,
                    velocity: 102,
                }),
                KeyState::Up => EventBody::Midi(MidiData::NoteOff {
                    channel: 0,
                    note,
                    velocity: 0,
                }),
            };
            midi_queue::enqueue(body);
            return EventStatus::Captured;
        }

        EventStatus::Ignored
    }

    fn save_state(&self) {
        let path = state_path(&self.plugin_name);
        let Ok(guard) = self.plugin.lock() else {
            eprintln!("[truce-rack-standalone] could not lock plugin to save state");
            return;
        };
        match guard.save_state() {
            Ok(blob) => match std::fs::write(&path, &blob) {
                Ok(()) => eprintln!(
                    "[truce-rack-standalone] state saved: {} ({} bytes)",
                    path.display(),
                    blob.len()
                ),
                Err(e) => eprintln!("[truce-rack-standalone] write {}: {e}", path.display()),
            },
            Err(e) => eprintln!("[truce-rack-standalone] save_state failed: {e}"),
        }
    }

    fn load_state(&self) {
        let path = state_path(&self.plugin_name);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[truce-rack-standalone] read {}: {e}", path.display());
                return;
            }
        };
        let Ok(mut guard) = self.plugin.lock() else {
            eprintln!("[truce-rack-standalone] could not lock plugin to load state");
            return;
        };
        match guard.load_state(&bytes) {
            Ok(()) => eprintln!(
                "[truce-rack-standalone] state loaded: {} ({} bytes)",
                path.display(),
                bytes.len()
            ),
            Err(e) => eprintln!("[truce-rack-standalone] load_state failed: {e}"),
        }
    }
}

fn state_path(plugin_name: &str) -> std::path::PathBuf {
    let slug: String = plugin_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join(format!("{slug}.state"))
}

fn is_mod_pressed(mods: Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        mods.contains(Modifiers::META)
    } else {
        mods.contains(Modifiers::CONTROL)
    }
}

/// Owns the cpal output stream. Drop closes the stream.
struct StreamOwner {
    _stream: cpal::Stream,
}

impl StreamOwner {
    fn start<P>(plugin: &Arc<Mutex<P>>) -> Result<Self>
    where
        P: PluginCore + Plugin<f32> + Send + 'static,
    {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| Error::Other("no default output device".into()))?;
        let config = device
            .default_output_config()
            .map_err(|e| Error::Other(format!("default_output_config: {e}")))?;
        let sample_rate = f64::from(config.sample_rate().0);
        let channels = usize::from(config.channels().max(1));

        // Activate under the lock; the audio callback also takes
        // the same lock every block, so the order is: activate →
        // build stream → play.
        {
            let mut guard = plugin.lock().expect("plugin mutex");
            guard.activate(BusLayout::stereo(), sample_rate, MAX_BLOCK)?;
        }

        let stream_config: cpal::StreamConfig = config.config();
        let bus_in = vec![BusRange::new(0, channels)];
        let bus_out = vec![BusRange::new(0, channels)];

        let mut input_buf = vec![vec![0.0f32; MAX_BLOCK]; channels];
        let mut output_buf = vec![vec![0.0f32; MAX_BLOCK]; channels];
        let mut clock = crate::transport::TransportClock::new();
        let plugin_cb = Arc::clone(plugin);

        let stream = device
            .build_output_stream(
                &stream_config,
                move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let frames = out.len() / channels.max(1);

                    for ch in &mut input_buf {
                        if ch.len() < frames {
                            ch.resize(frames, 0.0);
                        }
                        for v in &mut ch[..frames] {
                            *v = 0.0;
                        }
                    }
                    for ch in &mut output_buf {
                        if ch.len() < frames {
                            ch.resize(frames, 0.0);
                        }
                        for v in &mut ch[..frames] {
                            *v = 0.0;
                        }
                    }

                    let mut events = EventList::default();
                    midi_queue::drain_into(&mut events);

                    if let Ok(mut guard) = plugin_cb.try_lock() {
                        let inputs: Vec<&[f32]> = input_buf.iter().map(|c| &c[..frames]).collect();
                        let mut outputs: Vec<&mut [f32]> =
                            output_buf.iter_mut().map(|c| &mut c[..frames]).collect();
                        let mut buffer =
                            AudioBuffer::new(&inputs, &mut outputs, frames, &bus_in, &bus_out);
                        let mut out_events = EventList::default();
                        let mut ctx = ProcessContext {
                            sample_rate,
                            max_block_size: MAX_BLOCK,
                            transport: clock.next_block(frames, sample_rate),
                            output_events: &mut out_events,
                        };
                        let _ = guard.process(&mut buffer, &events, &mut ctx);
                    }
                    // If the UI thread held the lock this block we
                    // emit silence — preferable to glitching from a
                    // half-processed block.

                    for frame in 0..frames {
                        for ch in 0..channels {
                            out[frame * channels + ch] = output_buf[ch][frame];
                        }
                    }
                },
                move |err| eprintln!("[truce-rack-standalone] stream error: {err}"),
                None,
            )
            .map_err(|e| Error::Other(format!("build_output_stream: {e}")))?;
        stream
            .play()
            .map_err(|e| Error::Other(format!("stream.play: {e}")))?;
        Ok(Self { _stream: stream })
    }
}
