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

    let changed = p.with_info(|r| {
        let mut w = InfoWriter::attach(r).unwrap();
        let mut t = w.begin();
        t.set("game_info", "phase", Some(b"GameInProgress"), 5).unwrap();
        t.commit()
    });
    assert!(changed);
    p.signal_info();
    p.with_events(|r| ring_push(r, "kill", b"{}", 6).unwrap());
    p.signal_events();

    let mut got = [c.wait(1000), c.wait(1000)];
    got.sort_by_key(|w| *w as u8);
    assert_eq!(got, [Wake::Info, Wake::Events]);
    let snap = c.with_info(|r| InfoReader::attach(r).unwrap().snapshot(r));
    assert_eq!(snap[0].value.as_deref(), Some(&b"GameInProgress"[..]));
    let (recs, lost) = c.with_events(|r| RingReader::new().drain(r));
    assert_eq!((recs[0].name.as_str(), lost), ("kill", 0));

    drop(p);
    assert_eq!(c.wait(1000), Wake::Disconnect);
    assert!(Consumer::open(ch).is_err());
}
