#!/usr/bin/env bash
# ===========================================================================
#  EchoAgent macOS packaging script (bash)
#
#  Produces a platform-signed distributable .dmg. Updater archives and
#  updater signatures are generated later on the controlled release machine.
#  Public artifacts use EchoAgent-v<VERSION>-darwin-<ARCH>.dmg names.
#  Builds for the host architecture (Apple Silicon or Intel); Tauri picks
#  the right target automatically.
#
#  Usage:
#    bash scripts/build.sh
#    bash scripts/build.sh --version 0.2.0
#    bash scripts/build.sh --allow-unsigned-platform  # development only
#
#  Prerequisites:
#    The complete vendored Runtime source is included in the repository.
#    `scripts/setup.sh` can be used to verify checkout integrity.
#
#  Production builds must pass codesign and Gatekeeper assessment. The escape
#  hatch above exists only for local development and must not be used to make
#  an update release.
# ===========================================================================

set -euo pipefail

# ---- helpers --------------------------------------------------------------
log_step() { printf '\n\033[36m===> %s\033[0m\n' "$1"; }
log_ok()   { printf '  \033[32m[OK]\033[0m   %s\n' "$1"; }
log_err()  { printf '  \033[31m[ERR]\033[0m  %s\n' "$1"; }
log_info() { printf '         %s\n' "$1"; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

# ---------------------------------------------------------------------------
# 1. Parse args
# ---------------------------------------------------------------------------
NEW_VERSION=""
ALLOW_UNSIGNED_PLATFORM=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)
            NEW_VERSION="${2:-}"
            if [[ -z "$NEW_VERSION" ]]; then
                log_err "--version requires a value"
                exit 1
            fi
            shift 2
            ;;
        --allow-unsigned-platform)
            ALLOW_UNSIGNED_PLATFORM=1
            shift
            ;;
        -h|--help)
            sed -n '2,28p' "$0"
            exit 0
            ;;
        *)
            log_err "Unknown argument: $1"
            exit 1
            ;;
    esac
done

# ---------------------------------------------------------------------------
# 2. Platform + toolchain checks
# ---------------------------------------------------------------------------
log_step "Checking platform"
if [[ "$(uname -s)" != "Darwin" ]]; then
    log_err "This script targets macOS. On Windows use scripts/build.ps1 instead."
    exit 1
fi
ARCH="$(uname -m)"
log_ok "macOS detected ($ARCH)"

# A Rosetta shell on Apple Silicon reports x86_64, although the machine needs
# the arm64 release. Fail before staging an Intel Node.js runtime by mistake.
if [[ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" == "1" && "$ARCH" != "arm64" ]]; then
    log_err "This Apple Silicon Mac is running an Intel shell. Use a native arm64 shell and toolchain to build the darwin-aarch64 release."
    exit 1
fi

case "$ARCH" in
    arm64)
        RELEASE_TARGET="darwin-aarch64"
        TAURI_DMG_ARCH="aarch64"
        EXPECTED_NODE_ARCH="arm64"
        EXPECTED_RUST_HOST="aarch64-apple-darwin"
        ;;
    x86_64)
        RELEASE_TARGET="darwin-x86_64"
        TAURI_DMG_ARCH="x64"
        EXPECTED_NODE_ARCH="x64"
        EXPECTED_RUST_HOST="x86_64-apple-darwin"
        ;;
    *)
        log_err "Unsupported macOS architecture: $ARCH"
        exit 1
        ;;
esac

log_step "Checking toolchain"
for cmd in node pnpm cargo rustc; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        log_err "$cmd not found on PATH."
        exit 1
    fi
    log_info "$(printf '%-7s %s' "$cmd" "$("$cmd" --version 2>/dev/null | head -1)")"
done

NODE_ARCH="$(node -p 'process.arch')"
if [[ "$NODE_ARCH" != "$EXPECTED_NODE_ARCH" ]]; then
    log_err "Node.js architecture is $NODE_ARCH; $RELEASE_TARGET requires $EXPECTED_NODE_ARCH."
    exit 1
fi
RUST_HOST="$(rustc -vV | sed -n 's/^host: //p')"
if [[ "$RUST_HOST" != "$EXPECTED_RUST_HOST" ]]; then
    log_err "Rust host is $RUST_HOST; $RELEASE_TARGET requires $EXPECTED_RUST_HOST."
    exit 1
fi
log_ok "Node.js and Rust toolchain match $RELEASE_TARGET"

# echoagent's build.rs invokes protoc; honor $PROTOC or a protoc on PATH.
if [[ -z "${PROTOC:-}" ]] && ! command -v protoc >/dev/null 2>&1; then
    log_err "protoc not found. Install protobuf (brew install protobuf) or set PROTOC=/path/to/protoc."
    exit 1
fi
log_ok "protoc available via ${PROTOC:-PATH}"
log_ok "Core tools present"

