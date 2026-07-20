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

// ════════════════════════════════════════════════════════════════════════════
// Tag 87 — WithdrawInsuranceReserveToStake. TOKEN-VISIBLE tests.
//
// Counter-only assertions are exactly how a no-op ships (Task 7). Every test
// below asserts on REAL SPL balances: the stake vault's `amount` must rise by
// the transferred atoms and the market vault's must fall by the same, with
// `header.insurance` tracking it. Conservation is asserted explicitly.
//
// The stake pool here is hand-crafted at the v3 layout (392 B, version 3 --
// `percolator-stake/src/state.rs:688`/`:464`). It is NOT decoration: the real
// `percolator_stake.so` is loaded and its BindInsuranceAuthority (tag 19) is
// what establishes `insurance_authority`, so a wrong offset or version in the
// craft fails the bind and the test cannot reach tag 87 at all. That makes the
// bind a live cross-check of the wrapper's own layout constants.
// (`tests/v16_five_program_crosscut.rs:1662` still crafts the stale v2 384-B
// shape, which is why its stake tests fail at baseline.)
// ════════════════════════════════════════════════════════════════════════════

mod common;

use common::{
    assemble_five_program_svm, assert_custom, make_mint_data, make_token_data, send_ixs,
    spl_token_classic_id, PERCOLATOR_MAINNET, STAKE_ID,
};
use percolator_prog::ix::Instruction as ProgInstruction;
use percolator_prog::state;
use solana_program::instruction::{AccountMeta, Instruction};
use solana_sdk::{account::Account, pubkey::Pubkey, signature::Keypair, signer::Signer};
use spl_token::state::Account as TokenAccount;
use spl_token::solana_program::program_pack::Pack;

const FS_MAX_ASSETS: u16 = 2;

fn canonical_vault_ata(vault_authority: &Pubkey, mint: &Pubkey) -> Pubkey {
    let ata_program: Pubkey = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
        .parse()
        .unwrap();
    Pubkey::find_program_address(
        &[vault_authority.as_ref(), spl_token::ID.as_ref(), mint.as_ref()],
        &ata_program,
    )
    .0
}

/// v3 StakePool (392 B). Offsets mirror `percolator-stake/tests/struct_layout.rs`.
#[allow(clippy::too_many_arguments)]
fn craft_stake_pool_v3(
    market: &Pubkey,
    admin: &Pubkey,
    collateral_mint: &Pubkey,
    lp_mint: &Pubkey,
    stake_vault: &Pubkey,
    total_deposited: u64,
    total_lp_supply: u64,
    percolator_program: &Pubkey,
    vault_authority_bump: u8,
) -> Vec<u8> {
    let mut d = vec![0u8; 392];
    d[0] = 1; // is_initialized
    d[1] = 255; // pool bump (informational)
    d[2] = vault_authority_bump;
    d[8..40].copy_from_slice(market.as_ref()); // slab
    d[40..72].copy_from_slice(admin.as_ref());
    d[72..104].copy_from_slice(collateral_mint.as_ref());
    d[104..136].copy_from_slice(lp_mint.as_ref());
    d[136..168].copy_from_slice(stake_vault.as_ref()); // vault
    d[168..176].copy_from_slice(&total_deposited.to_le_bytes());
    d[176..184].copy_from_slice(&total_lp_supply.to_le_bytes());
    d[224..256].copy_from_slice(percolator_program.as_ref()); // CPI target
    // d[280] pool_mode = 0 (insurance LP) — already zero.
    d[320..328].copy_from_slice(b"SPOOL_V1"); // discriminator
    d[328] = 3; // CURRENT_VERSION
    d
}

struct FeeEnv {
    svm: litesvm::LiteSVM,
    payer: Keypair,
    admin: Keypair,
    market: Pubkey,
    mint: Pubkey,
    vault: Pubkey,
    pool_pda: Pubkey,
    vault_auth: Pubkey,
    stake_vault: Pubkey,
}

