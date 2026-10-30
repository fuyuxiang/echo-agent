# Installation Guide

## System Requirements

| Item | Minimum | Recommended |
|------|---------|-------------|
| Python | 3.11 | 3.12 |
| OS | See below | CI runs on Ubuntu |
| RAM | Depends on models and workload | Size through deployment testing |
| Disk | Depends on optional features and data | Plan for deployment volume |

**OS Support Matrix:**

| Operating System | Status | Notes |
|-----------------|--------|-------|
| Ubuntu | Covered by CI | Tests run with Python 3.11 and 3.12 |
| Debian / macOS / Windows with WSL2 | Verify in deployment | Meets installation constraints but is outside the current CI OS matrix |
| Native Windows / Alpine Linux | Verify each feature | External commands, process tools, or binary dependencies may differ |

---

## Installation Methods

=== "pip (Recommended)"

    Install the explicitly listed set of common optional packages (see `pyproject.toml` for each extra's exact scope):

    ```bash
    pip install "echo-agent[all]"
    ```

    Or install core + specific providers only:

    ```bash
    # Minimal install (core runtime only)
    pip install echo-agent

    # Add model providers as needed
    pip install "echo-agent[openai]"
    pip install "echo-agent[anthropic]"
    pip install "echo-agent[gemini]"
    pip install "echo-agent[bedrock]"

    # Combined install
    pip install "echo-agent[openai,anthropic,vector,browser]"
    ```

=== "From Source"

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

    For development, also install dev dependencies:

    ```bash
    pip install -e ".[all,dev]"
    ```

    This directory includes the complete Dashboard sources and lockfile. A separately downloaded `echo-agent-runtime/` needs no Git. Official `echo-agent-runtime-<version>.tar.gz` archives include prebuilt frontend assets: skip Node/pnpm and the frontend build, but install Python dependencies.

=== "One-line Install Script"

    For Linux / macOS / WSL2:

    ```bash
    curl -fsSL https://raw.githubusercontent.com/fuyuxiang/echo-agent/master/scripts/install.sh | bash
    ```

    The script prepares dependencies and the Dashboard. With local sources, enter `echo-agent-runtime/` and run `bash scripts/install.sh --source .` without cloning or Git updates. Remote installs use a sparse Runtime checkout. Prebuilt archives need no Node.

    !!! note "Script Behavior"
        - Checks for Python 3.11+ and follows platform-specific installation or selection steps
        - Installs source to `~/.echo-agent/echo-agent` by default, with `venv/` inside it; `ECHO_INSTALL_DIR` can change this
        - Uses `uv` to install Runtime into the virtual environment; selected extras depend on the installation flow
        - Prepares and checks the Dashboard by default; `--skip-dashboard` explicitly defers this step
        - Symlinks the `echo-agent` command to `~/.local/bin`

---

## Optional Dependencies (extras)

| Extra | Purpose | Key Packages |
|-------|---------|--------------|
| `openai` | OpenAI / compatible endpoints | openai, httpx[socks] |
| `anthropic` | Anthropic Claude | anthropic, httpx[socks] |
| `bedrock` | AWS Bedrock | anthropic, boto3 |
| `gemini` | Google Gemini | google-genai |
| `allproviders` | All model providers | All of the above |
| `vector` | Vector search | faiss-cpu |
| `browser` | Browser automation | playwright |
| `weixin` | WeChat channel | cryptography, pilk |
| `container` | Container sandbox | docker |
| `documents` | Document parsing | pymupdf, python-docx, openpyxl |
| `tui` | Terminal UI | textual |
| `tokenizers` | Token counting | tiktoken |
| `otel` | OpenTelemetry tracing | opentelemetry-* |
| `skills` | Built-in skill deps | ddgs, trafilatura, etc. |
| `all` | Explicitly listed common optional packages | See `pyproject.toml`; it does not automatically include every separate extra |

---

## Native Windows Notes

!!! warning "Native Windows Limitations"
    Native Windows installation has the following known limitations:

    - Some binary dependencies may lack a usable wheel on native Windows; verify the target Python version and platform first
    - Signal handling (graceful shutdown) behaves differently from Unix
    - Some skill dependencies (e.g., `tesseract`) require separate installation
    - WSL2 is strongly recommended instead

    Native Windows installation:

    ```powershell
    # Ensure Python 3.11+ is installed
    python --version

    # Minimal install (no faiss-cpu)
    pip install "echo-agent[openai,anthropic]"

    # Add and verify other extras only as needed
    ```

!!! note "faiss and fastembed are different things"
    `faiss-cpu` belongs to the `[vector]` and `[all]` extras, so it can be skipped by choosing extras; without it, vector retrieval degrades to keyword search rather than failing.

    FastEmbed is a **bundled embedding component** whose ONNX Runtime and related dependencies are declared by the core installation. Selecting extras cannot avoid them. Platforms without binary wheels may require compilation. If the install stalls there, WSL2 is the recommended route on Windows — resident-service registration is likewise limited to Linux / macOS / WSL2.

---

## Frontend Dashboard Build

Wheels built by the release process contain Dashboard assets; installing the `[all]` extra does not determine whether frontend assets are bundled. If you installed from source and need the Dashboard:

```bash
# Install Node.js dependencies
cd echo-agent-runtime/web
pnpm install --frozen-lockfile

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
# Output: echo-agent 0.3.8

# Check status
echo-agent status

# Run dependency check
echo-agent deps status
```

`echo-agent deps status` checks optional features registered with the dependency manager and reports missing packages. Check other extras against `pyproject.toml` separately.

!!! tip "Success Indicator"
    Seeing the version number confirms a successful installation. Next, read the [Quickstart](quickstart.en.md) to complete initial configuration.
