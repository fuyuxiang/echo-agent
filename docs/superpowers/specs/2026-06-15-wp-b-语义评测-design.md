# WP-B：语义评测 设计文档

**日期**：2026-06-15
**范围**：v0.3.0 路线图中 P1 评测质量修复的第二个工作包
**覆盖问题**：P1 #3（Evolution metrics 无语义评分，promotion gate 仅字面匹配）

---

## 一、背景与定位

架构评估报告指出 `evaluation/metrics.py` 的 5 个指标全部基于字面/词袋匹配，`EvolutionEngine` 的 `PromotionGate` 完全依赖这些指标聚合的 `pass_rate`/`avg_score` 决定技能晋升，无法判断语义质量退化。

核实真实代码后发现两个额外事实：
1. `response_quality`（词袋重叠）是唯一适合语义化的指标；其他四个（exact_match / contains_all / tool_usage / iteration_efficiency）是结构化事实判断，字面精确反而更严格。
2. `baseline.yaml` 里 7 个 case 全部未填 `expected_output`，`response_quality` 在生产中实际上是死代码——添加语义指标必须同步补充数据，否则新指标同样不会激活。

### 本轮明确不做的事

- embedding 相似度指标（`provider.embed` 不保证可用，YAGNI）
- judge 模型独立配置项（无真实差异化需求，YAGNI）
- gate.py 逻辑修改（runner 层自动传递，不需要改决策逻辑）
- E2E evolution 集成测试（超出 WP-B 范围）
- 其余 P1（WP-C/D 各自独立）

---

## 二、关键设计决策（已逐项确认）

### 决策 1：新增独立指标，不替换现有指标

- `response_quality`（词袋）保留：它是现有 avg_score 计算的一部分，替换会影响历史基准的可比性。
- 新增 `semantic_quality`（LLM-as-judge）作为独立指标，只在 `case.expected_output` 非空时追加到 `CaseResult.metrics`。
- 现有 7 个 case 无 `expected_output`，添加新指标后其 score 计算完全不变，历史基准零影响。

### 决策 2：纳入 avg_score，不加开关

- `semantic_quality` 和其他指标一样参与 `CaseResult.score` 均值，进而影响 `EvalReport.avg_score` 和 `gate._decide`。
- 理由：语义评测的价值在于约束晋升决策，仅上报等于没有约束力。
- runner.py 的指标均值计算已天然支持可变数量的 metrics，无需改 gate 逻辑。

### 决策 3：复用主推理模型，不加专用 judge 配置

- 通过 `ModelRouter` 的 `inference` 任务路由调用，和 evolver 生成候选一致。
- 自我评分偏差在 A/B 对比场景下是对称的，不影响相对比较结论。
- `provider=None` 时语义指标自动跳过，向后兼容。

### 决策 4：新文件承载 I/O 指标，保持 metrics.py 纯函数

- `echo_agent/evaluation/semantic_metrics.py`（新建）：async 函数 + LLM 调用。
- `echo_agent/evaluation/metrics.py`：保持同步纯函数不变。
- 边界原则：纯逻辑和 I/O 逻辑物理分离，各自可独立测试。

---

## 三、详细设计

### 3.1 semantic_metrics.py（新文件）

**接口**：

```python
async def semantic_quality(
    expected: str,
    actual: str,
    provider: LLMProvider,
    *,
    model: str | None = None,
) -> MetricResult:
```

**Prompt 策略**：

- System：你是评测 AI 回复质量的裁判，只关注语义等价性，不关注措辞差异
- User：提供 `expected`（参考答案）和 `actual`（模型输出），要求输出 `{"score": float, "reasoning": "..."}`
- 打分标准：1.0 = 语义完全等价；0.5 = 部分相关；0.0 = 语义无关或错误
- 使用 `provider.chat_with_retry`，与 evolver 调用方式一致

**pass 阈值**：`score >= 0.7`（比词袋 0.5 略严，因为语义判断更准确）

**容错策略**：
- JSON 解析失败（非法格式/缺字段）→ score=0.5（中性，不惩罚也不奖励），details 记录原始 response
- LLM 调用抛异常 → score=0.5，details 记录 error 信息
- score 越界（<0 或 >1）→ clamp 到 [0.0, 1.0]

### 3.2 runner.py 改动

**`__init__` 新增可选参数**：

```python
def __init__(
    self,
    agent_loop: AgentLoop,
    parallel: int = 3,
    timeout: int = 120,
    provider: LLMProvider | None = None,
    judge_model: str | None = None,
):
```

