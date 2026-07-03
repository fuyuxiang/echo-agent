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
        self._ok = False
        super().__init__(self._compose_text())

    def _compose_text(self) -> str:
        # 握手完成前如实显示未连接；on_mount 成功后由 app 置 True。
        # No client-side reconnect exists (attach_client connects once), so
        # don't claim "retrying" — say plainly it's disconnected.
        conn = "●已连接" if self._ok else "○已断开"
        # model 不显示：服务端每轮按 task_type 动态路由，不存在稳定"当前模型"，
        # 钉静态名会说谎（详见 spec）。set_model 值仍存下供将来经 cost_update 帧显示。
        return (
            f"{conn} · {self._session} · "
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
