#!/bin/bash
# Echo Agent installer for Linux, macOS, and WSL2.

# -E propagates the ERR trap into functions. Without it every failure inside a
# function (i.e. all real work — main() only calls functions) exits silently
# with a bare status and the on_error diagnostics below never print.
set -eE

# --- Environment sanitization ---
# UV_NO_CONFIG ignores uv config *files* only (uv.toml/pyproject), not env vars
# or CLI flags. We select the index ourselves via probe_pypi_index + an explicit
# --default-index flag, so a stray uv.toml can't override or slow the install;
# users who want a specific source use ECHO_PYPI_INDEX / UV_DEFAULT_INDEX.
unset PYTHONPATH PYTHONHOME
export UV_NO_CONFIG=1

# --- Colors ---
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
MAGENTA='\033[0;35m'
NC='\033[0m'
BOLD='\033[1m'

# --- Constants ---
# Code hosts carrying this repo. The two are kept in sync (origin pushes to
# both), so either is a complete source. Cloning from github.com is the single
# most likely step to fail for users in mainland China, and a failed clone is
# fatal to the whole install — so the host is chosen by measured reachability
# rather than hardcoded. Order within the array is only the tie-break used when
# probing is disabled or nothing responds.
REPO_HOSTS=(
    "github|git@github.com:fuyuxiang/echo-agent.git|https://github.com/fuyuxiang/echo-agent.git"
    "gitee|git@gitee.com:fuyuxiang/echo-agent.git|https://gitee.com/fuyuxiang/echo-agent.git"
)
# Force one host and skip the code-host probe: --repo github|gitee, or
# ECHO_REPO_HOST. Scope is the git clone/fetch only — the embedding and rerank
# model packages keep their own fixed source order (Gitee release first, then
# GitHub), because that order is about the 100MiB-per-asset volume split rather
# than about which host is reachable for git.
REPO_HOST="${ECHO_REPO_HOST:-}"
# Filled in by select_repo_host(). REPO_LABEL names the chosen host and drives
# the clone order in clone_repo (which reads the URLs out of REPO_HOSTS itself);
# REPO_URL_HTTPS is the fetch fallback used by update_repo.
REPO_LABEL=""
REPO_URL_HTTPS=""
# The runtime resolves its home as Path.home()/".echo-agent" with no env-var
# override (echo_agent/runtime_paths.py), and that same literal is hardcoded
# independently in path_policy, plugins/loader, the channel data dirs and the
# config schema defaults. An installer-side override could therefore only ever
# move *part* of the tree: setup would still write ~/.echo-agent/echo-agent.yaml
# while the service ran with -w <other>, leaving the foreground CLI and the
# service on two different databases. So the home is fixed here on purpose —
# use `echo-agent setup -w /path` for a project-local workspace instead.
ECHO_HOME="$HOME/.echo-agent"
ECHO_COMMAND_LINK_DIR="${ECHO_COMMAND_LINK_DIR:-}"
# Derived from ECHO_HOME but overridable; resolved after argument parsing so
# --dir is honored (see resolve_paths).
INSTALL_DIR="${ECHO_INSTALL_DIR:-}"
PYTHON_VERSION="3.11"
NODE_VERSION="22"
# pnpm major installed when corepack is unavailable. web/package.json's
# "packageManager" pin is the primary mechanism and corepack honours it; this is
# only the floor for the `npm i -g` fallback, which would otherwise grab the
# newest major (pnpm 11 needs Node >=22.13 while node_version_ok() accepts 20,
# so "newest" can mean "installs fine, crashes on every call"). Keep in sync
# with the pin and the CI matrix in .github/workflows/ci.yml.
PNPM_FALLBACK_VERSION="10"
BRANCH="master"
RUN_SETUP=true
# Probing: measure real latency to each candidate download source and use the
# fastest one, instead of guessing by GeoIP/locale. --no-mirror-probe turns off
# ALL THREE probes (PyPI index, code host, Node.js dist mirror) — the name predates
# the code-host and Node probes; it is kept for compatibility with existing docs
# and scripts, and the --help text spells out the full scope.
MIRROR_PROBE=true
DEPS_TIMEOUT="${ECHO_DEPS_TIMEOUT:-600}"
# Candidate mirrors raced by probe_pypi_index (label|url). The official PyPI is
# always kept as the extra fallback index so a mirror missing a package still
# resolves. Users can skip probing entirely by exporting UV_DEFAULT_INDEX /
# UV_INDEX_URL or ECHO_PYPI_INDEX before running.
PYPI_OFFICIAL="https://pypi.org/simple"
PYPI_MIRRORS=(
    "tsinghua|https://pypi.tuna.tsinghua.edu.cn/simple"
    "aliyun|https://mirrors.aliyun.com/pypi/simple"
    "ustc|https://mirrors.ustc.edu.cn/pypi/simple"
    "official|https://pypi.org/simple"
)
# Resolved by probe_pypi_index(); empty means "use uv defaults / user config".
PYPI_INDEX=""
# Node.js download hosts, raced the same way as the PyPI mirrors. The mirrors
# serve byte-identical tarballs (their SHASUMS256.txt matches nodejs.org), and
# downloads are verified against the official checksums regardless of source.
NODE_DIST_OFFICIAL="https://nodejs.org/dist"
NODE_DIST_MIRRORS=(
    "https://cdn.npmmirror.com/binaries/node"
    "https://mirrors.aliyun.com/nodejs-release"
    "https://nodejs.org/dist"
)
HAS_NODE=false
DASHBOARD_BUILT=false
# Skip the Dashboard build entirely (--skip-dashboard). The gateway then serves
# its built-in playground UI instead of the SPA.
SKIP_DASHBOARD=false
# Platform facts filled in by detect_os / detect_linux_flavor.
DISTRO="unknown"
IS_MUSL=false
IS_WSL=false
# Set when the background service could not be registered, so the closing
# summary can tell the user how to run the gateway instead.
SERVICE_SKIPPED=false

# --- Interactive detection ---
if [ -t 0 ]; then
    IS_INTERACTIVE=true
else
    IS_INTERACTIVE=false
fi

# --- Error trap ---
on_error() {
    echo ""
    log_error "Installation failed at line $1"
    log_info "You can re-run this script to resume (completed steps will be skipped)."
    log_info "For help: https://github.com/fuyuxiang/echo-agent/issues"
}
trap 'on_error $LINENO' ERR

# --- CLI argument parsing ---
# require_value guards `shift 2` on flags that take an argument: without it a
# trailing `--branch` makes shift fail and abort with a raw bash error.
require_value() {
    if [ -z "${2:-}" ]; then
        echo "Option $1 requires a value" >&2
        exit 1
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-setup) RUN_SETUP=false; shift ;;
        --branch) require_value "$1" "${2:-}"; BRANCH="$2"; shift 2 ;;
        --dir) require_value "$1" "${2:-}"; INSTALL_DIR="$2"; shift 2 ;;
        --no-mirror-probe) MIRROR_PROBE=false; shift ;;
        --repo)
            require_value "$1" "${2:-}"
            case "$2" in
                github|gitee) REPO_HOST="$2" ;;
                *) echo "Option --repo accepts 'github' or 'gitee' (got: $2)" >&2; exit 1 ;;
            esac
            shift 2
            ;;
        --skip-dashboard) SKIP_DASHBOARD=true; shift ;;
        -h|--help)
            echo "Echo Agent Installer"
            echo ""
            echo "Usage: install.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --skip-setup       Skip interactive setup wizard"
            echo "  --branch NAME      Git branch to install (default: master)"
            echo "  --dir PATH         Installation directory (default: ~/.echo-agent/echo-agent)"
            echo "  --no-mirror-probe  Disable ALL download-source speed probes:"
            echo "                     PyPI index, code host (github/gitee) and the"
            echo "                     Node.js dist mirror. Each then uses its first"
            echo "                     configured default instead of the fastest one"
            echo "  --repo HOST        Clone from 'github' or 'gitee' instead of"
            echo "                     picking whichever responds faster. Affects the"
            echo "                     git clone/fetch only — the model prefetch always"
            echo "                     tries the Gitee release first, then GitHub"
            echo "  --skip-dashboard   Don't build the web Dashboard (skips Node.js/pnpm);"
            echo "                     the gateway serves its built-in playground UI instead"
            echo "  -h, --help         Show this help"
            echo ""
            echo "Environment:"
            echo "  ECHO_PYPI_INDEX    Force a specific PyPI index URL (skips probing)"
            echo "  ECHO_DEPS_TIMEOUT  Dependency install timeout in seconds (default: 600)"
            echo "  ECHO_INSTALL_DIR   Same as --dir"
            echo "  ECHO_REPO_HOST     Same as --repo (github|gitee)"
            echo "  ECHO_COMMAND_LINK_DIR"
            echo "                     Directory to symlink the echo-agent command into"
            echo "                     (default: the first writable dir already on PATH)"
            echo "  UV_DEFAULT_INDEX / UV_INDEX_URL"
            echo "                     Respected as-is; also skips the PyPI mirror probe"
            echo ""
            echo "Model prefetch (best-effort; a failure never aborts the install):"
            echo "  ECHO_EMBED_MODEL   Embedding model to prefetch (default:"
            echo "                     BAAI/bge-small-zh-v1.5). Empty disables it."
            echo "                     Only the default model has a release package;"
            echo "                     any other value downloads from HuggingFace/GCS"
            echo "  ECHO_RERANK_MODEL  Rerank model to prefetch (default:"
            echo "                     BAAI/bge-reranker-base), same rules as above"
            echo "  ECHO_SKIP_RERANK_PREFETCH=1"
            echo "                     Skip the ~941MB rerank model prefetch; the"
            echo "                     runtime downloads it on first use instead"
            echo "  ECHO_EMBED_PREFETCH_TIMEOUT / ECHO_RERANK_PREFETCH_TIMEOUT"
            echo "                     Per-model prefetch timeout in seconds"
            echo "                     (default: 900 / 1800)"
            echo ""
            echo "Notes:"
            echo "  Config and data always live in ~/.echo-agent — the runtime hardcodes"
            echo "  that location. For a project-local workspace, install normally and"
            echo "  then run: echo-agent setup -w /path/to/workspace"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

# Resolve paths that derive from the options above. Doing this after parsing is
# what makes --dir actually take effect.
resolve_paths() {
    if [ -z "$INSTALL_DIR" ]; then
        INSTALL_DIR="$ECHO_HOME/echo-agent"
    fi
    # The prefetch cache must match the runtime's configured location
    # (memory.local_embedding_cache_dir defaults to ~/.echo-agent/models/fastembed
    # in echo_agent/config/schema.py); a mismatch silently wastes the download
    # and makes the runtime fetch the model again on first use.
    EMBED_CACHE_DIR="$ECHO_HOME/models/fastembed"
}
resolve_paths

# =============================================================================
# Logging helpers
# =============================================================================

log_info() {
    echo -e "${CYAN}→${NC} $1"
}

log_success() {
    echo -e "${GREEN}✓${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}!${NC} $1"
}

log_error() {
    echo -e "${RED}x${NC} $1"
}

