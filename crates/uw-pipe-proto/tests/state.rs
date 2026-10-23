use uw_pipe_proto::bells::to_game;
use uw_pipe_proto::state::{
    apply, check_header, init_header, read, set_applied_seq, Change, HeaderError, StateV1, BLOCK_SIZE,
    LAYOUT_VERSION,
};

fn fresh() -> Vec<u8> {
    let mut b = vec![0u8; BLOCK_SIZE];
    init_header(&mut b);
    b
}

/// Catches a header written at other offsets than the design's table (magic 0, version 4, size 8).
#[test]
fn the_header_sits_at_the_design_offsets() {
    let b = fresh();
    assert_eq!(&b[0..4], b"UWS1");
    assert_eq!(u16::from_le_bytes([b[4], b[5]]), 1);
    assert_eq!(u32::from_le_bytes(b[8..12].try_into().unwrap()), 64);
    assert_eq!(BLOCK_SIZE, 64);
    assert_eq!(LAYOUT_VERSION, 1);
    assert_eq!(check_header(&b), Ok(()));
}

/// Catches fields read from or written to the wrong offsets (seq 16, applied_seq 24, edit_mode 32,
/// visible 33), which the two sides would then disagree on.
#[test]
fn fields_sit_at_the_design_offsets() {
    let mut b = fresh();
    b[16..24].copy_from_slice(&7u64.to_le_bytes());
    b[32] = 1;
    b[33] = 1;
    set_applied_seq(&mut b, 5);
    assert_eq!(u64::from_le_bytes(b[24..32].try_into().unwrap()), 5);
    assert_eq!(read(&b), StateV1 { seq: 7, applied_seq: 5, edit_mode: true, visible: true });
}

/// Catches a header check that accepts a block of another layout (design: `version_mismatch`).
#[test]
fn a_foreign_header_is_rejected() {
    let mut b = fresh();
    b[0] = b'X';
    assert_eq!(check_header(&b), Err(HeaderError::BadMagic));

    let mut b = fresh();
    b[4..6].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(check_header(&b), Err(HeaderError::BadVersion(2)));

    let mut b = fresh();
    b[8..12].copy_from_slice(&128u32.to_le_bytes());
    assert_eq!(check_header(&b), Err(HeaderError::BadSize(128)));

    assert_eq!(check_header(&[0u8; 16]), Err(HeaderError::Short(16)));
    // A zeroed block (created but never initialised) is not v1 either.
    assert_eq!(check_header(&[0u8; BLOCK_SIZE]), Err(HeaderError::BadMagic));
}

/// Catches `set_state` bumping `seq` (and ringing) when nothing changed, or naming the wrong bits.
#[test]
fn apply_bumps_seq_once_per_change_and_names_the_changed_fields() {
    let mut b = fresh();
    assert_eq!(apply(&mut b, Some(true), None), Change { seq: 1, bits: to_game::EDIT_MODE_ON });
    assert_eq!(apply(&mut b, Some(true), Some(true)), Change { seq: 2, bits: to_game::SHOW });
    assert_eq!(
        apply(&mut b, Some(false), Some(false)),
        Change { seq: 3, bits: to_game::EDIT_MODE_OFF | to_game::HIDE }
    );
    assert_eq!(read(&b), StateV1 { seq: 3, applied_seq: 0, edit_mode: false, visible: false });
}

#[test]
fn apply_without_a_change_keeps_seq_and_rings_nothing() {
    let mut b = fresh();
    apply(&mut b, Some(true), Some(true));
    assert_eq!(apply(&mut b, Some(true), Some(true)), Change { seq: 1, bits: 0 });
    assert_eq!(apply(&mut b, None, None), Change { seq: 1, bits: 0 });
    assert_eq!(read(&b).seq, 1);
}
