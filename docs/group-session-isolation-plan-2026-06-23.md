# 群聊会话隔离 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 群聊默认按 sender 隔离会话，消除群内多用户串话；保留 shared 模式作为可配置项。

**Architecture:** `InboundEvent` 增加纯数据字段 `is_group`，由各通道归一化填入。会话作用域不在 property 内读 config（保持 InboundEvent 为纯数据），而是在 `loop._on_inbound` 这个唯一入站汇聚点把群聊 per_user 的 key 解析为 `channel:chat_id:sender_id` 并写入 `session_key_override`——该字段已是 `session_key` property 的最高优先级来源，因此一处解析即自动传播到全部约 15 个下游消费点（session 锁 / working memory / 记忆快照 / 可见性过滤 / source_session）。投递侧的 session_key 反解器同步剥离 sender 后缀以还原群 chat_id。

**Tech Stack:** Python 3.11+、dataclass、pydantic（config schema）、pytest。

## Global Constraints

- 测试统一用 `python -m pytest`（站点包遮蔽本地源，直接 `pytest` 可能跑到错误副本）。
- 源码根在 `echo_agent/`（非 `src/`）。
- commit message 不用 `feat:`/`fix:` 等约定式前缀，直接写中文改动描述（本仓库惯例）。
- 新增 config 字段必须带 `json_schema_extra={"status": "effective", "ref": ..., "desc_zh": ..., "desc_en": ...}`（本项目死字段治理要求），并同步进 `default.yaml` 与 `docs/config-reference.*`。
- 默认值 `group_session_scope=per_user`（安全默认，群内每人独立）。
- 威胁模型 local-first / trusted-operator：反解器可假设 group-capable 通道的 chat_id 不含冒号。
- 不进 setup 向导（与同类 `group_policy` 配置一致）。
- 不迁移历史群聊会话（自然过渡）。

---

### Task 1: InboundEvent 增加 is_group 字段与群聊 session_key 解析 helper

**Files:**
- Modify: `echo_agent/bus/events.py:57-77`（`InboundEvent` 字段 + 新增 staticmethod）
- Test: `tests/test_group_session_isolation.py`（新建）

**Interfaces:**
- Produces:
  - `InboundEvent.is_group: bool = False`（新 dataclass 字段）
  - `InboundEvent.scoped_session_key(scope: str) -> str` —— 实例方法。`scope == "per_user" and self.is_group and self.sender_id` 时返回 `f"{channel}:{chat_id}:{sender_id}"`，否则返回 `self.session_key`（即 `channel:chat_id`）。`session_key_override` 存在时仍最高优先级（直接返回它）。

- [ ] **Step 1: Write the failing test**

```python
# tests/test_group_session_isolation.py
from __future__ import annotations

from echo_agent.bus.events import InboundEvent


def _evt(sender_id: str, chat_id: str, is_group: bool) -> InboundEvent:
    return InboundEvent.text_message(
        channel="telegram", sender_id=sender_id, chat_id=chat_id,
        text="hi", is_group=is_group,
    )


def test_private_chat_key_never_includes_sender():
    evt = _evt("u1", "c1", is_group=False)
    assert evt.scoped_session_key("per_user") == "telegram:c1"
    assert evt.scoped_session_key("shared") == "telegram:c1"


def test_group_per_user_splits_by_sender():
    a = _evt("alice", "grp1", is_group=True)
    b = _evt("bob", "grp1", is_group=True)
    assert a.scoped_session_key("per_user") == "telegram:grp1:alice"
    assert b.scoped_session_key("per_user") == "telegram:grp1:bob"
    assert a.scoped_session_key("per_user") != b.scoped_session_key("per_user")


def test_group_shared_keeps_single_key():
    a = _evt("alice", "grp1", is_group=True)
    b = _evt("bob", "grp1", is_group=True)
    assert a.scoped_session_key("shared") == "telegram:grp1"
    assert b.scoped_session_key("shared") == "telegram:grp1"


def test_group_per_user_empty_sender_falls_back():
    evt = _evt("", "grp1", is_group=True)
    assert evt.scoped_session_key("per_user") == "telegram:grp1"


def test_override_wins_over_scope():
    evt = InboundEvent.text_message(
        channel="telegram", sender_id="alice", chat_id="grp1",
        text="hi", is_group=True, session_key_override="custom:key",
    )
    assert evt.scoped_session_key("per_user") == "custom:key"
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_group_session_isolation.py -v`
Expected: FAIL —— `TypeError: ... unexpected keyword argument 'is_group'`（字段尚不存在）。

