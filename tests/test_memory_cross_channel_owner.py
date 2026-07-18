"""工单② 跨通道记忆互通 + 群聊隔离 回归测试。

验证 memory_scope_key 的作用域派生与 MemoryStore 可见性/去重谓词在
scope_policy='session' 下的行为：
- 1:1 私聊(任意通道) 归一到 owner 键 -> 主人 USER 记忆跨通道互通
- 群聊 per_user -> 群成员之间、群成员与主人之间双向隔离(隐私护栏)
- _same_scope 去重按 owner 键合并,不与群键误并
"""
from pathlib import Path

from echo_agent.bus.events import InboundEvent
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType, MemoryTier


def _store(tmp_path: Path) -> MemoryStore:
    return MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")


def _user_entry(scope: str, content: str = "fact") -> MemoryEntry:
    return MemoryEntry(type=MemoryType.USER, tier=MemoryTier.SEMANTIC,
                       key="pref", content=content, source_session=scope)


# ── memory_scope_key 单元行为 ────────────────────────────────────

def test_memory_scope_key_1to1_returns_owner():
    ev = InboundEvent.text_message(channel="cli", sender_id="cli", chat_id="cli", text="x")
    assert ev.memory_scope_key("per_user", "owner") == "owner"


def test_memory_scope_key_1to1_gateway_returns_owner():
    # gateway 私聊键是 3 段(gateway:wechat:uid),与群 per_user 3 段重叠,
    # 但 is_group=False 时必须归一到 owner。
    ev = InboundEvent.text_message(channel="gateway:wechat", sender_id="u1", chat_id="u1", text="x")
    assert ev.memory_scope_key("per_user", "owner") == "owner"


def test_memory_scope_key_group_keeps_per_user_isolation():
    ev = InboundEvent.text_message(channel="telegram", sender_id="alice",
                                   chat_id="grp1", text="x", is_group=True)
    assert ev.memory_scope_key("per_user", "owner") == "telegram:grp1:alice"


# ── 跨通道互通(核心特性) ─────────────────────────────────────────

def test_owner_memory_visible_across_channels(tmp_path):
    store = _store(tmp_path)
    entry = _user_entry("owner")
    # 任意 1:1 私聊(memory_scope 解析为 owner)都能看到主人的 USER 记忆。
    assert store.is_visible_in_session(entry, "owner") is True


# ── 群聊隔离保持(安全护栏) ───────────────────────────────────────

def test_group_member_memory_not_visible_to_owner(tmp_path):
    store = _store(tmp_path)
    entry = _user_entry("telegram:grp1:alice")
    # 主人的 owner 作用域看不到群成员 alice 的私有记忆。
    assert store.is_visible_in_session(entry, "owner") is False


def test_owner_memory_not_leaked_into_group(tmp_path):
    store = _store(tmp_path)
    entry = _user_entry("owner")
    # owner 记忆在群聊 per_user 上下文不可见 -> 陌生人看不到主人全局记忆。
    assert store.is_visible_in_session(entry, "telegram:grp1:bob") is False


def test_group_members_isolated_from_each_other(tmp_path):
    store = _store(tmp_path)
    entry = _user_entry("telegram:grp1:alice")
    assert store.is_visible_in_session(entry, "telegram:grp1:bob") is False
    assert store.is_visible_in_session(entry, "telegram:grp1:alice") is True


# ── _same_scope 去重 ─────────────────────────────────────────────

def test_same_scope_owner_facts_merge(tmp_path):
    store = _store(tmp_path)
    a = _user_entry("owner", "likes dark mode")
    b = _user_entry("owner", "likes dark mode v2")
    assert store._same_scope(a, b) is True


def test_same_scope_owner_vs_group_no_merge(tmp_path):
    store = _store(tmp_path)
    a = _user_entry("owner")
    b = _user_entry("telegram:grp1:alice")
    assert store._same_scope(a, b) is False


# ── 开关退化 ─────────────────────────────────────────────────────

def test_feature_off_falls_back_to_session_key():
    # 开关关闭时 loop 把 memory_scope 赋为 session_key;此处验证 helper 语义:
    # is_group=False 的 scoped_session_key 返回 channel:chat_id。
    ev = InboundEvent.text_message(channel="cli", sender_id="cli", chat_id="cli", text="x")
    assert ev.scoped_session_key("per_user") == "cli:cli"
