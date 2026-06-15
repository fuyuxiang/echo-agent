# WP-A：安全分类闭环 设计文档

**日期**：2026-06-15
**范围**：v0.3.0 路线图中 P1 安全分类修复的第一个工作包
**覆盖问题**：P1 #2（Plugin activate 先于权限检查）、P1 #5（MCP 工具风险分类）

---

## 一、背景与定位

一份针对 Echo Agent v0.2.3 的架构评估报告指出了 5 个 P1 问题。经对照真实代码逐项核实，结论是报告所述基本属实。本工作包（WP-A）是将 5 个 P1 拆分为 4 个有序工作包（A 安全分类 / B 语义评测 / C Planning 闭环 / D Goal 编排）后的**第一个**，按风险从低到高排序，A 风险最低、体量最小、无依赖，先行落地。

WP-A 只处理两个受限的安全分类修复，不做能力增强，目标是在不破坏现有运行的前提下补上两个真实安全洞。

### 本轮明确不做的事（防 scope 蔓延）

- 物理沙箱 / subprocess / 容器隔离（属 v0.5.0 Runtime isolation）
- MCP server 白名单 / trust 开关（当前无真实多 server 场景，YAGNI）
- PluginSandbox 改名（报告 P2 项，非本轮）
- 其余 P1（WP-B/C/D 各自独立成 spec）

---

## 二、问题核实结论（基于真实代码）

### P1 #2 — Plugin activate() 先于权限检查【属实，真实安全风险】

`plugins/manager.py` 的 `_load_and_activate` 执行顺序为：

1. `check_required_env()` — 仅检查环境变量存在性，非权限
2. `load_plugin_module()` — import 插件模块（执行顶层代码）
3. 构造 `PluginSandbox`（构造本身不校验任何东西）
4. `activate_fn(ctx)` — **插件代码完整执行**
5. 仅在 activate 返回后，才对 `registered_tools/hooks` 做事后核验，越权则撤销

即：插件代码在任何权限校验之前就已运行；权限检查是"先执行、后撤销已注册项"。且只检查 `tool.register` / `hook.register` 两项，`network` / `subprocess` / `filesystem.*` 从不强制。

此外 `sandbox.py` 对 legacy 插件（manifest 未声明 `permissions`）是 **fail-open**（`is_legacy → return True`），trusted 插件完全跳过检查。仓库现状：唯一的 manifest 是测试 fixture `test-plugin`，且未声明 permissions——即"legacy 是当前唯一形态"。

### P1 #5 — MCP 工具风险分类【属实，且实际洞比报告更严重】

核实发现审批门禁走的是 `risk_level` + `security/risk_classifier.py`，**不是** `execution_mode`：

- `execution_mode`（`tool_adapter.py:72` 硬编码 `"side_effect"`）真实用途是 `registry.py:149/183` 的**幂等性 / replay guard**，注释写明 "for replay guards"，不进审批决策。
- `MCPToolAdapter` **从不设置 `risk_level`**，落到 `base.py:91` 默认 `"write"`，经 `classify_risk()` 第 4 档兜底为 `WRITE`。
- 而 `WRITE` 在可信通道上**自动放行、不需审批**（risk_classifier 注释：WRITE = auto-approved on trusted channels）。

因此真正的洞是：**一个外部 MCP server 提供的、实际会删库或执行命令的工具，今天被当成普通 WRITE 自动放行**。报告所述"只读也被当 WRITE"只是表象，"危险工具也只被当 WRITE 而自动放行"才是核心风险。

---

## 三、设计决策（已与用户逐项确认）

### 决策 1：Plugin legacy 插件处理 = fail-closed + 默认权限集 + compat/strict 开关

- legacy 插件（未声明 permissions）赋予**最小默认权限集** `{tool.register, hook.register}`——这是插件最常见、最低风险的用途，也正是 test-plugin 实际所需。
- 高风险权限 `network` / `subprocess` / `filesystem.write` / `filesystem.read` **不进默认集**，必须显式声明才放行。
- 新增配置 `plugins.permission_mode: compat | strict`，默认 `compat`：
  - `compat`：上述默认集行为，现有低风险插件零改动通过。
  - `strict`：legacy 插件视为空权限集，越权直接拒绝加载（给生产环境锁死）。

**否决的备选**：永久 fail-open（修了等于没修）；严格拒绝加载所有未声明插件（破坏现状，违背"保证正常运行"约束）。

### 决策 2：MCP 风险分类 = 保守采纳 hint，降级受限、升级顺从

- `destructiveHint: true` → `EXEC`（需 allowlist / smart approval）
- `readOnlyHint: true` 且非 destructive → `READ_ONLY`（唯一的放松，仅自称只读者，且只豁免只读级操作）
- 无 hint / 字段矛盾 / 解析异常 → `WRITE`（保持现状默认，绝不因外部输入放松）

**原理**：hint 来自外部不可信 server。"降级只到 READ_ONLY、升级则顺从 destructive" 使恶意 server 无法借伪造 hint 把危险工具骗成免审批。

### 决策 3：实现走"就地最小改动"（方案 1）

改动集中在现有 3 个源文件 + config + 测试，不抽新的网关层。WP-A 是受限安全修复而非架构重构；抽独立层在当前仅一处调用方的情况下属过度设计（YAGNI），留待未来真有复用需求时再做。

---

## 四、详细设计

### 4.1 Plugin 权限门禁（P1 #2）

**`_load_and_activate` 新顺序**：