prompt_yes_no() {
    local question="$1"
    local default="${2:-yes}"
    local prompt_suffix
    local answer=""

    case "$default" in
        [yY]|[yY][eE][sS]|[tT][rR][uU][eE]|1) prompt_suffix="[Y/n]" ;;
        *) prompt_suffix="[y/N]" ;;
    esac

    if [ "$IS_INTERACTIVE" = true ]; then
        read -r -p "$question $prompt_suffix " answer || answer=""
    elif [ -r /dev/tty ] && [ -w /dev/tty ]; then
        printf "%s %s " "$question" "$prompt_suffix" > /dev/tty
        IFS= read -r answer < /dev/tty || answer=""
    fi

    answer="${answer#"${answer%%[![:space:]]*}"}"
    answer="${answer%"${answer##*[![:space:]]}"}"
    if [ -z "$answer" ]; then
        case "$default" in
            [yY]|[yY][eE][sS]|[tT][rR][uU][eE]|1) return 0 ;;
            *) return 1 ;;
        esac
    fi

    case "$answer" in
        [yY]|[yY][eE][sS]) return 0 ;;
        *) return 1 ;;
    esac
}

# =============================================================================
# Utility functions
# =============================================================================

run_with_timeout() {
    local timeout_sec="$1"; shift

    # Normalize to a bare integer so the pure-shell loop below can count.
    case "$timeout_sec" in
        ''|*[!0-9]*) timeout_sec=600 ;;
    esac

    local timeout_bin=""
    if command -v timeout >/dev/null 2>&1; then
        timeout_bin="timeout"
    elif command -v gtimeout >/dev/null 2>&1; then
        timeout_bin="gtimeout"
    fi

    if [ -n "$timeout_bin" ]; then
        # --foreground keeps the child in the shell's foreground process group
        # so Ctrl+C reaches it; -k 10 escalates to SIGKILL 10s past the
        # deadline. Both are GNU-only, so probe once and fall back to plain
        # timeout on BusyBox (Alpine).
        if "$timeout_bin" --foreground -k 10 1 true >/dev/null 2>&1; then
            "$timeout_bin" --foreground -k 10 "$timeout_sec" "$@"
        else
            "$timeout_bin" "$timeout_sec" "$@"
        fi
        return $?
    fi

    # Stock macOS ships neither binary. Previously this branch ran the command
    # with no bound at all, so every timeout in this script (clone 120s, deps
    # 600s, prefetch 900s) silently became infinite and a stalled download could
    # only be escaped with Ctrl+C. Run it in its own process group instead and
    # poll, so we can kill the whole subtree on expiry.
    set -m
    ( "$@" ) &
    local cmd_pid=$!
    set +m

    local waited=0
    local rc=0
    while [ "$waited" -lt "$timeout_sec" ]; do
        if ! kill -0 "$cmd_pid" 2>/dev/null; then
            rc=0; wait "$cmd_pid" 2>/dev/null || rc=$?
            return "$rc"
        fi
        sleep 1
        waited=$((waited + 1))
    done

    # The command may have exited during the final poll interval — don't kill
    # (and mislabel as timed out) something that already finished cleanly.
    if ! kill -0 "$cmd_pid" 2>/dev/null; then
        rc=0; wait "$cmd_pid" 2>/dev/null || rc=$?
        return "$rc"
    fi

    kill -TERM "-$cmd_pid" 2>/dev/null || kill -TERM "$cmd_pid" 2>/dev/null || true
    sleep 2
    kill -KILL "-$cmd_pid" 2>/dev/null || kill -KILL "$cmd_pid" 2>/dev/null || true
    wait "$cmd_pid" 2>/dev/null || true
    return 124
}

# sha256 of a file, portable across macOS (shasum) and Linux (sha256sum).
file_sha256() {
    local f="$1" out=""
    out="$(shasum -a 256 "$f" 2>/dev/null | awk '{print $1}')"
    if [ -z "$out" ]; then
        out="$(sha256sum "$f" 2>/dev/null | awk '{print $1}')"
    fi
    echo "$out"
}

# =============================================================================
# Pre-flight checks
# =============================================================================

print_banner() {
    echo ""
    echo -e "${MAGENTA}${BOLD}"
    echo "┌─────────────────────────────────────────────────────────┐"
    echo "│                  Echo Agent Installer                   │"
    echo "├─────────────────────────────────────────────────────────┤"
    echo "│  Self-hosted AI agent runtime for your own workspace.    │"
    echo "└─────────────────────────────────────────────────────────┘"
    echo -e "${NC}"
}

detect_os() {
    case "$(uname -s)" in
        Linux*)
            OS="linux"
            detect_linux_flavor
            ;;
        Darwin*)
            OS="macos"
            ;;
        CYGWIN*|MINGW*|MSYS*)
            log_error "Native Windows is not supported."
            log_info "Use WSL2 and run this installer there."
            exit 1
            ;;
        *)
            log_error "Unsupported operating system: $(uname -s)"
            exit 1
            ;;
    esac
    log_success "Detected: $OS"
}

# Identify the Linux flavour we're on. Three things later depend on it: musl
# has no upstream Node build and almost no manylinux wheels, WSL2 usually has
# no running systemd, and the build-tools hint differs per package manager.
detect_linux_flavor() {
    DISTRO="unknown"
    if [ -r /etc/os-release ]; then
        # shellcheck disable=SC1091  # present at runtime, not in this repo
        DISTRO="$( . /etc/os-release 2>/dev/null && echo "${ID:-unknown}" )"
        [ -n "$DISTRO" ] || DISTRO="unknown"
    fi

    # musl vs glibc. `ldd --version` writes to stderr on musl and prints
    # "musl libc"; on glibc it reports "GNU libc"/"GLIBC".
    IS_MUSL=false
    if command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; then
        IS_MUSL=true
    elif [ -n "$(find /lib /usr/lib -maxdepth 1 -name 'libc.musl-*' -print -quit 2>/dev/null)" ]; then
        IS_MUSL=true
    fi

    IS_WSL=false
    if [ -r /proc/version ] && grep -qiE "microsoft|wsl" /proc/version 2>/dev/null; then
        IS_WSL=true
    fi

    local extra=""
    [ "$IS_WSL" = true ] && extra=" (WSL)"
    [ "$IS_MUSL" = true ] && extra="$extra (musl)"
    log_info "Linux flavour: ${DISTRO}${extra}"

    if [ "$IS_MUSL" = true ]; then
        log_warn "musl-based distro detected (Alpine and friends)."
        log_warn "Upstream Node.js and many Python wheels (faiss-cpu, onnxruntime)"
        log_warn "ship glibc-only builds, so parts of this install may fail or need"
        log_warn "a compiler. Consider a glibc distro for a smoother experience:"
        log_warn "  apk add build-base cmake python3-dev  # if you continue here"
    fi
}

# Point the user at the toolchain when a dependency failed while compiling.
# Cheap heuristic on the captured log; no-op when the failure looks unrelated.
suggest_build_tools() {
    local log_file="$1"
    [ -r "$log_file" ] || return 0
    grep -qiE "gcc|cc1|compiler|Microsoft Visual|python\.h|ffi\.h|cmake|maturin|failed building wheel|error: command" \
        "$log_file" 2>/dev/null || return 0

    log_warn "That looks like a build-from-source failure — the toolchain may be missing."
    case "${DISTRO:-unknown}" in
        ubuntu|debian|raspbian|pop|linuxmint|elementary|zorin|kali|parrot)
            log_info "  sudo apt-get install -y build-essential python3-dev libffi-dev" ;;
        fedora|rhel|centos|rocky|alma|ol)
            log_info "  sudo dnf install -y gcc gcc-c++ make python3-devel libffi-devel" ;;
        arch|manjaro|endeavouros|garuda|cachyos)
            log_info "  sudo pacman -S --needed base-devel libffi" ;;
        opensuse*|sles)
            log_info "  sudo zypper install -y gcc gcc-c++ make python3-devel libffi-devel" ;;
        alpine)
            log_info "  sudo apk add build-base cmake python3-dev libffi-dev" ;;
        *)
            log_info "  Install a C toolchain plus your distro's python3 development headers." ;;
    esac
}

check_prerequisites() {
    local missing=()
    command -v curl >/dev/null 2>&1 || missing+=("curl")
    command -v tar  >/dev/null 2>&1 || missing+=("tar")

    if [ ${#missing[@]} -eq 0 ]; then
        return 0
    fi

    # Minimal Linux images (debian-slim, ubuntu, alpine, minimal RHEL) ship
    # none of these. Every download path here needs curl, so fail early with an
    # actionable message instead of a confusing error twenty lines later.
    log_error "Missing required tool(s): ${missing[*]}"
    case "${DISTRO:-unknown}" in
        ubuntu|debian|raspbian|pop|linuxmint|elementary|zorin|kali|parrot)
            log_info "  sudo apt-get update && sudo apt-get install -y ${missing[*]} ca-certificates" ;;
        fedora|rhel|centos|rocky|alma|ol)
            log_info "  sudo dnf install -y ${missing[*]} ca-certificates" ;;
        arch|manjaro|endeavouros|garuda|cachyos)
            log_info "  sudo pacman -S --needed ${missing[*]} ca-certificates" ;;
        opensuse*|sles)
            log_info "  sudo zypper install -y ${missing[*]} ca-certificates" ;;
        alpine)
            log_info "  sudo apk add ${missing[*]} ca-certificates" ;;
        *)
            if [ "$OS" = "macos" ]; then
                log_info "  Install the Xcode command line tools: xcode-select --install"
            else
                log_info "  Install ${missing[*]} with your package manager, then rerun this script."
            fi
            ;;
    esac
    exit 1
}

check_network() {
    log_info "Checking network connectivity..."
    local all_ok=true

    # Probe a small per-package page, NOT https://pypi.org/simple/ — the full
    # index is ~42MB and takes longer than the 10s budget on an average link,
    # so it reported "cannot reach PyPI" on perfectly good connections.
    #
    # github.com is deliberately NOT probed here: it is commonly blocked in
    # mainland China while the install still succeeds entirely via the Gitee
    # mirror, so warning about it would be noise. select_repo_host() reports the
    # code host it actually picked.
    for url in "https://pypi.org/simple/pip/" "https://nodejs.org"; do
        if ! curl -sSf --connect-timeout 5 --max-time 10 "$url" >/dev/null 2>&1; then
            log_warn "Cannot reach $url — some steps may fail."
            all_ok=false
        fi
    done

    if [ "$all_ok" = true ]; then
        log_success "Network connectivity OK"
    fi
}

check_git() {
    log_info "Checking Git..."
    if command -v git >/dev/null 2>&1; then
        log_success "$(git --version)"
        return 0
    fi
    log_error "Git not found."
    if [ "$OS" = "macos" ]; then
        log_info "Install it with: xcode-select --install"
    else
        log_info "Install it with your package manager, then rerun this script."
    fi
    exit 1
}

# =============================================================================
# Install tools (uv, Python, Node)
# =============================================================================

install_uv() {
    log_info "Checking uv..."
    if command -v uv >/dev/null 2>&1; then
        UV_CMD="uv"
        log_success "$(uv --version)"
        return 0
    fi
    if [ -x "$HOME/.local/bin/uv" ]; then
        UV_CMD="$HOME/.local/bin/uv"
        log_success "$($UV_CMD --version)"
        return 0
    fi

    log_info "Installing uv..."
    # Download to a file first, then run it. `curl ... | sh` reports *sh's*
    # status, and sh exits 0 on empty stdin — so a failed or missing curl was
    # indistinguishable from a successful install.
    local uv_installer
    uv_installer="$(mktemp)"
    if ! run_with_timeout 120 curl -fsSL https://astral.sh/uv/install.sh -o "$uv_installer"; then
        rm -f "$uv_installer"
        log_error "Failed to download the uv installer."
        log_info "Check network access to astral.sh, or install uv manually:"
        log_info "  https://docs.astral.sh/uv/getting-started/installation/"
        exit 1
    fi
    if ! sh "$uv_installer"; then
        rm -f "$uv_installer"
        log_error "The uv installer exited with an error."
        exit 1
    fi
    rm -f "$uv_installer"

    if [ -x "$HOME/.local/bin/uv" ]; then
        UV_CMD="$HOME/.local/bin/uv"
    elif command -v uv >/dev/null 2>&1; then
        UV_CMD="uv"
    else
        log_error "uv installed but not found on PATH."
        log_info "Expected it at ~/.local/bin/uv."
        exit 1
    fi
    log_success "$($UV_CMD --version)"
}

