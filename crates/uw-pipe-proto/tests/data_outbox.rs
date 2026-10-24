use uw_pipe_proto::data::{
    InfoReader, InfoValue, Outbox, ProduceError, QUEUE_CAP, RING_SIZE, RingReader, Schema,
    SchemaCategory, THROTTLE_MAX, THROTTLE_WINDOW_MS, Throttle, info_init, ring_init,
};

fn outbox() -> (Outbox, Vec<u8>) {
    let s = Schema::new(vec![SchemaCategory::new(
        "test",
        &[("ticks", 4), ("last_seq", 8)],
    )])
    .unwrap();
    let o = Outbox::new(s);
    let mut info = vec![0u8; o.info_layout().size()];
    info_init(&mut info, o.schema(), o.info_layout());
    (o, info)
}

fn ring() -> Vec<u8> {
    let mut r = vec![0u8; RING_SIZE];
    ring_init(&mut r);
    r
}

fn value(key: &str, v: Option<&str>, ts: u64) -> InfoValue {
    InfoValue {
        category: "test".into(),
        key: key.into(),
        value: v.map(str::to_string),
        ts,
    }
}

/// Catches a throttle other than the design's one window (30 events per 200 ms, sliding), or one
/// that admits early.
#[test]
fn the_throttle_admits_30_per_200_ms_sliding() {
    assert_eq!((THROTTLE_MAX, THROTTLE_WINDOW_MS), (30, 200));
    let mut t = Throttle::new(THROTTLE_WINDOW_MS, THROTTLE_MAX);
    for _ in 0..10 {
        assert!(t.admit(1_000));
    }
    for _ in 0..20 {
        assert!(t.admit(1_100));
    }
    assert!(!t.admit(1_100));
    assert!(!t.admit(1_199));
    // The 10 from t=1000 have left the window; the 20 from t=1100 have not.
    for _ in 0..10 {
        assert!(t.admit(1_200));
    }
    assert!(!t.admit(1_250));
    assert!(t.admit(1_300));
}

/// Catches a commit that keeps part of a transaction it refused, or one that accepts an unknown
/// key or a value over its `max_len` (design: errors go to the caller, nothing is truncated).
#[test]
fn a_commit_with_a_bad_entry_is_refused_whole() {
    let (mut o, mut info) = outbox();
    assert_eq!(
        o.commit(
            &[("test", "ticks", Some("1")), ("test", "nope", Some("x"))],
            5
        ),
        Err(ProduceError::UnknownKey {
            category: "test".into(),
            key: "nope".into()
        })
    );
    assert_eq!(
        o.commit(&[("test", "ticks", Some("12345"))], 5),
        Err(ProduceError::ValueTooLong {
            category: "test".into(),
            key: "ticks".into(),
            len: 5,
            max_len: 4
        })
    );
    assert_eq!(
        o.commit(&[("other", "ticks", None)], 5),
        Err(ProduceError::UnknownKey {
            category: "other".into(),
            key: "ticks".into()
        })
    );
    assert!(!o.has_pending_info());
    assert!(!o.write_info(&mut info), "nothing was kept");
    assert_eq!(
        o.commit(&[("test", "ticks", Some("1234"))], 5),
        Ok(()),
        "max_len fits"
    );
}

