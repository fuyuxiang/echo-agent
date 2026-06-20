# 子项目 A:配置可读性基础设施 — 设计文档

- 日期:2026-06-20
- 状态:已确认设计,待写实现计划
- 上游背景:`echo-agent` setup/配置体系的"专业性、全面性、可读性"系统性改造,已拆为三个子项目(A 配置可读性 → C 配置完整性整顿 → B 向导覆盖面扩展)。本文件只覆盖**子项目 A**。

---

## 1. 背景与问题

对 `echo_agent/config/schema.py` 全部约 100 个字段做了全量代码读取点核查(6 个并行审计 agent,覆盖全部配置组)。两项结论决定了本子项目的形态:

1. **可读性缺口:** setup 向导只覆盖约 1/3 配置;向导未覆盖的字段没有面向用户的注释文档,用户只能去读 `schema.py` 源码。`default.yaml` 的注释是写给开发者的(防 schema 漂移),不是配置参考。无 `config explain/dump/validate` 类命令。
2. **有效性缺口(更严重):** 约 35 个字段是"死字段"——schema 里定义了,但运行时代码从不读取,用户配了不报错也不生效。这与"文档必须只含有效配置"的要求直接冲突。

死字段按危害分三类:

- **A 类 假配置(会真坑用户):** `storage.backend`(永远 SQLite,filesystem 后端不存在)、`gateway.platforms.*.enabled`(禁用平台无效)、`gateway.enable_progressive_edit`(关不掉)、`memory.archival_threshold`/`forget_threshold`(硬编码 0.05/0.01,改了静默忽略)、`agent.reasoning_effort`(未接线 provider)、`wecom.encoding_aes_key`(加密回调密钥未接线)。
- **B 类 命名误导:** `models.cost_limit_daily_usd`(真正生效的是 `cost.daily_budget_usd`)、`tools.exec.timeout_seconds`(只有 `code_exec.timeout_seconds` 生效)、`provider.max_retries`(重试硬编码)、`mcp.transport`(按 url/command 隐式选)。
- **C 类 纯孤儿:** `observability.{trace_enabled,show_tool_calls,show_route_decisions}`、`scheduler.dead_task_timeout_seconds`、`skills.{auto_load,platform_disabled}`、`planning.max_branches`、`a2a.capabilities`、`session.archive_after_hours`、`storage.workspace_dir`、`multi_agent.worker_profiles[].provider`、`evaluation.{enabled,parallel_cases}`、`memory.{hybrid_retrieval,adaptive_forgetting,max_episodes,embedding_batch_size,consolidation_idle_seconds}`、`knowledge.require_citations` 等。

完整审计结论(含每字段精确读取点)见项目记忆 `config-dead-fields-audit.md`。

## 2. 设计原则与边界

**核心架构决策:本子项目全程不修改任何运行时行为。** 死字段的修复(尤其是 A 类的安全/正确性 bug)风险等级与"配置可读性"完全不同,不能埋在文档 PR 里——它们交给子项目 C,其中安全相关项走快车道。

A 的职责锁定三件事:**如实呈现现状 + 防止再退化 + 给 C 铺路**。

不做(明确排除):
- 不改任何字段的类型、默认值;不删除任何字段;不接线任何死字段。
- 文档生成器不做运行时动态生成(改为开发期生成 + 提交进仓 + CI 校验)。
- 守护测试不自动 grep 验证 effective 字段的读取点(静态推断不可靠,见 §3.4)。

## 3. 组件设计

### 3.1 字段有效性元数据(地基)

给 `schema.py` 每个字段附结构化元数据,作为文档生成器、`config validate`、守护测试、未来 B 阶段向导**共用的单一真相源**。

承载方式:Pydantic `Field(json_schema_extra={...})`,不侵入字段类型与默认值。

```python
trigger_ratio: float = Field(
    default=0.7,
    json_schema_extra={
        "status": "effective",
        "ref": "agent/compression/compressor.py:46",
        "desc_zh": "上下文占用达到该比例时触发压缩",
        "desc_en": "Compress context when usage reaches this ratio",
    },
)

archival_threshold: float = Field(
    default=0.05,
    json_schema_extra={
        "status": "dead",
        "reason": "store.py:174 硬编码 0.05,该值未传入 ForgettingCurve",
        "disposition": "fix",  # fix | remove | keep
    },
)
```

元数据契约:
- `status`:`effective` | `dead`,**强制**(守护测试断言每字段都有)。
- `effective` 字段必须有:`ref`(读取点 `相对路径:行号`)、`desc_zh`、`desc_en`。
- `dead` 字段必须有:`reason`(为何不生效)、`disposition`(`fix` 该接线 / `remove` 该删 / `keep` 暂留)。

范围:本组件只**标注**,约 100 个字段全覆盖。effective 补 desc,dead 补 reason+disposition。

### 3.2 文档生成器

位置:`echo_agent/config/docgen.py`。纯函数(输入字段树,输出字符串,不碰磁盘),便于测试。

**只渲染 `status == "effective"` 的字段**,死字段一律不进用户文档。

两份产物:

1. **注释版 YAML 模板** → `docs/config-reference.yaml`(中文)/ `docs/config-reference.en.yaml`(英文)。每字段上方一行注释(`desc_zh`/`desc_en` + 默认值/范围),值为默认值,可直接复制修改。字段名用 camelCase(与 `default.yaml`、向导写出的配置一致)。
2. **Markdown 参考表** → `docs/config-reference.md`(中文)/ `docs/config-reference.en.md`(英文)。按配置组分章,每章一张表:字段 / 类型 / 默认值 / 可选值 / 说明。Literal 类型自动列出可选值。字段名同时标注 camelCase 与 snake_case。