check_python() {
    log_info "Checking Python $PYTHON_VERSION..."
    if command -v python3 >/dev/null 2>&1; then
        if python3 -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 11) else 1)' >/dev/null 2>&1; then
            PYTHON_PATH="$(command -v python3)"
            log_success "$($PYTHON_PATH --version)"
            return 0
        fi
    fi

    if "$UV_CMD" python find "$PYTHON_VERSION" >/dev/null 2>&1; then
        PYTHON_PATH="$("$UV_CMD" python find "$PYTHON_VERSION")"
        log_success "$($PYTHON_PATH --version)"
        return 0
    fi

    log_info "Installing Python $PYTHON_VERSION via uv..."
    "$UV_CMD" python install "$PYTHON_VERSION"
    PYTHON_PATH="$("$UV_CMD" python find "$PYTHON_VERSION")"
    log_success "$($PYTHON_PATH --version)"
}

node_version_ok() {
    local ver="${1#v}"
    local major="${ver%%.*}"
    [ "$major" -ge 20 ] 2>/dev/null
}

install_node() {
    log_info "Installing Node.js v${NODE_VERSION} LTS..."

    # nodejs.org publishes glibc-only builds. A glibc tarball on musl extracts
    # fine and even keeps its executable bit, so it would get symlinked onto the
    # user's PATH as a permanently broken node/npm/npx. Refuse instead.
    if [ "${IS_MUSL:-false}" = true ]; then
        log_warn "Skipping Node.js auto-install: no official musl build exists."
        log_info "Install your distro's package instead, e.g.: sudo apk add nodejs npm"
        return 1
    fi

    local arch
    case "$(uname -m)" in
        x86_64)  arch="x64" ;;
        aarch64) arch="arm64" ;;
        arm64)   arch="arm64" ;;
        armv7l)  arch="armv7l" ;;
        *)
            log_warn "Unsupported architecture $(uname -m) for Node.js auto-install."
            return 1
            ;;
    esac

    local node_os
    case "$OS" in
        linux)  node_os="linux" ;;
        macos)  node_os="darwin" ;;
        *)
            log_warn "Unsupported OS for Node.js auto-install."
            return 1
            ;;
    esac

    local node_dir="$ECHO_HOME/node"

    # Resolve which patch release to install from the OFFICIAL host only. The
    # mirrors lag (npmmirror served v22.20.0 while nodejs.org had v22.23.1) and
    # aliyun has no latest-vXX.x/ path at all, so letting a mirror pick the
    # version would silently install a stale Node — possibly one with known
    # CVEs. Version choice is official; only the bytes may come from a mirror.
    local node_full_ver=""
    node_full_ver=$(curl -sSfL --connect-timeout 5 --max-time 20 \
        "${NODE_DIST_OFFICIAL}/latest-v${NODE_VERSION}.x/SHASUMS256.txt" 2>/dev/null \
        | grep -oE "node-v${NODE_VERSION}\.[0-9]+\.[0-9]+" | head -1 | sed 's/node-//')
    if [ -z "$node_full_ver" ]; then
        log_warn "Could not resolve the latest Node ${NODE_VERSION}.x release from nodejs.org."
        log_info "Install Node.js >= 20 yourself, then rerun to build the Dashboard."
        return 1
    fi

    local pkg_name="node-${node_full_ver}-${node_os}-${arch}"
    local tmp_dir
    tmp_dir="$(mktemp -d)"
    # Always reclaim the temp dir, including on an early return below.
    trap 'rm -rf "$tmp_dir"' RETURN

    # Official checksums for exactly that release. Every candidate download is
    # verified against these, so a mirror serving different bytes is rejected.
    local sums_file="$tmp_dir/SHASUMS256.txt"
    if ! run_with_timeout 60 curl -fsSL "${NODE_DIST_OFFICIAL}/${node_full_ver}/SHASUMS256.txt" -o "$sums_file"; then
        log_warn "Could not fetch Node.js checksums; skipping Node auto-install."
        return 1
    fi

    # Choose a download host: race the mirrors for the *specific* release we
    # settled on, since a mirror that lags won't have it at all.
    local node_base="$NODE_DIST_OFFICIAL"
    if [ "$MIRROR_PROBE" = true ]; then
        local cand best_t="999" t
        for cand in "${NODE_DIST_MIRRORS[@]}"; do
            if t="$(curl -fsSL -o /dev/null -w '%{time_total}' --connect-timeout 3 --max-time 6 \
                    -r 0-0 "${cand}/${node_full_ver}/SHASUMS256.txt" 2>/dev/null)"; then
                if LC_ALL=C awk "BEGIN{exit !($t < $best_t)}"; then
                    best_t="$t"; node_base="$cand"
                fi
            fi
        done
        log_info "Node.js ${node_full_ver} download source: $node_base"
    fi

    local downloaded=false ext url host expected actual
    # Prefer .tar.xz, fall back to .tar.gz; for each, try the chosen host and
    # then the official one. Every step is guarded so a failure moves on instead
    # of aborting the whole installer via set -e.
    for ext in "tar.xz" "tar.gz"; do
        for host in "$node_base" "$NODE_DIST_OFFICIAL"; do
            url="${host}/${node_full_ver}/${pkg_name}.${ext}"
            log_info "Downloading ${pkg_name}.${ext} from ${host}..."
            if ! run_with_timeout 300 curl -fSL --progress-bar "$url" -o "$tmp_dir/node.${ext}"; then
                log_warn "Download failed from ${host}."
                rm -f "$tmp_dir/node.${ext}"
                [ "$host" = "$NODE_DIST_OFFICIAL" ] && break
                continue
            fi

            expected="$(grep " ${pkg_name}.${ext}\$" "$sums_file" 2>/dev/null | awk '{print $1}')"
            actual="$(file_sha256 "$tmp_dir/node.${ext}")"
            if [ -z "$expected" ] || [ "$actual" != "$expected" ]; then
                log_warn "Checksum mismatch from ${host} (got ${actual:-none}); discarding."
                rm -f "$tmp_dir/node.${ext}"
                [ "$host" = "$NODE_DIST_OFFICIAL" ] && break
                continue
            fi
            break
        done
        # Nothing verified for this extension — try the next archive format.
        [ -f "$tmp_dir/node.${ext}" ] || continue

        if [ "$ext" = "tar.xz" ]; then
            tar -xJf "$tmp_dir/node.${ext}" -C "$tmp_dir" || {
                log_warn "Extraction failed for ${ext} (xz support missing?); trying next format."
                continue
            }
        else
            tar -xzf "$tmp_dir/node.${ext}" -C "$tmp_dir" || {
                log_warn "Extraction failed for ${ext}."
                continue
            }
        fi

        # Check it actually runs — an executable bit alone doesn't prove the
        # binary matches this host's libc/arch.
        if [ ! -x "$tmp_dir/$pkg_name/bin/node" ] || \
           ! "$tmp_dir/$pkg_name/bin/node" -v >/dev/null 2>&1; then
            log_warn "Extracted Node.js binary does not run on this host; trying next format."
            continue
        fi

        # Only now touch the existing install: swap the fully-verified tree into
        # place, keeping the old one until the move succeeds. The previous order
        # (rm -rf then mv) destroyed a working Node whenever extraction or the
        # move failed.
        mkdir -p "$(dirname "$node_dir")"
        local old_dir="${node_dir}.old-$$"
        if [ -e "$node_dir" ] && ! mv "$node_dir" "$old_dir"; then
            log_warn "Could not move the existing Node.js aside; keeping it."
            return 1
        fi
        if mv "$tmp_dir/$pkg_name" "$node_dir"; then
            rm -rf "$old_dir"
            downloaded=true
            break
        fi
        log_warn "Could not install Node.js into $node_dir; restoring previous version."
        [ -d "$old_dir" ] && mv "$old_dir" "$node_dir"
        return 1
    done

    if [ "$downloaded" = false ]; then
        log_warn "Failed to install Node.js. Dashboard build will be skipped."
        return 1
    fi

    # Symlink node/npm/npx to the command link dir
    local link_dir
    link_dir="$(get_command_link_dir)"
    mkdir -p "$link_dir"
    ln -sf "$node_dir/bin/node" "$link_dir/node"
    ln -sf "$node_dir/bin/npm" "$link_dir/npm"
    ln -sf "$node_dir/bin/npx" "$link_dir/npx"

    HAS_NODE=true
    export PATH="$node_dir/bin:$PATH"
    log_success "Node.js $("$node_dir/bin/node" -v) installed to $node_dir"
}

check_node() {
    if [ "$SKIP_DASHBOARD" = true ]; then
        # Node exists in this install only to build the Dashboard.
        HAS_NODE=false
        return 0
    fi
    log_info "Checking Node.js..."

    # Check system node
    if command -v node >/dev/null 2>&1; then
        local sys_ver
        sys_ver="$(node -v 2>/dev/null || echo "")"
        if node_version_ok "$sys_ver"; then
            HAS_NODE=true
            log_success "System Node.js $sys_ver"
            return 0
        fi
    fi

    # Check managed node
    local managed_node="$ECHO_HOME/node/bin/node"
    if [ -x "$managed_node" ]; then
        local managed_ver
        managed_ver="$("$managed_node" -v 2>/dev/null || echo "")"
        if node_version_ok "$managed_ver"; then
            HAS_NODE=true
            export PATH="$ECHO_HOME/node/bin:$PATH"
            log_success "Managed Node.js $managed_ver at $ECHO_HOME/node/"
            return 0
        fi
    fi

    # Neither found — install
    if install_node; then
        return 0
    fi

    HAS_NODE=false
    log_warn "Node.js >= 20 not available. Dashboard build will be skipped."
}

# =============================================================================
# Repository & environment setup
# =============================================================================

# Pick the code host to clone from. Measures real reachability instead of
# guessing by locale/GeoIP, so a VPN, a corporate link or a blocked github.com
# all resolve correctly. Sets REPO_LABEL and REPO_URL_HTTPS; clone_repo reads the
# per-host URLs straight out of REPO_HOSTS, keyed by REPO_LABEL.
#
# The probe hits the git smart-HTTP endpoint rather than the project web page, so
# a host that serves HTML but cannot complete a clone is not selected. Only HTTPS
# is probed — SSH key availability is orthogonal, and clone_repo already falls
# back from SSH to HTTPS per host.
select_repo_host() {
    local entry label https_url

    # An explicit choice wins and skips the network entirely.
    if [ -n "$REPO_HOST" ]; then
        for entry in "${REPO_HOSTS[@]}"; do
            label="${entry%%|*}"
            if [ "$label" = "$REPO_HOST" ]; then
                REPO_LABEL="$label"; REPO_URL_HTTPS="${entry##*|}"
                log_success "Using $label as the code host (requested explicitly)"
                return 0
            fi
        done
        log_warn "Unknown repo host '$REPO_HOST'; falling back to probing."
    fi

    # Default to the first entry so a failed probe still yields a usable source.
    entry="${REPO_HOSTS[0]}"
    REPO_LABEL="${entry%%|*}"
    REPO_URL_HTTPS="${entry##*|}"

    if [ "$MIRROR_PROBE" != true ]; then
        log_info "Host probe disabled; using $REPO_LABEL."
        return 0
    fi

    log_info "Probing code hosts (github/gitee) for the fastest reachable one..."
    local best_label="" best_https="" best_t="999" t
    for entry in "${REPO_HOSTS[@]}"; do
        label="${entry%%|*}"
        https_url="${entry##*|}"
        # %{time_total} gives sub-second resolution, which `date +%s` cannot.
        # Gate on curl's exit status: the time is printed even for a failed
        # request, so a fast failure would otherwise look like the best host.
        if t="$(curl -fsSL -o /dev/null -w '%{time_total}' --connect-timeout 3 --max-time 8 \
                "${https_url}/info/refs?service=git-upload-pack" 2>/dev/null)"; then
            log_info "  $label: reachable (${t}s)"
            # LC_ALL=C pins the radix character: curl always writes '.', but awk
            # under a comma-decimal locale would read "0.11" as 0.
            if LC_ALL=C awk "BEGIN{exit !($t < $best_t)}"; then
                best_t="$t"; best_label="$label"; best_https="$https_url"
            fi
        else
            log_warn "  $label: unreachable"
        fi
    done

    if [ -n "$best_label" ]; then
        REPO_LABEL="$best_label"; REPO_URL_HTTPS="$best_https"
        log_success "Code host: $REPO_LABEL (${best_t}s)"
    else
        log_warn "No code host responded; will still try $REPO_LABEL."
        log_info "If you are in mainland China and github.com is blocked, try: --repo gitee"
    fi
}

