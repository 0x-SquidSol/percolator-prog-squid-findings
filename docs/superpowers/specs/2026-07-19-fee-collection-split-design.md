# v17 Fee Collection Split — Design

**Date:** 2026-07-19
**Status:** Approved design, pending implementation plan
**Scope:** wrapper (`percolator-prog`), stake (`percolator-stake`), SDK, keeper, app. **Engine (`percolator`) unchanged.**

---

## 1. Problem

The v17 fee split exists as *policy* — config fields, floors, validation, SDK encoders — but the *collection* that would route money to two of its four legs was never built. An audit on 2026-07-19 (against deployed commits wrapper `f6a83370`, engine `c87a8978`, stake `1e08d35`, nft `f18da24`) found five gaps of one shape: **the instruction exists, the SDK can encode it, but no live path reaches it.**

### 1.1 What the split actually is today

The split is encoded as a *ratio between two fee rates*, not as share percentages:

```
T = trade_fee_base_bps + backing_fee_bps
creator   ≤ 45% of T  →  trade_fee_base_bps
LP        ≥ 40% of T  →  backing_fee_bps  ─┬─ split by insurance_share_bps
insurance ≥ 15% of T  →                    ─┘
```

`trade_fee_base_bps` collects on every trade. `backing_fee_bps` does not, for two independent reasons:

1. **Rate is zero everywhere.** `handle_init_market` leaves it unconditionally 0 (`v16_program.rs:~6968`). `UpdateBackingFeePolicy` (tag 51) is the only setter and nothing calls it — zero hits in `newmarkets.ts`, `useCreateMarket.ts`, keeper, or oracle-keeper.
2. **Lien-gated even if set.** `collect_backing_domain_fees_for_account_view` (`v16_program.rs:15391`) charges only when `source_lien_counterparty_backing_num` increases. A lien is drawn only when raw capital fails initial margin (`v16.rs:7576`, lien created at `v16.rs:8737`). Well-capitalized traders never trigger it.

Consequently `fee_split_floor_ok` **skips entirely** when `backing_fee_bps == 0` (`v16_program.rs:5657`) — the floor hardened in W11 has never executed on a live market.

### 1.2 Actual vs decided economics

| Leg | Decided (T=20 bps) | Actual on ordinary volume |
|---|---|---|
| Protocol | 4.0 bps (20%) | **4.0 bps** — works |
| Creator | 3.2 bps (16%) | **16.0 bps** — receives everything else |
| LP | 9.6 bps (48%) | **0** |
| Insurance | 3.2 bps (16%) | **0** |

Deployed reality is **20% protocol / 80% creator**. The decided split is unachievable by any configuration of the deployed program.

### 1.3 The other gaps

- **Creator cannot withdraw.** `WithdrawInsuranceAsset` (tag 57) is the only claim path for the one leg that accrues. SDK encoder exists (`percolator-sdk/src/abi/instructions.ts:3292`); zero call sites.
- **Stake has no upside leg.** `total_pool_value = deposited − withdrawn − flushed + returned + fees(mode 1 only) − realized_junior_loss` (`percolator-stake/src/state.rs:604-608`). `FlushToInsurance` lowers pool value; `ReturnInsurance` is a manual admin reversal capped at `total_flushed` ("no over-settle", `:1367-1371`), so flush→return is at best break-even. `fees` requires mode 1, and `AccrueFees` measures vault SPL surplus over `total_pool_value()` — a surplus nothing produces, because the wrapper contains no transfer or CPI into a stake vault. Stake is a pure loss-absorbing tranche with no compensation.
- **`maintenance_fee_per_slot` has no setter.** It is an `InitMarket` constructor argument with no update instruction anywhere in the dispatch table, hardcoded 0 by wizard and seed script — permanently frozen per market.
- **Protocol fee is never swept.** Tags 84/85 work; only callers are one-off verification scripts.

### 1.4 Why the test suite did not catch this

The suite's assertions are real and would fail if the mechanism broke, but they operate at the **mechanism layer** and manufacture preconditions that do not exist in production:

