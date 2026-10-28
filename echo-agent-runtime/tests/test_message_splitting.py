from __future__ import annotations

from echo_agent.channels.qqbot import QQBotChannel
from echo_agent.channels.weixin import WeixinChannel
from echo_agent.utils.text import split_message


def test_split_message_does_not_split_within_limit() -> None:
    text = "a" * 3999

    assert split_message(text, 4000) == [text]


def test_split_message_ignores_early_newline_to_avoid_tiny_chunks() -> None:
    text = "intro\n" + ("x" * 110)

    chunks = split_message(text, 100)

    assert len(chunks) == 2
    assert len(chunks[0]) == 100
    assert chunks[1] == "x" * 16


def test_split_message_prefers_late_natural_boundary() -> None:
    text = ("a" * 82) + "\n\n" + ("b" * 40)

    chunks = split_message(text, 100)

    assert chunks == [("a" * 82), ("b" * 40)]


def test_split_message_uses_cjk_sentence_boundary_near_limit() -> None:
    text = ("a" * 80) + "\u3002" + ("b" * 40)

    chunks = split_message(text, 100)

    assert chunks == [("a" * 80) + "\u3002", "b" * 40]


def test_weixin_and_qqbot_use_same_low_fragment_splitter() -> None:
    text = "intro\n" + ("x" * 4050)

    weixin_chunks = WeixinChannel._split_text(text)
    qqbot_chunks = QQBotChannel._split_text(text)

    assert weixin_chunks == qqbot_chunks
    assert len(weixin_chunks) == 2
    assert len(weixin_chunks[0]) == 4000
