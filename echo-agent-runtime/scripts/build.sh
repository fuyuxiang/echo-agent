#!/bin/bash
# Build verified wheel, sdist and a standalone Runtime source archive. Never publishes.
set -euo pipefail
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_DIR"
PYTHON="${PYTHON:-python3}"

if [ "${1:-}" != "--prebuilt-dashboard" ]; then
    if [ "$#" -ne 0 ]; then
        echo "Usage: bash scripts/build.sh [--prebuilt-dashboard]" >&2
        exit 2
    fi
    (cd web && pnpm install --frozen-lockfile && pnpm build)
elif [ "$#" -ne 1 ]; then
    echo "Usage: bash scripts/build.sh [--prebuilt-dashboard]" >&2
    exit 2
fi
if [ ! -f web/dist/index.html ] && [ ! -f echo_agent/_bundled/dashboard/index.html ]; then
    echo "Dashboard assets are missing; build web/ first." >&2
    exit 1
fi

# Never allow a local escape hatch to produce an incomplete release.
unset ECHO_ALLOW_NO_DASHBOARD
"$PYTHON" -m build
"$PYTHON" scripts/build-runtime-archive.py
"$PYTHON" scripts/verify-distributions.py
