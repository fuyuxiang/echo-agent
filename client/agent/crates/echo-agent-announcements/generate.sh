#!/usr/bin/env bash
#
# Regenerate TypeScript announcement ACP types from this crate's ts-rs derives.
#
# The Rust structs (RemoteAnnouncement, AnnouncementCta, AnnouncementsRefreshed)
# are the single source of truth for the `echo.agent/announcements/update` payload.
# Pipeline:
#   1. cargo test --features ts → ts-rs writes every binding to a temp dir
#      (TS_RS_EXPORT_DIR; the `ts` feature stays off for normal/Bazel builds)
#   2. copy each binding + a "do not edit" header into the desktop consumer's
#      generated types directory (committed, so the TS app never needs cargo)
#   3. format the destination so committed bytes stay house-formatted
#
# Usage: ./generate.sh  (run from this package directory)
set -euo pipefail
shopt -s nullglob

CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Desktop consumer lives three levels up from this crate in the workspace tree.
REPO_ROOT="$(cd "$CRATE_DIR/../../.." && pwd)"
DESKTOP_DIR="$REPO_ROOT/frontend/apps/echo-agent-desktop"
DST="$DESKTOP_DIR/src/acp/generated"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "[generate] 1/3 exporting ts-rs bindings (cargo test --features ts) …"
(cd "$CRATE_DIR" && TS_RS_EXPORT_DIR="$TMP" cargo test --quiet --features ts)

# Never clobber the committed bindings unless the export actually produced some.
bindings=("$TMP"/*.ts)
[[ ${#bindings[@]} -gt 0 ]] || {
  echo "[generate] no bindings exported — aborting before touching $DST" >&2
  exit 1
}

echo "[generate] 2/3 copying ${#bindings[@]} bindings -> $DST"
mkdir -p "$DST"
rm -f "$DST"/*.ts
for f in "${bindings[@]}"; do
  {
    echo "// generated — do NOT edit by hand."
    echo "// Regenerate via generate.sh in the echo-agent-announcements package."
    cat "$f"
  } >"$DST/$(basename "$f")"
done

echo "[generate] 3/3 formatting (oxfmt) …"
(cd "$DESKTOP_DIR" && pnpm exec oxfmt --write src/acp/generated)
echo "[generate] done — ${#bindings[@]} bindings."
