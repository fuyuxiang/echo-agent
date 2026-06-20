# 配置契约清算（0.3.0）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把配置 schema 收敛到"零死字段"——7 个 fix 字段接线生效、28 个 remove 字段删除——为发布 0.3.0 清算版（稳定后贴 1.0.0）做准备。

**Architecture:** 死字段由 `schema.py` 字段的 `json_schema_extra` 元数据（`status`/`disposition`/`reason`）标注，经 `docgen.py` 渲染成 backlog 与 config-reference，`config gen-docs` 写入 docs/。清算顺着这套机制：fix 字段先改实现接线、再把 `status` 改 `effective`；remove 字段直接删定义；最后 `gen-docs` 重生文档使 backlog 清空。

**Tech Stack:** Python 3.11+、Pydantic（配置 schema）、pytest、ruff、aiohttp（gateway/channels）。

## Global Constraints

- Python 版本下限：3.11+（`pyproject.toml` `requires-python`）。
- 设计文档（spec）来源：`docs/superpowers/specs/2026-06-20-config-cleanup-v0.3.0-design.md`，本计划是其落地。
- 死字段总数 35 = 7 fix + 28 remove（schema 现标 19 fix + 16 remove，本计划把原 fix 中 12 个降级为 remove）。
- 提交信息规则（用户全局约束）：**禁止任何 Claude / Anthropic 署名**（无 `Co-Authored-By`、无 `Generated with`、无 🤖）；提交信息只描述改动本身，用简体中文。
- 提交粒度：每个 Task 末尾单独提交；只 `git add` 本 Task 涉及的文件，不夹带 README/CONTRIBUTING 等无关改动。
- 破坏性变更告知：移除 28 字段对用户是 breaking change（0.x 允许，但须在 CHANGELOG 列明替代项）。
- 验证基线：每个改实现的 Task 必须 `pytest` 相关用例通过；全计划收尾 `ruff check .` 与 `pytest` 全绿（CI 同款）。
- 中英文文档同步：凡改 README / config-reference，中英两份一起改。

---

## 文件结构总览

清算触及的文件及职责：

| 文件 | 改动职责 |
|---|---|
| `echo_agent/config/schema.py` | fix 字段改 `status` 标注；remove 字段删定义。**唯一的 schema 真相源** |
| `echo_agent/memory/store.py` | `MemoryStore.__init__` 增 `archival_threshold`/`forget_threshold` 参数，传入 `ForgettingCurve` |
| `echo_agent/agent/loop.py` | 构造 `MemoryStore`/`AgentPlanner` 时把对应 config 值传进去 |
| `echo_agent/models/providers/__init__.py` | `create_provider` 把 `config.max_retries` 注入 provider（对称于现有 `timeout_seconds` 注入） |
| `echo_agent/models/provider.py` | retry 循环用实例级 `max_retries` 而非类常量 `_RETRY_DELAYS` 写死的长度 |
| `echo_agent/agent/planning/strategies.py` | `TreeOfThoughtStrategy` 分支数由 `range(3)` 改为读 `max_branches` |
| `echo_agent/agent/planning/planner.py` | `AgentPlanner` 接收并下传 `max_branches` |
| `echo_agent/gateway/server.py` | 构造 `AgentCard` 时补传 `capabilities` |
| `echo_agent/channels/wecom.py` | `_verify`/`_webhook` 引入 AES-CBC 解密与 `msg_signature` 验签（安全项） |
| `tests/test_config_backlog.py` | 守护测试改写：校验"已清理字段不再出现于 backlog" |
| `tests/`（新增若干） | 7 个 fix 字段各补功能回归测试 |
| `CHANGELOG.md` | 列出 28 个被移除字段及替代项（破坏性变更告知） |
| `docs/config-reference.*` + `docs/config-dead-fields-backlog.md` | `config gen-docs` 重生 |
| `README.md` / `README.en.md` | 同步配置说明 |
| `pyproject.toml` | version → 0.3.0；分类器 Alpha → Beta |

**任务顺序**：先做 7 个 fix（Task 1–7，各自独立可测）→ 批量 remove（Task 8）→ 文档重生与守护测试（Task 9）→ CHANGELOG 与 README 同步（Task 10）→ 版本与分类器（Task 11）。1.0.0 最终打标在稳定观察期后，见末尾"发版收尾"。

---

### Task 1：memory threshold 两字段接线（archival_threshold / forget_threshold）

最简单的对称接线，作为开篇。`MemoryStore` 当前把阈值写死，改为从 config 流入。

**Files:**
- Modify: `echo_agent/memory/store.py:141-152`（`__init__` 签名）、`echo_agent/memory/store.py:172-176`（`ForgettingCurve` 构造）
- Modify: `echo_agent/agent/loop.py:102-110`（构造 MemoryStore 传参）
- Modify: `echo_agent/config/schema.py:1873-1886`（两字段 status 改 effective）
- Test: `tests/test_memory_threshold_config.py`（新建）

**Interfaces:**
- Consumes: `config.memory.archival_threshold: float`、`config.memory.forget_threshold: float`（schema 已有，default 0.05 / 0.01）
- Produces: `MemoryStore.__init__` 新增关键字参数 `archival_threshold: float = 0.05`、`forget_threshold: float = 0.01`

- [ ] **Step 1: 写失败测试**

新建 `tests/test_memory_threshold_config.py`：

```python
from pathlib import Path

from echo_agent.memory.store import MemoryStore


def test_thresholds_flow_into_forgetting_curve(tmp_path: Path):
    store = MemoryStore(
        memory_dir=tmp_path / "mem",
        archival_threshold=0.2,
        forget_threshold=0.1,
    )
    assert store._forgetting._archive_threshold == 0.2
    assert store._forgetting._forget_threshold == 0.1
```

- [ ] **Step 2: 运行测试确认失败**

Run: `pytest tests/test_memory_threshold_config.py -v`
Expected: FAIL —— `MemoryStore.__init__` 不接受 `archival_threshold` 参数（TypeError: unexpected keyword argument）。

- [ ] **Step 3: 改 store.py 签名与构造**

