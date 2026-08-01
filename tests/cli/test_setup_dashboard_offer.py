"""向导里的 Dashboard 构建询问。

这段代码在 CI 上以 AttributeError 崩掉过:BuildOutcome 早先被拆成
build_succeeded / artifact_usable 两个字段,而这里仍在读已不存在的 .ok。之所以直到
CI 才暴露,是因为没有任何测试直接调用 _maybe_offer_dashboard_build —— 唯一会走到它
的是 test_setup_gateway_start 里讲服务启动的用例,而那需要机器上真有 Node/pnpm 且
构建真的跑起来才会到达出错那一行。本文件用假的 outcome 直接钉住这条路径。
"""
from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

import pytest

from echo_agent.cli import setup as wiz
from echo_agent.gateway import dashboard_build
from echo_agent.gateway.dashboard_build import BuildOutcome, BuildReason


@pytest.fixture
def offer(monkeypatch, tmp_path, capsys):
    """打桩 find_web_dir / dashboard_build_needed / build_dashboard。

    返回一个可调用对象:传入 outcome 与用户是否同意构建,得到 (是否调用了构建, 输出)。
    """
    web = tmp_path / "web"
    web.mkdir()

    def run(outcome: BuildOutcome | None, *, consent: bool = True, needed: bool = True):
        built: list[Path] = []

        def _build(web_dir, **kw):
            built.append(web_dir)
            return outcome

        monkeypatch.setattr(dashboard_build, "find_web_dir", lambda: web)
        monkeypatch.setattr(dashboard_build, "dashboard_build_needed", lambda d: needed)
        monkeypatch.setattr(dashboard_build, "build_dashboard", _build)
        monkeypatch.setattr(wiz.ui, "confirm", lambda *a, **kw: consent)

        wiz._maybe_offer_dashboard_build()
        return bool(built), capsys.readouterr().out

    return SimpleNamespace(run=run, web=web)


def _outcome(reason, *, artifact_usable=False):
    return BuildOutcome(
        build_succeeded=reason is BuildReason.OK,
        artifact_usable=artifact_usable or reason is BuildReason.OK,
        reason=reason,
        detail="" if reason is BuildReason.OK else "boom",
    )


def test_success_is_reported_without_crashing(offer):
    """回归锚点:读取 outcome 的成功标志不得抛 AttributeError。"""
    ran, out = offer.run(_outcome(BuildReason.OK))
    assert ran
    assert "完整 Dashboard 已构建" in out


def test_build_failure_is_reported_as_warning_not_crash(offer):
    ran, out = offer.run(_outcome(BuildReason.BUILD_FAILED))
    assert ran
    assert "构建失败" in out
    assert "前端构建失败" in out  # 原因专属提示必须出现


def test_stale_bundle_is_not_announced_as_success(offer):
    """构建失败但旧产物还在:不能打成绿色成功。"""
    ran, out = offer.run(
        _outcome(BuildReason.BUILD_FAILED, artifact_usable=True)
    )
    assert ran
    assert "上一次" in out


def test_declining_skips_the_build(offer):
    ran, out = offer.run(_outcome(BuildReason.OK), consent=False)
    assert not ran


def test_no_build_needed_asks_nothing(offer):
    ran, _ = offer.run(_outcome(BuildReason.OK), needed=False)
    assert not ran
