# Skill 开发指南

Skill 是 Echo Agent 的知识扩展单元，以 Markdown 格式编写，可选附带 Python 脚本。Agent 根据用户意图自动选择并执行合适的 Skill。

## 目录结构

```
skills/
├── utility/
│   ├── calculator/
│   │   ├── SKILL.md         # Skill 定义（必需）
│   │   └── scripts/         # 可选脚本
│   │       └── calc.py
│   └── text-tools/
│       └── SKILL.md
├── productivity/
│   └── ...
└── research/
    └── ...
```

## SKILL.md 格式

每个 Skill 由一个 `SKILL.md` 文件定义，包含 YAML frontmatter 和 Markdown 正文：

```markdown
---
name: calculator
description: "Math calculations, unit conversions, date/time arithmetic, and currency rates. Python-powered, no API needed for math."
version: 1.0.0
metadata:
  echo:
    tags: [Math, Calculator, Units, Date, Currency, Utility]
---

# Calculator

Math, units, dates, and currency conversion.

## Math Expressions

Safe evaluation via Python:

```python
import ast
result = eval(compile(ast.parse("2**10 + 3.14 * 2", mode='eval'), '', 'eval'))
```

## Script

```bash
python3 scripts/calc.py "2**32 - 1"
python3 scripts/calc.py convert 100 km mi
```
```

## Frontmatter 字段

| 字段 | 必需 | 说明 |
|------|------|------|
| `name` | 是 | Skill 唯一标识（小写、连字符分隔） |
| `description` | 是 | 功能描述（Agent 据此判断何时使用） |
| `version` | 否 | 语义化版本号 |
| `metadata.echo.tags` | 否 | 分类标签（用于搜索和过滤） |
| `metadata.echo.dependencies` | 否 | Python 包依赖列表 |
| `metadata.echo.requires_env` | 否 | 需要的环境变量 |
| `metadata.echo.risk_level` | 否 | 风险等级：`read` / `write` / `exec` |

## 编写原则

### 1. description 是核心

`description` 是 Agent 判断是否使用该 Skill 的唯一依据。写作要求：

- 明确列出能做什么（关键词丰富）
- 说明不需要外部 API 的能力（降低使用门槛）
- 控制在 1-2 句内

```yaml
# 好的 description
description: "Math calculations, unit conversions, date/time arithmetic, and currency rates. Python-powered, no API needed for math."

# 不好的 description
description: "一个计算器工具"
```

### 2. 正文提供执行指导

Markdown 正文是 Agent 执行 Skill 时的参考手册。应包含：

- 具体的代码示例（Agent 会参考执行）
- 可用的命令和参数
- 常见用法模式
- 边界条件和注意事项

### 3. 脚本是可选的

Skill 可以：

- **纯知识型** — 只有 SKILL.md，Agent 基于内容自行推理
- **脚本辅助型** — 附带 `scripts/` 目录，Agent 调用脚本执行具体任务

## 完整示例：Web Search Skill

```markdown
---
name: web-search
description: "Search the web for current information, news, documentation, and answers. Uses DuckDuckGo, no API key required."
version: 1.0.0
metadata:
  echo:
    tags: [Search, Web, News, Research]
    dependencies: [duckduckgo_search]
    risk_level: read
---

# Web Search

Search the internet for up-to-date information.

## Basic Search

```python
from duckduckgo_search import DDGS

with DDGS() as ddgs:
    results = list(ddgs.text("query here", max_results=5))
    for r in results:
        print(f"- [{r['title']}]({r['href']})")
        print(f"  {r['body']}")
```

## News Search

```python
with DDGS() as ddgs:
    news = list(ddgs.news("topic", max_results=5))
    for n in news:
        print(f"- {n['title']} ({n['date']})")
        print(f"  {n['body']}")
```

## Best Practices

- Use specific, targeted queries
- Combine multiple searches for comprehensive coverage
- Verify facts from multiple sources
- Include date constraints for time-sensitive queries
```

## 带依赖的 Skill

如果 Skill 需要额外 Python 包，在 metadata 中声明：

```yaml
metadata:
  echo:
    dependencies: [duckduckgo_search, trafilatura]
    requires_env: []  # 无需 API Key
```

对应的 Python 包应在 `pyproject.toml` 的 `[project.optional-dependencies] skills` 中声明。

## Skill 分类

按领域组织 Skill 到对应目录：

| 目录 | 领域 | 示例 |
|------|------|------|
| `creative/` | 创意生成 | 写作辅助、头脑风暴 |
| `development/` | 软件开发 | 代码审查、重构建议 |
| `devops/` | 运维自动化 | 部署、监控 |
| `finance/` | 财务金融 | 汇率、预算计算 |
| `health/` | 健康管理 | 营养计算、运动规划 |
| `learning/` | 学习辅助 | 闪卡、笔记整理 |
| `media/` | 多媒体 | 图片处理、音频转换 |
| `productivity/` | 生产力 | 日程、任务管理 |
| `research/` | 研究分析 | 数据收集、文献整理 |
| `utility/` | 通用工具 | 计算器、文本处理 |

## 测试 Skill

### 手动测试

```bash
# 启动 Agent，然后在 CLI 通道中直接对话触发技能
echo-agent run
```

若已有网关在运行，可用瘦客户端接入同一实例：

```bash
echo-agent cli
```

两个命令都不接受待发送的消息作为参数 —— 启动后在交互界面里输入即可，例如"帮我计算 2^32 - 1"。

### 评估测试

在评估数据集中添加用例（参见 [测试与评估](testing-evaluation.md)）：

```yaml
- id: calculator_power
  input: "计算 2 的 32 次方减 1"
  expected_contains: ["4294967295"]
  expected_tools: ["skill_run"]
  tags: [skill, calculator]
```

## 检查清单

- [ ] `SKILL.md` 包含有效的 YAML frontmatter
- [ ] `name` 全局唯一（小写、连字符）
- [ ] `description` 关键词丰富，清晰描述能力边界
- [ ] 正文包含可执行的代码示例
- [ ] 如有依赖，在 metadata 和 pyproject.toml 中同时声明
- [ ] 放入正确的分类目录
- [ ] 手动测试验证 Agent 能正确触发

!!! note "技能之间没有互斥与优先级机制"
    所有启用的技能以「名称 + 分类 + 描述」的清单形式一次性注入系统提示，由模型自行判断本轮该用哪个。框架不提供互斥声明，也不对技能排序。

    因此区分度要靠 `description` 写出来：描述含义重叠的两个技能，模型只能靠措辞猜，结果不稳定。让每个技能的适用场景在描述里互相排除，比事后调整更有效。
