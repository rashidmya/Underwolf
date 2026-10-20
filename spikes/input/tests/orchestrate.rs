use uw_spike_input::capture_win32_hook::{handle_message, wm, HostSink, MessageOutcome};
use uw_spike_input::gate::{GameAction, InputGate};
use uw_spike_ipc::InputEvent;

#[derive(Default)]
struct FakeHost {
    inputs: Vec<InputEvent>,
    edit_changes: Vec<bool>,
}
impl HostSink for FakeHost {
    fn input(&mut self, event: &InputEvent) {
        self.inputs.push(event.clone());
    }
    fn edit_mode_changed(&mut self, on: bool) {
        self.edit_changes.push(on);
    }
}

#[test]
fn passthrough_message_is_not_swallowed_and_host_sees_nothing() {
    let mut gate = InputGate::new();
    let mut host = FakeHost::default();
    let out = handle_message(wm::WM_KEYDOWN, b'W' as usize, 0, &mut gate, &mut host);
    assert_eq!(out, MessageOutcome { swallow: false, synth: vec![] });
    assert!(host.inputs.is_empty());
    assert!(host.edit_changes.is_empty());
}

#[test]
fn entering_edit_mode_swallows_trigger_injects_releases_and_notifies_host() {
    let mut gate = InputGate::new();
    let mut host = FakeHost::default();
    // Player holds W (moving) then Ctrl (crouch), then presses Tab.
    handle_message(wm::WM_KEYDOWN, b'W' as usize, 0, &mut gate, &mut host);
    handle_message(wm::WM_KEYDOWN, 0xA2, 0, &mut gate, &mut host); // VK_LCONTROL
    let out = handle_message(wm::WM_KEYDOWN, 0x09, 0, &mut gate, &mut host); // VK_TAB

    assert!(out.swallow, "the Tab that opens edit mode must not reach the game");
    assert!(out.synth.contains(&GameAction::SynthKeyUp(b'W' as u32)));
    assert!(out.synth.contains(&GameAction::SynthKeyUp(0xA2)));
    assert_eq!(host.edit_changes, vec![true]);
    assert!(host.inputs.is_empty());
}

#[test]
fn edit_mode_message_is_swallowed_and_forwarded_to_host() {
    let mut gate = InputGate::new();
    let mut host = FakeHost::default();
    handle_message(wm::WM_KEYDOWN, 0xA2, 0, &mut gate, &mut host);
    handle_message(wm::WM_KEYDOWN, 0x09, 0, &mut gate, &mut host); // enter
    host.inputs.clear();
    host.edit_changes.clear();

    let out = handle_message(wm::WM_LBUTTONDOWN, 0, 0, &mut gate, &mut host);
    assert_eq!(out, MessageOutcome { swallow: true, synth: vec![] });
    assert_eq!(
        host.inputs,
        vec![InputEvent::MouseButton { button: 0, down: true }]
    );
}