clone_repo() {
    log_info "Preparing repository in $INSTALL_DIR..."

    # An interrupted earlier clone leaves a .git with no commits. Every git
    # operation below fails in that state ("you do not have the initial commit
    # yet"), and the old code hit the "not a git repository" exit instead and
    # wedged the installer permanently. Move it aside rather than deleting it.
    if [ -d "$INSTALL_DIR/.git" ] && ! git -C "$INSTALL_DIR" rev-parse --verify HEAD >/dev/null 2>&1; then
        local broken_dir
        broken_dir="${INSTALL_DIR}.broken-$(date -u +%Y%m%d-%H%M%S)"
        log_warn "Existing checkout has no commits (interrupted clone)."
        log_warn "Moving it aside to $broken_dir before re-cloning."
        mv "$INSTALL_DIR" "$broken_dir"
    fi

    if [ -d "$INSTALL_DIR/.git" ]; then
        update_repo
        return 0
    fi

    if [ -e "$INSTALL_DIR" ]; then
        log_error "Install directory exists but is not a git repository: $INSTALL_DIR"
        log_info "Remove it, or choose another location with --dir PATH"
        exit 1
    fi

    # Try the selected host first, then the other one. The probe measures
    # reachability, but a clone is a much bigger transfer than a refs
    # advertisement and can still fail (flaky link, partial outage) — and a
    # fatal clone failure aborts the entire install, so spend the second host
    # before giving up rather than telling the user to fix their network.
    local order=("$REPO_LABEL") entry label
    for entry in "${REPO_HOSTS[@]}"; do
        label="${entry%%|*}"
        [ "$label" = "$REPO_LABEL" ] || order+=("$label")
    done

    local ssh_url https_url
    for label in "${order[@]}"; do
        for entry in "${REPO_HOSTS[@]}"; do
            [ "${entry%%|*}" = "$label" ] || continue
            ssh_url="${entry#*|}"; ssh_url="${ssh_url%%|*}"
            https_url="${entry##*|}"
        done

        log_info "Trying SSH clone from $label..."
        if GIT_SSH_COMMAND="ssh -o BatchMode=yes -o ConnectTimeout=5" \
            run_with_timeout 300 git clone --branch "$BRANCH" "$ssh_url" "$INSTALL_DIR" 2>/dev/null; then
            log_success "Cloned via SSH from $label"
            return 0
        fi
        # A failed SSH attempt can leave a partial directory behind, which would
        # make the HTTPS clone fail with "destination path already exists".
        rm -rf "$INSTALL_DIR"

        log_info "SSH unavailable, trying HTTPS from $label..."
        if run_with_timeout 300 git clone --branch "$BRANCH" "$https_url" "$INSTALL_DIR"; then
            log_success "Cloned via HTTPS from $label"
            return 0
        fi
        rm -rf "$INSTALL_DIR"
        log_warn "Clone from $label failed."
    done

    log_error "Failed to clone the repository from any host (${order[*]})."
    log_info "Check your network, or pick a host explicitly: --repo gitee"
    log_info "Branch '$BRANCH' must exist on the host — verify with --branch."
    exit 1
}

# Update an existing checkout to origin/$BRANCH.
#
# The previous behaviour was to skip the update entirely whenever `git status`
# was dirty. Because a source install exists precisely to track code newer than
# the last PyPI release, that turned any stray local edit — including lockfile
# churn produced by the install itself — into a permanent freeze on an old
# commit, announced with a single warning the user would likely miss.
update_repo() {
    cd "$INSTALL_DIR"
    log_info "Existing installation found, updating..."

    local stash_ref="" stash_name=""
    if [ -n "$(git status --porcelain)" ]; then
        # An interrupted earlier update can leave unmerged index entries, in
        # which case `git stash` refuses to run and `git checkout` aborts.
        # Clearing the index keeps working-tree edits (stashed just below) and
        # only drops the conflict state.
        if [ -n "$(git ls-files --unmerged)" ]; then
            log_info "Clearing unmerged index entries from a previous conflict..."
            git reset -q
        fi
        stash_name="echo-install-autostash-$(date -u +%Y%m%d-%H%M%S)"
        log_info "Local changes detected; stashing them before the update..."
        if git stash push --include-untracked -m "$stash_name" >/dev/null; then
            stash_ref="stash@{0}"
            log_info "Stashed as $stash_name"
        else
            log_warn "Could not stash local changes; skipping the update to avoid losing them."
            log_info "Commit or clean $INSTALL_DIR, then rerun this installer."
            return 0
        fi
    fi

    # Fetch only the branch we need. A bare `git fetch origin` pulls every ref,
    # which makes each update proportional to the total number of branches.
    git remote set-branches origin "$BRANCH" 2>/dev/null || true
    if ! run_with_timeout 300 git fetch origin "$BRANCH"; then
        # An existing checkout keeps whatever remote it was cloned from, which
        # may now be the unreachable one (a user who cloned from github before
        # it got blocked, or vice versa). Retry against the host selected for
        # this run before giving up — without rewriting origin, so the user's own
        # remote configuration is left alone.
        local retried=false
        if [ -n "$REPO_URL_HTTPS" ] && ! git remote get-url origin 2>/dev/null | grep -qF "$REPO_URL_HTTPS"; then
            log_warn "Fetch from origin failed; retrying against $REPO_LABEL..."
            if run_with_timeout 300 git fetch "$REPO_URL_HTTPS" "$BRANCH"; then
                # FETCH_HEAD now holds the branch tip; point the tracking ref at
                # it so the comparisons below have something to work with.
                git update-ref "refs/remotes/origin/$BRANCH" FETCH_HEAD
                retried=true
                log_success "Fetched from $REPO_LABEL"
            fi
        fi
        if [ "$retried" != true ]; then
            log_warn "Fetch failed; keeping the current checkout."
            log_info "The install continues with the code already on disk."
            log_info "If your usual host is blocked, retry with: --repo gitee (or --repo github)"
            restore_stash "$stash_ref"
            return 0
        fi
    fi

    git checkout "$BRANCH"
    # Fast-forward from the tracking ref we just fetched, not `git pull origin
    # $BRANCH`: pull would open a second connection to origin — which is the
    # wrong host when the fetch above fell back to the other mirror, turning a
    # recovered update back into a failure. The ref is already local here, so
    # merge --ff-only needs no network at all.
    if ! git merge --ff-only "refs/remotes/origin/$BRANCH"; then
        # Fast-forward failed, so the branch has diverged from origin. Decide by
        # what would actually be DESTROYED, which is local commits — not the
        # working tree. `git status --porcelain` only reports uncommitted edits
        # and is always empty for a committed-but-unpushed change, so gating the
        # reset on it discarded the user's commits silently (the autostash above
        # captures the working tree only; it cannot hold a commit).
        local local_commits=""
        local_commits="$(git rev-list --count "origin/$BRANCH..HEAD" 2>/dev/null || echo "0")"
        if [ "${local_commits:-0}" -gt 0 ]; then
            log_warn "Fast-forward not possible: this checkout has $local_commits local commit(s)"
            log_warn "that are not on origin/$BRANCH. Leaving the code as it is —"
            log_warn "updating would throw those commits away."
            log_info "Keep them by pushing or moving them to a branch:"
            log_info "  git -C $INSTALL_DIR log origin/$BRANCH..HEAD    # see them"
            log_info "  git -C $INSTALL_DIR branch my-work              # park them"
            log_info "Or discard them deliberately, then rerun this installer:"
            log_info "  git -C $INSTALL_DIR reset --hard origin/$BRANCH"
            log_warn "The install continues with the code currently checked out."
            restore_stash "$stash_ref"
            return 0
        fi
        # No local commits to lose: the divergence is rewritten history upstream
        # (force-push, rebase). Realigning is safe — the working tree is either
        # clean or already captured in the stash restored below.
        log_warn "Fast-forward not possible; realigning this managed checkout to origin/$BRANCH."
        log_info "No local commits would be lost."
        git reset --hard "origin/$BRANCH"
    fi
    log_success "Repository updated to $(git rev-parse --short HEAD)"

    restore_stash "$stash_ref"
}

# Reapply an autostash, asking first when we have a terminal: replaying local
# edits onto updated code can conflict, and the user is better placed to decide.
restore_stash() {
    local ref="$1"
    [ -n "$ref" ] || return 0

    local restore="yes"
    if [ "$IS_INTERACTIVE" = true ] || { [ -r /dev/tty ] && [ -w /dev/tty ]; }; then
        echo ""
        log_warn "Your local changes were stashed before updating."
        log_warn "Reapplying them may conflict with the updated code."
        if prompt_yes_no "Restore your local changes now?" "yes"; then
            restore="yes"
        else
            restore="no"
        fi
    fi

    if [ "$restore" != "yes" ]; then
        log_info "Left your changes in the stash. Restore later with: git -C $INSTALL_DIR stash apply $ref"
        return 0
    fi

    log_info "Restoring local changes..."
    if git stash apply "$ref" >/dev/null 2>&1; then
        git stash drop "$ref" >/dev/null 2>&1 || true
        log_warn "Local changes reapplied on top of the updated code."
        log_info "Review with: git -C $INSTALL_DIR diff"
        return 0
    fi

    # A conflicting apply leaves conflict markers in the working tree and an
    # unmerged index. Installing from that state would compile source files
    # containing '<<<<<<<', so roll the tree back to the clean updated code and
    # leave the changes in the stash for the user to merge deliberately.
    log_warn "Your changes conflict with the updated code, so they were NOT reapplied."
    git checkout -- . >/dev/null 2>&1 || true
    git reset -q --hard HEAD >/dev/null 2>&1 || true
    log_info "The checkout is on clean updated code; your changes are preserved in the stash."
    log_info "Merge them yourself with: git -C $INSTALL_DIR stash apply $ref"
}

