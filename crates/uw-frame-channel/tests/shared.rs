#![cfg(target_os = "linux")]
use std::{
    fs::File,
    os::{
        fd::{AsFd, OwnedFd},
        unix::fs::FileExt,
    },
    sync::Arc,
};
use uw_frame_channel::*;
use uw_frame_proto::*;
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn key(n: u64) -> AllocationKey {
    AllocationKey {
        session: SessionId([3; 16]),
        widget: WidgetKey {
            id: 1,
            generation: 1,
        },
        generation: n,
    }
}
fn layout() -> FrameLayout {
    FrameLayout::new(4, 2, 4096).unwrap()
}
fn full() -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: 4,
        height: 2,
    }
}
fn duplicate(map: &FrameMapping) -> [OwnedFd; 2] {
    // SAFETY: tests use the same channel implementation and preserve mutex/ownership rules.
    let fds = unsafe { map.descriptors() };
    [
        fds[0].try_clone_to_owned().unwrap(),
        fds[1].try_clone_to_owned().unwrap(),
    ]
}
fn map() -> Arc<FrameMapping> {
    Arc::new(FrameMapping::create(key(1), layout(), PeerIdentity::current().unwrap()).unwrap())
}
#[test]
fn an_opened_mapping_sees_exact_dirty_rows_and_preserves_padding() {
    let _serial = SERIAL.lock().unwrap();
    let producer = map();
    let data = (0..32).collect::<Vec<u8>>();
    assert_eq!(
        producer
            .try_publish(
                1,
                Patch {
                    rect: full(),
                    stride: 16,
                    data: &data
                }
            )
            .unwrap(),
        PublishResult::Published { seq: 1, bytes: 32 }
    );
    // SAFETY: the descriptors and initialized mutex come from our live producer.
    let consumer = Arc::new(
        unsafe {
            FrameMapping::open(
                &producer.header().encode().unwrap(),
                duplicate(&producer),
                PeerIdentity::current().unwrap(),
            )
        }
        .unwrap(),
    );
    let lease = consumer.try_claim().unwrap().unwrap();
    let bytes = lease.copy_pixels().unwrap();
    assert_eq!(&bytes[..16], &data[..16]);
    assert_eq!(&bytes[64..80], &data[16..]);
    assert!(bytes[16..64].iter().all(|&b| b == 0));
    assert_eq!(lease.host_pointer() as usize % 4096, 0);
    assert_eq!(
        producer
            .try_publish(
                2,
                Patch {
                    rect: full(),
                    stride: 16,
                    data: &data
                }
            )
            .unwrap(),
        PublishResult::Busy
    );
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert_eq!(producer.try_state().unwrap().consumed, 0);
    // SAFETY: this CPU ownership fixture submitted no GPU reads.
    let completed = unsafe { consumer.try_complete_after_gpu(&lease) }.unwrap();
    assert_eq!(completed, CompleteResult::Completed);
    assert!(matches!(lease.copy_pixels(), Err(FrameError::Stale)));
    let next = vec![9; 32];
    assert!(matches!(
        producer
            .try_publish(
                2,
                Patch {
                    rect: full(),
                    stride: 16,
                    data: &next
                }
            )
            .unwrap(),
        PublishResult::Published { .. }
    ));
}
#[test]
fn malformed_patch_is_rejected_before_any_pixel_or_state_change() {
    let _serial = SERIAL.lock().unwrap();
    let m = map();
    let expected = vec![7; 32];
    m.try_publish(
        1,
        Patch {
            rect: full(),
            stride: 16,
            data: &expected,
        },
    )
    .unwrap();
    let before = m.try_state().unwrap();
    for (rect, stride, data) in [
        (full(), 16, &[8u8; 31][..]),
        (full(), 3, &[8u8; 32][..]),
        (
            Rect {
                x: 3,
                y: 0,
                width: 2,
                height: 1,
            },
            8,
            &[8u8; 8][..],
        ),
    ] {
        assert!(m.try_publish(2, Patch { rect, stride, data }).is_err());
        assert_eq!(m.try_state().unwrap(), before);
    }
    let lease = m.try_claim().unwrap().unwrap();
    assert_eq!(&lease.copy_pixels().unwrap()[..16], &expected[..16]);
    m.try_cancel_unsubmitted(&lease).unwrap();
}
#[test]
fn late_completion_does_not_touch_a_new_allocation() {
    let _serial = SERIAL.lock().unwrap();
    let old = map();
    let data = [1; 32];
    old.try_publish(
        1,
        Patch {
            rect: full(),
            stride: 16,
            data: &data,
        },
    )
    .unwrap();
    let lease = old.try_claim().unwrap().unwrap();
    let next = FrameMapping::create(key(2), layout(), PeerIdentity::current().unwrap()).unwrap();
    // SAFETY: no GPU work was submitted in this test.
    let completed = unsafe { next.try_complete_after_gpu(&lease) }.unwrap();
    assert_eq!(completed, CompleteResult::Stale);
    assert_eq!(next.try_state().unwrap().consumed, 0);
}
#[test]
fn reservations_stay_charged_while_a_retired_mapping_is_referenced() {
    let _serial = SERIAL.lock().unwrap();
    let b = Budget::new(4096);
    let m = Arc::new(
        FrameMapping::create_in(&b, key(1), layout(), PeerIdentity::current().unwrap()).unwrap(),
    );
    m.try_publish(
        1,
        Patch {
            rect: full(),
            stride: 16,
            data: &[1; 32],
        },
    )
    .unwrap();
    let held = m.try_claim().unwrap().unwrap();
    drop(m);
    assert_eq!(b.used(), 4096);
    assert!(matches!(b.reserve(1), Err(FrameError::Capacity)));
    drop(held);
    assert_eq!(b.used(), 0);
    assert!(b.reserve(4096).is_ok());
}
#[test]
fn size_seals_and_header_validation_prevent_unsafe_mappings() {
    let _serial = SERIAL.lock().unwrap();
    let m = map();
    let [control, pixels] = duplicate(&m);
    let control = File::from(control);
    let pixels = File::from(pixels);
    assert!(control.set_len(1).is_err());
    assert!(pixels.set_len(8192).is_err());
    let original = m.header().encode().unwrap();
    control.write_all_at(&[0, 0], 88).unwrap();
    // SAFETY: corrupted headers must be rejected before touching the initialized native mutex.
    assert!(
        // SAFETY: header mismatch is rejected before using the initialized mutex.
        unsafe {
            FrameMapping::open(
                &original,
                [
                    control.as_fd().try_clone_to_owned().unwrap(),
                    pixels.as_fd().try_clone_to_owned().unwrap(),
                ],
                PeerIdentity::current().unwrap(),
            )
        }
        .is_err()
    );
}
#[test]
fn a_hundred_generations_release_all_owned_mappings_and_descriptors() {
    let _serial = SERIAL.lock().unwrap();
    let count = live_mapping_count();
    let fds = std::fs::read_dir("/proc/self/fd").unwrap().count();
    for n in 1..=100 {
        let m = FrameMapping::create(key(n), layout(), PeerIdentity::current().unwrap()).unwrap();
        drop(m);
    }
    assert_eq!(live_mapping_count(), count);
    assert_eq!(std::fs::read_dir("/proc/self/fd").unwrap().count(), fds);
}
