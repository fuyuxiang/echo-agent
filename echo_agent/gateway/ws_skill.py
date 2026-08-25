"""WS 协议扩展:Skills 管理消息 handler。

接收 Desktop 端通过 WS 发来的 skill.list / skill.enable / skill.disable,
调用 ``SkillStore`` 完成操作,返回结构化响应。

store 由调用方从 ``agent_loop.skill_store`` 取,与 HTTP ``api/skills.py``
共用同一个实例:两条路径必须看到同一份技能集合,且同样受
``skills.enabled=false``(store 为 None)约束。

返回值约定:
  - 成功:None(调用方路由走 accepted ack,并自己负责拼 request_id)
  - list 类型: dict(含 skills 数组,如有 request_id 则合并)
  - 错误: {"type": "error", "message": "..."} ,如有 request_id 则合并
"""
from __future__ import annotations

from typing import Any

from echo_agent.skills.store import SkillStore


def _attach_request_id(frame: dict[str, Any], request_id: str | None) -> dict[str, Any]:
    """若 request_id 非空则写入响应帧,None 时不污染输出(让客户端区分新旧协议)。"""
    if request_id is None:
        return frame
    return {**frame, "request_id": request_id}


async def handle_skill_list(
    store: SkillStore, request_id: str | None = None,
) -> dict:
    """返回技能清单及其启用状态。

    字段与 ``SkillMeta.to_dict()`` 对齐(name / description / category /
    version / tags),另带 ``enabled`` 布尔值 —— 与 HTTP
    ``GET /api/skills`` 的 ``_list_all_with_status`` 保持同一形状,
    避免两条路径对同一状态给出不同表示。

    include_disabled=True:这是管理视图,已禁用的技能正是操作者要看到并
    决定是否启用的对象。
    """
    skills: list[dict[str, Any]] = []
    for meta in store.list_all(include_disabled=True):
        item = meta.to_dict()
        item["enabled"] = not store.is_disabled(meta.name)
        skills.append(item)
    skills.sort(key=lambda m: m["name"])
    return _attach_request_id(
        {"type": "skill.list_result", "skills": skills},
        request_id,
    )


def _missing_skill_error(
    store: SkillStore, name: str, request_id: str | None,
) -> dict[str, Any] | None:
    """磁盘上不存在该名字时返回 error 帧。

    与 HTTP toggle 同理:拒绝对不存在的名字下手,否则一个拼写错误会为幽灵
    技能留下永久的 disable 记录,把这个名字给未来真正安装的技能占死。
    """
    if store.find_skill_dir(name, include_disabled=True) is None:
        return _attach_request_id(
            {"type": "error", "message": f"skill not found: {name}"},
            request_id,
        )
    return None


async def handle_skill_enable(
    store: SkillStore, name: str, request_id: str | None = None,
) -> dict | None:
    """启用 skill。不存在时返回 error 帧。

    走 ``persist_enable``,使启用状态跨重启存活 —— 仅改内存集合的话进程一退
    就蒸发了。
    """
    error = _missing_skill_error(store, name, request_id)
    if error is not None:
        return error
    store.persist_enable(name)
    return None


async def handle_skill_disable(
    store: SkillStore, name: str, request_id: str | None = None,
) -> dict | None:
    """禁用 skill。不存在时返回 error 帧。"""
    error = _missing_skill_error(store, name, request_id)
    if error is not None:
        return error
    store.persist_disable(name)
    return None