setup_venv() {
    cd "$INSTALL_DIR"
    log_info "Setting up virtual environment..."

    # Idempotent: keep any venv whose interpreter satisfies requires-python
    # (>=3.11 in pyproject.toml) rather than demanding exactly PYTHON_VERSION.
    # An exact match would throw away a perfectly good 3.12/3.13 venv on every
    # run and reinstall every dependency with it.
    if [ -d "venv" ] && [ -x "venv/bin/python" ]; then
        local existing_ver
        existing_ver="$(venv/bin/python --version 2>/dev/null | awk '{print $2}' || echo "")"
        if venv/bin/python -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 11) else 1)' >/dev/null 2>&1; then
            log_success "Virtual environment already exists (Python $existing_ver)"
            return 0
        fi
        log_info "Existing venv Python ($existing_ver) is older than 3.11; rebuilding..."
        rm -rf venv
    fi

    "$UV_CMD" venv venv --python "$PYTHON_PATH"
    log_success "Virtual environment ready"
}

# Measure round-trip latency to each candidate index and keep the fastest that
# actually responds. We test real network quality rather than guessing location
# from GeoIP/locale, so VPNs, corporate links and offline mirrors all behave
# correctly. Sets PYPI_INDEX; leaves it empty if nothing responds.
probe_pypi_index() {
    # Honor an explicit user choice and skip probing entirely.
    if [ -n "$ECHO_PYPI_INDEX" ]; then
        PYPI_INDEX="$ECHO_PYPI_INDEX"
        log_success "Using PyPI index from ECHO_PYPI_INDEX: $PYPI_INDEX"
        return 0
    fi
    if [ -n "${UV_DEFAULT_INDEX:-}" ] || [ -n "${UV_INDEX_URL:-}" ]; then
        log_info "Respecting UV_DEFAULT_INDEX/UV_INDEX_URL from environment; skipping mirror probe."
        return 0
    fi
    if [ "$MIRROR_PROBE" != true ]; then
        log_info "Mirror probe disabled; using uv default index."
        return 0
    fi

    log_info "Probing PyPI mirrors for the fastest one..."
    local best_label="" best_url="" best_time="999"
    local entry label url t
    for entry in "${PYPI_MIRRORS[@]}"; do
        label="${entry%%|*}"
        url="${entry#*|}"
        # -o /dev/null: discard body; %{time_total}: full request time in seconds.
        # -f makes HTTP 4xx/5xx a failure and -L follows redirects; gate on curl's
        # exit status, since %{time_total} is printed even when the request fails
        # (a fast failure would otherwise look like the fastest mirror).
        if ! t="$(curl -fsSL -o /dev/null -w '%{time_total}' --connect-timeout 3 --max-time 5 \
             "${url}/pip/" 2>/dev/null)"; then
            log_warn "  $label: unreachable"
            continue
        fi
        log_info "  $label: ${t}s"
        # Numeric compare via awk (times are floats like 0.83). LC_ALL=C pins
        # the radix character: curl always writes '.', but awk under a
        # comma-decimal LC_NUMERIC would parse "0.83" as 0 and pick a mirror at
        # random.
        if LC_ALL=C awk "BEGIN{exit !($t < $best_time)}"; then
            best_time="$t"; best_label="$label"; best_url="$url"
        fi
    done

    if [ -n "$best_url" ]; then
        PYPI_INDEX="$best_url"
        log_success "Fastest index: $best_label (${best_time}s) -> $PYPI_INDEX"
    else
        log_warn "No PyPI index responded; using uv default. Install may be slow."
    fi
}

install_deps() {
    cd "$INSTALL_DIR"
    export VIRTUAL_ENV="$INSTALL_DIR/venv"

    probe_pypi_index

    # Build index args: chosen mirror as an --index (higher priority), official
    # PyPI kept as --default-index for fallback. uv always treats the
    # default-index as LOWEST priority and stops at the first index that has a
    # package (first-index strategy), so the mirror must be an --index to
    # actually be preferred; a --default-index mirror would be bypassed by the
    # official index on nearly every public package.
    local index_args=()
    if [ -n "$PYPI_INDEX" ] && [ "$PYPI_INDEX" != "$PYPI_OFFICIAL" ]; then
        index_args+=(--index "$PYPI_INDEX" --default-index "$PYPI_OFFICIAL")
    fi

    # DEPS_TIMEOUT is a budget for the whole dependency phase, not per attempt.
    # Giving each tier its own full timeout meant a user on a dead link waited
    # 3 x 600s before learning the install had degraded.
    local deadline=$(( $(date +%s) + DEPS_TIMEOUT ))
    remaining_budget() {
        local left=$(( deadline - $(date +%s) ))
        [ "$left" -lt 30 ] && left=30   # always allow a real attempt
        echo "$left"
    }

    local deps_log
    deps_log="$(mktemp)"

    # Run one tier. On failure, surface the head of uv's output — the previous
    # 2>/dev/null discarded exactly the text needed to tell a network timeout
    # apart from a missing compiler.
    install_tier() {
        local label="$1" spec="$2" budget rc=0
        budget="$(remaining_budget)"
        log_info "Installing dependencies ($label, up to ${budget}s)..."

        # uv writes everything (resolution, downloads, errors) to stderr, so
        # sending it to a file alone left this — the longest phase of the
        # install, up to DEPS_TIMEOUT seconds on `.[all]` — completely silent and
        # easy to mistake for a hang. We still need the text on disk for
        # suggest_build_tools, so duplicate both streams with `tee`.
        #
        # Process substitution, deliberately NOT `uv ... 2>&1 | tee`:
        #   - A pipeline puts uv on the left, so `$?` becomes *tee's* status (0
        #     even when uv fails) and the 124 timeout signal is lost — the same
        #     class of bug as the `if`-swallows-$? one fixed earlier.
        #   - Worse, the shell then waits for tee's EOF, which a build
        #     subprocess surviving the kill keeps open: measured 60s of hang on a
        #     3s budget, i.e. the pipe quietly defeats the timeout entirely.
        # With `> >(tee)` the command stays the foreground job, so `|| rc=$?`
        # sees uv's own status and the deadline is still enforced. Verified on
        # both the GNU-timeout path and the pure-shell fallback.
        : > "$deps_log"
        run_with_timeout "$budget" "$UV_CMD" pip install "${index_args[@]}" -e "$spec" \
            > >(tee -a "$deps_log") 2> >(tee -a "$deps_log" >&2) || rc=$?

        if [ "$rc" -eq 0 ]; then
            return 0
        fi
        if [ "$rc" -eq 124 ]; then
            log_warn "Tier '$label' timed out after ${budget}s."
        else
            log_warn "Tier '$label' failed (exit $rc)."
            suggest_build_tools "$deps_log"
        fi
        return 1
    }

    if install_tier "full" ".[all]"; then
        rm -f "$deps_log"
        log_success "Dependencies installed (full)"
        return 0
    fi

    # Full extras are large (playwright, faiss-cpu, pymupdf, ...) and may time
    # out on slow links. Fall back to the essential LLM SDKs so the setup wizard
    # can still verify a model, rather than dropping to a base install with no
    # provider SDK at all.
    if install_tier "essential LLM providers" ".[openai,anthropic,gemini]"; then
        rm -f "$deps_log"
        log_warn "Installed a REDUCED set (openai, anthropic, gemini)."
        log_warn "Some features (browser, documents, vector, TUI) are unavailable."
        log_info "To complete later, run:"
        log_info "  cd $INSTALL_DIR && $UV_CMD pip install --python \"$INSTALL_DIR/venv/bin/python\" -e \".[all]\""
        return 0
    fi

    log_warn "Provider install failed too. Falling back to BASE install (no LLM SDK)."
    if install_tier "base" "."; then
        rm -f "$deps_log"
        log_warn "Installed BASE only — no LLM provider SDK."
        log_warn "The setup wizard's model verification will fail until you install one:"
        log_warn "  cd $INSTALL_DIR && $UV_CMD pip install --python \"$INSTALL_DIR/venv/bin/python\" -e \".[openai]\"   # or .[all]"
        return 0
    fi

    rm -f "$deps_log"
    log_error "Dependency installation failed even with no extras."
    log_info "Check network access to PyPI, then retry:"
    log_info "  cd $INSTALL_DIR && $UV_CMD pip install -e \".[all]\""
    return 1
}

# Warm the local embedding model into a STABLE cache so first-run memory vector
# search is an offline cache hit instead of a live download racing the runtime's
# per-message budget. Prefetch runs with a generous budget and lets fastembed
# fall back from the (often unreachable) HF mirror to its GCS source. This is
# best-effort: a failure here never aborts the install — the runtime still works
# in keyword-only mode and retries the download later with backoff.
EMBED_MODEL="${ECHO_EMBED_MODEL:-BAAI/bge-small-zh-v1.5}"
# EMBED_CACHE_DIR is set by resolve_paths() to match the runtime's configured
# cache location; it is deliberately not overridable here, because prefetching
# into a directory the runtime does not read just wastes the download.
EMBED_PREFETCH_TIMEOUT="${ECHO_EMBED_PREFETCH_TIMEOUT:-900}"

# Release-hosted model package (self-owned mirrors, tried before HF/GCS).
# Keep the tag in sync with _RELEASE_PACKAGES in echo_agent/memory/local_embed.py.
EMBED_PKG_NAME="bge-small-zh-v1.5-fastembed.tar.gz"
EMBED_PKG_SHA256="d095c530b22f384d4d19a79c5862b65e8fff104af64ce9bb9e89690c186d418f"
EMBED_PKG_URLS=(
    "https://gitee.com/fuyuxiang/echo-agent/releases/download/v0.3.5/$EMBED_PKG_NAME"
    "https://github.com/fuyuxiang/echo-agent/releases/download/v0.3.5/$EMBED_PKG_NAME"
)
# The directory fastembed expects inside the cache (== tar top-level dir).
EMBED_PKG_DIR="fast-bge-small-zh-v1.5"

# Try to populate the fastembed cache from our own release mirrors (Gitee first
# for CN networks, then GitHub). Success means the later prefetch step is a pure
# offline cache hit. Best-effort: any failure returns non-zero and the caller
# falls through to the existing HF/GCS prefetch path.
fetch_embedding_model_from_release() {
    # Only the default model is packaged on the releases; a custom
    # ECHO_EMBED_MODEL must use the HF/GCS path.
    if [ "$EMBED_MODEL" != "BAAI/bge-small-zh-v1.5" ]; then
        return 1
    fi
    # Cache already materialized (marker: the onnx weights file)?
    if [ -f "$EMBED_CACHE_DIR/$EMBED_PKG_DIR/model_optimized.onnx" ]; then
        log_info "Embedding model already cached; skipping release download."
        return 0
    fi
    if ! command -v curl >/dev/null 2>&1; then
        return 1
    fi

    mkdir -p "$EMBED_CACHE_DIR"
    local tmp_tar="$EMBED_CACHE_DIR/.$EMBED_PKG_NAME.part"
    local url actual
    for url in "${EMBED_PKG_URLS[@]}"; do
        log_info "Downloading embedding model from release: $url"
        if ! curl -fsSL --retry 2 --connect-timeout 15 --max-time 600 \
                -o "$tmp_tar" "$url"; then
            log_warn "Download failed from $url; trying next source."
            rm -f "$tmp_tar"
            continue
        fi
        actual="$(file_sha256 "$tmp_tar")"
        if [ "$actual" != "$EMBED_PKG_SHA256" ]; then
            log_warn "sha256 mismatch from $url (got ${actual:-none}); trying next source."
            rm -f "$tmp_tar"
            continue
        fi
        if tar -xzf "$tmp_tar" -C "$EMBED_CACHE_DIR"; then
            rm -f "$tmp_tar"
            log_success "Embedding model fetched from release mirror (offline-ready)."
            return 0
        fi
        log_warn "Extraction failed for $url; trying next source."
        rm -f "$tmp_tar"
    done
    return 1
}

