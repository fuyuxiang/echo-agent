"""流式思考在转录里的呈现：一轮推理只占一行。

此前思考只有一帧，是模型答完之后才补发的 —— 用户在等待期间看不到任何推理内容。
改成边生成边发快照后，若客户端仍按"每帧挂一个块"处理，一轮推理会在屏幕上堆出
十几行、每行都是同一段思考更长的前缀。这里锁定按 thinking_id 原地更新的行为。
"""

from __future__ import annotations

import pytest
from textual.app import App

from echo_agent.cli.tui.blocks import CognitiveBlock
from echo_agent.cli.tui.details import DetailPrefs
from echo_agent.cli.tui.protocol import CogEvent
from echo_agent.cli.tui.transcript import TranscriptView


def _snap(text: str, *, eid: str, tid: str = "th_1", streaming: bool = True) -> CogEvent:
    return CogEvent(
        "thinking", eid, "in_1",
        {"text": text, "duration_ms": 0, "thinking_id": tid,
         "streaming": streaming, "retracted": False},
        "思考中" if streaming else "思考 2.0s",
    )


def _retract(tid: str = "th_1", eid: str = "e_r") -> CogEvent:
    return CogEvent(
        "thinking", eid, "in_1",
        {"text": "", "duration_ms": 1000, "thinking_id": tid,
         "streaming": False, "retracted": True},
        "思考 1.0s",
    )


class _T(App):
    def compose(self):
        yield TranscriptView()


def _thinking_blocks(tv) -> list:
    return [
        w for w in tv.children
        if isinstance(w, CognitiveBlock) and w.ev.cog_type == "thinking"
    ]


@pytest.mark.asyncio
async def test_snapshots_of_one_round_update_a_single_line():
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        first = tv.add_cognitive(_snap("先看", eid="e1"))
        again = tv.add_cognitive(_snap("先看 rerank_stage", eid="e2"))
        assert again is first
        assert len(_thinking_blocks(tv)) == 1
        assert first.ev.data["text"] == "先看 rerank_stage"


@pytest.mark.asyncio
async def test_two_rounds_get_two_lines():
    # 一次回复里可能有多轮 LLM 调用，每轮的推理是独立的一段
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        tv.add_cognitive(_snap("第一轮", eid="e1", tid="th_1"))
        tv.add_cognitive(_snap("第二轮", eid="e2", tid="th_2"))
        assert len(_thinking_blocks(tv)) == 2


@pytest.mark.asyncio
async def test_final_frame_replaces_the_partial_in_place():
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        block = tv.add_cognitive(_snap("想了一", eid="e1"))
        tv.add_cognitive(_snap("想了一下", eid="e2", streaming=False))
        assert len(_thinking_blocks(tv)) == 1
        assert block.is_streaming is False
        # 结束后才显示真实耗时，中途不能先报一个会往回跳的数字
        assert "思考 2.0s" in block.render_summary()


@pytest.mark.asyncio
async def test_a_retracted_round_leaves_no_trace_line():
    """推理被提升成答案时（content 为空、reasoning 即正文），
    正文马上要以回复呈现，trace 再留一份就是同一段文字读两遍。"""
    app = _T()
    async with app.run_test() as pilot:
        tv = app.query_one(TranscriptView)
        tv.add_cognitive(_snap("这就是答案", eid="e1"))
        assert tv.add_cognitive(_retract()) is None
        await pilot.pause()
        assert _thinking_blocks(tv) == []


@pytest.mark.asyncio
async def test_retraction_also_clears_the_ctrl_o_target():
    # 否则快捷键会去展开一个已经不在屏幕上的块，按下去像没反应
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        tv.add_cognitive(_snap("这就是答案", eid="e1"))
        assert tv.last_thinking_block() is not None
        tv.add_cognitive(_retract())
        assert tv.last_thinking_block() is None


@pytest.mark.asyncio
async def test_a_retraction_for_an_unknown_round_is_harmless():
    # 断线重连后可能只收到收尾帧
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        assert tv.add_cognitive(_retract(tid="th_ghost")) is None


@pytest.mark.asyncio
async def test_hidden_thinking_ignores_the_whole_stream():
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        tv.details = DetailPrefs(thinking="hidden")
        assert tv.add_cognitive(_snap("看不到", eid="e1")) is None
        assert tv.add_cognitive(_snap("也看不到", eid="e2")) is None
        assert _thinking_blocks(tv) == []


