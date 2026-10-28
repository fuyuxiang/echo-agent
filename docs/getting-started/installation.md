# 安装指南

## 系统要求

| 项目 | 最低要求 | 推荐 |
|------|----------|------|
| Python | 3.11 | 3.12 |
| 操作系统 | 见下表 | CI 在 Ubuntu 上运行 |
| 内存 | 依赖模型与工作负载 | 根据部署压测确定 |
| 磁盘 | 依赖可选组件与数据量 | 根据部署容量规划 |

**操作系统支持矩阵：**

| 操作系统 | 状态 | 说明 |
|----------|------|------|
| Ubuntu | CI 覆盖 | 使用 Python 3.11 和 3.12 运行测试 |
| Debian / macOS / Windows + WSL2 | 需部署验证 | 安装条件满足，但不在当前 CI 操作系统矩阵内 |
| Windows 原生 / Alpine Linux | 需逐项验证 | 外部命令、进程工具或二进制依赖可能存在平台差异 |

---

## 安装方式

=== "pip（推荐）"

    安装包内预列的常用可选依赖集合（各 extra 的准确范围见 `pyproject.toml`）：

    ```bash
    pip install "echo-agent[all]"
    ```

    或仅安装核心 + 指定提供商：

    ```bash
    # 最小安装（仅核心运行时）
    pip install echo-agent

    # 按需添加模型提供商
    pip install "echo-agent[openai]"
    pip install "echo-agent[anthropic]"
    pip install "echo-agent[gemini]"
    pip install "echo-agent[bedrock]"

    # 组合安装
    pip install "echo-agent[openai,anthropic,vector,browser]"
    ```

=== "源码安装"

    ```bash
    git clone --filter=blob:none --sparse https://github.com/fuyuxiang/echo-agent.git
    cd echo-agent
    git sparse-checkout set echo-agent-runtime
    cd echo-agent-runtime
    python3 -m venv .venv
    source .venv/bin/activate
    (cd web && pnpm install --frozen-lockfile && pnpm build)
    pip install ".[all]"
    ```

    开发模式额外安装开发依赖：

    ```bash
    pip install -e ".[all,dev]"
    ```

    本目录包含完整 Dashboard 源码及锁文件。已单独下载 `echo-agent-runtime/` 时直接进入它执行安装，无需 Git。正式 `echo-agent-runtime-<version>.tar.gz` 已含前端产物，可跳过 Node/pnpm 和前端构建；Python 依赖仍需安装。

=== "一键安装脚本"

    适用于 Linux / macOS / WSL2：

    ```bash
    curl -fsSL https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh | bash
    ```

    脚本会检测环境，在需要时准备 Python 和 `uv`，安装 Runtime 依赖并准备 Dashboard。已有源码时进入 `echo-agent-runtime/` 运行 `bash scripts/install.sh --source .`，不会克隆或更新 Git；预构建资源存在时不需要 Node。

    !!! note "脚本行为"
        - 检测 Python 3.11+，按平台提供安装或选择流程
        - 默认将源码安装到 `~/.echo-agent/echo-agent`，虚拟环境在其 `venv/` 下；可用 `ECHO_INSTALL_DIR` 改写
        - 远程安装只检出 Runtime；本地源码和解压目录使用 `--source PATH`
        - 使用 `uv` 将 Runtime 安装到虚拟环境；选用的 extra 取决于安装流程
        - 默认构建并检查 Dashboard；`--skip-dashboard` 明确跳过，之后可手动构建
        - 将 `echo-agent` 命令链接到 `~/.local/bin`

---

## 可选依赖（extras）

| Extra | 说明 | 包含的关键依赖 |
|-------|------|----------------|
| `openai` | OpenAI / 兼容端点 | openai, httpx[socks] |
| `anthropic` | Anthropic Claude | anthropic, httpx[socks] |
| `bedrock` | AWS Bedrock | anthropic, boto3 |
| `gemini` | Google Gemini | google-genai |
| `allproviders` | 所有模型提供商 | 以上全部 |
| `vector` | 向量检索 | faiss-cpu |
| `browser` | 浏览器自动化 | playwright |
| `weixin` | 微信通道 | cryptography, pilk |
| `container` | 容器沙箱 | docker |
| `documents` | 文档解析 | pymupdf, python-docx, openpyxl |
| `tui` | 终端 UI | textual |
| `tokenizers` | Token 计数 | tiktoken |
| `otel` | OpenTelemetry 追踪 | opentelemetry-* |
| `skills` | 内置技能依赖 | duckduckgo_search, trafilatura 等 |
| `all` | 包内预列的常用可选依赖集合 | 具体列表以 `pyproject.toml` 为准，不自动包含每个独立 extra |

---

## Windows 原生注意事项

!!! warning "Windows 原生限制"
    Windows 原生安装存在以下已知限制：

    - 部分二进制依赖在 Windows 原生环境下可能缺少可用的 wheel，应先验证目标 Python 版本与平台
    - 信号处理（graceful shutdown）行为与 Unix 不同
    - 部分技能依赖的命令行工具（如 `tesseract`）需单独安装
    - 建议优先使用 WSL2

    Windows 原生安装步骤：

    ```powershell
    # 确保 Python 3.11+ 已安装
    python --version

    # 最小安装（不含 faiss-cpu）
    pip install "echo-agent[openai,anthropic]"

    # 其他功能按所需 extra 单独安装并验证
    ```

!!! note "faiss 与 fastembed 是两件事"
    `faiss-cpu` 属于 `[vector]` 与 `[all]` 分组，可以通过挑选 extra 来跳过；缺少它时向量检索降级为关键词检索，功能不中断。

    `fastembed` 则是**核心依赖**，任何安装组合都会带上它，无法通过 extra 规避。它依赖 ONNX Runtime，在部分平台需要编译。若安装受阻，推荐在 Windows 上改用 WSL2——本项目的常驻服务注册也只支持 Linux / macOS / WSL2。

---

## 前端 Dashboard 构建

正式发布流程构建的 wheel 包含 Dashboard 产物；是否安装 `[all]` extra 不决定前端产物是否随包提供。如果从源码安装并需要 Dashboard：

```bash
# 安装 Node.js 依赖
cd echo-agent-runtime/web
pnpm install --frozen-lockfile

# 构建前端
pnpm build

# 构建产物在 web/dist，echo-agent 启动时会自动加载
```

!!! tip "跳过前端构建"
    如果不需要 Web Dashboard，可以跳过此步骤。Echo Agent 的核心功能不依赖前端。
    通过 `echo-agent gateway` 启动 Gateway 时会自动检测并加载 `web/dist`。

---

## Playwright 浏览器依赖

如果你需要使用浏览器自动化相关技能：

```bash
# 安装 playwright 浏览器
playwright install chromium

# 或安装所有浏览器
playwright install

# 安装系统依赖（Linux）
playwright install-deps chromium
```

!!! note "按需安装"
    浏览器依赖仅在使用 `browser` 相关技能时需要，不影响 Agent 核心运行。

---

## 验证安装

```bash
# 检查版本
echo-agent --version
# 输出: echo-agent 0.3.8

# 检查运行状态
echo-agent status

# 运行安装检查
echo-agent deps status
```

`echo-agent deps status` 会检查依赖管理器登记的可选功能，并报告缺失项；未登记的其他 extra 仍需按 `pyproject.toml` 自行确认。

!!! tip "安装成功标志"
    看到版本号输出即表示安装成功。接下来请阅读 [快速上手](quickstart.md) 完成首次配置。
