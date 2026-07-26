#!/bin/bash
# Publish Echo Agent to PyPI with Dashboard pre-built.
# Usage: bash scripts/publish.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_DIR"

echo "==> Building Dashboard frontend..."
cd web
# Strict: a release must be built from the exact dependency versions in the
# lockfile. The old `|| pnpm install` fallback hid lockfile drift (and, with
# 2>/dev/null, the error explaining it) behind a resolve that could pull
# different versions than CI tested.
pnpm install --frozen-lockfile
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

echo "==> Verifying the built artifacts bundle the Dashboard..."
python3 - <<'PY'
import sys, tarfile, zipfile
from pathlib import Path

MARKER = "echo_agent/_bundled/dashboard/index.html"
failures = []

wheels = sorted(Path("dist").glob("*.whl"))
sdists = sorted(Path("dist").glob("*.tar.gz"))
if not wheels or not sdists:
    sys.exit(f"ERROR: expected a wheel and an sdist in dist/, found {wheels + sdists}")

for whl in wheels:
    with zipfile.ZipFile(whl) as z:
        if MARKER not in z.namelist():
            failures.append(f"{whl.name} is missing {MARKER}")

for sd in sdists:
    with tarfile.open(sd) as t:
        # sdist entries are prefixed with the "<name>-<version>/" root dir.
        names = {n.split("/", 1)[-1] for n in t.getnames()}
        if MARKER not in names:
            failures.append(f"{sd.name} is missing {MARKER}")
        stray = [n for n in names if "node_modules" in n]
        if stray:
            failures.append(
                f"{sd.name} contains {len(stray)} node_modules entries, "
                f"e.g. {stray[0]}"
            )

if failures:
    sys.exit("ERROR: " + "\nERROR: ".join(failures))
print("    OK: Dashboard bundled in " + ", ".join(p.name for p in wheels + sdists))
PY

echo "==> Publishing to PyPI..."
hatch publish

echo ""
echo "Done! Package published with Dashboard bundled."
