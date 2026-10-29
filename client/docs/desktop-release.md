# 桌面发行流程

支持的发行目标为 Windows x86_64、macOS Apple Silicon 与 macOS Intel。Linux 目前只做源码与原生模块验证，尚无正式安装包。

## 构建与验收

1. 在三种目标机器或对应的原生 CI runner 上，使用 Node.js 22 或 24、pnpm 10 和项目要求的 Rust、protoc 及平台工具链安装依赖。使用 `pnpm install --frozen-lockfile`。
2. 在 Windows x64 上运行 `pnpm dist:win`；在两种 macOS 架构上分别运行 `pnpm dist:mac`。正式发布须配置平台签名，macOS 构建还必须通过 Gatekeeper 检查。`-AllowUnsignedPlatform` 和 `--allow-unsigned-platform` 只用于 CI 与本地验证。
3. `desktop-validation` 工作流会在三个原生 runner 上调用同一套发行构建脚本，安装或挂载安装包，并验证 WebView、原生 IPC 和包内 Theia IDE 启动。正式发布前检查对应版本的三个作业均通过。
4. 在受控 macOS 发行机运行 `scripts/prepare-update-artifacts.sh`，传入一个 Windows 安装包和两个 DMG。脚本核对文件名与包内版本、架构、平台签名，然后生成并验证三个 updater 签名。正式发布不要使用 `--unsigned` 或 `--allow-unsigned-platform`。

Windows 打包脚本的快速回归检查为 `pnpm test:windows-packaging`，无需编译安装包。CI 在 Windows PowerShell 5.1 和 PowerShell 7 下分别检查原生命令警告、失败退出码、版本输出与含方括号的项目路径。脚本在构建失败时停止，并恢复调用前的工作目录；原生命令仅输出警告且退出码为零时继续。

## 更新服务升级

更新服务须安装本仓库当前版本的 `deploy/update-server/publish-update.py` 和 `deploy/update-server/nginx-location.conf`。按 `deploy/update-server/install.sh` 的要求上传两个文件并执行安装脚本。安装脚本保留旧的单平台清单，并将其复制到初始清单目录；Nginx 随后通过 `stable/current` 读取清单。升级完成后，应核对三个现有更新 URL 与升级前结果一致，再发布新版本。

`stable/current` 是指向一个清单目录的符号链接。发布程序先复制并核对三个目标的更新文件，生成完整的新清单目录，再原子切换这个链接。中途失败时，现有三个清单保持原值；旧目录也保留，便于诊断和回滚。不要直接改写 `stable/current` 指向的文件。

## 发布

先运行 `bash scripts/publish-all-updates.sh --artifacts-dir release/v<版本> --dry-run` 进行本地预检，再去掉 `--dry-run` 发布。脚本上传三个更新文件及签名，由服务端一次性切换三个清单。发布后分别读取三个 `stable/<target>.json` URL，核对版本、签名和下载地址，并下载文件与 `artifacts.json`、`SHA256SUMS` 对照。

若发布后需要回滚，保留旧发行文件并将 `stable/current` 原子切回上一清单目录；先确认旧文件仍可下载。回滚操作应由发行负责人执行，并记录切换前后的目录名称。
