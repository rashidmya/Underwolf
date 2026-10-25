use uw_input::native::OwnershipGate;
use uw_pipe_proto::routing::Domains;
#[test]
fn revocation_invalidates_even_an_inactive_publication_ticket() {
    let gate = OwnershipGate::new();
    let ticket = gate.ticket();
    gate.revoke();
    assert!(!gate.publish(ticket, 1, Domains { keyboard: true, mouse: false }, false));
    assert_eq!(gate.snapshot().1, Domains::default());
    assert!(gate.publish(gate.ticket(), 2, Domains { keyboard: false, mouse: true }, false));
    assert_eq!(gate.snapshot(), (2, Domains { keyboard: false, mouse: true }, false));
    let ticket = gate.ticket();
    gate.revoke();
    assert!(!gate.publish(ticket, 2, Domains { keyboard: true, mouse: true }, true));
    assert_eq!(gate.snapshot(), (2, Domains::default(), false));
}
