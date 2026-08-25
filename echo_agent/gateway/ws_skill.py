"""WS 协议扩展:Skills 管理消息 handler。

接收 Desktop 端通过 WS 发来的 skill.list / skill.enable / skill.disable,
直接调用 SkillManager 完成操作,返回结构化响应。

返回值约定:
  - 成功:None(调用方路由走 accepted ack,并自己负责拼 request_id)
  - list 类型: dict(含 skills 数组,如有 request_id 则合并)
  - 错误: {"type": "error", "message": "..."} ,如有 request_id 则合并
"""
from __future__ import annotations

from typing import Any

from echo_agent.skills.manager import SkillManager


def _attach_request_id(frame: dict[str, Any], request_id: str | None) -> dict[str, Any]:
    """若 request_id 非空则写入响应帧,None 时不污染输出(让客户端区分新旧协议)。"""
    if request_id is None:
        return frame
    return {**frame, "request_id": request_id}


async def handle_skill_list(
    manager: SkillManager, request_id: str | None = None,
) -> dict:
    """返回已安装 skills 清单及其状态。

    字段与 SkillManifest 对齐:name / version / description / author /
    scope / dependencies / config_schema;另带 status 字段。
    """
    skills = manager.list_skills()
    return _attach_request_id(
        {
            "type": "skill.list_result",
            "skills": [
                {
                    "name": s.manifest.name,
                    "version": s.manifest.version,
                    "description": s.manifest.description,
                    "author": s.manifest.author,
                    "scope": s.manifest.scope,
                    "status": s.status.value,
                    "dependencies": s.manifest.dependencies,
                    "config_schema": s.manifest.config_schema,
                }
                for s in skills
            ],
        },
        request_id,
    )


async def handle_skill_enable(
    manager: SkillManager, name: str, request_id: str | None = None,
) -> dict | None:
    """启用 skill。失败(不存在 / 依赖未满足)返回 error 帧。"""
    if manager.enable(name):
        return None
    return _attach_request_id(
        {"type": "error", "message": f"failed to enable skill: {name}"},
        request_id,
    )


async def handle_skill_disable(
    manager: SkillManager, name: str, request_id: str | None = None,
) -> dict | None:
    """禁用 skill。不存在时返回 error 帧。"""
    if manager.disable(name):
        return None
    return _attach_request_id(
        {"type": "error", "message": f"failed to disable skill: {name}"},
        request_id,
    )