"""Prompt input box. Enter submits, Shift+Enter inserts a newline, Up/Down
recall history (only when the cursor is on the first/last line so multi-line
cursor movement is preserved). Height auto-grows 1..10 lines.

Textual 8.2.8: Key events encode shift in the key name ("shift+enter"), and
TextArea has no native placeholder/auto-height — the app layer overlays a
placeholder driven by ContentChanged, and height is recomputed here."""

from __future__ import annotations

from textual.message import Message
from textual.widgets import TextArea

from echo_agent.cli.tui.keymap import (
    KeyContext, decide_key, history_next, history_prev,
)

_MAX_HISTORY = 200
_MAX_ROWS = 10


class PromptInput(TextArea):
    def __init__(self) -> None:
        super().__init__()
        self._history: list[str] = []
        self._hist_idx = 0          # == len(history) means "not browsing / draft"
        self._draft = ""

    class Submitted(Message):
        def __init__(self, text: str) -> None:
            self.text = text
            super().__init__()

    class ContentChanged(Message):
        def __init__(self, is_empty: bool, text: str) -> None:
            self.is_empty = is_empty
            self.text = text
            super().__init__()

    @property
    def is_empty(self) -> bool:
        return self.text.strip() == ""

    def on_mount(self) -> None:
        super().on_mount()
        self.styles.border = ("none", "transparent")
        self._resize()

    def _resize(self) -> None:
        rows = max(1, min(_MAX_ROWS, self.document.line_count))
        self.styles.height = rows

    def on_text_area_changed(self, event) -> None:
        self._resize()
        self.post_message(self.ContentChanged(self.is_empty, self.text))

    def _panel_open(self) -> bool:
        # Overridden view: the app owns the panel; default False keeps the pure
        # keymap correct for the standalone widget. App layer sets this via
        # set_panel_open() when a completion panel is showing.
        return getattr(self, "_panel_open_flag", False)

    def set_panel_open(self, value: bool) -> None:
        self._panel_open_flag = value

    def _submit(self) -> None:
        text = self.text.strip()
        if not text:
            return
        self._history.append(text)
        while len(self._history) > _MAX_HISTORY:
            self._history.pop(0)
        self._hist_idx = len(self._history)
        self._draft = ""
        self.post_message(self.Submitted(text))
        self.text = ""

    def _apply_history(self) -> None:
        if self._hist_idx >= len(self._history):
            self.text = self._draft
        else:
            self.text = self._history[self._hist_idx]

    def _on_key(self, event) -> None:
        row = self.cursor_location[0]
        last_row = self.document.line_count - 1
        ctx = KeyContext(
            key=event.key, text=self.text, cursor_row=row, last_row=last_row,
            panel_open=self._panel_open(), hist_idx=self._hist_idx,
            hist_len=len(self._history),
        )
        action = decide_key(ctx)
        if action == "submit":
            event.prevent_default(); event.stop()
            self._submit()
            return
        if action == "newline":
            event.prevent_default(); event.stop()
            self.insert("\n")
            return
        if action == "history_prev":
            event.prevent_default(); event.stop()
            if self._hist_idx == len(self._history):
                self._draft = self.text
            self._hist_idx = history_prev(self._hist_idx, len(self._history))
            self._apply_history()
            return
        if action == "history_next":
            event.prevent_default(); event.stop()
            self._hist_idx = history_next(self._hist_idx, len(self._history))
            self._apply_history()
            return
        # panel_* actions are handled by the app layer (Task 6); passthrough
        # lets TextArea move the cursor normally.