- [ ] **Step 3: Add the field and method**

在 `echo_agent/bus/events.py` 的 `InboundEvent` 中，于 `gateway_metadata` 字段后新增 `is_group` 字段（line ~71 之后）：

```python
    gateway_metadata: dict[str, Any] = field(default_factory=dict)
    is_group: bool = False
```

在 `session_key` property 之后新增方法：

```python
    def scoped_session_key(self, scope: str) -> str:
        """会话作用域键。私聊及 shared 策略下等同 session_key；
        群聊 + per_user 策略时把 sender_id 纳入键，实现群内每人隔离。"""
        if self.session_key_override:
            return self.session_key_override
        base = f"{self.channel}:{self.chat_id}"
        if scope == "per_user" and self.is_group and self.sender_id:
            return f"{base}:{self.sender_id}"
        return base
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_group_session_isolation.py -v`
Expected: PASS（5 passed）。

- [ ] **Step 5: Commit**

```bash
git add echo_agent/bus/events.py tests/test_group_session_isolation.py
git commit -m "InboundEvent 增加 is_group 字段与群聊作用域键解析"
```

---

### Task 2: loop 入站汇聚点按 group_session_scope 解析会话键

**Files:**
- Modify: `echo_agent/config/schema.py:1744`（`SessionConfig` 增加 `group_session_scope`）
- Modify: `echo_agent/config/default.yaml`（session 段补字段）
- Modify: `echo_agent/agent/loop.py:597-620`（`_on_inbound` 入口解析）
- Test: `tests/test_group_session_isolation.py`（追加）

**Interfaces:**
- Consumes: `InboundEvent.scoped_session_key(scope)`（Task 1）、`self.config`（loop 已持有，`agent/loop.py:86`）。
- Produces: `config.session.group_session_scope: Literal["per_user", "shared"] = "per_user"`。`_on_inbound` 在做任何 session_key 读取前，将解析出的作用域键写回 `event.session_key_override`，使全部下游（session 锁 / working memory / 快照 / 可见性 / source_session）自动按该键隔离。

- [ ] **Step 1: Write the failing test**

```python
# tests/test_group_session_isolation.py 追加
def test_session_config_has_group_scope_default_per_user():
    from echo_agent.config.schema import SessionConfig
    cfg = SessionConfig()
    assert cfg.group_session_scope == "per_user"


def test_group_scope_field_rejects_unknown_value():
    import pytest
    from pydantic import ValidationError
    from echo_agent.config.schema import SessionConfig
    with pytest.raises(ValidationError):
        SessionConfig(group_session_scope="everyone")
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_group_session_isolation.py -k group_scope_field -v`
Expected: FAIL —— `AttributeError`/默认值不存在。

- [ ] **Step 3: Add the schema field**

在 `echo_agent/config/schema.py` 的 `SessionConfig`，于 `history_image_skip_if_current` 字段后（line ~1744）新增：

```python
    group_session_scope: Literal["per_user", "shared"] = Field(
        default="per_user",
        json_schema_extra={
            "status": "effective", "ref": "agent/loop.py:597",
            "desc_zh": "群聊会话隔离策略:per_user 每人独立会话(默认,防群内串话),shared 整群共享一个会话",
            "desc_en": "Group session scope: per_user = isolate per sender (default), shared = whole group shares one session",
        },
    )
```

确认文件顶部已 `from typing import Literal`（schema.py 已大量使用 `Literal`，无需新增 import；若缺失则补）。

在 `echo_agent/config/default.yaml` 的 `session:` 段下补一行（紧随其它 session 字段）：

```yaml
  group_session_scope: per_user
```

- [ ] **Step 4: Run schema test to verify it passes**

Run: `python -m pytest tests/test_group_session_isolation.py -k group_scope -v`
Expected: PASS（2 passed）。

- [ ] **Step 5: Write the failing loop-resolution test**