prefetch_embedding_model() {
    local venv_python="$INSTALL_DIR/venv/bin/python"
    if [ ! -x "$venv_python" ]; then
        log_warn "Skipping embedding model prefetch (venv python not found)."
        return 0
    fi
    if [ -z "$EMBED_MODEL" ]; then
        log_info "Embedding model prefetch disabled (ECHO_EMBED_MODEL empty)."
        return 0
    fi

    # Our own release mirrors first; on success the fastembed load below is an
    # offline cache hit and doubles as the verification step.
    fetch_embedding_model_from_release || true

    log_info "Prefetching local embedding model '$EMBED_MODEL' into $EMBED_CACHE_DIR ..."
    mkdir -p "$EMBED_CACHE_DIR"

    # Tight HF timeouts so a dead mirror fails fast and fastembed reaches its GCS
    # fallback within the budget. HF_ENDPOINT respects an operator override.
    #
    # The whole command is the condition of an `if`, which is what actually makes
    # a failure non-fatal: `trap - ERR` alone does NOT suppress `set -e`, so the
    # previous version aborted the entire installer here whenever the prefetch
    # failed — skipping the PATH symlink, the setup wizard and the service
    # registration, and leaving the user with no `echo-agent` command at all.
    # This step is pure optimization: the runtime re-downloads on demand with
    # backoff (echo_agent/memory/local_embed.py), so it must never be fatal.
    local rc=0
    if HF_HUB_DOWNLOAD_TIMEOUT="${HF_HUB_DOWNLOAD_TIMEOUT:-15}" \
       HF_HUB_ETAG_TIMEOUT="${HF_HUB_ETAG_TIMEOUT:-15}" \
       HF_ENDPOINT="${HF_ENDPOINT:-https://hf-mirror.com}" \
       FASTEMBED_CACHE_PATH="$EMBED_CACHE_DIR" \
       run_with_timeout "$EMBED_PREFETCH_TIMEOUT" "$venv_python" - "$EMBED_MODEL" "$EMBED_CACHE_DIR" <<'PYEOF'
import sys
model_name, cache_dir = sys.argv[1], sys.argv[2]
try:
    from fastembed import TextEmbedding
    emb = TextEmbedding(model_name=model_name, cache_dir=cache_dir)
    # Force a real inference so the ONNX weights are materialized, not just metadata.
    next(iter(emb.embed(["预热"])), None)
    print("ok")
except Exception as e:  # noqa: BLE001 - best-effort prefetch, never fatal
    print(f"prefetch failed: {e}", file=sys.stderr)
    sys.exit(1)
PYEOF
    then
        rc=0
    else
        rc=$?
    fi

    if [ "$rc" -eq 0 ]; then
        log_success "Embedding model cached (offline-ready)."
    else
        log_warn "Embedding model prefetch failed or timed out (rc=$rc)."
        log_warn "Memory vector search starts in keyword-only mode and retries the"
        log_warn "download at runtime. To retry now:"
        log_warn "  FASTEMBED_CACHE_PATH='$EMBED_CACHE_DIR' $INSTALL_DIR/venv/bin/python -c \"from fastembed import TextEmbedding; TextEmbedding(model_name='$EMBED_MODEL', cache_dir='$EMBED_CACHE_DIR')\""
    fi
    return 0
}

# Warm the local cross-encoder reranker the same way as the embedding model, and
# for the same reason: memory.rerank_enabled defaults to TRUE, so without this a
# fresh install pays a ~941MB download at runtime, where every turn until it
# lands silently degrades to the un-reranked RRF order.
#
# Two shapes of the same tarball: Gitee caps a release asset at 100MiB so the
# CN-friendly mirror hosts 10 ordered volumes that concatenate back into the
# exact same file, while GitHub serves it whole. The pinned sha256 belongs to the
# ASSEMBLED tarball (the volumes have no digests of their own), so it is always
# verified after joining — a truncated or misordered join fails like a corrupt
# download. Keep all of this in sync with _RELEASE_PACKAGES in
# echo_agent/memory/local_rerank.py.
RERANK_MODEL="${ECHO_RERANK_MODEL:-BAAI/bge-reranker-base}"
RERANK_PREFETCH_TIMEOUT="${ECHO_RERANK_PREFETCH_TIMEOUT:-1800}"
RERANK_PKG_NAME="bge-reranker-base-fastembed.tar.gz"
RERANK_PKG_SHA256="be3bcc7b24448b3467318f6b4e14fdf0f3e8d4ad0e3c2f1b612a1dd011163fd1"
RERANK_PKG_PART_COUNT=10
RERANK_PKG_PARTS_BASE="https://gitee.com/fuyuxiang/echo-agent/releases/download/v0.3.6/$RERANK_PKG_NAME.part-"
RERANK_PKG_WHOLE_URLS=(
    "https://github.com/fuyuxiang/echo-agent/releases/download/v0.3.6/$RERANK_PKG_NAME"
)
# The reranker's offline path is fastembed's HuggingFace hub cache layout, so the
# tar's top-level dir is this `models--…` directory (NOT the flat fastembed dir
# the embedding package uses).
RERANK_PKG_DIR="models--BAAI--bge-reranker-base"

