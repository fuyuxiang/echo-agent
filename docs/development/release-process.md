# 发布流程

Runtime 的以下命令均在 `echo-agent-runtime/` 内执行；桌面端继续使用 `client/` 的独立发布流程。

## 准备

- 更新 Runtime 的 `CHANGELOG.md` 和 `pyproject.toml` 版本。
- 检查中英文文档、模型与存储兼容性说明。
- 确认 Python 测试、Dashboard 测试、文档和打包 CI 通过。

## 构建和验收

```bash
python -m pip install build
bash scripts/build.sh
```

该命令不发布。它从锁文件构建 Dashboard，运行 sdist → wheel 构建，生成独立 Runtime 压缩包，并验证三个产物中的资源：

- `dist/echo_agent-<version>-py3-none-any.whl`：已打包 Runtime、Dashboard、技能和配置。
- `dist/echo_agent-<version>.tar.gz`：Python sdist，含预构建 Dashboard、完整前端源码和部署模板。
- `dist/echo-agent-runtime-<version>.tar.gz`：解压为 `echo-agent-runtime/`，含源码及预构建 `web/dist/`，可直接在服务器安装，无需 Node/pnpm。

已有可信前端产物时可运行 `bash scripts/build.sh --prebuilt-dashboard`。缺少前端产物会失败，不能用 `ECHO_ALLOW_NO_DASHBOARD` 发布。

CI 将 Runtime 单独复制到仓库外构建；在独立环境安装 wheel 和解压的 Runtime 源码包，检查资源、版本、CLI 和真实 Gateway 进程。发布前还需在目标服务器环境验证所用模型、通道及外部工具。

## 发布

```bash
python -m pip install twine
bash scripts/publish.sh
```

脚本构建并验证后只向 PyPI 上传 wheel/sdist。独立 Runtime 压缩包作为单独下载附件发布，不上传到 PyPI。按版本策略创建 Git tag，并更新下载入口。

## 文档站

文档站位于仓库根目录 `docs/`，配置为根目录 `mkdocs.yml`；安装文档依赖时使用 `pip install -e "./echo-agent-runtime[docs]"`，随后从仓库根目录运行文档生成和 MkDocs。线上文档随 master 更新，不做多版本化。

涉及版本或升级行为时同步更新 `docs/index.md`、英文版及 `docs/operations/upgrade-migrations.md`、英文版。
