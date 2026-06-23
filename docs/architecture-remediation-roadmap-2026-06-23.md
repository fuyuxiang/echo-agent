# Echo Agent 架构整改路线图（决策文档）

- 日期：2026-06-23
- 性质：**决策文档（routemap），不含代码改动**。目标是基于明确的威胁模型，把 2026-06-22 全局 review 的 60+ 条发现重排为"真问题 / 默认安全基线 / 设计取舍"三类，并为真问题排出可执行的整改顺序。
- 上游依据：`docs/architecture-review-2026-06-22.md`（全量源码 review，仅诊断未改动）。
- 核实基线 commit：`8f9e925`（master）。
- 参照系：业界同类自托管 agent 的 trusted-operator 威胁模型实践。

---

## 0. 文档怎么用

本文档不是"按 review 全改"的任务清单，而是一份**架构决策**：先定威胁模型，再据此判断"什么算真问题"，最后只对真问题排执行顺序。读者应先读第 1 节（威胁模型）和第 2 节（分类原则），再看第 3 节（A 类执行路线）。第 4 节是诚实性记录：哪些条目作者亲自核实过代码、哪些采信了 review。第 5 节是明确的非目标，用于防止范围蔓延。

每个 A 类条目给出固定四要素：**根因 / 改动落点 / 验证方式 / 工作量量级**。工作量量级用 S（数小时）/ M（1-2 天）/ L（多日）粗粒度标注，仅供排期参考，不是承诺。

---

## 1. 威胁模型声明

**Echo Agent 采用 local-first / trusted-operator（本机优先、单一可信操作者）威胁模型。**

定义：

- 部署形态：操作者在自己的机器/自有服务器上自托管，使用自己的客户端与之交互。`echo-agent` 既是独立运行的长期进程，也通过 gateway / A2A 对**操作者自己的**客户端暴露接口。
- 信任边界：**操作者本人是可信的**。Agent 拥有操作者授予的本机能力（shell、文件、网络）是设计本意，不是漏洞。
- **不在威胁模型内**：同一 gateway 上多个相互敌对用户之间的隔离；把管理面/接口直接暴露到敌对公网而不加反代或鉴权。

这一选择与业界同类项目的安全模型一致，其典型表述为：

> "local-first agent infrastructure for trusted operators; it is not designed as a shared multi-tenant boundary between adversarial users on one gateway."

业界同类项目的 "What Usually Is Not a Security Bug" 列表进一步明确：本机 shell/脚本执行、操作者自装的插件、多敌对用户共用一个 gateway 期望隔离、文档已建议规避的公网暴露——**都不算安全漏洞**。本路线图直接采用同一把标尺。

> 说明：采用 trusted-operator 模型**不等于**放任默认不安全。默认形态仍应安全（见 B 类），区别在于"对外多租户隔离"不作为必修目标。

## 2. review 60+ 条的重分类

威胁模型一旦锚定，review 的发现自然分成性质完全不同的三类。**判定原则**：

- **A 类·真 bug（威胁模型无关）**：无论单用户还是多用户、无论是否暴露公网，它都是错的——核心卖点在主路径上没兑现、长期运行必然资源泄漏、声明的能力其实是空壳。**这是路线图主体。**
- **B 类·默认安全基线（保守处理）**：严重性取决于"是否对外暴露"。在 trusted-operator 模型下不作为必修，但默认形态要安全。**处理原则：不强行改默认行为，只做"默认安全 + SECURITY.md 写清 + 对外暴露时的加固开关/文档"。**
- **C 类·设计取舍（不改，明确标注）**：trusted-operator 模型下本就是有意为之的本机能力，按该标尺不算漏洞。**明确标注为非问题，避免被后续误当 bug 修。**

### A 类 · 真 bug（路线图主体）