impl FeeEnv {
    /// Market at MAINNET + a bound v3 stake pool. `insurance` is funded with
    /// `insurance_atoms` of REAL tokens before the bind (TopUpInsurance is
    /// gated on asset 0's insurance_authority, which is `admin` until bound).
    fn new(insurance_atoms: u64) -> Self {
        let matcher_program = Pubkey::new_unique();
        let mut svm = assemble_five_program_svm(matcher_program);
        let program_id = PERCOLATOR_MAINNET;

        let payer = Keypair::new();
        let admin = Keypair::new();
        let market = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let (vault_authority, _) =
            Pubkey::find_program_address(&[b"vault", market.as_ref()], &program_id);
        let vault = canonical_vault_ata(&vault_authority, &mint);

        svm.airdrop(&payer.pubkey(), 1_000_000_000_000).unwrap();
        svm.airdrop(&admin.pubkey(), 1_000_000_000_000).unwrap();
        let plant = |svm: &mut litesvm::LiteSVM, key: Pubkey, data: Vec<u8>, owner: Pubkey| {
            svm.set_account(
                key,
                Account {
                    lamports: 1_000_000_000,
                    data,
                    owner,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
        };
        plant(&mut svm, mint, make_mint_data(), spl_token_classic_id());
        plant(
            &mut svm,
            vault,
            make_token_data(mint, vault_authority, 0),
            spl_token_classic_id(),
        );
        // Admin's funding account for TopUpInsurance.
        let admin_token = Pubkey::new_unique();
        plant(
            &mut svm,
            admin_token,
            make_token_data(mint, admin.pubkey(), insurance_atoms),
            spl_token_classic_id(),
        );
        let market_len = state::market_account_len_for_capacity(FS_MAX_ASSETS as usize).unwrap();
        plant(&mut svm, market, vec![0u8; market_len], program_id);

        let mut env = FeeEnv {
            svm,
            payer,
            admin,
            market,
            mint,
            vault,
            pool_pda: Pubkey::default(),
            vault_auth: Pubkey::default(),
            stake_vault: Pubkey::default(),
        };
        env.init_market();
        if insurance_atoms > 0 {
            env.top_up_insurance(admin_token, insurance_atoms);
        }
        env.setup_and_bind_stake_pool();
        env
    }

    fn init_market(&mut self) {
        let admin = self.admin.insecure_clone();
        let ix = Instruction {
            program_id: PERCOLATOR_MAINNET,
            accounts: vec![
                AccountMeta::new(admin.pubkey(), true),
                AccountMeta::new(self.market, false),
                AccountMeta::new_readonly(self.mint, false),
            ],
            data: ProgInstruction::InitMarket {
                max_portfolio_assets: FS_MAX_ASSETS,
                h_min: 0,
                h_max: 10,
                initial_price: 100,
                min_nonzero_mm_req: 1,
                min_nonzero_im_req: 2,
                maintenance_margin_bps: 10_000,
                initial_margin_bps: 10_000,
                max_trading_fee_bps: 10_000,
                trade_fee_base_bps: 0,
                liquidation_fee_bps: 0,
                liquidation_fee_cap: 0,
                min_liquidation_abs: 0,
                max_price_move_bps_per_slot: 10_000,
                max_accrual_dt_slots: 1,
                max_abs_funding_e9_per_slot: 0,
                min_funding_lifetime_slots: 1,
                max_account_b_settlement_chunks: 1,
                max_bankrupt_close_chunks: 1,
                max_bankrupt_close_lifetime_slots: 100,
                public_b_chunk_atoms: percolator::MAX_VAULT_TVL,
                maintenance_fee_per_slot: 0,
            }
            .encode(),
        };
        let payer = self.payer.insecure_clone();
        send_ixs(&mut self.svm, &payer, vec![ix], &[&admin]).expect("InitMarket");
    }

    fn top_up_insurance(&mut self, source: Pubkey, amount: u64) {
        let admin = self.admin.insecure_clone();
        let ix = Instruction {
            program_id: PERCOLATOR_MAINNET,
            accounts: vec![
                AccountMeta::new(admin.pubkey(), true),
                AccountMeta::new(self.market, false),
                AccountMeta::new(source, false),
                AccountMeta::new(self.vault, false),
                AccountMeta::new_readonly(spl_token_classic_id(), false),
            ],
            data: ProgInstruction::TopUpInsurance {
                amount: amount as u128,
            }
            .encode(),
        };
        let payer = self.payer.insecure_clone();
        send_ixs(&mut self.svm, &payer, vec![ix], &[&admin]).expect("TopUpInsurance");
    }

    /// Plant a v3 pool + its vault, then drive the REAL stake program's
    /// BindInsuranceAuthority so `insurance_authority == vault_auth`.
    fn setup_and_bind_stake_pool(&mut self) {
        let (pool_pda, _) =
            Pubkey::find_program_address(&[b"stake_pool", self.market.as_ref()], &STAKE_ID);
        let (vault_auth, vault_auth_bump) =
            Pubkey::find_program_address(&[b"vault_auth", pool_pda.as_ref()], &STAKE_ID);
        let stake_vault = Pubkey::new_unique();
        self.svm
            .set_account(
                stake_vault,
                Account {
                    lamports: 1_000_000_000,
                    data: make_token_data(self.mint, vault_auth, 0),
                    owner: spl_token_classic_id(),
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
        let lp_mint = Pubkey::new_unique();
        let pool_bytes = craft_stake_pool_v3(
            &self.market,
            &self.admin.pubkey(),
            &self.mint,
            &lp_mint,
            &stake_vault,
            0,
            1_000, // non-zero LP supply: a real staker constituency
            &PERCOLATOR_MAINNET,
            vault_auth_bump,
        );
        self.svm
            .set_account(
                pool_pda,
                Account {
                    lamports: 1_000_000_000,
                    data: pool_bytes,
                    owner: STAKE_ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();

        let admin = self.admin.insecure_clone();
        let ix = Instruction {
            program_id: STAKE_ID,
            accounts: vec![
                AccountMeta::new(admin.pubkey(), true),
                AccountMeta::new_readonly(pool_pda, false),
                AccountMeta::new_readonly(vault_auth, false),
                AccountMeta::new(self.market, false),
                AccountMeta::new_readonly(PERCOLATOR_MAINNET, false),
            ],
            data: vec![19u8],
        };
        let payer = self.payer.insecure_clone();
        send_ixs(&mut self.svm, &payer, vec![ix], &[&admin])
            .expect("BindInsuranceAuthority (v3 pool) — layout constants must match the stake program");

        self.pool_pda = pool_pda;
        self.vault_auth = vault_auth;
        self.stake_vault = stake_vault;
    }

    /// Make the market's insurance UNBUDGETED, i.e. the surplus a trade fee
    /// produces.
    ///
    /// `TopUpInsurance` is how real tokens get into the market vault, but it
    /// books the atoms into a DOMAIN BUDGET — and budgeted insurance is
    /// deliberately not surplus, so `engine_available` is 0 and every leg
    /// correctly declines to touch it. Trade fees behave the opposite way: they
    /// raise `header.insurance` without raising any domain budget, which is
    /// precisely what makes the three fee legs claimable. Zeroing the budget
    /// here reproduces that end state while keeping the real SPL tokens that
    /// TopUpInsurance moved. Mirrors `v16_wrapper.rs::seed_protocol_fee_fixture`,
    /// which seeds `group.insurance`/`group.vault` directly for the same reason.
    fn unbudget_insurance(&mut self) {
        let mut acct = self.svm.get_account(&self.market).unwrap();
        {
            let (_, group) = state::market_view_mut(&mut acct.data).unwrap();
            group.header.insurance_domain_budget_remaining_total =
                percolator::V16PodU128::new(0);
        }
        self.svm.set_account(self.market, acct).unwrap();
    }

    /// Directly seed the wrapper-side accrual counter. Accrual itself is
    /// Task 3's surface and is exercised by the split tests above; what is
    /// under test here is the WITHDRAW path, and seeding lets the clamp be
    /// driven to an exact boundary.
    fn set_reserve_accrued(&mut self, atoms: u128) {
        let mut acct = self.svm.get_account(&self.market).unwrap();
        let (mut cfg, _, _, _) =
            state::read_market_config_mode_and_capacity(&acct.data).unwrap();
        cfg.insurance_reserve_accrued_atoms = atoms;
        state::write_wrapper_config(&mut acct.data, &cfg).unwrap();
        self.svm.set_account(self.market, acct).unwrap();
    }

    fn reserve_counters(&self) -> (u128, u128) {
        let acct = self.svm.get_account(&self.market).unwrap();
        let (cfg, _, _, _) =
            state::read_market_config_mode_and_capacity(&acct.data).unwrap();
        (
            cfg.insurance_reserve_accrued_atoms,
            cfg.insurance_reserve_withdrawn_atoms,
        )
    }

    /// `header.insurance` and `header.vault`, read from the live account.
    fn header_insurance_and_vault(&self) -> (u128, u128) {
        let mut data = self.svm.get_account(&self.market).unwrap().data;
        let (_, group) = state::market_view_mut(&mut data).unwrap();
        (group.header.insurance.get(), group.header.vault.get())
    }

    fn token_amount(&self, key: Pubkey) -> u64 {
        TokenAccount::unpack(&self.svm.get_account(&key).unwrap().data)
            .unwrap()
            .amount
    }

    fn withdraw_to_stake(&mut self) -> Result<(), solana_sdk::transaction::TransactionError> {
        let cranker = Keypair::new();
        self.svm.airdrop(&cranker.pubkey(), 1_000_000_000).unwrap();
        let (vault_authority, _) =
            Pubkey::find_program_address(&[b"vault", self.market.as_ref()], &PERCOLATOR_MAINNET);
        let ix = Instruction {
            program_id: PERCOLATOR_MAINNET,
            accounts: vec![
                AccountMeta::new(cranker.pubkey(), true),
                AccountMeta::new(self.market, false),
                AccountMeta::new_readonly(self.pool_pda, false),
                AccountMeta::new(self.stake_vault, false),
                AccountMeta::new(self.vault, false),
                AccountMeta::new_readonly(vault_authority, false),
                AccountMeta::new_readonly(spl_token_classic_id(), false),
            ],
            data: ProgInstruction::WithdrawInsuranceReserveToStake.encode(),
        };
        let payer = self.payer.insecure_clone();
        send_ixs(&mut self.svm, &payer, vec![ix], &[&cranker])
    }
}

/// THE test: atoms must actually LAND in the stake vault as SPL tokens.
#[test]
fn tag87_moves_real_tokens_into_the_stake_vault_and_conserves_value() {
    const FUNDED: u64 = 1_000_000;
    const ACCRUED: u128 = 250_000;
    let mut env = FeeEnv::new(FUNDED);

    env.unbudget_insurance();
    env.set_reserve_accrued(ACCRUED);

    let stake_before = env.token_amount(env.stake_vault);
    let market_vault_before = env.token_amount(env.vault);
    let (ins_before, vault_before) = env.header_insurance_and_vault();
    let (accrued_before, withdrawn_before) = env.reserve_counters();
    assert_eq!(stake_before, 0, "stake vault starts empty");
    assert_eq!(withdrawn_before, 0, "nothing withdrawn yet");
    assert_eq!(accrued_before, ACCRUED);

    env.withdraw_to_stake().expect("tag 87 must succeed");

    let stake_after = env.token_amount(env.stake_vault);
    let market_vault_after = env.token_amount(env.vault);
    let (ins_after, vault_after) = env.header_insurance_and_vault();
    let (accrued_after, withdrawn_after) = env.reserve_counters();

    // ── TOKEN-VISIBLE: the stake vault's real SPL balance rose. ──
    assert_eq!(
        stake_after - stake_before,
        ACCRUED as u64,
        "stake vault SPL balance must rise by exactly the accrued insurance leg \
         — this is the assertion that a counter-only no-op cannot pass"
    );
    // ── The tokens came OUT of the market vault, not from nowhere. ──
    assert_eq!(
        market_vault_before - market_vault_after,
        ACCRUED as u64,
        "market vault SPL balance must fall by the same amount"
    );
    // ── CONSERVATION: no atoms created or destroyed. ──
    assert_eq!(
        market_vault_before + stake_before,
        market_vault_after + stake_after,
        "total SPL tokens across market vault + stake vault must be conserved"
    );
    // ── header.insurance tracks the real movement. ──
    assert_eq!(
        ins_before - ins_after,
        ACCRUED,
        "header.insurance must fall by exactly the transferred amount"
    );
    assert_eq!(
        vault_before - vault_after,
        ACCRUED,
        "header.vault must fall by exactly the transferred amount"
    );
    // ── The counter advanced by what was TRANSFERRED. ──
    assert_eq!(accrued_after, ACCRUED, "accrued is monotonic, untouched here");
    assert_eq!(
        withdrawn_after - withdrawn_before,
        stake_after as u128 - stake_before as u128,
        "withdrawn must advance by exactly the amount the stake vault received"
    );
    assert!(
        withdrawn_after <= accrued_after,
        "invariant: withdrawn <= accrued"
    );

    // Fully drained ⇒ a second crank has nothing left and says so.
    assert_custom(env.withdraw_to_stake(), 53, "second crank after full drain");
}

/// Partial fill: the shared surplus pool is smaller than the claim, so the
/// clamp bites. The remainder MUST stay claimable — marking it paid without
/// paying it is the exact defect this test exists to catch.
#[test]
fn tag87_partial_fill_marks_only_what_was_transferred() {
    const FUNDED: u64 = 100_000;
    // Claim far exceeds what `header.insurance` can supply.
    const ACCRUED: u128 = 400_000;
    let mut env = FeeEnv::new(FUNDED);
    env.unbudget_insurance();
    env.set_reserve_accrued(ACCRUED);

    let stake_before = env.token_amount(env.stake_vault);
    let (ins_before, _) = env.header_insurance_and_vault();
    assert!(
        ins_before < ACCRUED,
        "fixture must actually exercise the clamp"
    );

    env.withdraw_to_stake().expect("partial fill must succeed");

    let stake_after = env.token_amount(env.stake_vault);
    let (_, withdrawn) = env.reserve_counters();
    let moved = (stake_after - stake_before) as u128;

    assert_eq!(
        moved, ins_before,
        "the clamp must fill to the available surplus, not the requested claim"
    );
    assert!(moved < ACCRUED, "this must be a PARTIAL fill");
    assert_eq!(
        withdrawn, moved,
        "withdrawn must advance by the TRANSFERRED amount, never the pre-clamp claim"
    );
    assert_eq!(
        ACCRUED - withdrawn,
        ACCRUED - moved,
        "the unfilled remainder must stay claimable"
    );
    assert!(withdrawn < ACCRUED, "remainder must NOT be marked paid");
}

/// Zero case with the exact code.
#[test]
fn tag87_with_nothing_accrued_returns_custom_53() {
    let mut env = FeeEnv::new(1_000_000);
    let (accrued, withdrawn) = env.reserve_counters();
    assert_eq!(accrued, 0, "no volume ⇒ nothing accrued");
    assert_eq!(withdrawn, 0);
    assert_custom(
        env.withdraw_to_stake(),
        53,
        "NoInsuranceReserveToClaim on a zero-volume market",
    );
}

/// SECURITY: the destination is not the caller's to choose. A well-formed
/// token account of the right mint, at an address that is NOT `pool.vault`,
/// must be rejected — otherwise the whole insurance leg is drainable by
/// anyone, since tag 87 needs no authority signature.
#[test]
fn tag87_rejects_a_caller_supplied_destination() {
    let mut env = FeeEnv::new(1_000_000);
    env.unbudget_insurance();
    env.set_reserve_accrued(250_000);

    // An account the attacker controls, correct mint, correct decimals —
    // everything except being the pool's recorded vault.
    let attacker = Keypair::new();
    let attacker_token = Pubkey::new_unique();
    env.svm
        .set_account(
            attacker_token,
            Account {
                lamports: 1_000_000_000,
                data: make_token_data(env.mint, attacker.pubkey(), 0),
                owner: spl_token_classic_id(),
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    let real_stake_vault = env.stake_vault;
    env.stake_vault = attacker_token;

    let res = env.withdraw_to_stake();
    assert!(
        res.is_err(),
        "a destination that is not pool.vault MUST be rejected"
    );
    assert_eq!(
        env.token_amount(attacker_token),
        0,
        "not a single atom may reach an attacker-chosen destination"
    );
    assert_eq!(
        env.token_amount(real_stake_vault),
        0,
        "and the real stake vault must be untouched by the failed attempt"
    );
    let (_, withdrawn) = env.reserve_counters();
    assert_eq!(withdrawn, 0, "a rejected redirect must not mark anything paid");
}

/// SECURITY: the same, one step subtler — a token account whose SPL owner IS
/// the stake pool's `vault_auth` PDA, but which is not the address the pool
/// recorded. Catches a validation that checks only the owner and forgets the
/// `pool.vault` pin.
#[test]
fn tag87_rejects_a_vault_auth_owned_impostor_at_the_wrong_address() {
    let mut env = FeeEnv::new(1_000_000);
    env.unbudget_insurance();
    env.set_reserve_accrued(250_000);

    let impostor = Pubkey::new_unique();
    env.svm
        .set_account(
            impostor,
            Account {
                lamports: 1_000_000_000,
                data: make_token_data(env.mint, env.vault_auth, 0),
                owner: spl_token_classic_id(),
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    env.stake_vault = impostor;

    assert!(
        env.withdraw_to_stake().is_err(),
        "correct owner but wrong address must still be rejected — the pin is \
         pool.vault, not merely the SPL owner"
    );
    assert_eq!(env.token_amount(impostor), 0);
}

/// SECURITY: a market with no bound stake pool has no staker constituency
/// absorbing its losses, so there is nobody to pay. Fail closed rather than
/// transfer to a caller-supplied "pool".
#[test]
fn tag87_rejects_a_market_with_no_bound_stake_pool() {
    // Build an env, then point it at a market that was never bound.
    let mut env = FeeEnv::new(1_000_000);
    env.unbudget_insurance();
    env.set_reserve_accrued(250_000);

    // Zero the pool account's `slab` binding so the derived vault_auth can no
    // longer match the market's recorded insurance_authority.
    let mut pool = env.svm.get_account(&env.pool_pda).unwrap();
    pool.data[8..40].copy_from_slice(Pubkey::new_unique().as_ref());
    env.svm.set_account(env.pool_pda, pool).unwrap();

    assert!(
        env.withdraw_to_stake().is_err(),
        "a pool whose slab no longer names this market must be rejected"
    );
    let (_, withdrawn) = env.reserve_counters();
    assert_eq!(withdrawn, 0);
}

/// MODE GATE: a resolved market must refuse to push surplus out to stakers,
/// even though the sibling protocol leg (tag 84) permits Resolved. The
/// justification is in the handler doc comment: tag 84 is signer-gated and has
/// no other exit (W12), whereas tag 87 is permissionless and the staker claim
/// survives resolution through the bound insurance authority's tag-41 path.
/// Nothing is stranded by refusing here — the claim stays fully unwithdrawn.
#[test]
fn tag87_is_rejected_on_a_resolved_market_and_strands_nothing() {
    let mut env = FeeEnv::new(1_000_000);
    env.unbudget_insurance();
    env.set_reserve_accrued(250_000);

    let admin = env.admin.insecure_clone();
    let payer = env.payer.insecure_clone();
    env.svm.warp_to_slot(50_000);
    let ix = Instruction {
        program_id: PERCOLATOR_MAINNET,
        accounts: vec![
            AccountMeta::new(admin.pubkey(), true),
            AccountMeta::new(env.market, false),
        ],
        data: ProgInstruction::ResolveMarket.encode(),
    };
    send_ixs(&mut env.svm, &payer, vec![ix], &[&admin]).expect("ResolveMarket");

    let stake_before = env.token_amount(env.stake_vault);
    assert_custom(
        env.withdraw_to_stake(),
        21, // EngineLockActive
        "tag 87 on a resolved market",
    );
    assert_eq!(
        env.token_amount(env.stake_vault),
        stake_before,
        "no tokens may move on a rejected mode gate"
    );
    let (accrued, withdrawn) = env.reserve_counters();
    assert_eq!(withdrawn, 0, "a rejected call must not mark anything paid");
    assert_eq!(
        accrued - withdrawn,
        250_000,
        "the whole claim must remain outstanding, not be stranded as paid"
    );
}
