use uw_pipe_proto::bells::{bit_names, to_game, to_host, MASK_REGION_SIZE, TO_GAME_OFFSET, TO_HOST_OFFSET};

/// Catches a mask region that disagrees with the design (to_game u64 at 0, to_host u64 at 8).
#[test]
fn the_mask_region_matches_the_design() {
    assert_eq!((MASK_REGION_SIZE, TO_GAME_OFFSET, TO_HOST_OFFSET), (16, 0, 8));
}

/// Catches a renumbered bit: the overlay and the helper must agree on v1's bit numbers.
#[test]
fn the_v1_bits_have_the_design_numbers() {
    assert_eq!(
        [to_game::EDIT_MODE_ON, to_game::EDIT_MODE_OFF, to_game::SHOW, to_game::HIDE],
        [1 << 0, 1 << 1, 1 << 2, 1 << 3]
    );
    assert_eq!(to_host::STATE_APPLIED, 1 << 0);
}

/// Catches unknown bits dropped from the `bell` event, or named some other way than `bit<N>`.
#[test]
fn bits_are_named_and_unknown_ones_are_bit_n() {
    assert_eq!(bit_names(to_host::STATE_APPLIED, &to_host::NAMES), ["state_applied"]);
    assert_eq!(bit_names(to_host::STATE_APPLIED | 1 << 5 | 1 << 63, &to_host::NAMES), ["state_applied", "bit5", "bit63"]);
    assert_eq!(
        bit_names(0b1111, &to_game::NAMES),
        ["edit_mode_on", "edit_mode_off", "show", "hide"]
    );
    assert!(bit_names(0, &to_host::NAMES).is_empty());
}
