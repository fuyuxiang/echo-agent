# Echo Agent 文档

Echo Agent 提供两种使用入口：**EchoAgent Desktop（Client）** 面向本机交互式工作，**Echo Agent Runtime** 面向终端运行、消息接入和自托管服务。本手册涵盖安装、日常使用、运行原理与部署维护。

## 选择适合你的入口

| 需求 | 建议入口 | 开始阅读 |
| --- | --- | --- |
| 在界面中处理文件、编写代码、操作浏览器或电脑 | Desktop | [桌面端快速开始](desktop/quickstart.md) |
| 在终端使用 Agent，或通过网页管理服务 | Runtime | [Runtime 快速开始](getting-started/quickstart.md) |
| 接入消息平台、Webhook 或定时调度 | Runtime | [通道集成](integrations/channels/index.md)与[后台服务](operations/background-service.md) |
| 理解执行、记忆和工具调用 | 两端原理 | [架构总览](concepts/architecture.md) |
| 为自己的应用提供 Agent 服务 | Runtime Gateway | [Gateway 集成](integrations/gateway/index.md) |

## Desktop：在工作区中完成任务

桌面端将对话、任务计划、工具执行、权限确认、文件变更与交付物放在同一工作台中。代码任务可使用内嵌 IDE；办公任务可生成文档；浏览器与电脑操作按当前设备的能力启用。

首次使用的顺序是：安装应用 → 选择工作目录与模型 → 提交任务 → 检查执行结果。

- [安装与环境要求](desktop/installation.md)
- [第一个任务](desktop/quickstart.md)
- [任务、会话与权限](desktop/tasks-permissions.md)
- [代码工作台](desktop/coding.md)
- [浏览器与电脑操作](desktop/automation.md)
- [文档、知识与记忆](desktop/knowledge-memory.md)
- [模型与扩展](desktop/models-extensions.md)
- [专家团与定时任务](desktop/collaboration-scheduling.md)
- [数据管理与故障排查](desktop/operations.md)

## Runtime：运行自己的 Agent 服务

Runtime 负责接收请求、组织上下文、调用模型、执行工具、保存会话与记忆。Gateway 在此基础上提供 Dashboard、HTTP API 和 WebSocket 接入。消息通道、知识库、MCP 和技能进化按配置启用。

```bash
pip install "echo-agent[all]"
echo-agent setup
echo-agent gateway
```

启动后访问 <http://127.0.0.1:58123/>。安装环境、首次验证与终端使用方式见 [Runtime 快速开始](getting-started/quickstart.md)。

| 目标 | 对应文档 |
| --- | --- |
| 安装、选择依赖和检查环境 | [安装指南](getting-started/installation.md) |
| 配置模型并完成一个可验证的任务 | [使用流程](guides/first-workflow.md) |
| 管理会话、知识、记忆和费用 | [Runtime 使用指南](guides/index.md) |
| 保持服务在线、升级或恢复数据 | [部署与运维](operations/index.md) |
| 查询准确的命令、参数和接口 | [参考手册](reference/index.md) |

## 理解两端的关系

两端都采用“模型推理 → 工具执行 → 结果反馈”的 Agent 工作方式，但当前源码使用不同的运行时实现。Desktop 在应用进程内运行 Rust 执行引擎；独立 Runtime 使用 Python 执行引擎。Desktop 可以独立使用，无需先部署 Gateway。

模型连接、权限配置、记忆与插件机制分别由两端管理。阅读具体操作前，请先确认页面适用范围；配置文件不可直接互换。详见[架构总览](concepts/architecture.md)。

## 如何阅读本手册

**快速开始**给出最短可用流程与成功标准；**使用指南**围绕实际任务说明操作；**核心概念**解释设计思路、数据流和边界；**参考手册**列出精确参数；**参与开发**介绍工程与验证方式。

核心概念不要求阅读源码。架构图标注边界与职责，时序图说明交互顺序，流程图说明判断分支，数据流图说明数据来源、处理和存储。

## 项目状态

项目处于 Beta 阶段，两端版本独立发布。升级前应阅读对应发行说明并备份数据。Runtime 需要 Python 3.11+；Desktop 的正式发行目标为 macOS 和 Windows，具体架构及依赖见各自安装指南。

项目采用 [MIT 许可证](https://github.com/fuyuxiang/echo-agent/blob/master/LICENSE)。问题反馈见 [GitHub Issues](https://github.com/fuyuxiang/echo-agent/issues)。