```python
# tests/test_group_session_isolation.py 追加
import pytest
from echo_agent.bus.events import InboundEvent


def _resolve(scope: str, event: InboundEvent) -> str:
    """复刻 _on_inbound 的解析契约：群聊 per_user 写回 override。"""
    if not event.session_key_override:
        event.session_key_override = event.scoped_session_key(scope)
    return event.session_key


def test_on_inbound_resolution_isolates_group_per_user():
    a = InboundEvent.text_message(channel="telegram", sender_id="alice",
                                  chat_id="grp1", text="x", is_group=True)
    b = InboundEvent.text_message(channel="telegram", sender_id="bob",
                                  chat_id="grp1", text="y", is_group=True)
    assert _resolve("per_user", a) == "telegram:grp1:alice"
    assert _resolve("per_user", b) == "telegram:grp1:bob"


def test_on_inbound_resolution_shared_keeps_single():
    a = InboundEvent.text_message(channel="telegram", sender_id="alice",
                                  chat_id="grp1", text="x", is_group=True)
    assert _resolve("shared", a) == "telegram:grp1"
```

- [ ] **Step 6: Run to verify it fails or passes against the contract**

Run: `python -m pytest tests/test_group_session_isolation.py -k on_inbound_resolution -v`
Expected: PASS（该测试锁定契约；下一步把同一契约写进真实 `_on_inbound`）。

- [ ] **Step 7: Wire resolution into `_on_inbound`**

在 `echo_agent/agent/loop.py` 的 `_on_inbound`，于 `if not self._running: return`（line 600）之后、approval 命令判断（line 607）之前，插入：

```python
        # 群聊会话作用域解析：把按策略解析出的隔离键固化到 override，
        # 使下游全部 session_key 读取(锁/working memory/快照/可见性/source_session)统一按此键隔离。
        if not event.session_key_override:
            scope = self.config.session.group_session_scope
            event.session_key_override = event.scoped_session_key(scope)
```

> 说明：写回 `session_key_override` 而非改 property，确保同一 event 在本 turn 内 key 稳定，且 eval/cron/delegate 等已显式设置 override 的路径不受影响。

- [ ] **Step 8: Run full isolation test file + loop tests**

Run: `python -m pytest tests/test_group_session_isolation.py tests/test_session_atomic.py -v`
Expected: PASS（全部通过；session_atomic 验证无回归）。

- [ ] **Step 9: Commit**

```bash
git add echo_agent/config/schema.py echo_agent/config/default.yaml echo_agent/agent/loop.py tests/test_group_session_isolation.py
git commit -m "loop 入站按 group_session_scope 解析群聊隔离键,新增配置默认 per_user"
```

---

### Task 3: 各通道归一化填入 is_group

**Files:**
- Modify: `echo_agent/channels/base.py:114-153`（`_build_event` 加参数）、`155-176`（`_handle_message` 透传）
- Modify: `echo_agent/channels/telegram.py:230`、`echo_agent/channels/feishu.py:181,194`、`echo_agent/channels/qqbot.py:566,581`、`echo_agent/channels/discord.py:336`
- Test: `tests/test_group_session_isolation.py`（追加）

**Interfaces:**
- Consumes: `InboundEvent.is_group`（Task 1）。
- Produces: `BaseChannel._build_event(..., is_group: bool = False)` 与 `_handle_message(..., is_group: bool = False)` 新增关键字参数，默认 `False`（whatsapp/dingtalk/email 等未接线通道保守按私聊）。

- [ ] **Step 1: Write the failing test**

```python
# tests/test_group_session_isolation.py 追加
def test_build_event_sets_is_group_flag():
    from unittest.mock import MagicMock
    from echo_agent.channels.base import BaseChannel

    ch = BaseChannel.__new__(BaseChannel)
    ch.config = MagicMock(allow_from=["*"])
    ch.name = "telegram"

    grp = ch._build_event(sender_id="alice", chat_id="grp1", text="hi", is_group=True)
    assert grp.is_group is True
    priv = ch._build_event(sender_id="alice", chat_id="alice", text="hi")
    assert priv.is_group is False
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_group_session_isolation.py -k build_event -v`
Expected: FAIL —— `_build_event() got an unexpected keyword argument 'is_group'`。

- [ ] **Step 3: Add is_group to base channel**

在 `echo_agent/channels/base.py` 的 `_build_event` 签名末尾加参数（line 123 之后）：

```python
        thread_id: str | None = None,
        is_group: bool = False,
    ) -> InboundEvent:
```

在 `return InboundEvent(...)`（line 144）中加入 `is_group=is_group,`：

```python
        return InboundEvent(
            channel=self.name,
            sender_id=str(sender_id),
            chat_id=str(chat_id),
            content=content_blocks,
            reply_to_id=reply_to_id,
            thread_id=thread_id,
            session_key_override=session_key,
            metadata=metadata or {},
            is_group=is_group,
        )
```

