[中文](CONTRIBUTING.md) · [English](CONTRIBUTING.en.md)

# 贡献指南

感谢你愿意为 Echo Agent 出一份力。无论是修 bug、加功能，还是改文档，都欢迎。

## 搭建开发环境

```bash
git clone https://github.com/fuyuxiang/echo-agent.git   # 国内可用 https://gitee.com/fuyuxiang/echo-agent.git
cd echo-agent
uv venv venv --python 3.11 && source venv/bin/activate
uv pip install -e ".[all,dev]"
```

没有 `uv` 也可以用标准 venv：

```bash
python3.11 -m venv venv && source venv/bin/activate
pip install -e ".[all,dev]"
```

## 提交前检查

PR 前请确保 lint 和测试都通过，CI 会在每个 PR 上跑同样的检查：

```bash
ruff check .
pytest
```

## 提交 PR

- 从 `master` 切出特性分支，保持改动聚焦单一主题。
- commit message 只描述本次改动本身，保持干净。
- 涉及面向用户的改动时，同步更新中英文 README（`README.md` / `README.en.md`）。
- 改动配置项时，运行 `echo-agent config gen-docs` 重新生成配置参考文档。

## 参与方向

不知道从哪下手？这些地方很需要帮助：

- **通道适配器** — 接入更多消息平台
- **内置工具** — 扩充开箱即用的工具集
- **MCP 集成** — 对接更多 MCP server
- **技能示例** — 贡献可复用的技能样例
- **评测数据集** — 丰富自进化引擎的评测用例
- **文档完善** — 补充教程、示例与说明
- **部署模板** — Docker、Kubernetes、云厂商等部署方案

## 交流

- QQ 群：[47572014](https://qm.qq.com/q/JWOPDBNssw)
- [GitHub Issues](https://github.com/fuyuxiang/echo-agent/issues) — bug 反馈与功能建议
- [GitHub Discussions](https://github.com/fuyuxiang/echo-agent/discussions) — 设计讨论与使用问题
