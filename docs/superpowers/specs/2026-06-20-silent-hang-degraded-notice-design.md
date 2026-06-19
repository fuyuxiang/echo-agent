# 消除"静默挂起":回合降级通知收敛层 设计文档

- 日期:2026-06-20
- 状态:待评审
- 范围:第二档(兜底收敛层 + 审批链路修正),审批默认采用"保守但快速失败"
- 关联记忆:`gateway-api-auth-bypass`(同类——安全机制在边界条件下被静默绕过)

## 1. 背景与故障复盘

### 1.1 现象

用户通过微信通道发出一条调研任务,agent 收到并开始执行。期间 LLM provider(minimaxi)抖动:连续 3 次 `Embedding API error: No embedding data received`,紧接着 4 次 `Smart approval: unrecognized response (escalating)`。此后用户**两小时未收到任何回复**(已读不回)。两小时后用户追问"调研完成了吗",才收到调研结果。

### 1.2 根因(已核实)

故障链路:

```
provider 抖动 → smart approval 拿不到可解析的 APPROVE/DENY/ESCALATE
  → smart_approve 一律返回 escalate(security/smart_approval.py:77,80)
  → 落到 ApprovalGate Step 13 manual flow(agent/approval_gate.py:150-155)
  → exec 在 require_approval 默认清单中(config/schema.py:321-328)
    → request_approval 创建 PENDING(permissions/manager.py:95-103)
  → _publish_approval_request 发出审批消息(approval_gate.py:192,231-258,is_final=True)
  → wait_for_decision 阻塞最长 wait_timeout_seconds=300s(approval_gate.py:205)
  → 超时 → 返回错误 ToolResult(approval_gate.py:219-227)
  → inference_stage.py:369-378 仅把错误 append 到 messages 回灌模型,从不 publish 给用户
     ← 静默就发生在这一步
```

**核心缺陷**:工具失败/审批超时的结果**只回灌给模型**,而没有强制 publish 一条面向用户的降级通知。最终兜底(`inference_stage.py:549-556`)只保证 `response_text` 非空(一句泛化英文),不保证内容有意义、也不保证在流式 `outbound_sent` 已为真时仍能送达。此时模型本身正因 provider 抖动而失灵,"回灌模型"这条自愈路径同时失效,于是表现为完全静默。

### 1.3 修正的早期误判(诚实记录)

初步推测该 exec 命令被 `default_policy="approve"` 静默放行执行(fail-open)。核实后**推翻**:`exec` 在 `require_approval` 默认清单内,会进入 manual flow 创建 PENDING,走不到 `default_policy` 放行分支。真实缺陷是"审批走完整流程、超时后结果被静默吞掉",而非审批被绕过。

### 1.4 两个无法仅凭静态代码坐实的点

1. 审批消息当时是否真正送达微信:`_publish_approval_request` 确实发出且 `is_final=True`(不被基础层 `should_deliver` 丢弃),但 `weixin.send` 在 token 失效/errcode 异常时只打 warning、不重试、不通知——provider 抖动期间微信出站可能同时失败。需完整出站日志确认。
2. 4 次 escalate 仅间隔数秒、未出现 5 分钟阻塞,与"每次 manual flow 应阻塞 300s"在时序上不完全吻合。可能与 embedding 重试或实际 `wait_timeout_seconds` 配置有关,需原始日志或 `echo-agent.yaml` 确认。

**本设计不依赖上述两点的答案**:统一收敛层对"审批消息未送达"与"超时后静默吞掉"两种路径都覆盖。

## 2. 横向对比(业界同类项目)

业界同类项目都遇到过同型故障并已专门修复。共性结论:

> **"无论内部发生什么,用户最终一定收到一条有意义的消息"被当作硬不变量**,由一个统一收敛点兜住所有异常终态。典型做法是在 reply-runner 永远返回一个 final 结果并显式检测空响应;或在 gateway 末端做无条件兜底,审批超时/拒绝/推送失败统一转 BLOCKED 文本("Silence is not consent")。

| 维度 | echo-agent 现状 | 业界同类 A | 业界同类 B |
|---|---|---|---|
| 审批超时/解析失败 | 超时后静默吞掉;解析失败伪装成 escalate | fail-closed,带 reason 错误结果 | 三条路统一转 BLOCKED 文本 |
| 空/失败回复兜底 | 仅保证非空(英文);流式下可能被跳过 | 外层永远 final,空响应注入可见 payload | 四道闸门 + gateway 归一化 |
| provider 抖动 | 有 fallback 链但无用户可见降级通知 | SDK 重试 + 多模型 fallback + 通知 | 重试退避 + fallback + 凭证轮换 + 通知 |
| 审批等待 vs 会话锁 | 全程持 session_lock 最长 300s,堵后续 | abort signal + 队列 interrupt | 工作线程跑,不堵事件循环 |

