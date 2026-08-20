# Installation Guide

## System Requirements

| Item | Minimum | Recommended |
|------|---------|-------------|
| Python | 3.11 | 3.12 |
| OS | Linux / macOS / Windows | Ubuntu 22.04+ / macOS 13+ |
| RAM | 512 MB | 2 GB+ |
| Disk | 200 MB | 1 GB+ (with vector indices) |

**OS Support Matrix:**

| Operating System | Status | Notes |
|-----------------|--------|-------|
| Ubuntu 22.04+ | :white_check_mark: Fully supported | Recommended for production |
| Debian 12+ | :white_check_mark: Fully supported | |
| macOS 13+ (ARM/x86) | :white_check_mark: Fully supported | |
| Windows + WSL2 | :white_check_mark: Fully supported | Recommended for Windows users |
| Native Windows | :warning: Basic support | Some limitations, see below |
| Alpine Linux | :warning: Basic support | Manual build deps required |

---

## Installation Methods

=== "pip (Recommended)"

    Install the full version (all model providers and features):

    ```bash
    pip install echo-agent[all]
    ```

    Or install core + specific providers only:

    ```bash
    # Minimal install (core runtime only)
    pip install echo-agent

    # Add model providers as needed
    pip install echo-agent[openai]
    pip install echo-agent[anthropic]
    pip install echo-agent[gemini]
    pip install echo-agent[bedrock]

    # Combined install
    pip install echo-agent[openai,anthropic,vector,browser]
    ```

=== "From Source"

    ```bash
    git clone https://github.com/fuyuxiang/echo-agent.git
    cd echo-agent
    pip install -e ".[all]"
    ```

    For development, also install dev dependencies:

    ```bash
    pip install -e ".[all,dev]"
    ```

=== "One-line Install Script"

    For Linux / macOS / WSL2:

    ```bash
    curl -fsSL https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh | bash
    ```

    The script automatically detects your system, installs Python if needed, and installs echo-agent[all] via pip.

    !!! note "Script Behavior"
        - Detects and installs Python 3.11+ (via system package manager)
        - Creates a virtual environment at `~/.echo-agent/venv`
        - Installs `echo-agent[all]` into the virtual environment
        - Symlinks the `echo-agent` command to `~/.local/bin`

---

## Optional Dependencies (extras)

| Extra | Purpose | Key Packages |
|-------|---------|--------------|
| `openai` | OpenAI / compatible endpoints | openai, httpx[socks] |
| `anthropic` | Anthropic Claude | anthropic, httpx[socks] |
| `bedrock` | AWS Bedrock | anthropic, boto3 |
| `gemini` | Google Gemini | google-generativeai |
| `allproviders` | All model providers | All of the above |
| `vector` | Vector search | faiss-cpu |
| `browser` | Browser automation | playwright |
| `weixin` | WeChat channel | cryptography, pilk |
| `container` | Container sandbox | docker |
| `documents` | Document parsing | pymupdf, python-docx, openpyxl |
| `tui` | Terminal UI | textual |
| `tokenizers` | Token counting | tiktoken |
| `otel` | OpenTelemetry tracing | opentelemetry-* |
| `skills` | Built-in skill deps | duckduckgo_search, trafilatura, etc. |
| `all` | Full install | Everything above |

---

## Native Windows Notes

!!! warning "Native Windows Limitations"
    Native Windows installation has the following known limitations:

    - `faiss-cpu` does not provide official Windows wheels; use conda or unofficial sources
    - Signal handling (graceful shutdown) behaves differently from Unix
    - Some skill dependencies (e.g., `tesseract`) require separate installation
    - WSL2 is strongly recommended instead

    Native Windows installation:

    ```powershell
    # Ensure Python 3.11+ is installed
    python --version

    # Minimal install (no faiss-cpu)
    pip install echo-agent[openai,anthropic]

    # Full install
    pip install echo-agent[all]
    ```

!!! note "faiss and fastembed are different things"
    `faiss-cpu` belongs to the `[vector]` and `[all]` extras, so it can be skipped by choosing extras; without it, vector retrieval degrades to keyword search rather than failing.

    `fastembed`, by contrast, is a **core dependency**: every installation pulls it in and no choice of extras avoids it. It depends on ONNX Runtime, which needs compiling on some platforms. If the install stalls there, WSL2 is the recommended route on Windows — resident-service registration is likewise limited to Linux / macOS / WSL2.

---

## Frontend Dashboard Build

The built-in Dashboard is pre-packaged in the `echo-agent[all]` wheel. If you installed from source and need the Dashboard:

```bash
# Install Node.js dependencies
cd web
pnpm install

# Build frontend
pnpm build

# Output goes to web/dist, auto-loaded by echo-agent at startup
```

!!! tip "Skip Frontend Build"
    If you don't need the Web Dashboard, skip this step. Echo Agent's core functionality does not depend on the frontend.
    The `echo-agent gateway` command auto-detects and serves `web/dist` when available.

---

## Playwright Browser Dependencies

If you need browser automation skills:

```bash
# Install Playwright browsers
playwright install chromium

# Or install all browsers
playwright install

# Install system dependencies (Linux)
playwright install-deps chromium
```

!!! note "Install on Demand"
    Browser dependencies are only needed for `browser`-related skills and do not affect core Agent operation.

---

## Verify Installation

```bash
# Check version
echo-agent --version
# Output: echo-agent 0.3.7

# Check status
echo-agent status

# Run dependency check
echo-agent deps
```

`echo-agent deps` checks all optional dependencies and reports any missing items.

!!! tip "Success Indicator"
    Seeing the version number confirms a successful installation. Next, read the [Quickstart](quickstart.en.md) to complete initial configuration.