`echo_agent/memory/store.py` `__init__` 在 `contradiction_scan_on_store: bool = False,` 后追加两参数：

```python
        contradiction_scan_on_store: bool = False,
        archival_threshold: float = 0.05,
        forget_threshold: float = 0.01,
    ):
```

把 `:172-176` 的 `ForgettingCurve` 构造改为：

```python
        self._forgetting = ForgettingCurve(
            base_half_life_days=decay_half_life_days,
            archive_threshold=archival_threshold,
            forget_threshold=forget_threshold,
        )
```

- [ ] **Step 4: 运行测试确认通过**

Run: `pytest tests/test_memory_threshold_config.py -v`
Expected: PASS

- [ ] **Step 5: 在 loop.py 构造点传入 config 值**

`echo_agent/agent/loop.py:102-110` 的 `MemoryStore(...)` 调用，在 `contradiction_scan_on_store=...` 后加：

```python
            contradiction_scan_on_store=config.memory.contradiction_scan_on_store,
            archival_threshold=config.memory.archival_threshold,
            forget_threshold=config.memory.forget_threshold,
        )
```

- [ ] **Step 6: schema 两字段改 effective**

`echo_agent/config/schema.py:1873-1886`，把 `archival_threshold` 与 `forget_threshold` 的 `json_schema_extra` 由：

```python
        json_schema_extra={
            "status": "dead", "disposition": "fix",
            "reason": "store.py:174 硬编码 0.05,配置值未传入 ForgettingCurve",
        },
```

改为（forget_threshold 同理替换文案）：

```python
        json_schema_extra={
            "status": "effective", "ref": "memory/store.py:172",
            "desc_zh": "记忆归档分数阈值,低于此值进入归档层",
            "desc_en": "Archival score threshold; entries below it move to the archival tier",
        },
```

- [ ] **Step 7: 跑全量 memory 测试 + 提交**

Run: `pytest tests/ -k memory -q`
Expected: PASS

```bash
git add echo_agent/memory/store.py echo_agent/agent/loop.py echo_agent/config/schema.py tests/test_memory_threshold_config.py
git commit -m "memory 归档/遗忘阈值接线,配置生效"
```

---

### Task 2：provider max_retries 接线

`_RETRY_DELAYS=(1,2,4)` 是写死的指数退避，重试次数 = 元组长度 = 3。接线方式：加实例属性 `max_retries`，退避序列按它生成（`2**i`，保持原形态），由 `create_provider` 从 config 注入（对称于已生效的 `timeout_seconds` 注入）。

**Files:**
- Modify: `echo_agent/models/provider.py:79-92`（加实例属性）、`:194` 与 `:268`（两处 retry 循环）
- Modify: `echo_agent/models/providers/__init__.py:125-126`（注入 max_retries）
- Modify: `echo_agent/config/schema.py:870`（status 改 effective）
- Test: `tests/test_provider_max_retries.py`（新建）

**Interfaces:**
- Consumes: `config.max_retries: int`（`ProviderConfig`，schema.py:870，default 3）
- Produces: `LLMProvider.max_retries: int` 实例属性（default 3）；`LLMProvider._retry_delays() -> list[float]` 返回 `[2**i for i in range(self.max_retries)]`

- [ ] **Step 1: 写失败测试**

新建 `tests/test_provider_max_retries.py`：

```python
from echo_agent.models.provider import LLMProvider


class _Dummy(LLMProvider):
    async def chat(self, **kwargs):  # pragma: no cover - not invoked here
        raise NotImplementedError

    async def chat_stream(self, **kwargs):  # pragma: no cover
        raise NotImplementedError


def test_retry_delays_track_max_retries():
    p = _Dummy()
    p.max_retries = 5
    assert p._retry_delays() == [1, 2, 4, 8, 16]

    p.max_retries = 1
    assert p._retry_delays() == [1]
```

> 注：若 `LLMProvider` 是 ABC 且有其它抽象方法，`_Dummy` 需实现它们。先运行确认报错信息，按提示补齐抽象方法的空实现。

- [ ] **Step 2: 运行测试确认失败**

Run: `pytest tests/test_provider_max_retries.py -v`
Expected: FAIL —— `_retry_delays` 方法不存在（AttributeError）。

- [ ] **Step 3: 在 LLMProvider 加属性与方法**

`echo_agent/models/provider.py`，`LLMProvider.__init__`（:90）末尾加：

```python
        self.max_retries: int = 3
```

类内新增方法（放在 `chat_with_retry` 之前）：

```python
    def _retry_delays(self) -> list[float]:
        # Exponential backoff base delays; length == retry attempts.
        return [float(2 ** i) for i in range(max(1, self.max_retries))]
```

- [ ] **Step 4: 两处 retry 循环改用 _retry_delays()**

`provider.py:194` 与 `:268`，把 `for attempt, base_delay in enumerate(self._RETRY_DELAYS):` 改为：

```python
        for attempt, base_delay in enumerate(self._retry_delays()):
```

保留类常量 `_RETRY_DELAYS`（:82）暂不删，避免其它引用断裂——若 grep 确认无其它引用可一并删除（见 Step 6）。

- [ ] **Step 5: 运行测试确认通过**

Run: `pytest tests/test_provider_max_retries.py -v`
Expected: PASS

- [ ] **Step 6: 清理类常量（确认无其它引用）**

Run: `grep -rn "_RETRY_DELAYS" echo_agent/`
若仅剩 `:82` 的定义、无其它读取点，删除 `:82` 的 `_RETRY_DELAYS = (1, 2, 4)` 行。若仍有引用则保留。

- [ ] **Step 7: create_provider 注入 max_retries**

`echo_agent/models/providers/__init__.py:125-126`，在 `provider.request_timeout = float(config.timeout_seconds)` 后加：

```python
    provider.max_retries = int(config.max_retries)
```

并在文件末尾 `return provider` 前（:137 第二次设 timeout 之后）再加同样一行，确保外层 wrapper 也可见：

```python
    provider.max_retries = int(config.max_retries)
```

