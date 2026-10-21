use uw_spike_gep_channel::{EventRecord, InfoChange, InfoReader, InfoWriter, Schema, info_region_size};
use uw_spike_gep_helper::{relay, relay_events};
use uw_spike_ipc::Msg;

fn schema() -> Schema {
    Schema {
        categories: vec![(
            "game_info".into(),
            vec![("phase".into(), 64), ("game_mode".into(), 64), ("never_set".into(), 8)],
        )],
    }
}

fn info(category: &str, key: &str, value: Option<&str>, ts: u64) -> Msg {
    Msg::GepInfo { category: category.into(), key: key.into(), value: value.map(Into::into), ts }
}

#[test]
fn relay_snapshot_then_changes() {
    let schema = schema();
    let mut region = vec![0u8; info_region_size(&schema)];
    {
        let mut w = InfoWriter::init(&mut region, &schema).unwrap();
        let mut txn = w.begin();
        txn.set("game_info", "phase", Some(b"GameInProgress".as_slice()), 10).unwrap();
        txn.set("game_info", "game_mode", Some(b"bots".as_slice()), 10).unwrap();
        assert!(txn.commit());
    }
    let mut reader = InfoReader::attach(&region).unwrap();
    assert_eq!(
        relay(reader.snapshot(&region)),
        [info("game_info", "phase", Some("GameInProgress"), 10), info("game_info", "game_mode", Some("bots"), 10)]
    );

    {
        let mut w = InfoWriter::attach(&mut region).unwrap();
        let mut txn = w.begin();
        txn.set("game_info", "game_mode", None, 20).unwrap();
        assert!(txn.commit());
    }
    assert_eq!(relay(reader.changed(&region)), [info("game_info", "game_mode", None, 20)]);
}

#[test]
fn relay_drops_non_utf8_value() {
    let changes = vec![
        InfoChange { category: "game_info".into(), key: "phase".into(), value: Some(vec![0xff, 0xfe]), ts: 1 },
        InfoChange { category: "game_info".into(), key: "game_mode".into(), value: Some(b"ok".to_vec()), ts: 2 },
    ];
    assert_eq!(relay(changes), [info("game_info", "game_mode", Some("ok"), 2)]);
}

#[test]
fn relay_events_reports_loss_first_and_drops_non_utf8_data() {
    let records = vec![
        EventRecord { name: "clock".into(), data: br#"{"match_clock":"00:01"}"#.to_vec(), ts: 5 },
        EventRecord { name: "bad".into(), data: vec![0xc3], ts: 6 },
        EventRecord { name: "selftest".into(), data: b"{}".to_vec(), ts: 7 },
    ];
    assert_eq!(
        relay_events(records, 2),
        [
            Msg::GepLost { events: 2 },
            Msg::GepEvent { name: "clock".into(), data: r#"{"match_clock":"00:01"}"#.into(), ts: 5 },
            Msg::GepEvent { name: "selftest".into(), data: "{}".into(), ts: 7 },
        ]
    );
    assert_eq!(relay_events(Vec::new(), 0), []);
}
