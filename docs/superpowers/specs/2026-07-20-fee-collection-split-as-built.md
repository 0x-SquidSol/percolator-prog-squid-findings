# v17 Fee Collection Split — AS BUILT

**Date:** 2026-07-20
**Status:** Programs implemented; not deployed. Clients not built.
**Supersedes:** `2026-07-19-fee-collection-split-design.md`, which describes the *planned* design and is **stale in ways that matter** (see §10).

Wrapper `percolator-prog` `feat/protocol-fee-taker-only` @ `2b3a6a65` (18 commits).
Stake `percolator-stake` `feat/adopt-stake-lineage-plus-n7` @ `474079f` (5 commits).
Engine `percolator` @ `c87a8978` — **untouched. Zero upstream divergence.**

---

## 1. What ships

`T = trade_fee_base_bps` is the whole fee. It splits four ways at every trade-fee credit site.

| Leg | Share of T | Where it lands | How it is claimed |
|---|---|---|---|
| Protocol | 2000 bps (constant) | `cfg.protocol_fee_accrued_atoms` | `WithdrawProtocolFee` (84) — token transfer out |
| Creator | `cfg.creator_share_bps`, default 1600 | `insurance_domain_budget` (existing path) | `WithdrawInsuranceAsset` (57) |
| LP | `cfg.lp_share_bps`, default 4800 | `cfg.lp_fee_accrued_atoms` | `LpVaultCrankFees` (78) — reclassified to LP backing principal |
| Insurance → stakers | `cfg.insurance_share_bps`, default 1600 | `cfg.insurance_reserve_accrued_atoms` | `WithdrawInsuranceReserveToStake` (87) — token transfer to the stake vault |

Shares are bps **of T** and must sum to `FEE_SHARE_TOTAL_BPS = 10_000 − PROTOCOL_FEE_BPS = 8000`.

Floors (percentages of the post-protocol remainder, converted to bps-of-T by `pct × 8000`): `MAX_CREATOR_SHARE_BPS = 3600`, `MIN_LP_SHARE_BPS = 3200`, `MIN_INSURANCE_SHARE_BPS = 1200`. These sum to exactly 8000, so they are **precisely complementary** — `creator > 3600` necessarily drags another leg under its floor, and a single-violation creator case does not exist.

Defaults are hardcoded at `InitMarket`, never instruction arguments. **A market that never calls any setter still pays all four legs correctly from its first trade.**

`split_trade_fee` computes protocol/creator/lp as `floor(fee × bps / 10_000)` and insurance as the **remainder**, so conservation is exact by construction (Kani-proven, `kani_fee_split_conserves`). Consequence: sub-atom rounding all lands on insurance. Each leg first receives a nonzero amount at `f_min = ceil(10_000/bps)` — LP at fee ≥ 3, protocol ≥ 5, creator ≥ 7; insurance takes 100% of fees 1–2. Not exploitable: diverting atoms to a protocol reserve the actor does not own costs a full transaction per 1–4 atoms.

---

## 2. How LP actually gets paid — this is the part the old spec got wrong

The old spec assumed `LpVaultCrankFees` credits vault NAV. **It does not.** Its only sink was `registry.fee_distribution_total_atoms`, a counter **written and never read**. A first implementation shipped against that assumption and paid nobody while passing its tests.

Two naive fixes are both wrong:
- Crediting `ledger.total_earnings_atoms` alone **bricks redemption** — `ExecuteRedemption` gates on `earnings_portion > bucket.utilization_fee_earnings`.
- Crediting `bucket.utilization_fee_earnings` via `credit_backing_provider_earnings_not_atomic` **eats the junior residual**. Trade fees are `c_tot -= charged; insurance += charged` at constant vault (engine `v16.rs:13798-13805`), so the senior sum is exactly flat — there is no slack to consume.

**What shipped instead.** `withdraw_insurance_surplus_not_atomic` (engine `v16.rs:7942`) does *only* `insurance -= a; vault -= a` and **moves no tokens** — the wrapper decides separately whether to CPI a transfer. So tag 78:

