#!/usr/bin/env bash
# rebuild-deploy-perc328.sh
# Rebuild and redeploy all 3 devnet tiers from latest main (post-PERC-328 fix).
# Run from the percolator-prog repo root.
#
# Prerequisites:
#   - solana CLI in PATH (or set SOLANA_BIN below)
#   - Deployer wallet keypair (--keypair or DEPLOYER_KEYPAIR env)
#   - Sufficient SOL: Small ~0.68, Medium ~2.7, Large ~7.14 SOL
#   - Large tier currently blocked — deployer wallet needs top-up to 7.14+ SOL
#
# Usage:
#   DEPLOYER_KEYPAIR=~/.config/solana/id.json ./scripts/rebuild-deploy-perc328.sh [small|medium|large|all]
#   ./scripts/rebuild-deploy-perc328.sh all           # deploy all 3 (requires 7.14 SOL for Large)
#   ./scripts/rebuild-deploy-perc328.sh small medium  # deploy only Small + Medium

set -euo pipefail

# ── Config ─────────────────────────────────────────────────────────────────────
SOLANA_BIN="${SOLANA_BIN:-$HOME/.local/share/solana/install/active_release/bin}"
PATH="$SOLANA_BIN:$PATH"
RPC_URL="${RPC_URL:-https://api.devnet.solana.com}"
DEPLOYER_KEYPAIR="${DEPLOYER_KEYPAIR:-$HOME/.config/solana/id.json}"

# Program IDs from devnet (issue #537 — PERC-328 deployment)
SMALL_PROGRAM_ID="FwfBKZXbYr4vTK23bMFkbgKq3npJ3MSDxEaKmq9Aj4Qn"
MEDIUM_PROGRAM_ID="g9msRSV3sJmmE3r5Twn9HuBsxzuuRGTjKCVTKudm9in"
LARGE_PROGRAM_ID="FxfD37s1AZTeWfFQps9Zpebi2dNQ9QSSDtfMKdbsfKrD"

SO_PATH="target/deploy/percolator_prog.so"

# ── Helpers ─────────────────────────────────────────────────────────────────────
log() { echo "[$(date -u +%H:%M:%SZ)] $*"; }
die() { echo "❌ ERROR: $*" >&2; exit 1; }

check_prereqs() {
  command -v solana >/dev/null 2>&1 || die "solana CLI not found. Set SOLANA_BIN or add to PATH."
  command -v cargo-build-sbf >/dev/null 2>&1 || die "cargo-build-sbf not found."
  [[ -f "$DEPLOYER_KEYPAIR" ]] || die "Deployer keypair not found: $DEPLOYER_KEYPAIR. Set DEPLOYER_KEYPAIR env."
}

check_sol_balance() {
  local min_sol="$1"
  local balance
  balance=$(solana balance "$DEPLOYER_KEYPAIR" --url "$RPC_URL" 2>/dev/null | awk '{print $1}')
  log "Deployer wallet balance: ${balance} SOL (minimum required: ${min_sol} SOL)"
  if (( $(echo "$balance < $min_sol" | bc -l) )); then
    die "Insufficient SOL. Have ${balance}, need ${min_sol}. Top up the deployer wallet first."
  fi
}

build_tier() {
  local tier="$1"    # small | medium | large
  local features="$2"
  log "Building ${tier} tier (features: ${features:-none})..."
  if [[ -n "$features" ]]; then
    cargo build-sbf --features "$features" 2>&1
  else
    cargo build-sbf 2>&1
  fi
  [[ -f "$SO_PATH" ]] || die "Build output not found: $SO_PATH"
  log "✅ Build complete: $SO_PATH ($(du -sh "$SO_PATH" | cut -f1))"
}

deploy_tier() {
  local tier="$1"
  local program_id="$2"
  log "Deploying ${tier} to devnet (program ID: ${program_id})..."
  solana program deploy \
    "$SO_PATH" \
    --program-id "$program_id" \
    --keypair "$DEPLOYER_KEYPAIR" \
    --url "$RPC_URL" \
    2>&1
  log "✅ ${tier} deployed successfully."
}

verify_tier() {
  local tier="$1"
  local program_id="$2"
  log "Verifying ${tier} program on devnet..."
  solana program show "$program_id" --url "$RPC_URL" 2>&1
}

# ── Main ────────────────────────────────────────────────────────────────────────
TIERS=("${@:-all}")
if [[ "${TIERS[*]}" == "all" ]]; then
  TIERS=(small medium large)
fi

log "=== PERC-328 Rebuild + Redeploy ==="
log "Repo: $(git remote get-url origin 2>/dev/null || echo unknown)"
log "Commit: $(git log --oneline -1 2>/dev/null || echo unknown)"
log "RPC: $RPC_URL"
log "Deployer: $DEPLOYER_KEYPAIR"
log "Tiers: ${TIERS[*]}"
echo

check_prereqs

# Pull latest main first
log "Pulling latest main..."
git fetch origin main
git checkout main
git pull origin main
log "✅ On main at: $(git log --oneline -1)"
echo

for tier in "${TIERS[@]}"; do
  case "$tier" in
    small)
      check_sol_balance "0.70"
      build_tier "small" "devnet,small"
      deploy_tier "Small" "$SMALL_PROGRAM_ID"
      verify_tier "Small" "$SMALL_PROGRAM_ID"
      ;;
    medium)
      check_sol_balance "2.80"
      build_tier "medium" "devnet,medium"
      deploy_tier "Medium" "$MEDIUM_PROGRAM_ID"
      verify_tier "Medium" "$MEDIUM_PROGRAM_ID"
      ;;
    large)
      log "⚠️  Large tier requires 7.14+ SOL. Checking balance..."
      check_sol_balance "7.14"
      build_tier "large" "devnet"
      deploy_tier "Large" "$LARGE_PROGRAM_ID"
      verify_tier "Large" "$LARGE_PROGRAM_ID"
      ;;
    *)
      die "Unknown tier: $tier. Valid: small medium large all"
      ;;
  esac
  echo
done

log "=== All requested tiers deployed ==="
log "Next step: run devnet smoke tests (QA agent / percolator-prog tests)"
log "  cargo test-sbf --features devnet,small  # smoke Small"
log "  cargo test-sbf --features devnet,medium # smoke Medium"
log "  cargo test-sbf --features devnet        # smoke Large"
