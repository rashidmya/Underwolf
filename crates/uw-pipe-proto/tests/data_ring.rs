use uw_pipe_proto::data::{
    CatchUp, Drained, MAX_EVENT_DATA, MAX_EVENT_NAME, RECORD_SIZE, RING_HEADER_SIZE, RING_SIZE,
    RING_SLOTS, ReadError, ResumePoint, RingEvent, RingHead, RingReader, catch_up, read_ring_head,
    ring_init, ring_push, ring_set_dropped, start_after,
};

fn ring() -> Vec<u8> {
    let mut r = vec![0xEEu8; RING_SIZE];
    ring_init(&mut r);
    r
}

fn push_n(r: &mut [u8], n: u64) {
    for _ in 0..n {
        let seq = read_ring_head(r).unwrap().write_seq + 1;
        ring_push(r, "e", &seq.to_string(), seq * 10);
    }
}

fn seqs(d: &Drained) -> Vec<u64> {
    d.events.iter().map(|e| e.seq).collect()
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Catches a ring laid out differently from the design: the header (magic 0, version 4,
/// `write_seq` 8, `dropped` 16), 30 records after it, each `seq` 0, `ts` 8, `name_len` 16, the name
/// at 18, `data_len` 276 and the data at 280, record `seq` in slot `seq % 30`.
#[test]
fn the_ring_sits_at_the_design_offsets() {
    let mut r = ring();
    assert_eq!(
        (RING_HEADER_SIZE, RING_SLOTS, RECORD_SIZE),
        (24, 30, 280 + 63 * 1024)
    );
    assert_eq!(RING_SIZE, 24 + 30 * RECORD_SIZE);
    assert_eq!(&r[0..4], b"UWDE");
    assert_eq!(u32::from_le_bytes(r[4..8].try_into().unwrap()), 1);
    assert_eq!((u64_at(&r, 8), u64_at(&r, 16)), (0, 0));
    assert!(r[24..].iter().all(|&b| b == 0), "no records yet");

    assert_eq!(ring_push(&mut r, "tick", "7", 99), 1);
    ring_set_dropped(&mut r, 5);
    let rec = 24 + RECORD_SIZE; // slot 1 % 30
    assert_eq!(u64_at(&r, rec), 1);
    assert_eq!(u64_at(&r, rec + 8), 99);
    assert_eq!(
        u16::from_le_bytes(r[rec + 16..rec + 18].try_into().unwrap()),
        4
    );
    assert_eq!(&r[rec + 18..rec + 22], b"tick");
    assert_eq!(
        u32::from_le_bytes(r[rec + 276..rec + 280].try_into().unwrap()),
        1
    );
    assert_eq!(&r[rec + 280..rec + 281], b"7");
    assert_eq!((u64_at(&r, 8), u64_at(&r, 16)), (1, 5));
    assert_eq!(
        read_ring_head(&r),
        Ok(RingHead {
            write_seq: 1,
            dropped: 5
        })
    );
    // Seq 30 goes to slot 0, seq 31 back to slot 1.
    push_n(&mut r, 30);
    assert_eq!(u64_at(&r, 24), 30);
    assert_eq!(u64_at(&r, rec), 31);
}

/// Catches events delivered out of order, twice, or with their fields mixed up, and the limits
/// (255-byte names, 63 KiB of data) not fitting.
#[test]
fn events_drain_in_seq_order_once_each() {
    let mut r = ring();
    let (mut reader, from) = RingReader::start(&r, 7, None).unwrap();
    assert_eq!(from, 0);
    let long_name = "n".repeat(MAX_EVENT_NAME);
    let big_data = "d".repeat(MAX_EVENT_DATA);
    ring_push(&mut r, "a", "x", 1);
    ring_push(&mut r, &long_name, &big_data, 2);
    ring_push(&mut r, "c", "", 3);
    let d = reader.drain(&r).unwrap();
    assert_eq!(d.lost, 0);
    assert_eq!(
        d.events,
        [
            RingEvent {
                seq: 1,
                name: "a".into(),
                data: "x".into(),
                ts: 1
            },
            RingEvent {
                seq: 2,
                name: long_name,
                data: big_data,
                ts: 2
            },
            RingEvent {
                seq: 3,
                name: "c".into(),
                data: String::new(),
                ts: 3
            },
        ]
    );
    assert_eq!(reader.after(), 3);
    assert_eq!(
        reader.drain(&r).unwrap(),
        Drained::default(),
        "nothing twice"
    );
}

/// Catches a first attach that replays what the ring still holds (Spike 3's replay, which the
/// design replaces with "start at the write head").
#[test]
fn a_first_attach_starts_at_the_write_head() {
    let mut r = ring();
    push_n(&mut r, 5);
    let (mut reader, from) = RingReader::start(&r, 7, None).unwrap();
    assert_eq!(from, 5);
    assert_eq!(reader.drain(&r).unwrap(), Drained::default());
    push_n(&mut r, 1);
    assert_eq!(seqs(&reader.drain(&r).unwrap()), [6]);
}

/// Catches records overwritten before they were read going uncounted (design `data_lost`), or the
/// oldest ones the ring still holds not being delivered.
#[test]
fn wraparound_before_a_drain_is_counted_as_lost() {
    let mut r = ring();
    let (mut reader, _) = RingReader::start(&r, 7, None).unwrap();
    push_n(&mut r, 40);
    let d = reader.drain(&r).unwrap();
    assert_eq!(d.lost, 10);
    assert_eq!(seqs(&d), (11..=40).collect::<Vec<_>>());
    push_n(&mut r, 30);
    let d = reader.drain(&r).unwrap();
    assert_eq!(
        (d.lost, d.events.len()),
        (0, 30),
        "exactly a full ring is not a loss"
    );
}

/// Catches growth in the producer's `dropped` count not reaching `data_lost` (design "Failures"),
/// or drops from before the attach being reported as new.
#[test]
fn growth_in_dropped_is_lost_too() {
    let mut r = ring();
    ring_set_dropped(&mut r, 4);
    let (mut reader, _) = RingReader::start(&r, 7, None).unwrap();
    push_n(&mut r, 2);
    ring_set_dropped(&mut r, 10);
    let d = reader.drain(&r).unwrap();
    assert_eq!((d.lost, seqs(&d)), (6, vec![1, 2]));
    assert_eq!(reader.drain(&r).unwrap().lost, 0);
}

/// Catches a reader that follows a ring whose seq or dropped count goes backwards under the same
/// producer, or a slot holding another seq (design "Failures": `data_bad_layout`, not a resync).
#[test]
fn counters_that_go_backwards_are_a_bad_layout() {
    let mut r = ring();
    let (mut reader, _) = RingReader::start(&r, 7, None).unwrap();
    push_n(&mut r, 5);
    reader.drain(&r).unwrap();
    let mut b = r.clone();
    ring_init(&mut b);
    assert_eq!(
        reader.clone().drain(&b),
        Err(ReadError::Layout(
            "event seq went backwards: the ring is at 0, after 5".into()
        ))
    );
    let mut b = r.clone();
    ring_set_dropped(&mut b, 3);
    let mut grown = reader.clone();
    grown.drain(&b).unwrap();
    ring_set_dropped(&mut b, 1);
    assert_eq!(
        grown.drain(&b),
        Err(ReadError::Layout(
            "the dropped count went backwards: 1 after 3".into()
        ))
    );
    let mut b = r.clone();
    push_n(&mut b, 1);
    let rec6 = RING_HEADER_SIZE + 6 * RECORD_SIZE;
    b[rec6..rec6 + 8].copy_from_slice(&36u64.to_le_bytes());
    assert_eq!(
        reader.clone().drain(&b),
        Err(ReadError::Layout(
            "the ring slot for seq 6 holds seq 36".into()
        ))
    );
}

/// Catches a record or header the helper must not trust (design "Every read is checked"): a name
/// or data length over its limit, text that is not UTF-8, a wrong magic, another version
/// (`data_version_mismatch`), or a ring that is too small.
#[test]
fn records_and_headers_that_do_not_fit_are_refused() {
    let mut r = ring();
    let (reader, _) = RingReader::start(&r, 7, None).unwrap();
    push_n(&mut r, 1);
    let rec = RING_HEADER_SIZE + RECORD_SIZE;
    let bad = |edit: &dyn Fn(&mut Vec<u8>)| {
        let mut b = r.clone();
        edit(&mut b);
        reader.clone().drain(&b).unwrap_err()
    };
    let layout = |why: &str| ReadError::Layout(why.into());
    assert_eq!(
        bad(&|b| b[rec + 16..rec + 18].copy_from_slice(&256u16.to_le_bytes())),
        layout("event 1: name length 256 over 255")
    );
    assert_eq!(
        bad(&|b| b[rec + 276..rec + 280].copy_from_slice(&(64_513u32).to_le_bytes())),
        layout("event 1: data length 64513 over 64512")
    );
    assert_eq!(
        bad(&|b| b[rec + 18] = 0xFF),
        layout("event 1: name is not UTF-8")
    );
    assert_eq!(
        bad(&|b| b[rec + 280] = 0xFF),
        layout("event 1: data is not UTF-8")
    );
    assert_eq!(
        bad(&|b| b[0] = b'X'),
        layout("event ring magic is not UWDE")
    );
    assert_eq!(
        bad(&|b| b[4..8].copy_from_slice(&2u32.to_le_bytes())),
        ReadError::Version(2)
    );
    assert_eq!(
        read_ring_head(&r[..RING_SIZE - 1]),
        Err(layout(&format!(
            "event ring is {} bytes, under {RING_SIZE}",
            RING_SIZE - 1
        )))
    );
    assert_eq!(
        RingReader::start(&vec![0u8; RING_SIZE], 7, None).unwrap_err(),
        layout("event ring magic is not UWDE")
    );
}

/// Catches a ring whose counters are out of range being read: a `write_seq` or a `dropped` above
/// `i64::MAX` is a bad layout (no real producer gets there), and `i64::MAX` itself still reads.
#[test]
fn counters_out_of_range_are_a_bad_layout() {
    let layout = |why: &str| ReadError::Layout(why.into());
    let over = (i64::MAX as u64) + 1;
    let mut r = ring();
    r[8..16].copy_from_slice(&over.to_le_bytes());
    assert_eq!(
        read_ring_head(&r),
        Err(layout("event ring write_seq is over i64::MAX"))
    );
    assert_eq!(
        RingReader::start(&r, 7, None).unwrap_err(),
        layout("event ring write_seq is over i64::MAX")
    );
    let mut r = ring();
    r[16..24].copy_from_slice(&over.to_le_bytes());
    assert_eq!(
        read_ring_head(&r),
        Err(layout("event ring dropped is over i64::MAX"))
    );
    assert_eq!(
        RingReader::start(&r, 7, None).unwrap_err(),
        layout("event ring dropped is over i64::MAX")
    );

    let mut r = ring();
    r[8..16].copy_from_slice(&(i64::MAX as u64).to_le_bytes());
    r[16..24].copy_from_slice(&(i64::MAX as u64).to_le_bytes());
    assert_eq!(
        read_ring_head(&r),
        Ok(RingHead {
            write_seq: i64::MAX as u64,
            dropped: i64::MAX as u64
        })
    );
}

/// Catches the resume arithmetic going wrong (design "Failures"): the records after the resume
/// seq, a gap counted when the ring no longer holds them, and a resume point ahead of the ring.
#[test]
fn catch_up_gives_the_records_to_deliver_and_the_gap() {
    assert_eq!(catch_up(0, 0), Ok(CatchUp { lost: 0, first: 1 }));
    assert_eq!(catch_up(0, 30), Ok(CatchUp { lost: 0, first: 1 }));
    assert_eq!(catch_up(0, 31), Ok(CatchUp { lost: 1, first: 2 }));
    assert_eq!(
        catch_up(4, 44),
        Ok(CatchUp {
            lost: 10,
            first: 15
        })
    );
    assert_eq!(catch_up(20, 44), Ok(CatchUp { lost: 0, first: 21 }));
    assert_eq!(catch_up(44, 44), Ok(CatchUp { lost: 0, first: 45 }));
    assert_eq!(
        catch_up(45, 44),
        Err(ReadError::Layout(
            "event seq went backwards: the ring is at 44, after 45".into()
        ))
    );
}

/// Catches a resume that attaches to the wrong producer: a resume point for another producer (a
/// new game, a reused pid) must start at the write head, as a first attach does.
#[test]
fn start_after_resumes_only_the_same_producer() {
    let resume = Some(ResumePoint {
        producer_id: 7,
        seq: 3,
    });
    assert_eq!(start_after(7, 10, resume), 3);
    assert_eq!(start_after(8, 10, resume), 10);
    assert_eq!(start_after(7, 10, None), 10);
}

/// Catches a resume through a real ring going wrong end to end: every event after the resume
/// seq exactly once; an old resume seq gives the gap and then the oldest record; another producer
/// starts at the head; a resume point ahead of the ring is a bad layout.
#[test]
fn a_resumed_reader_delivers_each_event_after_the_resume_seq_once() {
    let mut r = ring();
    push_n(&mut r, 8);
    let (mut reader, from) = RingReader::start(
        &r,
        7,
        Some(ResumePoint {
            producer_id: 7,
            seq: 5,
        }),
    )
    .unwrap();
    assert_eq!(from, 5);
    assert_eq!(seqs(&reader.drain(&r).unwrap()), [6, 7, 8]);

    push_n(&mut r, 40); // the ring is at 48 and holds 19..=48
    let (mut old, from) = RingReader::start(
        &r,
        7,
        Some(ResumePoint {
            producer_id: 7,
            seq: 2,
        }),
    )
    .unwrap();
    assert_eq!(from, 2);
    let d = old.drain(&r).unwrap();
    assert_eq!(d.lost, 16);
    assert_eq!(seqs(&d), (19..=48).collect::<Vec<_>>());

    let (_, from) = RingReader::start(
        &r,
        9,
        Some(ResumePoint {
            producer_id: 7,
            seq: 2,
        }),
    )
    .unwrap();
    assert_eq!(from, 48);
    assert_eq!(
        RingReader::start(
            &r,
            7,
            Some(ResumePoint {
                producer_id: 7,
                seq: 49
            })
        )
        .unwrap_err(),
        ReadError::Layout("event seq went backwards: the ring is at 48, after 49".into())
    );
}

/// Catches `--data-resume` values parsed loosely or printed in another form than
/// `<producer_id>:<seq>` (`uw-host` prints it, the helper parses it).
#[test]
fn a_resume_point_is_producer_id_colon_seq() {
    let p = ResumePoint {
        producer_id: 123_456_789_012,
        seq: 42,
    };
    assert_eq!(p.to_string(), "123456789012:42");
    assert_eq!("123456789012:42".parse(), Ok(p));
    for bad in ["", "1", "1:", ":1", "a:1", "1:b", "1:2:3", "-1:2", "1:2 "] {
        assert!(bad.parse::<ResumePoint>().is_err(), "{bad:?} parsed");
    }
}