# True when the cache already holds a ready reranker: an ONNX weights file that
# resolves (through the HF blob symlink) to a non-empty file. Mirrors
# _hf_cache_has_ready_model() so a half-extracted tree re-downloads instead of
# being trusted.
rerank_cache_ready() {
    local onnx
    for onnx in "$EMBED_CACHE_DIR/$RERANK_PKG_DIR"/snapshots/*/onnx/model.onnx; do
        if [ -f "$onnx" ] && [ -s "$onnx" ]; then
            return 0
        fi
    done
    return 1
}

# Download the 10 Gitee volumes and concatenate them, in order, into $1.
# Any missing volume fails the whole source: a partial join can only fail the
# sha256 check later, and failing here keeps the warning on the URL that broke.
fetch_reranker_parts() {
    local out="$1" i part_url part_tmp
    : > "$out" || return 1
    for i in $(seq 0 $((RERANK_PKG_PART_COUNT - 1))); do
        part_url="$(printf '%s%02d' "$RERANK_PKG_PARTS_BASE" "$i")"
        part_tmp="$out.part"
        log_info "  volume $((i + 1))/$RERANK_PKG_PART_COUNT ..."
        if ! curl -fsSL --retry 2 --connect-timeout 15 --max-time 900 \
                -o "$part_tmp" "$part_url"; then
            log_warn "Volume $i failed to download; abandoning this mirror."
            rm -f "$part_tmp"
            return 1
        fi
        cat "$part_tmp" >> "$out" || { rm -f "$part_tmp"; return 1; }
        rm -f "$part_tmp"
    done
    return 0
}

# Populate the fastembed HF cache from our own mirrors: Gitee volumes first (CN
# networks), then the whole file from GitHub. Best-effort — a non-zero return
# just means the runtime downloads it later with backoff.
fetch_reranker_model_from_release() {
    # Only the default model is packaged; a custom ECHO_RERANK_MODEL must use the
    # runtime's HF path.
    if [ "$RERANK_MODEL" != "BAAI/bge-reranker-base" ]; then
        return 1
    fi
    if rerank_cache_ready; then
        log_info "Reranker model already cached; skipping release download."
        return 0
    fi
    if ! command -v curl >/dev/null 2>&1; then
        return 1
    fi

    mkdir -p "$EMBED_CACHE_DIR"
    local tmp_tar="$EMBED_CACHE_DIR/.$RERANK_PKG_NAME.joining"
    local staging actual url ok=1

    log_info "Downloading reranker model (~941MB) from Gitee volumes ..."
    if fetch_reranker_parts "$tmp_tar"; then
        ok=0
    else
        rm -f "$tmp_tar"
        for url in "${RERANK_PKG_WHOLE_URLS[@]}"; do
            log_info "Downloading reranker model from release: $url"
            if curl -fsSL --retry 2 --connect-timeout 15 --max-time 1800 \
                    -o "$tmp_tar" "$url"; then
                ok=0
                break
            fi
            log_warn "Download failed from $url."
            rm -f "$tmp_tar"
        done
    fi
    if [ "$ok" -ne 0 ]; then
        return 1
    fi

    actual="$(file_sha256 "$tmp_tar")"
    if [ "$actual" != "$RERANK_PKG_SHA256" ]; then
        log_warn "Reranker sha256 mismatch (got ${actual:-none}); discarding download."
        rm -f "$tmp_tar"
        return 1
    fi

    # Extract to a same-disk staging dir, then swap in atomically, so an
    # interrupted extract never leaves a half-tree that rerank_cache_ready trusts.
    staging="${EMBED_CACHE_DIR:?}/.staging-rerank.$$"
    rm -rf "$staging"
    mkdir -p "$staging" || { rm -f "$tmp_tar"; return 1; }
    if ! tar -xzf "$tmp_tar" -C "$staging"; then
        log_warn "Reranker extraction failed."
        rm -rf "$staging"
        rm -f "$tmp_tar"
        return 1
    fi
    rm -f "$tmp_tar"
    rm -rf "${EMBED_CACHE_DIR:?}/$RERANK_PKG_DIR"
    if ! mv "$staging/$RERANK_PKG_DIR" "$EMBED_CACHE_DIR/$RERANK_PKG_DIR"; then
        log_warn "Reranker install failed (unexpected archive layout)."
        rm -rf "$staging"
        return 1
    fi
    rm -rf "$staging"
    log_success "Reranker model fetched from release mirror (offline-ready)."
    return 0
}

prefetch_reranker_model() {
    local venv_python="$INSTALL_DIR/venv/bin/python"
    if [ ! -x "$venv_python" ]; then
        log_warn "Skipping reranker prefetch (venv python not found)."
        return 0
    fi
    if [ -z "$RERANK_MODEL" ] || [ "${ECHO_SKIP_RERANK_PREFETCH:-0}" = "1" ]; then
        log_info "Reranker prefetch disabled."
        return 0
    fi

    # Same non-fatal contract as the embedding prefetch: this is pure
    # optimization, so the whole thing runs as an `if` condition and always
    # returns 0. `trap - ERR` alone would NOT suppress `set -e`.
    local rc=0
    if fetch_reranker_model_from_release; then
        rc=0
    else
        rc=1
    fi

    if [ "$rc" -eq 0 ]; then
        # Verify the cache actually loads offline, which is the only check that
        # proves the runtime will get a hit rather than re-downloading.
        if HF_HUB_OFFLINE=1 \
           run_with_timeout "$RERANK_PREFETCH_TIMEOUT" "$venv_python" - \
                "$RERANK_MODEL" "$EMBED_CACHE_DIR" <<'PYEOF'
import sys
model_name, cache_dir = sys.argv[1], sys.argv[2]
try:
    from fastembed.rerank.cross_encoder import TextCrossEncoder
    enc = TextCrossEncoder(
        model_name=model_name, cache_dir=cache_dir, local_files_only=True
    )
    next(iter(enc.rerank("预热", ["预热文档"])), None)
    print("ok")
except Exception as e:  # noqa: BLE001 - best-effort prefetch, never fatal
    print(f"rerank prefetch failed: {e}", file=sys.stderr)
    sys.exit(1)
PYEOF
        then
            log_success "Reranker model cached (offline-ready)."
        else
            log_warn "Reranker cache present but failed to load offline."
            log_warn "Retrieval keeps the RRF order and the runtime retries later."
        fi
    else
        log_warn "Reranker model prefetch failed or was skipped."
        log_warn "Retrieval starts in un-reranked RRF order and the runtime"
        log_warn "downloads the model on demand with backoff. To skip entirely,"
        log_warn "set memory.rerank_enabled=false in $ECHO_HOME/echo-agent.yaml."
    fi
    return 0
}

# The Dashboard SPA is built from web/ in this checkout. We deliberately do NOT
# download a prebuilt bundle: the sources are already here, so building them is
# locally verifiable, whereas fetching an artifact would mean trusting that some
# external release matches this tree. The wheel on PyPI carries a prebuilt copy
# for `pip install` users (hatch_build.py bundles web/dist into
# echo_agent/_bundled/dashboard); a source install builds its own. Those are the
# only two dashboard channels — there is intentionally no third one.
#
# web/dist is exactly where the gateway looks for a source checkout
# (echo_agent/gateway/server.py:_resolve_dashboard_dir).

build_dashboard() {
    cd "$INSTALL_DIR"

    if [ "$SKIP_DASHBOARD" = true ]; then
        log_info "Skipping Dashboard build (--skip-dashboard)."
        dashboard_skip_note
        return 0
    fi

    if [ "$HAS_NODE" = false ]; then
        log_warn "Skipping Dashboard build (no Node.js available)."
        dashboard_skip_note
        return 0
    fi

    # pnpm is the package manager this project's lockfile belongs to; npm/yarn
    # would resolve different versions. Which pnpm *major* runs matters too:
    # web/package.json pins one via "packageManager", and both the bootstrap
    # paths below honour it (corepack downloads it; a global pnpm >=10.26 does
    # the same unless the user has opted out). Before that pin existed, corepack
    # and `npm i -g pnpm` both grabbed the newest major, so users landed on
    # pnpm 11 while CI stayed on 10 — see the ERR_PNPM_IGNORED_BUILDS note in
    # web/pnpm-workspace.yaml.
    # `pnpm --version`, not `command -v pnpm`: an *existing* pnpm can be present
    # and still be unrunnable. An earlier version of this installer left pnpm 11
    # behind on hosts with Node 20 (pnpm 11 needs >=22.13, node_version_ok()
    # accepts 20), where every invocation dies with
    # ERR_UNKNOWN_BUILTIN_MODULE: node:sqlite. Probing only the pnpm we just
    # installed would skip that host straight to a frozen install that cannot
    # work, and the Dashboard would be lost with a misleading diagnosis.
    if ! pnpm --version >/dev/null 2>&1; then
        if command -v pnpm >/dev/null 2>&1; then
            log_warn "pnpm is on PATH but does not run; re-bootstrapping it."
        else
            log_info "Installing pnpm..."
        fi
        # corepack only writes a shim — the real download happens on first use,
        # so verify pnpm actually runs before relying on it. It also resolves the
        # "packageManager" pin in web/package.json, which is how a host with an
        # unrunnable pnpm 11 gets back to the version CI tests; run it from web/
        # so that pin is what the probe downloads.
        #
        # When repairing an existing pnpm this branch often can't win: corepack
        # writes its shim next to the running node, and refuses to clobber a real
        # (non-symlink) binary, so a broken pnpm installed elsewhere on PATH
        # keeps shadowing it and the probe still fails. That is why the fallback
        # below is `npm i -g`, which overwrites the broken install in place
        # (verified: pnpm 11 on Node 20 -> 10.x, same prefix).
        if command -v corepack >/dev/null 2>&1 && corepack enable pnpm >/dev/null 2>&1 \
           && (cd "$INSTALL_DIR/web" && pnpm --version >/dev/null 2>&1); then
            log_success "pnpm $(cd "$INSTALL_DIR/web" && pnpm --version) (via corepack)"
        # Pin the major here too. Plain `npm i -g pnpm` always fetches the newest
        # one, which is how the unrunnable-on-Node-20 pnpm 11 got installed in
        # the first place; PNPM_FALLBACK_VERSION is the major CI covers.
        elif npm install -g "pnpm@${PNPM_FALLBACK_VERSION}" >/dev/null 2>&1 \
             && pnpm --version >/dev/null 2>&1; then
            log_success "pnpm $(pnpm --version)"
        else
            log_warn "Could not install a working pnpm; skipping Dashboard build."
            log_info "Node.js on this host: $(node -v 2>/dev/null || echo 'not found') (pnpm 11 needs >=22.13)."
            log_info "Install pnpm manually: npm i -g pnpm@${PNPM_FALLBACK_VERSION}"
            log_info "Then: cd $INSTALL_DIR/web && pnpm install --frozen-lockfile && pnpm build"
            dashboard_skip_note
            return 0
        fi
    fi

    log_info "Building Dashboard frontend..."
    cd "$INSTALL_DIR/web"

    # --frozen-lockfile with NO fallback to a loose install. web/package.json
    # pins nothing (every dependency is a caret range), so pnpm-lock.yaml is the
    # only thing making this build reproducible. Silently retrying without it
    # would let each machine resolve different versions and produce a different
    # Dashboard — a stale lockfile is a repo bug to fix, not something to paper
    # over on the user's machine.
    #
    # Keep pnpm's output: this used to fail with no diagnosis printed and a
    # hardcoded "lockfile is out of sync" guess, which sent users chasing a
    # non-existent lockfile bug (and, worse, told them to run a loose
    # `pnpm install` that really would drift their dependency versions).
    local install_log install_rc=0
    install_log="$(mktemp)"
    run_with_timeout 300 pnpm install --frozen-lockfile \
        > >(tee -a "$install_log") 2> >(tee -a "$install_log" >&2) || install_rc=$?

    if [ "$install_rc" -ne 0 ]; then
        log_warn "pnpm install --frozen-lockfile failed (exit $install_rc); skipping Dashboard build."
        if [ "$install_rc" -eq 124 ]; then
            log_info "It timed out after 300s — usually a slow or blocked npm registry."
            log_info "Retry with: cd $INSTALL_DIR/web && pnpm install --frozen-lockfile && pnpm build"
        else
            # The shell does not wait for a process substitution to drain, so
            # the log can still be empty the instant pnpm exits — grepping it
            # right away would race and fall through to the generic message.
            # Wait for tee to produce something, then let the tail flush.
            local waited=0
            while [ ! -s "$install_log" ] && [ "$waited" -lt 20 ]; do
                sleep 0.1
                waited=$((waited + 1))
            done
            sleep 0.2

            if grep -q "ERR_PNPM_IGNORED_BUILDS" "$install_log" 2>/dev/null; then
                # A dependency added a build script that web/pnpm-workspace.yaml
                # does not approve. Nothing is broken on the user's machine, so
                # point at the real fix instead of blaming their install.
                log_info "A dependency wants to run an unapproved build script (pnpm $(pnpm --version 2>/dev/null || echo '?'))."
                log_info "This is a repo bug: the package needs an entry in web/pnpm-workspace.yaml."
                # Name the packages so the report says which ones, but keep them
                # out of the command: pnpm prints them as name@version ("Ignored
                # build scripts: esbuild@0.25.0"), and while `approve-builds
                # <pkg>` works on v11, v10.34.5 silently ignores the positional
                # arg and opens the interactive picker instead — which would hang
                # a user who pasted the line into a script. `--all` is
                # non-interactive on both majors (verified), and here "all" is
                # exactly the ignored set anyway.
                #
                # NOT `pnpm install --allow-build=<pkg>`: that flag exists only
                # on `add`/`dlx`; on `install` both majors abort with "Unknown
                # option: 'allow-build'", and the unquoted <pkg> placeholder
                # would additionally be read by the shell as a redirection.
                local ignored
                ignored="$(sed -n 's/.*Ignored build scripts: *//p' "$install_log" 2>/dev/null \
                    | tr ',' '\n' | sed 's/^ *//; s/ *$//; s/@[^@]*$//' \
                    | grep -v '^$' | sort -u | tr '\n' ' ' | sed 's/ *$//')"
                [ -n "$ignored" ] && log_info "Unapproved: $ignored"
                log_info "To build now: cd $INSTALL_DIR/web && pnpm approve-builds --all && pnpm install --frozen-lockfile && pnpm build"
            elif grep -qi "ERR_PNPM_OUTDATED_LOCKFILE\|lockfile is not up to date\|frozen-lockfile" "$install_log" 2>/dev/null; then
                log_info "web/pnpm-lock.yaml is out of sync with web/package.json — a repo bug, please report it."
            else
                log_info "See the pnpm output above for the cause."
            fi
        fi
        rm -f "$install_log"
        dashboard_skip_note
        return 0
    fi
    rm -f "$install_log"

    # `tsc -b && vite build` on a cold cache regularly exceeds two minutes.
    if run_with_timeout 600 pnpm build; then
        if [ -f "$INSTALL_DIR/web/dist/index.html" ]; then
            DASHBOARD_BUILT=true
            log_success "Dashboard built successfully"
            return 0
        fi
        log_warn "Build reported success but web/dist/index.html is missing."
    else
        log_warn "Dashboard build failed."
    fi
    log_info "Fix the issue then run: cd $INSTALL_DIR/web && pnpm build"
    dashboard_skip_note
}

# Explain what running without the SPA actually means. The gateway falls back to
# its built-in playground page rather than serving nothing, so this is a reduced
# UI — not a broken install.
dashboard_skip_note() {
    log_info "The agent and gateway still work; the web UI falls back to the"
    log_info "built-in playground at http://localhost:58123/ instead of the full Dashboard."
}

# =============================================================================
# Path & symlinks
# =============================================================================

get_command_link_dir() {
    if [ -n "$ECHO_COMMAND_LINK_DIR" ]; then
        echo "$ECHO_COMMAND_LINK_DIR"
        return 0
    fi
    if [ "$(id -u)" -eq 0 ]; then
        echo "/usr/local/bin"
        return 0
    fi
    echo "$HOME/.local/bin"
}

get_command_link_display_dir() {
    local link_dir
    link_dir="$(get_command_link_dir)"
    if [ "$link_dir" = "$HOME/.local/bin" ]; then
        # Display form only — deliberately literal, never used as a path.
        # shellcheck disable=SC2088
        echo "~/.local/bin"
        return 0
    fi
    echo "$link_dir"
}

