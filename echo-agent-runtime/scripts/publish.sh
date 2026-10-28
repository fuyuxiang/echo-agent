#!/bin/bash
# Build and verify before explicitly publishing the Python distributions.
set -euo pipefail
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_DIR"
bash scripts/build.sh
PYTHON="${PYTHON:-python3}"
VERSION="$("$PYTHON" -c 'import tomllib; print(tomllib.load(open("pyproject.toml", "rb"))["project"]["version"])')"
# The Runtime archive is a separate download, not a PyPI distribution.
"$PYTHON" -m twine upload "dist/echo_agent-$VERSION.tar.gz" "dist/echo_agent-$VERSION-py3-none-any.whl"
