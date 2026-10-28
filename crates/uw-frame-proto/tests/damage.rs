use uw_frame_proto::*;
fn r(x: u32, y: u32, width: u32, height: u32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}
#[test]
fn disjoint_damage_includes_the_unchanged_gap() {
    assert_eq!(r(1, 2, 3, 4).union(r(8, 9, 2, 2)), r(1, 2, 9, 9));
    assert_eq!(r(8, 9, 2, 2).union(r(1, 2, 3, 4)), r(1, 2, 9, 9));
    assert_eq!(r(0, 0, 20, 20).union(r(5, 5, 2, 2)), r(0, 0, 20, 20));
}
#[test]
fn checked_rectangles_do_not_clip_or_overflow() {
    assert_eq!(
        Rect::checked(1, 2, 3, 4, (10, 10)).unwrap(),
        Some(r(1, 2, 3, 4))
    );
    assert_eq!(Rect::checked(10, 10, 0, 0, (10, 10)).unwrap(), None);
    for rect in [
        r(9, 0, 2, 1),
        r(0, 10, 1, 1),
        r(u32::MAX, 0, 1, 1),
        r(1, 0, u32::MAX, 1),
    ] {
        assert!(Rect::checked(rect.x, rect.y, rect.width, rect.height, (10, 10)).is_err());
    }
}
#[test]
fn frame_rows_and_backing_extent_respect_import_alignment() {
    assert_eq!(
        FrameLayout::new(13, 7, 4096).unwrap(),
        FrameLayout {
            width: 13,
            height: 7,
            stride: 64,
            pixel_len: 4096,
            alignment: 4096
        }
    );
    assert_eq!(
        FrameLayout::new(17, 2, 65536).unwrap(),
        FrameLayout {
            width: 17,
            height: 2,
            stride: 128,
            pixel_len: 65536,
            alignment: 65536
        }
    );
    for (w, h, a) in [
        (0, 1, 4096),
        (1, 0, 4096),
        (4097, 1, 4096),
        (1, 4097, 4096),
        (1, 1, 0),
        (1, 1, 4097),
        (1, 1, 2048),
        (1, 1, 2097152),
    ] {
        assert!(FrameLayout::new(w, h, a).is_err(), "{w} {h} {a}");
    }
    let malformed = FrameLayout {
        width: 13,
        height: 7,
        stride: 1,
        pixel_len: 4096,
        alignment: 4096,
    };
    assert!(malformed.validate().is_err());
}