在 `_handle_message` 签名末尾加同名参数（line 164 之后）并透传给 `_build_event`：

```python
        thread_id: str | None = None,
        is_group: bool = False,
    ) -> InboundEvent | None:
        try:
            event = self._build_event(
                sender_id=sender_id,
                chat_id=chat_id,
                text=text,
                media=media,
                metadata=metadata,
                session_key=session_key,
                reply_to_id=reply_to_id,
                thread_id=thread_id,
                is_group=is_group,
            )
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_group_session_isolation.py -k build_event -v`
Expected: PASS。

- [ ] **Step 5: Wire telegram**

`echo_agent/channels/telegram.py:230` 处 `_handle_message(...)` 调用已带 `metadata={"chat_type": chat.get("type", "private")}`。在该调用加 `is_group`：

```python
        await self._handle_message(
            sender_id=sender_id,
            chat_id=chat_id,
            text=text,
            media=media if media else None,
            reply_to_id=str(msg.get("message_id", "")),
            metadata={"chat_type": chat.get("type", "private")},
            is_group=chat.get("type") in ("group", "supergroup"),
        )
```

- [ ] **Step 6: Wire feishu**

`echo_agent/channels/feishu.py` 两处 `_handle_message`（line ~181、~194）。group 判据为 `chat_type == "group"`（p2p 为私聊）。第一处：

```python
            metadata={"chat_type": chat_type, "receive_id_type": receive_id_type},
            is_group=(chat_type == "group"),
```

第二处（line ~194）用 `event.get("chat_type", "")`：

```python
            metadata={"chat_type": event.get("chat_type", ""), "receive_id_type": "chat_id"},
            is_group=(event.get("chat_type", "") == "group"),
```

- [ ] **Step 7: Wire qqbot**

`echo_agent/channels/qqbot.py` 的 `_on_group_message`（line 566）是群消息入口，加 `is_group=True`：

```python
        await self._handle_message(
            sender_id=sender_id, chat_id=group_id, text=content,
            media=media or None,
            reply_to_id=msg_id, metadata={"msg_type": "group"},
            is_group=True,
        )
```

`_on_c2c_message`（line 581）为私聊，无需改（默认 False）。若存在频道(channel/guild)消息入口同理按群处理：搜索 `_on_channel`/`_on_guild` 类方法，若有则加 `is_group=True`；无则跳过。

- [ ] **Step 8: Wire discord**

`echo_agent/channels/discord.py:336` 处 `_handle_message` 带 `metadata={"guild_id": guild_id or ""}`。guild_id 非空即群：

```python
            metadata={"guild_id": guild_id or ""},
            is_group=bool(guild_id),
```

- [ ] **Step 9: Run full test file + channel tests**

Run: `python -m pytest tests/test_group_session_isolation.py tests/test_channel_delivery.py tests/test_channel_media.py -v`
Expected: PASS（无回归）。

- [ ] **Step 10: Commit**

```bash
git add echo_agent/channels/base.py echo_agent/channels/telegram.py echo_agent/channels/feishu.py echo_agent/channels/qqbot.py echo_agent/channels/discord.py tests/test_group_session_isolation.py
git commit -m "telegram/feishu/qqbot/discord 群聊消息归一化标记 is_group"
```

---

### Task 4: 投递侧 session_key 反解器剥离 sender 后缀

**Files:**
- Modify: `echo_agent/scheduler/delivery.py:10-19`（`target_from_session_key`）
- Test: `tests/test_group_session_isolation.py`（追加）

**Interfaces:**
- Consumes: 群聊 per_user 键格式 `channel:chat_id:sender_id`（Task 1/2）。
- Produces: `target_from_session_key(session_key)` 对普通通道三段键返回群 chat_id（剥离 sender 后缀），gateway 与私聊两段键行为不变。

- [ ] **Step 1: Write the failing test**

```python
# tests/test_group_session_isolation.py 追加
from echo_agent.scheduler.delivery import target_from_session_key


def test_target_strips_group_sender_suffix():
    # 群聊 per_user 键 -> 投递目标是群 chat_id，而非 chat_id:sender
    assert target_from_session_key("telegram:grp1:alice") == ("telegram", "grp1")


def test_target_private_two_part_unchanged():
    assert target_from_session_key("telegram:c1") == ("telegram", "c1")


def test_target_gateway_unchanged():
    assert target_from_session_key("gateway:sess123:user1") == ("gateway:sess123", "user1")


def test_target_empty_or_malformed():
    assert target_from_session_key("") == ("", "")
    assert target_from_session_key("nocolon") == ("", "")
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python -m pytest tests/test_group_session_isolation.py -k target_ -v`
Expected: FAIL —— `test_target_strips_group_sender_suffix` 得到 `("telegram", "grp1:alice")`（旧 `split(":", 1)` 未剥离 sender）。

