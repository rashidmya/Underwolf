use uw_spike_gep_channel::Throttle;

#[test]
fn throttle_admits_30_per_window() {
    let window = 200;
    let mut t = Throttle::new(window, 30);
    let t0 = 5_000;
    for i in 0..30 {
        assert!(t.admit(t0), "event {i}");
    }
    assert!(!t.admit(t0));
    assert!(!t.admit(t0 + window - 1));
    assert!(t.admit(t0 + window));
}

#[test]
fn throttle_window_slides() {
    let mut t = Throttle::new(100, 2);
    assert!(t.admit(0));
    assert!(t.admit(60));
    assert!(!t.admit(99));
    assert!(t.admit(100));
    assert!(!t.admit(120));
    assert!(t.admit(160));
}
