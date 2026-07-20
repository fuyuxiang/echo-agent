#!/bin/bash
# Echo Agent installer for Linux, macOS, and WSL2.

set -e

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
REPO_URL_SSH="git@github.com:fuyuxiang/echo-agent.git"
REPO_URL_HTTPS="https://github.com/fuyuxiang/echo-agent.git"
ECHO_HOME="${ECHO_HOME:-$HOME/.echo-agent}"
INSTALL_DIR="${ECHO_INSTALL_DIR:-$ECHO_HOME/echo-agent}"
ECHO_COMMAND_LINK_DIR="${ECHO_COMMAND_LINK_DIR:-}"
PYTHON_VERSION="3.11"
NODE_VERSION="22"
BRANCH="master"
RUN_SETUP=true
# Mirror probing: measure real latency to candidate PyPI indexes and use the
# fastest one, instead of guessing by GeoIP/locale. Disable with --no-mirror-probe.
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
HAS_NODE=false
DASHBOARD_BUILT=false

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
while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-setup) RUN_SETUP=false; shift ;;
        --branch) BRANCH="$2"; shift 2 ;;
        --dir) INSTALL_DIR="$2"; shift 2 ;;
        --echo-home) ECHO_HOME="$2"; shift 2 ;;
        --no-mirror-probe) MIRROR_PROBE=false; shift ;;
        -h|--help)
            echo "Echo Agent Installer"
            echo ""
            echo "Usage: install.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --skip-setup       Skip interactive setup wizard"
            echo "  --branch NAME      Git branch to install (default: master)"
            echo "  --dir PATH         Installation directory (default: ~/.echo-agent/echo-agent)"
            echo "  --echo-home PATH   Echo home directory (default: ~/.echo-agent)"
            echo "  --no-mirror-probe  Skip PyPI mirror speed test; use uv defaults"
            echo "  -h, --help         Show this help"
            echo ""
            echo "Environment:"
            echo "  ECHO_PYPI_INDEX    Force a specific PyPI index URL (skips probing)"
            echo "  ECHO_DEPS_TIMEOUT  Dependency install timeout in seconds (default: 600)"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

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
    if command -v timeout >/dev/null 2>&1; then
        timeout "$timeout_sec" "$@"
    elif command -v gtimeout >/dev/null 2>&1; then
        gtimeout "$timeout_sec" "$@"
    else
        "$@"
    fi
}

# =============================================================================
# Pre-flight checks
# =============================================================================

print_banner() {
    echo ""
    echo -e "${MAGENTA}${BOLD}"
    echo "┌─────────────────────────────────────────────────────────┐"
    echo "│                 Echo Agent Installer                   │"
    echo "├─────────────────────────────────────────────────────────┤"
    echo "│  Self-hosted AI agent runtime for your own workspace.  │"
    echo "└─────────────────────────────────────────────────────────┘"
    echo -e "${NC}"
}

detect_os() {
    case "$(uname -s)" in
        Linux*)
            OS="linux"
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