- [ ] **Step 8: schema 改 effective**

`echo_agent/config/schema.py:870` 的 `max_retries` 字段 `json_schema_extra` 改为：

```python
        json_schema_extra={
            "status": "effective", "ref": "models/providers/__init__.py:125",
            "desc_zh": "瞬时错误时的最大重试次数(指数退避)",
            "desc_en": "Max retries on transient errors (exponential backoff)",
        },
```

- [ ] **Step 9: 跑测试 + 提交**

Run: `pytest tests/test_provider_max_retries.py tests/ -k provider -q`
Expected: PASS

```bash
git add echo_agent/models/provider.py echo_agent/models/providers/__init__.py echo_agent/config/schema.py tests/test_provider_max_retries.py
git commit -m "provider 重试次数接线,max_retries 配置生效"
```

---

### Task 3：observability.trace_enabled 守卫接线

`TraceLogger`（实际在 `echo_agent/observability/monitor.py:55`）在 `loop.py:171` 无条件构造，写盘发生在 `flush_trace`（:94-97，写 `trace_{id}.json`）。接线 = 给它加 `enabled` 开关，关闭时不写盘。

**Files:**
- Modify: `echo_agent/observability/monitor.py:55-58`（`TraceLogger.__init__` 加 enabled）、`:94-97`（`flush_trace` 守卫）
- Modify: `echo_agent/agent/loop.py:171`（构造传 enabled）
- Modify: `echo_agent/config/schema.py:2245`（status 改 effective）
- Test: `tests/test_trace_enabled_config.py`（新建）

**Interfaces:**
- Consumes: `config.observability.trace_enabled: bool`（schema.py:2245）
- Produces: `TraceLogger.__init__(logs_dir=None, enabled: bool = True)`；`enabled=False` 时 `flush_trace` 不写盘

- [ ] **Step 1: 写失败测试**

新建 `tests/test_trace_enabled_config.py`。`flush_trace` 写 `trace_{id}.json`，关闭时不应产生该文件。先建一个 span 再 flush：

```python
from echo_agent.observability.monitor import TraceLogger


def test_disabled_tracelogger_does_not_write(tmp_path):
    logger = TraceLogger(logs_dir=tmp_path, enabled=False)
    span = logger.start_span("t1", "s1", name="x", kind="tool_call")
    logger.end_span(span)
    logger.flush_trace("t1")
    assert not any(tmp_path.glob("trace_*.json"))


def test_enabled_tracelogger_writes(tmp_path):
    logger = TraceLogger(logs_dir=tmp_path, enabled=True)
    span = logger.start_span("t2", "s2", name="x", kind="tool_call")
    logger.end_span(span)
    logger.flush_trace("t2")
    assert any(tmp_path.glob("trace_*.json"))
```

- [ ] **Step 2: 运行测试确认失败**

Run: `pytest tests/test_trace_enabled_config.py -v`
Expected: FAIL —— `TraceLogger` 不接受 `enabled` 参数（TypeError）。

- [ ] **Step 3: 给 TraceLogger 加 enabled 开关**

`echo_agent/observability/monitor.py:55-58` 改为：

```python
    def __init__(self, logs_dir: Path | None = None, enabled: bool = True):
        self._logs_dir = logs_dir
        self._enabled = enabled
        if logs_dir and enabled:
            logs_dir.mkdir(parents=True, exist_ok=True)
```

`flush_trace`（:96 的写盘条件）改为同时检查 enabled：

```python
        if self._enabled and self._logs_dir and spans:
            path = self._logs_dir / f"trace_{trace_id}.json"
```

- [ ] **Step 4: 运行测试确认通过**

Run: `pytest tests/test_trace_enabled_config.py -v`
Expected: PASS

- [ ] **Step 5: loop.py 构造点传入 config**

`echo_agent/agent/loop.py:171` 改为：

```python
        self.tracer = TraceLogger(
            logs_dir=workspace / config.storage.logs_dir,
            enabled=config.observability.trace_enabled,
        )
```

- [ ] **Step 6: schema 改 effective**

`echo_agent/config/schema.py:2245` 的 `trace_enabled` 字段 `json_schema_extra` 改为：

```python
        json_schema_extra={
            "status": "effective", "ref": "observability/monitor.py:55",
            "desc_zh": "是否记录执行轨迹(关闭则不写 trace 文件)",
            "desc_en": "Whether to record execution traces (off disables trace files)",
        },
```

- [ ] **Step 7: 跑测试 + 提交**

Run: `pytest tests/test_trace_enabled_config.py tests/ -k trace -q`
Expected: PASS

```bash
git add echo_agent/observability/monitor.py echo_agent/agent/loop.py echo_agent/config/schema.py tests/test_trace_enabled_config.py
git commit -m "observability.trace_enabled 接线,可关闭执行轨迹"
```

---

### Task 4：planning.max_branches 接线

ToT 分支数硬编码在 `strategies.py:133` 的 `range(3)`，且 prompt 文案也写死 "of 3"。接线 = `AgentPlanner` 接收 `max_branches` 并下传 `TreeOfThoughtStrategy`。

**Files:**
- Modify: `echo_agent/agent/planning/strategies.py:127-140`（`TreeOfThoughtStrategy.__init__` 加参数、`plan` 用之）
- Modify: `echo_agent/agent/planning/planner.py:13-29`（`AgentPlanner` 接收并下传）
- Modify: `echo_agent/agent/loop.py:191-196`（构造 planner 传 max_branches）
- Modify: `echo_agent/config/schema.py:2792`（status 改 effective）
- Test: `tests/test_planning_max_branches.py`（新建）

**Interfaces:**
- Consumes: `config.planning.max_branches: int`（schema.py:2792，default 3）
- Produces: `TreeOfThoughtStrategy.__init__(llm_call, max_branches: int = 3)`；`AgentPlanner.__init__` 新增 `max_branches: int = 3`

- [ ] **Step 1: 写失败测试**

新建 `tests/test_planning_max_branches.py`：