```
v_before = header.vault
withdraw_insurance_surplus_not_atomic(a)   →  I−a, V−a
header.vault = v_before                    →  V restored, tokens never left
add_fresh_counterparty_backing_view(…, a·BOUND_SCALE)   →  FB+a
ledger.total_principal_atoms += a
```

Net `C + (I−a) + E + (FB+a) = S` against unchanged `V` — identical to the starting state. Verified by hand including the intermediate `(V−a, I−a)` point, where the margin is preserved rather than consumed.

**LP yield is therefore junior at-risk backing capital, not a senior earnings claim.** It can be impaired by backing losses between crank and redemption. This was an explicit decision, taken to preserve zero engine divergence; the alternative was a ~15-line engine primitive making it senior.

Proven by a redeemer's real SPL balance: 999,000 → 1,248,750 (delta 249,750 = exact pro-rata).

---

## 3. Claim-path guards (added after review; not in the old spec)

**Tag 78** is permissionless and now gates on:
- mode — rejects Recovery/Resolved (`Custom 21`) and matured-Live (`Custom 27`). Without this, any signer could convert a recovery buffer into LP backing that LPs then redeem.
- `total_lp_shares_outstanding > LP_VAULT_MINIMUM_LIQUIDITY` (`Custom 41`) — otherwise a crank after full LP exit orphans atoms into a ledger nobody can redeem against.

**Consequence, accepted:** LP fees accrued on a market that later **Resolves** can never be cranked (Resolved is terminal). A carve-out would create a junior obligation during wind-down.

**Tag 87** is permissionless and moves real tokens out, so its destination is pinned three ways (§4).

**Consequence, accepted:** the insurance leg is **forfeited** if a market resolves before it is pushed. `WithdrawInsuranceAsset` (57) cannot recover it — tag 41 is budget-scoped and this leg is unbudgeted by construction of the clamp.

**Mandatory clamp.** Tags 78, 84 and 87 all draw from one pool: `insurance − source_insurance_credit_reserved_total_atoms − insurance_domain_budget_remaining_total`. Each clamps to engine-available surplus, mirroring `handle_withdraw_protocol_fee`. Each advances its `withdrawn` counter by the **amount actually applied**, never the requested amount, so partial fills leave the remainder claimable. Without the clamp, whichever leg cranks last gets `EngineLockActive`.

**No reservation.** The LP claim is deliberately **not** added to `additional_reserved` at the `credit_account_from_insurance_not_atomic` call sites. It was tried and reverted: the maintenance-crank drain it defended against is arithmetically unreachable (`Δengine_available == 0`), and reserving would make fee claims senior to bad-debt coverage — contradicting the junior decision — while reverting the whole `SyncMaintenanceFee` on distressed markets. ⚠ That neutrality argument is about *today's callers*, not a structural invariant.

---

## 4. The stake trust root — entirely absent from the old spec

Tag 87 is permissionless and transfers tokens to a caller-supplied destination. Trust between the two programs runs both ways, and only one direction was built: stake→wrapper was anchored by a hardcoded wrapper allowlist; **wrapper→stake had no anchor at all** and read `*pool_ai.owner`.