```
1. check_required_env                       (不变)
2. 构造 PluginSandbox(mode) + 计算有效权限集    ← 提前
3. load_plugin_module                        (import 仍需先发生，见下方诚实边界)
4. activate 前权限预检：插件声明的 provides(tools/hooks) 须在授权范围内
   - 越权 + strict → 拒绝加载，不调 activate
   - 越权 + compat → 记 warning，按默认集裁剪
5. activate_fn(ctx)                          (仅通过预检才执行)
6. 事后核验：ctx 实际注册的 tool/hook 再过一遍 check（防 activate 内动态越权注册）
```

**`sandbox.py` 改动**：

- 新增常量 `DEFAULT_LEGACY_PERMISSIONS = frozenset({"tool.register", "hook.register"})`
- `__init__` 增参 `mode: str = "compat"`
- 计算 `_effective_permissions`：
  - 声明了权限 → 用声明集
  - legacy + compat → 默认集
  - legacy + strict → 空集
- `check_permission` 移除 `is_legacy → return True` 的 fail-open 分支，改查 `_effective_permissions`；`trusted` 仍直通（本地用户自装的可信插件）。

**config**：新增 `plugins.permission_mode: "compat" | "strict"`，默认 `"compat"`。

**诚实边界（写入 spec 与 commit，呼应报告对"命名误导"的批评）**：
本轮是**逻辑权限门禁**，非物理沙箱。Python import 会执行模块顶层代码，无法在不 import 前提下校验权限（除非上 subprocess，属 v0.5）。故门禁针对的是 `activate()` 这一插件主动行为与注册动作，**不覆盖模块顶层代码**。

### 4.2 MCP 风险分类（P1 #5）

**`MCPToolAdapter.__init__` 解析逻辑**：

```python
annotations = mcp_tool.get("annotations", {}) or {}
read_only   = annotations.get("readOnlyHint") is True
destructive = annotations.get("destructiveHint") is True

if destructive:                      risk = EXEC
elif read_only and not destructive:  risk = READ_ONLY
else:                                risk = WRITE
self.risk_level = risk.value   # 字符串，匹配 base.py 的 risk_level: str 约定
```

如此 `classify_risk()` 第 2 优先级（tool 声明的 risk_level）即可拿到正确值，不再落到 WRITE 兜底。

**`execution_mode` 重写（幂等面）**：

```python
return "read_only" if self.risk_level == "read_only" else "side_effect"
```

**边界与防御**：
- annotations 非 dict / 字段非 bool → 走 else 返回 WRITE
- readOnly 与 destructive 同真（server 自相矛盾）→ destructive 优先判 EXEC（安全优先）

---

## 五、测试与验证策略

安全修复必须覆盖"放行"与"拦截"两个方向。

### Plugin 门禁测试（复用 test-plugin fixture）

- compat：legacy 仅注册 tool/hook → 通过（零回归）
- compat：legacy 尝试 network/subprocess/filesystem.write → 被拦
- strict：legacy → 拒绝加载，**断言 activate_fn 调用次数为 0**（"先校验后执行"的关键证明）
- 声明完整 permissions 的插件 → 按声明放行
- trusted 插件 → 直通
- 事后核验：activate 内动态注册超声明的 tool → 被撤销（保留现有行为）

### MCP 分类测试

- `readOnlyHint:true` → risk_level=read_only，execution_mode=read_only
- `destructiveHint:true` → risk_level=exec
- 无 annotations → risk_level=write（现状不变）
- readOnly+destructive 同真 → exec
- annotations 非 dict / 字段非 bool → write
- 端到端：destructive MCP 工具经 `classify_risk()` 得 EXEC（证明进了审批面，而非停在 adapter 属性）

### 回归验证（"保证项目正常运行"硬约束）

- 全量 `pytest` 通过
- `ruff check .` 通过
- 现有 plugin / MCP / 审批相关测试无回归

### 完成定义

新增用例全绿 + 全量测试无回归 + lint 通过 + config 新增项有默认值（不配置等价 compat，老用户零感知）。

---

## 六、影响面与提交策略

### 改动文件清单（就地修改 + 测试，无新增源文件）

| 文件 | 改动 |
|---|---|
| `echo_agent/plugins/sandbox.py` | 默认权限集常量、`mode` 参数、`_effective_permissions`；去 legacy fail-open |
| `echo_agent/plugins/manager.py` | `_load_and_activate` 顺序调整：权限预检前置到 activate 前；strict 越权拒绝 |
| `echo_agent/mcp/tool_adapter.py` | `__init__` 解析 annotations 算 risk_level；重写 `execution_mode` |
| `echo_agent/config/`（相应 schema） | 新增 `plugins.permission_mode`，默认 `compat` |
| `tests/` | 新增 Plugin / MCP 上述用例 |

### 配置兼容性

`permission_mode` 默认 `compat`，老用户不配置 = 现有低风险插件零感知。高风险权限从"legacy 时静默放行"收紧为"需显式声明"——有意的安全收口，写入 spec 与 commit message，避免用户困惑。

### 提交策略（遵循项目 CLAUDE.md：改完即提交、中文 message、无类型前缀、无 AI 署名）

拆 2 个 commit，便于回滚与审查：

1. Plugin 权限门禁前置 + compat/strict 模式 + 测试
2. MCP 工具按 annotations 分类风险等级 + 测试

每个 commit 前跑 `ruff check .` + `pytest`，绿了才提。

---

## 七、后续工作包（不在本轮）

- **WP-B**：Evolution 评测引入 LLM-as-judge / embedding 语义指标（P1 #3）
- **WP-C**：Planning 执行闭环——InferenceStage 内嵌步骤控制器 + 反思接入（P1 #1）
- **WP-D**：Goal/Objective 跨轮次编排层（P1 #4，路线图 v0.4.0）

依赖方向：WP-C（单次推理内循环）先于 WP-D（跨轮次外循环）；A/B 无依赖、可先行。
