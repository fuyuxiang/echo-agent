# 群聊会话隔离 设计文档

- 日期：2026-06-23
- 类型：设计文档（spec），不含代码改动
- 上游依据：
  - `docs/architecture-remediation-roadmap-2026-06-23.md` 第 3 批 1.5（群聊 session_key 加 sender），第一批拆分出的独立待设计项
  - 记忆 `remediation-batch1-done`、`global-architecture-review-2026-06`（主线二·作用域隔离失效）
- 威胁模型：沿用 local-first / trusted-operator（见 roadmap 第 1 节）

---

## 1. 问题

`InboundEvent.session_key`（`bus/events.py:73-77`）是无参 property，群聊场景算成 `channel:chat_id`，**不含 sender_id**。后果：

- 同一个群里所有成员共享**同一个会话**——working memory、记忆快照、上下文、晋升的 USER 事实全部混在一起（群内串话）。
- 这与第一批刚做的两项隔离机制（记忆主检索套 `_visible_in_session`、晋升事实带 `source_session`）冲突：底层隔离能力已正确，但群聊场景下 session_key 这个隔离键本身就把多人压成了一个，下游再隔离也无从区分。

review 主线二把它列为隐私问题。第一批因复杂度超原估（无参 property 被跨约 6 通道全局消费）将其拆出独立设计，即本文档。

## 2. 目标与非目标

**目标：**
- 群聊默认按 sender 隔离会话，消除群内多用户串话。
- 把"群=单会话"这一**隐式策略显式化**为一个可配置项，保留"bot 作为群成员参与多人讨论"的共享模式。
- `InboundEvent` 增加统一的群聊标记，消除各通道"各说各话"的群聊判定。

**非目标（防范围蔓延）：**
- 不迁移历史群聊会话（见 §6，技术上无法可靠拆分）。
- 不为每个通道做独立的隔离策略（隔离是全局隐私语义，全局一处即可，YAGNI）。
- 不把该配置加进 setup 向导（默认值已安全，与现有 `group_policy` 同类配置一致，见 §5）。
- 不重构 session_key 的字符串身份为结构化对象（影响面过大，非本次目标）。

## 3. 设计概览

三个改动点，从上游到下游：

1. **`InboundEvent` 加统一群聊标记** → 各通道归一化填入。
2. **`session_key` 计算** → 群聊 + per_user 策略时把 sender_id 纳入键。
3. **session_key 反解器** → 让从 key 反解 chat_id 的地方正确剥离 sender 后缀。

配套：新增全局配置 `group_session_scope`、补测试、写 config-reference。

## 4. 详细设计

### 4.1 InboundEvent 群聊标记

在 `InboundEvent`（`bus/events.py`）增加字段：

```python
is_group: bool = False
```

由各通道在构造事件时归一化填入。归一化逻辑集中在一处 helper（避免散落各通道），按通道已有 metadata 字段映射：

| 通道 | 现有判据 | is_group 判定 |
|---|---|---|
| telegram | `chat_type` ∈ {group, supergroup} | 是/否 |
| feishu | `chat_type` == group（p2p 为私聊） | 是/否 |
| qqbot | `msg_type` ∈ {group, channel}（c2c 为私聊） | 是/否 |
| discord | `guild_id` 非空 | 是/否 |
| whatsapp | **现 metadata 的 `message_type` 是内容类型，非群标记** | 见下 |
| dingtalk | **现 `msgtype` 是内容类型，非群标记** | 见下 |

**诚实标注：** whatsapp / dingtalk 当前事件里没有可靠的群/私聊判据。本次对这两个通道：若能从原始 payload 低成本补出群标记则补（实现时核实 API 字段），否则保守按**私聊**处理（`is_group=False`，即维持现状不隔离），并在文档与代码注释中标注"该通道群聊隔离待补判据"。不假装支持。

### 4.2 session_key 计算

`session_key` property 改为：

```python
@property
def session_key(self) -> str:
    if self.session_key_override:
        return self.session_key_override
    base = f"{self.channel}:{self.chat_id}"
    if self.is_group and self._group_session_scope == "per_user" and self.sender_id:
        return f"{base}:{self.sender_id}"
    return base
```

策略来源：property 自身拿不到全局 config。落地方式实现时二选一确认——(a) 构造事件时由通道/bus 注入 scope 到事件字段；(b) 计算 session_key 的调用点（loop/bus）读 config 后决定是否附加 sender。倾向 (b)，让 InboundEvent 保持纯数据、不依赖 config。本设计以"群聊 per_user 时 key = `channel:chat_id:sender_id`"为契约，注入方式属实现细节。

- **私聊**：永远 `channel:chat_id`，行为不变。
- **群聊 + per_user（默认）**：`channel:chat_id:sender_id`，每人独立。
- **群聊 + shared**：`channel:chat_id`，整群共享（与当前行为一致）。

下游全部自动受益：session 锁、working memory（`loop.py`）、记忆快照（`context_stage.py`）、`_visible_in_session` 可见性过滤、晋升 `source_session`——它们都以 session_key 为隔离键，键一变，按人隔离即自动成立，**无需再改记忆层**。

### 4.3 session_key 反解器（关键集成点）

从 session_key 反解出 chat_id 的地方，必须正确处理新增的 sender 后缀，否则 outbound 投递会发错地方。

已识别两处：