A working exploit existed: a market creator deploys their own program, derives matching PDAs, forges a 392-byte pool, and redirects the payout. An interim mitigation (requiring asset 0's `asset_admin` to be burned) **did not work** — `handle_update_asset_authority` has a second branch, self-rotation by the current holder, and `insurance_authority` bootstraps to the creator's own wallet. Demonstrated: with `asset_admin` burned, the pre-fix build let the forgery succeed and drain 250,000 atoms.

**What shipped:** `declare_id!` in `percolator-stake`; the wrapper pins `constants::STAKE_PROGRAM_ID` and asserts `pool_ai.owner == STAKE_PROGRAM_ID` **before reading any bytes**. Everything else derives from the pinned ID. The burn requirement is gone.

Cluster gating mirrors `percolator-stake/src/processor.rs:480-495`: the devnet ID is behind `#[cfg(feature = "devnet")]` so it cannot compile into a mainnet binary. **A default build has no pin and tag 87 fails closed with `StakeProgramNotPinned`** — the atoms stay in `header.insurance`, which is where they are safe. There is no v17 mainnet stake deployment yet.

Layout is now asserted with `offset_of!` in `percolator-stake/src/state.rs`. Previously only the total size (392) was asserted, so any same-size field reorder would have silently redirected tag-87 funds and passed every test in both repos.

Canonical devnet stake program: `GCHhcgwPyrai8SWHEVWw3odedguFXEtJobNnWSfWBCU3` = `percolator-stake@1e08d35`, lineage re-verified by rebuild-and-compare (`0e9c2572…`).

---

## 5. Stake: mode-0 accrual and the dilution fix

`InitPool` creates mode-0 pools; `AccrueFees` previously required mode 1, reachable only via `InitTradingPool`, which no real client calls. Mode-0 pools were a loss-absorbing tranche with **no upside leg at all** — a staker verifiably withdrew $700 on a $1000 deposit.

Shipped: `AccrueFees` accepts modes 0 and 1; `total_pool_value` counts `total_fees_earned` for both. Verified no migration hazard — `total_fees_earned` is provably 0 on every existing mode-0 pool.

**Plus a security fix found in review.** Making mode-0 accrue armed a front-running vector: deposit/withdraw price off the *stored* `total_pool_value()`, `pre_accrue_mode1` was gated to mode 1, and `AccrueFees` is permissionless. An attacker could deposit at the stale price then self-call `AccrueFees` to capture pre-stake fees. Demonstrated pre-fix: attacker minted 100,000 LP versus a fair 66,666, and the genesis LP's claim fell to 123,749 versus an honest 148,500. Fixed by extending the guard to both fee-accruing modes (`pre_accrue_fee_modes`).

---

## 6. Reachability: the CPI proxies

`StakeInitPool` irreversibly rotates `cfg.marketauth` to the stake-pool PDA, and `BindInsuranceAuthority` hands asset 0's `insurance_authority` to `vault_auth`. A PDA cannot sign a top-level transaction, so affected wrapper instructions are reachable **only** via a stake-program CPI proxy. Before this change exactly one existed (`AdminResolveMarket` → tag 19), leaving **1 of 16** marketauth-gated handlers reachable.

**This is the mechanical reason the fee split was unachievable.** Tag 51 `UpdateBackingFeePolicy` is gated on `insurance_authority`; on a staked market nobody *could* set the backing fee. Prior notes recorded this as "nothing calls tag 51" — the truth is stronger.

Shipped, stake tags 25–28:

| Stake tag | Wrapper tag | Signs as |
|---|---|---|
| 25 | 86 `UpdateFeeSplit` | pool PDA |
| 26 | 88 `UpdateMaintenanceFeePerSlot` | pool PDA |
| 27 | 51 `UpdateBackingFeePolicy` | `vault_auth` |
| 28 | 55 `UpdateTradeFeePolicy` | `vault_auth` |

Proven end-to-end: direct call succeeds pre-bind, fails `Custom(8)` post-bind, proxy succeeds, value read back off-chain.

**Tag 69 `RestartAssetOracle` is un-proxyable** — `asset_admin` is only ever burned to `[0;32]`, never rotated to a PDA, and `live_authority_matches` rejects a zero authority for any signer. A proxy would be dead code. Restoring it requires a wrapper change.

---

## 7. New instruction and error surface

Wrapper tags: **86** `UpdateFeeSplit` (marketauth), **87** `WithdrawInsuranceReserveToStake` (permissionless), **88** `UpdateMaintenanceFeePerSlot` (marketauth, payload `u128` — storage and `InitMarket` decode are u128, not u64).
Stake tags: **25–28** (§6).
Wrapper errors: **52** `FeeSplitSumInvalid`, **53** `NoInsuranceReserveToClaim`, **55** `StakePoolOwnerMismatch`, **60** (new), plus distinct codes for the tag-87 pool-content failures. **51** `FeeSplitFloorViolation` pre-existed and is reused.

`WrapperConfigV16` grows 496 → **576 B**. Field order is load-bearing: the four `u128` counters precede the three `u16` shares, with explicit `_padding_split: [u8; 10]`, because `bytemuck::Pod` forbids implicit padding.

`fee_split_floor_ok` and `FEE_SPLIT_SHARE_TOLERANCE_FLAT` are **deprecated but retained** (so their proofs still compile) with no live call sites. They validated a two-rate split that no longer exists and skipped whenever `backing_fee_bps == 0` — i.e. never ran on a live market.

---

## 8. Build and deploy

| Build | Tag 87 | Purpose |
|---|---|---|
| `cargo build-sbf --features devnet` | live, pinned | **devnet deploy** |
| `cargo build-sbf` (default) | fails closed | mainnet shape |

Both must be built **at the canonical path** — `-C metadata` is path-dependent even for crates with no path dependencies, so a rebuild elsewhere yields different bytecode with identical semantics.

Artifacts (clean, reproduced after forced deletion):
- wrapper devnet `606e630fe7b07e095ab1f04e5cc17b582213c377bd884e3ea6ad8cc9e9c0138a` (1,206,592 B)
- wrapper default `55285fe6bde9bb086af4219d076cd4284fe70aa6560ecc3c6743a38741b00fbe` (1,200,976 B)
- stake devnet `1bf05bbb4746d2e145c1595df03484b1dfe148f018cc9e1a1036289ae9dfb6eb` (253,968 B)

⚠ Byte-searching a `.so` for a raw pubkey does **not** work — a known-compiled-in control pubkey is equally absent. Use hash/size deltas and behavioural tests instead.

---

## 9. Required sequencing (nothing enforces this on-chain)

1. `InitMarket` — defaults are already correct; no setter call is required
2. `UpdateFeeSplit` (86) — **only if a non-default split is wanted, and only before `StakeInitPool`**; afterwards it is proxy-only
3. `CreateLpVault` + `DepositToLpVault` — before `StakeInitPool` (marketauth-gated)
4. `StakeInitPool` — rotates `marketauth`
5. `BindInsuranceAuthority` — **required, or the insurance/staker leg has no exit**

---

## 10. Where the previous spec is wrong

Do not build from `2026-07-19-fee-collection-split-design.md`. It states that tag 78 credits vault NAV (no such call exists), omits the stake program pin and the forgery gate entirely, omits the tag 78 mode and no-LP guards, omits the CPI proxies (so it implies the split is settable when it was not), still describes the `asset_admin` burn as part of the design, and presents LP yield as a senior earnings claim rather than junior at-risk capital.

---

## 11. Not done

- **Creator claim UI** — tag 57 still has zero call sites. The one leg that always accrued still cannot be claimed.
- **Protocol fee sweep** — tag 84 has no keeper job.
- **Wizard split control** — nothing writes share bps, so every market gets defaults.
- **Earn / stake product surfaces** — both now genuinely earn; neither UI says so.
- **Seed sequence** — §9 is not encoded anywhere.
- **SDK** — encoders for wrapper 86/87/88 and stake 25–28, error codes 52–60, and the 576-byte config decoder.
- **Product-layer proof** — one ordinary trade on a default market must leave all four legs nonzero, run against **deployed** bytecode.

## 12. Open decisions

- **Timelock** on fee-split retuning — pool admin can now retune LP/insurance shares under already-staked depositors; previously frozen.
- **Tag 69** — leave unreachable, or make the wrapper change.
- **Upgrade authority → Squads** for both programs. Today one on-curve EOA holds upgrade authority over both while `marketauth` is rotated into a PDA of one of them. Acceptable on devnet; must not reach mainnet.
- **`maintenance_fee_per_slot`** — the setter exists, the value is still 0. Enabling it is a separate product decision.
- **`clippy -D warnings`** already fails on clean HEAD (24 pre-existing errors); the stated PR gate is currently unmeetable.
