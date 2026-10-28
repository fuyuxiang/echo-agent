"""The private FastEmbed copy must work without upstream package metadata."""

import subprocess
import sys


def test_private_fastembed_does_not_import_upstream_package():
    # A clean wheel installation has no separate fastembed distribution. Block
    # upstream imports even on a developer machine that happens to have one.
    code = '''
import importlib.abc
import sys

class BlockUpstream(importlib.abc.MetaPathFinder):
    def find_spec(self, fullname, path=None, target=None):
        if fullname == "fastembed" or fullname.startswith("fastembed."):
            raise AssertionError(f"Upstream import escaped private namespace: {fullname}")
        return None

sys.meta_path.insert(0, BlockUpstream())
from echo_agent._vendor.fastembed import TextEmbedding, __version__
from echo_agent._vendor.fastembed.rerank.cross_encoder import TextCrossEncoder
from PIL import Image

assert TextEmbedding.__module__.startswith("echo_agent._vendor.fastembed.")
assert TextCrossEncoder.__module__.startswith("echo_agent._vendor.fastembed.")
assert __version__ == "0.9.0+echo.1"
assert Image.new("RGB", (2, 2)).resize((1, 1)).size == (1, 1)
'''
    result = subprocess.run(
        [sys.executable, "-c", code], capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 0, result.stderr
