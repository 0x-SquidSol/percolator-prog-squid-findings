//! Fee-split collection tests (2026-07-19 design).
//! The split is exact: protocol + creator + lp + insurance == fee, always.

use percolator_prog::policy_v16::{split_trade_fee, FeeSplitParts};

const P: u16 = 2000;
const C: u16 = 1600;
const L: u16 = 4800;
const I: u16 = 1600;

#[test]
fn split_is_exactly_conservative_for_round_amount() {
    let parts = split_trade_fee(10_000, P, C, L, I).unwrap();
    assert_eq!(parts.protocol, 2_000);
    assert_eq!(parts.creator, 1_600);
    assert_eq!(parts.lp, 4_800);
    assert_eq!(parts.insurance, 1_600);
    assert_eq!(
        parts.protocol + parts.creator + parts.lp + parts.insurance,
        10_000
    );
}

#[test]
fn dust_goes_to_insurance_and_total_is_still_exact() {
    // 7 atoms: every floor() is 0 or 1; the remainder must land on insurance.
    let parts = split_trade_fee(7, P, C, L, I).unwrap();
    assert_eq!(
        parts.protocol + parts.creator + parts.lp + parts.insurance,
        7,
        "conservation must hold even when every leg rounds down"
    );
    let floor_insurance = 7u128 * I as u128 / 10_000;
    assert!(
        parts.insurance >= floor_insurance,
        "insurance receives its floor plus all dust"
    );
}

#[test]
fn zero_fee_splits_to_all_zeros() {
    let parts = split_trade_fee(0, P, C, L, I).unwrap();
    assert_eq!(parts, FeeSplitParts { protocol: 0, creator: 0, lp: 0, insurance: 0 });
}

#[test]
fn conservation_holds_across_many_amounts() {
    for fee in 0u128..2_000 {
        let parts = split_trade_fee(fee, P, C, L, I).unwrap();
        assert_eq!(
            parts.protocol + parts.creator + parts.lp + parts.insurance,
            fee,
            "conservation failed at fee={fee}"
        );
    }
}

#[test]
fn config_size_is_576_and_16_byte_aligned() {
    use percolator_prog::constants::WRAPPER_CONFIG_LEN;
    use percolator_prog::state::WrapperConfigV16;
    assert_eq!(core::mem::size_of::<WrapperConfigV16>(), 576);
    assert_eq!(WRAPPER_CONFIG_LEN, 576);
    assert_eq!(576 % core::mem::align_of::<WrapperConfigV16>(), 0);
}

#[test]
fn default_shares_sum_to_total_and_satisfy_floors() {
    use percolator_prog::constants::*;
    assert_eq!(
        DEFAULT_CREATOR_SHARE_BPS + DEFAULT_LP_SHARE_BPS + DEFAULT_INSURANCE_SHARE_BPS,
        FEE_SHARE_TOTAL_BPS
    );
    assert!(DEFAULT_CREATOR_SHARE_BPS <= MAX_CREATOR_SHARE_BPS);
    assert!(DEFAULT_LP_SHARE_BPS >= MIN_LP_SHARE_BPS);
    assert!(DEFAULT_INSURANCE_SHARE_BPS >= MIN_INSURANCE_SHARE_BPS);
}

// NOTE: the brief placed this assertion in tests/v16_kani.rs, "alongside the
// existing ordinal assertions." That file opens with `#![cfg(kani)]` at file
// scope, so under plain `cargo test` (no kani cfg) its entire contents --
// including a bare `#[test]` fn -- compile to nothing (verified: "running 0
// tests" even with the file's pre-existing kani proofs present). A plain
// `#[test]` there would silently never execute, and `cargo kani` does not
// run non-`#[kani::proof]` items either, so it would never run at all. It
// lives here instead, alongside this file's other pure constant/ordinal
// checks (`default_shares_sum_to_total_and_satisfy_floors`), where it
// actually executes under `cargo test`.
#[test]
fn fee_split_error_ordinals_are_pinned() {
    use percolator_prog::error::PercolatorError;
    assert_eq!(PercolatorError::FeeSplitFloorViolation as u32, 51);
    assert_eq!(PercolatorError::FeeSplitSumInvalid as u32, 52);
    assert_eq!(PercolatorError::NoInsuranceReserveToClaim as u32, 53);
}

#[test]
fn validate_fee_split_accepts_defaults() {
    use percolator_prog::policy_v16::validate_fee_split;
    assert!(validate_fee_split(1600, 4800, 1600).is_ok());
}

#[test]
fn validate_fee_split_accepts_both_floor_extremes() {
    use percolator_prog::policy_v16::validate_fee_split;
    // The three floors sum to exactly 8000, so this is the ONLY point where
    // all three bind simultaneously. It must be accepted, not rejected.
    assert!(validate_fee_split(3600, 3200, 1200).is_ok());
}

#[test]
fn validate_fee_split_rejects_wrong_sum_with_exact_code() {
    use percolator_prog::policy_v16::validate_fee_split;
    use solana_program::program_error::ProgramError;
    // 1600 + 4800 + 1000 = 7400 != 8000
    assert_eq!(
        validate_fee_split(1600, 4800, 1000).unwrap_err(),
        ProgramError::Custom(52),
        "must be FeeSplitSumInvalid, not a generic error"
    );
    // Over-sum must also be rejected, not silently truncated.
    assert_eq!(
        validate_fee_split(1600, 4800, 2000).unwrap_err(),
        ProgramError::Custom(52)
    );
}

#[test]
fn validate_fee_split_rejects_floor_violations_with_exact_code() {
    use percolator_prog::policy_v16::validate_fee_split;
    use solana_program::program_error::ProgramError;
    // LP below floor, sum exactly 8000, no other floor violated.
    assert_eq!(
        validate_fee_split(3600, 3100, 1300).unwrap_err(),
        ProgramError::Custom(51)
    );
    // Insurance below floor, sum exactly 8000, no other floor violated.
    assert_eq!(
        validate_fee_split(3600, 3300, 1100).unwrap_err(),
        ProgramError::Custom(51)
    );
    // Creator above floor. Because the three floors sum to exactly 8000, this
    // necessarily drags another leg under its floor too -- a single-violation
    // creator case does not exist. Same code either way.
    assert_eq!(
        validate_fee_split(3700, 3200, 1100).unwrap_err(),
        ProgramError::Custom(51)
    );
}
