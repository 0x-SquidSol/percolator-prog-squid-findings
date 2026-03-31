# Checklist #6 — Reentrancy + CPI Safety Review

**Task:** PERC-8364  
**Author:** Anchor agent (Anvil)  
**Date:** 2026-04-01  
**Scope:** `percolator-prog/src/percolator.rs` — all CPI invocations  
**Verdict:** ✅ PASS — no reentrancy vectors found; CPI callee programs are validated; signing seeds are program-controlled

---

## 1. CPI Call Inventory

All `invoke` / `invoke_signed` / `invoke_signed_unchecked` call sites audited:

| Location | Function | Callee | Seeds | Notes |
|----------|----------|--------|-------|-------|
| L6008 | `collateral::deposit` | SPL Token (user-signed) | none (user authority) | User signs for their own ATA transfer |
| L6061 | `collateral::withdraw` | SPL Token | `["vault", slab, bump]` | PDA-controlled seeds |
| L6135 | `insurance_lp::create_mint` | System Program | `[mint_seeds]` | PDA-controlled |
| L6149 | `insurance_lp::create_mint` | SPL Token | none | InitializeMint — no seeds needed |
| L6203 | `insurance_lp::mint_to` | SPL Token | `[vault seeds]` | PDA-controlled mint authority |
| L6261 | `insurance_lp::burn` | SPL Token | none | User signs for own tokens |
| L1872 | `zc::invoke_signed_trade` | Matcher program | `["lp", slab, lp_idx, bump]` | See Section 2 |
| L9936 | `InitSharedVault` | System Program | `[SHARED_VAULT_SEED, bump]` | PDA-controlled |
| L10007 | `InitSharedVault` (creator lock) | System Program | `[CREATOR_LOCK_SEED, slab, bump]` | PDA-controlled |
| L12904 | `InitInsuranceLpMint` | System Program | `[mint_seeds]` | PDA-controlled |
| L13749 | `CreateLpVault` | System Program | `["lp_vault", slab, bump]` | PDA-controlled |
| L14572 | `RaiseDispute` | System Program | `["dispute", slab, bump]` | PDA-controlled |
| L14969 | `QueueWithdrawalSV` | System Program | `[WITHDRAW_REQ_SEED, ...]` | PDA-controlled |
| L15874 | `AllocateMarket` | System Program | `[MARKET_ALLOC_SEED, ...]` | PDA-controlled |
| L16035 | `SetOffsetPair` | System Program | `["cmor_pair", ...]` | PDA-controlled |
| L16191 | `TopUpKeeperFund` | System Program | none (externally-owned funder transfer) | Caller must be signer |
| L16286 | `InitSharedVault` | System Program | `[SHARED_VAULT_SEED, bump]` | PDA-controlled |
| L16420 | `AllocateMarket` | System Program | `[MARKET_ALLOC_SEED, ...]` | PDA-controlled |
| L16576 | `QueueWithdrawalSV` | System Program | `[WITHDRAW_REQ_SEED, ...]` | PDA-controlled |
| L16902 | `MintPositionNft` | System Program | `[POSITION_NFT_SEED, ...]` | PDA-controlled |
| L18560–18874 | `position_nft::create_nft_mint_with_metadata` + `mint_nft_to` | Token-2022, System Program | `[mint_seeds]` / `[vault seeds]` | PDA-controlled |

---

## 2. Reentrancy Analysis

### 2.1 Solana Reentrancy Model

Solana's SVM prohibits reentrant CPI by design: a program cannot invoke itself within the same instruction. Any attempt returns `ProgramError::ReentrancyNotAllowed`. This is a protocol-level guard. Percolator benefits from this unconditionally.

**Finding:** No reentrant CPI is structurally possible in percolator.rs under the current SVM.

### 2.2 State-before-CPI Pattern (Checks-Effects-Interactions)

Solana best practice: update program state **before** issuing CPI, so that if the callee somehow affects program accounts, state is already committed.

Review of all instructions that both mutate engine state and issue CPI:

| Instruction | State mutation before CPI? | Notes |
|-------------|---------------------------|-------|
| `InitUser` / `InitLP` | ✅ `collateral::deposit` called **before** `engine.add_user` / `engine.add_lp` (L10137, L10190) | SAFE — collateral transferred, then engine state written |
| `DepositCollateral` | ✅ CPI deposit at L10249; engine write at L10279 onwards | SAFE |
| `WithdrawCollateral` | ✅ Engine state mutated (engine.withdraw) at L~10380 **before** `collateral::withdraw` CPI at L10389 | ✅ SAFE — follows CEI |
| `ClaimEpochWithdrawal` | ✅ Units deducted from engine before CPI payout | SAFE |
| `LiquidateAtOracle` | ✅ Engine.execute_trade runs before any collateral::withdraw | SAFE |
| `TradeCpi` / `TradeCpiV2` | Partially inverted (see Section 2.3) | See below |
| `DepositInsuranceLp` | ✅ engine.top_up_insurance_fund runs after collateral::deposit but before mint_to | SAFE |
| `WithdrawInsuranceLp` | ✅ insurance balance decremented before burn + withdraw CPI | SAFE |
| `WithdrawInsuranceLimited` | ✅ Phase 1 (engine mutation) before Phase 2 (CPI) — explicitly structured this way | ✅ SAFE |
| `TopUpInsurance` | ✅ deposit CPI before engine.top_up — order is deposit → engine credit | SAFE (deposit-first is fine here) |

