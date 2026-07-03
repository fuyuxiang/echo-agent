"""Prompt input box. Enter submits, Shift+Enter inserts a newline.

Textual 8.2.8 note: the ``Key`` event has no ``.shift`` attribute — shift is
encoded in the key name (``"shift+enter"`` vs ``"enter"``). Also, TextArea's
default binding inserts a newline on plain Enter, so we must intercept both
keys explicitly: swallow ``enter`` (to submit) and turn ``shift+enter`` into a
manual newline insert."""

from __future__ import annotations

from textual.message import Message
from textual.widgets import TextArea


class PromptInput(TextArea):
    def __init__(self) -> None:
        super().__init__()
        # 裸 TextArea 只有闪烁光标，用户认不出这是输入区。给边框加标题明确用途。
        self.border_title = "❯ 输入消息 · Enter 发送 · Shift+Enter 换行"

    def on_mount(self) -> None:
        # border_title 需要有可见边框才会显示。
        self.styles.border = ("round", "grey")

    class Submitted(Message):
        def __init__(self, text: str) -> None:
            self.text = text
            super().__init__()

    def _on_key(self, event) -> None:
        if event.key == "enter":
            event.prevent_default()
            event.stop()
            text = self.text.strip()
            if text:
                self.post_message(self.Submitted(text))
                self.text = ""
            return
        if event.key == "shift+enter":
            event.prevent_default()
            event.stop()
            self.insert("\n")
            return