check_network() {
    log_info "Checking network connectivity..."
    local all_ok=true

    for url in "https://pypi.org/simple/" "https://github.com" "https://nodejs.org"; do
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
    if curl -LsSf https://astral.sh/uv/install.sh | sh; then
        if [ -x "$HOME/.local/bin/uv" ]; then
            UV_CMD="$HOME/.local/bin/uv"
        elif command -v uv >/dev/null 2>&1; then
            UV_CMD="uv"
        else
            log_error "uv installed but not found on PATH."
            exit 1
        fi
        log_success "$($UV_CMD --version)"
        return 0
    fi

    log_error "Failed to install uv."
    exit 1
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
    # Resolve the latest LTS version for the major version instead of hardcoding
    # a patch release that may not exist or contain known vulnerabilities.
    local node_full_ver=""
    node_full_ver=$(curl -sSf --connect-timeout 5 "https://nodejs.org/dist/latest-v${NODE_VERSION}.x/" 2>/dev/null \
        | grep -oE "node-v${NODE_VERSION}\.[0-9]+\.[0-9]+" | head -1 | sed 's/node-//')
    if [ -z "$node_full_ver" ]; then
        node_full_ver="v${NODE_VERSION}.0.0"
        log_warn "Could not resolve latest Node ${NODE_VERSION}.x version, falling back to ${node_full_ver}"
    fi
    local base_url="https://nodejs.org/dist/${node_full_ver}"
    local pkg_name="node-${node_full_ver}-${node_os}-${arch}"
    local tmp_dir
    tmp_dir="$(mktemp -d)"

    local downloaded=false
    # Prefer .tar.xz, fallback to .tar.gz
    for ext in "tar.xz" "tar.gz"; do
        local url="${base_url}/${pkg_name}.${ext}"
        log_info "Downloading ${pkg_name}.${ext}..."
        if run_with_timeout 120 curl -fSL --progress-bar "$url" -o "$tmp_dir/node.${ext}" 2>/dev/null; then
            mkdir -p "$node_dir"
            if [ "$ext" = "tar.xz" ]; then
                tar -xJf "$tmp_dir/node.${ext}" -C "$tmp_dir"
            else
                tar -xzf "$tmp_dir/node.${ext}" -C "$tmp_dir"
            fi
            # Move contents into node_dir
            rm -rf "$node_dir"
            mv "$tmp_dir/$pkg_name" "$node_dir"
            downloaded=true
            break
        fi
    done

    rm -rf "$tmp_dir"

    if [ "$downloaded" = false ]; then
        log_warn "Failed to download Node.js. Dashboard build will be skipped."
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

clone_repo() {
    mkdir -p "$ECHO_HOME"
    log_info "Preparing repository in $INSTALL_DIR..."

    if [ -d "$INSTALL_DIR/.git" ]; then
        cd "$INSTALL_DIR"
        if [ -n "$(git status --porcelain)" ]; then
            log_warn "Local changes detected in $INSTALL_DIR; skipping update."
            log_info "Clean the repo manually if you want the installer to update it."
        else
            git fetch origin
            git checkout "$BRANCH"
            git pull --ff-only origin "$BRANCH"
            log_success "Repository updated"
        fi
        return 0
    fi

    if [ -e "$INSTALL_DIR" ]; then
        log_error "Install directory exists but is not a git repository: $INSTALL_DIR"
        exit 1
    fi

    log_info "Trying SSH clone..."
    if GIT_SSH_COMMAND="ssh -o BatchMode=yes -o ConnectTimeout=5" \
        run_with_timeout 120 git clone --branch "$BRANCH" "$REPO_URL_SSH" "$INSTALL_DIR" 2>/dev/null; then
        log_success "Cloned via SSH"
        return 0
    fi

    log_info "SSH unavailable, trying HTTPS..."
    run_with_timeout 120 git clone --branch "$BRANCH" "$REPO_URL_HTTPS" "$INSTALL_DIR"
    log_success "Cloned via HTTPS"
}

setup_venv() {
    cd "$INSTALL_DIR"
    log_info "Setting up virtual environment..."

    # Idempotent: if venv exists and Python version matches, skip rebuild
    if [ -d "venv" ] && [ -x "venv/bin/python" ]; then
        local existing_ver
        existing_ver="$(venv/bin/python --version 2>/dev/null | awk '{print $2}' || echo "")"
        local required_major_minor="${PYTHON_VERSION}"
        if echo "$existing_ver" | grep -q "^${required_major_minor}"; then
            log_success "Virtual environment already exists (Python $existing_ver)"
            return 0
        fi
        log_info "Python version mismatch ($existing_ver vs ${required_major_minor}*), rebuilding venv..."
    fi

    rm -rf venv
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
        # Numeric compare via awk (times are floats like 0.83).
        if awk "BEGIN{exit !($t < $best_time)}"; then
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

    log_info "Installing Echo Agent dependencies (full)..."
    if run_with_timeout "$DEPS_TIMEOUT" "$UV_CMD" pip install "${index_args[@]}" -e ".[all]"; then
        log_success "Dependencies installed (full)"
        return 0
    fi

    # Full extras are large (playwright, faiss-cpu, pymupdf, ...) and may time
    # out on slow links. Fall back to the essential LLM SDKs so the setup wizard
    # can still verify a model, rather than dropping to a base install with no
    # provider SDK at all.
    log_warn "Full install failed or timed out. Falling back to essential LLM providers..."
    if run_with_timeout "$DEPS_TIMEOUT" "$UV_CMD" pip install "${index_args[@]}" -e ".[openai,anthropic,gemini]"; then
        log_warn "Installed a REDUCED set (openai, anthropic, gemini)."
        log_warn "Some features (browser, documents, vector, TUI) are unavailable."
        log_info "To complete later, run:"
        log_info "  cd $INSTALL_DIR && $UV_CMD pip install --python \"$INSTALL_DIR/venv/bin/python\" -e \".[all]\""
        return 0
    fi

    log_warn "Provider install failed too. Falling back to BASE install (no LLM SDK)."
    log_warn "The setup wizard's model verification will fail until you install a provider:"
    log_warn "  cd $INSTALL_DIR && $UV_CMD pip install --python \"$INSTALL_DIR/venv/bin/python\" -e \".[openai]\"   # or .[all]"
    run_with_timeout "$DEPS_TIMEOUT" "$UV_CMD" pip install "${index_args[@]}" -e "."
    log_success "Dependencies installed (base only)"
}

# Warm the local embedding model into a STABLE cache so first-run memory vector
# search is an offline cache hit instead of a live download racing the runtime's
# per-message budget. Prefetch runs with a generous budget and lets fastembed
# fall back from the (often unreachable) HF mirror to its GCS source. This is
# best-effort: a failure here never aborts the install — the runtime still works
# in keyword-only mode and retries the download later with backoff.
EMBED_MODEL="${ECHO_EMBED_MODEL:-BAAI/bge-small-zh-v1.5}"
EMBED_CACHE_DIR="${ECHO_EMBED_CACHE_DIR:-$ECHO_HOME/models/fastembed}"
EMBED_PREFETCH_TIMEOUT="${ECHO_EMBED_PREFETCH_TIMEOUT:-900}"

# Release-hosted model package (self-owned mirrors, tried before HF/GCS).
# NOTE: tags differ in case between the two forges (Gitee v1.1.0 / GitHub V1.1.0).
EMBED_PKG_NAME="bge-small-zh-v1.5-fastembed.tar.gz"
EMBED_PKG_SHA256="d095c530b22f384d4d19a79c5862b65e8fff104af64ce9bb9e89690c186d418f"
EMBED_PKG_URLS=(
    "https://gitee.com/fuyuxiang/echo-agent/releases/download/v1.1.0/$EMBED_PKG_NAME"
    "https://github.com/fuyuxiang/echo-agent/releases/download/V1.1.0/$EMBED_PKG_NAME"
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
        actual=$(shasum -a 256 "$tmp_tar" 2>/dev/null | awk '{print $1}')
        [ -n "$actual" ] || actual=$(sha256sum "$tmp_tar" 2>/dev/null | awk '{print $1}')
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
    # Bounded by run_with_timeout as a hard ceiling; the trap is disarmed so a
    # non-zero exit here is swallowed rather than failing the whole install.
    trap - ERR
    HF_HUB_DOWNLOAD_TIMEOUT="${HF_HUB_DOWNLOAD_TIMEOUT:-15}" \
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
    local rc=$?
    trap 'on_error $LINENO' ERR

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

build_dashboard() {
    cd "$INSTALL_DIR"

    if [ "$HAS_NODE" = false ]; then
        log_warn "Skipping Dashboard build (no Node.js available)."
        log_info "Install Node.js >= 20 then run: cd $INSTALL_DIR/web && pnpm install && pnpm build"
        return 0
    fi

    # Ensure pnpm is available
    if ! command -v pnpm >/dev/null 2>&1; then
        log_info "Installing pnpm..."
        if command -v corepack >/dev/null 2>&1; then
            corepack enable pnpm 2>/dev/null || npm install -g pnpm 2>/dev/null || {
                log_warn "Cannot install pnpm, skipping Dashboard build."
                log_info "Install pnpm then run: cd $INSTALL_DIR/web && pnpm install && pnpm build"
                return 0
            }
        else
            npm install -g pnpm 2>/dev/null || {
                log_warn "Cannot install pnpm, skipping Dashboard build."
                log_info "Install pnpm then run: cd $INSTALL_DIR/web && pnpm install && pnpm build"
                return 0
            }
        fi
    fi

    log_info "Building Dashboard frontend..."
    cd "$INSTALL_DIR/web"

    if ! run_with_timeout 180 pnpm install --frozen-lockfile 2>/dev/null; then
        if ! run_with_timeout 180 pnpm install; then
            log_warn "pnpm install failed. Dashboard build skipped."
            return 0
        fi
    fi

    if run_with_timeout 120 pnpm build; then
        DASHBOARD_BUILT=true
        log_success "Dashboard built successfully"
    else
        log_warn "Dashboard build failed. The agent will work without the web UI."
        log_info "Fix issues then run: cd $INSTALL_DIR/web && pnpm build"
    fi
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
            ;;
    esac

    PATH_LINE="export PATH=\"$link_dir:\$PATH\""
    for shell_config in "${SHELL_CONFIGS[@]}"; do
        if ! grep -Fq "$link_dir" "$shell_config" 2>/dev/null; then
            echo "" >> "$shell_config"
            echo "# Echo Agent" >> "$shell_config"
            echo "$PATH_LINE" >> "$shell_config"
            log_success "Added $link_display_dir to PATH in $shell_config"
        fi
    done

    if [ "$IS_FISH" = true ]; then
        if ! grep -Fq "$link_dir" "$FISH_CONFIG" 2>/dev/null; then
            echo "" >> "$FISH_CONFIG"
            echo "# Echo Agent" >> "$FISH_CONFIG"
            echo "fish_add_path \"$link_dir\"" >> "$FISH_CONFIG"
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
        "$echo_cmd" setup
    else
        log_info "You can run setup later with: echo-agent setup"
    fi
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

    echo ""
    if prompt_yes_no "Register the Echo Agent gateway as a background service (auto-start on login)?" "yes"; then
        "$echo_cmd" gateway install -w "$ECHO_HOME"
        if prompt_yes_no "Start the service now?" "yes"; then
            "$echo_cmd" gateway start
        fi
    else
        log_info "You can register later with: echo-agent gateway install"
    fi
}

print_success() {
    echo ""
    echo -e "${GREEN}${BOLD}"
    echo "┌─────────────────────────────────────────────────────────┐"
    echo "│              Installation Complete                     │"
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

    if [ "$DASHBOARD_BUILT" != true ]; then
        echo -e "${YELLOW}${BOLD}Note:${NC} Dashboard was not built."
        echo "  To build manually: cd $INSTALL_DIR/web && pnpm install && pnpm build"
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
    check_network
    check_git
    install_uv
    check_python
    clone_repo
    setup_venv
    install_deps
    prefetch_embedding_model
    check_node
    build_dashboard
    setup_path
    prepare_home
    run_setup_wizard
    setup_service
    print_success
}

main
