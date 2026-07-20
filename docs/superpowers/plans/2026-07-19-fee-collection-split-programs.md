# Fee Collection Split — Programs Implementation Plan (Phase 1 of 4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make all four fee legs (protocol, creator, LP, insurance→stakers) collect real revenue on every ordinary trade, with safe defaults so no client call is required to turn revenue on.

**Architecture:** Collapse to a single fee rate `T = trade_fee_base_bps` with three stored share-bps fields. Split it at the three existing trade-fee sites using the `accrued`/`withdrawn` counter pattern the protocol fee already proves. Leave the backing fee dormant at 0 and the engine (`percolator`) completely untouched — zero upstream divergence.

**Tech Stack:** Rust, Solana SBF (`cargo build-sbf`), `bytemuck::Pod` zero-copy config, Kani formal verification, LiteSVM/surfpool integration tests.

**Spec:** `docs/superpowers/specs/2026-07-19-fee-collection-split-design.md`

## Global Constraints

- **Engine `~/v17/percolator` must not be modified.** Any diff to `percolator/src/v16.rs` fails this plan. Zero upstream divergence is the reason this approach was chosen.
- **Deploy and upgrade are USER-GATED.** No task in this plan deploys, upgrades, or sends a transaction to a live cluster. The plan ends by handing verified artifacts to the user.
- **No external actions.** No `gh issue create`, no publishing, no outward network calls.
- Wrapper repo: `~/v17/percolator-prog`, branch `feat/protocol-fee-taker-only`.
- Stake repo: `~/v17/percolator-stake`, branch `feat/adopt-stake-lineage-plus-n7`.
- Wrapper build flags: **default features** (`cargo build-sbf`). Never `--no-default-features` — that produces a different, legacy binary.
- Stake build flags: `--features devnet`.
- Error enum ordinals are asserted in `tests/v16_kani.rs`. **Never reorder**; append only.
- `WrapperConfigV16` derives `bytemuck::Pod`, which **forbids implicit padding**. All padding must be explicit.
- Every new assertion must be mutation-proven: perturb expectation → observe FAIL → restore → observe PASS.
- Tests assert **exact** error codes via `expect_custom`, never "any error".

## Constants (exact values, used across tasks)

| Name | Value |
|---|---|
| `PROTOCOL_FEE_BPS` (existing) | 2000 |
| `DEFAULT_CREATOR_SHARE_BPS` | 1600 |
| `DEFAULT_LP_SHARE_BPS` | 4800 |
| `DEFAULT_INSURANCE_SHARE_BPS` | 1600 |
| `FEE_SHARE_TOTAL_BPS` (= 10000 − protocol) | 8000 |
| `MAX_CREATOR_SHARE_BPS` | 3600 |
| `MIN_LP_SHARE_BPS` | 3200 |
| `MIN_INSURANCE_SHARE_BPS` | 1200 |

**Floor derivation (do not "simplify" these numbers).** The decided floors — creator ≤45%, LP ≥40%, insurance ≥15% — are percentages of the **post-protocol remainder**, which is what the on-chain doc means by "non-protocol-remainder floors". Shares are stored as bps **of T** and sum to 8000, so each floor converts as `pct × 8000`: `0.45×8000 = 3600`, `0.40×8000 = 3200`, `0.15×8000 = 1200`. These sum to exactly 8000, so the three floors are precisely complementary — the constraint space is tight but non-empty, and the defaults (1600/4800/1600 = 20%/60%/20% of the remainder) sit comfortably inside it.

A consequence worth knowing when writing tests: because the floors sum to exactly the budget, **creator > 3600 always forces a second violation** — a single-violation creator case does not exist. `FeeSplitFloorViolation` is one code for all three floors, so this is harmless, but do not waste time trying to construct one.
| `WRAPPER_CONFIG_LEN` old → new | 496 → 576 |
| New tags | 86 `UpdateFeeSplit`, 87 `WithdrawInsuranceReserveToStake`, 88 `UpdateMaintenanceFeePerSlot` |
| New errors | 52 `FeeSplitSumInvalid`, 53 `NoInsuranceReserveToClaim` (51 `FeeSplitFloorViolation` **already exists** — reuse it) |

## File Structure

| File | Responsibility |
|---|---|
| `src/v16_program.rs` `policy_v16` mod (5404-5765) | `split_trade_fee` pure function + floor/sum validators. Pure, no I/O, Kani-provable. |
| `src/v16_program.rs` struct `WrapperConfigV16` (~936-947) | New share fields + counters. |
| `src/v16_program.rs` `processor` mod | Three fee call sites; new handlers for tags 86/87/88; tag 78 repoint. |
| `tests/v16_fee_split.rs` (new) | Unit + integration tests for the split. |
| `tests/v16_kani.rs` (modify) | Conservation/monotonicity proofs, error-ordinal assertions. |
| `~/v17/percolator-stake/src/processor.rs` | Mode-0 fee accrual gate. |
| `~/v17/percolator-stake/src/state.rs` | Mode-0 fee term in `total_pool_value`. |

---

### Task 1: Pure split function + conservation tests

**Files:**
- Modify: `~/v17/percolator-prog/src/v16_program.rs` (inside `pub mod policy_v16`, after `fee_split_floor_ok`, ~line 5760)
- Test: `~/v17/percolator-prog/tests/v16_fee_split.rs` (create)

**Interfaces:**
- Consumes: nothing (first task).
- Produces: `policy_v16::FeeSplitParts { protocol: u128, creator: u128, lp: u128, insurance: u128 }` and
  `policy_v16::split_trade_fee(fee: u128, protocol_bps: u16, creator_bps: u16, lp_bps: u16, insurance_bps: u16) -> Result<FeeSplitParts, ProgramError>`.
  Takes plain bps rather than `&WrapperConfigV16` so it stays free of the zero-copy type and is trivially Kani-provable.

- [ ] **Step 1: Write the failing tests**

Create `~/v17/percolator-prog/tests/v16_fee_split.rs`:

```rust
//! Fee-split collection tests (2026-07-19 design).
//! The split is exact: protocol + creator + lp + insurance == fee, always.

use percolator_prog::v16_program::policy_v16::{split_trade_fee, FeeSplitParts};

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
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd ~/v17/percolator-prog && cargo test --test v16_fee_split 2>&1 | tail -20`
Expected: FAIL — `unresolved import`/`cannot find function split_trade_fee`.

- [ ] **Step 3: Implement `split_trade_fee`**

In `src/v16_program.rs`, inside `pub mod policy_v16`, after `fee_split_floor_ok`:

```rust
    /// Exact four-way split of a trade fee (2026-07-19 fee-collection design).
    ///
    /// Each leg is `floor(fee * share_bps / 10_000)`; the residual dust is
    /// assigned to `insurance` — the most conservative destination, since it
    /// grows the backstop rather than anyone's withdrawable revenue.
    ///
    /// CONSERVATION INVARIANT (Kani-proven, `kani_fee_split_conserves`):
    ///   protocol + creator + lp + insurance == fee, for every input.
    /// Violating it desyncs `header.insurance` and the vault accounting with it.
    ///
    /// Takes plain bps rather than `&WrapperConfigV16` so it stays free of the
    /// zero-copy type and is directly Kani-provable (same rationale as
    /// `maintenance_cranker_reward`).
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct FeeSplitParts {
        pub protocol: u128,
        pub creator: u128,
        pub lp: u128,
        pub insurance: u128,
    }

    pub fn split_trade_fee(
        fee: u128,
        protocol_bps: u16,
        creator_bps: u16,
        lp_bps: u16,
        insurance_bps: u16,
    ) -> Result<FeeSplitParts, ProgramError> {
        if fee == 0 {
            return Ok(FeeSplitParts::default());
        }
        let cut = |bps: u16| -> Result<u128, ProgramError> {
            fee.checked_mul(bps as u128)
                .map(|v| v / 10_000)
                .ok_or_else(|| PercolatorError::EngineArithmeticOverflow.into())
        };
        let protocol = cut(protocol_bps)?;
        let creator = cut(creator_bps)?;
        let lp = cut(lp_bps)?;
        // Insurance takes its floor share PLUS all residual dust, so the four
        // parts sum to exactly `fee` regardless of rounding.
        let assigned = protocol
            .checked_add(creator)
            .and_then(|v| v.checked_add(lp))
            .ok_or(PercolatorError::EngineArithmeticOverflow)?;
        let insurance = fee
            .checked_sub(assigned)
            .ok_or(PercolatorError::EngineCounterUnderflow)?;
        Ok(FeeSplitParts { protocol, creator, lp, insurance })
    }
```