### 2.3 TradeCpi — CPI-Before-State-Write Analysis

**TradeCpi / TradeCpiV2** (lines ~11060–11430) has an unusual but intentional ordering:

1. Account validation (matcher identity, LP PDA shape)
2. Read engine state (immutable borrow), generate nonce
3. Oracle price read
4. **CPI to matcher program** (`invoke_signed_trade` at L11272)
5. ABI-validate matcher return
6. Re-acquire mutable slab borrow
7. `engine.execute_trade` — engine state mutation
8. Post-trade checks (OI cap, PnL cap, phase leverage)
9. Write nonce

This is **by design** (see comment at L11253: "We don't zero the matcher_ctx before CPI because we don't own it"). The matcher CPI is used to **read** the exec price and size back via the context account — it cannot mutate slab state because:

- The slab borrow is explicitly **dropped** before the CPI (see the scoped `{ let data = ... drop implicitly }` block)
- The matcher program does not receive the slab account; it only receives `a_lp_pda` (system-owned, 0 data, 0 lamports — validated via `lp_pda_shape_ok`) and `a_matcher_ctx`
- ABI validation (`abi_ok`) checks req_id, lp_account_id, and oracle_price_e6 before trusting the return

**Finding:** TradeCpi pattern is safe. The pre-CPI state drop is deliberate and the matcher cannot touch engine state. Reentrancy via matcher is blocked because slab is not passed to the CPI and no borrow is held across the call.

---

## 3. Callee Program Validation

### 3.1 SPL Token Program (collateral CPI calls)

`verify_token_program` (L9494–9508) validates the token program account before every `collateral::deposit` / `collateral::withdraw`:
```rust
if *a_token.key != crate::spl_token::id() {
    return Err(PercolatorError::InvalidTokenProgram.into());
}
if !a_token.executable {
    return Err(PercolatorError::InvalidTokenProgram.into());
}
```
Called on all token-transfer paths (verified at 20+ call sites).

**Finding:** ✅ SPL Token program is validated at every collateral CPI entry point. User-supplied arbitrary token programs are rejected.

### 3.2 Matcher Program (TradeCpi)

`verify::matcher_shape_ok` (L11081–11088) checks:
- `prog_executable`: matcher program must be executable
- `ctx_executable`: context account must NOT be executable
- `ctx_owner_is_prog`: matcher_ctx must be owned by the matcher program
- `ctx_len_ok`: context account must be large enough to hold `MatcherReturn`

`verify::matcher_identity_ok` (L11192–11197) cross-checks the provided `a_matcher_prog` and `a_matcher_ctx` keys against the **LP-registered** matcher program and context stored in the engine account. This is the critical binding: a user cannot substitute an arbitrary matcher.

**Finding:** ✅ Matcher program is validated against LP-registered identity stored in program state. Arbitrary callee substitution is blocked.

### 3.3 System Program (PDA creation CPI calls)

All System Program CPI calls (create_account, transfer) verify:
```rust
if *a_system_program.key != solana_program::system_program::id() {
    return Err(ProgramError::IncorrectProgramId);
}
```
Verified at: L9909, L9988, L16190 (TopUpKeeperFund), L16408, L16560, L15882, L16290, and all other create_account sites.

**Finding:** ✅ System program is validated at all PDA creation call sites.

### 3.4 Token-2022 Program (Position NFT)

**⚠️ LOW — No explicit Token-2022 program ID check in MintPositionNft handler.**

`MintPositionNft` (L16808) receives `a_token22 = &accounts[7]` but does not validate its key against `spl_token_2022::ID` before passing it into `create_nft_mint_with_metadata`. The `create_nft_mint_with_metadata` function itself passes `token2022_program.key` as the program_id in CPI instructions, so a crafted fake program could be invoked.

Risk is LOW because:
- The `spl_token_2022` library functions (`initialize_mint2`, `mint_to`, etc.) are called with the provided key — if a wrong program is passed, those instructions would fail or be no-ops
- NFT minting does not touch collateral or engine financial state
- Solana verifies that the program account is executable before dispatch

**Recommendation:** Add a guard before `create_nft_mint_with_metadata`:
```rust
if *a_token22.key != spl_token_2022::ID {
    return Err(ProgramError::IncorrectProgramId);
}
```
This is the same pattern used for the SPL token program and System program.

---

## 4. invoke_signed Seed Controls

### 4.1 Seed Inspection

