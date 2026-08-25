"""Tests for skill-related WS handlers."""
from __future__ import annotations

import asyncio
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from echo_agent.gateway.skill_singleton import (
    get_skill_manager,
    reset_skill_manager,
    set_skill_manager_for_tests,
)
from echo_agent.gateway.ws_skill import (
    handle_skill_disable,
    handle_skill_enable,
    handle_skill_list,
)
from echo_agent.runtime_paths import echo_home
from echo_agent.skills.manager import InstalledSkill, SkillManager, SkillManifest, SkillStatus


@pytest.fixture
def skills_dir(tmp_path: Path) -> Path:
    return tmp_path / "skills"


@pytest.fixture
def manager(skills_dir: Path) -> SkillManager:
    return SkillManager(skills_dir)


def _make_skill(manager: SkillManager, name: str, status: SkillStatus = SkillStatus.INSTALLED) -> None:
    """向 manager 注入一个测试 skill。"""
    manifest = SkillManifest(name=name, version="1.0.0", description=f"desc-{name}")
    manager._skills[name] = type(manager._skills)()  # noqa: SLF001 — 测试内部状态
    from echo_agent.skills.manager import InstalledSkill
    # 真实 enable/disable 会写 <skills_dir>/<name>/.status,这里必须先建好目录
    (manager._skills_dir / name).mkdir(parents=True, exist_ok=True)
    manager._skills[name] = InstalledSkill(manifest=manifest, status=status, path=str(manager._skills_dir / name))  # noqa: SLF001


def test_handle_skill_list_returns_empty_when_no_skills(manager: SkillManager) -> None:
    """无 skill 时返回空数组。"""
    import asyncio
    result = asyncio.run(handle_skill_list(manager))
    assert result["type"] == "skill.list_result"
    assert result["skills"] == []


def test_handle_skill_list_returns_all_skills(manager: SkillManager) -> None:
    """返回已安装 skill 列表。"""
    _make_skill(manager, "ppt-author", SkillStatus.ENABLED)
    _make_skill(manager, "summarize", SkillStatus.DISABLED)
    import asyncio
    result = asyncio.run(handle_skill_list(manager))
    names = sorted(s["name"] for s in result["skills"])
    assert names == ["ppt-author", "summarize"]


def test_handle_skill_list_includes_status(manager: SkillManager) -> None:
    """每条 skill 携带 status 字段。"""
    _make_skill(manager, "ppt-author", SkillStatus.ENABLED)
    import asyncio
    result = asyncio.run(handle_skill_list(manager))
    assert result["skills"][0]["status"] == "enabled"
    assert result["skills"][0]["name"] == "ppt-author"
    assert result["skills"][0]["version"] == "1.0.0"
    assert result["skills"][0]["description"] == "desc-ppt-author"


def test_handle_skill_enable_success(manager: SkillManager) -> None:
    """启用成功返回 None(走 accepted ack)。"""
    _make_skill(manager, "ppt-author", SkillStatus.INSTALLED)
    import asyncio
    assert asyncio.run(handle_skill_enable(manager, "ppt-author")) is None
    assert manager.get_skill("ppt-author").status == SkillStatus.ENABLED


def test_handle_skill_enable_unknown_returns_error(manager: SkillManager) -> None:
    """启用不存在的 skill 返回 error 帧。"""
    import asyncio
    result = asyncio.run(handle_skill_enable(manager, "ghost"))
    assert result is not None
    assert result["type"] == "error"
    assert "ghost" in result["message"]


def test_handle_skill_disable_success(manager: SkillManager) -> None:
    """禁用成功。"""
    _make_skill(manager, "ppt-author", SkillStatus.ENABLED)
    import asyncio
    assert asyncio.run(handle_skill_disable(manager, "ppt-author")) is None
    assert manager.get_skill("ppt-author").status == SkillStatus.DISABLED


def test_handle_skill_disable_unknown_returns_error(manager: SkillManager) -> None:
    """禁用不存在的 skill 返回 error。"""
    import asyncio
    result = asyncio.run(handle_skill_disable(manager, "ghost"))
    assert result is not None
    assert result["type"] == "error"


# --- SkillManager singleton integration ----------------------------------


def test_get_skill_manager_returns_same_instance(tmp_path: Path) -> None:
    """模块级 singleton:同一进程内多次调用必须返回同一实例。"""
    set_skill_manager_for_tests(SkillManager(tmp_path / "skills"))
    try:
        a = get_skill_manager()
        b = get_skill_manager()
        assert a is b
    finally:
        reset_skill_manager()


def test_get_skill_manager_uses_echo_home_skills(tmp_path: Path, monkeypatch) -> None:
    """首次调用时按 echo_home()/skills 路径实例化,不走 bundled_skills_dir。"""
    # skill_singleton 在 import 时已经把 echo_home 绑到自身模块名空间,
    # 必须 patch 绑定后的符号(而非 runtime_paths 原模块),才能影响单例创建。
    monkeypatch.setattr("echo_agent.gateway.skill_singleton.echo_home", lambda: tmp_path)
    reset_skill_manager()
    try:
        manager = get_skill_manager()
        assert manager._skills_dir == tmp_path / "skills"  # noqa: SLF001
        assert manager._skills_dir.exists()  # SkillManager 创建时已 mkdir
    finally:
        reset_skill_manager()