/// Catches pending keys sharing one timestamp after a busy info lock (Spike 3's merge, which the
/// design replaces: "each pending key keeps its own"), or an older pending value winning.
#[test]
fn pending_keys_keep_their_own_timestamps_and_the_newest_value() {
    let (mut o, mut info) = outbox();
    // Two passes while the helper held the info lock: nothing was written in between.
    o.commit(&[("test", "ticks", Some("1"))], 10).unwrap();
    o.commit(
        &[("test", "last_seq", None), ("test", "ticks", Some("2"))],
        20,
    )
    .unwrap();
    o.commit(&[("test", "last_seq", Some("7"))], 30).unwrap();
    assert!(o.has_pending_info());
    assert!(o.write_info(&mut info));
    assert!(!o.has_pending_info());
    let mut r = InfoReader::attach(&info).unwrap();
    assert_eq!(
        r.snapshot(&info).unwrap(),
        [
            value("ticks", Some("2"), 20),
            value("last_seq", Some("7"), 30)
        ]
    );
    // An unchanged value changes nothing, so no bell.
    o.commit(&[("test", "ticks", Some("2"))], 40).unwrap();
    assert!(!o.write_info(&mut info));
    assert_eq!(r.changes(&info).unwrap(), []);
    // The bell rings when any key changed, not only the last one committed.
    o.commit(
        &[
            ("test", "ticks", Some("3")),
            ("test", "last_seq", Some("7")),
        ],
        50,
    )
    .unwrap();
    assert!(o.write_info(&mut info));
}

/// Catches an event over the design's limits being queued (or truncated) instead of refused.
#[test]
fn an_event_over_its_limits_is_refused() {
    let (mut o, _) = outbox();
    assert_eq!(
        o.push_event(&"n".repeat(256), "", 1),
        Err(ProduceError::NameTooLong(256))
    );
    assert_eq!(
        o.push_event("e", &"d".repeat(63 * 1024 + 1), 1),
        Err(ProduceError::DataTooLong(64_513))
    );
    assert_eq!(o.queued(), 0);
    assert!(
        o.push_event(&"n".repeat(255), &"d".repeat(63 * 1024), 1)
            .is_ok()
    );
}

/// Catches queue overflow that is not counted in `dropped` (design: "oldest dropped and
/// counted"), a dropped count that never reaches the ring's header, events over the throttle
/// being lost instead of waiting in the queue, or events written out of order.
#[test]
fn queue_overflow_is_counted_and_the_rest_wait_for_the_throttle() {
    let (mut o, _) = outbox();
    let mut r = ring();
    let (mut reader, _) = RingReader::start(&r, 1, None).unwrap();
    for i in 0..300u64 {
        o.push_event("burst", &i.to_string(), i).unwrap();
    }
    assert_eq!(QUEUE_CAP, 256);
    assert_eq!((o.queued(), o.dropped()), (256, 44));
    assert!(o.has_pending_events());

    assert!(o.write_events(&mut r, 1_000));
    assert_eq!(o.queued(), 226);
    let d = reader.drain(&r).unwrap();
    assert_eq!(d.lost, 44, "the dropped count reached the ring");
    let data: Vec<String> = d.events.iter().map(|e| e.data.clone()).collect();
    let want: Vec<String> = (44..74).map(|i: u64| i.to_string()).collect();
    assert_eq!(data, want, "the oldest went, the rest are in order");
    assert_eq!(
        d.events[0].ts, 44,
        "each event keeps the time it was pushed"
    );

    assert!(
        !o.write_events(&mut r, 1_100),
        "the throttle's budget is spent"
    );
    assert_eq!(o.queued(), 226);
    assert!(o.write_events(&mut r, 1_200));
    assert_eq!(o.queued(), 196);
    let d = reader.drain(&r).unwrap();
    assert_eq!((d.lost, d.events.len()), (0, 30));
    assert_eq!(d.events[0].data, "74");

    let mut now = 1_400;
    while o.queued() > 0 {
        o.write_events(&mut r, now);
        reader.drain(&r).unwrap();
        now += 200;
    }
    assert!(!o.has_pending_events());
    assert!(!o.write_events(&mut r, now));
}

/// Catches an empty queue spending throttle budget (the budget check must come after the
/// emptiness check), which would leave events waiting that the window had room for.
#[test]
fn an_empty_queue_spends_no_throttle_budget() {
    let (mut o, _) = outbox();
    let mut r = ring();
    o.push_event("one", "", 0).unwrap();
    o.write_events(&mut r, 0);
    for i in 0..29u64 {
        o.push_event("more", &i.to_string(), 0).unwrap();
    }
    o.write_events(&mut r, 0);
    assert_eq!(o.queued(), 0);
}
