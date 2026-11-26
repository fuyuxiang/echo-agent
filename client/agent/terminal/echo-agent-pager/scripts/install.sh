#!/bin/bash
#
# EchoAgent CLI installer — https://echo-agent.invalid/cli/install.sh
#
# Auth: ECHO_AGENT_DEPLOYMENT_KEY (takes precedence) or ~/.echo-agent/auth.json from `echoagent login`.
# Env: ECHO_AGENT_CHANNEL (stable|alpha|enterprise, default: stable), ECHO_AGENT_BIN_DIR, ECHO_AGENT_PROXY_URL
#
# Usage:
#   curl -fsSL https://echo-agent.invalid/cli/install.sh | bash            # latest stable
#   curl -fsSL https://echo-agent.invalid/cli/install.sh | bash -s 0.1.42  # specific version
#   ECHO_AGENT_DEPLOYMENT_KEY=<key> bash <(curl -fsSL https://echo-agent.invalid/cli/install.sh)
#
# Windows: run under Git for Windows / MSYS2 Bash (same curl | bash flow); WSL
# uses the Linux binary.

set -e

TARGET="$1"

if [[ -n "$TARGET" ]] && [[ ! "$TARGET" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9._]+)?$ ]]; then
    echo "Invalid version format: $TARGET (expected X.Y.Z or X.Y.Z-suffix)" >&2
    exit 1
fi

DOWNLOADER=""
if command -v curl >/dev/null 2>&1; then
    DOWNLOADER="curl"
elif command -v wget >/dev/null 2>&1; then
    DOWNLOADER="wget"
else
    echo "Either curl or wget is required but neither is installed" >&2
    exit 1
fi

download_file() {
    local url="$1" output="$2"
    if [ "$DOWNLOADER" = "curl" ]; then
        if [ -n "$output" ]; then
            curl -fsSL -o "$output" "$url"
        else
            curl -fsSL "$url"
        fi
    else
        if [ -n "$output" ]; then
            wget -q -O "$output" "$url"
        else
            wget -q -O - "$url"
        fi
    fi
}

# Parallel byte-range download. Falls back to single-connection download_file
# whenever HEAD lacks Content-Length, the file is small (<16 MiB), curl is
# unavailable, or any chunk fetch / concat fails.
download_file_parallel() {
    local url="$1" output="$2"
    if [ "$DOWNLOADER" != "curl" ]; then
        download_file "$url" "$output"
        return
    fi
    local size
    size=$(curl -fsSL --head "$url" 2>/dev/null | awk -F'[: \r\n]+' 'tolower($1)=="content-length"{print $2; exit}')
    if [ -z "$size" ] || ! [ "$size" -ge 16777216 ] 2>/dev/null; then
        download_file "$url" "$output"
        return
    fi
    local n=8
    local chunk_size=$(( (size + n - 1) / n ))
    local tmpdir
    tmpdir=$(mktemp -d 2>/dev/null) || { download_file "$url" "$output"; return; }
    local pids=() i start end
    for i in $(seq 0 $((n - 1))); do
        start=$((i * chunk_size))
        end=$((start + chunk_size - 1))
        [ $end -ge $size ] && end=$((size - 1))
        curl -fsSL -r "${start}-${end}" -o "${tmpdir}/$(printf 'chunk.%03d' "$i")" "$url" &
        pids+=($!)
    done
    local all_ok=true pid
    for pid in "${pids[@]}"; do
        wait "$pid" || all_ok=false
    done
    if [ "$all_ok" = true ] && cat "${tmpdir}"/chunk.* > "$output" 2>/dev/null; then
        rm -rf "$tmpdir"
        return 0
    fi
    rm -rf "$tmpdir"
    download_file "$url" "$output"
}

