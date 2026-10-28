"""An explicitly empty model environment value disables installation prefetch."""

import os
import subprocess
from pathlib import Path


def test_empty_model_overrides_skip_prefetch_without_download(tmp_path):
    root = Path(__file__).resolve().parents[1]
    source = (root / "scripts/install.sh").read_text()
    body, marker, trailer = source.rpartition("\nmain\n")
    assert marker and not trailer.strip()
    installer = tmp_path / "functions.sh"
    installer.write_text(body)
    python = tmp_path / "install/venv/bin/python"
    python.parent.mkdir(parents=True)
    python.write_text("#!/bin/sh\nexit 77\n")
    python.chmod(0o755)
    script = r'''
set --
source "$TEST_INSTALLER"
INSTALL_DIR="$TEST_INSTALL"
EMBED_CACHE_DIR="$TEST_INSTALL/cache"
fetch_embedding_model_from_release() { echo 'unexpected embedding download'; return 1; }
fetch_reranker_model_from_release() { echo 'unexpected reranker download'; return 1; }
prefetch_embedding_model
prefetch_reranker_model
'''
    result = subprocess.run(
        ["bash", "-c", script], capture_output=True, text=True, timeout=15,
        env={**os.environ, "ECHO_EMBED_MODEL": "", "ECHO_RERANK_MODEL": "",
             "TEST_INSTALLER": str(installer), "TEST_INSTALL": str(tmp_path / "install")},
    )
    assert result.returncode == 0, result.stderr
    assert "unexpected" not in result.stdout
    assert "disabled" in result.stdout
