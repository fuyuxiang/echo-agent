#!/bin/bash
# Publish Echo Agent to PyPI with Dashboard pre-built.
# Usage: bash scripts/publish.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_DIR"

echo "==> Building Dashboard frontend..."
cd web
pnpm install --frozen-lockfile 2>/dev/null || pnpm install
pnpm build
cd "$PROJECT_DIR"

echo "==> Verifying web/dist/ exists..."
if [ ! -f "web/dist/index.html" ]; then
    echo "ERROR: web/dist/index.html not found. Frontend build failed."
    exit 1
fi

echo "==> Cleaning previous builds..."
rm -rf dist/

echo "==> Building Python package (sdist + wheel)..."
hatch build

echo "==> Publishing to PyPI..."
hatch publish

echo ""
echo "Done! Package published with Dashboard bundled."
