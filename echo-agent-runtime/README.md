[中文](README.md) · [English](README.en.md)

# Echo Agent Runtime

可独立部署的 Python Agent Runtime，包含 CLI、Gateway、Dashboard、内置技能、配置资源和部署模板。只下载本目录即可从源码构建和安装，不依赖仓库中的 `client/`、`docs/` 或上级文件。Python 包名和命令继续使用 `echo-agent`，导入名为 `echo_agent`。

## 从源码安装

需要 Python 3.11+；构建 Dashboard 还需要 Node.js 22/24 和 pnpm（版本锁定在 `web/package.json`）。模型服务及部分技能所需的外部依赖按使用场景配置。

已下载整个仓库时先进入 `echo-agent-runtime/`；只下载了本目录时直接在其中执行：

```bash
python3 -m venv .venv
source .venv/bin/activate
(cd web && pnpm install --frozen-lockfile && pnpm build)
python -m pip install ".[openai]"
python -m pip check
echo-agent setup
echo-agent gateway
```

`[openai]` 支持 OpenAI 及兼容模型服务。可以换成 `[anthropic]`、`[gemini]`、`[bedrock]` 或 `[all]`。安装后的 Dashboard 位于 Gateway 的 `/`，简化交互页位于 `/playground`，默认地址是 `http://127.0.0.1:58123/`。模型、令牌和工具权限由配置控制。

开发时将安装命令换为 `python -m pip install -e ".[all,dev]"`。构建前端后启动 Gateway；`echo-agent dashboard build` 可重建源码中的 Dashboard。

Windows PowerShell 使用 `.venv\Scripts\Activate.ps1` 激活环境，并分别执行 `cd web`、`pnpm install --frozen-lockfile`、`pnpm build`、`cd ..`。后台服务注册支持 Linux/systemd 和 macOS/launchd；Windows 用户可使用 WSL2。

## 从 Runtime 发布压缩包安装

`echo-agent-runtime-<version>.tar.gz` 包含完整源码和预构建的 `web/dist/`。解压后进入 `echo-agent-runtime/`，执行上面的 Python 环境创建与安装步骤，**跳过前端构建**，服务器无需 Node/pnpm。该压缩包与 Git 源码压缩包不同，后者需要先构建 Dashboard。

wheel 和 Python sdist 同样内置 Dashboard 及技能。sdist 还保留完整前端源码、部署模板和测试，重新构建 wheel 无需 Node。普通 `pip install .` 会拒绝缺少 Dashboard 的源码；不要通过 `ECHO_ALLOW_NO_DASHBOARD` 制作部署包。

这不是离线依赖包：Python 依赖、首次使用的本地模型及外部工具仍需预先准备或联网获取。

## Linux / macOS / WSL2 安装脚本

```bash
# 使用当前目录的源码，不克隆、不更新 Git；默认创建当前目录的 venv/
bash scripts/install.sh --source .

# 明确将虚拟环境放到其他目录；源码需保留在原位置
bash scripts/install.sh --source . --dir /srv/echo-agent
```

脚本会安装依赖、准备 Dashboard，再进入配置和服务安装流程。已有有效模型配置会保留；`--reconfigure` 强制重新配置，`--skip-setup` 跳过配置向导。`--skip-dashboard` 明确跳过前端准备，之后运行 `echo-agent dashboard build`。详见 `bash scripts/install.sh --help`。

从网络单独下载安装脚本运行时，会获取 Runtime 的稀疏源码检出；不会下载桌面端源码。在 Runtime 源码目录直接运行脚本会自动选择本地源码。

## 服务器配置与数据

`deploy/echo-agent.yaml.example` 提供最小服务端配置。复制为自己的配置文件，填写模型、不同的 API/admin 随机令牌，并限制文件权限。模型密钥通过配置中的 `api_key_env` 从服务进程环境读取。

```bash
echo-agent config validate --config /absolute/path/echo-agent.yaml
echo-agent gateway --config /absolute/path/echo-agent.yaml --workspace /srv/echo-agent/workspace
```

先验证前台运行，再使用 `echo-agent gateway install` 注册后台服务。服务必须能读取同一配置、工作区和模型密钥环境。远程访问可使用 SSH 端口转发；反向代理部署需配置实际域名和 Host/Origin 规则。

源码与生产数据分开存放。默认工作区是 `~/.echo-agent/`；数据库、日志、用户技能、模型缓存和凭证不应放入源码发布包。`data/eval/` 是版本化评测样例，不是生产数据库目录。升级前备份实际工作区及显式配置的外部数据路径。

容器构建和运行模板见 [deploy/README.md](deploy/README.md)。

## 构建与验证

```bash
python -m pip install build
bash scripts/build.sh
```

产物在本目录 `dist/`：Python wheel、Python sdist，以及 `echo-agent-runtime-<version>.tar.gz`。脚本先构建前端，再构建 Python 包、生成独立源码包，并检查 Dashboard、技能、配置及第三方许可文件。已有可信前端产物时可用 `bash scripts/build.sh --prebuilt-dashboard`。

```bash
python -m pip install -e ".[all,dev]"
ruff check .
python -m pytest tests/
(cd web && pnpm test --run)
```

发布脚本 `scripts/publish.sh` 需要额外安装 `twine`，会明确上传 wheel/sdist 到 PyPI；单独构建不会发布。CI 会在仓库外构建 Runtime，并检查解压安装和真实 Gateway 进程。

完整文档：[中文文档站](https://fuyuxiang.github.io/echo-agent/)。版本记录：[CHANGELOG.md](CHANGELOG.md)。项目使用 [MIT License](LICENSE)；内置 FastEmbed 的来源及许可见 [VENDOR.md](echo_agent/_vendor/fastembed/VENDOR.md)。
