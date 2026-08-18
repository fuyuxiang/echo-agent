"""识别"曾是 spill 产物、现已被清扫"的路径,给模型一条读得懂的提示。

harness 靠"永不删除"绕过了这个问题,常驻服务不能。
"""

from __future__ import annotations

from pathlib import Path

from echo_agent.security.path_policy import resolve_path

# 不能只说"已过保留期":产物也可能因总体积上限在当天就被按最旧优先删掉,
# 那时告诉模型"过期了"会让它据此推断产物年龄,而那个推断是错的。
EXPIRED_NOTICE = (
    "该 spill 产物已因保留期或容量上限被清理,无法取回。"
    "如需完整内容请重新执行产生它的原命令。"
)
_NOTICE = EXPIRED_NOTICE  # 旧名保留:内部调用点已在用,不值得为改名动一圈。


def expired_notice(path: str, workspace: str, spill_root: Path | None) -> str | None:
    """路径确属 spill 根目录内、且文件已不存在时返回提示,否则 None。"""
    if spill_root is None:
        return None
    try:
        resolved = resolve_path(path, workspace)
        root = Path(spill_root).resolve()
    except (OSError, ValueError):
        return None
    # 用解析后路径比对,不能用字符串前缀
    if root not in resolved.parents:
        return None
    if resolved.exists():
        return None
    return _NOTICE
