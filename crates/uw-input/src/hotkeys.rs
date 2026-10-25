//! Allocation-free matching against Policy's real physical key state, before game filtering.
use crate::configuration::PreparedConfig;
use uw_pipe_proto::{input::InputRecord, routing::*};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyMatch {
    pub binding_id: u32,
    pub action: HotkeyAction,
    pub target: Option<WidgetRef>,
    pub passthrough: bool,
}
pub struct HotkeyMatcher {
    bindings: [Option<HotkeySpec>; MAX_HOTKEYS],
    pressed: [bool; 256],
    consumed: [bool; 256],
    last_consumed: bool,
}
impl Default for HotkeyMatcher {
    fn default() -> Self {
        Self { bindings: [None; MAX_HOTKEYS], pressed: [false; 256], consumed: [false; 256], last_consumed: false }
    }
}
impl HotkeyMatcher {
    pub fn reseed(&mut self, physical: &[bool; 256]) {
        self.pressed = *physical;
        for (consumed, held) in self.consumed.iter_mut().zip(physical) {
            *consumed &= *held;
        }
        self.last_consumed = false;
    }
    /// Seed trigger latches from current physical state; a replacement never invents a new edge.
    pub fn replace(&mut self, table: &PreparedConfig, physical: &[bool; 256]) {
        self.bindings = [None; MAX_HOTKEYS];
        for (slot, binding) in self.bindings.iter_mut().zip(&table.config().hotkeys) {
            *slot = Some(*binding);
        }
        self.pressed = *physical;
        for (consumed, held) in self.consumed.iter_mut().zip(physical) {
            *consumed &= *held;
        }
        self.last_consumed = false;
    }
    /// Inspect after `observe` under the same owner/policy guard, including non-activating key-ups.
    pub fn consumes_last(&self) -> bool {
        self.last_consumed
    }
    pub fn observe(&mut self, record: InputRecord, physical: &[bool; 256]) -> Option<HotkeyMatch> {
        self.last_consumed = false;
        if record.kind != 1 || record.validate().is_err() {
            return None;
        }
        let vk = record.a as usize;
        if record.flags & 1 == 0 {
            self.last_consumed = self.consumed[vk];
            self.consumed[vk] = false;
            self.pressed[vk] = false;
            return None;
        }
        if self.pressed[vk] || record.flags & 4 != 0 {
            self.pressed[vk] = true;
            self.last_consumed = self.consumed[vk];
            return None;
        }
        self.pressed[vk] = true;
        let modifiers = physical_modifiers(physical);
        let binding = self
            .bindings
            .iter()
            .flatten()
            .find(|h| h.vk as usize == vk && modifiers_match(h.modifiers, modifiers, h.flags & 2 != 0))?;
        let passthrough = binding.flags & 1 != 0;
        self.consumed[vk] = !passthrough;
        self.last_consumed = !passthrough;
        Some(HotkeyMatch { binding_id: binding.id, action: binding.action, target: binding.target, passthrough })
    }
}
/// The existing Win32 adapter normalizes modifier events to these left/right VK entries.
pub fn physical_modifiers(keys: &[bool; 256]) -> u8 {
    let mut bits = 0;
    for (family, (left, right)) in [(160, 161), (162, 163), (164, 165), (91, 92)].into_iter().enumerate() {
        bits |= (keys[left] as u8) << (family * 2);
        bits |= (keys[right] as u8) << (family * 2 + 1);
    }
    bits
}
