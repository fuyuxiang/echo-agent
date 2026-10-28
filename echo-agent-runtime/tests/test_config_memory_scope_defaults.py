from __future__ import annotations

import pytest
from pydantic import ValidationError

from echo_agent.config.schema import MemoryConfig


def test_cross_channel_owner_defaults_on():
    # Phase 1 绑定表就绪后,归一已受 principal_bindings 约束(未列入者按会话隔离),
    # 默认改回开启;是否有 sender 归一到 owner 取决于绑定表而非开关本身。
    assert MemoryConfig().cross_channel_owner is True


def test_owner_key_rejects_empty():
    # 空 owner_key 会使私聊 scope 变空串,store 对空 key fail-open 放行全库,须拒绝。
    with pytest.raises(ValidationError):
        MemoryConfig(owner_key="")


def test_owner_key_rejects_whitespace():
    with pytest.raises(ValidationError):
        MemoryConfig(owner_key="   ")


def test_owner_key_accepts_normal():
    assert MemoryConfig(owner_key="owner").owner_key == "owner"


def test_principal_bindings_defaults_empty():
    assert MemoryConfig().principal_bindings == []


def test_principal_bindings_accepts_valid():
    b = ["telegram:alice", "slack:U0XXX"]
    assert MemoryConfig(principal_bindings=b).principal_bindings == b


def test_principal_bindings_rejects_no_colon():
    with pytest.raises(ValidationError):
        MemoryConfig(principal_bindings=["telegramalice"])


def test_principal_bindings_rejects_empty_side():
    with pytest.raises(ValidationError):
        MemoryConfig(principal_bindings=["telegram:"])
    with pytest.raises(ValidationError):
        MemoryConfig(principal_bindings=[":alice"])


def test_principal_bindings_normalizes_whitespace():
    m = MemoryConfig(principal_bindings=["telegram: alice", " slack :U0XXX"])
    assert m.principal_bindings == ["telegram:alice", "slack:U0XXX"]
