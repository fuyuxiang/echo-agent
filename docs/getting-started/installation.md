# 安装指南

## 系统要求

| 项目 | 最低要求 | 推荐 |
|------|----------|------|
| Python | 3.11 | 3.12 |
| 操作系统 | Linux / macOS / Windows | Ubuntu 22.04+ / macOS 13+ |
| 内存 | 512 MB | 2 GB+ |
| 磁盘 | 200 MB | 1 GB+（含向量索引） |

**操作系统支持矩阵：**

| 操作系统 | 状态 | 说明 |
|----------|------|------|
| Ubuntu 22.04+ | :white_check_mark: 完全支持 | 推荐生产环境 |
| Debian 12+ | :white_check_mark: 完全支持 | |
| macOS 13+ (ARM/x86) | :white_check_mark: 完全支持 | |
| Windows + WSL2 | :white_check_mark: 完全支持 | 推荐 Windows 用户使用 |
| Windows 原生 | :warning: 基本支持 | 部分功能受限，见下文 |
| Alpine Linux | :warning: 基本支持 | 需手动安装编译依赖 |

---

## 安装方式

=== "pip（推荐）"

    安装完整版（包含所有模型提供商和功能）：

    ```bash
    pip install echo-agent[all]
    ```

    或仅安装核心 + 指定提供商：

    ```bash
    # 最小安装（仅核心运行时）
    pip install echo-agent

    # 按需添加模型提供商
    pip install echo-agent[openai]
    pip install echo-agent[anthropic]
    pip install echo-agent[gemini]
    pip install echo-agent[bedrock]

    # 组合安装
    pip install echo-agent[openai,anthropic,vector,browser]
    ```

=== "源码安装"

    ```bash
    git clone https://github.com/fuyuxiang/echo-agent.git
    cd echo-agent
    pip install -e ".[all]"
    ```

    开发模式额外安装开发依赖：

    ```bash
    pip install -e ".[all,dev]"
    ```

=== "一键安装脚本"

    适用于 Linux / macOS / WSL2：

    ```bash
    curl -fsSL https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh | bash
    ```

    脚本会自动检测系统环境，安装 Python（如需要）并通过 pip 安装 echo-agent[all]。

    !!! note "脚本行为"
        - 检测并安装 Python 3.11+（通过系统包管理器）
        - 创建虚拟环境 `~/.echo-agent/venv`
        - 安装 `echo-agent[all]` 到虚拟环境
        - 将 `echo-agent` 命令链接到 `~/.local/bin`

---

## 可选依赖（extras）

| Extra | 说明 | 包含的关键依赖 |
|-------|------|----------------|
| `openai` | OpenAI / 兼容端点 | openai, httpx[socks] |
| `anthropic` | Anthropic Claude | anthropic, httpx[socks] |
| `bedrock` | AWS Bedrock | anthropic, boto3 |
| `gemini` | Google Gemini | google-generativeai |
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
| `all` | 完整安装 | 以上全部 |

---

## Windows 原生注意事项

!!! warning "Windows 原生限制"
    Windows 原生安装存在以下已知限制：

    - `faiss-cpu` 不提供 Windows 官方 wheel，需从非官方源安装或使用 conda
    - 信号处理（graceful shutdown）行为与 Unix 不同
    - 部分技能依赖的命令行工具（如 `tesseract`）需单独安装
    - 建议优先使用 WSL2

    Windows 原生安装步骤：

    ```powershell
    # 确保 Python 3.11+ 已安装
    python --version

    # 最小安装（不含 faiss-cpu）
    pip install echo-agent[openai,anthropic]

    # 完整安装
    pip install echo-agent[all]
    ```

!!! note "faiss 与 fastembed 是两件事"
    `faiss-cpu` 属于 `[vector]` 与 `[all]` 分组，可以通过挑选 extra 来跳过；缺少它时向量检索降级为关键词检索，功能不中断。

    `fastembed` 则是**核心依赖**，任何安装组合都会带上它，无法通过 extra 规避。它依赖 ONNX Runtime，在部分平台需要编译。若安装受阻，推荐在 Windows 上改用 WSL2——本项目的常驻服务注册也只支持 Linux / macOS / WSL2。

---

## 前端 Dashboard 构建

内置 Dashboard 已打包在 `echo-agent[all]` 的 wheel 中。如果你从源码安装并需要 Dashboard：

```bash
# 安装 Node.js 依赖
cd web
pnpm install

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
echo-agent deps
```

`echo-agent deps` 会检查所有可选依赖的安装状态，并报告缺失项。

!!! tip "安装成功标志"
    看到版本号输出即表示安装成功。接下来请阅读 [快速上手](quickstart.md) 完成首次配置。
