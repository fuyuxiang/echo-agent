# 快速开始

本节帮助你从零开始安装并运行 Echo Agent。

---

## 章节目录

| 章节 | 说明 |
|------|------|
| [安装](installation.md) | 系统要求、安装方式、依赖配置 |
| [快速上手](quickstart.md) | 5 分钟完成首次对话 |
| [升级与卸载](upgrade-uninstall.md) | 版本升级、数据迁移、完全卸载 |

---

## 概览

Echo Agent 的安装流程非常简单：

```bash
pip install echo-agent[all]
echo-agent setup
echo-agent run
```

三条命令即可启动一个具备记忆和技能的 AI Agent。`setup` 向导会引导你配置模型 API Key 和基本参数。

!!! tip "推荐环境"
    建议使用 Linux 或 macOS 作为生产环境。Windows 用户推荐通过 WSL2 运行，也支持 Windows 原生安装。
