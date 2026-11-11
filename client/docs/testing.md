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
| 离线语音 | `pnpm test:meeting-speech` | 固定资源解包、原生语音进程编译及实际离线推理；安装包仍须单独验证 |
| 原生控件模块 | `pnpm test:desktop-native` | 生产 AX/UIA 模块；不等于真实桌面输入 |
| 异常恢复 | `pnpm test:stability`、`pnpm test:stability:native` | 恢复协议，以及独立原生程序的窗口和进程故障注入 |
| UI 与主题 | `pnpm test:ui`；专题入口见 [体验约定](product-experience.md)、[主题设计](theme-design.md) | 生产组件的浏览器渲染和受控 IPC，不替代 OS 或真实服务 |
| 生产首帧 | `pnpm build`、`pnpm test:production-startup` | 实际 dist 的分块加载与挂载；静态 HTTP 中预期的原生 IPC 诊断另记 |
| Office 与文档编辑 | `pnpm office:test`、`pnpm test:document-workspace` | 实际文件、持久化、格式解析及导出 |
| IDE 与插件 | `pnpm test:ide`、`pnpm test:ide:unit` | 先准备对应插件和编译产物，再验桥接、SCM、RPC 及权限契约 |
| 依赖与发行 | `pnpm test:security`、`pnpm test:windows-packaging` | 安全补丁和打包流程回归；完整流程见 [依赖维护](dependency-security.md) 与 [发行说明](desktop-release.md) |

Rust 检查与格式门禁分别为 `cargo clippy --locked --manifest-path src-tauri/Cargo.toml --lib -- -D warnings` 和 `cargo fmt --manifest-path src-tauri/Cargo.toml --check`。新增独立 harness 时同步维护锁文件、安全审计覆盖及 CI。

## CI 故障定位

`ci` 和 `desktop-validation` 是独立流程；同一提交的两套流程及全部平台通过后，才能认为 CI 已通过。被后续推送取消、尚未运行到的步骤和本机定向测试都不能替代该结果。

配置 `main` 的必需状态检查时，选择 `Fast checks`。它汇总类型检查、Rust 锁文件审计覆盖、单元测试和 Rust 格式检查。发布仍须核对同一提交的常规 CI 和三个安装包作业。

macOS 宿主和 Runtime 库分别运行完整测试，避免两个 Cargo 工作区在同一作业中重编译并耗尽 Intel runner 的时间预算。Windows 先独立验证语音资源和推理，再执行完整安装包验证；语音 `.bz2` 归档通过现有 Python 工具链的标准库在进程内解压，避免系统 tar 委托给 MSYS bzip2 后卡在原生管道上。归档固定哈希、模型文件校验和真实离线推理均保留。

主干打包保留已启动的完整矩阵，新推送仅替换等待中的版本，避免长时间构建被连续提交反复取消；PR 仍优先验证最新版本。报告上传放在应用验证之后，服务超时仍会报错，但不会提前截断后续安装和 Runtime 检查。

Rust 缓存在失败后仍保存已完成的依赖编译，供后续修复复用；失败测试会照常执行并报告，不把缓存命中作为验收结果。

UI 检查在固定的 Ubuntu 24.04、Node 22.23.4 和 Playwright 1.55.0 下，分别运行设计、主题、主题生命周期、对比度及栅格采样五套检查。`UI behavior, themes and rendered contrast` 汇总状态要求全部通过。失败时先查看对应 `UI / ...` 作业及 `ui-contract-evidence-*` artifact；主题检查的 `failure.json` 包含窗口、字号、主题、当前场景和浏览器错误，`failure.png` 保留失败画面。macOS 本地通过不代表 Linux 字体和浏览器采样也通过。

主题检查额外用 Linux 浏览器标识运行真实 Monaco，覆盖其专有的 `renderFinalNewline=dimmed` 默认值。末尾空行的行号必须参与对比度检查，并在切换主题后继续使用可读颜色。

Rust 宿主及 Runtime 测试通过 `NATIVE_TEST_OUTPUT` 保存每次子进程的完整 `stdout.log`、`stderr.log`，并上传 `native-test-results-*`。测试失败详情通常位于 stdout，编译警告通常位于 stderr，排查时应同时查看。公开错误注释分别截取两条输出的末尾，避免警告覆盖断言，或超长注释在真正的错误前被平台截断。

隔离验收固定合成的 `ECHO_AGENT_AGENT_ID`。全新数据目录没有缓存的机器标识，Runtime 的初始化响应会等待平台硬件查询；专家团和模型流程测试应排除这项无关依赖，保留原有握手超时及业务断言。

安全审计的反例单测会模拟注册表不可用，但必须将 `GITHUB_STEP_SUMMARY` 隔离到每个测试的临时文件，避免模拟失败污染真实 CI 摘要。实际扫描仍发布自己的报告，扫描未完成仍须失败；Rust 维护提示与安全漏洞分开报告。

各原生平台通过同一个本地 Action 安装固定的 protoc。归档缓存按平台、架构及安装脚本区分，使用前重新校验 SHA-256；下载完成后立即保存缓存，后续测试失败不会丢弃已验证的工具。损坏响应只作有上限的重试，不会放宽哈希校验。

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
