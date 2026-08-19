# 测试与评估

Echo Agent 的测试策略和评估框架。

---

## 测试框架

- **后端**：pytest + pytest-asyncio + pytest-cov
- **前端**：Vitest + @testing-library/react

## 运行测试

```bash
# 全部后端测试
python -m pytest tests/ -v --cov

# 只运行特定模块
python -m pytest tests/test_memory*.py -v

# 前端测试
cd web && pnpm test --run
```

## 测试结构

```
tests/
├── test_agent*.py          # Agent loop 相关
├── test_channels*.py       # 通道适配器
├── test_config*.py         # 配置系统
├── test_gateway*.py        # Gateway API
├── test_memory*.py         # 记忆系统
├── test_knowledge*.py      # 知识库
├── test_security*.py       # 安全策略
├── test_tools*.py          # 工具执行
├── test_evolution*.py      # 进化系统
├── test_eval*.py           # 评估框架
├── test_plugins*.py        # 插件系统
├── test_mcp*.py            # MCP 客户端
├── test_a2a*.py            # A2A 协议
├── test_docs*.py           # 文档一致性
└── fixtures/               # 测试夹具
```

## 覆盖率要求

`pyproject.toml` 中配置 `fail_under = 75`，CI 会在覆盖率低于此阈值时失败。

## 评估框架

Echo Agent 内建了 Agent 行为评估框架：

```bash
echo-agent eval
```

### 评估数据集

YAML 格式定义测试用例：

```yaml
- id: test_web_search
  input: "搜索今天的新闻"
  expected_tools: [web]
  expected_contains: ["新闻"]
  forbidden_tools: [shell]
  max_iterations: 5
```

### 评估指标

- `contains_all` — 输出包含预期内容
- `tool_usage_correctness` — 使用了正确的工具
- `iteration_efficiency` — 迭代次数在合理范围
- `response_quality` — 响应质量评分
- `forbidden_tools_check` — 未使用禁止的工具
- `semantic_quality` — 语义质量评估

### 评估报告

输出包含：通过率、各项指标得分、耗时统计。