echo-agent 已有的韧性:逐工具熔断器 `ToolCircuitBreaker`(`loop.py:247`)、模型 fallback 链(`inference_stage.py:600`)、重复调用拦截、空回复重试。**缺的是把这些异常终态统一冒泡为"保证送达用户"的收敛层**——现有兜底分散在 inference_stage 内部,且都依赖"模型仍正常工作"这一前提。

## 3. 设计目标与不变量

### 3.1 硬不变量

> 每一个进入处理的 inbound 事件,无论内部经历什么(工具失败、审批超时/受阻、provider 抖动、空回复、异常),最终必须送达用户一条有意义的中文消息。"静默"是 bug,不是可接受状态。

### 3.2 审批安全默认

> provider 失灵不该让高风险命令更容易执行,而应让它更明确地暂停并告知。审批系统失灵时默认拒绝(保守),但必须出声(快速失败 + 明确通知),不进入长时间阻塞等待。

### 3.3 范围边界

- 纳入:第 1 节收敛点、2.1 smart 快速失败、2.3 审批结果冒泡通知。
- **不纳入(留作独立后续)**:2.2 审批等待期间释放 session 锁(并发锁重构,风险与本次安全修复不应混做);provider 韧性增强(embedding 降级、fallback 用户通知)作为独立迭代。

## 4. 详细设计

### 4.1 统一收敛点(对应不变量)

不新建大模块,复用 `loop.py:_on_inbound`(`loop.py:578-620`)这一天然出口——它已有 try/except 兜底和 `if response_text and not result.outbound_sent` 的发送逻辑,强化为"保证发出且内容有意义"。收敛逻辑集中一处,避免在工具循环内部散落多处 publish。

### 4.2 审批链路 2.1:smart 解析失败 → 保守快速失败

区分 escalate 的两种语义:

- **模型主动判 ESCALATE**(认为"不确定,该问人")→ 保留走 manual flow,合理。
- **解析失败 / 调用异常**(provider 返回垃圾,根本没判出来)→ 不再伪装成 escalate,返回新的明确状态 `unavailable`。

`smart_approve` 返回类型由 `Literal["approve","deny","escalate"]` 扩展为加入 `"unavailable"`。`ApprovalGate` 收到 `unavailable` 时直接返回 denial,打上 `notify_user=True` + 原因 `approval_unavailable`,不进入阻塞等待。

### 4.3 审批链路 2.3:审批结果冒泡为用户通知

`inference_stage.py:369-378` 处理 denial 时,除现有的 append 到 messages(保留回灌模型的自愈能力)外,将带 `notify_user=True` 的 denial 记入待通知列表,由收敛点在回合结束时统一发出——即使模型之后又生成了别的回复,审批受阻这件事也不被淹没。

两类需要冒泡的事件及其携带通知的位置:

- **审批类 denial**(经 `ApprovalGate.check` 返回):
  - `approval_unavailable`(2.1,smart 解析失败/异常)——在 `ApprovalGate` 处设 `notify_user=True`、`notice=approval_unavailable 文案`。
  - `approval_timeout`(manual flow `wait_for_decision` 超时,`approval_gate.py:219-227`)——同样设 `notify_user=True`、`notice=approval_timeout 文案`(含 `tool`/`id`)。
  这两类在 `inference_stage` 处理 `approval_check.denial` 时,若 `denial.notify_user` 为真,则把 `denial.notice` 追加进 `degraded_notices`。
- **重复调用被拦**(`repeat_blocked`,`inference_stage.py:385-406`):该路径不经过 `ApprovalCheck`,由 inference_stage 在触发拦截时**直接**把 `repeat_blocked` 文案追加进 `degraded_notices`(同样保留回灌模型的 `result_text_blocked`)。

### 4.4 数据流改造

让"待通知的降级事件"从工具循环冒泡到出口:

- `ApprovalCheck`(`approval_gate.py:20-23`)新增:`notify_user: bool = False`、`notice: str = ""`(中文降级文案)。
- `inference_stage` 的 `_LoopResult`(`inference_stage.py:558-567`)新增:`degraded_notices: list[str]`,收集本回合所有"应让用户知道"的降级事件(审批受阻、审批超时、重复调用被拦等)。
- `_ProcessResult`(`_process_event` 返回值)将 `degraded_notices` 透传到 `_on_inbound`。

