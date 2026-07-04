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
        # Completion-panel state, read by _on_key when building the KeyContext:
        #   _panel_visible — the panel is on screen (auto-popped as the user types)
        #   _panel_active  — the user has stepped into it with Up/Down
        self._panel_visible = False
        self._panel_active = False

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
        # Border is set once in app.tcss (`PromptInput { border: none }`); no
        # need to repeat it here. The sizing pass still runs so the box starts
        # at one row instead of TextArea's default tall height.
        super().on_mount()
        self._resize()

    def _resize(self) -> None:
        rows = max(1, min(_MAX_ROWS, self.document.line_count))
        self.styles.height = rows

    def on_text_area_changed(self, event) -> None:
        self._resize()
        self.post_message(self.ContentChanged(self.is_empty, self.text))

    class PanelNav(Message):
        """Ask the app to move the completion-panel highlight. Sent only while
        the panel is open and the user presses Up/Down."""

        def __init__(self, direction: int) -> None:
            self.direction = direction   # -1 up, +1 down
            super().__init__()

    class PanelAccept(Message):
        """Ask the app to complete the actively-highlighted command."""

    class PanelClose(Message):
        """Ask the app to close the completion panel (Escape)."""

    def _has_active_panel_selection(self) -> bool:
        # True only once the user has stepped into the visible panel with Up/Down.
        # decide_key uses this to gate Enter/Tab completion: the panel merely
        # being visible (auto-popped as they type) must NOT hijack Enter — that
        # is the Critical fix.
        return self._panel_active

    def set_panel_visible(self, value: bool) -> None:
        # App layer reports panel *visibility* on every content change. Any
        # content change means the user is typing (not navigating with the
        # arrows), so the active selection is dropped unconditionally — mirroring
        # the app layer resetting the panel highlight to None on every refilter.
        # This closes the narrow path where the panel stays visible but the
        # highlight was reset: active=True + highlighted=None used to swallow
        # Enter. Pressing Down re-activates it (no content change between
        # Down and the following Tab/Enter), so arrow-then-complete still works.
        self._panel_visible = value
        self._panel_active = False

    def apply_completion(self, completed: str) -> None:
        """Replace the buffer with a completed command and park the cursor at
        the end (the parameter position for arg-taking commands). Clears the
        active-selection flag so the next Enter submits."""
        self._panel_active = False
        self.text = completed
        self.move_cursor(self.document.end)

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
        # decide_key is the single authority for key priority; _on_key just
        # snapshots the current state and executes the side effect for whatever
        # action comes back. panel_visible/panel_active let decide_key own the
        # panel-vs-editor decision (Up/Down/Esc drive the panel while it is
        # visible; Enter/Tab complete only once a selection is active, else fall
        # through to submit — the Critical fix).
        row = self.cursor_location[0]
        last_row = self.document.line_count - 1
        ctx = KeyContext(
            key=event.key, text=self.text, cursor_row=row, last_row=last_row,
            panel_visible=self._panel_visible,
            panel_active=self._has_active_panel_selection(),
            hist_idx=self._hist_idx, hist_len=len(self._history),
        )
        action = decide_key(ctx)
        if action == "submit":
            event.prevent_default()
            event.stop()
            self._submit()
            return
        if action == "newline":
            event.prevent_default()
            event.stop()
            self.insert("\n")
            return
        if action == "history_prev":
            event.prevent_default()
            event.stop()
            if self._hist_idx == len(self._history):
                self._draft = self.text
            self._hist_idx = history_prev(self._hist_idx, len(self._history))
            self._apply_history()
            return
        if action == "history_next":
            event.prevent_default()
            event.stop()
            self._hist_idx = history_next(self._hist_idx, len(self._history))
            self._apply_history()
            return
        if action in ("panel_prev", "panel_next"):
            event.prevent_default()
            event.stop()
            # Stepping into the panel with an arrow activates the selection so
            # the following Enter/Tab completes.
            self._panel_active = True
            self.post_message(
                self.PanelNav(1 if action == "panel_next" else -1)
            )
            return
        if action == "panel_accept":
            event.prevent_default()
            event.stop()
            self.post_message(self.PanelAccept())
            return
        if action == "panel_close":
            event.prevent_default()
            event.stop()
            self._panel_active = False
            self.post_message(self.PanelClose())
            return
        # passthrough lets TextArea handle the key normally (cursor moves, Tab
        # while the panel is visible-but-inactive, printable characters, …).
