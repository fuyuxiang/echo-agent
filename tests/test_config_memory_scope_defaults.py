from __future__ import annotations

import pytest
from pydantic import ValidationError

from echo_agent.config.schema import MemoryConfig


def test_cross_channel_owner_defaults_off():
    # 认证绑定(Phase 1)就绪前,私聊不得全局合并到单一 owner,默认必须关闭。
    assert MemoryConfig().cross_channel_owner is False


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
