"""Tests for skill-related WS handlers."""
from __future__ import annotations

import asyncio
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from echo_agent.gateway.ws_skill import (
    handle_skill_disable,
    handle_skill_enable,
    handle_skill_list,
)
from echo_agent.skills.store import SkillStore


@pytest.fixture
def skills_dir(tmp_path: Path) -> Path:
    return tmp_path / "skills"


@pytest.fixture
def store(skills_dir: Path) -> SkillStore:
    return SkillStore(user_dir=skills_dir)


def _make_skill(store: SkillStore, name: str, *, enabled: bool = True) -> None:
    """在 store 的 user_dir 下落一个真实技能目录。

    走 create_skill 而非直接写文件,确保 frontmatter 形状与 store 的解析口径
    一致;禁用态用 persist_disable 表达(与生产同一条路径)。
    """
    content = (
        f"---\nname: {name}\ndescription: desc-{name}\n---\n\n# {name}\n"
    )
    error = store.create_skill(name, content)
    assert error is None, error
    if not enabled:
        store.persist_disable(name)


def test_handle_skill_list_returns_empty_when_no_skills(store: SkillStore) -> None:
    """无 skill 时返回空数组。"""
    result = asyncio.run(handle_skill_list(store))
    assert result["type"] == "skill.list_result"
    assert result["skills"] == []


def test_handle_skill_list_returns_all_skills(store: SkillStore) -> None:
    """返回技能列表,含已禁用者(管理视图)。"""
    _make_skill(store, "ppt-author")
    _make_skill(store, "summarize", enabled=False)
    result = asyncio.run(handle_skill_list(store))
    names = [s["name"] for s in result["skills"]]
    assert names == ["ppt-author", "summarize"]


def test_handle_skill_list_includes_enabled_flag(store: SkillStore) -> None:
    """每条 skill 携带 enabled 布尔值,与 HTTP api/skills 形状一致。"""
    _make_skill(store, "ppt-author")
    _make_skill(store, "summarize", enabled=False)
    result = asyncio.run(handle_skill_list(store))
    by_name = {s["name"]: s for s in result["skills"]}
    assert by_name["ppt-author"]["enabled"] is True
    assert by_name["summarize"]["enabled"] is False
    assert by_name["ppt-author"]["version"] == "1.0.0"
    assert "desc-ppt-author" in by_name["ppt-author"]["description"]


def test_handle_skill_enable_success(store: SkillStore) -> None:
    """启用成功返回 None(走 accepted ack),且状态真的翻转。"""
    _make_skill(store, "ppt-author", enabled=False)
    assert asyncio.run(handle_skill_enable(store, "ppt-author")) is None
    assert store.is_disabled("ppt-author") is False


def test_handle_skill_enable_persists_across_instances(skills_dir: Path) -> None:
    """启用要跨重启存活 —— 换一个 store 实例读同一目录仍应看到结果。"""
    first = SkillStore(user_dir=skills_dir)
    _make_skill(first, "ppt-author", enabled=False)
    assert asyncio.run(handle_skill_enable(first, "ppt-author")) is None

    reloaded = SkillStore(user_dir=skills_dir)
    assert reloaded.is_disabled("ppt-author") is False


def test_handle_skill_enable_unknown_returns_error(store: SkillStore) -> None:
    """启用不存在的 skill 返回 error 帧。"""
    result = asyncio.run(handle_skill_enable(store, "ghost"))
    assert result is not None
    assert result["type"] == "error"
    assert "ghost" in result["message"]


def test_handle_skill_enable_unknown_leaves_no_disable_entry(store: SkillStore) -> None:
    """拼错的名字不得留下 disable 记录,否则会把该名字给未来的技能占死。"""
    asyncio.run(handle_skill_disable(store, "ghost"))
    _make_skill(store, "ghost")
    assert store.is_disabled("ghost") is False


def test_handle_skill_disable_success(store: SkillStore) -> None:
    """禁用成功。"""
    _make_skill(store, "ppt-author")
    assert asyncio.run(handle_skill_disable(store, "ppt-author")) is None
    assert store.is_disabled("ppt-author") is True