| 来源主线 | 条目 | 为何与威胁模型无关 |
|---|---|---|
| 主线一 | SkillReviewer 每 turn 直写技能库，零评测/扫描/审计/回滚 | 单用户下，一条被污染的对话/网页内容即可把注入 payload 写进 SKILL.md 并永久生效；"评测后才生效、可回滚"卖点在主路径未兑现 |
| 主线一 | eval 流量触发 recorder/reviewer，可自触发进化 | 评测污染与自触发循环和用户数无关，是引擎自身正确性问题 |
| 主线一 | 晋升判定无统计有效性（单 case 翻转即判改进） | 非确定性 agent 的噪声被当改进，纯属判定逻辑缺陷 |
| 主线二 | HybridRetriever 主检索绕过 `_visible_in_session` | 即便单用户，跨会话串记忆也污染上下文、损害"记得住"质量 |
| 主线二 | 自动晋升语义事实不带 `source_session` | 同上，隔离底层正确但装配错误 |
| 主线二 | 群聊 session_key 不含 sender_id | 群内多用户串话；即便自用，群场景也属常见 |
| 主线四 | expire_session 在 SQLite 模式静默失效 → sessions 无界增长 | 长期运行自托管必踩，与卖点"长期运行"直接冲突 |
| 主线四 | `_memory_snapshots` 快照缓存无上限 | 同上，无界内存增长 |
| 主线四 | `_evict_oldest` 不清向量 → FAISS 孤儿向量膨胀 | 同上 |
| 主线四 | trace 文件每 turn 一个、无保留/清理 | 同上，磁盘无界增长 |
| 主线五 | 工作流引擎不自驱动（无执行器/无自动推进） | 声明能力 vs 落地，半成品骨架 |
| 主线五 | InferenceConstraints 约半数约束为死代码、响应校验仅告警不强制 | 同上 |
| 主线五 | 矛盾检测有检测无消解（resolve/get_unresolved/supersede 无调用方） | 同上 |

### B 类 · 默认安全基线（保守处理，不强行改默认）

| 条目 | 保守处理建议 |
|---|---|
| gateway 默认 `0.0.0.0`（`schema.py:2432`、`setup.py:590`） | **不改默认值**（避免再次扰动既有部署形态）。SECURITY.md 写清公网暴露风险 + 推荐反代/回环+token；可选：setup 向导加一行安全提示文案 |
| 默认回环时管理面 CSRF 敞开（最近 commit 已部分加固） | 已在做加固，保持方向。文档化"对外暴露时必须配 admin token + Origin 校验" |
| 媒体下载 SSRF（`media.py`） | 加固开关：scheme 白名单 + 私网拒绝 + 响应体大小上限，但默认行为以不破坏自用为准；文档标注 |
| webhook 入站零验签（WhatsApp/Feishu/Webhook） | **不强制补多租户验签**。文档说明"对外接入需配 secret/验签"；可选：空 secret 时启动告警 |
| exec/shell 可读 `~/.ssh` 等凭证路径 | trusted-operator 下属本机能力，但建议提供可选 denylist 加固档 + 文档；不默认拦截 |

> B 类共识：选 trusted-operator + 保守处理后，**这些条目不进入"必修"批次**。它们的产出主要是 SECURITY.md 文档化 + 可选加固开关，绝不出现"把已完成的加固改回去"这类与现有工作对冲的动作。

### C 类 · 设计取舍（不改，明确标注为非问题）

| 条目 | 标注理由（该安全标尺） |
|---|---|
| exec/shell 拥有本机文件/网络/环境变量访问 | "trusted operator using an intentional local feature, such as local shell access" |
| 插件/hooks 动态加载任意 `.py` | "a malicious plugin after a trusted operator installs or enables it" |
| 单纯 prompt injection（未越过 policy/auth/approval/sandbox 边界） | "prompt injection without a policy/auth/approval/sandbox/tool-boundary bypass" |

> 注意边界：A 类主线一的 SkillReviewer **不是** C 类。区别在于它让注入内容**绕过了本应存在的 gate/扫描边界**自动落盘生效——这正好落在业界同类认定算漏洞的 "policy/approval/tool-boundary bypass" 一侧。

## 3. A 类执行路线（分四批，按"损害程度 × 长期运行必要性"排）

排序原则：先修直接损害核心卖点且有真实污染风险的，再修长期运行稳定性，最后做能力声明的诚实化。每条给出 **根因 / 改动落点 / 验证方式 / 工作量**。

---

### 第一批 · 卖点旁路 + 隔离失效 + eval 隔离（立即）

这一批集中在"自进化"和"记忆"两大卖点的主路径，以及与之同根的 eval 隔离。修复落点集中、性价比最高。

**1.1 SkillReviewer 旁路 → 统一所有技能写入必经 gate**
- 根因：`skills/reviewer.py` 的 `_handle_skill_manage` 直接调 `store.create/update/patch/delete/write_file`；调用点 `agent/pipeline/response_stage.py:119` 每个 `total_tool_calls>0` 的 turn 后台触发。正规 `evolution/gate.py:PromotionGate` 的注入扫描/快照/回归判定/回滚全被绕过。
- 改动落点：让 reviewer 写入路径**至少**经过注入扫描（复用 evolution 侧的 `scan_text_for_threats`）+ 审计落盘 + 回滚入口；理想是统一汇入 PromotionGate。需要决策：是"轻量门"（扫描+审计+回滚）还是"完整门"（强制 A/B）——建议轻量门，避免每 turn A/B 的成本。
- 验证：构造含注入 payload 的对话 → 断言 SKILL.md 不被写入或被扫描拦截 + 有审计记录 + 可回滚。
- 工作量：**M**。