def test_set_skill_manager_for_tests_replaces_singleton(tmp_path: Path) -> None:
    """注入测试用 manager,验证替换生效(也证明全局 singleton 真的可变)。"""
    injected = SkillManager(tmp_path / "injected")
    set_skill_manager_for_tests(injected)
    try:
        assert get_skill_manager() is injected
    finally:
        set_skill_manager_for_tests(None)


# --- Routing dispatch integration ----------------------------------------


async def _dispatch_skill_frame(
    msg_type: str, data: dict, websocket, manager: SkillManager,
    *, authenticated: bool = True,
) -> None:
    """镜像 server.py:_handle_websocket 内 skill.* 分支的派发逻辑。

    包含:
      - pre-auth 闸门(``authenticated=False`` 模拟未完成握手)
      - request_id 透传(handler 入参 + accepted 帧拼接)
      - None -> accepted, dict -> 原样转发的映射

    测试用真 SkillManager + AsyncMock WebSocket;``asyncio.run`` 在每个
    test 里只调一次,本助手内部用 ``await``。
    """
    if not authenticated:
        # 与 server.py:1067 / 1074 / 1085 的前置闸门一致:未认证统一拒收。
        response: dict = {"type": "error", "error": "authenticate first"}
        await websocket.send_json(response)
        return

    request_id = data.get("request_id")

    if msg_type == "skill.list":
        result = await handle_skill_list(manager, request_id)
        await websocket.send_json(result)
    elif msg_type == "skill.enable":
        result = await handle_skill_enable(manager, str(data.get("name", "")), request_id)
        if result is None:
            accepted: dict = {"type": "accepted"}
            if request_id is not None:
                accepted["request_id"] = request_id
            await websocket.send_json(accepted)
        else:
            await websocket.send_json(result)
    elif msg_type == "skill.disable":
        result = await handle_skill_disable(manager, str(data.get("name", "")), request_id)
        if result is None:
            accepted = {"type": "accepted"}
            if request_id is not None:
                accepted["request_id"] = request_id
            await websocket.send_json(accepted)
        else:
            await websocket.send_json(result)


def test_skill_list_routing_sends_result_frame(manager: SkillManager) -> None:
    """完整 list 路由:handler 输出 -> mocked WS 收到 skill.list_result 帧。"""
    _make_skill(manager, "ppt-author", SkillStatus.ENABLED)
    _make_skill(manager, "summarize", SkillStatus.DISABLED)

    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame("skill.list", {}, websocket, manager))

    websocket.send_json.assert_awaited_once()
    sent = websocket.send_json.await_args.args[0]
    assert sent["type"] == "skill.list_result"
    names = sorted(s["name"] for s in sent["skills"])
    assert names == ["ppt-author", "summarize"]


def test_skill_enable_routing_sends_accepted_on_success(manager: SkillManager) -> None:
    """enable 成功路径:handler 返回 None -> WS 收到 {type: accepted}。"""
    _make_skill(manager, "ppt-author", SkillStatus.INSTALLED)

    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame("skill.enable", {"name": "ppt-author"}, websocket, manager))

    websocket.send_json.assert_awaited_once_with({"type": "accepted"})
    assert manager.get_skill("ppt-author").status == SkillStatus.ENABLED


def test_skill_enable_routing_sends_error_on_unknown(manager: SkillManager) -> None:
    """enable 失败路径:handler 返回 error dict -> WS 收到 error 帧原样转发。"""
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame("skill.enable", {"name": "ghost"}, websocket, manager))

    websocket.send_json.assert_awaited_once()
    sent = websocket.send_json.await_args.args[0]
    assert sent["type"] == "error"
    assert "ghost" in sent["message"]


def test_skill_disable_routing_sends_accepted_on_success(manager: SkillManager) -> None:
    """disable 成功路径。"""
    _make_skill(manager, "ppt-author", SkillStatus.ENABLED)

    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame("skill.disable", {"name": "ppt-author"}, websocket, manager))

    websocket.send_json.assert_awaited_once_with({"type": "accepted"})
    assert manager.get_skill("ppt-author").status == SkillStatus.DISABLED


def test_skill_disable_routing_sends_error_on_unknown(manager: SkillManager) -> None:
    """disable 失败路径。"""
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame("skill.disable", {"name": "ghost"}, websocket, manager))

    websocket.send_json.assert_awaited_once()
    sent = websocket.send_json.await_args.args[0]
    assert sent["type"] == "error"


