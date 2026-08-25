"""WS 协议扩展:Skills 管理消息 handler。

接收 Desktop 端通过 WS 发来的 skill.list / skill.enable / skill.disable,
直接调用 SkillManager 完成操作,返回结构化响应。

返回值约定:
  - 成功:None(调用方路由走 accepted ack)
  - list 类型: dict(含 skills 数组)
  - 错误: {"type": "error", "message": "..."}
"""
from __future__ import annotations

from echo_agent.skills.manager import SkillManager


async def handle_skill_list(manager: SkillManager) -> dict:
    """返回已安装 skills 清单及其状态。

    字段与 SkillManifest 对齐:name / version / description / author /
    scope / dependencies / config_schema;另带 status 字段。
    """
    skills = manager.list_skills()
    return {
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
    }


async def handle_skill_enable(manager: SkillManager, name: str) -> dict | None:
    """启用 skill。失败(不存在 / 依赖未满足)返回 error 帧。"""
    if manager.enable(name):
        return None
    return {"type": "error", "message": f"failed to enable skill: {name}"}


async def handle_skill_disable(manager: SkillManager, name: str) -> dict | None:
    """禁用 skill。不存在时返回 error 帧。"""
    if manager.disable(name):
        return None
    return {"type": "error", "message": f"failed to disable skill: {name}"}