### 4.5 收敛点决策逻辑(`_on_inbound` 出口)

回合结束、发送前判定:

```
若 response_text 是有意义的正常回复:
    正常发送(现有逻辑)
    若同时有 degraded_notices:作为补充紧随其后(只发一次)
若 response_text 为空 / 等于泛化英文兜底 / outbound 实际未发出:
    用 degraded_notices 合成一条明确中文通知发出
    若连 degraded_notices 都没有(纯异常):
        发通用中文兜底
```

绝不允许"回合结束但用户什么都没收到"。在收敛点做统一空响应兜底。

### 4.6 中文降级文案分类

| 原因 | 文案 |
|---|---|
| `approval_unavailable`(审批系统失灵,2.1) | ⚠️ 这步需要执行命令,但安全审批暂时不可用(模型服务异常),已暂停。请稍后回复让我重试。 |
| `approval_timeout`(等真人审批超时) | ⚠️ 这步需要你确认执行 `{tool}`,等待超时已暂停。回复 `/approve {id}` 继续,或 `/deny {id}` 取消。 |
| `repeat_blocked`(重复调用被拦) | ⚠️ 我多次尝试同一操作未成功,已停止。可能是工具或服务异常,请稍后重试。 |
| 通用兜底 | ⚠️ 处理你的请求时遇到问题,已中止。可以稍后重试或换个说法。 |

### 4.7 边界处理

- 用 `outbound_sent` 标志确保不会既发正常回复又发兜底(防双发)。
- `degraded_notices` 去重:同一原因只发一次,避免 4 次重复调用刷 4 条通知。
- 收敛点发出的通知 `is_final=True`,确保不被微信 `should_deliver`(`channels/base.py:56-68`)丢弃。

## 5. 测试策略

理念:用测试把"任何路径都不静默"钉死,防止未来重构回归。沿用 `tests/` 现有惯例。

### 5.1 不变量测试(新增 `tests/test_approval_degraded_notice.py`)

- smart 解析失败 → 返回 `unavailable`,denial 带 `notify_user=True`、原因 `approval_unavailable`。
- smart 调用抛异常 → 同上。
- 模型主动 ESCALATE → 仍走 manual flow(回归保护,确保 2.1 不误伤)。
- 审批超时 → 收敛点产出 `approval_timeout` 文案。
- 4 次重复调用 → 只发 1 条 `repeat_blocked`(去重)。

### 5.2 收敛点测试(扩展 loop 相关测试)

- 空回复 + 有 degraded_notices → 发合成中文通知,而非泛化英文。
- 空回复 + 无 notices(纯异常)→ 发通用中文兜底。
- 正常回复 + 有 notices → 正常回复发出,通知作为补充,只发一次(防双发)。
- 正常回复无异常 → 行为不变(回归保护)。

### 5.3 端到端契约测试(推荐)

模拟完整"provider 抖动"回合(embedding 失败 + smart approval 解析失败),断言最终 bus 上一定出现至少一条 `is_final=True` 的 outbound。复现故障链路并锁定回归。

### 5.4 验证步骤

- `ruff check .` 通过
- `pytest`(尤其新增测试)全绿
- 不降低现有覆盖率(当前约 75%)

## 6. 影响面

| 文件 | 改动 |
|---|---|
| `echo_agent/security/smart_approval.py` | 返回类型加 `unavailable`;解析失败/异常返回 `unavailable` 而非 `escalate` |
| `echo_agent/agent/approval_gate.py` | `ApprovalCheck` 加 `notify_user`/`notice`;处理 `unavailable`;denial 携带通知语义 |
| `echo_agent/agent/pipeline/inference_stage.py` | `_LoopResult` 加 `degraded_notices`;denial 冒泡;重复拦截冒泡 |
| `echo_agent/agent/pipeline/response_stage.py` / 返回链 | `_ProcessResult` 透传 `degraded_notices` |
| `echo_agent/agent/loop.py` | `_on_inbound` 出口收敛决策 + 中文文案合成 |
| `tests/test_approval_degraded_notice.py` 等 | 新增/扩展测试 |

## 7. 不做的事(YAGNI)

- 不重构 session 锁(2.2 独立后续)。
- 不引入新的 provider 重试/熔断框架(已有 fallback 链与熔断器,本次不扩展)。
- 不改微信通道出站重试逻辑(独立问题,本次仅保证收敛层通知 `is_final=True` 可送达)。
- 不引入主动 heartbeat/notify 通道(超出本次范围)。
