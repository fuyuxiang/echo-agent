# 开发与验收

测试结果需要绑定源码版本、平台和实际执行范围。本页维护可重复使用的入口与判定标准；单次运行日志、截图、覆盖率和评审过程放在临时目录或 CI artifacts 中。

## 环境准备

按 [README](../README.md#从源码运行) 安装工具链并运行 `pnpm install --frozen-lockfile`。完整桌面开发或打包会准备检索模型、Theia 和 Office Worker；手动运行相关独立测试前，先执行其准备步骤。

- Office 文件生成：`pnpm office:build`。
- IDE 与插件：`pnpm ide:prepare`；仅构建源码时用 `pnpm ide:build`，再用 `pnpm ide:stage` 暂存。
- 离线检索：`pnpm retrieval:prepare`，资源完整性检查为 `pnpm retrieval:check`。
- 浏览器验证复用已安装的 Playwright，或通过 `PLAYWRIGHT_MODULE` 指向独立工具目录的 `playwright/index.mjs`；`CHROME_BIN` 可指定浏览器。

## 回归层级

| 范围 | 常用入口 | 证明的范围 |
| --- | --- | --- |
| 前端与工具组件 | `pnpm test`、`pnpm test:coverage` | 状态、交互与可控并发；覆盖率不代表业务成功 |
| 脚本与隔离保护 | `pnpm test:scripts`、`pnpm test:native-guards` | 产物校验、进程隔离、证据资格及反例拒绝 |
| Rust 宿主 | `pnpm test:rust` | 独立数据目录中的宿主单元与集成测试 |
| Runtime 与记忆库 | `pnpm test:runtime:rust`、`pnpm test:memory:rust` | 依赖库自身的单测；宿主测试不能替代 |
| Runtime 启停 | `pnpm test:runtime-smoke`、`pnpm test:memory-shutdown` | 真实 Runtime、受控服务、取消和检查点恢复 |
| 原生控件模块 | `pnpm test:desktop-native` | 生产 AX/UIA 模块；不等于真实桌面输入 |
| 异常恢复 | `pnpm test:stability`、`pnpm test:stability:native` | 恢复协议，以及独立原生程序的窗口和进程故障注入 |
| UI 与主题 | `pnpm test:ui`；专题入口见 [体验约定](product-experience.md)、[主题设计](theme-design.md) | 生产组件的浏览器渲染和受控 IPC，不替代 OS 或真实服务 |
| 生产首帧 | `pnpm build`、`pnpm test:production-startup` | 实际 dist 的分块加载与挂载；静态 HTTP 中预期的原生 IPC 诊断另记 |
| Office 与文档编辑 | `pnpm office:test`、`pnpm test:document-workspace` | 实际文件、持久化、格式解析及导出 |
| IDE 与插件 | `pnpm test:ide`、`pnpm test:ide:unit` | 先准备对应插件和编译产物，再验桥接、SCM、RPC 及权限契约 |
| 依赖与发行 | `pnpm test:security`、`pnpm test:windows-packaging` | 安全补丁和打包流程回归；完整流程见 [依赖维护](dependency-security.md) 与 [发行说明](desktop-release.md) |

Rust 检查与格式门禁分别为 `cargo clippy --locked --manifest-path src-tauri/Cargo.toml --lib -- -D warnings` 和 `cargo fmt --manifest-path src-tauri/Cargo.toml --check`。新增独立 harness 时同步维护锁文件、安全审计覆盖及 CI。

## 原生应用与真实模型

原生驱动需要当前源码构建的内嵌页面程序。先准备资源和前端，再构建；普通开发 URL 二进制不能代替：

```sh
pnpm retrieval:prepare
pnpm ide:prepare
pnpm office:build
pnpm build
cargo build --locked --manifest-path src-tauri/Cargo.toml --features tauri/custom-protocol --bin echoagent -j 2
```

macOS 固定验收驱动接收该可执行文件路径；其平台、窗口、夹具和进程身份检查须保留：

| 场景 | 驱动（位于 `scripts/`） | 独立核验重点 |
| --- | --- | --- |
| 会话、取消、队列、审批、接续及重启 | `validate-native-core-review.mjs` | 当前回合事件、取消 ACK、审批调用身份、历史和成果文件 |
| Theia、Git、Diff 与终端 | `validate-native-ide-git-review.mjs` | 真实编辑器、临时无远端仓库、终端输出与未保存内容 |
| Ask / Plan / Agent 代码任务 | `validate-native-coding-workspace.mjs` | 缓冲区版本、实际模型请求、文件修改和有效验证 |
| Browser Use | `validate-native-browser-review.mjs` | 合成站点请求、下载字节、拒绝、暂停、清理及重启 |
| Computer Use | `validate-native-desktop-controls.mjs` | [桌面控制验收](desktop-control/validation.md) |
| 专家团与定时任务 | `validate-native-collaboration-review.mjs` | 多成员实际输出、协调汇总、真正到点触发与防重复 |
| MCP、技能、插件与连接器 | `validate-native-extensions-review.mjs` | 取消/允许、当前回合技能读取、真实服务调用及卸载 |
| 知识、手动记忆与引用 | `validate-native-knowledge-review.mjs` | 索引代次、来源撤销、引用路径、记忆更新及重启 |
| 后台记忆与整理 | `validate-native-memoryauto-review.mjs` | 普通对话触发、实时开关、待处理恢复及全新会话召回 |
| 强制退出和持续运行 | `validate-native-lifecycle-review.mjs` | 守护实际恢复、不重发未知请求、数据及资源观察 |

模型流程要求显式设置 `NATIVE_REVIEW_MODEL_CONFIG`，指向独立私有目录中的普通 JSON 文件，目录权限 0700、文件权限 0600。当前固定契约见 [模型资源校验](../scripts/lib/native-review-model.mjs)：字段为 `baseUrl`、`model`、`protocol`、`apiKey`；模型固定为 `claude-opus-5-5`，协议为 `chat_completions`，端点必须是无内嵌凭据的 HTTPS URL。凭据不写入文档或命令行参数。

驱动创建隔离的 `ECHO_AGENT_HOME` 和合成工作区，不复用日常会话或用户模型配置。需要系统选择器的流程应核对自有 PID、完整路径和实际目录授权；后台自动记忆会排除系统临时工作区，应遵守其固定驱动的工作区安排。

Windows 安装包、文件切换、IME、通知和窗口行为按 [Windows 桌面验收](windows-desktop-feedback-acceptance.md) 与 [发行说明](desktop-release.md) 在原生环境执行。外部 OAuth、微信、组织服务、麦克风和云端 ASR 使用专用测试账户及合成数据另行验证。

## 结果与证据

- 区分合成单测、模拟 IPC 浏览器、真实 Runtime、真实应用和安装包。能力探测、工具派发与最终业务结果分别记录。
- 保存源码提交及未提交差异、平台/架构/工具链、源二进制与实际签名副本哈希、运行参数、退出码、未执行/跳过项和失败原因。重编译或修改源码后，旧证据不自动适用于新版本。
- 按真实 sessionId、promptId 和 toolCallId 关联当前回合；历史回放、旧报告和其他任务输出不作为新执行证据。父进程独立核对文件、请求或持久数据，截图作为补充。
- 权限取消应核对没有发生副作用，允许后再核对实际调用。定时触发须核对 scheduledFor 与实际到点记录，手动运行不能替代。
- 取消确认后检查迟到输出和实体输入；重启后检查原成果不变、不会重复派发。持续运行同时报告墙钟、活动时间、锁屏/睡眠与资源采样范围。
- 保留失败与重试记录；低并发复验应注明范围，不将定向通过写成全量通过。人工复核、平台或外部服务未测时明确保留边界。

支持的输出变量以脚本为准：常见有 `UI_REVIEW_OUTPUT`、`NATIVE_LOCAL_REVIEW_OUTPUT` 和 `PRODUCTION_SMOKE_OUTPUT`。使用系统临时目录或被忽略的 `ci-artifacts/`；长期保存时上传 CI artifacts，并去除凭据、私人路径、真实业务数据及不必要的桌面内容。不要把一次运行的 `latest-*` 文件当作版本无关的项目规范。
