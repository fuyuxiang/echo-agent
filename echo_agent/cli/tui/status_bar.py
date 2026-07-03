"""Bottom status bar: session/model/cost/connection plus key hints.

Rendering is a pure ``_render`` string so it can be inspected in tests
without a live screen."""

from __future__ import annotations

from textual.widgets import Static


class StatusBar(Static):
    def __init__(self) -> None:
        self._session = ""
        self._model = ""
        self._cost = 0.0
        self._ok = True
        super().__init__(self._compose_text())

    def _compose_text(self) -> str:
        # No client-side reconnect exists (attach_client connects once), so
        # don't claim "retrying" — say plainly it's disconnected.
        conn = "●连接" if self._ok else "○已断开"
        return (
            f"{conn} · {self._session} · {self._model} · "
            f"累计 ${round(self._cost, 4)} · Enter发送 Shift+Enter换行 "
            f"ctrl+r记忆 ctrl+o思考 ctrl+c退出"
        )

    def set_session(self, key: str) -> None:
        self._session = key
        self.update(self._compose_text())

    def set_model(self, name: str) -> None:
        self._model = name
        self.update(self._compose_text())

    def set_cost(self, total: float) -> None:
        self._cost = total
        self.update(self._compose_text())

    def set_connection(self, ok: bool) -> None:
        self._ok = ok
        self.update(self._compose_text())