Note: `insurance` is computed as the remainder rather than as its own floor, which is what makes conservation exact by construction. The `dust_goes_to_insurance` test asserts it is still at least its floor share.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd ~/v17/percolator-prog && cargo test --test v16_fee_split 2>&1 | tail -20`
Expected: `test result: ok. 4 passed`

- [ ] **Step 5: Mutation-proof the conservation test**

Temporarily change `assert_eq!(parts.protocol, 2_000)` to `2_001` in the first test. Run the tests.
Expected: FAIL. Restore to `2_000`. Run again. Expected: PASS.
This confirms the test is not vacuous.

- [ ] **Step 6: Commit**

```bash
cd ~/v17/percolator-prog
git add src/v16_program.rs tests/v16_fee_split.rs
git commit -m "feat(fee-split): exact four-way split_trade_fee with dust-to-insurance

Pure function in policy_v16, no zero-copy dependency, Kani-ready.
Conservation (sum of parts == fee) holds by construction because
insurance takes the remainder rather than its own floor."
```

---

### Task 2: Kani proofs for conservation and monotonicity

**Files:**
- Modify: `~/v17/percolator-prog/tests/v16_kani.rs`

**Interfaces:**
- Consumes: `policy_v16::split_trade_fee` from Task 1.
- Produces: proof harnesses `kani_fee_split_conserves`, `kani_fee_split_no_leg_exceeds_fee`.

- [ ] **Step 1: Add the proof harnesses**

Append to `~/v17/percolator-prog/tests/v16_kani.rs`:

```rust
/// Conservation: the four legs sum to exactly the input fee, for EVERY input.
/// This is the invariant `header.insurance` accounting depends on.
///
/// Widths are declared honestly (u16 shares, u128 fee bounded to a realistic
/// range) rather than assumed on wider types -- CBMC bit-blasts by DECLARED
/// width, and an unbounded u128 previously caused a 10h57m non-convergence.
#[cfg(kani)]
#[kani::proof]
fn kani_fee_split_conserves() {
    let fee: u64 = kani::any();
    let creator_bps: u16 = kani::any();
    let lp_bps: u16 = kani::any();
    kani::assume(creator_bps <= 8_000);
    kani::assume(lp_bps <= 8_000);
    kani::assume(creator_bps as u32 + lp_bps as u32 <= 8_000);
    let insurance_bps: u16 = 8_000 - creator_bps - lp_bps;

    let parts = percolator_prog::v16_program::policy_v16::split_trade_fee(
        fee as u128, 2_000, creator_bps, lp_bps, insurance_bps,
    )
    .unwrap();

    assert!(parts.protocol + parts.creator + parts.lp + parts.insurance == fee as u128);
}

/// No single leg may exceed the whole fee (guards a sign/overflow regression).
#[cfg(kani)]
#[kani::proof]
fn kani_fee_split_no_leg_exceeds_fee() {
    let fee: u64 = kani::any();
    let parts = percolator_prog::v16_program::policy_v16::split_trade_fee(
        fee as u128, 2_000, 1_600, 4_800, 1_600,
    )
    .unwrap();
    assert!(parts.protocol <= fee as u128);
    assert!(parts.creator <= fee as u128);
    assert!(parts.lp <= fee as u128);
    assert!(parts.insurance <= fee as u128);
}
```

- [ ] **Step 2: Run the proofs**

Run: `cd ~/v17/percolator-prog && cargo kani --tests --harness kani_fee_split_conserves --harness kani_fee_split_no_leg_exceeds_fee 2>&1 | tail -30`

**`--tests` is REQUIRED.** The harnesses live in `tests/v16_kani.rs`, and Kani does not scan integration-test targets without it. Omitting it fails with `error: no harnesses matched the harness filters` after a full dependency rebuild — which reads like a slow hang, not a flag error. This repo's convention is `cargo kani --tests` (see `README.md:654`, `kani_audit.md:3` — 81/81 harnesses in 11m05s).
Expected: `VERIFICATION:- SUCCESSFUL` for both.

If a harness runs past ~10 minutes, stop it: the cause is a declared width too wide, not a logic error. Narrow `fee` to `u32` and re-run.

- [ ] **Step 3: Mutation-proof the conservation proof**

Temporarily change `split_trade_fee`'s `insurance` computation from the remainder to `cut(insurance_bps)?`. Re-run `kani_fee_split_conserves`.
Expected: `VERIFICATION:- FAILED` (dust is now unassigned). Restore the remainder form. Re-run. Expected: SUCCESSFUL.
This proves the harness actually constrains the property.

- [ ] **Step 4: Commit**

```bash
cd ~/v17/percolator-prog
git add tests/v16_kani.rs
git commit -m "test(fee-split): Kani proofs for conservation and per-leg bounds

Widths declared honestly (u64 fee, u16 shares) -- CBMC bit-blasts by
declared width, which caused a prior 10h57m non-convergence."
```

---

### Task 3: Config schema — fields, size bump, safe defaults

**Files:**
- Modify: `~/v17/percolator-prog/src/v16_program.rs:58` (`WRAPPER_CONFIG_LEN`)
- Modify: `~/v17/percolator-prog/src/v16_program.rs:936-947` (struct tail)
- Modify: `~/v17/percolator-prog/src/v16_program.rs:7018-7021` (`InitMarket` config literal)
- Modify: `~/v17/percolator-prog/src/v16_program.rs` constants module (near `PROTOCOL_FEE_BPS`, ~line 115)
- Test: `~/v17/percolator-prog/tests/v16_fee_split.rs`

**Interfaces:**
- Consumes: nothing from prior tasks.
- Produces: `cfg.creator_share_bps`, `cfg.lp_share_bps`, `cfg.insurance_share_bps` (all `u16`);
  `cfg.lp_fee_accrued_atoms`, `cfg.lp_fee_withdrawn_atoms`, `cfg.insurance_reserve_accrued_atoms`,
  `cfg.insurance_reserve_withdrawn_atoms` (all `u128`); constants listed in the Constants table.

- [ ] **Step 1: Add the constants**

In the `constants` module of `src/v16_program.rs`, immediately after `PROTOCOL_FEE_BPS` (~line 115):

```rust
    /// Fee-split defaults (2026-07-19 design). Written unconditionally at
    /// InitMarket, never caller-supplied -- a market that never calls
    /// UpdateFeeSplit still pays all four legs correctly from its first trade.
    pub const DEFAULT_CREATOR_SHARE_BPS: u16 = 1600;
    pub const DEFAULT_LP_SHARE_BPS: u16 = 4800;
    pub const DEFAULT_INSURANCE_SHARE_BPS: u16 = 1600;
    /// The three stored shares must sum to exactly this (= 10_000 - PROTOCOL_FEE_BPS).
    pub const FEE_SHARE_TOTAL_BPS: u16 = 10_000 - PROTOCOL_FEE_BPS;
    /// Decided floors (creator <=45%, LP >=40%, insurance >=15%) are
    /// percentages of the POST-PROTOCOL REMAINDER. Shares are stored as bps of
    /// T summing to FEE_SHARE_TOTAL_BPS (8000), so each floor is `pct * 8000`.
    /// These three sum to exactly 8000, i.e. they are precisely complementary.
    pub const MAX_CREATOR_SHARE_BPS: u16 = 3600; // 45% of the remainder
    pub const MIN_LP_SHARE_BPS: u16 = 3200;      // 40% of the remainder
    pub const MIN_INSURANCE_SHARE_BPS: u16 = 1200; // 15% of the remainder
