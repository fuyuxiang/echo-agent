"""/save — 把对话写成 Markdown 文件（本地命令，不发上行）。"""

from pathlib import Path

import pytest

from echo_agent.cli.tui.app import EchoTUI
from echo_agent.cli.tui.prompt_input import PromptInput


async def _drive(app, fn):
    async with app.run_test() as pilot:
        await pilot.pause()
        await fn(pilot)


@pytest.mark.asyncio
async def test_save_writes_markdown_and_not_sent(tmp_path):
    sent: list[str] = []

    async def fake_send(t):
        sent.append(t)

    app = EchoTUI(send_coro=fake_send, session_key="cli:u", save_dir=tmp_path)

    async def body(pilot):
        app._tv.add_user("你好")
        app.on_user_reply_final("in1", "回复一")
        await pilot.pause()
        app.post_message(PromptInput.Submitted("/save"))
        await pilot.pause()
        files = list(tmp_path.glob("echo-*.md"))
        assert len(files) == 1
        text = files[0].read_text(encoding="utf-8")
        assert "# Echo 对话记录" in text
        assert "## 用户" in text and "你好" in text
        assert "## 助手" in text and "回复一" in text
        assert "cli:u" in text
        assert sent == []  # 本地命令不发上行

    await _drive(app, body)


@pytest.mark.asyncio
async def test_save_custom_filename(tmp_path):
    app = EchoTUI(save_dir=tmp_path)

    async def body(pilot):
        app._tv.add_user("问题")
        app.on_user_reply_final("in1", "答案")
        await pilot.pause()
        app.post_message(PromptInput.Submitted("/save 我的对话"))
        await pilot.pause()
        # 相对文件名落在默认 save_dir 下，且自动补 .md
        target = tmp_path / "我的对话.md"
        assert target.exists()
        assert "答案" in target.read_text(encoding="utf-8")

    await _drive(app, body)


@pytest.mark.asyncio
async def test_save_directory_arg_keeps_auto_name(tmp_path):
    app = EchoTUI(save_dir=tmp_path)
    sub = tmp_path / "out"
    sub.mkdir()

    async def body(pilot):
        app._tv.add_user("q")
        app.on_user_reply_final("in1", "a")
        await pilot.pause()
        # 目录参数（结尾带斜杠）→ 自动命名文件放进该目录
        app.post_message(PromptInput.Submitted("/save out/"))
        await pilot.pause()
        files = list(sub.glob("echo-*.md"))
        assert len(files) == 1

    await _drive(app, body)


@pytest.mark.asyncio
async def test_save_empty_conversation_warns(tmp_path):
    app = EchoTUI(save_dir=tmp_path)

    async def body(pilot):
        app.post_message(PromptInput.Submitted("/save"))
        await pilot.pause()
        # 无对话时不写文件
        assert list(tmp_path.glob("*.md")) == []

    await _drive(app, body)


@pytest.mark.asyncio
async def test_save_dir_default_is_transcripts(tmp_path, monkeypatch):
    # 未显式传 save_dir 时回退到 cwd/transcripts
    monkeypatch.chdir(tmp_path)
    app = EchoTUI()
    assert app._save_dir == Path.cwd() / "transcripts"