- `m8_stake_nft_matrix.rs:886-945` calls `InitTradingPool` itself, then cheat-writes +7,500 into the vault via `rpc.set_account` (comment: "simulated trading-fee sweep").
- `m10_lp_vault_full_cycle.rs` builds a 1×-max-leverage market (`initial_margin_bps: 10_000`, `:110`), deposits 2,200 against 2,000 notional ("thin-margined by design", `:289`), and back-solves Trade B's size to force a lien (`:394-398`).

Nothing in the suite asserts a product claim. That is how 99% coverage and `anyFaked = NONE` coexisted with two zero-earning features.

---

## 2. Approach

**Collapse to a single fee rate with explicit share fields.** Collect all four legs at the three existing trade-fee sites using the counter pattern the protocol fee already proves. Leave the backing fee exactly as upstream wrote it, dormant at rate 0, permanently.

Rejected alternatives:

- **Make the backing leg collect on ordinary volume.** Would preserve the two-rate encoding, but changes the backing fee's meaning from "you drew on backing" to "you traded," charges traders twice, and would surface the bucket-freshness deadlock routinely (making the lapse-to-limbo fix a hard prerequisite). Still requires a client to call tag 51 on every market forever.
- **Hybrid — use both mechanisms.** Two overlapping fee mechanisms with near-identical names; this naming collision is the direct cause of the current confusion.

The deciding property: in the chosen approach, money reaching each leg is a **structural** consequence of trading, not a configuration someone must remember to set.

---

## 3. The split model

`T = trade_fee_base_bps` is the entire fee. It is what the trader is quoted and what the trader pays. Nothing else charges.

Shares are bps **of T**, so the decided policy carries over verbatim:

| Leg | Field | Default | Floor |
|---|---|---|---|
| Protocol | none — compile-time `PROTOCOL_FEE_BPS` | 2000 | fixed, not settable |
| Creator | `creator_share_bps` | 1600 | ≤ 4500 |
| LP | `lp_share_bps` | 4800 | ≥ 4000 |
| Insurance → stakers | `insurance_share_bps` | 1600 | ≥ 1500 |

The three stored fields must sum to exactly `10_000 − PROTOCOL_FEE_BPS` = **8000**.

This replaces `FEE_SPLIT_SHARE_TOLERANCE_FLAT = 5001` and its tolerance apparatus. With one rate there is no cross-rate rounding to absorb, so validation is exact integer comparison and floors are the literal decided percentages.

**Rounding.** Each leg is `floor(fee × share / 10_000)`. Residual dust goes deterministically to the **insurance leg** — the most conservative destination, since it grows the backstop rather than anyone's withdrawable revenue.

**Conservation invariant:**

```
protocol_cut + creator_cut + lp_cut + insurance_cut == fee    (exactly, for all inputs)
```

---

## 4. Schema

Additive at the tail of `WrapperConfigV16`, following the protocol-fee precedent (432→496 B):

```rust
// u128 counters FIRST — see alignment note below.
pub lp_fee_accrued_atoms: u128,
pub lp_fee_withdrawn_atoms: u128,
pub insurance_reserve_accrued_atoms: u128,
pub insurance_reserve_withdrawn_atoms: u128,
pub creator_share_bps: u16,
pub lp_share_bps: u16,
pub insurance_share_bps: u16,
pub _padding_split: [u8; 10],
```

+80 B → **496 → 576 B**. `WRAPPER_CONFIG_LEN` bumps in lockstep; the existing
`const _: () = assert!(size_of::<WrapperConfigV16>() == WRAPPER_CONFIG_LEN)` catches desync at compile time.

**Field order is load-bearing, not cosmetic.** `WrapperConfigV16` derives `bytemuck::Pod`, which forbids *implicit* padding. The current struct ends at 496 B (a multiple of 16, so `u128`-aligned). Placing the three `u16` shares before the counters would push the `u128`s to offset 502, forcing the compiler to insert implicit padding and **failing the `Pod` derive at compile time**. Counters therefore come first (496→560, still aligned), then the shares (566), then explicit `_padding_split` to round the struct up to its 16-byte alignment (576). The explicit array is what keeps the padding visible to `Pod`.

