"""过程信息显示程度（/details）：纯设置对象 + transcript 接线。

此前详略是每个块自己的事：网关发来的每一帧认知事件都变成一行，唯一的控制手段
是手动展开单个块。于是同一个设计招来两种相反的抱怨 —— 一次长工具运行会把答案
埋在 trace 底下；而排查坏答案的人希望思考文本默认就是打开的，却得对每个块按一次
ctrl+o。三态设置把这两件事分开。
"""

from __future__ import annotations

import pytest
from textual.app import App

from echo_agent.cli.tui.details import (
    SECTION_DEFAULTS,
    SECTIONS,
    STATES,
    DetailPrefs,
    parse_command,
    parse_env,
)
from echo_agent.cli.tui.protocol import CogEvent
from echo_agent.cli.tui.transcript import TranscriptView


def _mem(eid: str = "e1") -> CogEvent:
    return CogEvent(
        "memory_recalled", eid, "in_1",
        {"items": [{"source": "偏好", "content": "中文回复"}]}, "召回 1 条记忆",
    )


def _think(eid: str = "e2") -> CogEvent:
    return CogEvent(
        "thinking", eid, "in_1", {"text": "先看 rerank_stage", "duration_ms": 1200}, "思考",
    )


def _tool(tcid: str, status: str = "running") -> CogEvent:
    data = {
        "tool_call_id": tcid, "name": "file_read",
        "params": {"path": "a.py"}, "status": status,
    }
    if status != "running":
        data.update({"result_text": "42 行", "duration_ms": 1500})
    return CogEvent("tool_call", f"ev_{tcid}_{status}", "in_1", data, "读取 a.py")


# ── 设置对象 ──────────────────────────────────────────────────────────

def test_defaults_keep_the_answer_the_biggest_thing_on_screen():
    prefs = DetailPrefs()
    # 工具行的摘要已含动词/对象/结果/耗时，明细是原始参数与 diff，
    # “折叠”只显示这两行过程，不展开原始载荷；用户因此能看到 Agent 真正在做
    # 什么，又不会让每次调用多出 5~8 行明细。
    assert prefs.state("tools") == "collapsed"
    assert prefs.state("thinking") == "collapsed"
    # 逐帧的运行状态与页脚常驻指示器重复
    assert prefs.state("activity") == "hidden"
    assert dict(SECTION_DEFAULTS) == {s: prefs.state(s) for s in SECTIONS}


def test_hidden_sections_are_not_mounted():
    prefs = DetailPrefs(activity="hidden", tools="hidden")
    assert prefs.shows("heartbeat") is False
    assert prefs.shows("tool_call") is False
    assert prefs.shows("thinking") is True


def test_a_failure_is_shown_even_when_its_section_is_hidden():
    """隐藏不允许隐藏失败：用户看不到的报错就是无法处理的报错，
    而"智能体啥也没干"正是这个设置本会制造的假 bug。"""
    prefs = DetailPrefs(tools="hidden")
    assert prefs.shows("tool_call", failed=True) is True


def test_lean_hides_successful_read_only_tools():
    prefs = DetailPrefs(tools="lean")
    assert prefs.shows("tool_call", tool_name="read_file") is False
    assert prefs.shows("tool_call", tool_name="search_files") is False
    assert prefs.shows("tool_call", tool_name="list_dir") is False
    assert prefs.shows("tool_call", tool_name="web_fetch") is False


def test_lean_shows_write_and_exec_tools():
    prefs = DetailPrefs(tools="lean")
    assert prefs.shows("tool_call", tool_name="write_file") is True
    assert prefs.shows("tool_call", tool_name="exec") is True
    assert prefs.shows("tool_call", tool_name="edit_file") is True


def test_lean_shows_unknown_tools():
    prefs = DetailPrefs(tools="lean")
    assert prefs.shows("tool_call", tool_name="brand_new_tool") is True


def test_lean_shows_failed_read_only_tools():
    prefs = DetailPrefs(tools="lean")
    assert prefs.shows("tool_call", failed=True, tool_name="read_file") is True