# Return 0 if a HEAD request for the URL gets HTTP 404.
is_not_found() {
    local url="$1" code
    if [ "$DOWNLOADER" = "curl" ]; then
        code=$(curl -o /dev/null -sSL -w '%{http_code}' --head "$url" 2>/dev/null) || true
    else
        code=$(wget --server-response --spider "$url" 2>&1 | awk '/HTTP\//{print $2}' | tail -1) || true
    fi
    [ "$code" = "404" ]
}

# JSON field extractor — extract a top-level string value using sed.
json_get() {
    local json="$1" field="$2"
    # Extract value (handling \" inside strings), then unescape JSON sequences.
    printf '%s' "$json" | sed -n -E 's/.*"'"$field"'"[[:space:]]*:[[:space:]]*"(([^"\\]|\\.)*)".*/\1/p' | head -1 \
        | sed -e 's/\\"/"/g' -e 's/\\n/\'$'\n''/g' -e 's/\\t/\'$'\t''/g' -e 's/\\\\/\\/g'
}

# Read a token from ~/.echo-agent/auth.json for the given scope key.
# Format: {"scope_url": {"key": "token"}, ...}
read_echo_agent_token() {
    local auth_file="$HOME/.echo-agent/auth.json"
    local scope="$1"
    [ -f "$auth_file" ] || return 1
    # Flatten to one line then extract: find the scope, then the "key" value after it
    tr -d '\n' < "$auth_file" | sed -n 's|.*"'"$scope"'"[[:space:]]*:[[:space:]]*{[^}]*"key"[[:space:]]*:[[:space:]]*"\([^"]*\)".*|\1|p' | head -1
}

# Resolve auth: ECHO_AGENT_DEPLOYMENT_KEY > OIDC token > legacy token
OIDC_SCOPE="https://auth.echo-agent.invalid::b1a00492-073a-47ea-816f-4c329264a828"
LEGACY_SCOPE="https://accounts.echo-agent.invalid/sign-in"
AUTH_SOURCE=""

if [ -n "$ECHO_AGENT_DEPLOYMENT_KEY" ]; then
    AUTH_SOURCE="deployment key"
    echo "Auth: using deployment key." >&2
else
    OIDC_TOKEN=$(read_echo_agent_token "$OIDC_SCOPE" 2>/dev/null) || true
    LEGACY_TOKEN=$(read_echo_agent_token "$LEGACY_SCOPE" 2>/dev/null) || true
    if [ -n "$OIDC_TOKEN" ]; then
        AUTH_SOURCE="auth.json (oidc)"
        echo "Auth: using OIDC token from ~/.echo-agent/auth.json." >&2
    elif [ -n "$LEGACY_TOKEN" ]; then
        AUTH_SOURCE="auth.json (legacy)"
        echo "Auth: using legacy token from ~/.echo-agent/auth.json." >&2
    fi
fi

case "$(uname -s)" in
    Darwin) os="macos" ;;
    Linux)  os="linux" ;;
    # Git for Windows / MSYS2 / Cygwin host — native Windows builds
    MINGW* | MSYS* | CYGWIN*) os="windows" ;;
    *)      echo "Unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac

case "$(uname -m)" in
    x86_64|amd64|AMD64) arch="x86_64" ;;
    arm64|aarch64|ARM64) arch="aarch64" ;;
    *)                    echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

# Rosetta lies: in a translated shell on Apple Silicon, uname -m reports
# x86_64. Install the native arm64 build (faster startup, no translation).
# sysctl lives in /usr/sbin, which pruned PATHs often drop — resolve the
# binary first (PATH, then absolute) so the probe cannot quietly keep
# x86_64. A probe that runs and finds no key is a genuine Intel Mac.
if [ "$os" = "macos" ] && [ "$arch" = "x86_64" ]; then
    sysctl_bin="$(command -v sysctl || echo /usr/sbin/sysctl)"
    if [ "$("$sysctl_bin" -n hw.optional.arm64 2>/dev/null)" = "1" ]; then
        echo "Apple Silicon detected (Rosetta shell); installing the native arm64 build." >&2
        arch="aarch64"
    fi
