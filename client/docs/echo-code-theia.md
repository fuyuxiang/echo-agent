# Echo Code 与 Theia 集成

「更多 → 代码开发」使用仓库内 Eclipse Theia 1.74.0 的浏览器 IDE。Theia 提供文件树、Monaco 编辑器、搜索、终端、Git 和网页预览；旁边的 EchoAgent 面板负责 Agent 会话、任务阶段、变更审阅、验证记录与交付。

## 构建与打包

- 开发机器使用 Node.js 22 或 24。`pnpm ide:build` 编译 [`vendor/theia-platform/`](../vendor/theia-platform/)，`pnpm ide:stage` 将 Theia 后端、生产依赖和当前平台的 Node.js 放入 Tauri 资源目录。
- `pnpm tauri dev` 和 `pnpm tauri build` 的前置钩子会运行 `pnpm ide:prepare`；缺少资源、架构变化或源码更新时重新构建和暂存。安装包中的 IDE 使用随包分发的 Node.js，普通用户不需另装。
- `pnpm ide:build` 会下载固定版本的基础语言扩展，`pnpm ide:stage` 将它们与仓库内的轻量 TOML 语法扩展一起放入安装包。运行时从本地 `plugins/` 加载，语法高亮不依赖用户联网或手动安装扩展。默认覆盖 JavaScript/JSX、TypeScript/TSX、Rust、Python、Java、JSON、YAML、TOML、HTML、CSS、Markdown 和 Shell Script。TOML 扩展只提供语法功能，避免为基础高亮启动额外语言服务。
- `pnpm ide:build` 和 `pnpm ide:stage` 会给固定版本 `node-pty` 的 Windows ConPTY 清理 helper 加 `windowsHide: true`，保持交互 shell 的上游 ConPTY 创建参数，并处理终端管道在连接前后发生的错误。构建会将旧版加过 `CREATE_NO_WINDOW` 的源码恢复到固定版本的上游参数，再重建原生模块并同步到 `prebuilds/`。暂存缓存和运行时验证会检查源码与二进制；Windows 打包还会实际启动、使用并关闭一个 ConPTY 终端。升级 `node-pty` 时须先复核补丁，构建会在版本或调用点变化时失败。
- 如启动失败，先看应用数据目录中的 `theia.log`。开发者可以用 [`smoke-theia.mjs`](../scripts/smoke-theia.mjs) 检查内嵌页面与桥接。

## 运行与权限边界

Tauri 的 `coding_theia_start` 先检查选定目录是否为 EchoAgent 已授权工作区，再在 `127.0.0.1` 启动 Theia 后端。进程使用启动时生成的令牌验证就绪状态；切换项目时重启后端，关闭应用时停止进程。React 宿主与 Theia iframe 的消息桥校验来源和每帧令牌，文件写入前检查任务阶段，写入后同步变更集。

Windows 后端使用 `CREATE_NO_WINDOW` 启动 Node，并在进程恢复运行前放入独立 Job，避免闪出控制台窗口并确保早期子进程也被托管。应用级 Job 在客户端异常退出时清理未单独托管的子进程；每次托管启动另有独立 Job，在 IDE 关闭或切换项目时定向清理旧进程树。应用重启和更新安装器启动前短暂允许新进程脱离应用级 Job，使它们能在旧客户端退出后继续运行。

Theia 连接断开时，宿主重新验证本地后端并恢复 IDE。若编辑器有未保存文件，或无法确认保存状态，先保留当前页面供复制内容，等待用户手动重新连接。后端重启会保留现有 `theia.log` 内容；超过 5 MiB 后将旧日志保存在 `theia.previous.log`。

Theia 的文件操作和交互终端以当前系统用户身份运行。授权工作区是工作台范围，不是操作系统沙箱；打开来源未知的项目时仍应检查终端命令与扩展。Windows 启动路径和工作区桥接会处理中文及长路径兼容。

关键实现见 [`theia.rs`](../src-tauri/src/theia.rs)、[`TheiaIdeFrame.tsx`](../src/features/coding/TheiaIdeFrame.tsx) 和 [`prepare-theia.mjs`](../scripts/prepare-theia.mjs)。
