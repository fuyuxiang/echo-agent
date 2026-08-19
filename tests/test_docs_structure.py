"""CI guard: verify documentation structure consistency.

Checks that expected documentation files exist, Chinese/English pairs match,
and key content stays aligned with code.
"""
from __future__ import annotations

from pathlib import Path

import pytest

DOCS_DIR = Path(__file__).resolve().parent.parent / "docs"

REQUIRED_SECTIONS = [
    "index.md",
    "getting-started/index.md",
    "getting-started/installation.md",
    "getting-started/quickstart.md",
    "guides/index.md",
    "guides/models/index.md",
    "concepts/index.md",
    "concepts/architecture.md",
    "integrations/index.md",
    "integrations/channels/index.md",
    "operations/index.md",
    "operations/troubleshooting.md",
    "reference/index.md",
    "reference/cli.md",
    "reference/configuration-guide.md",
    "reference/glossary.md",
    "development/index.md",
    "development/setup.md",
]


@pytest.mark.parametrize("rel_path", REQUIRED_SECTIONS)
def test_required_doc_exists(rel_path: str):
    path = DOCS_DIR / rel_path
    assert path.is_file(), f"必需文档缺失: docs/{rel_path}"


def _zh_en_pairs():
    """Yield (zh_path, en_path) for all .md files that should have .en.md counterparts."""
    for md in DOCS_DIR.rglob("*.md"):
        if md.name.endswith(".en.md"):
            continue
        if "superpowers" in md.parts or "includes" in md.parts:
            continue
        en = md.with_suffix("").with_suffix(".en.md")
        if en.exists():
            yield md, en


def test_zh_en_pairs_have_matching_h1():
    """Chinese and English docs should both have a top-level heading."""
    missing_h1 = []
    for zh, en in _zh_en_pairs():
        for path in (zh, en):
            content = path.read_text(encoding="utf-8")
            if not any(line.startswith("# ") for line in content.splitlines()[:5]):
                missing_h1.append(str(path.relative_to(DOCS_DIR)))
    assert not missing_h1, f"以下文档缺少 H1 标题: {missing_h1}"


def test_channel_count_consistent():
    """Channel index should mention all 14 adapters."""
    from echo_agent.channels import manager  # noqa: E402

    channel_index = DOCS_DIR / "integrations" / "channels" / "index.md"
    if not channel_index.is_file():
        pytest.skip("channels index not yet created")
    content = channel_index.read_text(encoding="utf-8").lower()
    expected = ["telegram", "discord", "slack", "weixin", "wecom",
                "feishu", "dingtalk", "qqbot", "whatsapp", "email",
                "matrix", "webhook", "cli", "cron"]
    missing = [ch for ch in expected if ch not in content]
    assert not missing, f"Channel 总览页缺少: {missing}"
