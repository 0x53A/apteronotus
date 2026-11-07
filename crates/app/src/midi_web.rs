//! Browser MIDI permission and input ownership. Only the UI thread touches JS;
//! callbacks pass owned note messages through the same bounded native ingress.

use std::cell::RefCell;
use std::rc::Rc;

use apteronotus_live::MidiMessage;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{MidiAccess, MidiInput, MidiMessageEvent, MidiOptions, MidiPortDeviceState};

use crate::midi_input::{MidiIngress, MidiPort};

pub struct MidiWeb {
    state: Rc<RefCell<State>>,
    ingress: MidiIngress,
}

struct State {
    access: Option<MidiAccess>,
    pending: bool,
    notice: Option<String>,
    active: Option<ActiveInput>,
    generation: u64,
}

struct ActiveInput {
    input: MidiInput,
    // Keep the Rust trampoline alive until its JS handler has been cleared.
    _handler: Closure<dyn FnMut(MidiMessageEvent)>,
}

impl Drop for ActiveInput {
    fn drop(&mut self) {
        self.input.set_onmidimessage(None);
        let promise = self.input.close();
        spawn_local(async move {
            let _ = JsFuture::from(promise).await;
        });
    }
}

impl MidiWeb {
    pub fn new(ingress: MidiIngress) -> Self {
        Self {
            state: Rc::new(RefCell::new(State {
                access: None,
                pending: false,
                notice: Some("Enable MIDI to allow keyboard access in this browser.".into()),
                active: None,
                generation: 0,
            })),
            ingress,
        }
    }

    /// Called directly from the Enable MIDI button, before awaiting anything.
    /// SysEx and output access are never used.
    pub fn request_access(&mut self) {
        let mut state = self.state.borrow_mut();
        if state.access.is_some() || state.pending {
            return;
        }
        let Some(window) = web_sys::window() else {
            state.notice = Some("Web MIDI requires a browser window.".into());
            return;
        };
        let navigator = window.navigator();
        let supported = js_sys::Reflect::get(&navigator, &JsValue::from_str("requestMIDIAccess"))
            .is_ok_and(|value| value.is_function());
        if !supported {
            state.notice = Some(
                "Web MIDI is unavailable. Use a browser with Web MIDI support over HTTPS or localhost."
                    .into(),
            );
            return;
        }
        let options = MidiOptions::new();
        options.set_sysex(false);
        let promise = match navigator.request_midi_access_with_options(&options) {
            Ok(promise) => promise,
            Err(error) => {
                state.notice = Some(format!("MIDI access failed: {}", error_text(error)));
                return;
            }
        };
        state.pending = true;
        state.notice = Some("Waiting for browser MIDI permission…".into());
        drop(state);
        let weak = Rc::downgrade(&self.state);
        spawn_local(async move {
            let result = JsFuture::from(promise).await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            let mut state = state.borrow_mut();
            state.pending = false;
            match result.and_then(|value| value.dyn_into::<MidiAccess>()) {
                Ok(access) => {
                    state.access = Some(access);
                    state.notice = Some("MIDI enabled. Select a keyboard input.".into());
                }
                Err(error) => {
                    state.notice = Some(format!("MIDI permission failed: {}", error_text(error)));
                }
            }
        });
    }

    pub fn ports(&self) -> Vec<MidiPort> {
        let state = self.state.borrow();
        let Some(access) = state.access.as_ref() else {
            return Vec::new();
        };
        let mut ports = Vec::new();
        // Iteration owns JS handles only for this enumeration. Disconnects are
        // filtered out instead of leaving unusable entries in the selector.
        for value in access.inputs().values().into_iter().flatten() {
            let Ok(input) = value.dyn_into::<MidiInput>() else {
                continue;
            };
            if input.state() == MidiPortDeviceState::Connected {
                ports.push(MidiPort {
                    id: input.id(),
                    name: input.name().unwrap_or_else(|| "Unnamed MIDI input".into()),
                });
            }
        }
        ports.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        ports
    }

    pub fn selected(&self) -> Option<String> {
        self.state
            .borrow()
            .active
            .as_ref()
            .map(|active| active.input.id())
    }

    pub fn notice(&self) -> Option<String> {
        self.state.borrow().notice.clone()
    }

    pub fn connect(&mut self, id: Option<&str>) -> Result<(), String> {
        if self.selected().as_deref() == id {
            return Ok(());
        }
        let input = if let Some(id) = id {
            let state = self.state.borrow();
            let access = state.access.as_ref().ok_or("Enable browser MIDI first.")?;
            let input = access
                .inputs()
                .get(id)
                .ok_or("The MIDI input is no longer available.")?;
            if input.state() != MidiPortDeviceState::Connected {
                return Err("The MIDI input is disconnected.".into());
            }
            Some(input)
        } else {
            None
        };
        let mut state = self.state.borrow_mut();
        state.active = None;
        state.generation = state.generation.wrapping_add(1);
        self.ingress
            .send(MidiMessage::AllNotesOff { channel: None });
        let Some(input) = input else {
            state.notice = Some("MIDI input disconnected.".into());
            return Ok(());
        };
        let ingress = self.ingress.clone();
        let handler = Closure::new(move |event: MidiMessageEvent| {
            if let Ok(bytes) = event.data()
                && let Some(message) = MidiMessage::parse(&bytes)
            {
                ingress.send(message);
            }
        });
        input.set_onmidimessage(Some(handler.as_ref().unchecked_ref()));
        let promise = input.open();
        let generation = state.generation;
        state.notice = Some(format!(
            "MIDI input: {}",
            input.name().unwrap_or_else(|| input.id())
        ));
        state.active = Some(ActiveInput {
            input,
            _handler: handler,
        });
        drop(state);
        let weak = Rc::downgrade(&self.state);
        let ingress = self.ingress.clone();
        spawn_local(async move {
            if let Err(error) = JsFuture::from(promise).await {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                let mut state = state.borrow_mut();
                // A late rejection from the previous port cannot close a new one.
                if state.generation == generation {
                    state.active = None;
                    state.notice =
                        Some(format!("MIDI input failed to open: {}", error_text(error)));
                    ingress.send(MidiMessage::AllNotesOff { channel: None });
                }
            }
        });
        Ok(())
    }

    pub fn poll(&mut self) {
        let disconnected = self
            .state
            .borrow()
            .active
            .as_ref()
            .is_some_and(|active| active.input.state() == MidiPortDeviceState::Disconnected);
        if disconnected {
            let _ = self.connect(None);
            self.state.borrow_mut().notice =
                Some("MIDI keyboard was unplugged; held notes released.".into());
        }
    }
}

impl Drop for MidiWeb {
    fn drop(&mut self) {
        self.state.borrow_mut().active = None;
        self.ingress
            .send(MidiMessage::AllNotesOff { channel: None });
    }
}

fn error_text(error: JsValue) -> String {
    if let Some(text) = error.as_string() {
        text
    } else if let Some(error) = error.dyn_ref::<js_sys::Error>() {
        error.message().into()
    } else {
        format!("{error:?}")
    }
}