```

- [ ] **Step 2: Add struct fields (order is load-bearing)**

In `WrapperConfigV16`, replace the trailing `protocol_fee_withdrawn_atoms: u128,` line with:

```rust
        pub protocol_fee_withdrawn_atoms: u128,
        // ── Fee-collection split (2026-07-19 design; 496 -> 576 B) ──────────
        // FIELD ORDER IS LOAD-BEARING. This struct derives bytemuck::Pod, which
        // forbids IMPLICIT padding. The struct ends at 496 B (a multiple of 16,
        // so u128-aligned). Placing the u16 shares first would push these u128s
        // to offset 502, forcing the compiler to insert implicit padding and
        // FAILING the Pod derive. Counters first (496->560), then shares (566),
        // then explicit padding to the 16-byte alignment boundary (576).
        /// Cumulative atoms accrued to the LP vault's claim. Monotonic.
        /// Claimed via `LpVaultCrankFees` (tag 78) into vault NAV.
        pub lp_fee_accrued_atoms: u128,
        /// Cumulative atoms already credited to the vault. `<= lp_fee_accrued_atoms`.
        pub lp_fee_withdrawn_atoms: u128,
        /// Cumulative atoms accrued to the insurance/staker leg. Monotonic.
        /// Claimed via `WithdrawInsuranceReserveToStake` (tag 87).
        pub insurance_reserve_accrued_atoms: u128,
        /// Cumulative atoms already pushed to the stake vault. `<= accrued`.
        pub insurance_reserve_withdrawn_atoms: u128,
        /// Creator's share of T in bps. Floor: <= MAX_CREATOR_SHARE_BPS.
        pub creator_share_bps: u16,
        /// LP's share of T in bps. Floor: >= MIN_LP_SHARE_BPS.
        pub lp_share_bps: u16,
        /// Insurance/staker share of T in bps. Floor: >= MIN_INSURANCE_SHARE_BPS.
        pub insurance_share_bps: u16,
        /// Explicit padding to the struct's 16-byte alignment. Explicit because
        /// bytemuck::Pod forbids implicit padding.
        pub _padding_split: [u8; 10],
```

- [ ] **Step 3: Bump the length constant**

At `src/v16_program.rs:58`:

```rust
    pub const WRAPPER_CONFIG_LEN: usize = 576;
```

- [ ] **Step 4: Set safe defaults at InitMarket**

In the config literal (~line 7019), after `protocol_fee_withdrawn_atoms: 0,`:

```rust
            protocol_fee_withdrawn_atoms: 0,
            // Fee-split: hardcoded defaults, never caller-supplied, so no market
            // can be created with a zero or hostile split. A market that never
            // calls UpdateFeeSplit still pays all four legs correctly.
            lp_fee_accrued_atoms: 0,
            lp_fee_withdrawn_atoms: 0,
            insurance_reserve_accrued_atoms: 0,
            insurance_reserve_withdrawn_atoms: 0,
            creator_share_bps: constants::DEFAULT_CREATOR_SHARE_BPS,
            lp_share_bps: constants::DEFAULT_LP_SHARE_BPS,
            insurance_share_bps: constants::DEFAULT_INSURANCE_SHARE_BPS,
            _padding_split: [0u8; 10],
```

- [ ] **Step 5: Add the size + defaults test**

Append to `tests/v16_fee_split.rs`:

```rust
#[test]
fn config_size_is_576_and_16_byte_aligned() {
    use percolator_prog::v16_program::state::WrapperConfigV16;
    use percolator_prog::v16_program::constants::WRAPPER_CONFIG_LEN;
    assert_eq!(core::mem::size_of::<WrapperConfigV16>(), 576);
    assert_eq!(WRAPPER_CONFIG_LEN, 576);
    assert_eq!(576 % core::mem::align_of::<WrapperConfigV16>(), 0);
}

#[test]
fn default_shares_sum_to_total_and_satisfy_floors() {
    use percolator_prog::v16_program::constants::*;
    assert_eq!(
        DEFAULT_CREATOR_SHARE_BPS + DEFAULT_LP_SHARE_BPS + DEFAULT_INSURANCE_SHARE_BPS,
        FEE_SHARE_TOTAL_BPS
    );
    assert!(DEFAULT_CREATOR_SHARE_BPS <= MAX_CREATOR_SHARE_BPS);
    assert!(DEFAULT_LP_SHARE_BPS >= MIN_LP_SHARE_BPS);
    assert!(DEFAULT_INSURANCE_SHARE_BPS >= MIN_INSURANCE_SHARE_BPS);
}
```

Adjust the two `use` paths if the crate exposes these under different module paths; run `cargo test` and follow the compiler's suggestion.

- [ ] **Step 6: Build and test**

Run: `cd ~/v17/percolator-prog && cargo build 2>&1 | tail -20 && cargo test --test v16_fee_split 2>&1 | tail -20`
Expected: build succeeds (the `const _: () = assert!(size_of == WRAPPER_CONFIG_LEN)` guard passes), 6 tests pass.

If the build fails with a `Pod` derive error about padding, the field order is wrong — counters must precede the `u16` shares.

- [ ] **Step 7: Commit**

```bash
cd ~/v17/percolator-prog
git add src/v16_program.rs tests/v16_fee_split.rs
git commit -m "feat(fee-split): config schema, 496->576 B, safe defaults at InitMarket

