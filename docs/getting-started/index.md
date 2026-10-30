# 开始使用

先选择使用方式，再准备环境和模型。两端的安装方式、配置和数据分别管理。

| 你的目标 | 阅读路线 |
| --- | --- |
| 使用桌面应用处理代码、文档与自动化 | [Desktop 安装](../desktop/installation.md) → [第一个任务](../desktop/quickstart.md) |
| 在终端运行 Agent 或部署服务 | [Runtime 安装](installation.md) → [Runtime 快速开始](quickstart.md) |
| 把 Agent 接入自己的应用 | [Runtime 快速开始](quickstart.md) → [Gateway](../integrations/gateway/index.md) |

## Runtime 的最短流程

需要 Python 3.11+ 和一个可用模型服务。安装后运行向导，再选择终端交互或 Gateway 服务。

```bash
pip install "echo-agent[all]"
echo-agent setup
echo-agent gateway
```

打开 <http://127.0.0.1:58123/>。完整环境准备、模型验证与成功标准见[快速开始](quickstart.md)。仅在终端使用时，可以选择 `echo-agent run`；同一工作区不要同时启动两个完整 Runtime。

## 首次使用后

阅读[第一个 Runtime 工作流](../guides/first-workflow.md)完成一项可核对的任务，再根据需要配置[模型](../guides/models/index.md)、[消息通道](../integrations/channels/index.md)或[后台服务](../operations/background-service.md)。升级与删除数据前阅读[升级与卸载](upgrade-uninstall.md)。
