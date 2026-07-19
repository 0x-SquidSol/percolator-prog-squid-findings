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