```python
from echo_agent.agent.planning.strategies import TreeOfThoughtStrategy


def test_tot_branch_count_configurable():
    async def _fake_llm(**kwargs):  # pragma: no cover - not called
        raise NotImplementedError

    s = TreeOfThoughtStrategy(_fake_llm, max_branches=5)
    assert s._max_branches == 5

    s_default = TreeOfThoughtStrategy(_fake_llm)
    assert s_default._max_branches == 3
```

- [ ] **Step 2: 运行测试确认失败**

Run: `pytest tests/test_planning_max_branches.py -v`
Expected: FAIL —— `TreeOfThoughtStrategy` 不接受 `max_branches`。

- [ ] **Step 3: TreeOfThoughtStrategy 加参数并用于 plan**

`strategies.py`，`TreeOfThoughtStrategy` 加 `__init__`（父类 `PlanningStrategy.__init__(self, llm_call)` 已确认存 `self._llm_call`，:50-52）：

```python
    def __init__(self, llm_call, max_branches: int = 3):
        super().__init__(llm_call)
        self._max_branches = max_branches
```

`plan` 方法 `:131-133` 改为：

```python
        candidates: list[Plan] = []
        n = self._max_branches
        for candidate_index in range(n):
```

并把 `:135-136` 的 prompt 文案 `#{candidate_index+1} (of 3 different approaches)` 改为：

```python
                        {"role": "system", "content": f"Create approach #{candidate_index+1} (of {n} different approaches). Call create_plan."},
```

- [ ] **Step 4: 运行测试确认通过**

Run: `pytest tests/test_planning_max_branches.py -v`
Expected: PASS

- [ ] **Step 5: AgentPlanner 接收并下传**

`planner.py:13-29`，`__init__` 加参数 `max_branches: int = 3`，并把 `TreeOfThoughtStrategy(llm_call)` 改为 `TreeOfThoughtStrategy(llm_call, max_branches=max_branches)`：

```python
        max_tree_depth: int = 5,
        reflection_enabled: bool = True,
        max_branches: int = 3,
    ):
        ...
            StrategyType.TREE_OF_THOUGHT: TreeOfThoughtStrategy(llm_call, max_branches=max_branches),
```

- [ ] **Step 6: loop.py 构造点传入**

`echo_agent/agent/loop.py:191-196` 的 `AgentPlanner(...)`，在 `reflection_enabled=...` 后加：

```python
            reflection_enabled=config.planning.reflection_enabled,
            max_branches=config.planning.max_branches,
        )
```

- [ ] **Step 7: schema 改 effective**

`echo_agent/config/schema.py:2792` 的 `max_branches` 字段 `json_schema_extra` 改为：

```python
        json_schema_extra={
            "status": "effective", "ref": "agent/planning/strategies.py:131",
            "desc_zh": "思维树(ToT)策略探索的候选分支数",
            "desc_en": "Number of candidate branches the Tree-of-Thought strategy explores",
        },
```

- [ ] **Step 8: 跑测试 + 提交**

Run: `pytest tests/test_planning_max_branches.py tests/ -k planning -q`
Expected: PASS

```bash
git add echo_agent/agent/planning/strategies.py echo_agent/agent/planning/planner.py echo_agent/agent/loop.py echo_agent/config/schema.py tests/test_planning_max_branches.py
git commit -m "planning.max_branches 接线,ToT 分支数可配置"
```

---

### Task 5：a2a.capabilities 接线

`server.py:212` 构造 `AgentCard` 时漏传 `capabilities`，落到 `a2a/models.py:87` 的默认值。接线 = 补传 `self._a2a_config.capabilities`。

**Files:**
- Modify: `echo_agent/gateway/server.py:212-216`（AgentCard 构造补 capabilities）
- Modify: `echo_agent/config/schema.py:2834`（status 改 effective）
- Test: `tests/test_a2a_capabilities_config.py`（新建）

**Interfaces:**
- Consumes: `config.a2a.capabilities: list[str]`（schema.py:2834）
- Produces: `AgentCard(..., capabilities=...)` 实际反映配置

- [ ] **Step 1: 写失败测试**

新建 `tests/test_a2a_capabilities_config.py`。`AgentCard` 是 dataclass，直接验证传入即可：

```python
from echo_agent.a2a.models import AgentCard


def test_agentcard_accepts_capabilities():
    card = AgentCard(
        name="x", description="y", url="http://localhost",
        capabilities=["chat", "search"],
    )
    assert card.capabilities == ["chat", "search"]
```

- [ ] **Step 2: 运行测试确认通过或失败**

Run: `pytest tests/test_a2a_capabilities_config.py -v`
Expected: 大概率 PASS（dataclass 已有该字段）。这一步确认 `AgentCard` 支持 `capabilities` 参数。若 FAIL（字段名不符），按 `a2a/models.py:87` 实际字段名修正测试。

- [ ] **Step 3: server.py 补传 capabilities**

`echo_agent/gateway/server.py:212-216` 的 `AgentCard(...)` 改为：

```python
            card = AgentCard(
                name=self._a2a_config.agent_name,
                description=self._a2a_config.agent_description,
                url=f"http://{self._config.host}:{self._config.port}",
                capabilities=self._a2a_config.capabilities,
            )
```

- [ ] **Step 4: schema 改 effective**

`echo_agent/config/schema.py:2834` 的 `capabilities` 字段 `json_schema_extra` 改为：

```python
        json_schema_extra={
            "status": "effective", "ref": "gateway/server.py:212",
            "desc_zh": "A2A AgentCard 对外声明的能力标签",
            "desc_en": "Capability tags advertised in the A2A AgentCard",
        },
```

- [ ] **Step 5: 跑测试 + 提交**

Run: `pytest tests/test_a2a_capabilities_config.py tests/ -k a2a -q`
Expected: PASS

```bash
git add echo_agent/gateway/server.py echo_agent/config/schema.py tests/test_a2a_capabilities_config.py
git commit -m "a2a.capabilities 接线,AgentCard 反映配置"
```

---

### Task 6：channels.wecom.encoding_aes_key 接线（企业微信加密回调）⚠️ 安全项

