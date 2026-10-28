"""spill 目录布局:产物路径的形状定义,由 store / sweeper / read_spill 共用。

单独成模块的理由是这三者必须对"什么算 spill 产物"给出同一个答案:
store 按这个形状写,sweeper 只删符合这个形状的,read_spill 只允许读符合这个
形状且属于本会话的。三处各写一遍正则,迟早会漂移成互不认账。
"""

from __future__ import annotations

import hashlib
import re
from pathlib import Path

# 会话目录:session- 后接 sha256(session_key) 的前 12 位十六进制。
SESSION_DIR_GLOB = "session-*"
_SESSION_DIR_RE = re.compile(r"^session-[0-9a-f]{12}$")
# 产物文件:8 位十六进制随机前缀 + 清洗后的工具名 + .txt。
_ARTIFACT_RE = re.compile(r"^[0-9a-f]{8}-[A-Za-z0-9._-]{1,64}\.txt$")


def session_dir_name(session_key: str) -> str:
    """会话目录名。session_key 可能含通道名、chat id 等不适合做路径段的字符。"""
    digest = hashlib.sha256(session_key.encode("utf-8")).hexdigest()[:12]
    return f"session-{digest}"


def is_session_dir(path: Path) -> bool:
    return bool(_SESSION_DIR_RE.match(path.name))


def is_artifact(path: Path) -> bool:
    """路径是否形如一个 spill 产物文件(只看名字,不碰文件系统)。

    父目录也要是会话目录:否则 spillDir 被误配到源码树时,恰好叫
    ``1a2b3c4d-x.txt`` 的无关文件会被当成产物删掉。
    """
    return bool(_ARTIFACT_RE.match(path.name)) and is_session_dir(path.parent)
