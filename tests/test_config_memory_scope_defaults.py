from __future__ import annotations

from echo_agent.config.schema import MemoryConfig


def test_cross_channel_owner_defaults_off():
    # 认证绑定(Phase 1)就绪前,私聊不得全局合并到单一 owner,默认必须关闭。
    assert MemoryConfig().cross_channel_owner is False