**这是清算中唯一的安全项，优先级最高，不可仅"接个值"了事。** 当前 `wecom.py` 只做明文 token 的 SHA1 验签、明文回显/解析；配了加密模式的用户，回调实际跑在裸协议上。企业微信加密回调是标准协议（AES-256-CBC + PKCS7 + 四元组验签）。依赖 `cryptography`（已在 `weixin`/`all` extra，已安装 46.0.5），无需新增依赖。

把解密逻辑放进独立模块 `wecom_crypto.py`，纯函数、易测，再由 `wecom.py` 调用。

**Files:**
- Create: `echo_agent/channels/wecom_crypto.py`（加解密 + 验签纯函数）
- Modify: `echo_agent/channels/wecom.py:24-33`（存 `encoding_aes_key`）、`:82-94`（验签含 echostr/encrypt）、`:87-94`（`_verify` 解密 echostr）、`:96-101`（`_webhook` 解密 body）
- Modify: `echo_agent/config/schema.py:670`（status 改 effective）
- Test: `tests/test_wecom_crypto.py`（新建）

**Interfaces:**
- Consumes: `config.encoding_aes_key: str`（`WeComChannelConfig`，schema.py:670；为空表示明文模式）
- Produces:
  - `decrypt_message(encoding_aes_key: str, corp_id: str, encrypt_b64: str) -> str` 返回明文 XML
  - `verify_signature(token: str, timestamp: str, nonce: str, *extra: str) -> str` 返回期望的 sha1 签名（四元组排序拼接）

企业微信加密协议要点（实现依据）：
- AES key = `base64decode(encoding_aes_key + "=")`，长度 32 字节（AES-256），IV = key 前 16 字节。
- 密文 base64 解码后 AES-CBC 解密，去 PKCS7 padding，得 `random(16B) + msg_len(4B, big-endian) + msg(msg_len B) + receive_id`。
- `msg` 即明文 XML；`receive_id` 应等于 `corp_id`，不等则报错（防伪造）。
- 验签：`sha1(sorted([token, timestamp, nonce, encrypt]) join)`，验证阶段 `encrypt` 用 query 的 `echostr`，消息阶段用 body XML `<Encrypt>` 的内容。

- [ ] **Step 1: 写失败测试（先实现一个 encrypt 辅助，自洽验证 round-trip）**

新建 `tests/test_wecom_crypto.py`。用同一套 AES 参数做加密再解密，验证 round-trip 与 receive_id 校验：

```python
import base64
import os
import struct

import pytest

from echo_agent.channels.wecom_crypto import decrypt_message, verify_signature


def _encrypt(aes_key_b64: str, corp_id: str, plaintext: str) -> str:
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
    key = base64.b64decode(aes_key_b64 + "=")
    iv = key[:16]
    rand = os.urandom(16)
    msg = plaintext.encode()
    raw = rand + struct.pack(">I", len(msg)) + msg + corp_id.encode()
    pad = 32 - (len(raw) % 32)
    raw += bytes([pad]) * pad
    enc = Cipher(algorithms.AES(key), modes.CBC(iv)).encryptor()
    return base64.b64encode(enc.update(raw) + enc.finalize()).decode()


def test_decrypt_round_trip():
    aes_key = base64.b64encode(os.urandom(32)).decode().rstrip("=")
    corp_id = "wwcorp123"
    cipher = _encrypt(aes_key, corp_id, "<xml><Content>hi</Content></xml>")
    assert decrypt_message(aes_key, corp_id, cipher) == "<xml><Content>hi</Content></xml>"


def test_decrypt_rejects_wrong_corp_id():
    aes_key = base64.b64encode(os.urandom(32)).decode().rstrip("=")
    cipher = _encrypt(aes_key, "rightcorp", "<xml/>")
    with pytest.raises(ValueError):
        decrypt_message(aes_key, "wrongcorp", cipher)


def test_verify_signature_sorts_four_tuple():
    sig = verify_signature("tok", "100", "nonce", "encblob")
    import hashlib
    expected = hashlib.sha1("".join(sorted(["tok", "100", "nonce", "encblob"])).encode()).hexdigest()
    assert sig == expected
```

- [ ] **Step 2: 运行测试确认失败**

Run: `pytest tests/test_wecom_crypto.py -v`
Expected: FAIL —— `echo_agent.channels.wecom_crypto` 模块不存在（ImportError）。

- [ ] **Step 3: 实现 wecom_crypto.py**

新建 `echo_agent/channels/wecom_crypto.py`：

```python
"""WeCom (Enterprise WeChat) encrypted-callback crypto helpers.

Implements the standard 企业微信 callback scheme: AES-256-CBC with a
PKCS7-padded payload of random(16) + len(4, big-endian) + msg + receive_id,
plus the sha1 four-tuple message signature.
"""
from __future__ import annotations

import base64
import hashlib
import struct

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes


def verify_signature(token: str, timestamp: str, nonce: str, *extra: str) -> str:
    """Return the expected sha1 signature over the sorted tuple."""
    items = sorted([token, timestamp, nonce, *extra])
    return hashlib.sha1("".join(items).encode()).hexdigest()


def decrypt_message(encoding_aes_key: str, corp_id: str, encrypt_b64: str) -> str:
    """Decrypt a WeCom <Encrypt> blob; return the plaintext XML.

    Raises ValueError on padding/format errors or receive_id mismatch.
    """
    key = base64.b64decode(encoding_aes_key + "=")
    if len(key) != 32:
        raise ValueError("encoding_aes_key must decode to 32 bytes")
    iv = key[:16]
    ciphertext = base64.b64decode(encrypt_b64)
    decryptor = Cipher(algorithms.AES(key), modes.CBC(iv)).decryptor()
    raw = decryptor.update(ciphertext) + decryptor.finalize()
    # strip PKCS7 padding
    pad = raw[-1]
    if pad < 1 or pad > 32:
        raise ValueError("invalid padding")
    raw = raw[:-pad]
    # random(16) + msg_len(4) + msg + receive_id
    msg_len = struct.unpack(">I", raw[16:20])[0]
    msg = raw[20:20 + msg_len].decode()
    receive_id = raw[20 + msg_len:].decode()
    if receive_id != corp_id:
        raise ValueError("receive_id mismatch (possible forgery)")
    return msg
```

