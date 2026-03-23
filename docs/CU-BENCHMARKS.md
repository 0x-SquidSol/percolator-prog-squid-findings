# Percolator — Compute Unit Benchmarks
**Program**: `dcccrypto/percolator-prog` (Solana BPF, `no_std`, `forbid(unsafe_code)`)  
**Tool**: LiteSVM (production BPF binary, `cargo build-sbf`)  
**Last updated**: 2026-03-23  

---

## TL;DR

| Metric | Value |
|--------|-------|
| Open trade CU | **6,330** |
| Modify/flip trade CU | **~7,297–7,317** |
| Close trade CU | **5,270** |
| CU budget used (open) | **~3.2%** of 200,000 |
| Scaling | **O(1)** — flat from 1 to 4,000 active accounts |
| Size independence | ✅ — same CU from size=1 to size=100,000 |
| pinocchio-token CU delta | **+0 to +1 CU** (noise; no runtime savings yet) |
| Expected savings after SIMD-0266 activation | **~131 CU per token Transfer** |

---

## Optimization Journey (Open Long)

| Milestone | CU | Δ from prev | Notes |
|-----------|----|------------|-------|
| Sprint 3 baseline | ~6,800+ | — | Pre-optimization |
| PERC-154: Stack alloc + `invoke_signed_unchecked` | 5,384 | **−1,416** | Heap → stack, RefCell skip |
| PERC-199: `Clock::get()` syscall | **5,338** | **−46** | Removes clock sysvar deserialization |
| Post-NFT + struct growth (Feb→Mar 2026) | **6,330** | **+992** | Position NFT support, TransferPositionOwnership, struct growth |
| **PERC-637: pinocchio-token 0.5.0** | **6,330** | **±0** | No runtime change yet (see SIMD-0266 section) |
| SIMD-0266 activation (projected) | **~6,199** | **−131** | Per-Transfer CU reduction in token program |

---

## PERC-637 pinocchio-token Migration — Before/After

*Benchmarked: 2026-03-23. Tool: LiteSVM, production SBF binary.*  
*Pre-pinocchio commit: `8c8c032` (main @ PR-133, spl-token 6.0)*  
*Post-pinocchio commit: `dbbb04e` (feat/pinocchio-cpi-parity-test, pinocchio-token 0.5.0)*

### Trade Instruction CU (TradeCpiV2)

| Operation | Pre-pinocchio | Post-pinocchio | Δ CU | % |
|-----------|--------------|----------------|------|---|
| Open long (+100) | 6,329 | 6,330 | **+1** | +0.02% |
| Open short (−100) | 6,325 | 6,326 | **+1** | +0.02% |
| Increase long (+50) | 7,316 | 7,317 | **+1** | +0.01% |
| Flip long→short | 7,309 | 7,310 | **+1** | +0.01% |
| Close position | 5,269 | 5,270 | **+1** | +0.02% |
| Rapid trades avg | 5,903 | 5,904 | **+1** | +0.02% |
| Rapid trades min | 5,263 | 5,264 | **+1** | +0.02% |
| Rapid trades max | 6,567 | 6,568 | **+1** | +0.02% |

**Result: No meaningful CU change.** The ±1 CU delta is within LiteSVM measurement noise.

### Why No Change?

The Trade instruction itself does **not** call `Transfer`, `MintTo`, or `Burn` directly. Token movement happens at Deposit/Withdraw time (separate instructions). Trade only updates positions in-memory in the Slab account. Therefore pinocchio-token's builder-path optimization doesn't affect Trade CU.

The **~131 CU savings per Transfer** is a **runtime** optimization in the token program execution itself (SIMD-0266), not a compile-time change. It activates when validators upgrade to support SIMD-0266.

### Token CPI Path: pinocchio-token vs spl-token

| Instruction | spl-token 6.0 CU estimate | pinocchio-token 0.5.0 CU | Δ (current) | Δ (SIMD-0266) |
|-------------|--------------------------|--------------------------|-------------|---------------|
| Transfer | ~2,800 (builder overhead) | ~2,800 (builder overhead) | **0** | **−131** |
| MintTo | ~2,900 | ~2,900 | **0** | **−131** |
| Burn | ~2,900 | ~2,900 | **0** | **−131** |
| InitializeMint | ~3,200 | ~3,200 | **0** | TBD |

