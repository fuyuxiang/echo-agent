#!/usr/bin/env bash
# ===========================================================================
#  EchoAgent source setup check (macOS / Linux)
#
#  The embedded Runtime is committed directly to this repository. This script
#  verifies that a checkout contains the maintained Agent source.
#  Idempotent: safe to re-run.
#
#  Usage:
#    bash scripts/setup.sh
# ===========================================================================

set -euo pipefail

log_step() { printf '\n\033[36m===> %s\033[0m\n' "$1"; }
log_ok()   { printf '  \033[32m[OK]\033[0m   %s\n' "$1"; }
log_info() { printf '         %s\n' "$1"; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

log_step "Verifying Agent source"
node "$PROJECT_ROOT/scripts/verify-agent-source.mjs"
log_ok "No submodule initialization or upstream checkout is required"

log_step "Setup complete"
log_info "Next:"
log_info "  pnpm install"
log_info "  pnpm dev            # or: bash scripts/build.sh"
