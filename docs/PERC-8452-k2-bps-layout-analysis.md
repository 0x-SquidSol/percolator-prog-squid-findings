# PERC-8452: k2_bps Layout Collision Analysis

## Summary

`funding_k2_bps` (u16) is stored in `_insurance_isolation_padding[2..4]`, which
**directly collides** with `oracle_phase` (byte `[2]`) and `cumulative_volume[0]`
(byte `[3]`). The current workaround in `UpdateConfig` (save/restore pattern at
line 12000-12009) preserves oracle_phase and cumulative_volume but **silently
corrupts k2_bps reads** — `get_funding_k2_bps()` returns `u16_le(oracle_phase,
cumul_vol[0])` instead of the actual k2 value.

## Byte Map: `_insurance_isolation_padding[14]`

```
Byte  Field 1                  Field 2                  Field 3
────  ───────────────────────  ───────────────────────  ──────────────────
[0]   mark_oracle_weight_bps   —                        —
[1]   mark_oracle_weight_bps   —                        —
[2]   oracle_phase (u8)        funding_k2_bps[0] ⚠️    —
[3]   cumulative_volume[0]     funding_k2_bps[1] ⚠️    —
[4]   cumulative_volume[1]     ewmv_e12[0]              —
[5]   cumulative_volume[2]     ewmv_e12[1]              —
[6]   cumulative_volume[3]     ewmv_e12[2]              —
[7]   cumulative_volume[4]     ewmv_e12[3]              —
[8]   cumulative_volume[5]     last_vol_price_e3[0]     —
[9]   cumulative_volume[6]     last_vol_price_e3[1]     —
[10]  cumulative_volume[7]     last_vol_price_e3[2]     —
[11]  phase2_delta_slots[0]    last_vol_price_e3[3]     —
[12]  phase2_delta_slots[1]    vol_margin_scale_bps[0]  —
[13]  phase2_delta_slots[2]    vol_margin_scale_bps[1]  —
```

### Collision Groups

| Group | Bytes | Fields | Severity |
|-------|-------|--------|----------|
| **A** | `[2..4]` | oracle_phase + k2_bps + cumul_vol[0] | **HIGH** — k2 corrupts on every read |
| B | `[4..8]` | cumul_vol[1..5] + ewmv_e12 | Mutually exclusive by design? No — both written |
| C | `[8..12]` | cumul_vol[5..8] + last_vol_price + phase2_delta[0] | Mutually exclusive by design? No — both written |
| D | `[11..14]` | phase2_delta + vol_margin_scale_bps | Overlap at [12..14] |

**Wait** — on closer inspection, Groups B/C/D are even worse than the k2
collision. `cumulative_volume` (8 bytes at [3..11]) overlaps with:
- `ewmv_e12` (4 bytes at [4..8])
- `last_vol_price_e3` (4 bytes at [8..12])
- `phase2_delta_slots` (3 bytes at [11..14])

These can't all be live simultaneously. Let me check which features are
actually active on deployed markets...

## Runtime Reality: Which Features Are Live?

Checking the code paths that **write** to each field:

### oracle_phase / cumulative_volume / phase2_delta
- Written by: `AdvanceOraclePhase` (line 16254+), `accumulate_volume` on every trade (line 11117, 11485)
- Read by: `Crank` (line 8923, 8979) for OI cap / leverage scaling
- **ALWAYS ACTIVE** on every market (even if phase=0)

### funding_k2_bps
- Written by: `UpdateConfig` only (line 12006), with save/restore workaround
- Read by: `Crank` → `compute_inventory_funding_bps_per_slot` (line 10751)
- **CORRUPT on read** — returns u16(oracle_phase, cumul_vol[0]) not actual k2
- Impact: Quadratic funding coefficient is garbage. If oracle_phase=2 and cumul_vol
  byte[0] is non-zero, the returned "k2" could be large, causing unintended
  quadratic funding to be applied.

### ewmv_e12 / last_vol_price_e3 / vol_margin_scale_bps (VRAM)
- Written by: `Crank` VRAM block (line 10654+) when `vol_scale > 0`
- Read by: `compute_vram_margin_adjustment` (line 417+)
- **ACTIVE only when vol_margin_scale_bps > 0** — which is stored at [12..14]
  **which is also phase2_delta_slots[1..3]**. So if phase2_delta_slots is set,
  it appears as a non-zero vol_margin_scale_bps, enabling VRAM unintentionally.

### Summary of Live Collision Impact