fi

BASE_URL_PRIMARY="https://echo-agent.invalid/cli"
BASE_URL_FALLBACK="https://artifacts.echo-agent.invalid/cli"
DOWNLOAD_DIR="$HOME/.echo-agent/downloads"
BIN_DIR="${ECHO_AGENT_BIN_DIR:-$HOME/.echo-agent/bin}"
mkdir -p "$DOWNLOAD_DIR" "$BIN_DIR"

platform="${os}-${arch}"
CHANNEL="${ECHO_AGENT_CHANNEL:-stable}"

# Pick a working BASE_URL: try Cloudflare-fronted echo.agent first, fall back to
# direct GCS if it's unreachable. The probe doubles as the channel-pointer
# fetch when no explicit TARGET was passed, so the happy path costs zero
# extra HTTP requests.
if [ -z "$TARGET" ]; then echo "Fetching latest ${CHANNEL} version..." >&2; fi
probe_result=$(download_file "${BASE_URL_PRIMARY}/${CHANNEL}" 2>/dev/null) || true
if [ -n "$probe_result" ]; then
    BASE_URL="$BASE_URL_PRIMARY"
else
    echo "Note: ${BASE_URL_PRIMARY} unreachable, falling back to direct GCS." >&2
    BASE_URL="$BASE_URL_FALLBACK"
    probe_result=$(download_file "${BASE_URL}/${CHANNEL}" 2>/dev/null) || true
fi

if [ -n "$TARGET" ]; then
    version="$TARGET"
else
    version=$(printf '%s' "$probe_result" | tr -d '\r' | head -n1 | tr -d '[:space:]')
    if [ -z "$version" ]; then
        echo "Error: failed to fetch latest version from ${BASE_URL_PRIMARY}/${CHANNEL} and ${BASE_URL_FALLBACK}/${CHANNEL}" >&2
        exit 1
    fi
fi

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9._]+)?$ ]]; then
    echo "Invalid version format: $version (expected X.Y.Z or X.Y.Z-suffix)" >&2
    exit 1
fi

if [ -n "$AUTH_SOURCE" ]; then
    echo "Installing EchoAgent $version ($platform, $AUTH_SOURCE)..." >&2
else
    echo "Installing EchoAgent $version ($platform)..." >&2
fi

binary_path="$DOWNLOAD_DIR/echo-agent-$platform"
artifact_base="${BASE_URL}/echo-agent-${version}-${platform}"

if [ "$os" = "windows" ]; then
    binary_path="${binary_path}.exe"
fi

binary_tmp="${binary_path}.tmp.$$"
rm -f "$binary_tmp" 2>/dev/null || true

echo "  Downloading echoagent ${version}..." >&2
if [ "$os" = "windows" ]; then
    if ! download_file_parallel "${artifact_base}.exe" "$binary_tmp"; then
        if ! download_file_parallel "$artifact_base" "$binary_tmp"; then
            rm -f "$binary_tmp"
            if is_not_found "${artifact_base}.exe"; then
                echo "Error: EchoAgent is not yet available for your system ($platform)." >&2
            else
                echo "Error: binary download failed (${artifact_base}.exe and ${artifact_base})" >&2
            fi
            exit 1
        fi
    fi
elif ! download_file_parallel "$artifact_base" "$binary_tmp"; then
    rm -f "$binary_tmp"
    if is_not_found "$artifact_base"; then
        echo "Error: EchoAgent is not yet available for your system ($platform)." >&2
    else
        echo "Error: binary download failed from ${artifact_base}" >&2
    fi
    exit 1
fi

