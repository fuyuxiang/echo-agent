#!/bin/bash
# Compatibility entry point for existing raw download links and local callers.
set -e
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCAL_INSTALLER="$SCRIPT_DIR/../echo-agent-runtime/scripts/install.sh"
if [ -f "$LOCAL_INSTALLER" ]; then
    exec bash "$LOCAL_INSTALLER" "$@"
fi

# A downloaded bootstrap has no neighbouring source tree. Fetch only the
# installer; it obtains a sparse Runtime checkout using the selected host.
BRANCH="master"
REPO_HOST="${ECHO_REPO_HOST:-github}"
HOST_EXPLICIT="${ECHO_REPO_HOST:-}"
LOCAL_SOURCE="${ECHO_SOURCE_DIR:-}"
ARGS=("$@")
for ((i=0; i<${#ARGS[@]}; i++)); do
    case "${ARGS[$i]}" in
        --branch) BRANCH="${ARGS[$((i+1))]:-master}" ;;
        --repo) REPO_HOST="${ARGS[$((i+1))]:-github}"; HOST_EXPLICIT="$REPO_HOST" ;;
        --source) LOCAL_SOURCE="${ARGS[$((i+1))]:-}" ;;
    esac
done
if [ -n "$LOCAL_SOURCE" ]; then
    if [ -f "$LOCAL_SOURCE/echo-agent-runtime/scripts/install.sh" ]; then
        exec bash "$LOCAL_SOURCE/echo-agent-runtime/scripts/install.sh" "$@"
    elif [ -f "$LOCAL_SOURCE/scripts/install.sh" ] && [ -f "$LOCAL_SOURCE/echo_agent/app.py" ]; then
        exec bash "$LOCAL_SOURCE/scripts/install.sh" "$@"
    fi
fi
case "$REPO_HOST" in
    gitee|github) ;;
    *) echo "--repo / ECHO_REPO_HOST must be github or gitee" >&2; exit 1 ;;
esac
TEMP_INSTALLER="$(mktemp)"
trap 'rm -f "$TEMP_INSTALLER"' EXIT
HOSTS=("$REPO_HOST")
if [ -z "$HOST_EXPLICIT" ]; then HOSTS+=("gitee"); fi
for HOST in "${HOSTS[@]}"; do
    case "$HOST" in
        gitee) URL="https://gitee.com/fuyuxiang/echo-agent/raw/$BRANCH/echo-agent-runtime/scripts/install.sh" ;;
        github) URL="https://raw.githubusercontent.com/fuyuxiang/echo-agent/$BRANCH/echo-agent-runtime/scripts/install.sh" ;;
    esac
    if curl --fail --show-error --silent --location --connect-timeout 10 --max-time 120 "$URL" -o "$TEMP_INSTALLER"; then
        bash "$TEMP_INSTALLER" "$@"
        exit $?
    fi
done
echo "Unable to download the Runtime installer from ${HOSTS[*]}." >&2
exit 1