Counters precede u16 shares because bytemuck::Pod forbids implicit
padding and the struct ends 16-aligned at 496. Defaults are hardcoded,
not caller-supplied, so a market that never calls a setter still pays
all four legs."
```

---

### Task 4: Wire the single-trade fee site

**Files:**
- Modify: `~/v17/percolator-prog/src/v16_program.rs:7361-7387`

**Interfaces:**
- Consumes: `split_trade_fee` (Task 1), config fields (Task 3).
- Produces: on every non-batch trade, `cfg.protocol_fee_accrued_atoms`, `cfg.lp_fee_accrued_atoms`, and `cfg.insurance_reserve_accrued_atoms` all increase; `domain_fee_a`/`domain_fee_b` carry only the creator share.

- [ ] **Step 1: Replace the protocol-only skim**

Replace lines 7361-7387 (from `let protocol_cut_a =` through the closing `}` of `if protocol_cut_total != 0 {`) with:

```rust
            // Four-way split (2026-07-19 design). Taker-only (§1A) guarantees
            // exactly one of outcome.fee_a/fee_b is nonzero, so splitting 0 is
            // all-zeros and the maker's domain gets exactly the 0 credit it
            // should -- no special-casing needed.
            let split_a = policy_v16::split_trade_fee(
                outcome.fee_a,
                constants::PROTOCOL_FEE_BPS,
                cfg.creator_share_bps,
                cfg.lp_share_bps,
                cfg.insurance_share_bps,
            )?;
            let split_b = policy_v16::split_trade_fee(
                outcome.fee_b,
                constants::PROTOCOL_FEE_BPS,
                cfg.creator_share_bps,
                cfg.lp_share_bps,
                cfg.insurance_share_bps,
            )?;
            // The creator leg keeps the pre-existing domain-budget path; only
            // its AMOUNT changes (a configured share, not "everything left").
            let domain_fee_a = split_a.creator;
            let domain_fee_b = split_b.creator;

            let protocol_cut_total = split_a
                .protocol
                .checked_add(split_b.protocol)
                .ok_or(PercolatorError::EngineArithmeticOverflow)?;
            let lp_cut_total = split_a
                .lp
                .checked_add(split_b.lp)
                .ok_or(PercolatorError::EngineArithmeticOverflow)?;
            let insurance_cut_total = split_a
                .insurance
                .checked_add(split_b.insurance)
                .ok_or(PercolatorError::EngineArithmeticOverflow)?;

            if protocol_cut_total != 0 || lp_cut_total != 0 || insurance_cut_total != 0 {
                cfg.protocol_fee_accrued_atoms = cfg
                    .protocol_fee_accrued_atoms
                    .checked_add(protocol_cut_total)
                    .ok_or(PercolatorError::EngineArithmeticOverflow)?;
                cfg.lp_fee_accrued_atoms = cfg
                    .lp_fee_accrued_atoms
                    .checked_add(lp_cut_total)
                    .ok_or(PercolatorError::EngineArithmeticOverflow)?;
                cfg.insurance_reserve_accrued_atoms = cfg
                    .insurance_reserve_accrued_atoms
                    .checked_add(insurance_cut_total)
                    .ok_or(PercolatorError::EngineArithmeticOverflow)?;
                // CRITICAL: force the write-back below even if nothing else in
                // this instruction would otherwise have dirtied cfg. The cfg_after
                // pattern is opt-in per mutation -- a missed write-back here
                // SILENTLY DISCARDS accrued fees for all three legs.
                cfg_after = Some(cfg);
            }
```

- [ ] **Step 2: Build**

Run: `cd ~/v17/percolator-prog && cargo build 2>&1 | tail -20`
Expected: success.

- [ ] **Step 3: Run the existing wrapper suite to catch regressions**

Run: `cd ~/v17/percolator-prog && cargo test --test v16_wrapper 2>&1 | tail -25`
Expected: all pass. Any fee-amount assertion that now fails is expected to be a *real* change (the creator leg dropped from 80% to 16%) — update those expectations to the new split and note each one in the commit message. Do not weaken an assertion to make it pass.

- [ ] **Step 4: Commit**

```bash
cd ~/v17/percolator-prog
git add src/v16_program.rs tests/
git commit -m "feat(fee-split): route the single-trade fee site through split_trade_fee

All four legs now accrue on every ordinary trade. The creator leg keeps
its existing domain-budget path; only its amount changes from
'everything left over' to a configured share."
```

---

### Task 5: Wire the batch fee site

**Files:**
- Modify: `~/v17/percolator-prog/src/v16_program.rs:7663-7672` and the surrounding accumulator block (~7651-7700)

**Interfaces:**
- Consumes: `split_trade_fee` (Task 1), config fields (Task 3).
- Produces: per-leg accrual identical in effect to Task 4, accumulated across legs into `lp_cut_running_total` and `insurance_cut_running_total` alongside the existing `protocol_cut_running_total`.

- [ ] **Step 1: Add the running totals**

At ~line 7652, beside `let mut protocol_cut_running_total: u128 = 0;`:

```rust
                let mut lp_cut_running_total: u128 = 0;
                let mut insurance_cut_running_total: u128 = 0;
```

- [ ] **Step 2: Replace the per-leg skim**

Replace the `let protocol_cut_leg = ...` / `let domain_amount_leg = ...` / `protocol_cut_running_total = ...` block (~7663-7672) with:

```rust
                    let split_leg = policy_v16::split_trade_fee(
                        fee_leg,
                        constants::PROTOCOL_FEE_BPS,
                        cfg.creator_share_bps,
                        cfg.lp_share_bps,
                        cfg.insurance_share_bps,
                    )?;
                    let domain_amount_leg = split_leg.creator;
                    protocol_cut_running_total = protocol_cut_running_total
                        .checked_add(split_leg.protocol)
                        .ok_or(PercolatorError::EngineArithmeticOverflow)?;
                    lp_cut_running_total = lp_cut_running_total
                        .checked_add(split_leg.lp)
                        .ok_or(PercolatorError::EngineArithmeticOverflow)?;
                    insurance_cut_running_total = insurance_cut_running_total
                        .checked_add(split_leg.insurance)
                        .ok_or(PercolatorError::EngineArithmeticOverflow)?;
```

- [ ] **Step 3: Accrue the batch totals after the loop**

Find where `protocol_cut_running_total` is folded into `cfg.protocol_fee_accrued_atoms` after the loop (search: `grep -n "protocol_cut_running_total" src/v16_program.rs`). Extend that block so all three totals accrue and `cfg_dirty` is set when **any** is nonzero:

```rust
            if protocol_cut_running_total != 0
                || lp_cut_running_total != 0
                || insurance_cut_running_total != 0
            {
                cfg.protocol_fee_accrued_atoms = cfg
                    .protocol_fee_accrued_atoms
                    .checked_add(protocol_cut_running_total)
                    .ok_or(PercolatorError::EngineArithmeticOverflow)?;
                cfg.lp_fee_accrued_atoms = cfg
                    .lp_fee_accrued_atoms
                    .checked_add(lp_cut_running_total)
                    .ok_or(PercolatorError::EngineArithmeticOverflow)?;
                cfg.insurance_reserve_accrued_atoms = cfg
                    .insurance_reserve_accrued_atoms
                    .checked_add(insurance_cut_running_total)
                    .ok_or(PercolatorError::EngineArithmeticOverflow)?;
                cfg_dirty = true;
            }
```

- [ ] **Step 4: Build and test**

Run: `cd ~/v17/percolator-prog && cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -30`
Expected: build succeeds, suite passes (with the same "real change, update expectation" rule as Task 4 Step 3).

- [ ] **Step 5: Commit**

```bash
cd ~/v17/percolator-prog
git add src/v16_program.rs tests/
git commit -m "feat(fee-split): route the batch fee site through split_trade_fee"
```

---

### Task 6: New error codes + `UpdateFeeSplit` (tag 86)

**Files:**
- Modify: `~/v17/percolator-prog/src/v16_program.rs:317` (error enum tail, after `FeeSplitFloorViolation`)
- Modify: `~/v17/percolator-prog/src/v16_program.rs:~3826` (`ProgInstruction` enum), `~4142` (dispatch), plus a new handler
- Modify: `~/v17/percolator-prog/tests/v16_kani.rs` (ordinal assertions)
- Test: `~/v17/percolator-prog/tests/v16_fee_split.rs`

**Interfaces:**
- Consumes: constants (Task 3).
- Produces: `PercolatorError::FeeSplitSumInvalid` (52), `PercolatorError::NoInsuranceReserveToClaim` (53);
  `policy_v16::fee_split_shares_ok(creator: u16, lp: u16, insurance: u16) -> bool`;
  instruction `UpdateFeeSplit { creator_share_bps: u16, lp_share_bps: u16, insurance_share_bps: u16 }` at tag 86.
  **`FeeSplitFloorViolation` (51) already exists — reuse it, do not add a duplicate.**

- [ ] **Step 1: Append the two new error variants**

After `FeeSplitFloorViolation, // Custom(51)`:

```rust
        // ── Fee-collection split (2026-07-19) ───────────────────────────────
        // Appended after FeeSplitFloorViolation (ordinal 51). Do NOT reorder.
        /// `UpdateFeeSplit` shares do not sum to exactly
        /// `FEE_SHARE_TOTAL_BPS` (= 10_000 - PROTOCOL_FEE_BPS).
        /// SDK agent: add `FeeSplitSumInvalid = 52` to the client error map.
        FeeSplitSumInvalid, // Custom(52)
        /// `WithdrawInsuranceReserveToStake` called with nothing available
        /// (`insurance_reserve_accrued == insurance_reserve_withdrawn`).
        /// SDK agent: add `NoInsuranceReserveToClaim = 53` to the client error map.
        NoInsuranceReserveToClaim, // Custom(53)
```

- [ ] **Step 2: Add the ordinal assertions**

In `tests/v16_kani.rs`, alongside the existing ordinal assertions:

```rust
#[test]
fn fee_split_error_ordinals_are_pinned() {
    use percolator_prog::v16_program::PercolatorError;
    assert_eq!(PercolatorError::FeeSplitFloorViolation as u32, 51);
    assert_eq!(PercolatorError::FeeSplitSumInvalid as u32, 52);
    assert_eq!(PercolatorError::NoInsuranceReserveToClaim as u32, 53);
}
```

- [ ] **Step 3: Add the share validator**

In `pub mod policy_v16`, after `split_trade_fee`:

```rust
    /// Exact fee-split validation for `UpdateFeeSplit` (2026-07-19 design).
    ///
    /// Replaces `fee_split_floor_ok`'s tolerance-based two-rate check: with a
    /// single rate there is no cross-rate rounding to absorb, so this is an
    /// exact integer comparison with no tolerance and no skip path.
    ///
    /// Returns Ok(()) or the specific error. MUST be called ONLY from the
    /// UpdateFeeSplit handler -- never from a load-time validator, because
    /// `validate_wrapper_config` runs on every deserialize and a floor there
    /// would retroactively brick markets whose stored split predates it.
    pub fn validate_fee_split(
        creator_bps: u16,
        lp_bps: u16,
        insurance_bps: u16,
    ) -> Result<(), ProgramError> {
        let sum = creator_bps as u32 + lp_bps as u32 + insurance_bps as u32;
        if sum != crate::v16_program::constants::FEE_SHARE_TOTAL_BPS as u32 {
            return Err(PercolatorError::FeeSplitSumInvalid.into());
        }
        if creator_bps > crate::v16_program::constants::MAX_CREATOR_SHARE_BPS
            || lp_bps < crate::v16_program::constants::MIN_LP_SHARE_BPS
            || insurance_bps < crate::v16_program::constants::MIN_INSURANCE_SHARE_BPS
        {
            return Err(PercolatorError::FeeSplitFloorViolation.into());
        }
        Ok(())
    }
```

Adjust the `crate::v16_program::constants::` paths to whatever resolves inside `policy_v16` (likely bare `constants::`); the compiler will say.

- [ ] **Step 4: Add the instruction variant**

In `ProgInstruction`, after `SetProtocolFeeAuthority`:

```rust
        /// UpdateFeeSplit (tag 86) — sets the three fee-split shares.
        /// Gated on `marketauth`. Shares must sum to FEE_SHARE_TOTAL_BPS and
        /// satisfy the floors (creator <=45%, LP >=40%, insurance >=15%).
        UpdateFeeSplit {
            creator_share_bps: u16,
            lp_share_bps: u16,
            insurance_share_bps: u16,
        },
```

- [ ] **Step 5: Add decoding and dispatch**

In the decode match, after arm `85`:

```rust
                86 => {
                    if data.len() < 7 {
                        return Err(ProgramError::InvalidInstructionData);
                    }
                    Self::UpdateFeeSplit {
                        creator_share_bps: u16::from_le_bytes([data[1], data[2]]),
                        lp_share_bps: u16::from_le_bytes([data[3], data[4]]),
                        insurance_share_bps: u16::from_le_bytes([data[5], data[6]]),
                    }
                }
```

In the processor dispatch, beside `handle_set_protocol_fee_authority`:

```rust
            ProgInstruction::UpdateFeeSplit {
                creator_share_bps,
                lp_share_bps,
                insurance_share_bps,
            } => handle_update_fee_split(
                program_id,
                accounts,
                creator_share_bps,
                lp_share_bps,
                insurance_share_bps,
            ),
```

- [ ] **Step 6: Add the handler**

Place next to `handle_set_protocol_fee_authority` (~line 10227), following that function's account-loading and marketauth-gating pattern exactly (read the neighbouring `handle_update_trade_fee_policy` for the precise marketauth check and cfg write-back idiom, and mirror it):

```rust
    /// UpdateFeeSplit (tag 86) — marketauth-gated. Validates the shares, then
    /// stores them. Validation lives ONLY here, never in a load-time validator.
    fn handle_update_fee_split<'a>(
        program_id: &Pubkey,
        accounts: &[AccountInfo<'a>],
        creator_share_bps: u16,
        lp_share_bps: u16,
        insurance_share_bps: u16,
    ) -> ProgramResult {
        policy_v16::validate_fee_split(creator_share_bps, lp_share_bps, insurance_share_bps)?;
        // Load market + cfg and enforce the marketauth signer gate exactly as
        // handle_update_trade_fee_policy does, then:
        cfg.creator_share_bps = creator_share_bps;
        cfg.lp_share_bps = lp_share_bps;
        cfg.insurance_share_bps = insurance_share_bps;
        // write cfg back via the same idiom the neighbouring setter uses
        Ok(())
    }
```

- [ ] **Step 7: Add negative tests with exact codes**

Append to `tests/v16_fee_split.rs`:

```rust
#[test]
fn validate_fee_split_accepts_defaults() {
    use percolator_prog::v16_program::policy_v16::validate_fee_split;
    assert!(validate_fee_split(1600, 4800, 1600).is_ok());
}

#[test]
fn validate_fee_split_accepts_both_floor_extremes() {
    use percolator_prog::v16_program::policy_v16::validate_fee_split;
    // The three floors sum to exactly 8000, so this is the ONLY point where
    // all three bind simultaneously. It must be accepted, not rejected.
    assert!(validate_fee_split(3600, 3200, 1200).is_ok());
}

#[test]
fn validate_fee_split_rejects_wrong_sum_with_exact_code() {
    use percolator_prog::v16_program::policy_v16::validate_fee_split;
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
    use percolator_prog::v16_program::policy_v16::validate_fee_split;
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
```

- [ ] **Step 8: Build, test, mutation-proof, commit**

Run: `cd ~/v17/percolator-prog && cargo build 2>&1 | tail -20 && cargo test --test v16_fee_split --test v16_kani 2>&1 | tail -25`
Expected: all pass.

Mutation-proof: change `ProgramError::Custom(52)` to `Custom(53)` in the sum test → must FAIL → restore → PASS.

```bash
cd ~/v17/percolator-prog
git add src/v16_program.rs tests/
git commit -m "feat(fee-split): UpdateFeeSplit (tag 86) + errors 52/53

Floors are the decided percentages of the post-protocol remainder,
converted to bps-of-T (0.45/0.40/0.15 x 8000 = 3600/3200/1200). They sum
to exactly 8000, so they are precisely complementary. Validation lives
only in the setter, never in a load-time validator -- a floor there
would retroactively brick existing markets."
```

---

### Task 7: Repoint `LpVaultCrankFees` (tag 78) at the LP counter

**Files:**
- Modify: `~/v17/percolator-prog/src/v16_program.rs:~14094` (the `LpVaultNoFeesToCrank` site) and its enclosing handler
- Test: `~/v17/percolator-prog/tests/v16_fork_lp_vault_admin.rs`

**Interfaces:**
- Consumes: `cfg.lp_fee_accrued_atoms` / `cfg.lp_fee_withdrawn_atoms` (Task 3).
- Produces: tag 78 credits `available = lp_fee_accrued_atoms - lp_fee_withdrawn_atoms` into vault NAV and advances `lp_fee_withdrawn_atoms` by exactly that amount; returns `LpVaultNoFeesToCrank` (38) when `available == 0`.

- [ ] **Step 1: Read the current handler**

Run: `cd ~/v17/percolator-prog && sed -n '14060,14115p' src/v16_program.rs`
Identify where it reads `bucket.utilization_fee_earnings` and where it credits the vault ledger.

- [ ] **Step 2: Replace the earnings source**

Change the source from `bucket.utilization_fee_earnings` to:

```rust
        let available = cfg
            .lp_fee_accrued_atoms
            .checked_sub(cfg.lp_fee_withdrawn_atoms)
            .ok_or(PercolatorError::EngineCounterUnderflow)?;
        if available == 0 {
            return Err(PercolatorError::LpVaultNoFeesToCrank.into());
        }
        // Credit the vault ledger with `available` using the handler's existing
        // ledger-credit call, then mark it withdrawn. Monotonic invariant:
        // lp_fee_withdrawn_atoms <= lp_fee_accrued_atoms, always.
        cfg.lp_fee_withdrawn_atoms = cfg
            .lp_fee_withdrawn_atoms
            .checked_add(available)
            .ok_or(PercolatorError::EngineArithmeticOverflow)?;
        cfg_after = Some(cfg);
```

Keep the existing NAV-credit call unchanged; only its input amount changes.

- [ ] **Step 3: Build and run the LP vault suite**

Run: `cd ~/v17/percolator-prog && cargo build 2>&1 | tail -10 && cargo test --test v16_fork_lp_vault_admin --test v16_fork_lp_vault_redeem 2>&1 | tail -25`
Expected: pass. Tests asserting `LpVaultNoFeesToCrank` on a zero-volume market still pass (available is 0 there).

- [ ] **Step 4: Commit**

```bash
cd ~/v17/percolator-prog
git add src/v16_program.rs tests/
git commit -m "feat(fee-split): LpVaultCrankFees pulls from lp_fee_accrued_atoms

Tag 78 previously dead-ended at LpVaultNoFeesToCrank because
bucket.utilization_fee_earnings is always 0 (the backing fee is
rate-0 and lien-gated). It now drains the real LP counter."
```

---

### Task 8: `WithdrawInsuranceReserveToStake` (tag 87)

**Files:**
- Modify: `~/v17/percolator-prog/src/v16_program.rs` (`ProgInstruction`, decode, dispatch, new handler)
- Test: `~/v17/percolator-prog/tests/v16_fee_split.rs`

**Interfaces:**
- Consumes: `cfg.insurance_reserve_accrued_atoms` / `..._withdrawn_atoms` (Task 3), `NoInsuranceReserveToClaim` (Task 6).
- Produces: instruction at tag 87, **permissionless** (no signer gate), transferring `accrued - withdrawn` from the market vault to the stake vault and advancing `..._withdrawn_atoms` by exactly the transferred amount.

- [ ] **Step 1: Add the instruction variant, decode arm (tag 87), and dispatch**

Follow the exact shape used for tag 86 in Task 6 Steps 4-5. The instruction carries no arguments:

```rust
        /// WithdrawInsuranceReserveToStake (tag 87) — permissionless. Pushes
        /// the accrued insurance/staker leg into the stake vault, producing
        /// exactly the vault surplus percolator-stake's AccrueFees measures.
        WithdrawInsuranceReserveToStake,
```

Decode arm: `87 => Self::WithdrawInsuranceReserveToStake,`

- [ ] **Step 2: Add the handler**

Model the token transfer and vault-authority PDA signing on `handle_withdraw_protocol_fee` (~line 10104), which already moves tokens out of the market vault:

```rust
    /// WithdrawInsuranceReserveToStake (tag 87) — permissionless.
    ///
    /// Transfers `insurance_reserve_accrued - insurance_reserve_withdrawn` to
    /// the stake vault. Permissionless because the destination is fixed by the
    /// stake pool PDA derivation, not by the caller -- there is nothing for a
    /// caller to redirect. This produces the vault surplus AccrueFees measures.
    fn handle_withdraw_insurance_reserve_to_stake<'a>(
        program_id: &Pubkey,
        accounts: &[AccountInfo<'a>],
    ) -> ProgramResult {
        // Load market + cfg exactly as handle_withdraw_protocol_fee does.
        let available = cfg
            .insurance_reserve_accrued_atoms
            .checked_sub(cfg.insurance_reserve_withdrawn_atoms)
            .ok_or(PercolatorError::EngineCounterUnderflow)?;
        if available == 0 {
            return Err(PercolatorError::NoInsuranceReserveToClaim.into());
        }
        // Clamp to u64 for the SPL transfer and to the vault's real balance,
        // mirroring handle_withdraw_protocol_fee's partial-fill handling.
        // Mark ONLY the transferred amount withdrawn.
        cfg.insurance_reserve_withdrawn_atoms = cfg
            .insurance_reserve_withdrawn_atoms
            .checked_add(transferred as u128)
            .ok_or(PercolatorError::EngineArithmeticOverflow)?;
        // Verify the destination is the stake pool's vault PDA before transferring.
        Ok(())
    }
```

Read `handle_withdraw_protocol_fee` in full and mirror its account validation, PDA signing, and partial-fill semantics — do not invent a new transfer idiom.

- [ ] **Step 3: Test the empty case with the exact code**

Append to `tests/v16_fee_split.rs` an integration test that calls tag 87 on a market with zero volume and asserts `ProgramError::Custom(53)`. Use the harness pattern from `tests/v16_fork_lp_vault_admin.rs` for building and submitting the instruction.

- [ ] **Step 4: Build, test, commit**

```bash
cd ~/v17/percolator-prog && cargo build 2>&1 | tail -10 && cargo test 2>&1 | tail -25
git add src/v16_program.rs tests/
git commit -m "feat(fee-split): WithdrawInsuranceReserveToStake (tag 87), permissionless"
```

---

### Task 9: `UpdateMaintenanceFeePerSlot` (tag 88)

**Files:**
- Modify: `~/v17/percolator-prog/src/v16_program.rs` (`ProgInstruction`, decode, dispatch, new handler)

**Interfaces:**
- Consumes: nothing from prior tasks.
- Produces: instruction `UpdateMaintenanceFeePerSlot { maintenance_fee_per_slot: u64 }` at tag 88, marketauth-gated.

- [ ] **Step 1: Add variant, decode (9 bytes: tag + u64), dispatch, handler**

```rust
        /// UpdateMaintenanceFeePerSlot (tag 88) — marketauth-gated.
        /// Closes a real defect: the value was an InitMarket constructor
        /// argument with NO setter anywhere in the dispatch table, so it was
        /// permanently frozen per market. Default remains 0; this restores
        /// optionality only and does not enable the maintenance fee.
        UpdateMaintenanceFeePerSlot { maintenance_fee_per_slot: u64 },
```

Handler mirrors `handle_update_trade_fee_policy`'s marketauth gate, writes `cfg.maintenance_fee_per_slot`, sets `cfg_after`.

- [ ] **Step 2: Test round-trip**

Add a test that sets it to a nonzero value, re-reads config, and asserts the value persisted; and one asserting a non-marketauth signer is rejected with the same code `handle_update_trade_fee_policy` uses for that case.

- [ ] **Step 3: Build, test, commit**

```bash
cd ~/v17/percolator-prog && cargo build 2>&1 | tail -10 && cargo test 2>&1 | tail -25
git add src/v16_program.rs tests/
git commit -m "feat: UpdateMaintenanceFeePerSlot (tag 88), setter only, default stays 0"
```

---

### Task 10: Retire `fee_split_floor_ok` from the setters

**Files:**
- Modify: `~/v17/percolator-prog/src/v16_program.rs` — `handle_update_trade_fee_policy` (~11692) and `handle_update_backing_fee_policy`

**Interfaces:**
- Consumes: `validate_fee_split` (Task 6) now owns split validation.
- Produces: no behavioural change to `T` or backing-fee shape validation; only the two-rate split check is removed.

- [ ] **Step 1: Remove the call sites**

Delete the `fee_split_floor_ok(...)` invocation and its surrounding comment block from both handlers. Keep every other check (`max_trading_fee_bps`, `MAX_DYNAMIC_TRADE_FEE_BPS`, `backing_trade_fee_policy_shape_ok`) exactly as-is.

- [ ] **Step 2: Deprecate, do not delete, the function**

Leave `fee_split_floor_ok` and `FEE_SPLIT_SHARE_TOLERANCE_FLAT` in `policy_v16` with a doc note so its Kani proofs and unit tests keep compiling:

```rust
    /// DEPRECATED (2026-07-19 fee-collection design): validated a two-rate
    /// split (`T = trade_fee_base_bps + backing_fee_bps`) that no longer
    /// exists. Superseded by `validate_fee_split`, which is exact rather than
    /// tolerance-based and has no `backing_fee_bps == 0` skip path -- this one
    /// skipped on every live market, so it never actually ran. Retained so its
    /// existing proofs and tests keep compiling; no live call sites remain.
```

- [ ] **Step 3: Confirm no live call sites remain**

Run: `cd ~/v17/percolator-prog && grep -n "fee_split_floor_ok" src/v16_program.rs`
Expected: only the definition and its doc comment — no calls from either handler.

- [ ] **Step 4: Build, test, commit**

```bash
cd ~/v17/percolator-prog && cargo build 2>&1 | tail -10 && cargo test 2>&1 | tail -25
git add src/v16_program.rs
git commit -m "refactor(fee-split): retire fee_split_floor_ok from the setters

Superseded by validate_fee_split. Kept (deprecated, uncalled) so its
proofs and tests keep compiling."
```

---

### Task 11: Stake — mode-0 fee accrual

**Files:**
- Modify: `~/v17/percolator-stake/src/processor.rs:2585-2589`
- Modify: `~/v17/percolator-stake/src/state.rs:597-601`
- Test: `~/v17/percolator-stake/tests/` (follow the existing test-file convention there)

**Interfaces:**
- Consumes: nothing from the wrapper tasks (independent repo).
- Produces: mode-0 pools accrue fees and count them in `total_pool_value`.

- [ ] **Step 1: Relax the AccrueFees mode gate**

At `src/processor.rs:2585-2589`, replace:

```rust
    // Only trading LP mode pools accrue fees
    if pool.pool_mode != 1 {
        msg!("AccrueFees: pool is not in trading LP mode");
        return Err(StakeError::InvalidPoolMode.into());
    }
```

with:

```rust
    // Modes 0 (insurance pool) and 1 (trading LP) both accrue fees.
    //
    // 2026-07-19 fee-collection design: mode-0 pools are the market's
    // loss-absorbing backstop and are now compensated with the insurance leg
    // of the trade-fee split, pushed in by the wrapper's
    // WithdrawInsuranceReserveToStake (tag 87). Restricting accrual to mode 1
    // left mode-0 stakers with a downside leg (FlushToInsurance) and NO upside
    // leg at all. Every real client calls InitPool (mode 0), so gating on
    // mode 1 made fee accrual unreachable in practice.
    //
    // Flush/return semantics and the junior/senior tranche math are UNCHANGED.
    if pool.pool_mode > 1 {
        msg!("AccrueFees: unknown pool mode");
        return Err(StakeError::InvalidPoolMode.into());
    }
```

- [ ] **Step 2: Count fees for mode 0 in `total_pool_value`**

At `src/state.rs:597-601`, replace:

```rust
        let fees = if self.pool_mode == 1 {
            self.total_fees_earned as i128
        } else {
            0
        };
```

with:

```rust
        // PERC-272: accrued trading fees count toward a trading pool's value.
        // 2026-07-19: mode-0 insurance pools also accrue fees (the insurance
        // leg of the trade-fee split), so their fees count too. Both modes now
        // include total_fees_earned; the field is 0 for any pool that has never
        // accrued, so this is a no-op for existing mode-0 pools.
        let fees = self.total_fees_earned as i128;
```

- [ ] **Step 3: Add a mode-0 accrual test**

In the stake test suite, add a test that: creates a mode-0 pool via `InitPool`, deposits, transfers a surplus into the vault, calls `AccrueFees`, and asserts `total_fees_earned` grew by **exactly** the surplus and `total_pool_value()` grew by the same amount.

**This test may set the vault balance directly** — it is a *unit* test of stake's accounting, and the wrapper-side producer is proven separately in Phase 2's product driver. Do not confuse this with the product-level test, which forbids state forgery.

- [ ] **Step 4: Run the full stake suite**

Run: `cd ~/v17/percolator-stake && cargo test 2>&1 | tail -30`
Expected: all pass. Pay particular attention to any `FlushToInsurance` test asserting mode-0 behaviour — flush semantics must be unchanged.

- [ ] **Step 5: Mutation-proof**

Change the new test's expected delta to `surplus + 1` → must FAIL → restore → PASS.

- [ ] **Step 5b: THIRD EDIT (plan amendment, user-approved 2026-07-19) — extend the pre-accrual guard to mode 0**

Review of the first two edits found a **Critical** front-running/dilution vector that those edits themselves arm. Confirmed in source:

- `calc_lp_for_deposit` / `calc_collateral_for_withdraw` price off the **stored** `total_pool_value()` (`state.rs:642-652`), never the live vault balance
- `pre_accrue_mode1` is gated `if pool.pool_mode == 1` (`processor.rs:2444`), so mode-0 pools never crystallize a pending vault surplus before pricing
- `AccrueFees` requires only *a* signer, with no authority gate (`processor.rs:2544-2548`) — fully permissionless

Attack: deposit while a surplus is pending (priced at the stale, lower share value) → self-call `AccrueFees` in the same transaction → the surplus distributes pro-rata over the **post-deposit** LP supply → the attacker captures fees that accrued before they staked, diluting every existing LP. Mirror case: an honest LP withdrawing in that window forfeits their share.

This was inert before this task, because mode-0 `total_fees_earned` was structurally always 0. It becomes live the moment Task 8 (`WithdrawInsuranceReserveToStake`) starts funding mode-0 vaults — in this same plan. **Therefore it must be fixed before Task 8, not deferred.**

Fix: extend the guard to run for both fee-accruing modes, and rename it so the name stops lying:

```rust
// Was: fn pre_accrue_mode1(...) { if pool.pool_mode == 1 { ... } }
// Now: crystallize any pending vault surplus BEFORE deposit/withdraw pricing, for
// every fee-accruing mode. Modes 0 (insurance backstop) and 1 (trading LP) both
// accrue fees as of the 2026-07-19 fee-collection design; pricing off a stale
// total_pool_value() while a surplus sits un-accrued in the vault lets a depositor
// mint LP cheaply and then permissionlessly self-call AccrueFees to capture
// pre-stake fees at existing holders' expense.
fn pre_accrue_fee_modes(pool: &mut state::StakePool, vault: &AccountInfo) -> ProgramResult {
    if pool.pool_mode <= 1 {
        // ... existing body unchanged ...
    }
    Ok(())
}
```

Update all call sites to the new name. Do **not** change the body's logic — only the mode predicate and the name.

Add a test proving the exploit is closed: with a pending vault surplus on a **mode-0** pool, a deposit must be priced *after* crystallization — i.e. the depositor's minted LP must equal what they'd get at the post-accrual share price, and an existing holder's claim must not fall. Mutation-proof it.

- [ ] **Step 6: Commit**

```bash
cd ~/v17/percolator-stake
git add src/processor.rs src/state.rs tests/
git commit -m "feat: mode-0 insurance pools accrue fees

Mode-0 stakers absorb losses via FlushToInsurance but had no upside leg.
They are now compensated with the insurance leg of the wrapper's
trade-fee split. Every real client calls InitPool (mode 0), so gating
accrual on mode 1 made it unreachable. Flush/return and the
junior/senior tranche math are unchanged."
```

---

### Task 13: Stake CPI proxies for the marketauth-gated setters (PLAN AMENDMENT, user-approved 2026-07-19)

**Files:**
- Modify: `~/v17/percolator-stake/src/instruction.rs` (new instruction variants)
- Modify: `~/v17/percolator-stake/src/processor.rs` (new handlers, modelled on `process_admin_resolve_market`)
- Modify: `~/v17/percolator-stake/src/cpi.rs` (new CPI helpers, modelled on the existing `AdminResolveMarket` path)
- Test: `~/v17/percolator-stake/tests/`

**Interfaces:**
- Consumes: wrapper tags 86 (`UpdateFeeSplit`, Task 6) and 88 (`UpdateMaintenanceFeePerSlot`, Task 9).
- Produces: two stake instructions that `invoke_signed` the stake-pool PDA to call wrapper tags 86 and 88 on a market whose `marketauth` the stake pool now holds.

**Why this task exists.** `percolator-stake`'s `InitPool` irreversibly rotates `cfg.marketauth` from the human admin to the stake-pool PDA (`processor.rs:542-575` → `cpi.rs:160-183`). A PDA cannot sign a top-level transaction, so **every** marketauth-gated wrapper instruction is thereafter reachable only through a CPI proxy issued by this program. The stake program currently has exactly one such proxy (`AdminResolveMarket` → wrapper tag 19). Its own documentation states the consequence plainly:

> *"Other wrapper admin operations gated on marketauth ... remain UNREACHABLE through this program and must be proxied here before they can ever be exercised on an InitPool market."* (`instruction.rs:1-22`, `lib.rs:6-21`)

Without this task, `UpdateFeeSplit` and `UpdateMaintenanceFeePerSlot` ship **born unreachable on any staked market** — the exact promise-vs-reachability defect this entire plan exists to fix. The defaults are safe and the wizard can set a split pre-stake, so this is not a launch blocker; what it recovers is post-launch retuning.

- [ ] **Step 1: Read the existing proxy end-to-end**

Read `process_admin_resolve_market` and its `cpi.rs` helper in full. It is the *only* working example of this pattern in the repo. Mirror its account layout, authority checks, PDA seed derivation, and `invoke_signed` idiom exactly. **Do not invent a new proxy idiom** — this path signs as the authority that governs a live market, and a mistake here is a privilege-escalation bug.

- [ ] **Step 2: Decide and document who may invoke each proxy**

`AdminResolveMarket`'s existing authority model governs who can drive the pool. Apply the same model unless there is a documented reason not to, and state the reasoning in the commit message. A proxy that anyone may call would let a third party rewrite a market's fee policy.

- [ ] **Step 3: Add the two proxy instructions**

One forwarding to wrapper tag 86 (`UpdateFeeSplit`, args `creator_share_bps`/`lp_share_bps`/`insurance_share_bps`, all `u16`), one to tag 88 (`UpdateMaintenanceFeePerSlot`, arg `maintenance_fee_per_slot: u64`). Append new stake instruction tags; do not renumber existing ones.

Validation stays in the wrapper — do not duplicate `validate_fee_split`'s floor logic here, or the two copies will drift.

- [ ] **Step 4: Prove reachability end-to-end (this is the whole point)**

A test that calls the proxy in isolation proves nothing. The test must:
1. create a market, 2. run `StakeInitPool` so `marketauth` really is the pool PDA, 3. confirm a **direct** wrapper `UpdateFeeSplit` from the original admin now **fails** with the authority error, 4. confirm the **proxy** succeeds, 5. read the config back and assert the shares actually changed on chain.

Step 3 is what distinguishes this from a test that would pass even if the proxy did nothing.

- [ ] **Step 5: Run the full stake suite, mutation-proof the new assertions, commit**

Compare failure name sets against the base commit, not counts.

---

### Task 12: Build, hash, and hand off for user-gated deploy

**Files:** none modified.

**Interfaces:**
- Consumes: all prior tasks.
- Produces: verified `.so` artifacts and their sha256 hashes, for the user to authorize deploying.

- [ ] **Step 1: Confirm the engine is untouched**

Run: `cd ~/v17/percolator && git status --short && git diff --stat`
Expected: **empty output**. Any diff here violates the plan's core constraint — stop and report.

- [ ] **Step 2: Build the wrapper with correct flags**

Run: `cd ~/v17/percolator-prog && cargo build-sbf 2>&1 | tail -15`
Expected: success. **Do not pass `--no-default-features`** — that produces a different, legacy binary.

- [ ] **Step 3: Build stake with correct flags**

Run: `cd ~/v17/percolator-stake && cargo build-sbf --features devnet 2>&1 | tail -15`
Expected: success.

- [ ] **Step 4: Hash both artifacts**

Run:
```bash
shasum -a 256 ~/v17/percolator-prog/target/deploy/percolator_prog.so \
              ~/v17/percolator-stake/target/deploy/percolator_stake.so
```
Record both hashes.

- [ ] **Step 5: Run the full test suites one final time**

Run: `cd ~/v17/percolator-prog && cargo test 2>&1 | tail -15 && cd ~/v17/percolator-stake && cargo test 2>&1 | tail -15`
Expected: all green.

- [ ] **Step 6: STOP and report to the user**

Report: both hashes, the test summaries, and confirmation that the engine diff is empty.

**Do not deploy. Do not upgrade. Do not send any transaction.** Deploy is user-gated. The product-layer verification (`m17_fee_split_product.rs`, spec §10.1) runs against **deployed bytecode** and therefore belongs to Phase 2, after the user authorizes the upgrade.

---

## Out of scope for this plan (later phases)

- **Phase 2:** `m17_fee_split_product.rs` product-layer driver + value-delivery tests (spec §10.1-10.2) — requires deployed bytecode.
- **Phase 3:** SDK encoders for tags 86/87/88, config decoder for the new fields.
- **Phase 4:** Keeper shared threshold-gated crank loop; app creator-claim UI; wizard writes share bps.

## Self-Review Notes

**Spec coverage:** §3 split model → Tasks 1, 3, 6. §4 schema → Task 3. §5 collection → Tasks 4, 5, 6, 10. §6 claim paths → Tasks 7, 8. §6.1 stake modes → Task 11. §7 maintenance setter → Task 9. §9 invariants → Tasks 1, 2, plus write-back handling in Tasks 4, 5. §10 testing → Tasks 1, 2, 6, 9, 11 here; §10.1-10.2 deferred to Phase 2 (needs deployed bytecode). §7 sweep + client work → Phase 4.

**Defect found and fixed during self-review — floor units.** My first draft carried the decided floors (45/40/15) across as raw bps, which is a unit error: those are percentages of the **post-protocol remainder**, while the stored shares are bps **of T** summing to 8000. Left uncorrected, the floors would have been ~25% too strict, rejecting the defaults themselves. Correct conversion is `pct × 8000` → **3600 / 3200 / 1200**, verified to sum to exactly 8000 and to accept the defaults (1600/4800/1600) with margin. The Constants table, Task 3 Step 1, and Task 6 all carry the corrected values; there is nothing left for the implementer to resolve.

**Type consistency check:** `split_trade_fee` takes `(u128, u16, u16, u16, u16)` and returns `FeeSplitParts` in Tasks 1, 2, 4, 5 — consistent. `validate_fee_split` takes `(u16, u16, u16)` returning `Result<(), ProgramError>` in Tasks 6 and 10 — consistent. Counter field names (`lp_fee_accrued_atoms`, `lp_fee_withdrawn_atoms`, `insurance_reserve_accrued_atoms`, `insurance_reserve_withdrawn_atoms`) are identical in Tasks 3, 4, 5, 7, 8.

**Placeholder check:** Tasks 6 Step 6, 8 Step 2, and 9 Step 1 deliberately say "mirror the neighbouring handler's account-loading / marketauth / PDA-signing idiom" rather than reproducing those 40-line blocks. This is intentional — inventing a fresh transfer or auth idiom is exactly how a security regression gets introduced, and the named neighbour (`handle_withdraw_protocol_fee`, `handle_update_trade_fee_policy`) is a precise, existing reference. Every other step contains literal code.