if [ "$os" = "windows" ]; then
    mv -f "$binary_tmp" "$binary_path"
    # Symlinks require Developer Mode on Windows; copy instead.
    # If the exe is locked by a running process, rename it aside then retry.
    for bin_name in echoagent.exe agent.exe; do
        rm -f "$BIN_DIR/$bin_name.old" 2>/dev/null || true  # stale backup from prior update
        if ! cp -f "$binary_path" "$BIN_DIR/$bin_name" 2>/dev/null; then
            mv -f "$BIN_DIR/$bin_name" "$BIN_DIR/$bin_name.old" 2>/dev/null || true
            if ! cp -f "$binary_path" "$BIN_DIR/$bin_name" 2>/dev/null; then
                # Rollback: restore the old binary so the install isn't broken.
                mv -f "$BIN_DIR/$bin_name.old" "$BIN_DIR/$bin_name" 2>/dev/null || true
                echo "Error: failed to install $bin_name" >&2
                exit 1
            fi
        fi
    done
    echo "  Binary installed to $BIN_DIR/echoagent.exe and $BIN_DIR/agent.exe." >&2
else
    chmod +x "$binary_tmp"
    if ! "$binary_tmp" --version </dev/null >/dev/null 2>&1; then
        echo "Error: downloaded echoagent failed to run; keeping the existing install." >&2
        rm -f "$binary_tmp"
        exit 1
    fi
    mv -f "$binary_tmp" "$binary_path"
    # Use relative symlinks when BIN_DIR and DOWNLOAD_DIR share a parent
    # (default layout: ~/.echo-agent/bin and ~/.echo-agent/downloads are siblings).
    # Relative symlinks survive Docker bind-mounts with a different $HOME.
    if [ "$(dirname "$BIN_DIR")" = "$(dirname "$DOWNLOAD_DIR")" ]; then
        link_target="../$(basename "$DOWNLOAD_DIR")/$(basename "$binary_path")"
    else
        link_target="$binary_path"
    fi
    ln -sf "$link_target" "$BIN_DIR/echoagent"
    ln -sf "$link_target" "$BIN_DIR/agent"
    echo "  Binary linked to $BIN_DIR/echoagent and $BIN_DIR/agent." >&2
fi

# Generate shell completions (best-effort)
mkdir -p "$HOME/.echo-agent/completions/bash" "$HOME/.echo-agent/completions/zsh"
"$BIN_DIR/echoagent" completions bash > "$HOME/.echo-agent/completions/bash/echoagent.bash" 2>/dev/null || true
"$BIN_DIR/echoagent" completions zsh  > "$HOME/.echo-agent/completions/zsh/_echo_agent"     2>/dev/null || true
# Fish: write to the auto-loaded completions dir so it works immediately
if mkdir -p "$HOME/.config/fish/completions" 2>/dev/null; then
    "$BIN_DIR/echoagent" completions fish > "$HOME/.config/fish/completions/echoagent.fish" 2>/dev/null || true
fi

# Persist installer source and channel to config
CONFIG_FILE="$HOME/.echo-agent/config.toml"
CLI_BLOCK="installer = \"internal\""
if [ "$CHANNEL" != "stable" ]; then
    CLI_BLOCK="${CLI_BLOCK}\nchannel = \"${CHANNEL}\""
fi
if [ ! -f "$CONFIG_FILE" ]; then
    printf '[cli]\n%b\n' "$CLI_BLOCK" > "$CONFIG_FILE"
