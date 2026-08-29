# 参与开发

欢迎参与 Echo Agent 的开发！本章节覆盖从环境搭建到发布流程的完整开发指南。

## 目录

| 文档 | 说明 |
|------|------|
| [开发环境搭建](setup.md) | Python/Node 环境、依赖安装、IDE 配置 |
| [仓库地图](repository-map.md) | 按子系统介绍目录结构与模块职责 |
| [新增 Provider](add-provider.md) | 接入新的 LLM 服务商 |
| [新增 Tool](add-tool.md) | 为 Agent 添加新工具 |
| [新增 Channel](add-channel.md) | 开发新的消息通道适配器 |
| [Skill 开发](skill-authoring.md) | 编写 SKILL.md 格式的技能 |
| [Plugin API](plugin-api.md) | 插件清单、生命周期钩子、manifest 权限准入 |
| [Dashboard 前端开发](dashboard-development.md) | React SPA 架构、组件开发、测试 |
| [测试与评估](testing-evaluation.md) | pytest、评估框架、覆盖率门禁 |
| [文档贡献指南](documentation.md) | 文档站架构、i18n、本地预览 |
| [发布流程](release-process.md) | 版本号、构建、发布检查清单 |

## 快速开始

```bash
# 克隆仓库
git clone https://github.com/fuyuxiang/echo-agent.git
cd echo-agent

# 后端：安装全部依赖（含 dev 工具）
pip install -e ".[all,dev]"

# 前端：安装 Dashboard 依赖
cd web && pnpm install --frozen-lockfile && cd ..

# 验证环境
ruff check .
python -m pytest tests/ -v --cov
cd web && pnpm build && pnpm test --run
```

## 开发原则

- **测试先行** — 新功能需附带对应测试用例
- **类型安全** — 使用 Pydantic model 和 type hints
- **最小依赖** — 可选功能通过 extras 隔离（如 `[openai]`、`[browser]`）
- **向后兼容** — 配置变更走 migration 机制，不破坏现有部署