**`run_case` 追加语义指标**（紧接现有 response_quality 块之后）：

```python
if case.expected_output:
    result.metrics.append(response_quality(case.expected_output, result.response))
if case.expected_output and self._provider is not None:
    result.metrics.append(
        await semantic_quality(
            case.expected_output, result.response,
            self._provider, model=self._judge_model,
        )
    )
```

两个指标并存：词袋（低代价、确定性强）+ 语义（高质量判断），各自独立贡献 score 均值，两个数据点比一个更有诊断价值。

**gate.py 改动**：`PromotionGate.__init__` 新增 `provider: LLMProvider | None = None`，透传给 `EvalRunner` 工厂函数。`app.py` 构造 `PromotionGate` 时补 `provider=self._provider`。

### 3.3 baseline.yaml 数据补充

给以下 3 个 case 补充 `expected_output`（选择标准：开放式语义判断，字面匹配不足以评估质量）：

**explain_evolution**（开放式定义题，词袋只检查 "agent" 是否存在）：
```yaml
expected_output: "A self-evolving agent is an AI system that improves its own skills or behavior over time based on experience or feedback."
```

**ask_for_help**（操作指引，语义准确性比子串匹配更有意义）：
```yaml
expected_output: "You can check the agent's current status by running the echo-agent status command in the CLI."
```

**tool_choice_reasoning**（推理质量，expected_contains 为空，语义评分是唯一质量约束）：
```yaml
expected_output: "For a single short sentence, no summarization tool is needed — the text is already concise and can be used as-is."
```

**不补的 case**：`chat_smoke`（打招呼，expected_contains 已够）、`numeric_reasoning`（答案是 "42"，字面精确更严格）、`refuse_nonsense`（安全拒绝，语义不适合量化）、`list_skills`（工具调用，expected_tools 已覆盖）。

---

## 四、测试策略

### semantic_metrics.py 测试（新文件）

全部 mock provider，不真正调 LLM：

| 用例 | mock 输入 | 期望结果 |
|---|---|---|
| happy path 高分 | `{"score": 0.9, "reasoning": "..."}` | score=0.9, passed=True, name="semantic_quality" |
| 低分 fail | `{"score": 0.6, "reasoning": "..."}` | score=0.6, passed=False |
| JSON 解析失败 | `"looks good"`（纯文本）| score=0.5, passed=False |
| LLM 抛异常 | raise Exception("timeout") | score=0.5, details 含 error |
| score 越界 clamp | `{"score": 1.5, "reasoning": "..."}` | score=1.0 |

### runner.py 测试（扩展现有）

- `provider=None`：有 `expected_output` 的 case 只追加 `response_quality`，semantic_quality 不追加，metrics 数量正确
- `provider` 非 None：有 `expected_output` 的 case 同时追加两个指标
- 无 `expected_output`：semantic_quality 不被调用（provider mock 调用次数为 0）

### 回归

- `python -m pytest` 全量通过
- `ruff check .` 通过
- 现有 baseline.yaml case 的 `score` 计算不变（无 `expected_output` 的 case 不受影响）

---

## 五、影响面与提交策略

### 改动文件清单

| 文件 | 操作 | 说明 |
|---|---|---|
| `echo_agent/evaluation/semantic_metrics.py` | 新建 | LLM-as-judge 指标 |
| `echo_agent/evaluation/runner.py` | 修改 | 新增 provider/judge_model 参数，追加语义指标 |
| `echo_agent/evolution/gate.py` | 修改 | PromotionGate 接收并透传 provider |
| `echo_agent/app.py` | 修改 | 构造 PromotionGate 时传入 provider |
| `data/eval/baseline.yaml` | 修改 | 3 个 case 补充 expected_output |
| `tests/test_semantic_metrics.py` | 新建 | semantic_quality 单元测试 |
| `tests/test_eval_runner.py`（或同等） | 修改 | runner 语义指标用例 |

### 提交策略（拆 3 个 commit）

1. 新增 semantic_metrics.py + 测试
2. runner/gate/app 接线 + baseline 数据补充
3. 回归修复（若有）

---

## 六、后续工作包

- **WP-C**：Planning 执行闭环——InferenceStage 内嵌步骤控制器 + 反思接入（P1 #1）
- **WP-D**：Goal/Objective 跨轮次编排层（P1 #4，路线图 v0.4.0）