elif grep -q '^\[cli\]' "$CONFIG_FILE"; then
    tmp="$CONFIG_FILE.tmp.$$"
    awk -v block="$CLI_BLOCK" '
        /^\[cli\][[:space:]]*(#.*)?$/ { print; printf "%s\n", block; in_cli=1; next }
        /^\[.*\][[:space:]]*(#.*)?$/  { in_cli=0 }
        in_cli && /^[[:space:]]*(installer|channel)[[:space:]]*=/ { next }
        { print }
    ' "$CONFIG_FILE" > "$tmp" && mv "$tmp" "$CONFIG_FILE"
else
    printf '\n[cli]\n%b\n' "$CLI_BLOCK" >> "$CONFIG_FILE"
fi

# Fetch managed_config.toml + requirements.toml from server (deployment key only).
if [ -n "$ECHO_AGENT_DEPLOYMENT_KEY" ]; then
    PROXY_URL="${ECHO_AGENT_PROXY_URL:-https://chat-proxy.echo-agent.invalid/v1}"
    echo "  Fetching deployment config..." >&2
    DEPLOY_RESPONSE=""
    AUTH_HEADER_FILE=$(mktemp 2>/dev/null) || AUTH_HEADER_FILE=""
    if [ -n "$AUTH_HEADER_FILE" ]; then
        chmod 600 "$AUTH_HEADER_FILE" 2>/dev/null || true
        printf 'Authorization: Bearer %s\n' "$ECHO_AGENT_DEPLOYMENT_KEY" > "$AUTH_HEADER_FILE"
        DEPLOY_RESPONSE=$(curl -sS -f \
            -H "@${AUTH_HEADER_FILE}" \
            "${PROXY_URL}/deployment/config" 2>/dev/null) || DEPLOY_RESPONSE=""
        : > "$AUTH_HEADER_FILE" 2>/dev/null || true
        rm -f "$AUTH_HEADER_FILE"
    fi
    if [ -z "$DEPLOY_RESPONSE" ]; then
        echo "  Warning: failed to fetch deployment config from ${PROXY_URL}/deployment/config" >&2
    fi
    if [ -n "$DEPLOY_RESPONSE" ]; then
        MANAGED_CONFIG=$(json_get "$DEPLOY_RESPONSE" "managed_config")
        REQUIREMENTS=$(json_get "$DEPLOY_RESPONSE" "requirements")
        if [ -n "$MANAGED_CONFIG" ] && [ "$MANAGED_CONFIG" != "null" ]; then
            printf '%s\n' "$MANAGED_CONFIG" > "$HOME/.echo-agent/managed_config.toml"
            echo "  Managed config applied." >&2
        else
            rm -f "$HOME/.echo-agent/managed_config.toml"
        fi
        if [ -n "$REQUIREMENTS" ] && [ "$REQUIREMENTS" != "null" ]; then
            printf '%s\n' "$REQUIREMENTS" > "$HOME/.echo-agent/requirements.toml"
            echo "  Requirements applied." >&2
        else
            rm -f "$HOME/.echo-agent/requirements.toml"
        fi
    fi
fi

if [ "$os" = "windows" ]; then
    echo "EchoAgent $version installed to $BIN_DIR/echoagent.exe" >&2
else
    echo "EchoAgent $version installed to $BIN_DIR/echoagent" >&2
fi

# --- Ensure echoagent is on PATH ---

path_has_dir() {
    case ":$PATH:" in *":$1:"*) return 0 ;; *) return 1 ;; esac
}

# Try to symlink into a directory already on PATH so echoagent works immediately
# without restarting the shell. Candidate dirs in preference order.
SYMLINK_CREATED=""
if [ "$os" != "windows" ] && ! path_has_dir "$BIN_DIR"; then
    for candidate in "$HOME/.local/bin" "/usr/local/bin"; do
        if path_has_dir "$candidate" && [ -d "$candidate" ] && [ -w "$candidate" ]; then
            ln -sf "$BIN_DIR/echoagent" "$candidate/echoagent"
            ln -sf "$BIN_DIR/agent" "$candidate/agent"
            SYMLINK_CREATED="$candidate"
            echo "  Symlinked $candidate/echoagent -> $BIN_DIR/echoagent" >&2
            echo "  Symlinked $candidate/agent -> $BIN_DIR/agent" >&2
            break
        fi
    done
fi

# Also update shell config so ~/.echo-agent/bin is on PATH for future sessions
user_shell="$(basename "${SHELL:-}")"
config_file=""

