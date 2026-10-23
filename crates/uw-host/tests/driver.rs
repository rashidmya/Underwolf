use uw_host::driver::{Desired, Driver};
use uw_host::RequestIds;
use uw_pipe_proto::jsonl::{Command, Event};

const OVERLAY: &str = r"Z:\games\overlay.dll";

fn bootstrap_connected(loaded: bool) -> Event {
    Event::BootstrapConnected { overlay_loaded: loaded, overlay_path: loaded.then(|| OVERLAY.to_string()) }
}

/// Catches a fresh bootstrap left without the overlay, or a status that triggers another load.
#[test]
fn a_fresh_bootstrap_gets_load_overlay_and_a_status_sends_nothing() {
    let mut d = Driver::new(Desired::new(Some(OVERLAY.into())), RequestIds::default());
    assert_eq!(
        d.on_event(&bootstrap_connected(false)),
        vec![Command::LoadOverlay { request_id: 1, path: OVERLAY.into() }]
    );
    let ok = Event::Status { request_id: 1, code: "ok".into(), detail: 0, message: String::new() };
    assert_eq!(d.on_event(&ok), vec![]);
}

/// Catches a reload after a client restart: the pinned bootstrap already runs the overlay.
#[test]
fn a_bootstrap_that_has_the_overlay_gets_no_load() {
    let mut d = Driver::new(Desired::new(Some(OVERLAY.into())), RequestIds::default());
    assert_eq!(d.on_event(&bootstrap_connected(true)), vec![]);
}

#[test]
fn with_no_overlay_nothing_is_loaded() {
    let mut d = Driver::new(Desired::new(None), RequestIds::default());
    assert_eq!(d.on_event(&bootstrap_connected(false)), vec![]);
}

/// Catches an overlay connect without the full state, or with stale values, or reusing an id the
/// user already took.
#[test]
fn every_overlay_connect_gets_the_full_desired_state_with_a_fresh_id() {
    let ids = RequestIds::default();
    let mut d = Driver::new(Desired::new(None), ids.clone());
    let connected = Event::OverlayConnected { module_version: 1 };
    assert_eq!(
        d.on_event(&connected),
        vec![Command::SetState { request_id: 1, edit_mode: Some(false), visible: Some(true) }]
    );
    d.note_command(&Command::SetState { request_id: ids.next(), edit_mode: Some(true), visible: None });
    assert_eq!(
        d.on_event(&connected),
        vec![Command::SetState { request_id: 3, edit_mode: Some(true), visible: Some(true) }]
    );
    d.note_command(&Command::LoadOverlay { request_id: ids.next(), path: r"Z:\b.dll".into() });
    assert_eq!(d.desired(), &Desired { overlay: Some(r"Z:\b.dll".into()), edit_mode: true, visible: true });
}
