"""spill 产物的本地落盘。

按 sha256(session_key) 分子目录：session_key 可能含通道名、chat id 等
不适合直接做路径段的字符，哈希掉最省事，同会话产物聚集也让清扫能整目录处理。
文件名带随机前缀并用独占创建，防的是符号链接抢占——workspace 是私有目录,
这层威胁本就低,但成本近乎零。
"""

from __future__ import annotations

import os
import re
import secrets
from pathlib import Path

from echo_agent.spill.layout import session_dir_name

_UNSAFE = re.compile(r"[^A-Za-z0-9._-]")
_MAX_SEGMENT = 64


def _safe_segment(name: str) -> str:
    """把任意名字清洗成单个安全路径段。绝不返回空串或 . / .."""
    cleaned = _UNSAFE.sub("_", name).strip("._")[:_MAX_SEGMENT]
    return cleaned or "artifact"


class SpillStore:
    """把超长工具输出写进按会话分的私有目录。"""

    def __init__(self, root: Path):
        self.root = Path(root).resolve()

    def session_dir(self, session_key: str) -> Path:
        """本会话的产物目录。read_spill 用它做授权边界,故必须与 save 同源。"""
        return self.root / session_dir_name(session_key)

    def save(self, session_key: str, tool_name: str, content: str) -> Path:
        session_dir = self.session_dir(session_key)
        session_dir.mkdir(parents=True, exist_ok=True)
        name = f"{secrets.token_hex(4)}-{_safe_segment(tool_name)}.txt"
        path = session_dir / name
        # 独占创建 + 仅属主可读：目标已存在（含符号链接）时直接失败,
        # planted target 无法把写入重定向到别处。0o600 在 win32 上不产生
        # 等效 ACL 效果,故只在 POSIX 上构成权限保证。
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as f:
            f.write(content)
        return path