# ---------------------------------------------------------------------------
# 3. Version sync (optional)
# ---------------------------------------------------------------------------
if [[ -n "$NEW_VERSION" ]]; then
    log_step "Syncing version -> $NEW_VERSION"
    node "$PROJECT_ROOT/scripts/release-version.mjs" set "$NEW_VERSION" >/dev/null
    log_ok "Bumped and verified all four version sources"
fi
APP_VERSION="$(node "$PROJECT_ROOT/scripts/release-version.mjs" check)"
log_ok "Release version: $APP_VERSION"

# ---------------------------------------------------------------------------
# 4. Vendored Runtime sanity check. Path dependencies in Cargo.toml resolve
#    directly into vendor/echo-agent-build, which is tracked by this repository.
# ---------------------------------------------------------------------------
log_step "Checking vendored Runtime source"
node "$PROJECT_ROOT/scripts/verify-vendored-runtime.mjs"
log_ok "Vendored Runtime source is complete"

log_step "Building and staging Echo Code IDE"
pnpm ide:build
pnpm ide:stage
node scripts/validate-theia-git-runtime.mjs
log_ok "Vendored Theia IDE and Node runtime staged"

# ---------------------------------------------------------------------------
# 5. Build the application, then package it separately. Tauri only exposes
#    bundle_dmg.sh output at verbose level; keep that enabled for packaging
#    without flooding the compilation log with every rustc invocation.
#    The controlled release machine creates and signs updater files later.
# ---------------------------------------------------------------------------
log_step "Building application (pnpm tauri build --no-bundle)"
BUILD_RC=0
pnpm tauri build --no-bundle || BUILD_RC=$?
if [[ $BUILD_RC -eq 0 ]]; then
    log_step "Building .dmg (pnpm tauri bundle --bundles dmg --verbose)"
    node scripts/macos-dmg.mjs bundle \
        "$PROJECT_ROOT/src-tauri/target/release/bundle/macos" \
        "EchoAgent_${APP_VERSION}_${TAURI_DMG_ARCH}.dmg" || BUILD_RC=$?
    if [[ $BUILD_RC -ne 0 ]]; then
        log_err "Application compilation succeeded, but installer packaging failed."
        log_info "To diagnose or retry packaging without recompiling: node scripts/macos-dmg.mjs bundle \"$PROJECT_ROOT/src-tauri/target/release/bundle/macos\" \"EchoAgent_${APP_VERSION}_${TAURI_DMG_ARCH}.dmg\""
    fi
fi

