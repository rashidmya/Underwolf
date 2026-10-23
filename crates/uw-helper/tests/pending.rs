use uw_helper::pending::Pending;

/// Catches plan 1's final-review M2: after a `status_timeout`, the late `status` must not be taken
/// as the reply to the next request.
#[test]
fn a_late_status_is_discarded_and_the_next_request_gets_its_own() {
    let mut p = Pending::new();
    p.add(1, 15_000);
    assert_eq!(p.expire(14_999), Vec::<u32>::new());
    assert_eq!(p.expire(15_000), vec![1]);
    // Request 1's status arrives late: not pending any more, so discarded.
    assert!(!p.take(1));
    p.add(2, 40_000);
    assert!(!p.take(1), "a second late copy is discarded too");
    assert!(p.take(2));
    assert_eq!(p.next_deadline(), None);
}

/// Catches one request's reply or timeout touching another.
#[test]
fn requests_time_out_and_resolve_independently() {
    let mut p = Pending::new();
    p.add(1, 100);
    p.add(2, 50);
    assert_eq!(p.next_deadline(), Some(50));
    assert_eq!(p.expire(60), vec![2]);
    assert_eq!(p.next_deadline(), Some(100));
    assert!(p.take(1));
    assert!(!p.take(1), "answered once");
    p.add(3, 10);
    p.add(4, 20);
    assert_eq!(p.drain(), vec![3, 4]);
    assert_eq!(p.next_deadline(), None);
}

/// Catches owner decision D1 breaking at the bookkeeping level: a ping past its deadline expires
/// once (one `overlay_not_responding`), and its late `pong` is then discarded, while a ping still
/// in time is answered.
#[test]
fn a_ping_past_its_deadline_expires_once_and_its_late_pong_is_discarded() {
    let mut pings = Pending::new();
    pings.add(10, 15_000);
    pings.add(11, 30_000);
    assert_eq!(pings.expire(15_000), vec![10]);
    assert_eq!(pings.expire(15_001), Vec::<u32>::new(), "expired once");
    assert!(!pings.take(10), "the late pong is discarded");
    assert!(pings.take(11));
}
