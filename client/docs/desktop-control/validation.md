# 桌面控制验收

本页维护可重复执行的验收方法。能力探测、模块测试、真实 Runtime 工具调用、完成桌面任务及安装包验收分别记录；历史通过结果只适用于当时的源码、二进制和平台。

## 环境与准备

当前固定驱动运行于 macOS，需要可访问且已解锁的图形会话，以及实际验收应用的辅助功能和屏幕录制权限。终端或单元测试程序获授权不代表应用具有同样授权；重新签名后应重新核对。

先按 [开发与验收](../testing.md) 准备显式私有模型资源、检索模型、Theia、Office 和前端，再构建带 `tauri/custom-protocol` 的程序。该驱动创建独立的私有数据目录和合成工作区，不复用真实会话或业务资料。

```sh
node scripts/validate-native-desktop-controls.mjs src-tauri/target/debug/echoagent --task-flow
```

驱动记录源码构建产物、实际签名副本、固定夹具、应用 PID 和模型身份。需要系统目录选择器时，核对自有 PID、完整路径及持久授权。普通开发服务器地址不能作为独立内嵌应用验收。

## 模式与完成条件

| 运行方式 | 范围与判定 |
| --- | --- |
| 不带模式参数 | 只读 Runtime 接入资格：真实工具身份、分页和暂停；`desktopAcceptance` 不标为完成 |
| `--smoke` | 短文稿输入、读回、保存和暂停；有活动执行预算，不替代完整跨应用验证 |
| `--journey` | 合成桌面场景准备及操作记录；各场景须分别核验实际结果 |
| `--task-flow` | 明确新建空文稿、精确正文、暂停静默、同任务自然语言继续及真实保存；完成后必须运行独立核验器 |
| `--universal-flow` | 在严格任务流程上追加 Safari 的本地合成页面输入与系统文件选择；不代表任意应用已通过 |

任务模式通过正常 Composer 提交用户请求，由实际模型与生产 Native MCP 操作桌面。驱动只准备合成输入与记录证据；不能通过 Shell、直接文件写入、CDP 或测试 IPC 代做目标文稿。

驱动打印的 `home` 为本次验收目录。待流程完成后，将下面的目录替换为该实际值：

```sh
ACCEPTANCE_HOME='/path/to/acceptance-home'
node scripts/verify-native-desktop-task-flow.mjs "$ACCEPTANCE_HOME"
```

独立核验要求原始 Runtime journal、当前会话和模型身份相符，并联合检查：

- 本轮实际执行新建命令，在空文稿中输入本轮独有正文。
- 实际读回与用户全文逐字一致，保留中文、特殊字符及明确要求的末尾换行。工具的 `verified` 仅证明该次入参被满足，不能替代用户要求。
- “停一下”撤销执行权，取消确认后的静默区间没有实体操作；“继续保存”在同一任务建立新令牌并续做。
- 系统保存面板产生真实文件，正文、字节及原生 `documentUrl` 与指定路径一致。文件存在、窗口标题或保存快捷键返回均不足以判定成功。
- 模型自然结束，普通任务内不出现重复审批；没有由验收器代批、代写或以取消制造完成。
- 跨应用页面实际接收输入和所选文件字节，并由目标应用原生状态读到本轮成功标记。

## 回归与负例

| 范围 | 入口 |
| --- | --- |
| 前端暂停、继续与确认状态 | `pnpm test` |
| Runtime journal、分页与任务证据 | `pnpm test:scripts` |
| AX/UIA、引用、焦点、取消和边界 | `pnpm test:desktop-native` |
| 主程序和 Runtime 类型检查 | `cargo check --locked --manifest-path src-tauri/Cargo.toml --lib` |
| 真实 ACP 启停与工具配置恢复 | `pnpm test:runtime-smoke` |

macOS 原生模块测试需要可访问图形会话，但普通测试不派发输入。可在已经安装对应 Rust 目标时用 `cargo check --locked --manifest-path scripts/desktop-native-check/Cargo.toml --target x86_64-pc-windows-msvc` 检查 Windows 类型；这不等于 Windows 实机通过。

负例应覆盖跨会话/过期引用、变化目标、歧义与截断、受保护/禁用控件、缺页或重复页、错误模型、历史回放、缺少真实新建、正文不符、未保存及终端替代。不能删除这些约束来获得通过。

## 记录与平台边界

保留原始失败、重试和最终核验，不把后续补丁当成正在运行的旧应用已经更新。报告应区分单次原生调用、模型参数错误、系统授权和提供方异常；某个应用查询失败不能直接推断全局权限丢失。

休眠、锁屏、人工等待和验收活动时限分别记录；期限只暂停、不自动恢复任务。清理时仅处理本次明确归属的 PID，记录正常退出或强制收尾的区别。完整证据留在私有验收目录或 CI artifacts，长期文档保留方法与限制。

Windows 在实际交互桌面另行验收原生输入、文件面板和暂停恢复；安装包与通知检查见 [Windows 桌面验收](../windows-desktop-feedback-acceptance.md)。自绘界面、受保护内容、无限长文读取和 provider 硬挂起的限制见 [原生控件说明](README.md#平台限制)。性能比较见 [性能验证](performance.md)。