@pytest.mark.asyncio
async def test_hiding_mid_round_still_settles_the_line_already_on_screen():
    """/details 隐藏不会撤掉用户已经读过的行（见 set_details），
    所以那一行必须继续收到收尾帧，否则会永远停在"思考中"。"""
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        block = tv.add_cognitive(_snap("想到一半", eid="e1"))
        tv.set_details(DetailPrefs(thinking="hidden"))
        tv.add_cognitive(_snap("想完了", eid="e2", streaming=False))
        assert block.is_streaming is False
        assert "思考 2.0s" in block.render_summary()


@pytest.mark.asyncio
async def test_hiding_mid_round_still_honours_a_retraction():
    app = _T()
    async with app.run_test() as pilot:
        tv = app.query_one(TranscriptView)
        tv.add_cognitive(_snap("这就是答案", eid="e1"))
        tv.set_details(DetailPrefs(thinking="hidden"))
        tv.add_cognitive(_retract())
        await pilot.pause()
        assert _thinking_blocks(tv) == []


@pytest.mark.asyncio
async def test_an_expanded_line_keeps_growing_without_reclosing():
    """用户中途按 ctrl+o 展开后，下一帧快照不能把它合回摘要。"""
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        block = tv.add_cognitive(_snap("第一段", eid="e1"))
        block.toggle()
        assert block.expanded is True
        tv.add_cognitive(_snap("第一段\n第二段", eid="e2"))
        assert block.expanded is True
        assert "第二段" in block.render_detail()


@pytest.mark.asyncio
async def test_a_dead_turn_settles_a_half_streamed_thinking_line():
    """收尾帧来自同一个已经死掉的回合，不处理这行就永远停在"思考中"。"""
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        block = tv.add_cognitive(_snap("正在想", eid="e1"))
        tv.end_turn_cleanup()
        assert block.is_streaming is False
        assert "未完成" in block.render_summary()
        # 已收到的推理保留：中断时它往往是屏幕上最有价值的信息
        assert "正在想" in block.render_detail()


@pytest.mark.asyncio
async def test_cleanup_leaves_a_settled_thinking_line_alone():
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        tv.add_cognitive(_snap("想了一下", eid="e1"))
        block = tv.add_cognitive(_snap("想了一下", eid="e2", streaming=False))
        tv.end_turn_cleanup()
        assert "思考 2.0s" in block.render_summary()
        assert "未完成" not in block.render_summary()


@pytest.mark.asyncio
async def test_a_new_turn_forgets_the_previous_round_index():
    app = _T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        tv.add_user("问题一")
        tv.add_cognitive(_snap("旧的一轮", eid="e1"))
        tv.add_user("问题二")
        # 同一个 id 不会真的复用，这里验证的是索引不会一直持有旧块
        assert tv._thinking_blocks == {}


# ── 摘要行 ────────────────────────────────────────────────────────────

def test_collapsed_streaming_line_shows_the_latest_thought():
    """默认折叠时，"思考中"三个字本身不含信息量；带上最新一行才看得出在想什么。"""
    block = CognitiveBlock(_snap("先定位 rerank_stage 的入口", eid="e1"))
    summary = block.render_summary()
    assert "思考中" in summary
    assert "rerank_stage" in summary


def test_the_tail_is_the_last_nonempty_line():
    block = CognitiveBlock(_snap("第一步\n第二步\n\n", eid="e1"))
    assert "第二步" in block.render_summary()


def test_the_tail_is_clipped_so_the_row_does_not_wrap_and_jitter():
    block = CognitiveBlock(_snap("啊" * 200, eid="e1"))
    assert len(block.render_summary()) < 160


def test_a_settled_line_drops_the_tail_and_offers_the_shortcut():
    block = CognitiveBlock(_snap("完整推理", eid="e1", streaming=False))
    summary = block.render_summary()
    assert "ctrl+o" in summary
    assert "思考 2.0s" in summary


def test_an_empty_streaming_snapshot_shows_no_separator():
    block = CognitiveBlock(_snap("", eid="e1"))
    assert "思考中" in block.render_summary()


def test_update_event_swaps_the_payload_of_one_block():
    block = CognitiveBlock(_snap("一", eid="e1"))
    block.update_event(_snap("一二", eid="e2"))
    assert block.ev.cog_event_id == "e2"
    assert block.ev.data["text"] == "一二"