def test_skill_dispatch_uses_module_singleton(tmp_path: Path) -> None:
    """端到端:get_skill_manager() -> handler -> WS 帧,使用真实模块单例。"""
    set_skill_manager_for_tests(SkillManager(tmp_path / "skills"))
    try:
        _make_skill(get_skill_manager(), "ppt-author", SkillStatus.INSTALLED)

        websocket = AsyncMock()
        asyncio.run(_dispatch_skill_frame(
            "skill.enable", {"name": "ppt-author"}, websocket, get_skill_manager(),
        ))

        websocket.send_json.assert_awaited_once_with({"type": "accepted"})
        assert get_skill_manager().get_skill("ppt-author").status == SkillStatus.ENABLED
    finally:
        reset_skill_manager()


# --- request_id 透传 ------------------------------------------------------


def test_handler_attaches_request_id_to_list_result(manager: SkillManager) -> None:
    """ws_skill 内部透传:handler 把 request_id 合并进 list_result 帧。"""
    _make_skill(manager, "ppt-author", SkillStatus.ENABLED)
    result = asyncio.run(handle_skill_list(manager, "req-001"))
    assert result["type"] == "skill.list_result"
    assert result["request_id"] == "req-001"


def test_handler_omits_request_id_when_none(manager: SkillManager) -> None:
    """None 时不污染输出,客户端可据此区分新旧协议。"""
    result = asyncio.run(handle_skill_list(manager))
    assert "request_id" not in result


def test_handler_attaches_request_id_to_enable_error(manager: SkillManager) -> None:
    """enable 失败的 error 帧也必须携带 request_id,便于客户端配对。"""
    result = asyncio.run(handle_skill_enable(manager, "ghost", "req-002"))
    assert result is not None
    assert result["request_id"] == "req-002"


def test_routing_propagates_request_id_on_list(manager: SkillManager) -> None:
    """端到端 list:WS 收到的 list_result 帧带 inbound request_id。"""
    _make_skill(manager, "ppt-author", SkillStatus.ENABLED)
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.list", {"request_id": "req-list-1"}, websocket, manager,
    ))
    sent = websocket.send_json.await_args.args[0]
    assert sent["type"] == "skill.list_result"
    assert sent["request_id"] == "req-list-1"


def test_routing_propagates_request_id_on_enable_success(manager: SkillManager) -> None:
    """enable 成功:accepted 帧拼接 inbound request_id(关键路径)。"""
    _make_skill(manager, "ppt-author", SkillStatus.INSTALLED)
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.enable",
        {"name": "ppt-author", "request_id": "req-enable-1"},
        websocket, manager,
    ))
    websocket.send_json.assert_awaited_once_with({
        "type": "accepted", "request_id": "req-enable-1",
    })


def test_routing_propagates_request_id_on_disable_error(manager: SkillManager) -> None:
    """disable 失败:error 帧带 inbound request_id。"""
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.disable",
        {"name": "ghost", "request_id": "req-disable-1"},
        websocket, manager,
    ))
    sent = websocket.send_json.await_args.args[0]
    assert sent["type"] == "error"
    assert sent["request_id"] == "req-disable-1"
    assert "ghost" in sent["message"]


def test_routing_omits_request_id_when_absent(manager: SkillManager) -> None:
    """请求未带 request_id 时,响应帧也不应出现该键(None 透传而非空字符串)。"""
    _make_skill(manager, "ppt-author", SkillStatus.INSTALLED)
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.enable", {"name": "ppt-author"}, websocket, manager,
    ))
    sent = websocket.send_json.await_args.args[0]
    assert sent == {"type": "accepted"}


# --- pre-auth 闸门 ------------------------------------------------------


def test_pre_auth_gate_rejects_skill_list(manager: SkillManager) -> None:
    """未认证时 skill.list 直接返回 authenticate first,handler 不被调用。"""
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.list", {}, websocket, manager, authenticated=False,
    ))
    websocket.send_json.assert_awaited_once_with({
        "type": "error", "error": "authenticate first",
    })


def test_pre_auth_gate_rejects_skill_enable(manager: SkillManager) -> None:
    """未认证时 skill.enable 同样拒收,且不调用 handler(状态不应改变)。"""
    _make_skill(manager, "ppt-author", SkillStatus.INSTALLED)
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.enable", {"name": "ppt-author"}, websocket, manager, authenticated=False,
    ))
    websocket.send_json.assert_awaited_once_with({
        "type": "error", "error": "authenticate first",
    })
    assert manager.get_skill("ppt-author").status == SkillStatus.INSTALLED  # 未触发


def test_pre_auth_gate_rejects_skill_disable(manager: SkillManager) -> None:
    """未认证时 skill.disable 也走闸门。"""
    _make_skill(manager, "ppt-author", SkillStatus.ENABLED)
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.disable", {"name": "ppt-author"}, websocket, manager, authenticated=False,
    ))
    websocket.send_json.assert_awaited_once_with({
        "type": "error", "error": "authenticate first",
    })
    assert manager.get_skill("ppt-author").status == SkillStatus.ENABLED  # 未触发