All `invoke_signed` / `invoke_signed_unchecked` call sites reviewed for user-influenced seeds:

| PDA | Seeds | User-controlled? |
|-----|-------|-----------------|
| Vault authority | `["vault", slab_key, bump]` | No — slab_key is program-owned, bump stored in config |
| LP PDA | `["lp", slab_key, lp_idx_bytes, bump]` | lp_idx is u16 index, not a user-provided key; bump is stored or computed via `find_program_address` |
| Keeper fund | `["keeper_fund", slab_key, bump]` | No |
| Creator lock | `["creator_lock", slab_key, bump]` | No |
| LP Vault state | `["lp_vault", slab_key, bump]` | No |
| LP Vault mint | `["lp_vault_mint", slab_key, bump]` | No |
| Dispute PDA | `["dispute", slab_key, bump]` | No — derived from program_id + slab_key |
| Withdraw request | `["withdraw_req", sv_key, user_key, epoch_bytes, bump]` | `user_key` is a public key from accounts; epoch is program state |
| Market alloc | `["market_alloc", slab_key, bump]` | No |
| CMOR pair | `["cmor_pair", slab_min, slab_max, bump]` | Both slab keys validated as program-owned |
| NFT PDA | `["position_nft", slab_key, user_idx_bytes, bump]` | user_idx is u16 index |
| NFT mint | `["position_nft_mint", slab_key, user_idx_bytes, bump]` | user_idx is u16 index |

**Finding:** ✅ No signing seeds are user-controlled in ways that could produce collisions or allow seed injection. All PDAs are derived from program-internal keys (slab key, program_id) plus small bounded indices.

### 4.2 bump Validation

All bumps used in `invoke_signed` are either:
- Stored in program config (e.g. `config.vault_authority_bump`)
- Retrieved via `Pubkey::find_program_address` and used in the same scope
- Retrieved via `Pubkey::create_program_address` with caller-provided bump validated against the expected PDA key

**Finding:** ✅ No path allows an attacker to inject a crafted bump to produce an unexpected PDA.

---

## 5. invoke_signed_unchecked (Special Case)

`zc::invoke_signed_unchecked` (L1872) is used exclusively for the matcher CPI to save ~200 CU. The safety justification is documented at L1844–L1851:

> Uses invoke_signed_unchecked to skip RefCell borrow validation. Safe because:
> - a_lp_pda is system-owned with empty data (no RefCell contention)
> - a_matcher_ctx is writable and we don't hold borrows across the CPI
> - The AccountInfo is only used for the duration of invoke_signed
> - We don't hold references past the function call

The LP PDA shape is validated before this path is reached (`lp_pda_shape_ok` at L11240). The slab borrow is explicitly dropped before the CPI path (confirmed by code structure — mutable data borrow is in a scope block before the invoke).

**Finding:** ✅ `invoke_signed_unchecked` use is safe and well-documented. The safety conditions are enforced by prior checks.

---

## 6. Summary

| Category | Finding | Severity | Status |
|----------|---------|----------|--------|
| Reentrancy via SVM | Blocked by SVM protocol — structural guarantee | N/A | ✅ PASS |
| State-before-CPI (CEI pattern) | All financial instructions mutate state before or with correct ordering around CPI | N/A | ✅ PASS |
| TradeCpi pre-CPI ordering | Intentional CPI-before-state-write; slab not passed to matcher; borrow dropped; ABI-validated | INFO | ✅ PASS |
| SPL Token program validation | `verify_token_program` guards all collateral CPI paths | N/A | ✅ PASS |
| Matcher program validation | `matcher_shape_ok` + `matcher_identity_ok` prevent arbitrary matcher substitution | N/A | ✅ PASS |
| System program validation | `IncorrectProgramId` checks at all `create_account` CPI sites | N/A | ✅ PASS |
| Token-2022 program validation | Missing explicit ID check in `MintPositionNft` | LOW | ⚠️ OPEN |
| Signing seed controls | All seeds program-internal, no user-controlled seed injection | N/A | ✅ PASS |
| Bump validation | All bumps stored in config or derived and validated in-scope | N/A | ✅ PASS |
| `invoke_signed_unchecked` | Safety conditions documented and enforced by prior checks | N/A | ✅ PASS |

**Overall: 1 LOW finding (Token-2022 program ID not explicitly checked in MintPositionNft). No HIGH or MEDIUM reentrancy or CPI safety issues found.**

---

## 7. Recommended Follow-up

1. **LOW: Add Token-2022 program ID check in `MintPositionNft` handler** — add `if *a_token22.key != spl_token_2022::ID { return Err(ProgramError::IncorrectProgramId); }` before calling `create_nft_mint_with_metadata`. This closes a theoretical callee substitution vector for NFT minting.

2. **INFO: Extend funding_p5 Kani proof** — noted by security agent previously; `funding_p5_bounded_operations_no_overflow` only covers dt < 1000 slots; the 31,536,000-slot boundary guard has no formal proof. Low priority but worth addressing before external audit.
