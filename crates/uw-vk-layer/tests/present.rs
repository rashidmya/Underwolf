use uw_frame_proto::*;
use uw_vk_layer::*;
#[test]
fn rectangular_upload_preserves_row_pitch_and_destination() {
    let layout = FrameLayout::new(16, 8, 4096).unwrap();
    let damage = Rect {
        x: 3,
        y: 2,
        width: 4,
        height: 3,
    };
    let r = upload_region(layout, damage).unwrap();
    assert_eq!(r.offset, 140);
    assert_eq!(r.row_length, 16);
    assert_eq!(r.damage, damage);
    assert!(
        upload_region(
            layout,
            Rect {
                x: 15,
                y: 0,
                width: 2,
                height: 1
            }
        )
        .is_err()
    );
}
#[test]
fn busy_or_unsignaled_present_submits_nothing_and_keeps_original_waits() {
    for capable in [true, false] {
        let original = [11, 22];
        let mut waited = vec![];
        let result = submit_if_ready(capable, false, 1, || {
            waited.extend(original);
            true
        });
        assert!(waited.is_empty());
        assert_eq!(
            result,
            if capable {
                DrawOutcome::Skipped
            } else {
                DrawOutcome::Unavailable
            }
        );
    }
}
#[test]
fn successful_submit_chains_the_signal_and_failure_never_counts_as_submitted() {
    let mut calls = 0;
    let result = submit_if_ready(true, true, 17, || {
        calls += 1;
        true
    });
    assert_eq!(calls, 1);
    assert_eq!(result, DrawOutcome::Submitted { epoch: 17 });
    assert_eq!(
        submit_if_ready(true, true, 18, || false),
        DrawOutcome::Unavailable
    );
}
#[cfg(target_os = "linux")]
#[test]
fn an_unsubmitted_claim_returns_to_ready_without_consumption() {
    use std::sync::Arc;
    use uw_frame_channel::*;
    let key = AllocationKey {
        session: SessionId([9; 16]),
        widget: WidgetKey {
            id: 1,
            generation: 1,
        },
        generation: 1,
    };
    let m = Arc::new(
        FrameMapping::create(
            key,
            FrameLayout::new(4, 1, 4096).unwrap(),
            PeerIdentity::current().unwrap(),
        )
        .unwrap(),
    );
    m.try_publish(
        1,
        Patch {
            rect: Rect {
                x: 0,
                y: 0,
                width: 4,
                height: 1,
            },
            stride: 16,
            data: &[0; 16],
        },
    )
    .unwrap();
    let lease = m.try_claim().unwrap().unwrap();
    assert_eq!(
        m.try_cancel_unsubmitted(&lease).unwrap(),
        CompleteResult::Completed
    );
    let s = m.try_state().unwrap();
    assert_eq!(s.phase, Phase::Ready);
    assert_eq!(s.consumed, 0);
}
