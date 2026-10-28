use uw_frame_proto::*;
fn key() -> AllocationKey {
    AllocationKey {
        session: SessionId([1; 16]),
        widget: WidgetKey {
            id: 1,
            generation: 2,
        },
        generation: 9007199254740993,
    }
}
fn header() -> AllocationHeader {
    AllocationHeader {
        key: key(),
        consumer_pid: 12,
        producer_pid: 13,
        start_ticks: 15,
        layout: FrameLayout {
            width: 16,
            height: 8,
            stride: 64,
            pixel_len: 4096,
            alignment: 4096,
        },
    }
}
fn messages() -> Vec<(Message, usize)> {
    vec![
        (
            Message::Hello {
                pid: 12,
                start_ticks: 15,
                alignment: 4096,
                mutex_size: 40,
                mutex_align: 8,
                abi: 1,
                capability: 1,
            },
            32,
        ),
        (
            Message::Welcome {
                session: SessionId([1; 16]),
                producer_pid: 13,
                max_widgets: 32,
                max_side: 4096,
                shared_budget: 268435456,
            },
            36,
        ),
        (Message::Register(header()), 128),
        (
            Message::Dirty {
                key: key(),
                produced: 4,
            },
            40,
        ),
        (
            Message::Consumed {
                key: key(),
                consumed: 4,
            },
            40,
        ),
        (
            Message::Layout {
                widget: key().widget,
                revision: 5,
                x: -2,
                y: 3,
                width: 16,
                height: 8,
                z: 2,
                visible: true,
            },
            37,
        ),
        (
            Message::Remove {
                widget: key().widget,
                revision: 5,
            },
            16,
        ),
        (
            Message::Dimming {
                revision: 5,
                active: false,
            },
            9,
        ),
        (
            Message::NeedBaseline {
                widget: key().widget,
            },
            8,
        ),
        (
            Message::Unavailable {
                key: None,
                reason: 3,
            },
            36,
        ),
        (Message::Retired { key: key() }, 32),
        (Message::Registered { key: key() }, 32),
        (
            Message::Swapchain {
                generation: 3,
                width: 1280,
                height: 720,
                ready: true,
            },
            17,
        ),
        (
            Message::Progress {
                swapchain_generation: 3,
                completed_draw: 9007199254740993,
                monotonic_ns: 123,
            },
            24,
        ),
    ]
}
#[test]
fn all_messages_roundtrip_exact_lengths_and_fd_counts() {
    for (i, (m, payload)) in messages().into_iter().enumerate() {
        let b = encode_message(&m).unwrap();
        assert_eq!(b.len(), 16 + payload);
        assert_eq!(&b[..4], b"UWFR");
        assert_eq!(&b[6..8], &((i + 1) as u16).to_le_bytes());
        assert_eq!(b[12] as usize, m.expected_fds());
        assert_eq!(decode_message(&b).unwrap(), m);
        for len in [0, 15, b.len() - 1] {
            assert!(decode_message(&b[..len]).is_err());
        }
        let mut extra = b.clone();
        extra.push(0);
        assert!(decode_message(&extra).is_err());
    }
}
#[test]
fn header_validation_precedes_native_mutex_use() {
    let h = header();
    let b = h.encode().unwrap();
    assert_eq!(AllocationHeader::decode(&b).unwrap(), h);
    assert_eq!(&b[48..56], &9007199254740993u64.to_le_bytes());
    for offset in [
        0, 4, 6, 8, 24, 28, 32, 40, 44, 48, 56, 60, 64, 68, 72, 80, 88, 90, 92, 96,
    ] {
        let mut broken = b;
        if offset == 96 {
            broken[offset] = 1;
        } else {
            broken[offset..(offset + 4).min(128)].fill(0);
        }
        if offset == 8 {
            broken[8..24].fill(0);
        }
        if offset == 48 {
            broken[48..56].fill(0);
        }
        assert!(
            AllocationHeader::decode(&broken).is_err(),
            "accepted corrupt field @{offset}"
        );
    }
}
#[test]
fn wire_rejects_bad_tags_lengths_rights_and_booleans() {
    let b = encode_message(&Message::Dimming {
        revision: 1,
        active: true,
    })
    .unwrap();
    for (offset, value) in [(0, 0), (4, 2), (6, 99), (8, 0), (12, 2), (14, 1), (24, 2)] {
        let mut bad = b.clone();
        bad[offset] = value;
        assert!(decode_message(&bad).is_err(), "{offset}");
    }
    let mut bad = b.clone();
    bad[8..12].copy_from_slice(&4097u32.to_le_bytes());
    assert!(decode_message(&bad).is_err());
    assert!(
        encode_message(&Message::Dirty {
            key: key(),
            produced: 0
        })
        .is_err()
    );
    assert!(
        encode_message(&Message::Unavailable {
            key: None,
            reason: 0
        })
        .is_err()
    );
}