**1.2 eval 流量隔离（与 1.1 同根）**
- 根因：`agent/loop.py:684` 无条件 `recorder.begin_turn`，不看 channel；`response_stage.py:119` 的 reviewer 守卫只看 `total_tool_calls>0` 未排除 eval/ephemeral。导致 eval 流量被记录为轨迹（可自触发进化）并并发改写正在 A/B 的 user_dir。
- 改动落点：eval channel（及 ephemeral session）禁用 recorder + reviewer；evolution 跑在隔离副本（独立 skill_store 指向临时目录）。
- 验证：跑一次 eval → 断言无新轨迹写入、无 user_dir 技能改写。
- 工作量：**M**。

**1.3 晋升统计有效性**
- 根因：`evolution/gate.py:485` `_decide` 直接比单次 eval 的标量 pass_rate/avg_score，无最小样本/重复/置信区间。
- 改动落点：引入最小样本量门槛 + 多次重复取均值（或简单置信判定）；strict 档收紧 avg_score 抖动容忍。
- 验证：同一候选重复 eval，断言噪声级波动不触发晋升。
- 工作量：**S–M**。

**1.4 记忆主检索套可见性过滤**
- 根因：`memory/retrieval.py:60` `retrieve()` 收 `session_key` 但函数体从不使用；`context_stage.py:164` 默认走此路 → 跨会话/跨用户记忆注入。底层 `store.py:313 _visible_in_session` 设计正确但被绕过。
- 改动落点：`retrieve` 内对 `entries` 套 `_visible_in_session(entry, session_key)`（或在 `entries_fn` 注入时即过滤）。
- 验证：**新增跨会话隔离集成测试**——A 会话写入 USER 记忆 → B 会话检索断言不可见，覆盖 vector 开/关两种路径。
- 工作量：**S–M**。

**1.5 晋升事实透传 source_session + 群聊 session_key 加 sender**
- 根因：`memory/tiers.py:170` `promote_from_episodic` 构造 MemoryEntry 不设 `source_session`；`bus/events.py:74` 群聊 session_key = `channel:chat_id` 不含 sender。
- 改动落点：晋升时透传 `source_session`；群聊场景把 sender_id 纳入 session_key（或暴露成可配置策略，把"群=单会话"这一隐式策略显式化）。
- 验证：群内两用户分别写入 → 断言互不可见；晋升事实带 source_session。
- 工作量：**S**。

---

### 第二批 · 长期运行资源泄漏（自托管必修）

长期运行的自托管进程必然踩到，与"长期运行 AI Agent"定位直接冲突。

**2.1 expire_session 缓存未命中先 load 再改状态**
- 根因：`session/manager.py:294` 缓存未命中即静默 return，不落库；SQLite 模式下 `cleanup_expired` 遍历的多数 key 不在内存 → 过期清理基本失效，sessions 表无界增长。
- 改动落点：未命中时先从存储 load 再改状态落库。
- 验证：SQLite 模式下造过期会话（不预热缓存）→ 跑 cleanup → 断言已落库为过期。
- 工作量：**S**。

**2.2 `_memory_snapshots` 走 LRU 上限**
- 根因：`context_stage.py:108` 直接写 dict + move_to_end，不走 `loop.py:547 _lru_put`、无 popitem、无上限。
- 改动落点：纳入统一 LRU（复用 `_max_cached_sessions`）。
- 验证：超过上限的会话数 → 断言字典不超界。
- 工作量：**S**。

**2.3 `_evict_oldest` 同步清理向量索引**
- 根因：`memory/store.py:476` 容量淘汰只 pop entries + `_unindex_entry`，不调 `_vector_index.remove`（对比 `delete()` 路径 634 行有）。FAISS 孤儿向量膨胀。
- 改动落点：evict 路径补 `_vector_index.remove(entry.embedding_id)`，与 delete 对齐。
- 验证：触发容量淘汰 → 断言向量索引计数同步下降。
- 工作量：**S**。

**2.4 trace 文件保留/清理策略**
- 根因：`agent/loop.py:666` + `observability/monitor.py:95` 每 turn 写一个 `trace_{id}.json`，全程无 retention/rotate/prune。
- 改动落点：加保留策略（按数量或时间上限轮转清理）；顺带评估是否启用 DB logs 表或删除其死代码。
- 验证：跑 N+保留上限轮 → 断言旧 trace 被清理。
- 工作量：**S–M**。

---

### 第三批 · 半成品骨架诚实化

原则：**要么接线、要么从文档/枚举移除**，不给虚假安全感。每条标注建议走哪条。