- [ ] **Step 3: Fix the parser**

替换 `echo_agent/scheduler/delivery.py:10-19` 的 `target_from_session_key`：

```python
def target_from_session_key(session_key: str) -> tuple[str, str]:
    if not session_key or ":" not in session_key:
        return "", ""
    if session_key.startswith("gateway:"):
        parts = session_key.split(":", 2)
        if len(parts) == 3 and parts[1] and parts[2]:
            return f"gateway:{parts[1]}", parts[2]
        return "", ""
    # 普通通道键为 channel:chat_id 或群聊 per_user 的 channel:chat_id:sender_id。
    # 投递目标始终是群/会话本身(chat_id)，需剥离末段 sender 后缀。
    # 前提(trusted-operator)：group-capable 通道的 chat_id 不含冒号。
    parts = session_key.split(":")
    channel = parts[0]
    chat_id = parts[1] if len(parts) >= 2 else ""
    return (channel, chat_id) if channel and chat_id else ("", "")
```

- [ ] **Step 4: Run test to verify it passes**

Run: `python -m pytest tests/test_group_session_isolation.py -k target_ -v`
Expected: PASS（4 passed）。

- [ ] **Step 5: Run scheduler regression tests**

Run: `python -m pytest tests/test_scheduler_delivery.py -v`
Expected: PASS（无回归）。

- [ ] **Step 6: Commit**

```bash
git add echo_agent/scheduler/delivery.py tests/test_group_session_isolation.py
git commit -m "投递反解器剥离群聊 sender 后缀,还原群 chat_id"
```

---

### Task 5: 跨 sender 记忆可见性端到端隔离测试

**Files:**
- Modify: `tests/test_group_session_isolation.py`（追加端到端断言）
- Reference: `echo_agent/memory/store.py:303-327`（`list_all` + `_visible_in_session`）、`echo_agent/memory/retrieval.py:60-78`

**Interfaces:**
- Consumes: 已落地的 Task 1-2 解析链 + 既有 `MemoryStore._visible_in_session`（第一批已修）。
- Produces: 锁定"群内两 sender 的 per_user 键 → USER 记忆互不可见"的回归测试。

- [ ] **Step 1: Inspect MemoryStore visibility API**

Run: `python -m pytest tests/test_memory_session_isolation.py -v`
Expected: PASS —— 先确认既有隔离测试的构造方式（store 初始化、写入 USER entry、`is_visible_in_session` 调用签名），照其风格写本任务测试。

- [ ] **Step 2: Write the end-to-end visibility test**

参照 `tests/test_memory_session_isolation.py` 的 store 构造方式，新增（下例为契约骨架，store 构造按既有测试 fixture 对齐）：

```python
# tests/test_group_session_isolation.py 追加
def test_group_per_user_memory_not_cross_visible():
    """群内 alice 写入的 USER 记忆，对 bob 的 per_user 会话键不可见。"""
    from echo_agent.bus.events import InboundEvent

    alice = InboundEvent.text_message(channel="telegram", sender_id="alice",
                                      chat_id="grp1", text="x", is_group=True)
    bob = InboundEvent.text_message(channel="telegram", sender_id="bob",
                                    chat_id="grp1", text="y", is_group=True)
    a_key = alice.scoped_session_key("per_user")
    b_key = bob.scoped_session_key("per_user")
    assert a_key != b_key  # 隔离键前提成立

    # 与既有 test_memory_session_isolation.py 相同方式构造 store，
    # 写入 source_session=a_key 的 USER 记忆，断言 _visible_in_session(entry, b_key) is False。
    # （store fixture 按既有测试对齐；此处锁定 per_user 键确实驱动可见性隔离。）
```

> 实现注意：若 `test_memory_session_isolation.py` 已有可复用的 store fixture，直接 import 复用；否则按其内联构造方式复制最小 store 初始化。务必覆盖 `scope_policy` 非 legacy 的情形（legacy 下 USER 全局可见，隔离为 no-op —— 见记忆 remediation-batch1-done）。

- [ ] **Step 3: Run the test**