**The creator leg gets no counter.** It keeps the existing `domain_fee_a/b` → `credit_fee_to_domain_budget_view` → `insurance_domain_budget` path, claimed by tag 57 unchanged. Only its amount changes, from "everything left over" to a configured share. This minimises new surface and preserves the one leg already proven to accrue.

**Consequence:** `insurance_domain_budget` now contains creator revenue *only*. The risk of a creator draining the market's loss backstop is eliminated structurally by the schema, not by a withdrawal floor.

---

## 5. Collection

One pure function, three call sites:

```rust
pub fn split_trade_fee(fee: u128, cfg: &WrapperConfigV16)
    -> Result<FeeSplitParts, ProgramError>   // { protocol, creator, lp, insurance }
```

All arithmetic lives here — `fee_share_floor` per leg, dust to insurance, `parts.sum() == fee` guaranteed internally. Pure and side-effect-free, so it is directly Kani-provable and unit-testable without a validator (same rationale as the existing `maintenance_cranker_reward` extraction).

Call sites: `v16_program.rs:7362`, `:7364` (single trade), `:7665` (batch). These fire on every trade, so every leg collects unconditionally.

Routing: protocol → `protocol_fee_accrued_atoms`; LP → `lp_fee_accrued_atoms`; insurance → `insurance_reserve_accrued_atoms`; creator → existing `domain_fee_a/b` credit path.

**Safe defaults at `InitMarket`.** Shares are written as hardcoded constants `1600 / 4800 / 1600`, **not instruction arguments** — mirroring `protocol_fee_authority`, so no market can be created with a zero or hostile split. A market that never calls any setter pays all four legs correctly from its first trade. Every gap in §1 exists because revenue depended on someone remembering to call something; here, forgetting is safe.

**New setter `UpdateFeeSplit` (tag 86)**, marketauth-gated. Validates floors and `sum == 8000`, then writes the three shares. `UpdateTradeFeePolicy` keeps setting `T` and loses its split-validation role.

**W11 supersession.** `fee_split_floor_ok` and `FEE_SPLIT_SHARE_TOLERANCE_FLAT` are removed from the two setter handlers. That code is correct and the bypass it closed was real, but it validates a two-rate ratio that no longer exists and already no-ops on every live market. The replacement is stricter: exact, no tolerance, no skip path, and it actually runs.

---

## 6. Claim paths

| Leg | Path | Change |
|---|---|---|
| Protocol | `WithdrawProtocolFee` (84) | none — needs a caller (§7) |
| Creator | `WithdrawInsuranceAsset` (57) | none — needs a caller (§7) |
| LP | `LpVaultCrankFees` (78) | repoint at `lp_fee_accrued_atoms` |
| Stakers | `WithdrawInsuranceReserveToStake` (tag 87) | new wrapper instruction |

Dispatch currently tops out at tag 85 (`SetProtocolFeeAuthority`), so 86/87/88 are the next free tags.

**LP.** Tag 78 already exists as the "sync earnings into the vault ledger" crank and currently dead-ends at `LpVaultNoFeesToCrank(38)`. Repointed, it pulls accrued → marks withdrawn → credits vault NAV; the existing redemption path pays LPs unchanged. Permissionless.

**Stakers.** `WithdrawInsuranceReserveToStake` is permissionless and transfers `insurance_reserve_accrued − insurance_reserve_withdrawn` into the stake vault. This produces exactly the vault surplus `AccrueFees` already measures, so stake's fee accounting — already correct and already tested — starts working with no change to its math.

### 6.1 Stake mode resolution

Modes 0 and 1 are mutually exclusive in a way that collides with this product: mode 0 (insurance pool) permits `FlushToInsurance` but forbids fee accrual; mode 1 (trading LP) permits fees but forbids flush. Stakers need both — they absorb losses *and* earn the insurance leg.