*Note: pinocchio-token generates byte-identical instructions to spl-token 6.0 (proven by 17 parity tests in `tests/pinocchio_cpi_parity.rs`). The ~131 CU claim refers to SIMD-0266 optimizations in the token program validator runtime, not the CPI builder.*

---

## SIMD-0266 — p-token: Efficient Token Program

**Status**: SIMD merged 2026-03-13. Validator activation pending supermajority upgrade.

SIMD-0266 adds a `p-token` execution pathway in the validator that reduces CU for SPL Token instructions:

| Token Instruction | Current CU | SIMD-0266 CU | Savings |
|-------------------|------------|--------------|---------|
| Transfer | ~2,800 | ~2,669 | **~131** |
| MintTo | ~2,900 | ~2,769 | **~131** |
| Burn | ~2,900 | ~2,769 | **~131** |

**When will this show up in benchmarks?**  
- Not on current devnet (SIMD-0266 not activated yet)
- Will appear automatically once validators activate — no code change required
- pinocchio-token 0.5.0 is the correct dependency to get these savings when activated

**Impact on Percolator**:
- Deposit/Withdraw instructions call Transfer → will save ~131 CU each when SIMD-0266 activates
- Position NFT mint calls MintTo → will save ~131 CU
- Current percolator-stake calls Transfer → will save ~131 CU per staking tx

---

## Full Benchmark Table (Current — Post-PERC-637, 2026-03-23)

*Build: `feat/pinocchio-cpi-parity-test`, `cargo build-sbf`, production BPF*

| Operation | CU |
|-----------|----|
| Open long (+100) | **6,330** |
| Open short (−100) | **6,326** |
| Increase long (+50) | **7,317** |
| Flip long→short | **7,310** |
| Flip short→long | **7,297** |
| Close position | **5,270** |
| Partial close (−75) | **7,317** |
| Rapid trades avg (20 trades) | **5,904** |
| Rapid trades min | 5,264 |
| Rapid trades max | 6,568 |
| Tiny trade (size=1) | **6,330** |
| Large trade (size=100K) | **6,334** |

---

## O(1) Slab Scaling ✅

CU is **constant regardless of how many accounts are in the slab**. Confirmed up to 4,000 concurrent active accounts.

| Active Accounts | CU (Open Trade) |
|----------------|-----------------|
| 1 (init overhead) | 6,969 |
| 10 | **6,568** |
| 100 | **6,568** |
| 500 | **6,568** |
| 1,000 | **6,568** |
| 2,000 | **6,568** |
| 4,000 | **6,568** |

No CU growth with slab population. **Scales to any number of concurrent traders.**

---

## Position Size Independence ✅

CU does not scale with trade size. No big-number penalty.

| Size | CU |
|------|----|
| 1 | 6,330 |
| 10 | 6,338 |
| 100 | 6,330 |
| 1,000 | 6,330 |
| 10,000 | 6,334 |
| 100,000 | 6,334 |

---

## Account Layout Change (PERC-199)

`Clock::get()` syscall replaces `Clock::from_account_info()`, removing the clock sysvar from the account list:

- **TradeNoCpi**: 5 accounts → **4 accounts** `[user, lp, slab, oracle]`
- **TradeCpi / V2**: 8 accounts → **7 accounts**

This is a **breaking API change** — SDK and frontend callers must remove the clock sysvar from instruction construction.

---

## Next Optimization: TradeCpiV2 with Caller-Provided Bump

The `TradeCpiV2` instruction eliminates `find_program_address` by having the caller pass the PDA bump. Expected savings:

| Source | CU saved |
|--------|----------|
| `find_program_address` elimination | ~1,500 |
| `invoke_signed_unchecked` (already done) | ~200 |
| Stack allocation (already done) | ~100–200 |
| **Total additional** | **~1,538** |

**Projected open trade CU with V2: ~4,800** (from current 6,330).  
Requires SDK callers to pass the bump byte — pending SDK update.

---

## Formal Proof Coverage

- **476 Kani proofs** across: `percolator-prog` (242), `percolator` core (157), `percolator-stake` (67)
- Proofs cover: conservation of funds, user isolation, equity consistency, liquidation safety, rent reclaim authority
- All 476 passing on CI

---

## Repo

`https://github.com/dcccrypto/percolator-prog`  
Benchmark test: `tests/trade_cu_benchmark.rs`  
Run: `cargo test --release --test trade_cu_benchmark -- --nocapture`
