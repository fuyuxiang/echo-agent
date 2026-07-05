import pytest

from echo_agent.agent.browser.snapshot import (
    build_snapshot,
    build_snapshot_with_locators,
)


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


class _FakeLocatorRoleQuery:
    """Records the (role, name, nth) it was built from so a test can assert the
    ref map lines up with AX-tree order."""

    def __init__(self, role, name):
        self.role = role
        self.name = name
        self.index = None

    def nth(self, k):
        self.index = k
        return self


class _RolePage:
    """Fake page whose get_by_role hands back a locator that remembers how it
    was constructed, so we can verify @eN → locator ordering."""

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

    def get_by_role(self, role, name=None):
        return _FakeLocatorRoleQuery(role, name)


@pytest.mark.asyncio
async def test_locators_follow_ax_order_with_css_missed_role():
    # A 'radio' sits between two buttons. The old positional query_selector_all
    # scheme did not select radios, so every later ref shifted by one. The
    # single-traversal builder must map each @eN to a locator for the SAME node.
    tree = {
        "role": "WebArea", "name": "p",
        "children": [
            {"role": "button", "name": "ok"},
            {"role": "radio", "name": "opt-a"},
            {"role": "button", "name": "cancel"},
        ],
    }
    text, ref_map = await build_snapshot_with_locators(_RolePage(tree))

    assert "[@e1] button 'ok'" in text
    assert "[@e2] radio 'opt-a'" in text
    assert "[@e3] button 'cancel'" in text
    assert set(ref_map.keys()) == {"@e1", "@e2", "@e3"}

    # each ref resolves to the exact (role, name) of the AX node at that position
    assert (ref_map["@e1"].role, ref_map["@e1"].name) == ("button", "ok")
    assert (ref_map["@e2"].role, ref_map["@e2"].name) == ("radio", "opt-a")
    assert (ref_map["@e3"].role, ref_map["@e3"].name) == ("button", "cancel")


@pytest.mark.asyncio
async def test_locators_disambiguate_repeated_role_name_with_nth():
    # Two identical (role, name) pairs must get distinct nth() indices.
    tree = {
        "role": "WebArea", "name": "p",
        "children": [
            {"role": "button", "name": "go"},
            {"role": "button", "name": "go"},
        ],
    }
    _, ref_map = await build_snapshot_with_locators(_RolePage(tree))
    assert ref_map["@e1"].index == 0
    assert ref_map["@e2"].index == 1
