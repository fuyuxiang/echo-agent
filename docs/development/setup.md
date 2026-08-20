# 开发环境搭建

## 系统要求

| 组件 | 最低版本 | 说明 |
|------|----------|------|
| Python | 3.11 | 支持 3.11、3.12 |
| Node.js | 24 | Dashboard 前端构建 |
| pnpm | 10 | 前端包管理器（package.json 锁定 10.34.5） |
| Git | 2.30+ | 版本控制 |

## 后端环境

### 1. 克隆仓库

```bash
git clone https://github.com/fuyuxiang/echo-agent.git
cd echo-agent
```

### 2. 创建虚拟环境（推荐）

```bash
python -m venv .venv
# Linux/macOS
source .venv/bin/activate
# Windows
.venv\Scripts\activate
```

### 3. 安装依赖

```bash
# 安装全部可选依赖 + 开发工具
pip install -e ".[all,dev]"
```

各 extras 说明：

| Extra | 用途 |
|-------|------|
| `all` | 全部运行时可选依赖（所有 Provider、向量存储、TUI 等） |
| `dev` | ruff（锁定 0.15.12）、pytest、pytest-asyncio、pytest-cov |
| `docs` | mkdocs-material、mkdocs-static-i18n |
| `openai` | 仅 OpenAI Provider |
| `anthropic` | 仅 Anthropic Provider |
| `bedrock` | AWS Bedrock Provider |
| `gemini` | Google Gemini Provider |
| `browser` | Playwright 浏览器工具 |
| `tui` | Textual 终端 UI |
| `skills` | 内置 Skill 依赖集合 |

### 4. 验证安装

```bash
# 代码检查
ruff check .

# 运行测试
python -m pytest tests/ -v --cov

# 启动 Agent（需配置模型）
echo-agent --help
```

## 前端环境

### 1. 安装 pnpm

```bash
# 使用 corepack（Node.js 内置）
corepack enable
corepack prepare pnpm@10.34.5 --activate

# 或使用 npm
npm install -g pnpm@10.34.5
```

### 2. 安装前端依赖

```bash
cd web
pnpm install --frozen-lockfile
```

### 3. 开发模式

```bash
pnpm dev       # 启动 Vite 开发服务器（默认 http://localhost:5173）
pnpm build     # 生产构建（输出到 web/dist/）
pnpm test --run  # 运行 Vitest 测试
```

## IDE 配置

### VS Code 推荐扩展

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

### VS Code 设置

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

## 环境变量

开发时常用的环境变量：

```bash
# 模型 API Key（按需配置）
export OPENAI_API_KEY="sk-..."
export ANTHROPIC_API_KEY="sk-ant-..."
export GOOGLE_API_KEY="..."

# 可选：AWS Bedrock
export AWS_ACCESS_KEY_ID="..."
export AWS_SECRET_ACCESS_KEY="..."
export AWS_REGION="us-east-1"
```

仓库不提供 `.env.example` 模板：凭证的常规入口是 `echo-agent setup` 向导与配置文件，环境变量只作为覆盖手段。全部可用变量见[环境变量参考](../reference/environment-variables.md)。

## 常见问题

### fastembed 安装失败

fastembed 依赖 ONNX Runtime，在某些平台需要编译。如果只做不涉及向量存储的开发：

```bash
pip install -e ".[dev]"  # 不含 fastembed 的 all 集合
```

### Playwright 浏览器未安装

```bash
pip install -e ".[browser]"
python -m playwright install chromium
```

### ruff 版本不匹配

项目锁定 ruff==0.15.12，CI 使用相同版本。请勿自行升级：

```bash
pip install -e ".[dev]"  # 自动安装锁定版本
```