- [ ] **Step 4: 运行测试确认通过**

Run: `pytest tests/test_wecom_crypto.py -v`
Expected: PASS（3 个用例全过）

- [ ] **Step 5: wecom.py 接入加密模式**

`echo_agent/channels/wecom.py`：

(a) `__init__`（:29 后）存密钥：

```python
        self._token = config.token
        self._encoding_aes_key = config.encoding_aes_key
```

(b) 顶部加导入（:16 附近）：

```python
from echo_agent.channels.wecom_crypto import decrypt_message, verify_signature
```

(c) `_verify`（:87-94）改为：加密模式下用四元组（含 echostr）验签，再解密 echostr 回显：

```python
    async def _verify(self, request: web.Request) -> web.Response:
        signature = request.query.get("msg_signature", "")
        timestamp = request.query.get("timestamp", "")
        nonce = request.query.get("nonce", "")
        echostr = request.query.get("echostr", "")
        if self._encoding_aes_key:
            expected = verify_signature(self._token, timestamp, nonce, echostr)
            if expected != signature:
                return web.Response(status=403, text="Forbidden")
            try:
                plain = decrypt_message(self._encoding_aes_key, self._corp_id, echostr)
            except ValueError:
                return web.Response(status=403, text="Forbidden")
            return web.Response(text=plain)
        # plaintext mode (legacy)
        if self._check_signature(signature, timestamp, nonce):
            return web.Response(text=echostr)
        return web.Response(status=403, text="Forbidden")
```

(d) `_webhook`（:96-101）在解析前先解密：取出 body XML 的 `<Encrypt>`，四元组验签后解密成业务 XML 再 `ET.fromstring`：

```python
    async def _webhook(self, request: web.Request) -> web.Response:
        body = await request.text()
        if self._encoding_aes_key:
            try:
                outer = ET.fromstring(body)
                encrypt = outer.findtext("Encrypt", "")
                signature = request.query.get("msg_signature", "")
                timestamp = request.query.get("timestamp", "")
                nonce = request.query.get("nonce", "")
                if verify_signature(self._token, timestamp, nonce, encrypt) != signature:
                    return web.Response(status=403, text="Forbidden")
                body = decrypt_message(self._encoding_aes_key, self._corp_id, encrypt)
            except (ET.ParseError, ValueError):
                return web.Response(text="success")
        try:
            root = ET.fromstring(body)
        except ET.ParseError:
            return web.Response(text="success")
```

（其余 `_webhook` 逻辑 :103 起不变。）

- [ ] **Step 6: 写 wecom.py 集成测试（明文模式不回归 + 加密模式验签拒绝）**

在 `tests/test_wecom_crypto.py` 追加（或新建 `tests/test_wecom_channel.py`），验证明文模式仍工作、加密模式坏签名被拒。构造 channel 需要最小 config，可参考现有 `tests/` 中 wecom 相关用例的构造方式（先 `grep -rln "WeComChannel" tests/`）。若无现成范式，至少保留 Step 1 的纯函数测试作为安全保证，并在此手动验证：

Run: `grep -rln "WeComChannel\|wecom" tests/`
按结果决定是否补 channel 级测试；纯 crypto 测试已覆盖核心安全逻辑。

- [ ] **Step 7: schema 改 effective**

`echo_agent/config/schema.py:670` 的 `encoding_aes_key` 字段 `json_schema_extra` 改为：

```python
        json_schema_extra={
            "status": "effective", "ref": "channels/wecom.py:87",
            "desc_zh": "企业微信加密回调的 EncodingAESKey,留空则为明文模式",
            "desc_en": "EncodingAESKey for WeCom encrypted callbacks; empty means plaintext mode",
        },
```

- [ ] **Step 8: 跑测试 + 提交**

Run: `pytest tests/test_wecom_crypto.py -v && ruff check echo_agent/channels/`
Expected: PASS

```bash
git add echo_agent/channels/wecom_crypto.py echo_agent/channels/wecom.py echo_agent/config/schema.py tests/test_wecom_crypto.py
git commit -m "企业微信加密回调接线,encoding_aes_key 生效(AES-CBC 解密+四元组验签)"
```

---

### Task 7：批量移除 28 个死字段

把 28 个 remove 字段从 `schema.py` 删除。这是破坏性变更，操作机械但要逐个确认：删字段定义本身，不能误删整段 model。删完用 schema 的死字段自检立即验证。

**28 个待删字段**（snake 路径）：

```
storage.backend
tools.exec.timeout_seconds
tools.mcp_servers.transport
gateway.enable_progressive_edit
memory.hybrid_retrieval
memory.adaptive_forgetting
evaluation.enabled
evaluation.parallel_cases
gateway.platforms.enabled
models.cost_limit_daily_usd
multi_agent.worker_profiles.provider
models.routes.context_window
memory.max_episodes
memory.embedding_batch_size
memory.consolidation_idle_seconds
scheduler.dead_task_timeout_seconds
storage.workspace_dir
observability.show_tool_calls
observability.show_route_decisions
skills.auto_load
skills.platform_disabled
gateway.max_agent_cache_size
gateway.platforms.home_chat_id
gateway.platforms.reply_mode
agent.reasoning_effort
session.archive_after_hours
knowledge.require_citations
gateway.platforms.home_channel
```

**Files:**
- Modify: `echo_agent/config/schema.py`（删 28 个字段定义）
- 可能 Modify: 引用被删字段的代码（grep 兜底，见 Step 2）

**Interfaces:**
- Produces: schema 中 `status="dead"` 字段数归零（fix 已在 Task 1-6 改 effective）

- [ ] **Step 1: 删除前先建一个守护测试（确保删干净）**

新建 `tests/test_no_dead_fields.py`：

```python
from echo_agent.config.metadata import iter_fields
from echo_agent.config.schema import Config


def test_no_dead_fields_remain():
    dead = [
        f.snake_path for f in iter_fields(Config)
        if f.extra.get("status") == "dead"
    ]
    assert dead == [], f"still dead: {dead}"
```

