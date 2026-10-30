# 兼容性参考

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

本页以 `pyproject.toml` 和当前 CI 配置为准。项目处于 `0.x` Beta 阶段；升级前应阅读[更新记录](https://github.com/fuyuxiang/echo-agent/blob/master/echo-agent-runtime/CHANGELOG.md)并备份工作区。

## Python 与平台

`requires-python = ">=3.11"`。CI 测试 Python 3.11 和 3.12；更高版本满足安装条件，但当前未列入 CI 矩阵或包分类器。项目未在打包元数据中限定操作系统。具体通道、外部命令、浏览器和可选依赖在各操作系统上的表现需按部署环境验证；不要将未测试的平台或版本视作已获正式保证。

Gateway 使用 `aiohttp`，命令行使用 `argparse`。数据存储使用 SQLite；当前没有 PostgreSQL 后端。默认 Gateway 监听 `127.0.0.1:58123`，API 前缀为 `/api/v1`；没有默认的独立 Prometheus HTTP 监听端口。

## 必需依赖

以下列表从 `pyproject.toml` 的 `project.dependencies` 生成；安装器负责解析其传递依赖。

| 包 | 版本约束 |
|---|---|
| `pydantic` | `pydantic>=2.0` |
| `pydantic-settings` | `pydantic-settings>=2.0` |
| `cryptography` | `cryptography>=41.0` |
| `regex` | `regex>=2024.11.6` |
| `pyyaml` | `pyyaml>=6.0` |
| `loguru` | `loguru>=0.7.2,<0.8` |
| `aiohttp` | `aiohttp>=3.9` |
| `aiosqlite` | `aiosqlite>=0.20` |
| `croniter` | `croniter>=1.4` |
| `numpy` | `numpy>=1.24` |
| `Pillow` | `Pillow>=13.0.0,<14` |
| `huggingface-hub` | `huggingface-hub>=0.20,<2.0` |
| `mmh3` | `mmh3>=4.1.0,<6.0.0` |
| `numpy` | `numpy>=1.26; python_version == '3.12'` |
| `numpy` | `numpy>=2.1.0; python_version == '3.13'` |
| `numpy` | `numpy>=2.3.0; python_version >= '3.14'` |
| `onnxruntime` | `onnxruntime>=1.17.0,!=1.20.0,!=1.24.0,!=1.24.1; python_version < '3.13'` |
| `onnxruntime` | `onnxruntime>1.21.0,!=1.24.0,!=1.24.1; python_version == '3.13'` |
| `onnxruntime` | `onnxruntime>=1.24.2; python_version >= '3.14'` |
| `py-rust-stemmers` | `py-rust-stemmers>=0.1.0,<0.2` |
| `requests` | `requests>=2.31,<3` |
| `tokenizers` | `tokenizers>=0.15,<1.0` |
| `tqdm` | `tqdm>=4.66,<5` |
| `questionary` | `questionary>=2.0` |
| `prompt-toolkit` | `prompt-toolkit>=3.0` |
| `httpx` | `httpx>=0.23` |
| `rich` | `rich>=13.5` |

## 可选依赖组

组名与 `pyproject.toml` 的 `project.optional-dependencies` 一致。安装示例：`pip install "echo-agent[browser]"`。`all` 是预先列出的常用可选依赖集合，不应推断它等于未来所有新增组的并集。

| 组名 | 安装示例 |
|---|---|
| `openai` | `pip install "echo-agent[openai]"` |
| `anthropic` | `pip install "echo-agent[anthropic]"` |
| `bedrock` | `pip install "echo-agent[bedrock]"` |
| `gemini` | `pip install "echo-agent[gemini]"` |
| `allproviders` | `pip install "echo-agent[allproviders]"` |
| `vector` | `pip install "echo-agent[vector]"` |
| `container` | `pip install "echo-agent[container]"` |
| `process` | `pip install "echo-agent[process]"` |
| `fal` | `pip install "echo-agent[fal]"` |
| `weixin` | `pip install "echo-agent[weixin]"` |
| `browser` | `pip install "echo-agent[browser]"` |
| `otel` | `pip install "echo-agent[otel]"` |
| `tokenizers` | `pip install "echo-agent[tokenizers]"` |
| `documents` | `pip install "echo-agent[documents]"` |
| `tui` | `pip install "echo-agent[tui]"` |
| `all` | `pip install "echo-agent[all]"` |
| `dev` | `pip install "echo-agent[dev]"` |
| `docs` | `pip install "echo-agent[docs]"` |
| `skills` | `pip install "echo-agent[skills]"` |

## 升级与数据

SQLite 表结构迁移在连接初始化时自动执行。`echo-agent migrate` 只处理 USER 记忆归属与旧 `MEMORY.*.md` 分片，不是数据库表结构迁移命令。`checkpoint restore` 不恢复数据库、会话、记忆或日志。跨版本回退应停止服务并恢复与目标版本匹配的完整备份。操作步骤见[升级与数据迁移](../operations/upgrade-migrations.md)。

## 2026-10-21 服务端资格边界

正式 wheel 已包含 Dashboard、fallback 页面、内置 skills 和默认配置；安装后运行 Gateway 不需要 Node/pnpm、Rust/Tauri 或根目录 client。只有从源码构建 Dashboard 才需要 Node（推荐 24 LTS）及锁定 pnpm 10.34.5；CI 覆盖 pnpm 10/11。Node 20 的已有构建兼容不表示仍受上游维护。

Gemini extra 现使用 `google-genai>=2.32,<3`（2026-10-21 发布的 2.32.0 已做真实 SDK 序列化、流和工具返回验证）；仍使用原 `gemini`/`google` provider 名、key/env 与模型配置，SDK 不自动执行工具。`api_base` 是该 API 服务基础地址，图像输入支持 inline data URL；不替调用者抓取任意远程图像。Gemini signed content 只回传给对应 converter。没有进行真实 Google API 联调。

FastEmbed 是随包提供的 `0.9.0+echo.1` 兼容副本，不再是 `fastembed>=0.6` 的浮动安装。Pillow 13 范围、来源/hash、许可与补丁见 `echo_agent/_vendor/fastembed/VENDOR.md`。PyMuPDF 2.0 的发行元数据错误已在干净 all 安装复现，暂限定 `<2`。cryptography 是默认凭据加密所需核心依赖；regex 是带执行期限的搜索所需核心依赖。

MCP 明确支持 2024-11-05、2025-03-26、2025-06-18，未实现 2026-07-28 的无会话/逐请求协商模型。A2A 目前是入站 legacy `tasks/send/get/cancel` 子集，不宣称 A2A 1.x 完整一致性。具体升级依据、替代方案、回滚和实测边界见[服务端评审](../reviews/server-review-and-remediation.md)。