Run: `python -m pytest tests/test_group_session_isolation.py -k cross_visible -v`
Expected: PASS。

- [ ] **Step 4: Run the whole new test file once green**

Run: `python -m pytest tests/test_group_session_isolation.py -v`
Expected: PASS（全部通过）。

- [ ] **Step 5: Commit**

```bash
git add tests/test_group_session_isolation.py
git commit -m "新增群聊跨 sender 记忆不可见的端到端隔离测试"
```

---

### Task 6: 配置参考文档与全量回归

**Files:**
- Modify: `docs/config-reference.md`、`docs/config-reference.yaml`、`docs/config-reference.en.md`、`docs/config-reference.en.yaml`
- Reference: 全量测试

**Interfaces:**
- Consumes: Task 2 的 schema 字段。
- Produces: config-reference 四个文件含 `group_session_scope` 条目。

- [ ] **Step 1: Check whether config-reference is generated or hand-maintained**

Run: `cd "$(git rev-parse --show-toplevel)" && grep -rn "config-reference" echo_agent/ --include=*.py | grep -i "gen\|write\|dump" | head`
Expected: 判断是否有生成脚本。若有生成器（如 `cli` 下的 config dump 命令），运行它重新生成四个文件；若手维护，则手动补条目。

- [ ] **Step 2: Add/regenerate the field entry**

若手维护：在四个 config-reference 文件的 `session:` 段补入 `group_session_scope`，中英文描述与 schema 的 `desc_zh`/`desc_en` 一致，标注默认 `per_user`。若有生成器：运行生成命令覆盖。

```yaml
# config-reference.yaml 的 session 段示例
  group_session_scope: per_user   # per_user(默认,群内每人独立) | shared(整群共享一个会话)
```

- [ ] **Step 3: Verify config metadata guard passes**

Run: `python -m pytest tests/test_config_metadata_guard.py tests/test_config_loader.py -v`
Expected: PASS —— 该守卫校验 schema 字段元数据完整性（status/ref/desc），新字段须通过。

- [ ] **Step 4: Full test suite**

Run: `python -m pytest`
Expected: PASS（全绿；确认无跨模块回归）。

- [ ] **Step 5: Commit**

```bash
git add docs/config-reference.md docs/config-reference.yaml docs/config-reference.en.md docs/config-reference.en.yaml
git commit -m "配置参考补充 group_session_scope 群聊会话隔离策略"
```

---

## Self-Review

**Spec coverage（对照设计文档各节）：**
- §4.1 InboundEvent 加 is_group + 归一化 → Task 1（字段）+ Task 3（各通道接线，含 whatsapp/dingtalk 保守 False 的诚实标注）。✓
- §4.2 session_key 计算（群 per_user 加 sender）→ Task 1（`scoped_session_key`）+ Task 2（在 `_on_inbound` 注入 override，采纳 spec 倾向的"调用点解析"方案 b）。✓
- §4.3 反解器剥离 sender → Task 4。✓
- §4.4 配置字段 + schema + default.yaml + config-reference → Task 2（schema/default）+ Task 6（reference）。✓
- §4.5 不进 setup → 计划未触 setup，Global Constraints 已记。✓
- §6 历史会话不迁移 → 无迁移任务，Global Constraints 已记。✓
- §7 错误边界（空 sender 回退 / override 优先 / shared 保底）→ Task 1 测试 `empty_sender_falls_back`、`override_wins`、`shared_keeps_single`。✓
- §8 测试 1-5 → Task 1（per_user/shared/私聊键）、Task 3（is_group 归一化）、Task 4（反解）、Task 5（跨 sender 记忆不可见，覆盖 scope_policy 非 legacy）。✓
- §9 改动落点清单 → Task 1-6 全覆盖。✓

**Placeholder scan:** Task 5 的 store 构造显式指向 `test_memory_session_isolation.py` 既有 fixture 复用（非 TODO，是有依据的对齐指令）；Task 6 Step 1 先判定生成/手维护再执行（非占位，是分支决策）。其余步骤均含可执行代码与命令。无 "TBD/handle edge cases" 类空洞。

**Type consistency:** `scoped_session_key(scope: str) -> str`、`is_group: bool`、`group_session_scope: Literal["per_user","shared"]`、`target_from_session_key -> tuple[str,str]` 在 Task 1/2/3/4 间命名与签名一致。`_on_inbound` 写回 `session_key_override` 与 Task 1 的 override 最高优先级契约一致。

无遗留问题。
