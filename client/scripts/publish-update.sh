#!/usr/bin/env bash
# Internal helper: upload one preflighted updater artifact and atomically
# publish its latest.json. Use publish-all-updates.sh for release operations.
set -euo pipefail

if [[ "${ECHOAGENT_BATCH_PUBLISH:-}" != "1" ]]; then
  echo "This helper cannot publish directly. Use scripts/publish-all-updates.sh." >&2
  exit 2
fi

HOST="root@123.56.188.16"
VERSION=""
TARGET=""
ARTIFACT=""
NOTES_FILE=""
MANDATORY=0

usage() {
  echo "Usage: $0 --version 0.3.11 --target windows-x86_64|darwin-aarch64|darwin-x86_64 --artifact FILE [--notes-file FILE] [--mandatory] [--host user@host]"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="${2:-}"; shift 2 ;;
    --target) TARGET="${2:-}"; shift 2 ;;
    --artifact) ARTIFACT="${2:-}"; shift 2 ;;
    --notes-file) NOTES_FILE="${2:-}"; shift 2 ;;
    --mandatory) MANDATORY=1; shift ;;
    --host) HOST="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ -z "$VERSION" || -z "$TARGET" || -z "$ARTIFACT" ]]; then
  usage >&2
  exit 2
fi
VERSION="${VERSION#v}"
if ! node "$(dirname "$0")/release-version.mjs" validate "$VERSION" >/dev/null; then
  exit 2
fi
if [[ ! "$TARGET" =~ ^(windows-x86_64|darwin-x86_64|darwin-aarch64)$ ]]; then
  echo "Unsupported updater target: $TARGET" >&2
  exit 2
fi
if [[ ! -f "$ARTIFACT" || ! -f "$ARTIFACT.sig" ]]; then
  echo "Artifact and generated signature are required: $ARTIFACT and $ARTIFACT.sig" >&2
  exit 2
fi
if [[ -n "$NOTES_FILE" && ! -f "$NOTES_FILE" ]]; then
  echo "Notes file not found: $NOTES_FILE" >&2
  exit 2
fi

case "$TARGET" in
  windows-x86_64) EXPECTED_NAME="EchoAgent-v$VERSION-$TARGET-setup.exe" ;;
  darwin-x86_64|darwin-aarch64) EXPECTED_NAME="EchoAgent-v$VERSION-$TARGET.app.tar.gz" ;;
esac
if [[ "$(basename "$ARTIFACT")" != "$EXPECTED_NAME" ]]; then
  echo "Artifact name does not match version/target; expected: $EXPECTED_NAME" >&2
  exit 2
fi
node "$(dirname "$0")/verify-updater-signature.mjs" "$ARTIFACT" >/dev/null

REMOTE_TMP="$(ssh "$HOST" 'mktemp -d /tmp/echoagent-update.XXXXXX')"
if [[ ! "$REMOTE_TMP" =~ ^/tmp/echoagent-update\.[A-Za-z0-9]+$ ]]; then
  echo "Unexpected remote staging path: $REMOTE_TMP" >&2
  exit 1
fi
# REMOTE_TMP is accepted only after the strict /tmp/echoagent-update.* check.
# shellcheck disable=SC2029
cleanup() { ssh "$HOST" "rm -rf '$REMOTE_TMP'" >/dev/null 2>&1 || true; }
trap cleanup EXIT

scp "$ARTIFACT" "$ARTIFACT.sig" "$HOST:$REMOTE_TMP/"
REMOTE_ARTIFACT="$REMOTE_TMP/$(basename "$ARTIFACT")"
REMOTE_SIGNATURE="$REMOTE_TMP/$(basename "$ARTIFACT").sig"
REMOTE_NOTES=""
if [[ -n "$NOTES_FILE" ]]; then
  scp "$NOTES_FILE" "$HOST:$REMOTE_TMP/release-notes.txt"
  REMOTE_NOTES="--notes-file '$REMOTE_TMP/release-notes.txt'"
fi
REMOTE_MANDATORY=""
if [[ $MANDATORY -eq 1 ]]; then REMOTE_MANDATORY="--mandatory"; fi

# Version, target and artifact name are all allowlisted above.
# shellcheck disable=SC2029
ssh "$HOST" \
  "/usr/local/sbin/echoagent-publish-update --version '$VERSION' --target '$TARGET' --artifact '$REMOTE_ARTIFACT' --signature '$REMOTE_SIGNATURE' $REMOTE_NOTES $REMOTE_MANDATORY"

echo "Published manifest: https://123.56.188.16:8787/desktop-updates/stable/$TARGET.json"