> `iter_fields` 来自 `echo_agent/config/metadata.py:68`；`FieldInfo` 有 `snake_path` 与 `extra`（metadata.py:21,25）。

- [ ] **Step 2: 运行测试确认失败**

Run: `pytest tests/test_no_dead_fields.py -v`
Expected: FAIL —— 列出当前所有仍标 dead 的字段（应为 28 个 remove，fix 字段若 Task 1-6 已做则不在列）。

- [ ] **Step 3: 逐个删除字段定义**

对上表每个字段，在 `echo_agent/config/schema.py` 中定位其 `<name>: <type> = Field(... "status": "dead" ... "disposition": "remove" ...)` 整段定义并删除。注意嵌套字段删的是子 model 内的字段行，不是整个 model：
- `tools.exec.timeout_seconds` → 删 `ExecToolConfig`（或对应 model）内的该字段
- `tools.mcp_servers.transport` → 删 MCP server 配置 model 内的 `transport` 字段
- `gateway.platforms.{enabled,home_channel,home_chat_id,reply_mode}` → 删 `PlatformConfig` 内这 4 个字段
- `models.providers` 不涉及（max_retries 是 fix）；`models.routes.context_window` 删 route 配置 model 内字段；`models.cost_limit_daily_usd` 删 models 顶层字段
- `multi_agent.worker_profiles.provider` → 删 `WorkerProfileConfig` 内 `provider` 字段
- 其余为各自 model 的顶层字段，直接删整段 `Field(...)` 定义

逐个删除后频繁保存，不要一次性大改后再验证。

- [ ] **Step 4: grep 兜底——确认没有代码还在读被删字段**

Run（对每个被删字段的 snake 末段做一次，示例）：

```bash
grep -rn "reasoning_effort\|cost_limit_daily_usd\|archive_after_hours\|require_citations\|max_episodes\|embedding_batch_size\|consolidation_idle_seconds\|dead_task_timeout_seconds\|workspace_dir\|show_tool_calls\|show_route_decisions\|auto_load\|platform_disabled\|max_agent_cache_size\|home_chat_id\|home_channel\|reply_mode\|enable_progressive_edit\|hybrid_retrieval\|adaptive_forgetting" echo_agent/ --include="*.py" | grep -v "test"
```

预期：除已删的 schema 定义外无残留读取点（这些字段本就是死的，理论上无运行时读取）。**例外**：`cli/setup.py` 的配置向导可能写入 `home_channel`/`trace_enabled` 等——属于"写入向导项"。被删字段对应的向导项也要从 `setup.py` 移除，否则向导会写一个 schema 不接受的字段，导致校验失败。逐个处理 grep 命中的向导项。

- [ ] **Step 5: 运行守护测试确认通过**

Run: `pytest tests/test_no_dead_fields.py -v`
Expected: PASS —— 无 dead 字段残留。

- [ ] **Step 6: 跑配置加载冒烟测试**

Run: `pytest tests/ -k config -q`
Expected: PASS —— schema 仍能正常构造、默认配置可加载。若有用例引用被删字段，更新该用例（删字段对应断言）。

- [ ] **Step 7: 提交**

```bash
git add echo_agent/config/schema.py echo_agent/cli/setup.py tests/test_no_dead_fields.py
git commit -m "移除 28 个死字段(虚假能力/冗余/孤儿),配置 schema 收敛"
```

> 注：若 Step 4 未触及 `setup.py`，提交时去掉该文件。

---

### Task 8：重生配置文档 + 改写 backlog 守护测试

清算后 `config gen-docs` 重新生成参考文档与 backlog（此时 fix/remove 两节应为空）。现有 `tests/test_config_backlog.py` 断言 `storage.backend`/`reasoning_effort` 存在于 backlog——这些已被删，测试必然失败，需改写为"校验已清理"。

**Files:**
- Modify: `tests/test_config_backlog.py`（改写断言）
- Regenerate: `docs/config-reference.md`、`docs/config-reference.en.md`、`docs/config-reference.yaml`、`docs/config-reference.en.yaml`、`docs/config-dead-fields-backlog.md`

**Interfaces:**
- Consumes: `echo-agent config gen-docs`（`config_cmd.py:195`，写入 `docs/`）

- [ ] **Step 1: 改写 test_config_backlog.py**

把 `tests/test_config_backlog.py` 改为（删掉断言"已知死字段存在"的两处，新增"backlog 不再含已清理字段"）：

```python
"""Tests for dead-field backlog generation."""
from __future__ import annotations

from echo_agent.config.docgen import render_backlog


def test_backlog_has_no_dead_fields_after_cleanup():
    out = render_backlog()
    # 清算后 fix/remove 两节应为空 —— 已清理字段不再出现
    assert "storage.backend" not in out
    assert "reasoning_effort" not in out and "reasoningEffort" not in out
    assert "cost_limit_daily_usd" not in out


def test_backlog_renders_without_error():
    # 即便无死字段也应能正常生成(标题或空表)
    out = render_backlog()
    assert isinstance(out, str)
    assert out.strip() != ""
```

- [ ] **Step 2: 运行测试确认通过**

Run: `pytest tests/test_config_backlog.py -v`
Expected: PASS（依赖 Task 7 已删除这些字段）

- [ ] **Step 3: 重生文档**

Run: `echo-agent config gen-docs`
（若 CLI 入口名不同，用 `python -m echo_agent config gen-docs`；参考 `config_cmd.py:223`。）

- [ ] **Step 4: 核对 backlog 已清空**

Read: `docs/config-dead-fields-backlog.md`
Expected: fix 节与 remove 节为空（或整个文件只剩标题）。

- [ ] **Step 5: 提交**

```bash
git add tests/test_config_backlog.py docs/config-reference.md docs/config-reference.en.md docs/config-reference.yaml docs/config-reference.en.yaml docs/config-dead-fields-backlog.md
git commit -m "重生配置参考文档,backlog 清空,守护测试改为校验已清理"
```

