//! Runs only for the windows target (e.g. under Wine): `cargo test --target x86_64-pc-windows-gnu`.
#![cfg(windows)]

use uw_spike_gep_channel::win::{Consumer, Producer, Wake};
use uw_spike_gep_channel::{FORMAT_VERSION, InfoReader, InfoWriter, RingReader, Schema, ring_push};

fn schema() -> Schema {
    Schema { categories: vec![("game_info".into(), vec![("phase".into(), 64)])] }
}

#[test]
fn producer_consumer_roundtrip() {
    let ch = 900_001;
    assert!(Consumer::open(ch).is_err());
    let mut p = Producer::create(ch, &schema()).unwrap();
    assert!(Producer::create(ch, &schema()).is_err());
    let c = Consumer::open(ch).unwrap();
    assert_eq!(c.hello(), (FORMAT_VERSION, std::process::id()));
    assert_eq!(c.wait(10), Wake::Timeout);

    let changed = p
        .with_info(|r| {
        let mut w = InfoWriter::attach(r).unwrap();
        let mut t = w.begin();
        t.set("game_info", "phase", Some(b"GameInProgress"), 5).unwrap();
        t.commit()
    })
        .unwrap();
    assert!(changed);
    p.signal_info();
    p.with_events(|r| ring_push(r, "kill", b"{}", 6).unwrap()).unwrap();
    p.signal_events();

    let mut got = [c.wait(1000), c.wait(1000)];
    got.sort_by_key(|w| *w as u8);
    assert_eq!(got, [Wake::Info, Wake::Events]);
    let snap = c.with_info(|r| InfoReader::attach(r).unwrap().snapshot(r)).unwrap();
    assert_eq!(snap[0].value.as_deref(), Some(&b"GameInProgress"[..]));
    let (recs, lost) = c.with_events(|r| RingReader::new().drain(r)).unwrap();
    assert_eq!((recs[0].name.as_str(), lost), ("kill", 0));

    drop(p);
    assert_eq!(c.wait(1000), Wake::Disconnect);
    assert!(Consumer::open(ch).is_err());
}

#[test]
fn producer_returns_none_while_another_handle_holds_the_mutex() {
    let ch = 900_002;
    let mut p = Producer::create(ch, &schema()).unwrap();
    let c = Consumer::open(ch).unwrap();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    // Mutexes are owned per thread, so the holder must be another thread.
    let holder = std::thread::spawn(move || {
        c.with_info(|_| {
            c.with_events(|_| {
                held_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
            .unwrap();
        })
        .unwrap();
    });
    held_rx.recv().unwrap();

    let start = std::time::Instant::now();
    let mut ran = false;
    assert!(p.with_info(|_| ran = true).is_none());
    assert!(p.with_events(|_| ran = true).is_none());
    assert!(!ran);
    assert!(start.elapsed() < std::time::Duration::from_millis(500));

    release_tx.send(()).unwrap();
    holder.join().unwrap();
    assert_eq!(p.with_info(|_| 7), Some(7));
    assert_eq!(p.with_events(|_| 8), Some(8));
}
