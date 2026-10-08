#!/usr/bin/env bash
# Validate artifacts.json and publish all supported desktop updater targets.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
HOST="root@123.56.188.16"
ARTIFACTS_DIR=""
NOTES_FILE=""
MANDATORY=0
DRY_RUN=0

usage() {
  cat <<'EOF'
Usage:
  bash scripts/publish-all-updates.sh --artifacts-dir release/v0.3.11 \
    [--notes-file FILE] [--mandatory] [--host user@host] [--dry-run]

The directory must have been produced by prepare-update-artifacts.sh. All
updater and platform signatures must pass preflight before anything uploads.
The three updater manifests become visible together after the remote copy.
EOF
}

die() {
  printf '错误：%s\n' "$1" >&2
  exit "${2:-1}"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --artifacts-dir) ARTIFACTS_DIR="${2:-}"; shift 2 ;;
    --notes-file) NOTES_FILE="${2:-}"; shift 2 ;;
    --mandatory) MANDATORY=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --host) HOST="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "未知参数：$1" 2 ;;
  esac
done

[[ -n "$ARTIFACTS_DIR" ]] || { usage >&2; die "--artifacts-dir 是必需参数" 2; }
if [[ "$ARTIFACTS_DIR" != /* ]]; then
  ARTIFACTS_DIR="$PROJECT_ROOT/$ARTIFACTS_DIR"
fi
[[ -d "$ARTIFACTS_DIR" ]] || die "产物目录不存在：$ARTIFACTS_DIR"
MANIFEST="$ARTIFACTS_DIR/artifacts.json"
[[ -f "$MANIFEST" ]] || die "缺少 artifacts.json：$MANIFEST"
[[ -f "$ARTIFACTS_DIR/SHA256SUMS" ]] || die "缺少 SHA256SUMS"
[[ -z "$NOTES_FILE" || -f "$NOTES_FILE" ]] || die "发布说明文件不存在：$NOTES_FILE"

read_manifest() {
  node -e '
    const fs = require("fs");
    const manifest = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const keys = process.argv[2].split(".");
    let value = manifest;
    for (const key of keys) value = value?.[key];
    if (value === undefined || value === null) process.exit(3);
    process.stdout.write(typeof value === "string" ? value : JSON.stringify(value));
  ' "$MANIFEST" "$1"
}

[[ "$(read_manifest schemaVersion)" == "1" ]] || die "不支持的 artifacts.json schemaVersion"
[[ "$(read_manifest signed)" == "true" ]] || die "updater 产物尚未签名，禁止发布"
[[ "$(read_manifest releaseEligible)" == "true" ]] \
  || die "产物使用了开发放行参数或没有通过全部生产校验，禁止发布"
VERSION="$(read_manifest version)"
node "$SCRIPT_DIR/release-version.mjs" validate "$VERSION" >/dev/null
EXPECTED_KEY_ID="$(read_manifest updaterKeyId)"
[[ "$EXPECTED_KEY_ID" =~ ^[0-9A-F]{16}$ ]] || die "artifacts.json 的 updaterKeyId 无效"

TARGETS=(windows-x86_64 darwin-aarch64 darwin-x86_64)
for target in "${TARGETS[@]}"; do
  signing_state="$(read_manifest "platformSigning.$target")"
  [[ "$signing_state" == "valid" ]] \
    || die "$target 的平台签名状态为 $signing_state，禁止发布"

  artifact_name="$(read_manifest "updaterArtifacts.$target")"
  artifact="$ARTIFACTS_DIR/$artifact_name"
  case "$target" in
    windows-x86_64) expected="EchoAgent-v$VERSION-$target-setup.exe" ;;
    darwin-aarch64|darwin-x86_64) expected="EchoAgent-v$VERSION-$target.app.tar.gz" ;;
  esac
  [[ "$artifact_name" == "$expected" ]] \
    || die "$target 产物名错误：期望 $expected，实际 $artifact_name"
  [[ -f "$artifact" && -f "$artifact.sig" ]] \
    || die "$target 缺少更新文件或 .sig"
  actual_key_id="$(node "$SCRIPT_DIR/verify-updater-signature.mjs" "$artifact")"
  [[ "$actual_key_id" == "$EXPECTED_KEY_ID" ]] \
    || die "$target 的签名密钥 ID 与 artifacts.json 不一致"
done

(
  cd "$ARTIFACTS_DIR"
  shasum -a 256 -c SHA256SUMS >/dev/null
)
printf '预检通过：EchoAgent %s，三个平台的命名、哈希和签名元数据均有效。\n' "$VERSION"
if [[ $DRY_RUN -eq 1 ]]; then
  printf 'dry-run 完成，未上传任何文件。\n'
  exit 0
fi

REMOTE_TMP="$(ssh "$HOST" 'mktemp -d /tmp/echoagent-update.XXXXXX')"
if [[ ! "$REMOTE_TMP" =~ ^/tmp/echoagent-update\.[A-Za-z0-9]+$ ]]; then
  die "Unexpected remote staging path: $REMOTE_TMP"
fi
# REMOTE_TMP is accepted only after the strict path check above.
# shellcheck disable=SC2029
cleanup() { ssh "$HOST" "rm -rf '$REMOTE_TMP'" >/dev/null 2>&1 || true; }
trap cleanup EXIT

files=()
for target in "${TARGETS[@]}"; do
  artifact_name="$(read_manifest "updaterArtifacts.$target")"
  files+=("$ARTIFACTS_DIR/$artifact_name" "$ARTIFACTS_DIR/$artifact_name.sig")
done
scp "${files[@]}" "$HOST:$REMOTE_TMP/"

REMOTE_NOTES=""
if [[ -n "$NOTES_FILE" ]]; then
  scp "$NOTES_FILE" "$HOST:$REMOTE_TMP/release-notes.txt"
  REMOTE_NOTES="--notes-file '$REMOTE_TMP/release-notes.txt'"
fi
REMOTE_MANDATORY=""
if [[ $MANDATORY -eq 1 ]]; then REMOTE_MANDATORY="--mandatory"; fi

# VERSION is validated SemVer; REMOTE_TMP is restricted to the pattern above.
# shellcheck disable=SC2029
ssh "$HOST" \
  "/usr/local/sbin/echoagent-publish-update --version '$VERSION' --batch-dir '$REMOTE_TMP' $REMOTE_NOTES $REMOTE_MANDATORY"

printf 'EchoAgent %s 的三个平台更新已全部发布。\n' "$VERSION"
