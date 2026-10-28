use uw_frame_proto::*;

fn copy_rect(from: &[u8], to: &mut [u8], rect: Rect) {
    for y in rect.y..rect.y + rect.height {
        let range = (y * 64 + rect.x * 4) as usize..(y * 64 + (rect.x + rect.width) * 4) as usize;
        to[range.clone()].copy_from_slice(&from[range]);
    }
}

#[test]
fn two_hundred_update_schedules_reconstruct_every_consumed_pixel() {
    for seed in 0..200u64 {
        let key = AllocationKey {
            session: SessionId([7; 16]),
            widget: WidgetKey {
                id: 3,
                generation: 1,
            },
            generation: seed + 1,
        };
        let layout = FrameLayout {
            width: 16,
            height: 8,
            stride: 64,
            pixel_len: 4096,
            alignment: 4096,
        };
        let full = Rect {
            x: 0,
            y: 0,
            width: 16,
            height: 8,
        };
        let mut state = FrameState::new(key, layout);
        let mut reference = vec![42u8; 512];
        let mut shared = reference.clone();
        let mut gpu = vec![0u8; 512];
        state.publish(1, full).unwrap();
        let mut waiting: Option<(Claim, Vec<u8>)> = None;
        let mut deferred: Option<Rect> = None;
        for step in 0..30u64 {
            let rect = Rect {
                x: ((step * 7 + seed * 3) % 14) as u32,
                y: ((step * 3 + seed) % 6) as u32,
                width: 2,
                height: 2,
            };
            for y in rect.y..rect.y + 2 {
                for x in rect.x..rect.x + 2 {
                    reference[(y * 64 + x * 4) as usize..(y * 64 + x * 4 + 4) as usize]
                        .fill(step as u8);
                }
            }
            deferred = Some(deferred.map_or(rect, |old| old.union(rect)));
            if waiting.is_none() {
                let patch = deferred.take().unwrap();
                copy_rect(&reference, &mut shared, patch);
                state.publish(step + 2, patch).unwrap();
                if step % 3 == seed % 3 {
                    waiting = Some((state.claim().unwrap().unwrap(), reference.clone()));
                }
            }
            if step % 5 == seed % 5
                && let Some((claim, expected)) = waiting.take()
            {
                copy_rect(&shared, &mut gpu, claim.damage);
                state.complete(claim).unwrap();
                assert_eq!(gpu, expected, "schedule {seed}, step {step}");
            }
        }
        // Drain the actual last GPU read before allowing the final pending patch.
        if let Some((claim, expected)) = waiting.take() {
            copy_rect(&shared, &mut gpu, claim.damage);
            state.complete(claim).unwrap();
            assert_eq!(gpu, expected, "schedule {seed}, final in-flight copy");
        }
        // No new paint is needed to publish the retained latest pixels.
        if let Some(patch) = deferred {
            copy_rect(&reference, &mut shared, patch);
            state.publish(999, patch).unwrap();
        }
        if let Some(claim) = state.claim().unwrap() {
            copy_rect(&shared, &mut gpu, claim.damage);
            state.complete(claim).unwrap();
        }
        assert_eq!(gpu, reference, "schedule {seed}, final pixels");
    }
}
