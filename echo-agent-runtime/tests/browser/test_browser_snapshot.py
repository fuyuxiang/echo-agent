"""Snapshot builder tests.

The builder must go through ``frame.evaluate`` — Playwright removed
``page.accessibility`` in 1.57, so any test that fakes that attribute would
validate a code path which cannot run against a real browser.
"""

import pytest

from echo_agent.agent.browser.snapshot import build_page_snapshot

from ._fakes import FakePage, element, make_payload


@pytest.mark.asyncio
async def test_refs_are_numbered_and_mapped_to_locators():
    page = FakePage([make_payload([
        {"kind": "heading", "level": 1, "text": "标题"},
        element("textbox", "搜索", "/html/body/input[1]"),
        {"kind": "text", "text": "普通文本"},
        element("button", "提交", "/html/body/button[1]"),
    ])])
    text, ref_map = await build_page_snapshot(page)

    assert "[@e1] textbox '搜索'" in text
    assert "[@e2] button '提交'" in text
    assert "# 标题" in text
    assert "普通文本" in text
    assert set(ref_map) == {"@e1", "@e2"}
    # each ref resolves to a locator built from that node's own xpath
    assert ref_map["@e1"].locator.selector == "xpath=/html/body/input[1]"
    assert ref_map["@e2"].locator.selector == "xpath=/html/body/button[1]"
    # …and remembers the identity it had at capture time, for drift detection
    assert (ref_map["@e1"].role, ref_map["@e1"].name) == ("textbox", "搜索")


@pytest.mark.asyncio
async def test_page_header_includes_title_and_url():
    page = FakePage([make_payload([], url="https://x.test/a", title="标题页")])
    text, _ = await build_page_snapshot(page)
    assert "Page: 标题页" in text
    assert "URL: https://x.test/a" in text


@pytest.mark.asyncio
async def test_identical_role_name_get_distinct_locators():
    # Two buttons with the same accessible name must NOT collapse onto one
    # locator. This is the failure the previous get_by_role(...).nth() scheme
    # produced whenever accessible names collided.
    page = FakePage([make_payload([
        element("button", "ok", "/html/body/button[1]"),
        element("button", "ok", "/html/body/button[2]"),
    ])])
    _, ref_map = await build_page_snapshot(page)
    assert ref_map["@e1"].locator.selector != ref_map["@e2"].locator.selector
    assert ref_map["@e2"].locator.selector.endswith("button[2]")


@pytest.mark.asyncio
async def test_states_are_rendered():
    page = FakePage([make_payload([
        element("checkbox", "同意", "/html/body/input[1]", ["checked", "required"]),
        element("textbox", "pw", "/html/body/input[2]", ["value=***"]),
    ])])
    text, _ = await build_page_snapshot(page)
    assert "[@e1] checkbox '同意' [checked required]" in text
    assert "value=***" in text


@pytest.mark.asyncio
async def test_truncates_long_content():
    entries = [element("button", f"btn{i}", f"/html/body/button[{i}]") for i in range(500)]
    text, ref_map = await build_page_snapshot(FakePage([make_payload(entries)]),
                                              max_chars=200)
    assert "截断" in text
    assert len(text) <= 300
    # refs are still fully mapped; only the *text* is truncated
    assert len(ref_map) == 500


@pytest.mark.asyncio
async def test_evaluate_failure_yields_placeholder_not_silence():
    """A failed traversal must not masquerade as an empty page."""
    page = FakePage([RuntimeError("frame detached")])
    text, ref_map = await build_page_snapshot(page)
    assert ref_map == {}
    assert text == "(页面无可提取内容)"


@pytest.mark.asyncio
async def test_non_dict_payload_is_ignored():
    page = FakePage(["not-a-dict"])
    text, ref_map = await build_page_snapshot(page)
    assert ref_map == {}
    assert isinstance(text, str)


class _MultiFramePage(FakePage):
    """Page whose frames each return their own payload."""

    def __init__(self, payloads):
        super().__init__()
        self._frames = []
        for payload in payloads:
            frame = FakePage([payload])
            self._frames.append(frame)

    @property
    def frames(self):
        return self._frames


@pytest.mark.asyncio
async def test_iframe_elements_get_refs_scoped_to_their_frame():
    page = _MultiFramePage([
        make_payload([element("button", "outer", "/html/body/button[1]")],
                     url="https://x.test/"),
        make_payload([element("button", "inner", "/html/body/button[1]")],
                     url="https://x.test/frame"),
    ])
    text, ref_map = await build_page_snapshot(page)
    assert "[@e1] button 'outer'" in text
    assert "[@e2] button 'inner'" in text
    assert "iframe: https://x.test/frame" in text
    # same xpath, different frames — the locators must come from different frames
    assert ref_map["@e1"] is not ref_map["@e2"]


@pytest.mark.asyncio
async def test_non_interactive_role_never_becomes_a_ref():
    """A structural role must not be handed a @eN.

    Any explicit role used to be accepted as an interactive element, which made
    a role="dialog" wrapper clickable AND (in the traversal) terminated the walk,
    so the buttons inside it never reached the snapshot at all.
    """
    page = FakePage([make_payload([
        {"kind": "container", "role": "dialog", "name": "确认删除"},
        element("button", "取消", "/html/body/div[1]/button[1]"),
        element("button", "删除", "/html/body/div[1]/button[2]"),
    ])])
    text, ref_map = await build_page_snapshot(page)
    # the dialog is described for orientation, but only the buttons are refs
    assert "<dialog: 确认删除>" in text
    assert set(ref_map) == {"@e1", "@e2"}
    assert ref_map["@e1"].name == "取消"
    assert ref_map["@e2"].name == "删除"


@pytest.mark.asyncio
async def test_unexpected_element_role_is_dropped_without_gaps_in_numbering():
    """The Python side re-checks the role, and refs stay consecutively numbered
    so the model never sees a @eN it cannot use."""
    page = FakePage([make_payload([
        element("button", "前", "/html/body/button[1]"),
        element("region", "不该出现", "/html/body/div[1]"),
        element("button", "后", "/html/body/button[2]"),
    ])])
    text, ref_map = await build_page_snapshot(page)
    assert set(ref_map) == {"@e1", "@e2"}
    assert ref_map["@e2"].name == "后"
    assert "不该出现" not in text


@pytest.mark.asyncio
async def test_frame_budget_is_capped():
    payloads = [make_payload([element("button", f"b{i}", "/html/body/button[1]")])
                for i in range(20)]
    page = _MultiFramePage(payloads)
    _, ref_map = await build_page_snapshot(page, max_frames=3)
    assert len(ref_map) == 3