def test_collapsed_still_shows_read_only_tools():
    prefs = DetailPrefs(tools="collapsed")
    assert prefs.shows("tool_call", tool_name="read_file") is True
    assert prefs.shows("tool_call", tool_name="search_files") is True


def test_non_trace_frames_never_go_through_the_filter():
    # 待批准/待澄清是必须处理的事，不是过程信息
    prefs = DetailPrefs(thinking="hidden", tools="hidden", activity="hidden")
    for cog_type in ("approval_request", "clarify_request", "evolution"):
        assert prefs.section_of(cog_type) is None
        assert prefs.shows(cog_type) is True


def test_unknown_cog_type_stays_visible_but_quiet():
    # 网关可以先于客户端上线新的 cog_type，未分类的 trace 不能被丢掉
    prefs = DetailPrefs()
    assert prefs.shows("brand_new_type") is True
    assert prefs.starts_expanded("brand_new_type") is False


def test_prefs_are_immutable_so_a_change_is_one_assignment():
    prefs = DetailPrefs()
    changed = prefs.with_section("tools", "expanded")
    assert prefs.state("tools") == "collapsed"
    assert changed.state("tools") == "expanded"
    assert changed is not prefs


def test_with_section_rejects_nonsense():
    prefs = DetailPrefs()
    with pytest.raises(ValueError):
        prefs.with_section("nope", "expanded")
    with pytest.raises(ValueError):
        prefs.with_section("tools", "kinda")


# ── 环境变量与命令解析 ────────────────────────────────────────────────

def test_env_supplies_defaults_for_the_very_first_turn():
    prefs = parse_env({"ECHO_TUI_DETAILS": "thinking=expanded,tools=hidden"})
    assert prefs.state("thinking") == "expanded"
    assert prefs.state("tools") == "hidden"
    assert prefs.state("activity") == SECTION_DEFAULTS["activity"]


def test_a_stale_env_value_must_not_stop_the_tui_from_starting():
    prefs = parse_env({"ECHO_TUI_DETAILS": "tools,thinking=louder,=,tools=expanded"})
    assert prefs.state("tools") == "expanded"
    assert prefs.state("thinking") == SECTION_DEFAULTS["thinking"]


def test_missing_env_falls_back_to_defaults():
    assert parse_env({}) == DetailPrefs()


@pytest.mark.parametrize("arg", ["tools expanded", "tools=expanded", "工具 展开", "工具=展开"])
def test_command_accepts_both_languages_and_both_separators(arg):
    """/help 用中文列出这些分区，用户把那段文字原样敲回来不该得到"参数无效"。"""
    assert parse_command(arg) == ("tools", "expanded")


@pytest.mark.parametrize("arg", ["tools lean", "工具 精简", "工具=精简", "tools=lean"])
def test_command_lean_state_round_trips(arg):
    assert parse_command(arg) == ("tools", "lean")


@pytest.mark.parametrize("arg", ["", "   ", "tools", "tools expanded extra", "nope off", "工具 打开一点"])
def test_command_rejects_unparseable_input(arg):
    assert parse_command(arg) is None


def test_every_section_state_combination_is_expressible():
    for section in SECTIONS:
        for state in STATES:
            assert parse_command(f"{section} {state}") == (section, state)


# ── transcript 接线 ───────────────────────────────────────────────────

class _T(App):
    def compose(self):
        yield TranscriptView()


@pytest.mark.asyncio
async def test_hidden_section_mounts_nothing_and_returns_none():
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        tv.details = DetailPrefs(thinking="hidden")
        before = len(tv.children)
        assert tv.add_cognitive(_think()) is None
        assert len(tv.children) == before


@pytest.mark.asyncio
async def test_a_failing_tool_still_lands_when_tools_are_hidden():
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        tv.details = DetailPrefs(tools="hidden")
        assert tv.add_tool_call(_tool("c1", "running")) is None
        failed = tv.add_tool_call(_tool("c1", "error"))
        assert failed is not None
        assert failed in tv.children
        # 成功的调用则始终不上屏
        assert tv.add_tool_call(_tool("c2", "running")) is None
        assert tv.add_tool_call(_tool("c2", "ok")) is None