def test_handle_skill_disable_persists_across_instances(skills_dir: Path) -> None:
    """禁用同样要跨实例存活。"""
    first = SkillStore(user_dir=skills_dir)
    _make_skill(first, "ppt-author")
    assert asyncio.run(handle_skill_disable(first, "ppt-author")) is None

    reloaded = SkillStore(user_dir=skills_dir)
    assert reloaded.is_disabled("ppt-author") is True


def test_handle_skill_disable_unknown_returns_error(store: SkillStore) -> None:
    """禁用不存在的 skill 返回 error。"""
    result = asyncio.run(handle_skill_disable(store, "ghost"))
    assert result is not None
    assert result["type"] == "error"


# --- request_id 透传 ------------------------------------------------------


def test_request_id_absent_keeps_frame_clean(store: SkillStore) -> None:
    """不带 request_id 时响应帧里不得出现该键(让客户端区分新旧协议)。"""
    result = asyncio.run(handle_skill_list(store))
    assert "request_id" not in result


def test_request_id_echoed_on_list(store: SkillStore) -> None:
    """list 结果回带 request_id。"""
    result = asyncio.run(handle_skill_list(store, "req-1"))
    assert result["request_id"] == "req-1"


def test_request_id_echoed_on_error(store: SkillStore) -> None:
    """error 帧同样回带 request_id。"""
    result = asyncio.run(handle_skill_enable(store, "ghost", "req-2"))
    assert result is not None
    assert result["request_id"] == "req-2"


# --- Routing dispatch integration ----------------------------------------


async def _dispatch_skill_frame(
    msg_type: str, data: dict, websocket, store: SkillStore | None,
    *, authenticated: bool = True, is_admin: bool = True,
) -> None:
    """镜像 server.py:_handle_websocket 内 skill.* 分支的派发逻辑。

    包含:
      - pre-auth 闸门(``authenticated=False`` 模拟未完成握手)
      - skills 系统关闭(``store=None``)
      - 写操作的 admin 作用域闸门(``is_admin=False``)
      - request_id 透传(handler 入参 + accepted 帧拼接)
      - None -> accepted, dict -> 原样转发的映射

    测试用真 SkillStore + AsyncMock WebSocket;``asyncio.run`` 在每个
    test 里只调一次,本助手内部用 ``await``。
    """
    if not authenticated:
        # 与 server.py skill.* 分支的前置闸门一致:未认证统一拒收。
        await websocket.send_json({"type": "error", "error": "authenticate first"})
        return

    request_id = data.get("request_id")

    def _err(message: str) -> dict:
        frame = {"type": "error", "message": message}
        if request_id is not None:
            frame["request_id"] = request_id
        return frame

    if store is None:
        await websocket.send_json(
            _err("skills system is disabled (skills.enabled=false)"))
        return

    if msg_type != "skill.list" and not is_admin:
        await websocket.send_json(_err("admin token required"))
        return

    if msg_type == "skill.list":
        await websocket.send_json(await handle_skill_list(store, request_id))
        return

    handler = (
        handle_skill_enable if msg_type == "skill.enable" else handle_skill_disable
    )
    result = await handler(store, str(data.get("name", "")), request_id)
    if result is None:
        accepted: dict = {"type": "accepted"}
        if request_id is not None:
            accepted["request_id"] = request_id
        await websocket.send_json(accepted)
    else:
        await websocket.send_json(result)


def test_skill_list_routing_sends_result_frame(store: SkillStore) -> None:
    """完整 list 路由:handler 输出 -> mocked WS 收到 skill.list_result 帧。"""
    _make_skill(store, "ppt-author")
    _make_skill(store, "summarize", enabled=False)

    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame("skill.list", {}, websocket, store))

    sent = websocket.send_json.await_args.args[0]
    assert sent["type"] == "skill.list_result"
    assert [s["name"] for s in sent["skills"]] == ["ppt-author", "summarize"]


def test_skill_enable_routing_sends_accepted(store: SkillStore) -> None:
    """enable 成功 -> accepted 帧(handler 返回 None)。"""
    _make_skill(store, "ppt-author", enabled=False)

    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.enable", {"name": "ppt-author"}, websocket, store,
    ))
    assert websocket.send_json.await_args.args[0] == {"type": "accepted"}


