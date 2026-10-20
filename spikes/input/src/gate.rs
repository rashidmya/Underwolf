//! The edit-mode input gate: the game-independent core of candidate C3.
//!
//! A Win32 input hook (see [`crate::capture_win32_hook`]) feeds every raw input
//! event the game would receive into [`InputGate::step`]. The gate decides what
//! the game should actually see and what to forward to the overlay host, and it
//! owns the edit-mode transitions.
//!
//! The hard requirement it exists to satisfy is Review Focus 3: Ctrl is crouch
//! in Deadlock, so entering/leaving edit mode with Ctrl+Tab must never leave a
//! key stuck down in the game's view. The gate guarantees this by releasing
//! every key/button the game believes is held at the moment edit mode is
//! entered, and by passing nothing to the game while edit mode is on — so on
//! exit the game's input state is already clean.

use uw_spike_ipc::InputEvent;

/// Windows virtual-key codes the gate reasons about directly.
pub mod vk {
    pub const TAB: u32 = 0x09;
    pub const CONTROL: u32 = 0x11;
    pub const LCONTROL: u32 = 0xA2;
    pub const RCONTROL: u32 = 0xA3;
}

/// A raw input event as seen by the in-process hook, before the gate decides its
/// fate. Mouse coordinates are in window pixels.
#[derive(Debug, Clone, PartialEq)]
pub enum RawInput {
    Key { code: u32, down: bool, text: Option<String> },
    MouseMove { x: f32, y: f32 },
    MouseButton { button: u8, down: bool },
    Wheel { dx: f32, dy: f32 },
}

/// What the hook should hand to the game as a result of one raw event.
#[derive(Debug, Clone, PartialEq)]
pub enum GameAction {
    /// Deliver this event to the game unchanged.
    Pass(RawInput),
    /// Synthesize a key-up to the game so the key does not stick down.
    SynthKeyUp(u32),
    /// Synthesize a mouse-button-up to the game so the button does not stick.
    SynthButtonUp(u8),
}

/// The outcome of feeding one raw event through the gate.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Step {
    /// Actions the hook applies to the game, in order.
    pub to_game: Vec<GameAction>,
    /// Events forwarded to the overlay host, in order.
    pub to_host: Vec<InputEvent>,
    /// `Some(true)` if this event entered edit mode, `Some(false)` if it left.
    pub edit_mode_changed: Option<bool>,
}

#[derive(Default)]
pub struct InputGate {
    edit_mode: bool,
    ctrl_down: bool,
    tab_down: bool,
    held_keys: Vec<u32>,
    held_buttons: Vec<u8>,
}

impl InputGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn edit_mode(&self) -> bool {
        self.edit_mode
    }

    /// Keys the game currently believes are held down.
    pub fn game_held_keys(&self) -> &[u32] {
        &self.held_keys
    }

    /// Mouse buttons the game currently believes are held down.
    pub fn game_held_buttons(&self) -> &[u8] {
        &self.held_buttons
    }

    pub fn step(&mut self, input: RawInput) -> Step {
        // Track the physical Ctrl and Tab state in every mode so the chord works
        // whether or not edit mode is already on.
        let mut tab_rising = false;
        if let RawInput::Key { code, down, .. } = &input {
            if is_ctrl(*code) {
                self.ctrl_down = *down;
            } else if *code == vk::TAB {
                // Only the rising edge counts; Win32 repeats WM_KEYDOWN while a
                // key is held, which must not toggle edit mode over and over.
                tab_rising = *down && !self.tab_down;
                self.tab_down = *down;
            }
        }

        // Ctrl+Tab toggles edit mode. The triggering Tab never reaches the game.
        if tab_rising && self.ctrl_down {
            return self.toggle_edit_mode();
        }

        if self.edit_mode {
            self.step_edit_mode(input)
        } else {
            self.step_passthrough(input)
        }
    }

    fn toggle_edit_mode(&mut self) -> Step {
        if self.edit_mode {
            // Exit. The game has believed nothing is held since we released
            // everything on entry and passed nothing through while editing, so
            // there is nothing to clean up — the hero is not crouched and no
            // movement key repeats. The exit chord itself is swallowed.
            self.edit_mode = false;
            Step { to_game: Vec::new(), to_host: Vec::new(), edit_mode_changed: Some(false) }
        } else {
            // Enter. Release everything the game currently holds so nothing
            // sticks down while the player edits the layout.
            let mut to_game = Vec::new();
            for &code in &self.held_keys {
                to_game.push(GameAction::SynthKeyUp(code));
            }
            for &button in &self.held_buttons {
                to_game.push(GameAction::SynthButtonUp(button));
            }
            self.held_keys.clear();
            self.held_buttons.clear();
            self.edit_mode = true;
            Step { to_game, to_host: Vec::new(), edit_mode_changed: Some(true) }
        }
    }

    fn step_passthrough(&mut self, input: RawInput) -> Step {
        match &input {
            RawInput::Key { code, down, .. } => {
                if *down {
                    if !self.held_keys.contains(code) {
                        self.held_keys.push(*code);
                    }
                } else {
                    self.held_keys.retain(|c| c != code);
                }
            }
            RawInput::MouseButton { button, down } => {
                if *down {
                    if !self.held_buttons.contains(button) {
                        self.held_buttons.push(*button);
                    }
                } else {
                    self.held_buttons.retain(|b| b != button);
                }
            }
            _ => {}
        }
        Step { to_game: vec![GameAction::Pass(input)], to_host: Vec::new(), edit_mode_changed: None }
    }

    fn step_edit_mode(&mut self, input: RawInput) -> Step {
        let event = match input {
            RawInput::Key { code, down, text } => InputEvent::Key { code, down, text },
            RawInput::MouseMove { x, y } => InputEvent::MouseMove { x, y },
            RawInput::MouseButton { button, down } => InputEvent::MouseButton { button, down },
            RawInput::Wheel { dx, dy } => InputEvent::Wheel { dx, dy },
        };
        Step { to_game: Vec::new(), to_host: vec![event], edit_mode_changed: None }
    }
}

fn is_ctrl(code: u32) -> bool {
    code == vk::CONTROL || code == vk::LCONTROL || code == vk::RCONTROL
}
