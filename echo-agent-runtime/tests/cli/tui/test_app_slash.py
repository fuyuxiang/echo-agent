import pytest

from echo_agent.cli.tui.app import EchoTUI
from echo_agent.cli.tui.prompt_input import PromptInput
from echo_agent.cli.tui.transcript import TranscriptView


@pytest.mark.asyncio
async def test_slash_opens_panel_with_filtered_commands():
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        app.query_one(PromptInput).text = "/ap"
        await pilot.pause()
        panel = app.query_one("#slash_panel")
        assert panel.display is True
        assert panel.option_count == 2   # /approve /approvals


@pytest.mark.asyncio
async def test_panel_hidden_for_non_slash_text():
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        app.query_one(PromptInput).text = "普通消息"
        await pilot.pause()
        assert app.query_one("#slash_panel").display is False


@pytest.mark.asyncio
async def test_local_clear_command_clears_transcript_and_not_sent():
    sent = []
    async def fake_send(t): sent.append(t)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        await pilot.pause()
        tv = app.query_one(TranscriptView)
        tv.add_user("旧消息")
        await pilot.pause()
        app.post_message(PromptInput.Submitted("/clear"))
        await pilot.pause()
        assert len(tv.children) == 0
        assert sent == []                # 本地命令不发上行


@pytest.mark.asyncio
async def test_local_quit_command_not_sent():
    sent = []
    async def fake_send(t): sent.append(t)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        await pilot.pause()
        app.post_message(PromptInput.Submitted("/quit"))
        await pilot.pause()
        assert sent == []


@pytest.mark.asyncio
async def test_normal_message_still_sent():
    sent = []
    async def fake_send(t): sent.append(t)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        await pilot.pause()
        app.post_message(PromptInput.Submitted("你好"))
        await pilot.pause()
        assert sent == ["你好"]


# --- keyboard-path tests (real pilot.press, exercising the Critical fix) ---

@pytest.mark.asyncio
async def test_enter_without_arrow_submits_local_command():
    # The Critical bug: with the panel auto-open on a slash prefix, Enter was
    # swallowed and /clear could never be sent by keyboard. Typing the full
    # command and pressing Enter (no arrow) must submit it → clears transcript.
    sent = []
    async def fake_send(t): sent.append(t)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        await pilot.pause()
        tv = app.query_one(TranscriptView)
        tv.add_user("旧消息")
        pi = app.query_one(PromptInput)
        pi.focus()
        pi.text = "/clear"
        await pilot.pause()
        assert app.query_one("#slash_panel").display is True
        await pilot.press("enter")
        await pilot.pause()
        assert len(tv.children) == 0     # /clear executed
        assert sent == []                # local command not sent upstream


@pytest.mark.asyncio
async def test_arrow_then_more_typing_then_enter_submits():
    # Narrow regression of the Critical fix: type "/cl", press Down (activates
    # the panel selection, highlighted=0), then keep typing "ear" so the text
    # becomes "/clear". The content change refilters and resets highlighted to
    # None, but the active-selection flag must also drop so Enter falls back to
    # submit. Before the fix, Enter was swallowed (active=True, highlighted=None)
    # and /clear never executed.
    sent = []
    async def fake_send(t): sent.append(t)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        await pilot.pause()
        tv = app.query_one(TranscriptView)
        tv.add_user("旧消息")
        pi = app.query_one(PromptInput)
        pi.focus()
        pi.text = "/cl"
        pi.move_cursor(pi.document.end)      # type at the tail, not position 0
        await pilot.pause()
        assert app.query_one("#slash_panel").display is True
        await pilot.press("down")            # active selection, highlighted=0
        await pilot.press("e", "a", "r")     # text -> "/clear", refilter resets
        await pilot.pause()
        await pilot.press("enter")
        await pilot.pause()
        assert len(tv.children) == 0         # /clear executed
        assert sent == []                    # local command not sent upstream


@pytest.mark.asyncio
async def test_arrow_then_tab_completes_and_not_sent():
    sent = []
    async def fake_send(t): sent.append(t)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.focus()
        pi.text = "/ap"
        await pilot.pause()
        await pilot.press("down")        # actively highlight first match
        await pilot.press("tab")         # accept -> complete
        await pilot.pause()
        assert pi.text == "/approve "     # completed to param position
        assert app.query_one("#slash_panel").display is False
        assert sent == []                # completion never sends


@pytest.mark.asyncio
async def test_arrow_then_enter_completes_and_not_sent():
    sent = []
    async def fake_send(t): sent.append(t)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.focus()
        pi.text = "/ap"
        await pilot.pause()
        await pilot.press("down")
        await pilot.press("enter")       # Enter also accepts when actively selected
        await pilot.pause()
        assert pi.text == "/approve "
        assert app.query_one("#slash_panel").display is False
        assert sent == []


@pytest.mark.asyncio
async def test_escape_closes_panel_keeps_text():
    sent = []
    async def fake_send(t): sent.append(t)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.focus()
        pi.text = "/ap"
        await pilot.pause()
        assert app.query_one("#slash_panel").display is True
        await pilot.press("escape")
        await pilot.pause()
        assert app.query_one("#slash_panel").display is False
        assert pi.text == "/ap"          # text preserved
        assert sent == []


@pytest.mark.asyncio
async def test_typed_normal_message_sent_by_enter():
    sent = []
    async def fake_send(t): sent.append(t)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.focus()
        await pilot.press("h", "e", "l", "l", "o")
        assert app.query_one("#slash_panel").display is False
        await pilot.press("enter")
        await pilot.pause()
        assert sent == ["hello"]


@pytest.mark.asyncio
async def test_shift_enter_inserts_newline():
    app = EchoTUI()
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.focus()
        await pilot.press("h", "i")
        await pilot.press("shift+enter")
        await pilot.press("x")
        await pilot.pause()
        assert pi.document.line_count == 2
        assert "\n" in pi.text


@pytest.mark.asyncio
async def test_multiline_arrows_move_cursor_not_panel():
    # No slash prefix -> panel stays closed; Up on a lower line moves the
    # cursor up rather than triggering panel navigation or history.
    sent = []
    async def fake_send(t): sent.append(t)
    app = EchoTUI(send_coro=fake_send)
    async with app.run_test() as pilot:
        await pilot.pause()
        pi = app.query_one(PromptInput)
        pi.focus()
        await pilot.press("a")
        await pilot.press("shift+enter")
        await pilot.press("b")
        await pilot.pause()
        assert pi.cursor_location[0] == 1     # on the second line
        await pilot.press("up")
        await pilot.pause()
        assert pi.cursor_location[0] == 0     # cursor moved up, not eaten
        assert app.query_one("#slash_panel").display is False
        assert sent == []                     # nothing submitted
