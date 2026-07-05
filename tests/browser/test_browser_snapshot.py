import pytest

from echo_agent.agent.browser.snapshot import build_snapshot


class _FakePage:
    def __init__(self, tree):
        self._tree = tree

    class _AX:
        def __init__(self, tree):
            self._tree = tree

        async def snapshot(self):
            return self._tree

    @property
    def accessibility(self):
        return self._AX(self._tree)


@pytest.mark.asyncio
async def test_snapshot_numbers_interactive_elements():
    tree = {
        "role": "WebArea", "name": "page",
        "children": [
            {"role": "textbox", "name": "搜索"},
            {"role": "button", "name": "提交"},
            {"role": "text", "name": "普通文本"},
        ],
    }
    text, ref_map = await build_snapshot(_FakePage(tree))
    assert "[@e1] textbox '搜索'" in text
    assert "[@e2] button '提交'" in text
    assert "普通文本" in text  # non-interactive text still shown
    assert set(ref_map.keys()) == {"@e1", "@e2"}


@pytest.mark.asyncio
async def test_snapshot_truncates_long_content():
    children = [{"role": "button", "name": f"btn{i}"} for i in range(500)]
    tree = {"role": "WebArea", "name": "p", "children": children}
    text, ref_map = await build_snapshot(_FakePage(tree), max_chars=200)
    assert len(text) <= 260  # max_chars + truncation notice slack
    assert "截断" in text


@pytest.mark.asyncio
async def test_snapshot_empty_tree():
    text, ref_map = await build_snapshot(_FakePage(None))
    assert ref_map == {}
    assert isinstance(text, str)
