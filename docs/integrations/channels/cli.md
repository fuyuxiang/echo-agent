# CLI Channel

The CLI channel provides terminal-based interaction with Echo Agent in foreground mode.

---

## 概述

CLI 通道是最简单的交互方式，在前台模式 (`echo-agent run`) 下自动启用，无需额外配置。

## 配置

```yaml
channels:
  cli:
    enabled: true
```

CLI 通道在前台模式下默认启用，通常无需手动配置。

## 能力

| 能力 | 支持 |
|------|------|
| 编辑消息 | ❌ |
| 表情回应 | ❌ |
| 文件发送 | ❌ |
| 实时响应 | ✅ |
| 群聊 | ❌ |

## 使用方式

```bash
# 前台模式直接使用
echo-agent run

# 或连接到运行中的 Gateway
echo-agent cli
```

## TUI 命令

在 CLI 交互中可使用本地命令：

- `/help` — 显示帮助
- `/clear` — 清屏
- `/copy` — 复制最后回复
- `/details` — 显示详情
- `/save` — 保存对话
- `/theme` — 切换主题
- `/quit` — 退出

服务端命令（连接 Gateway 时）：

- `/approve` — 批准工具执行
- `/deny` — 拒绝工具执行
- `/approvals` — 查看待批准列表
- `/clarify` — 回复澄清请求

## 与 Gateway CLI Client 的区别

- `echo-agent run`：前台模式，CLI 通道内建于进程中
- `echo-agent cli`：TUI 薄客户端，通过 WebSocket 连接已运行的 Gateway