# ---------------------------------------------------------------------------
# 6. Verify the app and give the installer one canonical public name.
# ---------------------------------------------------------------------------
BUNDLE_DIR="$PROJECT_ROOT/src-tauri/target/release/bundle/dmg"
CANONICAL_DMG=""
DMG_MOUNT_POINT=""
DMG_MOUNTED=0
DMG_CLEANUP_ATTEMPTED=0
cleanup_dmg_mount() {
    if [[ -n "$DMG_MOUNT_POINT" ]]; then
        if [[ $DMG_MOUNTED -eq 1 ]]; then
            # An EXIT trap must not restart an exhausted recovery loop.
            if [[ $DMG_CLEANUP_ATTEMPTED -eq 1 ]]; then
                return 1
            fi
            DMG_CLEANUP_ATTEMPTED=1
            if ! node "$PROJECT_ROOT/scripts/macos-dmg.mjs" detach "$DEFAULT_DMG" "$DMG_MOUNT_POINT"; then
                log_err "Could not unmount DMG: $DMG_MOUNT_POINT"
                return 1
            fi
            DMG_MOUNTED=0
        fi
        if ! rmdir "$DMG_MOUNT_POINT"; then
            log_err "Could not remove temporary mount point: $DMG_MOUNT_POINT"
            return 1
        fi
        DMG_MOUNT_POINT=""
    fi
}
trap 'cleanup_dmg_mount || true' EXIT
if [[ $BUILD_RC -eq 0 ]]; then
    DEFAULT_DMG="$BUNDLE_DIR/EchoAgent_${APP_VERSION}_${TAURI_DMG_ARCH}.dmg"
    CANONICAL_DMG="$BUNDLE_DIR/EchoAgent-v${APP_VERSION}-${RELEASE_TARGET}.dmg"

    if [[ ! -f "$DEFAULT_DMG" ]]; then
        log_err "Expected Tauri DMG not found: $DEFAULT_DMG"
        exit 1
    fi
    if ! hdiutil verify "$DEFAULT_DMG" >/dev/null; then
        log_err "DMG checksum verification failed: $DEFAULT_DMG"
        exit 1
    fi

    # Inspect the app that users will actually install. Tauri may remove the
    # intermediate .app after packaging, and an older one may also be present.
    DMG_MOUNT_POINT="$(mktemp -d "${TMPDIR:-/tmp}/echoagent-dmg-check.XXXXXX")"
    if ! hdiutil attach -readonly -nobrowse -quiet -mountpoint "$DMG_MOUNT_POINT" "$DEFAULT_DMG"; then
        log_err "Could not mount the generated DMG for app verification: $DEFAULT_DMG"
        exit 1
    fi
    DMG_MOUNTED=1
    APP_BUNDLE="$DMG_MOUNT_POINT/EchoAgent.app"
    if [[ ! -d "$APP_BUNDLE" ]]; then
        log_err "Expected EchoAgent.app not found inside: $DEFAULT_DMG"
        exit 1
    fi
    node "$PROJECT_ROOT/scripts/prepare-retrieval-models.mjs" --check --directory "$APP_BUNDLE/Contents/Resources/retrieval-models"
    node "$PROJECT_ROOT/scripts/prepare-meeting-speech.mjs" --installed-app "$APP_BUNDLE/Contents/MacOS/echoagent"

    APP_PLIST="$APP_BUNDLE/Contents/Info.plist"
    BUNDLE_VERSION="$(plutil -extract CFBundleShortVersionString raw -o - "$APP_PLIST")"
    BUNDLE_ID="$(plutil -extract CFBundleIdentifier raw -o - "$APP_PLIST")"
    EXECUTABLE_NAME="$(plutil -extract CFBundleExecutable raw -o - "$APP_PLIST")"
    if [[ "$BUNDLE_VERSION" != "$APP_VERSION" ]]; then
        log_err "App version $BUNDLE_VERSION does not match release version $APP_VERSION"
        exit 1
    fi
    if [[ "$BUNDLE_ID" != "com.echoagent.desktop" ]]; then
        log_err "Unexpected bundle identifier: $BUNDLE_ID"
        exit 1
    fi
    if ! lipo -archs "$APP_BUNDLE/Contents/MacOS/$EXECUTABLE_NAME" | tr ' ' '\n' | grep -Fxq "$ARCH"; then
        log_err "App executable does not contain expected architecture: $ARCH"
        exit 1
    fi
    SPEECH_EXECUTABLE="$APP_BUNDLE/Contents/MacOS/echo-meeting-speech"
    if [[ ! -x "$SPEECH_EXECUTABLE" ]] || ! lipo -archs "$SPEECH_EXECUTABLE" | tr ' ' '\n' | grep -Fxq "$ARCH"; then
        log_err "Bundled meeting transcription executable is missing or has the wrong architecture: $ARCH"
        exit 1
    fi
    THEIA_ENTRY="$APP_BUNDLE/Contents/Resources/theia/browser/lib/backend/main.js"
    NODE_EXECUTABLE="$APP_BUNDLE/Contents/Resources/theia/node/bin/node"
    if [[ ! -f "$THEIA_ENTRY" || ! -x "$NODE_EXECUTABLE" ]]; then
        log_err "Echo Code IDE or its Node.js runtime is missing from the app bundle"
        exit 1
    fi
    if ! lipo -archs "$NODE_EXECUTABLE" | tr ' ' '\n' | grep -Fxq "$ARCH"; then
        log_err "Bundled Node.js does not contain expected architecture: $ARCH"
        exit 1
    fi

    PLATFORM_SIGNING_OK=1
    if ! codesign --verify --deep --strict --verbose=2 "$APP_BUNDLE"; then
        PLATFORM_SIGNING_OK=0
    fi
    if ! spctl --assess --type execute --verbose=2 "$APP_BUNDLE"; then
        PLATFORM_SIGNING_OK=0
    fi
    if [[ $PLATFORM_SIGNING_OK -ne 1 ]]; then
        if [[ $ALLOW_UNSIGNED_PLATFORM -eq 1 ]]; then
            log_info "WARNING: macOS signing/Gatekeeper check failed; development override accepted."
        else
            log_err "macOS signing or Gatekeeper assessment failed."
            log_err "Configure Developer ID signing/notarization, or use --allow-unsigned-platform for development only."
            exit 1
        fi
    else
        log_ok "macOS code signature and Gatekeeper assessment passed"
    fi

    cleanup_dmg_mount
    mv -f "$DEFAULT_DMG" "$CANONICAL_DMG"
    log_ok "Normalized release names for $RELEASE_TARGET"
fi

# ---------------------------------------------------------------------------
# 7. Report artifacts.
# ---------------------------------------------------------------------------
if [[ $BUILD_RC -eq 0 && -f "$CANONICAL_DMG" ]]; then
    log_step "Build succeeded. Artifacts:"
    size_mb=$(du -m "$CANONICAL_DMG" | cut -f1)
    log_ok "$(basename "$CANONICAL_DMG")  (${size_mb} MB) [installer]"
    log_info "$CANONICAL_DMG"
    log_info "Run scripts/prepare-update-artifacts.sh on the release machine to create updater files."
else
    log_err "Build failed (exit $BUILD_RC). See output above."
    if [[ $BUILD_RC -eq 0 ]]; then
        BUILD_RC=1
    fi
fi

exit $BUILD_RC
