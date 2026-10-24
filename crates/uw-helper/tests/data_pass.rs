use uw_helper::data_pass::{Owed, Woke};

/// Catches the data thread reading the wrong region on a wake (design 2b flow step 5: a bell
/// reads its own region, and a timeout reads both, since a signal can be missed while a lock
/// attempt times out).
#[test]
fn a_bell_reads_its_own_region_and_a_timeout_reads_both() {
    let owed = Owed::default();
    assert_eq!(owed.reads(Woke::InfoBell), (true, false));
    assert_eq!(owed.reads(Woke::EventsBell), (false, true));
    assert_eq!(owed.reads(Woke::Timeout), (true, true));
}

/// Catches a read skipped on a busy lock waiting for its own bell or a quiet second (design 2b
/// flow step 5: "skips and retries on the next pass"): with a producer that commits info all the
/// time, a skipped drain would otherwise wait for the next event, and a last event would sit in
/// the ring.
#[test]
fn a_read_a_busy_lock_skipped_is_retried_on_the_next_wake_of_any_kind() {
    let events_owed = Owed {
        info: false,
        events: true,
    };
    assert_eq!(events_owed.reads(Woke::InfoBell), (true, true));
    let info_owed = Owed {
        info: true,
        events: false,
    };
    assert_eq!(info_owed.reads(Woke::EventsBell), (true, true));
    let both = Owed {
        info: true,
        events: true,
    };
    assert_eq!(both.reads(Woke::InfoBell), (true, true));
}
