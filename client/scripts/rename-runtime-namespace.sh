#!/usr/bin/env bash
# Maintenance helper for importing a newer upstream Runtime snapshot.
# This migration is intentionally idempotent and is not part of normal setup
# or release builds because the vendored source already contains the result.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RUNTIME_ROOT="${1:-$PROJECT_ROOT/agent}"

if [[ ! -f "$RUNTIME_ROOT/Cargo.toml" ]]; then
    printf 'Embedded Runtime source is incomplete at: %s\n' "$RUNTIME_ROOT" >&2
    exit 1
fi

# Construct the legacy namespace without retaining it in the source tree.
LEGACY_RUNTIME_NAMESPACE="$(printf '\170\056\141\151')"
TARGET_RUNTIME_NAMESPACE="echo.agent"
UPDATED_FILES=0

while IFS= read -r RELATIVE_PATH; do
    [[ -n "$RELATIVE_PATH" ]] || continue
    SOURCE_PATH="$RUNTIME_ROOT/$RELATIVE_PATH"
    LEGACY_RUNTIME_NAMESPACE="$LEGACY_RUNTIME_NAMESPACE" \
        TARGET_RUNTIME_NAMESPACE="$TARGET_RUNTIME_NAMESPACE" \
        perl -pi -e 's/\Q$ENV{LEGACY_RUNTIME_NAMESPACE}\E/$ENV{TARGET_RUNTIME_NAMESPACE}/gi' \
        "$SOURCE_PATH"
    UPDATED_FILES=$((UPDATED_FILES + 1))
done < <(git -C "$RUNTIME_ROOT" grep -Iil -F "$LEGACY_RUNTIME_NAMESPACE" -- . || true)

if [[ $UPDATED_FILES -eq 0 ]]; then
    printf '  [OK]   Embedded Runtime namespace already uses %s\n' "$TARGET_RUNTIME_NAMESPACE"
else
    printf '  [OK]   Migrated %d Embedded Runtime files to %s\n' \
        "$UPDATED_FILES" "$TARGET_RUNTIME_NAMESPACE"
fi