def test_skill_enable_routing_echoes_request_id_on_accepted(store: SkillStore) -> None:
    """accepted 帧要带上 request_id(由路由层拼接,不是 handler 给的)。"""
    _make_skill(store, "ppt-author", enabled=False)

    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.enable", {"name": "ppt-author", "request_id": "r-9"}, websocket, store,
    ))
    assert websocket.send_json.await_args.args[0] == {
        "type": "accepted", "request_id": "r-9",
    }


def test_skill_disable_routing_forwards_error_frame(store: SkillStore) -> None:
    """disable 失败 -> error 帧原样转发。"""
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.disable", {"name": "ghost"}, websocket, store,
    ))
    sent = websocket.send_json.await_args.args[0]
    assert sent["type"] == "error"
    assert "ghost" in sent["message"]


# --- skills 系统关闭 ----------------------------------------------------


def test_disabled_skills_system_reports_error_on_list() -> None:
    """skills.enabled=false 时 store 为 None,应如实回错而非抛异常。"""
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame("skill.list", {}, websocket, None))
    sent = websocket.send_json.await_args.args[0]
    assert sent["type"] == "error"
    assert "disabled" in sent["message"]


def test_disabled_skills_system_reports_error_on_enable() -> None:
    """写操作在 skills 关闭时同样拒收。"""
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.enable", {"name": "ppt-author"}, websocket, None,
    ))
    assert websocket.send_json.await_args.args[0]["type"] == "error"


# --- admin 作用域闸门 ---------------------------------------------------


def test_non_admin_cannot_enable_skill(store: SkillStore) -> None:
    """enable 与 HTTP toggle 同属高危写操作,非 admin 令牌必须被拒。"""
    _make_skill(store, "ppt-author", enabled=False)

    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.enable", {"name": "ppt-author"}, websocket, store, is_admin=False,
    ))
    sent = websocket.send_json.await_args.args[0]
    assert sent["type"] == "error"
    assert "admin" in sent["message"]
    # 关键:状态不得改变
    assert store.is_disabled("ppt-author") is True


def test_non_admin_cannot_disable_skill(store: SkillStore) -> None:
    """disable 同样要求 admin。"""
    _make_skill(store, "ppt-author")

    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.disable", {"name": "ppt-author"}, websocket, store, is_admin=False,
    ))
    assert websocket.send_json.await_args.args[0]["type"] == "error"
    assert store.is_disabled("ppt-author") is False


def test_non_admin_can_still_list_skills(store: SkillStore) -> None:
    """list 是只读,api 作用域即可,不应被 admin 闸门挡住。"""
    _make_skill(store, "ppt-author")

    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.list", {}, websocket, store, is_admin=False,
    ))
    assert websocket.send_json.await_args.args[0]["type"] == "skill.list_result"


# --- pre-auth 闸门 ------------------------------------------------------


def test_pre_auth_gate_rejects_skill_list(store: SkillStore) -> None:
    """未认证时 skill.list 直接返回 authenticate first,handler 不被调用。"""
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.list", {}, websocket, store, authenticated=False,
    ))
    websocket.send_json.assert_awaited_once_with({
        "type": "error", "error": "authenticate first",
    })


def test_pre_auth_gate_rejects_skill_enable(store: SkillStore) -> None:
    """未认证时 skill.enable 同样拒收,且不调用 handler(状态不应改变)。"""
    _make_skill(store, "ppt-author", enabled=False)
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.enable", {"name": "ppt-author"}, websocket, store, authenticated=False,
    ))
    websocket.send_json.assert_awaited_once_with({
        "type": "error", "error": "authenticate first",
    })
    assert store.is_disabled("ppt-author") is True  # 未触发


def test_pre_auth_gate_rejects_skill_disable(store: SkillStore) -> None:
    """未认证时 skill.disable 也走闸门。"""
    _make_skill(store, "ppt-author")
    websocket = AsyncMock()
    asyncio.run(_dispatch_skill_frame(
        "skill.disable", {"name": "ppt-author"}, websocket, store, authenticated=False,
    ))
    websocket.send_json.assert_awaited_once_with({
        "type": "error", "error": "authenticate first",
    })
    assert store.is_disabled("ppt-author") is False  # 未触发