**Decision: extend mode 0 to accrue fees.** Two surgical edits in `percolator-stake`:

1. `process_accrue_fees` — allow `pool_mode == 0` alongside 1 (`processor.rs:2586-2588`)
2. `total_pool_value` — count `total_fees_earned` for mode 0 as well as mode 1 (`state.rs:597-601`)

Rationale: mode 0 is semantically the insurance pool, which is what these are; flush is the loss-absorption mechanism and must stay; and **every existing client already calls `InitPool` (mode 0)**, so no seed-script or client change is needed and `InitTradingPool` becomes unnecessary. Same principle as safe defaults — the working path is the one people already take.

Junior/senior tranche math and `realized_junior_loss` are untouched. Stakers keep absorbing losses in the existing order; they now have an upside leg to be compensated by.

---

## 7. Remaining pieces

**Maintenance fee setter.** Add `UpdateMaintenanceFeePerSlot` (tag 88), marketauth-gated, mirroring existing policy setters. **Default remains 0 and this design does not turn it on.** The defect is that the value is frozen at `InitMarket` with no setter, so it is permanently unchangeable per market; this restores optionality only. Enabling it is a separate product decision requiring its own UX story, since it charges accounts continuously.

**Protocol fee sweep.** A periodic threshold-gated task in `percolator-keeper` — sweep only above a minimum so fees are not burned on dust transactions. Threshold is a keeper env var (`FEE_SWEEP_MIN_ATOMS`), defaulting to a value covering ~100× the transaction cost so a sweep is never net-negative. No program change.

**Client work.** Creator claim UI calling tag 57; wizard calls `UpdateFeeSplit` when a creator picks a non-default split. The wizard's split control keeps its current shape, writing share bps instead of back-solving two rates.

---

## 8. Component map

| Component | Change | Size |
|---|---|---|
| wrapper | schema +72 B; `split_trade_fee`; 3 call sites; `UpdateFeeSplit`; `WithdrawInsuranceReserveToStake`; repoint tag 78; `UpdateMaintenanceFeePerSlot`; drop `fee_split_floor_ok` from setters | moderate |
| stake | 2 edits (§6.1) | tiny |
| **engine** | **none** | **zero** |
| SDK | encoders for 3 new tags; config decoder reads share bps | small |
| keeper | crank 78; sweep 84; push insurance→stake | small |
| app | creator claim UI; wizard writes share bps | small |

**Keeper structure.** The three permissionless cranks go in **one shared threshold-gated loop**, not three. They are the same shape — read a counter, act if above threshold — so one loop means one place to reason about cadence and failure. Reuses the retry/backoff hardening from the recovery-cranker work.

**Deploy sequencing.** Wrapper and stake deploy together as one user-gated upgrade. Then SDK, then keeper, then app — each depends on the prior. Zero markets exist on the fresh triple (`DhSkE7u` / `GCHhcgw` / `CNGBPZR`), so there is no migration and no coexistence window; markets seeded after deploy get correct defaults from birth.

This is a two-program change, not wrapper-only. The stake edits do not touch its tranche math or any security-reviewed guard, but stake requires redeploy and re-verification alongside the wrapper.

---

## 9. Invariants and error handling

1. **Conservation** — `protocol + creator + lp + insurance == fee`, exactly, all inputs. Enforced in `split_trade_fee` via dust-to-insurance; carried as a Kani proof. Breaking it desyncs `header.insurance` and the vault accounting with it.
2. **Monotonicity** — `accrued` and `withdrawn` only rise; `withdrawn ≤ accrued` always; capacity is the difference. Same shape as the protocol fee.
3. **Solvency** — `c_tot + insurance + earnings ≤ vault` holds because every cut is debited from the domain credit before being credited elsewhere. This is the trap that sank the earlier "Option A" (crediting bucket earnings with no matching debit); safety is inherited from the protocol fee's existing structure.
4. **Split validity** — `sum == 8000` and floors hold at write time; defaults satisfy it by construction.