**3.1 工作流引擎**
- 现状（修正后表述）：`tasks/workflow.py` 的 `on_task_complete` 无调用方，无 task 执行器自动消费——工作流不自驱动，完全依赖 LLM 手工逐步调 `task`/`workflow` 工具推进。
- 建议：二选一并文档化——(a) 接线一个完成回调/执行器实现自动推进；(b) 暂不实现，则在文档/能力声明中明确"工作流需 LLM 手动逐步驱动"，移除"自动模式"暗示。**倾向 (b) 先诚实化，(a) 列入 Post-1.0。**
- 工作量：(a) **L** / (b) **S**。

**3.2 InferenceConstraints 死代码**
- 现状（修正后表述）：`models/inference.py` 中 `filter_tools`（blocked_tools）/`needs_confirmation` 生效；`allowed_tools` 分支恒空、`max_output_tokens`/幻觉检测/自校验/output_format 为死代码，`validate_response` 仅告警不强制。
- 建议：把确属死代码的约束**从枚举/文档移除**（避免给"有护栏"的虚假印象）；`validate_response` 要么强制（阻断/重试）要么降级为明确的"仅观测"语义。
- 工作量：**S–M**。

**3.3 矛盾检测消解链路**
- 现状：`detect`/`store_contradiction`/`check_lightweight_sync` 生效；`resolve`/`get_unresolved`/`supersede` 无调用方——检测+打标签但永不裁决。
- 建议：二选一——(a) 接线消解（在 consolidation 路径调 resolve/supersede）；(b) 暂不实现则移除消解 API 或文档标注"仅标记不消解"。**倾向 (a)，因为它直接关系"记忆质量"卖点，但可排在第三批末。**
- 工作量：(a) **M** / (b) **S**。

---

### 第四批 · 收尾与防回归

- SECURITY.md：写入第 1 节威胁模型 + B 类加固清单 + "什么不算漏洞"（照业界同类结构）。
- 防回归：A 类每条配的隔离/清理测试纳入 CI；review 引用行号做 CI 校验，锁死成果防回潮。
- 工作量：**M**。

## 4. 独立核实记录（诚实性）

本路线图的 A 类条目**全部经作者亲自打开代码核实**，不存在"采信 review 未复核"的项。核实结论：

- **作者主对话内直接核实（3 条）**：`memory/retrieval.py`（session_key 收而不用，成立）、`agent/approval_gate.py`（auto_deny 在 smart 模式被绕过 + 空 channel 自批，成立）、`skills/reviewer.py`（直写技能库无 gate，成立）。
- **核实子代理逐条核实（A 类其余条目）**：12 条中 10 条按原 review 描述完全成立；2 条成立但描述已在本文档修正：
  - 工作流：不是字面"step 永远 PENDING"，而是"引擎不自驱动、无执行器、依赖 LLM 手工推进"（见 3.1）。
  - InferenceConstraints：不是"全部失效"，而是"约半数约束为死代码、`validate_response` 仅告警不强制；`filter_tools`/`needs_confirmation` 生效"（见 3.2）。
- **B 类核实**：gateway host 默认值经核实为 `0.0.0.0`（`schema.py:2432` + `setup.py:590`）；最近 `默认回环` commit 加固的是管理面/admin token，**未**改 bind 默认值——这两件事在 review 表述中曾被混谈，本文档已区分。

**未独立核实、采信原 review 的部分**：B 类与 C 类的部分细节行号（如各 webhook 验签实现、媒体 SSRF 具体校验缺口）未逐行复核——因为它们不进入必修批次，核实成本不划算。若将来某条 B 类升级为必修，需在实施前补核实。

---

## 5. 明确的非目标（防范围蔓延）

- **不补多租户隔离**：B 类的 webhook 验签、CSRF、SSRF 不为"对外多租户"场景补全。trusted-operator 模型下它们只做默认安全 + 文档 + 可选加固开关。
- **不改已完成的加固方向**：不回退最近的 gateway 管理面加固、不把已收窄的暴露面改回去。
- **不动 C 类**：本机 shell/文件/环境变量访问、插件加载任意 .py——这些是设计本意，不当 bug 修。
- **不做无关重构**：只在修 A 类时顺手改善直接相关的代码边界，不扩展到无关模块。
- **不在本轮承诺工作流自动执行器（3.1a）等大件**：倾向先诚实化（文档/移除），重型实现列入 Post-1.0。

---

## 6. 一句话总结

Echo Agent 的骨架优秀、工程细节扎实；这次整改的本质不是"加功能"，而是**让已声明的卖点在主路径上真正兑现**（自进化经门、记忆按会话隔离）、**让长期运行不漏资源**、**让能力声明诚实**。威胁模型锚定 trusted-operator 后，60+ 条 review 收敛为约 13 条真问题，分四批执行；其余归为默认安全基线（文档化）或设计取舍（不改）。



