# 项目文档

从源码运行、项目结构和主要能力见根目录 [README](../README.md)（[English](../README.en.md)）。本目录维护当前有效的使用说明、设计约定和维护流程。

## 使用与能力边界

| 主题 | 文档 |
| --- | --- |
| 任务、搜索、资料来源与失败恢复 | [产品体验约定](product-experience.md) |
| 代码编辑、Git、终端与工作区 | [Echo Code 与 Theia](echo-code-theia.md) |
| 浏览器和电脑操作的平台与权限 | [自动化平台说明](automation-platform-support.md) |
| 原生桌面工具、引用及读取限制 | [桌面控件](desktop-control/README.md) |
| 电脑任务的交互与暂停续做 | [桌面任务体验](desktop-control/task-experience-design.md) |
| 办公文档生成、预览与编辑 | [办公文档](office-documents.md) |
| 会议录音、转写与校对 | [录音转写](recording-transcription.md) |
| 调度、会话方式与执行记录 | [定时任务](scheduled-tasks.md) |
| 天气与公开网络搜索 | [实时信息](live-information.md) |
| 微信会话交接、指令与授权 | [微信远程对话](weixin-remote.md) |
| 可执行 Skill 的清单与预检 | [Skill 能力声明](skill-capability-manifest.md) |

## 设计与可靠性

| 主题 | 文档 |
| --- | --- |
| 颜色角色、字号、焦点与编辑器主题 | [主题设计](theme-design.md) |
| 资源列表、卡片、模板与响应式布局 | [资源管理](resource-management-design.md) |
| 图标源文件、生成及校验 | [图标维护](icon-assets.md) |
| 记忆提取、隐私过滤与可靠退出 | [记忆处理](memory-recovery.md) |
| 窗口、进程保护与异常恢复 | [客户端恢复](desktop-recovery.md) |

## 开发、验证与发行

| 主题 | 文档 |
| --- | --- |
| 测试层级、回归入口及证据要求 | [开发与验收](testing.md) |
| 原生桌面任务的准备和独立核验 | [桌面控制验收](desktop-control/validation.md) |
| 桌面调用统计与性能比较 | [性能验证](desktop-control/performance.md) |
| Windows 文件切换、输入与通知 | [Windows 桌面验收](windows-desktop-feedback-acceptance.md) |
| 依赖图、补丁来源和安全审计 | [依赖维护](dependency-security.md) |
| 三平台构建、签名、更新与回滚 | [桌面发行](desktop-release.md)（[English](desktop-release.en.md)） |

## 文档维护约定

- 将长期有效的行为、约束、设计理由和复现方法写入对应专题；实现变化时同步更新。
- 一次性 review、任务进度、前后截图、运行日志和机器统计保存在系统临时目录、被忽略的 `ci-artifacts/` 或 CI artifacts 中。专题文档保留生成入口和判定标准，历史记录可从 Git 历史查阅。
- 可重复使用的测试夹具放在 `scripts/fixtures/` 或所属测试目录；发行所需的许可证、资源和来源信息继续跟踪。
- 文档使用仓库相对链接。避免引用维护者电脑上的绝对路径、私有模型连接、会话标识和临时证据目录。
- 文档中的验收要求不代表当前版本已经通过。验证结果应绑定源码版本、二进制、平台和实际运行记录；模拟界面、单元测试与实机结果分别说明。