双语:复用现有 `echo_agent/cli/i18n` 的 locale 机制。

触发与防漂移:不做用户日常命令,而是开发期生成命令(如 `config gen-docs`)+ CI 校验"产物与仓库已提交版本一致"(沿用 `default.yaml` 防漂移思路)。PR 改了 schema 字段但忘了重新生成文档,CI 会失败。

### 3.3 `config` 子命令

在 `__main__.py` 按现有 argparse `add_parser` 模式注册 `config` 子命令;实现放 `echo_agent/cli/config_cmd.py`。面向用户的运行时命令。

**`config dump`** — 打印当前生效的完整配置
- 走 `load_config()` 真实合并链(schema 默认 → `default.yaml` → 用户 yaml → env),输出最终 `Config`。
- 默认 YAML;`--format json` 可选。
- `--source`:标注每个值来自哪一层(default / file / env)。
- **脱敏:** api_key / token / secret / password 类字段值打成 `****`(复用向导识别敏感字段的同套规则)。

**`config explain <key>`** — 解释单个配置项(点路径,如 `memory.archivalThreshold`)
- 输出:中/英说明、类型、默认值、当前生效值、可选值(Literal)、来源层。
- **死字段如实告知:** 该 key 为 dead 时,明确提示「⚠ 此项当前未生效(原因:…)」。

**`config validate`** — 校验当前配置文件
- 加载并跑 Pydantic 校验,错误翻译成可读信息(复用 `loader.py` 已有翻译)。
- **未知字段/拼写错误检测:** 用户写了 schema 不存在的 key 时主动报告(Pydantic 默认忽略多余字段,这里要主动查)。
- **死字段提示:** 用户显式设置了某 dead 字段时提示「此项不会生效」。
- 退出码:有错误返回非 0(可接入 CI / 部署前检查)。

边界:识别"未知字段"需把用户 YAML 键与 schema 已知键比对;schema 用 camelCase alias,用户可能写 snake_case,比对两种都接受(与 loader `populate_by_name=True` 一致)。

### 3.4 守护测试(防再退化)

位置:`tests/test_config_metadata_guard.py`,纯 pytest,无外部依赖。

断言三件事:
1. **完整性(核心):** 遍历 `Config` 全部字段(含嵌套子模型),每个字段元数据都带 `status`。漏标 → 失败。效果:加新字段必须声明 effective/dead,否则 CI 拦下。
2. **元数据齐全:** effective 字段有 `desc_zh`+`desc_en`;dead 字段有 `reason`+`disposition`。
3. **ref 弱校验:** effective 字段 `ref` 指向的文件存在(只验文件路径真实,不强求行号精确)。

有意不做:不自动 grep 验证 effective 字段真有读取点——整组透传、`getattr`、字符串拼字段名都会导致静态推断漏判,误报会让测试变噪音。有效性初始判定靠本次人工审计;守护测试只保证"分类不缺失、元数据完整、ref 路径不假"。

配套:§3.2 的"文档产物与 schema 一致"CI 校验归在本层一起跑。

### 3.5 死字段处置 backlog

产物:`docs/config-dead-fields-backlog.md`,从元数据 `disposition` 字段**自动生成**(不手写),作为子项目 C 的输入。

按处置意向分三组,每条带:字段路径、当前 status/reason、读取点缺口、处置建议。
- **fix:** 标注严重度;安全/正确性相关项标「快车道,建议不等 B 直接排」。
- **remove:** 纯孤儿。
- **keep:** 有意保留的(若有)。

## 4. 组件总览

| # | 组件 | 产物 | 改运行时? |
|---|------|------|:---:|
| 1 | 字段元数据 | schema.py 每字段 status + desc/reason | 否 |
| 2 | 文档生成器 | config-reference.{yaml,md}(中英)+ CI 校验 | 否 |
| 3 | config 命令 | dump / explain / validate | 否 |
| 4 | 守护测试 | 分类完整性 + 元数据齐全 + ref 存在 | 否 |
| 5 | 处置 backlog | dead-fields-backlog.md(自动生成) | 否 |

## 5. 测试策略

- **docgen 单元测试:** 喂构造的字段树,断言只渲染 effective、注释取对应 locale、Literal 可选值正确列出、camelCase 字段名。
- **config 命令测试:** dump 脱敏生效、`--source` 标注正确;explain 对 effective/dead/未知 key 三种路径输出正确;validate 对合法/非法/含未知字段/含 dead 字段四种情况退出码与提示正确。
- **守护测试:** 见 §3.4。
- 全部走项目现有 pytest + ruff;实现遵循 TDD。

## 6. 验收标准

1. `schema.py` 全部字段带完整元数据,守护测试通过。
2. `docs/config-reference.{md,en.md,yaml,en.yaml}` 生成并提交;CI 校验一致性通过。
3. 文档中**不含任何 dead 字段**。
4. `config dump/explain/validate` 三命令可用,行为符合 §3.3(含脱敏、死字段提示、未知字段检测)。
5. `docs/config-dead-fields-backlog.md` 自动生成,分组与处置意向完整,可作为子项目 C 的 spec 素材。
6. 全程未改动任何运行时行为;`ruff check` 与 `pytest` 全绿。

## 7. 后续子项目(非本次范围)

- **子项目 C(配置完整性整顿):** 按 backlog 处置死字段。安全/正确性真 bug(`gateway.platforms.enabled`、`storage.backend` 等)走快车道,可与 B 并行;接线类(`reasoning_effort` 等)与清理类分批。
- **子项目 B(向导覆盖面扩展):** 把缺失的高价值配置接进 setup 向导,复用 A 的字段元数据作为问题说明文案来源。