def _read_tool(tcid: str, status: str = "running") -> CogEvent:
    data = {
        "tool_call_id": tcid, "name": "read_file",
        "params": {"path": "/tmp/a.py"}, "status": status,
    }
    if status != "running":
        data.update({"result_text": "42 行", "duration_ms": 200})
    return CogEvent("tool_call", f"ev_{tcid}_{status}", "in_1", data, "读取 a.py")


@pytest.mark.asyncio
async def test_collapsed_default_shows_successful_read_only_tool():
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        assert tv.details.state("tools") == "collapsed"
        block = tv.add_tool_call(_read_tool("r1", "running"))
        assert block is not None
        assert tv.add_tool_call(_read_tool("r1", "ok")) is block


@pytest.mark.asyncio
async def test_lean_shows_failed_read_only_tool():
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        tv.details = DetailPrefs(tools="lean")
        assert tv.details.state("tools") == "lean"
        assert tv.add_tool_call(_read_tool("r1", "running")) is None
        block = tv.add_tool_call(_read_tool("r1", "error"))
        assert block is not None
        assert block in tv.children


@pytest.mark.asyncio
async def test_lean_shows_write_tools():
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        tv.details = DetailPrefs(tools="lean")
        assert tv.details.state("tools") == "lean"
        block = tv.add_tool_call(_tool("w1", "ok"))
        assert block is not None


@pytest.mark.asyncio
async def test_expanded_section_opens_on_mount_not_after():
    """挂载后再 toggle 会先画一帧摘要，配合底部锚点还会顶一下滚动位置。"""
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        tv.details = DetailPrefs(thinking="expanded", tools="expanded")
        assert tv.add_cognitive(_think()).expanded is True
        assert tv.add_cognitive(_mem()).expanded is True
        assert tv.add_tool_call(_tool("c1", "running")).expanded is True


@pytest.mark.asyncio
async def test_a_block_with_no_payload_stays_summarized():
    # 空载荷的行标成"已展开"却只有一行，是自相矛盾的提示
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        tv.details = DetailPrefs(thinking="expanded")
        bare = tv.add_cognitive(CogEvent("thinking", "e9", "in_1", {}, "思考"))
        assert bare.expanded is False
        assert "▾" not in bare.render_summary()


@pytest.mark.asyncio
async def test_a_tool_that_gains_detail_later_honours_the_setting():
    # running 帧可能还没有明细，明细在 done 帧才到
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        tv.details = DetailPrefs(tools="expanded")
        ev = _tool("c1", "running")
        ev.data["params"] = {}
        block = tv.add_tool_call(ev)
        assert block.expanded is False
        tv.add_tool_call(_tool("c1", "ok"))
        assert block.expanded is True


@pytest.mark.asyncio
async def test_changing_the_setting_rerenders_what_is_already_on_screen():
    """/details 是整段转录的视图设置，不是"只影响接下来发生的事"。"""
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        think = tv.add_cognitive(_think())
        tool = tv.add_tool_call(_tool("c1", "ok"))
        assert (think.expanded, tool.expanded) == (False, False)
        tv.set_details(DetailPrefs(thinking="expanded", tools="expanded"))
        assert (think.expanded, tool.expanded) == (True, True)
        tv.set_details(DetailPrefs())
        assert (think.expanded, tool.expanded) == (False, False)


@pytest.mark.asyncio
async def test_switching_to_hidden_keeps_history_on_screen():
    """回溯删掉用户已经读过（甚至已滚动定位过）的行，会让转录与他们看到的不一致，
    而且重建这些行所需的状态已经不在了 —— hidden 管接下来，不管历史。"""
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        think = tv.add_cognitive(_think())
        tv.set_details(DetailPrefs(thinking="hidden"))
        assert think in tv.children
        # 之后到达的才被拦下
        assert tv.add_cognitive(_think("e3")) is None
