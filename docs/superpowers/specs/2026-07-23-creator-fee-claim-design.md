# Creator fee claim — design

**Date:** 2026-07-23
**Scope:** `percolator-prog` (wrapper) only. The engine (`percolator`) is FROZEN and is not touched.
**Status:** approved (design), pending implementation.

## Problem

A market creator has a configured fee share (`creator_share_bps`, default 1600 bps of T) but **no safe way to claim it**.

Verified against the current post-fee-split wrapper source:

- The four-way `split_trade_fee` (`v16_program.rs:6177`) routes protocol / LP / insurance legs into dedicated monotonic counters (`protocol_fee_accrued_atoms`, `lp_fee_accrued_atoms`, `insurance_reserve_accrued_atoms`, `:7971-7982`).
- **The creator leg is the exception.** It routes
  `credit_trade_fees_to_market_budgets_view` (`:6947`) →
  `credit_fee_to_domain_budget_view` →
  `credit_domain_insurance_budget_not_atomic`
  i.e. into the **insurance domain budget**.
- That budget **is the loss backstop** — the engine draws it down via
  `consume_domain_insurance_for_negative_pnl` (`percolator/src/v16.rs:8084`) to cover negative trader PnL / bad debt.
- The only withdraw path is tag 57 `WithdrawInsuranceAsset` (`:10600`), gated on `insurance_operator`
  (defaults to the creator: `insurance_operator = config.marketauth` at InitMarket, `:1928`),
  a market-wide cooldown (#396), and `live_domain_withdraw_health_or_shutdown_view` (`:6466`) — which
  only blocks withdrawal **during** active stress (bankruptcy / threshold-stress / loss-stale / recovery),
  not before one. Amount is clamped only by vault balance (`require_token_balance`, `:9966`).

**Consequences:** (1) a "claim fees" button is really a "withdraw from the market's own loss backstop"
button — a creator can drain the backstop preemptively while the market is healthy; (2) there is **no
on-chain figure for "creator earned X"**, since creator revenue is commingled with the backstop, so an
honest claimable balance cannot even be displayed.

## Design

Separate creator revenue into its own claimable counter, and give it a dedicated withdraw instruction.

### 1. Storage — reuse existing padding, do NOT grow the config

`WrapperConfigV16` is 576 B with a 10-byte `_padding_split` tail. Adding the conventional
`u128` accrued+withdrawn pair (32 B) would grow it to 608 B, shifting `MARKET_GROUP_OFF`
(592→624) and **every** asset-profile offset, and breaking the already-deployed 576-byte markets
(`BPgSUbDs…`, `7FBXdrm1…`) — a repeat of the 496→576 offset incident.

Instead, place a single `u64` inside the existing pad at the only 8-aligned slot:

| Field | Offset | Note |
|---|---|---|
| `creator_share_bps: u16` | 560..562 | unchanged |
| `lp_share_bps: u16` | 562..564 | unchanged |
| `insurance_share_bps: u16` | 564..566 | unchanged |
| `_padding_split: [u8; 2]` | 566..568 | was `[u8; 10]` |
| **`creator_fee_claimable_atoms: u64`** | **568..576** | **NEW**, 8-aligned |

`WRAPPER_CONFIG_LEN` stays **576**. No existing field moves. `bytemuck::Pod` holds (no implicit
padding; struct align 16, size 576 is a multiple of 16). The compile-time guard
`assert!(size_of::<WrapperConfigV16>() == WRAPPER_CONFIG_LEN)` (`:1185`) continues to pass unchanged.

**Backward compatibility:** deployed markets have bytes 566..576 zeroed (they were padding), so after an
in-place program upgrade the counter reads `0` and accrues fresh. No market migration, no new addresses,
no SDK/keeper/frontend *offset* changes.

**Accepted trade-offs:** `u64` rather than `u128`, and a single "claimable" counter rather than the
accrued/withdrawn audit pair the other legs use — both forced by the 10-byte budget. Magnitude is a
non-issue (u64 ≈ 1.8e19 atoms ≈ $18T at 6dp). Creator fees already sitting in the insurance budget from
before the upgrade are **not** migrated (negligible on the devnet test markets).

### 2. Routing — creator leg leaves the backstop

**Verified site enumeration — there are TWO logical paths, and the batch path does NOT go through
`credit_trade_fees_to_market_budgets_view`:**

| Path | Creator amount | Sink |
|---|---|---|
| Single trade | `split_a.creator`→`domain_fee_a` (`:7954`), `split_b.creator`→`domain_fee_b` (`:7955`) | `credit_trade_fees_to_market_budgets_view` (`:7989`) → `credit_fee_to_domain_budget_view` (`:6947`,`:6948`) |
| **Batch** | `split_leg.creator`→`domain_amount_leg` (`:8276`) | `credit_fee_to_domain_budget_view` **directly** at `:8296` (taker) and `:8311` (maker) |

Both must be re-routed. In the batch loop, accumulate a `creator_cut_running_total` alongside the existing
`protocol_/lp_/insurance_cut_running_total` (`:8277-8285`) and add it to the counter once after the loop —
mirroring how the other three legs are already handled there.

**Completion guard:** every caller of `credit_fee_to_domain_budget_view` (`:6947`, `:6948`, `:8296`,
`:8311`) carries a creator amount, so after this change that function — and its only wrapper,
`credit_trade_fees_to_market_budgets_view` — become dead. Remove them (or the build will warn), and a grep
for either name must return only the definition-site deletion. A test must assert the insurance domain
budget is unchanged by a trade on **both** the single and batch paths.

At each site, replace the creator-leg credit with an accrual to the new counter:

```
creator_cut_total = split_a.creator + split_b.creator      // checked
cfg.creator_fee_claimable_atoms = cfg.creator_fee_claimable_atoms
    .checked_add(u64::try_from(creator_cut_total)?)         // overflow → EngineArithmeticOverflow
cfg_after = Some(cfg);                                      // MUST force write-back
```

The `cfg_after = Some(cfg)` write-back is load-bearing — the existing comment at `:7983-7987` warns a
missed write-back **silently discards accrued fees**. The same applies here.

Consequence (intended): the insurance domain budget no longer receives the per-trade creator drip. That
drip was the footgun. The backstop remains funded by explicit insurance top-ups, the LP vault, and the
maintenance-fee credit (`credit_maintenance_fee_to_active_market_budgets_view`, `:6972`).

### 3. Withdraw — new instruction, tag 90

`WithdrawCreatorFee { amount: u128 }`, dispatch tag **90** (verified free; 83 also free).

- **Authority:** signer must match `insurance_operator` **and ONLY that**. It defaults to the creator
  (`= marketauth` at InitMarket, `:1928`) and — unlike `marketauth`, which `StakeInitPool` rotates to the
  stake-pool PDA — it is **not** rotated by staking, so the creator can still claim on a staked market.
  *Verified:* the only writes to `insurance_operator` are the InitMarket default (`:1928`), the new-slot
  setter in `activate_dynamic_asset_slot` (`:2166`), and an explicit **preserve** on reconfiguration
  (`profile.insurance_operator = existing.insurance_operator`, `:6627`). The marketauth rotation
  (`cfg.marketauth = new_pubkey`, `:11879`) assigns `marketauth` only — it never touches
  `insurance_operator`.
  Note this deliberately diverges from `verify_domain_withdrawal_preflight` (`:9953`), which accepts
  `marketauth` as an alternate gate: accepting `marketauth` here would let a **staked market's pool PDA**
  claim the creator's revenue. Do not reuse that preflight's authority check verbatim.
- **Capacity clamp:** `amount <= cfg.creator_fee_claimable_atoms`; on success decrement the counter by
  `amount`. Reject over-claims (do not saturate).
- **Token movement:** mirror `handle_withdraw_protocol_fee` (tag 84, `:10796`) — vault → creator's token
  account via the derived vault authority, `require_token_balance`, `verify_withdrawable_token_accounts`.
- **Deliberately NOT reused:** the tag-57 health-check/cooldown path. This counter is genuinely the
  creator's earned revenue and is disjoint from the backstop, so backstop-health gating does not apply.

### 4. Client changes (additive only)

- **SDK:** export the counter's offset (568) + a parse accessor, and `encodeWithdrawCreatorFee` + accounts
  const for tag 90. Minor version bump. No offset constants change.
- **Keeper:** no change required (relink only if the SDK version is bumped in lockstep).
- **Frontend:** creator-claim UI reads `creator_fee_claimable_atoms` for an accurate claimable balance and
  calls tag 90. It can no longer touch the backstop.

## Testing (no vacuous tests; every assertion mutation-proven)

1. **Layout:** `size_of::<WrapperConfigV16>() == 576`; counter reads/writes at byte 568..576 LE; the three
   share fields still read at 560/562/564 (guards against an accidental reorder).
2. **Backward compat:** a config built from an all-zero 566..576 tail yields `claimable == 0`.
3. **Accrual:** a trade credits exactly `split_a.creator + split_b.creator` to the counter and **zero** to
   the insurance domain budget (the negative half is the point — assert the budget is unchanged).
4. **Write-back:** a trade whose only cfg mutation is the creator accrual still persists (regression guard
   for the `cfg_after` trap).
5. **Withdraw:** claim < capacity succeeds and decrements; claim == capacity drains to zero; claim >
   capacity is rejected; wrong signer rejected; claim on a **staked** market (marketauth rotated) still
   succeeds via `insurance_operator`.
6. **Isolation:** `WithdrawCreatorFee` cannot reduce `insurance_domain_budget`; `WithdrawInsuranceAsset`
   cannot reduce `creator_fee_claimable_atoms`.
7. **Overflow:** `checked_add` / `u64::try_from` paths error rather than wrap.

## Rollout

1. Implement + test in `percolator-prog`.
2. Rebuild **with `--features devnet`** (load-bearing — a default build changes the stake-program pin) and
   hash-verify.
3. **Deploy is USER-GATED.** In-place upgrade of `DhSkE7u…` (config length unchanged, so existing markets
   keep working).
4. Bump SDK, relink keeper, build the creator-claim UI.

## Out of scope

Migrating pre-existing creator fees out of the insurance budget; re-routing the insurance leg (currently
funds the staker reserve, tag 87); any engine change.
