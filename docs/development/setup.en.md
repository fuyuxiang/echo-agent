# Development Setup

## System Requirements

| Component | Minimum Version | Notes |
|-----------|----------------|-------|
| Python | 3.11 | Supports 3.11, 3.12 |
| Node.js | 24 | Dashboard frontend build |
| pnpm | 10 | Frontend package manager (pinned to 10.34.5 in package.json) |
| Git | 2.30+ | Version control |

## Backend Environment

### 1. Clone the Repository

```bash
git clone https://github.com/fuyuxiang/echo-agent.git
cd echo-agent
```

### 2. Create a Virtual Environment (Recommended)

```bash
python -m venv .venv
# Linux/macOS
source .venv/bin/activate
# Windows
.venv\Scripts\activate
```

### 3. Install Dependencies

```bash
# Install all optional dependencies + dev tools
pip install -e ".[all,dev]"
```

Available extras:

| Extra | Purpose |
|-------|---------|
| `all` | All runtime optional deps (all Providers, vector store, TUI, etc.) |
| `dev` | ruff (pinned 0.15.12), pytest, pytest-asyncio, pytest-cov |
| `docs` | mkdocs-material, mkdocs-static-i18n |
| `openai` | OpenAI Provider only |
| `anthropic` | Anthropic Provider only |
| `bedrock` | AWS Bedrock Provider |
| `gemini` | Google Gemini Provider |
| `browser` | Playwright browser tool |
| `tui` | Textual terminal UI |
| `skills` | Built-in Skill dependencies |

### 4. Verify Installation

```bash
# Lint check
ruff check .

# Run tests
python -m pytest tests/ -v --cov

# Start the Agent (requires model configuration)
echo-agent --help
```

## Frontend Environment

### 1. Install pnpm

```bash
# Using corepack (built into Node.js)
corepack enable
corepack prepare pnpm@10.34.5 --activate

# Or via npm
npm install -g pnpm@10.34.5
```

### 2. Install Frontend Dependencies

```bash
cd web
pnpm install --frozen-lockfile
```

### 3. Development Mode

```bash
pnpm dev        # Start Vite dev server (default http://localhost:5173)
pnpm build      # Production build (output to web/dist/)
pnpm test --run # Run Vitest tests
```

## IDE Configuration

### VS Code Recommended Extensions

```json
{
  "recommendations": [
    "charliermarsh.ruff",
    "ms-python.python",
    "bradlc.vscode-tailwindcss",
    "dbaeumer.vscode-eslint"
  ]
}
```

### VS Code Settings

```json
{
  "python.defaultInterpreterPath": ".venv/bin/python",
  "[python]": {
    "editor.defaultFormatter": "charliermarsh.ruff",
    "editor.formatOnSave": true
  },
  "ruff.lineLength": 120
}
```

## Environment Variables

Common environment variables for development:

```bash
# Model API Keys (configure as needed)
export OPENAI_API_KEY="sk-..."
export ANTHROPIC_API_KEY="sk-ant-..."
export GOOGLE_API_KEY="..."

# Optional: AWS Bedrock
export AWS_ACCESS_KEY_ID="..."
export AWS_SECRET_ACCESS_KEY="..."
export AWS_REGION="us-east-1"
```

The repository ships no `.env.example` template: credentials normally go through the `echo-agent setup` wizard and the configuration file, with environment variables acting as overrides. For the full list, see the [environment variables reference](../reference/environment-variables.md).

## Troubleshooting

### fastembed installation failure

fastembed depends on ONNX Runtime, which may require compilation on some platforms. If your work doesn't involve vector storage:

```bash
pip install -e ".[dev]"  # Without the full 'all' set that includes fastembed
```

### Playwright browsers not installed

```bash
pip install -e ".[browser]"
python -m playwright install chromium
```

### ruff version mismatch

The project pins ruff==0.15.12 and CI uses the same version. Do not upgrade independently:

```bash
pip install -e ".[dev]"  # Automatically installs the pinned version
```
