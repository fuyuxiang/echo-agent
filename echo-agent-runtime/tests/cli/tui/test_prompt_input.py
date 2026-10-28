import pytest
from textual.app import App

from echo_agent.cli.tui.prompt_input import _MAX_ROWS, PromptInput


class _Host(App):
    def compose(self):
        yield PromptInput()


@pytest.mark.asyncio
async def test_no_border_title():
    app = _Host()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        assert not pi.border_title  # 提示符已移到行首兄弟 widget，标题清空


@pytest.mark.asyncio
async def test_is_empty_tracks_text():
    app = _Host()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        assert pi.is_empty is True
        pi.text = "hi"
        assert pi.is_empty is False


@pytest.mark.asyncio
async def test_enter_submits_and_records_history():
    app = _Host()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.text = "第一条"
        await pilot.press("enter")
        await pilot.pause()
        assert pi.text == ""
        assert pi._history[-1] == "第一条"


@pytest.mark.asyncio
async def test_up_recalls_previous_message():
    app = _Host()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.text = "早先"
        await pilot.press("enter")
        await pilot.pause()
        await pilot.press("up")
        await pilot.pause()
        assert pi.text == "早先"


@pytest.mark.asyncio
async def test_down_restores_draft_at_end():
    app = _Host()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.text = "历史项"
        await pilot.press("enter")
        await pilot.pause()
        pi.text = "草稿中"          # 未发送的草稿
        await pilot.press("up")       # 进入历史，暂存草稿
        await pilot.pause()
        assert pi.text == "历史项"
        await pilot.press("down")     # 走回草稿
        await pilot.pause()
        assert pi.text == "草稿中"


@pytest.mark.asyncio
async def test_down_keeps_draft_when_not_browsing():
    # 回归：发送一条历史后敲新草稿（未进入历史浏览），光标在行尾按 down
    # 不应把草稿抹掉
    app = _Host()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.text = "历史项"
        await pilot.press("enter")
        await pilot.pause()
        pi.text = "新草稿还没发"        # 未发送、也未按 up 进入浏览
        pi.move_cursor(pi.document.end)  # 光标移到行尾
        await pilot.press("down")
        await pilot.pause()
        assert pi.text == "新草稿还没发"


@pytest.mark.asyncio
async def test_single_row_when_empty():
    app = _Host()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        assert pi._visual_rows() == 1


@pytest.mark.asyncio
async def test_long_wrapped_line_grows_box():
    # 回归：一行很长的文本在软折行下应把输入框撑高（此前只数硬换行，
    # line_count 恒为 1，长文本被压在一行看不全）。
    app = _Host()
    async with app.run_test(size=(40, 24)) as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        # 没有任何 \n，但远超一行宽度，软折行后必然多于 1 可视行。
        pi.text = "长文本" * 60
        await pilot.pause()
        assert "\n" not in pi.text                 # 确实是单个硬行
        assert pi.document.line_count == 1          # 旧逻辑会停在 1
        assert pi._visual_rows() > 1                # 新逻辑按可视折行长高


@pytest.mark.asyncio
async def test_row_count_capped_at_max():
    app = _Host()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.text = "\n".join(str(i) for i in range(_MAX_ROWS + 20))
        await pilot.pause()
        assert pi._visual_rows() == _MAX_ROWS