setup_path() {
    local echo_bin="$INSTALL_DIR/venv/bin/echo-agent"
    local link_dir
    local link_display_dir
    local original_path="$PATH"

    if [ ! -x "$echo_bin" ]; then
        log_error "echo-agent entry point not found at $echo_bin"
        exit 1
    fi

    link_dir="$(get_command_link_dir)"
    link_display_dir="$(get_command_link_display_dir)"
    mkdir -p "$link_dir"
    ln -sf "$echo_bin" "$link_dir/echo-agent"
    log_success "Symlinked echo-agent -> $link_display_dir/echo-agent"

    if echo "$original_path" | tr ':' '\n' | grep -qx "$link_dir"; then
        export PATH="$link_dir:$PATH"
        log_info "$link_display_dir already on PATH"
        return 0
    fi

    LOGIN_SHELL="$(basename "${SHELL:-/bin/bash}")"
    SHELL_CONFIGS=()
    IS_FISH=false

    case "$LOGIN_SHELL" in
        zsh)
            [ -f "$HOME/.zshrc" ] && SHELL_CONFIGS+=("$HOME/.zshrc")
            [ -f "$HOME/.zprofile" ] && SHELL_CONFIGS+=("$HOME/.zprofile")
            if [ ${#SHELL_CONFIGS[@]} -eq 0 ]; then
                touch "$HOME/.zshrc"
                SHELL_CONFIGS+=("$HOME/.zshrc")
            fi
            ;;
        bash)
            [ -f "$HOME/.bashrc" ] && SHELL_CONFIGS+=("$HOME/.bashrc")
            [ -f "$HOME/.bash_profile" ] && SHELL_CONFIGS+=("$HOME/.bash_profile")
            # On most Linux distros a *login* shell (ssh, tty, display manager)
            # reads ~/.profile and never sources ~/.bashrc — Debian/Ubuntu's
            # default .bashrc even returns early for non-interactive shells. So
            # write ~/.profile too, otherwise `echo-agent` is missing from
            # exactly the sessions people log in with.
            if [ "$OS" = "linux" ] && [ ! -f "$HOME/.bash_profile" ]; then
                [ -f "$HOME/.profile" ] || touch "$HOME/.profile"
                SHELL_CONFIGS+=("$HOME/.profile")
            fi
            if [ ${#SHELL_CONFIGS[@]} -eq 0 ]; then
                touch "$HOME/.bashrc"
                SHELL_CONFIGS+=("$HOME/.bashrc")
            fi
            ;;
        fish)
            IS_FISH=true
            FISH_CONFIG="$HOME/.config/fish/config.fish"
            mkdir -p "$(dirname "$FISH_CONFIG")"
            touch "$FISH_CONFIG"
            ;;
        *)
            [ -f "$HOME/.bashrc" ] && SHELL_CONFIGS+=("$HOME/.bashrc")
            [ -f "$HOME/.zshrc" ] && SHELL_CONFIGS+=("$HOME/.zshrc")
            # Fall back to the POSIX login file so an unrecognized shell still
            # gets the command on PATH.
            if [ ${#SHELL_CONFIGS[@]} -eq 0 ]; then
                [ -f "$HOME/.profile" ] || touch "$HOME/.profile"
                SHELL_CONFIGS+=("$HOME/.profile")
            fi
            ;;
    esac

    PATH_LINE="export PATH=\"$link_dir:\$PATH\""
    for shell_config in "${SHELL_CONFIGS[@]}"; do
        # Match the exact line we would add, not just the directory anywhere in
        # the file: a substring match let an unrelated mention (a comment, or a
        # longer path containing this one) suppress the PATH export entirely.
        if ! grep -Fqx "$PATH_LINE" "$shell_config" 2>/dev/null; then
            {
                echo ""
                echo "# Echo Agent"
                echo "$PATH_LINE"
            } >> "$shell_config"
            log_success "Added $link_display_dir to PATH in $shell_config"
        fi
    done

    if [ "$IS_FISH" = true ]; then
        FISH_PATH_LINE="fish_add_path \"$link_dir\""
        if ! grep -Fqx "$FISH_PATH_LINE" "$FISH_CONFIG" 2>/dev/null; then
            {
                echo ""
                echo "# Echo Agent"
                echo "$FISH_PATH_LINE"
            } >> "$FISH_CONFIG"
            log_success "Added $link_display_dir to PATH in $FISH_CONFIG"
        fi
    fi

    export PATH="$link_dir:$PATH"
}

# =============================================================================
# Post-install setup
# =============================================================================

prepare_home() {
    mkdir -p "$ECHO_HOME"
    # The wizard writes echo-agent.yaml here with provider API keys in it. With
    # the common Linux umask of 022 the directory would be world-readable, so
    # tighten it to the owner on a shared host. Applied every run so existing
    # installs get repaired too.
    chmod 700 "$ECHO_HOME" 2>/dev/null || true
    log_success "Home directory ready: $ECHO_HOME"
}

run_setup_wizard() {
    local echo_cmd

    if [ "$RUN_SETUP" != true ]; then
        log_info "Skipping setup wizard (--skip-setup)"
        return 0
    fi

    echo_cmd="$INSTALL_DIR/venv/bin/echo-agent"
    if [ ! -x "$echo_cmd" ]; then
        return 0
    fi

    if prompt_yes_no "Run Echo Agent setup now?" "yes"; then
        # Pass -w explicitly and run from a stable cwd. Without -w the wizard
        # picks its config target by searching the current directory first, and
        # cwd here is whatever the last step left behind ($INSTALL_DIR/web after
        # a dashboard build) — which would drop echo-agent.yaml inside the
        # checkout instead of the home the service and CLI actually read.
        cd "$ECHO_HOME"
        # Never let a wizard that the user aborted (Ctrl+C, or a non-zero exit
        # in a CI shell) abort the whole installer via the ERR trap.
        if ! "$echo_cmd" setup -w "$ECHO_HOME"; then
            log_warn "Setup did not complete. Run it later with: echo-agent setup"
        fi
    else
        log_info "You can run setup later with: echo-agent setup"
    fi
}

# True when a background service can actually be registered here. WSL2 without
# `systemd=true` in /etc/wsl.conf, Docker, LXC and chroots all ship systemctl
# without a running system manager, and `systemctl --user` additionally needs a
# session bus that `sudo` strips.
service_manager_available() {
    if [ "$OS" = "macos" ]; then
        return 0   # launchd is always present
    fi
    if [ "$OS" != "linux" ]; then
        return 1
    fi
    command -v systemctl >/dev/null 2>&1 || return 1

    local state=""
    state="$(systemctl is-system-running 2>/dev/null || true)"
    case "$state" in
        ""|offline|unknown) return 1 ;;
    esac

    # Root manages a system-scope unit, which needs no session bus.
    if [ "$(id -u)" -eq 0 ]; then
        return 0
    fi
    # Non-root uses the user manager; verify its bus is reachable.
    systemctl --user show-environment >/dev/null 2>&1
}

setup_service() {
    if [ "$OS" != "linux" ] && [ "$OS" != "macos" ]; then
        return 0
    fi

    local echo_cmd="$INSTALL_DIR/venv/bin/echo-agent"
    if [ ! -x "$echo_cmd" ]; then
        return 0
    fi
    if ! "$echo_cmd" gateway --help >/dev/null 2>&1; then
        log_warn "Installed echo-agent does not support gateway service management; skipping service registration."
        log_info "Update the installed code and rerun the installer to enable service management."
        return 0
    fi

    # Bail out before touching the service when there is no manager to talk to.
    # `gateway install` exits non-zero in that case, which — under the ERR trap
    # — used to abort the installer and report a failed install even though
    # everything had in fact been installed correctly. This is the normal state
    # on WSL2 without systemd, in containers, and under plain `sudo`.
    if ! service_manager_available; then
        SERVICE_SKIPPED=true
        log_warn "No usable service manager detected; skipping background service registration."
        if [ "$IS_WSL" = true ]; then
            log_info "WSL2: enable systemd by adding to /etc/wsl.conf, then 'wsl --shutdown':"
            log_info "    [boot]"
            log_info "    systemd=true"
        elif [ "$OS" = "linux" ] && [ "$(id -u)" -ne 0 ]; then
            log_info "If you ran this under sudo, rerun it as your normal user, or use:"
            log_info "  sudo echo-agent gateway install --system"
        fi
        log_info "Meanwhile, run the gateway in the foreground: echo-agent gateway"
        log_info "  (or keep it alive with tmux/nohup)"
        return 0
    fi

    # Root on Linux owns no user-scope manager; register a system unit instead.
    local scope_args=()
    if [ "$OS" = "linux" ] && [ "$(id -u)" -eq 0 ]; then
        scope_args+=(--system)
        log_info "Running as root — registering a system-scope service."
    fi

    echo ""
    if prompt_yes_no "Register the Echo Agent gateway as a background service (auto-start on login)?" "yes"; then
        if ! "$echo_cmd" gateway install "${scope_args[@]}" -w "$ECHO_HOME"; then
            SERVICE_SKIPPED=true
            log_warn "Service registration failed; the install itself is fine."
            log_info "Retry later with: echo-agent gateway install"
            return 0
        fi
        if prompt_yes_no "Start the service now?" "yes"; then
            if ! "$echo_cmd" gateway start "${scope_args[@]}"; then
                log_warn "Could not start the service. Inspect it with: echo-agent gateway status"
            fi
        fi
        # A Linux user service stops when the login session ends unless
        # lingering is enabled — surprising for something meant to run 24/7.
        if [ "$OS" = "linux" ] && [ "$(id -u)" -ne 0 ]; then
            if ! loginctl show-user "$(id -un)" 2>/dev/null | grep -q "^Linger=yes"; then
                log_warn "The service will stop when you log out (user-scope systemd)."
                log_info "To keep it running across logouts:"
                log_info "  sudo loginctl enable-linger $(id -un)"
            fi
        fi
    else
        log_info "You can register later with: echo-agent gateway install"
    fi
}

print_success() {
    echo ""
    echo -e "${GREEN}${BOLD}"
    echo "┌─────────────────────────────────────────────────────────┐"
    echo "│                 Installation Complete                   │"
    echo "└─────────────────────────────────────────────────────────┘"
    echo -e "${NC}"
    echo ""
    echo -e "${CYAN}${BOLD}Paths:${NC}"
    echo "  Config:    $ECHO_HOME/echo-agent.yaml"
    echo "  Data:      $ECHO_HOME/data/"
    echo "  Code:      $INSTALL_DIR"
    echo ""
    echo -e "${CYAN}${BOLD}Commands:${NC}"
    echo "  echo-agent          Start CLI"
    echo "  echo-agent setup    Run setup wizard"
    echo "  echo-agent status   Show current config status"
    echo "  echo-agent gateway  Start gateway server (foreground)"
    echo "  echo-agent gateway install|start|stop|status|logs"
    echo "                      Manage the gateway as a background service"
    echo ""
    echo -e "${CYAN}${BOLD}Dashboard:${NC}"
    echo "  http://localhost:58123/"
    echo ""

    if [ "$SERVICE_SKIPPED" = true ]; then
        echo -e "${YELLOW}${BOLD}Note:${NC} The background service was not registered."
        echo "  Run the gateway in the foreground with: echo-agent gateway"
        if [ "$IS_WSL" = true ]; then
            echo "  On WSL2, enable systemd (/etc/wsl.conf -> [boot] systemd=true) to"
            echo "  register it as a service, or keep it alive with tmux/nohup."
        fi
        echo ""
    fi

    if [ "$DASHBOARD_BUILT" != true ]; then
        echo -e "${YELLOW}${BOLD}Note:${NC} The full Dashboard was not built, so the gateway"
        echo "  serves its built-in playground UI instead."
        echo "  To build it: cd $INSTALL_DIR/web && pnpm install --frozen-lockfile && pnpm build"
        echo ""
    fi

    if [ -x "$ECHO_HOME/node/bin/node" ]; then
        echo -e "${CYAN}${BOLD}Managed Node.js:${NC}"
        echo "  $ECHO_HOME/node/bin/node ($("$ECHO_HOME/node/bin/node" -v 2>/dev/null || echo 'unknown'))"
        echo ""
    fi

    echo -e "${CYAN}${BOLD}Command link:${NC}"
    echo "  $(get_command_link_dir)/echo-agent"
    echo ""
    if ! echo "$PATH" | tr ':' '\n' | grep -qx "$(get_command_link_dir)"; then
        echo -e "${CYAN}${BOLD}If the command is not available yet:${NC}"
        case "$(basename "${SHELL:-/bin/bash}")" in
            zsh) echo "  source ~/.zshrc" ;;
            fish) echo "  source ~/.config/fish/config.fish" ;;
            *) echo "  source ~/.bashrc" ;;
        esac
        echo ""
    fi
    echo "To use a project-local workspace instead of ~/.echo-agent:"
    echo "  echo-agent setup -w /path/to/workspace"
}

# =============================================================================
# Main
# =============================================================================

main() {
    print_banner
    detect_os
    check_prerequisites
    prepare_home
    check_network
    check_git
    install_uv
    check_python
    select_repo_host
    clone_repo
    setup_venv
    install_deps
    prefetch_embedding_model
    prefetch_reranker_model
    check_node
    build_dashboard
    setup_path
    run_setup_wizard
    setup_service
    print_success
}

main