1. **k2_bps corruption**: `get_funding_k2_bps()` returns garbage (oracle_phase +
   cumul_vol byte). This feeds into funding rate computation on every crank.
   If oracle_phase=2 and cumul_vol low byte is e.g. 0x64, k2 reads as 0x6402 =
   25602 bps = 256% — massive unintended quadratic funding.

2. **VRAM ghost activation**: `phase2_delta_slots` overlaps `vol_margin_scale_bps`.
   Any market that enters Phase 2 and records a delta > 0 will have its low bytes
   interpreted as a VRAM scale, activating volatility margin adjustments.

3. **Volume corruption**: `cumulative_volume` bytes 1-7 overlap with EWMV and
   vol_price. When VRAM writes, it corrupts cumulative_volume, potentially
   preventing Phase 1→2 volume-based transition.

## Fix Strategy

### Option A: Relocate k2_bps to `_adaptive_pad` / `_adaptive_pad2` (5 free bytes)
- `_adaptive_pad` (1 byte at offset between adaptive_funding_enabled and adaptive_scale_bps) — unused
- `_adaptive_pad2` (4 bytes at offset between adaptive_scale_bps and adaptive_max_funding_bps) — unused
- **k2_bps is only 2 bytes** → fits in `_adaptive_pad2[0..2]` with 2 bytes spare

**Migration**: On-chain, after relocation:
1. New `set_funding_k2_bps` writes to `_adaptive_pad2[0..2]` only
2. New `get_funding_k2_bps` reads from `_adaptive_pad2[0..2]`
3. Remove save/restore workaround from UpdateConfig
4. Old markets: k2_bps was always corrupt anyway (reads garbage), so "migrating"
   from old location has no value — just zero-init the new location
5. UpdateConfig automatically writes the correct k2 value to the new location

### Option B: Full layout expansion (CONFIG_LEN increase)
- Add proper named fields for k2_bps, oracle_phase, cumulative_volume
- Requires: version bump, slab reallocation, migration instruction
- **Much higher risk**, changes ENGINE_OFF, affects all slab math

### Recommendation: **Option A**
- Minimal diff, zero CONFIG_LEN change, no slab reallocation needed
- k2_bps gets clean, non-overlapping 2 bytes in `_adaptive_pad2`
- Phase data (oracle_phase, cumul_vol, phase2_delta) keeps existing layout
- VRAM collision (ewmv/vol_price vs cumul_vol, phase2_delta vs vol_scale) is a
  separate issue — file follow-up if any market actually has both features enabled

## Additional Finding: VRAM vs Phase Layout Collision

The VRAM fields (ewmv_e12, last_vol_price_e3, vol_margin_scale_bps) and the
oracle phase fields (cumulative_volume, phase2_delta_slots) occupy overlapping
bytes in `_insurance_isolation_padding[4..14]`. This is **only safe if no market
has both VRAM enabled AND oracle phase tracking active simultaneously**.

Current state: VRAM is activated by setting `vol_margin_scale_bps > 0` via
`TAG_SET_VRAM_PARAMS`. Oracle phase is always active. If VRAM has never been
configured (scale=0), the VRAM writes are skipped (line 10656: `if vol_scale > 0`),
so the collision is dormant.

**Risk**: If admin sets VRAM params on any market, VRAM writes will corrupt
cumulative_volume, and phase2_delta_slots will read as vol_margin_scale_bps.

**Recommendation**: File a follow-up GH issue for the VRAM vs Phase collision.
For PERC-8452, focus on the k2_bps fix only.

## Implementation Plan

1. Add `get_funding_k2_bps_v2` / `set_funding_k2_bps_v2` reading from `_adaptive_pad2[0..2]`
2. Rename old functions to `_legacy` (keep for reference/tests)
3. Update `compute_inventory_funding_bps_per_slot` call site (Crank, line 10751) to use v2
4. Update `UpdateConfig` handler (line 12006) to use v2, remove save/restore workaround
5. Add migration in `UpdateConfig`: if `_adaptive_pad2[0..2] == 0` AND `old get_funding_k2_bps != 0`,
   migrate value. (Note: old value is corrupt, so just use the freshly-passed k2 param.)
6. Write Kani proofs:
   - Proof: set_v2 → get_v2 roundtrip preserves value
   - Proof: set_v2 does NOT touch oracle_phase or cumulative_volume
   - Proof: set_v2 does NOT touch any _insurance_isolation_padding bytes
7. Write regression tests:
   - UpdateConfig with k2_bps no longer needs save/restore
   - oracle_phase and cumulative_volume untouched after set_funding_k2_bps_v2
8. File GH issue for VRAM vs oracle-phase collision (separate task)
