# 参考手册

本章节汇集 Echo Agent 的完整技术参考资料，适用于日常开发、运维和集成场景。

## 文档索引

| 文档 | 说明 |
|------|------|
| [CLI 命令](cli.md) | 所有命令行子命令及参数详解 |
| [TUI 命令](tui-commands.md) | 交互式终端 UI 中的斜杠命令 |
| [配置指南](configuration-guide.md) | YAML 配置字段、加载顺序、Profile 系统 |
| [环境变量](environment-variables.md) | ECHO_AGENT_ 前缀变量与覆盖规则 |
| [内置工具](tools.md) | 30 个内置工具的用途、参数与风险等级 |
| [安全配置矩阵](security-profile-matrix.md) | 安全级别与工具配置的组合矩阵 |
| [Gateway API](gateway-api.md) | HTTP REST 端点、认证、请求/响应格式 |
| [WebSocket 协议](websocket-protocol.md) | 实时通信帧格式与事件类型 |
| [文件系统布局](filesystem-layout.md) | 数据目录结构与文件用途 |
| [兼容性](compatibility.md) | 平台、Python 版本、依赖兼容矩阵 |
| [术语表](glossary.md) | 核心概念与术语定义 |

## 版本信息

- **当前版本**: v0.3.8 Beta
- **Python 要求**: 3.11+
- **支持平台**: Linux, macOS, Windows (WSL2 推荐)

## 约定说明

本手册使用以下标记约定：

!!! tip "提示"
    表示推荐做法或快捷操作。

!!! warning "注意"
    表示可能影响系统行为的重要事项。

!!! danger "危险"
    表示安全风险或不可逆操作。

!!! question "Q: 常见问题"
    以问句为标题，用于 FAQ 条目。