**Write-back trap.** `v16_program.rs:7381-7384` warns that `cfg` write-back was opt-in per mutation and a missed write-back *silently discards accrued fees*. Three counters now mutate on every trade, making this the most likely way this design loses money in production. The forced-write-back pattern applies to all three, with a test that trades and re-reads config from chain to confirm persistence.

**Floor checks never go in load-time validators.** W11's own comment explains why: `validate_wrapper_config` runs on every deserialize, so a floor there retroactively bricks any market whose stored split fails it. The new check lives **only** in `UpdateFeeSplit`.

**Distinct error codes.** New: `FeeSplitSumInvalid`, `FeeSplitFloorViolation`, `NoInsuranceReserveToClaim`. Tag 78 retains `LpVaultNoFeesToCrank(38)`. Tests assert exact codes via `expect_custom`; never "any error."

**Missing counterparty.** If a market has no stake pool or LP vault, that leg accrues and stays unclaimed as unbudgeted surplus in `header.insurance` — still solvency-supporting, claimable once the vault or pool exists. **No silent rerouting to another leg**: a trader's fee never vanishes and never quietly becomes someone else's revenue.

---

## 10. Testing

The suite's failure mode was real tests at the wrong layer. This adds the missing layer.

### 10.1 Product-layer driver

`m17_fee_split_product.rs`, with one structural rule: **real instructions only.** No `rpc.set_account` state forgery, no cheat-writes, no `InitTradingPool`, no hand-sized trades, no engineered leverage. Market created by the **real seed path** with **no setter calls**. Trade is ordinary and well-capitalized at default leverage — what `TradeForm.tsx` actually produces.

Core assertion:

> After one ordinary trade on a default market, all four of `protocol_fee_accrued`, `lp_fee_accrued`, `insurance_reserve_accrued`, and `insurance_domain_budget` are strictly greater than zero.

This single assertion would have caught all five gaps in §1.

### 10.2 Value delivery, measured in token balances

Accrual is not income. Each leg proven end-to-end by actual balance deltas:

| Leg | Path | Assertion |
|---|---|---|
| LP | trade → crank 78 → redeem | receives more than principal |
| Staker | trade → push to stake → `AccrueFees` → withdraw | receives more than deposit |
| Creator | trade → tag 57 | balance rises by accrued amount |
| Protocol | trade → tag 84 | balance rises by accrued amount |

The staker case inverts today's verified behavior — a staker currently withdraws $700 on a $1000 deposit. The test demands strictly positive return and fails loudly if the upside leg does not land.

### 10.3 Non-vacuity

Every new assertion is mutation-proven: perturb the expected value → must FAIL → restore → must PASS. No assertion enters the suite without that cycle recorded.

### 10.4 Formal proofs

Kani proofs for conservation, monotonicity, and floor validity. `split_trade_fee` is pure specifically to make these cheap. Declare `u16`/`u128` widths honestly rather than assuming ranges on wider types — CBMC bit-blasts by declared width, which caused a 10h57m non-convergence previously.

### 10.5 Negative and persistence tests

Negative: `sum != 8000`, each floor violated, claim-with-nothing-available — each asserting its specific code.

Persistence: trade, then re-read config from chain and confirm counters persisted (§9 write-back trap).

### 10.6 Suite hygiene

`m10` is annotated as a mechanism proof so it cannot be re-read as product evidence. The deploy memo's "LP-VAULT YIELD FULL CYCLE PROVEN E2E" claim is already corrected.

All tests run against **deployed bytecode**, hash-pinned, never cheatcode-mounted — avoiding the stale-mount trap that served `efe48d83` while `8a833a53` was live.

---

## 11. Out of scope

- Enabling `maintenance_fee_per_slot` (setter only; default stays 0)
- Fixing the backing-fee lien-gate or the bucket lapse-to-limbo defect — routed around, not through; both remain open engine-level issues
- The multi-asset accrue-staleness lock
- Migrating the old deployment (`69VUZ7a2` / `51CeUNpb` / `5TnritLt`), which remains live and independent