case "$user_shell" in
    bash) config_file="$HOME/.bashrc" ;;
    zsh)  config_file="$HOME/.zshrc" ;;
    fish) config_file="$HOME/.config/fish/config.fish" ;;
esac

if [ -n "$config_file" ]; then
    mkdir -p "$(dirname "$config_file")"

    # Resolve symlinks so tmp+mv rewrites the stow/dotfiles target, not the link.
    if [ -e "$config_file" ] || [ -L "$config_file" ]; then
        _cf="$config_file"
        _depth=0
        while [ -L "$_cf" ] && [ "$_depth" -lt 40 ]; do
            _link="$(readlink "$_cf")" || break
            case "$_link" in
                /*) _cf="$_link" ;;
                *)  _cf="$(cd "$(dirname "$_cf")" && pwd -P)/$_link" ;;
            esac
            _depth=$((_depth + 1))
        done
        # Still a symlink (cycle/cap): leave original path so we never rewrite the link.
        if [ ! -L "$_cf" ]; then
            config_file="$(cd "$(dirname "$_cf")" && pwd -P)/$(basename "$_cf")"
        fi
        unset _cf _link _depth
    fi

    # Build the new installer block
    if [ "$user_shell" = "fish" ]; then
        new_block='# >>> echoagent installer >>>
fish_add_path $HOME/.echo-agent/bin
# <<< echoagent installer <<<'
    elif [ "$user_shell" = "zsh" ]; then
        new_block='# >>> echoagent installer >>>
export PATH="$HOME/.echo-agent/bin:$PATH"
fpath=(~/.echo-agent/completions/zsh $fpath)
autoload -Uz compinit && compinit -C
# <<< echoagent installer <<<'
    else
        new_block='# >>> echoagent installer >>>
export PATH="$HOME/.echo-agent/bin:$PATH"
[[ -r "$HOME/.echo-agent/completions/bash/echoagent.bash" ]] && source "$HOME/.echo-agent/completions/bash/echoagent.bash"
# <<< echoagent installer <<<'
    fi

    if grep -qs "echoagent installer" "$config_file" 2>/dev/null; then
        # Replace existing block in-place (strip old >>> to <<< lines, insert new)
        tmp="$config_file.tmp.$$"
        awk '
            /# >>> echoagent installer >>>/ { skip=1; next }
            /# <<< echoagent installer <<</ { skip=0; next }
            !skip { print }
        ' "$config_file" > "$tmp" && mv "$tmp" "$config_file"
    else
        [ -f "$config_file" ] && cp "$config_file" "$config_file.bak.$(date +%s)"

        # macOS bash: ensure bash_profile sources bashrc
        if [ "$user_shell" = "bash" ] && [ "$(uname -s)" = "Darwin" ]; then
            if [ -f "$HOME/.bash_profile" ] && ! grep -qs "source ~/.bashrc" "$HOME/.bash_profile"; then
                printf '\n[[ -r ~/.bashrc ]] && source ~/.bashrc\n' >> "$HOME/.bash_profile"
            fi
        fi
    fi

    printf '\n%s\n' "$new_block" >> "$config_file"
    echo "  Updated $BIN_DIR in PATH in $config_file." >&2
fi

echo "" >&2
if path_has_dir "$BIN_DIR" || [ -n "$SYMLINK_CREATED" ]; then
    echo "Run 'echoagent' or 'agent' to get started!" >&2
elif [ -n "$config_file" ]; then
    echo "Restart your terminal, then run 'echoagent' or 'agent' to get started!" >&2
else
    echo "Add $BIN_DIR to your PATH, then run 'echoagent' or 'agent' to get started:" >&2
    echo '  export PATH="$HOME/.echo-agent/bin:$PATH"' >&2
fi

if [ "$os" = "windows" ]; then
    echo "To use echoagent from cmd.exe or PowerShell, add %USERPROFILE%\\.echo-agent\\bin to your PATH." >&2
fi