- **`scheduler/delivery.py:14` `target_from_session_key`**：现对 `gateway:` 特判成 3 段、其余 `split(":", 1)` 把剩余整段当 chat_id。群聊 per_user key 为 `channel:chat_id:sender`，会把 `chat_id:sender` 误当 chat_id。**必须修**：投递目标是群（chat_id），需剥离 sender 后缀还原群 chat_id。
- **`agent/tools/delegate.py:384`**：用 `split(":")[1]` 取 chat_id（索引 1），加 sender 到索引 2 不受影响，**安全**，但实现时一并复核。

**反解契约假设**：group-capable 通道的 chat_id 不含冒号（gateway 已单独特判）。该假设在 trusted-operator 模型下成立（telegram/discord/qq 的 chat_id 为数字或 hex openid）。集中提供一个 `parse_session_key` helper 统一反解逻辑，两处调用点共用，假设写进注释。实现时逐通道核实 chat_id 不含冒号；若某通道违反，则该通道的反解改用"末段为 sender、其余为 chat_id"或换分隔符策略。

### 4.4 配置

全局一处新增字段（挂在 `memory` 段或新 `session` 段，实现时按 schema 现有分组择优）：

```yaml
group_session_scope: per_user   # per_user(默认) | shared
```

- `per_user`：群聊每人独立会话（默认，安全）。
- `shared`：整群共享一个会话，bot 作为群成员参与多人讨论。

字段进 `config/schema.py`（带 `status: effective` + 中英文 desc，符合本项目死字段治理要求），进 `default.yaml`，进 `docs/config-reference.*`。**不进 setup 向导**（理由见下）。环境变量覆盖天然支持（loader 已有 env 机制）。

### 4.5 为何不进 setup 向导

- 与同类配置一致：现有 `group_policy`（群响应策略）也未进向导，只能改 YAML。
- 默认值已安全（per_user = 隔离），向导价值在于帮用户避开不安全默认或填必填项，此项不属于。
- YAGNI：shared 是少数高级场景，用户改 YAML 或用 env var 即可。

用户配置路径：在 `echo-agent.yaml`（loader 在 cwd / echo_home 查找）加一行，或用对应环境变量；不配走安全默认。

## 5. 数据流

```
通道收到消息
  → 归一化 helper 判定 is_group（按通道 metadata 映射）
  → 构造 InboundEvent(is_group=...)
  → session_key 计算：群聊 per_user 时 = channel:chat_id:sender_id
  → 下游全部以此 key 隔离（session 锁 / working memory / 记忆快照 / 可见性过滤 / source_session）

outbound 投递 / delegate 取 chat_id
  → parse_session_key 剥离 sender 后缀 → 还原群 chat_id → 发回群
```

## 6. 历史会话兼容

**策略：不迁移，自然过渡。**

升级后群聊 key 从 `channel:chat_id` 变为 `channel:chat_id:sender_id`，旧群聊会话 key 对不上，等于群内每人"失忆重开"。处理：旧会话留在库里，按正常过期清理消失；新消息用新 key 重建会话。

不做迁移脚本的原因：旧会话里多人消息已混在一起，**没有可靠依据拆回各人**，迁移做不干净。不做兼容读回退的原因：会把要消除的串话老数据又带回新会话，违背隔离初衷。群聊上下文短时效，自然过渡代价最小。

## 7. 错误处理与边界

- `sender_id` 为空的群聊事件：退回 `channel:chat_id`（不构造空 sender 后缀），避免产生 `channel:chat_id:` 这类畸形键。
- `session_key_override` 优先级最高，保持不变（delegate / 后台任务回灌依赖它）。
- shared 模式下行为与升级前完全一致，作为可回退保底。

## 8. 测试

`python -m pytest`（站点包遮蔽本地源，统一用此命令）。

1. 群聊 + per_user（默认）：两个不同 sender 的事件 → 断言 session_key 不同、记忆互不可见（覆盖 vector 开/关两条路径）。
2. 群聊 + shared：两个 sender → 断言同一 session_key、上下文共享。
3. 私聊：断言 session_key 不含 sender（行为不变，防回归）。
4. is_group 归一化：telegram/feishu/qqbot/discord 各造群聊与私聊样本 → 断言判定正确；whatsapp/dingtalk 断言按保守私聊处理。
5. 反解器：`channel:chat_id:sender` → `parse_session_key` 断言还原出正确 chat_id；gateway 与私聊 key 不回归。

## 9. 改动落点清单

- `bus/events.py`：`InboundEvent` 加 `is_group` 字段、改 `session_key` 计算。
- 群聊判定归一化 helper（新增，放 bus 或 channels 公共处）。
- 各通道构造事件处：填 `is_group`（telegram/feishu/qqbot/discord 接线；whatsapp/dingtalk 保守 False + 标注）。
- session_key 注入 scope 的调用点（loop 或 bus，实现时定）。
- `scheduler/delivery.py`：`target_from_session_key` 剥离 sender 后缀；抽 `parse_session_key` helper。
- `agent/tools/delegate.py:384`：复核反解（预期安全）。
- `config/schema.py` + `default.yaml` + `docs/config-reference.*`：新增 `group_session_scope`。
- 测试：新增群聊隔离测试文件。

## 10. 工作量

路线图原估 S（升级为含 is_group 归一化 + 反解器修复后约 S–M）。落点集中，下游因 session_key 是统一隔离键而自动受益，不触及记忆内核。