---

### Task 9：CHANGELOG 与 README 同步（破坏性变更告知 + post-1.0 候选）

记录 28 个被移除字段及替代项，并把 post-1.0 候选清单写入文档。

**Files:**
- Create/Modify: `CHANGELOG.md`（若不存在则创建）
- Modify: `docs/config-dead-fields-backlog.md` 末尾或新建 `docs/roadmap.md`（post-1.0 候选）
- Modify: `README.md` / `README.en.md`（如配置说明涉及被删字段则更新）

**Interfaces:** 无代码接口，纯文档。

- [ ] **Step 1: 确认 CHANGELOG 是否存在**

Run: `ls CHANGELOG.md 2>/dev/null && echo EXISTS || echo MISSING`

- [ ] **Step 2: 写 0.3.0 CHANGELOG 条目**

在 `CHANGELOG.md` 顶部加入（无文件则新建，遵循 Keep a Changelog 格式）：

```markdown
# Changelog

## [0.3.0] - 2026-06-20

### 配置契约清算（破坏性变更）

收敛配置 schema 至"零死字段"：每个配置项要么真生效，要么移除。

**接线生效（原先配了不生效，现已生效）：**
- `memory.archival_threshold` / `memory.forget_threshold` — 记忆归档/遗忘阈值
- `models.providers[].max_retries` — provider 重试次数
- `observability.trace_enabled` — 执行轨迹开关
- `planning.max_branches` — ToT 分支数
- `a2a.capabilities` — AgentCard 能力声明
- `channels.wecom.encoding_aes_key` — 企业微信加密回调（安全修复）

**移除（虚假能力/冗余/孤儿，配置中请删除以下项）：**
- `storage.backend`、`storage.workspace_dir`（filesystem 后端不存在）
- `tools.exec.timeout_seconds`（请用 `tools.code_exec.timeout_seconds`）
- `tools.mcp_servers{}.transport`（按 url/command 自动选择）
- `gateway.enable_progressive_edit`（请用 `gateway.emit_progress_events`）
- `gateway.platforms{}.enabled` / `home_channel` / `home_chat_id` / `reply_mode`
- `gateway.max_agent_cache_size`
- `memory.hybrid_retrieval`、`memory.adaptive_forgetting`、`memory.max_episodes`、`memory.embedding_batch_size`、`memory.consolidation_idle_seconds`
- `models.cost_limit_daily_usd`（请用 `cost.daily_budget_usd`）
- `models.routes[].context_window`（实际由 `session.context_window_tokens` 驱动）
- `multi_agent.worker_profiles[].provider`
- `evaluation.enabled`、`evaluation.parallel_cases`（并发度用 CLI `--parallel`）
- `scheduler.dead_task_timeout_seconds`
- `observability.show_tool_calls`、`observability.show_route_decisions`
- `skills.auto_load`、`skills.platform_disabled`
- `agent.reasoning_effort`、`session.archive_after_hours`、`knowledge.require_citations`（转入 post-1.0 候选）
```

- [ ] **Step 3: 写 post-1.0 候选清单**

新建 `docs/roadmap.md`（或加到 backlog 末尾），内容来自 spec §6：

```markdown
# Post-1.0 候选

以下能力在 0.3.0 移除，将来带各自设计、以非破坏性加字段方式回归：

| 能力 | 移除的字段 | 回归时需要的设计 |
|---|---|---|
| 推理强度控制 | `agent.reasoning_effort` | 抹平各家 provider reasoning 语义差异的映射层 |
| 会话归档 | `session.archive_after_hours` | 归档器：触发条件、归档目标、可检索性 |
| 可关闭引用 | `knowledge.require_citations` | 引用生成处的开关 + 默认行为决策 |
| 主动推送 | `gateway.platforms{}.home_channel` / `home_chat_id` | 推送触发时机、去重、频控 |
| 多后端存储 | `storage.backend` / `storage.workspace_dir` | filesystem 后端实现 + 后端抽象 |
```

- [ ] **Step 4: 检查 README 是否提及被删字段**

Run: `grep -n "reasoning_effort\|cost_limit_daily\|storage.backend\|hybrid_retrieval" README.md README.en.md`
若命中，更新相应说明；未命中则 README 无需改。

- [ ] **Step 5: 提交**

```bash
git add CHANGELOG.md docs/roadmap.md
git commit -m "0.3.0 变更日志与 post-1.0 候选清单,告知移除字段及替代项"
```

> README 若有改动，单独 `git add README.md README.en.md` 再补一次提交。

---

### Task 10：版本号与分类器（0.3.0 / Beta）

**Files:**
- Modify: `pyproject.toml:7`（version）、`pyproject.toml:15`（分类器）

**Interfaces:** 无代码接口。

- [ ] **Step 1: 改版本号**

`pyproject.toml:7` `version = "0.2.3"` → `version = "0.3.0"`

- [ ] **Step 2: 改分类器**

`pyproject.toml:15` `"Development Status :: 3 - Alpha",` → `"Development Status :: 4 - Beta",`

- [ ] **Step 3: 全量验证（CI 同款）**

Run: `ruff check . && pytest -q`
Expected: 全绿。若有红，回到对应 Task 修复后再继续。

- [ ] **Step 4: 提交**

```bash
git add pyproject.toml
git commit -m "版本号升至 0.3.0,分类器调整为 Beta"
```

---

## 发版收尾（稳定观察期后，单独执行）

0.3.0 发布并观察稳定后，再做 1.0.0（不属于本计划的 TDD 任务，单独执行）：

- [ ] `pyproject.toml` version → `1.0.0`
- [ ] 分类器 `Development Status :: 4 - Beta` → `5 - Production/Stable`
- [ ] CHANGELOG 加 1.0.0 条目（"已稳定之物的正式命名，无新破坏性变更"）
- [ ] 打 tag `v1.0.0`

**为什么分两步**：清算本身是破坏性的（移除 28 字段），在 0.x 完成所有 break，让 1.0 成为"已稳定之物的正式命名"，而非"希望它稳定"。
