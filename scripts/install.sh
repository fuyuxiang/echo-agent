#!/bin/bash
# Echo Agent installer for Linux, macOS, and WSL2.

set -e

# --- Environment sanitization ---
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
        -h|--help)
            echo "Echo Agent Installer"
            echo ""
            echo "Usage: install.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --skip-setup      Skip interactive setup wizard"
            echo "  --branch NAME     Git branch to install (default: master)"
            echo "  --dir PATH        Installation directory (default: ~/.echo-agent/echo-agent)"
            echo "  --echo-home PATH  Echo home directory (default: ~/.echo-agent)"
            echo "  -h, --help        Show this help"
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

install_deps() {
    cd "$INSTALL_DIR"
    export VIRTUAL_ENV="$INSTALL_DIR/venv"
    log_info "Installing Echo Agent dependencies..."
    if ! run_with_timeout 300 "$UV_CMD" pip install -e ".[all]"; then
        log_warn "Full install failed, falling back to base install."
        run_with_timeout 300 "$UV_CMD" pip install -e "."
    fi
    log_success "Dependencies installed"
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
    check_node
    build_dashboard
    setup_path
    prepare_home
    run_setup_wizard
    setup_service
    print_success
}

main
