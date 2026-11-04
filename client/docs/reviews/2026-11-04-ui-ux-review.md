# EchoAgent 桌面工作台 UI/UX 评审与修复

评审开始于 2026-11-04，验证延续至 11-05。初始提交 `004563af55b1ad32e6e50f7e1c03d6becf1e16b8`，工作区干净；本次修改保留在本地。下面的通过范围均以本轮日志为准，不沿用已有主题评审的历史测试结果。

共修复7项已复现的生产问题。最终代码工作台7项原生连续验收通过，窗口13项通过；完整单测3702项为前一阶段快照，后续修复另有53项前端定向、29项Rust及20项最新夹具范围检查。其余平台与连续流程边界在下文明确列出。

证据已归档，剔除了编译缓存、可执行文件和用户profile/凭据。证据根目录：[/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a710-8a26-70d2-baad-51e070d4c84a/echoagent-ux-review](/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a710-8a26-70d2-baad-51e070d4c84a/echoagent-ux-review)。[前后截图索引](/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a710-8a26-70d2-baad-51e070d4c84a/echoagent-ux-review/index.html)、[命令与结果清单](/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a710-8a26-70d2-baad-51e070d4c84a/echoagent-ux-review/verification.json)、[实际人工抽查记录](/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a710-8a26-70d2-baad-51e070d4c84a/echoagent-ux-review/visual-review.json)。原始运行目录 `/private/tmp/echoagent-ux-review-20261104` 与隔离profile保留用于定位；归档中的截图、日志与收据可独立查看。

## 改进结果与设计取舍

这轮主要修复任务与文档的状态归属问题，并统一待回答卡片的操作层级。已选择的答案、当前问题、倒计时和等待确认状态属于请求；文档的持久化属于原任务；导航、Toast 和向输入框注入上下文则属于发起操作的当前视图。两类生命周期分开处理，避免切换页面后丢失输入或操作错误任务。

沿用现有中性工作面和靛蓝主操作体系，没有再次更换品牌颜色。标题、正文和辅助信息继续使用 token 的层级；危险、等待和错误同时用文字及图标表达。问答头部明确显示“需要你回答”、已填写数量、剩余时间及排队数量，提交期间显示等待任务确认。紧凑图标按钮统一为 32px，正文控件继续遵循现有用户字号缩放。保留卡片内部滚动、Diff/代码的局部横向滚动和大字号布局；没有全局隐藏溢出、缩小字体或移除操作。

现有视觉规则复核：页面标题 24/32、区块标题 20/28 或 16/24、正文 14/22、辅助文字 12/18，使用 `tokens.css` 中现有层级，并服从各页面已有字号设置。圆角使用 4/6/8/12px 等既有 token。创建、保存、发送用 primary，选择用 selected，暂停/待确认用 warning，失败用 error；文档纸张、品牌图形、代码语法与 Diff 保留专用语义。静态截图冻结动画，动态图标、侧栏及执行过程另行启用实际动画并检查减少动画设置。

## 源码入口与实际可达页面

生产入口是 `src/main.tsx → App.tsx`。`App` 决定初始化、导航、新会话/已有会话、代码工作台、专家团和设置覆盖层。名为 `PlaceholderPage` 的组件实际承载多个生产管理页面，不能按名称判断为未实现。代码工作台打开后会保持挂载并隐藏，故编辑器重开和主题变化需要独立检查。fixture 入口由验证服务器注入，不是普通用户导航，也不作为真实后端证据。

| 页面/子视图 | 关键状态与生产组件 | 本轮主要验证入口 |
|---|---|---|
| 首页、新建任务 | `HomePage`、`Composer`、`App.handleSendNew`；模型/目录/资料、创建失败、草稿 | ui-surfaces、model-presentation/connections、composer-feedback、App/Composer 单元测试 |
| 会话、阅读、任务操作 | `ChatView`、`Sidebar`、`session-store`、`message-queue-store`；流式、队列、接续、失败/不确定、侧栏偏好 | session-workbench/workspace、sidebar-session/toolbar、conversation-navigation/continuation、task-recovery、相关单元测试 |
| 用户决策 | `FolderTrustDialog`、`PlanPanel`、`PermissionDialog`、`QuestionInlineCard`、`QueuePanel`；版本、超时、拒绝、后端确认、跨会话 | permissions、question、unified-tool-modes、状态与组件测试 |
| 结果、变更、交付 | `ToolSidePanel`、`ToolCallCard`、`FileChangesPanel`、`FilePreview`；Diff、局部滚动、读取错误、导出 | execution-outcome/progress、share-export、FileChangesPanel 测试 |
| 附件、文档 | `AttachmentPreview`、`DocumentWorkbench`、`DocumentSession`；副本/编辑稿/Word/导出、冲突、恢复、迟到回调 | attachment-preview、document-reading/workspace/failure、document-workspace-check、Office 实际导出 |
| 项目 | `ProjectsPanel`、`ProjectDetailView`、`KnowledgePicker`；动态/工作项/资料、默认配置和本次引用 | ui-surfaces、design-system 的项目子视图及配置抽屉、组件测试 |
| 组织 | `OrganizationMemoryPanel`；资源与我的提交、共享范围、索引和部分失败 | organization-workspace、OrganizationMemoryPanel/KnowledgeRuntime 测试，隔离 IPC |
| 专家、技能、连接器、插件 | `PlaceholderPage` 分发的管理页面、引用选择与安装/卸载状态 | ui-surfaces、design-system、plugin-management、相关组件/状态测试 |
| 专家团 | `features/groups/GroupWorkspace`、`group-store`；聊天/成员/工作/交付物/设置、并行、单项停止、归档恢复 | group-ui、group-lifecycle、team-ui、组模块单元测试，隔离 IPC |
| 代码开发 | `features/coding`、`TheiaIdeFrame`、bridge、`TheiaAgentComposer`、`local-monaco`；Ask/Plan/Agent、上下文、脏编辑、版本与保存 | coding-composer、真实 Theia 后端、主题生命周期；完整原生 Agent 旅程另列 |
| 设置 | `SettingsPanel`、`SettingsSections`；导航、搜索、未保存屏障、模型连接和部分成功 | settings-navigation/controls、notification-settings、model-connections、SettingsPanel 测试 |
| 原生壳层 | Tauri 配置、`src-tauri/src/lib.rs` 及窗口/托盘/关闭/通知/恢复模块 | 配置源码、原生 WKWebView、本次构建的桌面窗口检查；平台边界见后表 |
| 关联模块 | 知识库、定时任务、会议、备份/退出保护等共享界面与逻辑 | design-system、meeting-workbench、task-recovery、全量现有单元测试；没有宣称全部后台实机验收 |

设置实际导航与兼容入口按当前 `SettingsPanel` 判断；通用、通知、模型、记忆、安全、高级设置、微信、云存储、用量、数据、帮助，以及搜索/兼容入口分开验证，不以通用页截图代表整个设置模块。

## 已复现问题与修复

### UX-01 · P1 · 切换任务丢失问答草稿，并重置等待/超时状态

- 范围：普通会话和代码开发 Agent 栏；两主题、所有窗口，窄面板和大字号定向复查。
- 复现：打开多问题请求，选择并填写第一项、进入下一项；切到其他任务再返回。另在有超时的请求倒计时后重挂卡片；提交后端尚未确认时重挂并再次点击。
- 修改前证据：`question-reproduction.log` 中新增三项回归分别失败，证明答案/步骤丢失、倒计时重启及重复提交窗口。`question-before/` 保留原视觉基线。
- 根因：草稿、当前步骤、倒计时起点和 busy 都存在卡片本地，生命周期短于后端请求。
- 修改：`src/stores/question-store.ts:30` 的响应状态以 `[sessionId, requestId]` 隔离；`request:89` 记录绝对截止时间；`beginAnswer:71` 原子检查是否已提交/过期；关闭时清理，迟到 finish 不复活请求。`src/components/QuestionInlineCard.tsx:21` 读取请求状态，`:46` 按绝对时间计算，`:157` 仅在真实接纳后结束等待。
- 验证：新增重挂、迟到响应、隔离、超时测试；最终问答 34 组布局与交互通过。请求超时后保留可读内容并禁用提交，后端 retained registry 仍是有效性权威。没有自动重放答案。

### UX-02 · P1 · 旧文档的异步完成事件干扰已切换的任务

- 范围：文档创建副本、导出、交给 Agent、剪贴板反馈、保存错误。
- 复现：在任务 A 创建副本/保存后交给 Agent；完成前关闭预览并切任务 B；再让 A 的异步回调完成或失败。
- 修改前证据：新增生命周期测试的前三项在原生产代码失败；记录见 `document-reproduction.log`。
- 根因：持久化工作与瞬时 UI 回调共用一个异步闭包，回调没有检查发起视图是否还有效。
- 修改：`src/components/DocumentWorkbench.tsx:54` 添加视图代数，`:90` 只允许当前视图反馈与解锁；`:96` 产物仍登记给原任务，但不抢当前导航；`:102` 保存后再次检查，再注入 Agent 上下文。生产调用点 `ToolSidePanel.tsx:819` 仍以任务与路径作为 key。
- 验证：`DocumentWorkbench.lifecycle.test.tsx` 验证迟到副本、flush、失败和当前视图成功；真实 DocumentSession 的保存队列/恢复测试继续执行。测试中的编辑器被替换，仅证明宿主回调；真实 TipTap 由下一条和文档 UI 检查覆盖。

### UX-03 · P1 · 编辑稿保存失败仍显示“已保存”，读取失败提供错误重试

- 范围：已有同步版本的文档；最小窗口、两主题、实际 TipTap。
- 复现：先创建已保存副本，编辑“6:30”为“7:45”，令编辑稿保存失败；原顶部仍显示“已保存 · Word 已同步”。另让已有记录的 refresh 失败，原按钮错误地调用 flush。
- 证据：`document-failure-before/save-failed-{light,dark}.png` 与 `document-failure-after/` 同名图。验证同时检查原持久化记录未变、新内容仍在编辑器中，而非仅截 Toast。
- 根因：状态文案先按旧 record 的 revision 推断成功，没有表达 `phase=error`；重试只用“有无 record”判断失败类型。
- 修改：`src/lib/document-workspace.ts:19` 区分 read/save 失败；读取和保存 catch 分别设置来源。`DocumentWorkbench.tsx:30` 独立状态映射先处理错误，`:109` 和 `:130` 按来源提供“重试读取”或“重试保存”。保留原恢复数据、版本冲突、外部变更和 Word 同步屏障。
- 验证：新脚本 `scripts/validate-document-failure-ui.mjs` 在原代码与修复后各执行两主题：失败记录保持旧值，草稿保留；恢复写入后点击重试，实际 fixture 记录变为新值。IPC 为模拟，编辑器和保存控制器为生产实现。Word/PDF 的磁盘证据由实际 Office/Rust 检查单列。

### UX-04 · P2 · 待回答层级与紧凑工具栏尺寸不一致

- 复现：在代码开发 300–410px Agent 面板、18px 应用字号打开多问题卡片。原头部图标 26px，主工具栏 32px；填写与排队进度不够明确。
- 修改：`src/styles/tokens.css:609` 提供共享 32px 紧凑图标尺寸；`src/styles/app.css:87` 和 `:3581` 使用同一 token；问答标题加重，显示已填写/排队数量，保留按钮、键盘导航和内部滚动。
- 证据：`question-before/` → `question-chrome-final/`；`sidebar-toolbar-final/`。
- 验证：问答 34 组、侧栏工具栏 56 组通过。处理中发现 33px 中间方案违反既有 32px 合约，已纠正为 32px 并重跑；没有修改该断言。最小卡片仍保留至少 120px 阅读区，长选项使用局部滚动。

### UX-05 · P2 · Monaco 初始化 promise 合约与主题重开处理不一致

- 证据：初始全量测试 327 文件中，只有 `local-monaco.test.ts` 三项失败并产生两条未处理拒绝；其他 3675 项通过。没有据此声称实际编辑器全面阻断。
- 根因：每次返回 `preparing.then(...)` 得到不同 promise；失败重试和已加载主题刷新应在同一个初始化合约下处理。
- 修改：`src/features/coding/lib/local-monaco.ts:31` 返回共享初始化 promise；初始化内注册主题，失败清理；已加载编辑器重开同步当前主题。测试补全 defineTheme mock，并保留本地资源、worker、共享身份及失败重试断言。
- 验证：定向测试通过；theme-lifecycle 与真实 Theia 11 组检查通过。Monaco 与 Theia 是不同集成路径，分别记录。

### UX-06 · P1 · Plan 使用上一轮答复，且编辑器重复

- 实机复现：真实 Ask 后切换 Plan，`native-code-live/02-real-plan.png` 中计划框出现上一轮 Ask 的原始推理标签，并重复显示多个编辑器。
- 根因：`CodingWorkbench` 直接取最近的完整 assistant text，没有核对该轮是否属于 Plan，也没有使用现有公开答复提取器；计划和 composer 兄弟节点还用了相同 key，React 更新时产生重复节点。
- 修改：`src/features/coding/lib/plan-result.ts:5` 核对最新用户轮的实际 Plan prompt，仅从其完整答复提取公开内容；`CodingWorkbench.tsx:3019` 与 `:3061` 使用不同前缀 key。用户编辑过的计划继续由既有草稿机制保留，不被迟到结果覆盖。
- 验证：新增 `CodingWorkbench.test.tsx:267` 在原代码先因错误 Ask 内容失败；修复来源后又准确复现三个编辑器，修正 key 后通过。相关4文件53项通过（含18项原生范围检查），见 `plan-source-before.log`、`plan-source-after.log`、`plan-source-final.log`。原生 driver 另保留唯一编辑器、完整 Plan 入框与推理标签排除断言。

### UX-07 · P1 · 保存后旧检查被误判为“不适用”

- 实机复现：`native-coding-plan-fixed-final` 完成真实 Ask、Plan、Agent 改写/保存、Undo/Redo、test/lint和Diff。随后在真实Theia编辑器追加注释，脏缓冲状态的两项检查已显示过期；保存后原生交付报告未返回必须的 `not_satisfied`，旅程停止，未把部分成功当作整组通过。
- 根因：`delivery.rs:510` 先过滤出当前内容和配置的检查，再调用门槛计算。旧回执全部失效时，过滤后的空集合被当成没有检查历史，freshness 变成 `not_applicable`，不能作为交付阻断。磁盘变更集已正确清空 verifiedRevision，历史回执也没有被删除。
- 修改：`src-tauri/src/coding/delivery.rs:511` 区分“确实没有检查历史”与“有历史但没有当前回执”；后者在仍有文件变更时返回 `not_satisfied`，要求重新验证。实际旧 test/lint 回执保留，不能显示成对新内容的证明。
- 验证：同文件`:1584` 的磁盘回归覆盖保存后改内容、改变检查配置两种失效，并核对原JSONL字节不变；交付模块 **29项全部通过**，见 `delivery-freshness-final.log`。首轮单测误用Git基线而在初始化失败，保留 `delivery-freshness-isolated-before.log`，不把该失败当成产品复现；产品修改前证据来自上述原生流程。

### QA-01 · P2 · 验收脚本使用旧品牌色和错误基线说明

- `validate-design-system.mjs` 仍要求绿色通道占优，而本次初始代码和 `docs/enterprise-theme-review.md` 已明确为靛蓝色。未修改生产配色去迁就过时规则。
- 新 `scripts/ui-primary-action-audit.mjs:16` 与 `scripts/validate-design-system.mjs:33` 逐主题、逐 normal/hover/active 状态严格比对现有正式 RGB/前景/不透明度，并保留 ≥4.5:1 对比度门槛；6 项检查包括正式配色正例，以及“对比度足够的旧绿色”、单通道偏移、错误前景/主题/状态和透明度负例。最终设计系统 378 组观察通过。此处是更严格的规范同步，不是忽略失败。
- 截图索引原硬编码旧提交 `540769d`。`build-theme-review-index.mjs:5` 现在读取本轮 manifest 的真实基线，未记录时明确显示未记录，避免伪造对比依据。

### QA-02 · P2 · 固定原生旅程没有处理真实 Plan 的保存澄清

真实模型先后询问“是否保存未保存缓冲区”以及“何时运行本地验证”。旧夹具把 AskUserQuestion 一律判为意外工具，导致合法决策中断。保留两次失败日志 `native-coding-final.log`、`native-coding-clarification.log`，不将其算作客户端流程通过。

`scripts/fixtures/native-coding-workspace-review.js:20`（范围）、`:70`（输入事件）、`:181`（决策服务）与`:290`（任务提示）现在仅在 Plan、所属会话匹配且问题属于固定 `double.cjs` 保存/已知两条验证命令时，通过生产权限卡“允许一次”和 `QuestionInlineCard` 填写、下一步、提交。问题数量和长度有限制；其他问题、计划审批和目录信任仍拒绝。答案明确本轮只规划，最终计划批准后保存，检查由用户触发；所有后续文件候选、路径和命令仍经过原严格检查。明确了原来含糊的测试任务提示。模型经普通问答提出“是否定稿”时，回答明确当前不批准执行，先展示可编辑计划。`native-coding-clarification.test.mjs` 的 9 项测试覆盖实际两类问题、输入/textarea原生事件，以及错误模式、文件、结构、数量与长度负例。没有注入模型回复、放行任意命令或改写最终验收结果。后续实际问题把 `double.cjs` 放在选项描述中；范围判断改为核对完整问题和选项，保存范围仍只能是该文件，混入 other.cjs/test.cjs 的保存请求均拒绝。最新20项范围回归通过，见 `native-final-decision-guards.log`。

### QA-03 · P2 · 原生夹具拒绝 Runtime 的本地工具接口发现

重新解锁后的真实流程已完成只读Ask、只读Plan、编辑最终计划和点击执行，随后在Agent请求 `SearchTool(query="editor save document buffer write file", limit=15)` 时被旧夹具拒绝；见 `native-coding-unlocked-final.log`。此失败记录不作为整组通过依据。

源码核实 `vendor/echo-agent-build/crates/codegen/echo-agent-tools/src/implementations/search_tool/mod.rs:259`：run读取本地ToolIndex快照并返回工具schema，不执行查到的工具。新增 `scopedDiscovery` 只允许Agent、SearchTool、编辑器相关有限词汇、160字符内及最多15条结果，并通过生产权限卡“允许一次”。UseTool/MCPTool、文件目标、替换候选、版本与命令的原检查全部保留；没有按generic read-only标签放行。

后续两轮分别发现夹具漏列自己创建的 `completion.cjs` 只读文件和 Runtime 当前会话的 `plan.md`。`scopedRead:49` 只读四个固定样本文件，`scopedSessionPlanRead:51` 仅在 Plan/Agent 且数据目录、编码工作目录、会话 ID 和文件名完全匹配时读取当前计划；拒绝其他会话、目录穿越、URI和其他文件。写入仍限于 `double.cjs` 的既有候选检查。新增范围正负例后，17项检查通过后，再针对实际观察到的 `local check run project verification` 增加完全匹配的本地接口查询分支与负例；当时18项随 `plan-source-final.log` 通过，后续20项范围检查见QA-02。查询不会执行工具，后续执行范围不变。

### QA-04 · P2 · 项目切换在 iframe 卸载间隙过早发命令

`native-coding-delivery-fixed-final` 已通过前五项，包括UX-07保存后失效。切项目后，旧夹具只看目标标题和加载遮罩消失，在旧iframe已卸载、新iframe尚未挂载的瞬间就发送编辑命令，触发“Actual Theia frame missing”。`switchProject`现在同时要求新的可见iframe已替换旧节点且生产就绪遮罩消失；随后仍由真实WorkspaceService精确核对根目录、文件内容与任务会话归属。原有项目切换与回程断言保留，驱动语法/隔离自检通过，不将这一轮部分通过算作完整通过。

后续 `native-coding-plan-decision-final` 已通过前六项（包含两项目切换及原任务/文档恢复）。新增A08留下的检查面板挡住编辑器，Tab前的原生焦点步骤因此超时。夹具现在先通过生产“返回编辑器”按钮退出检查面板，再执行原有实际文本焦点、建议展示、Tab接受/Undo和Escape丢弃检查；没有移除焦点断言或直接插入补全文本。

本轮未复现 P0。其余覆盖中没有发现需要改写布局骨架、权限模型或编辑器内核的证据，因此保持已有架构。

## 窗口、主题、字号与证据边界

本机 macOS 15.8.2 / Intel x86_64 / 16GiB；Node 24.22.1、pnpm 10.34.5、Rust 1.98.0。初始依赖安装后执行测试，未改依赖锁。Tauri 基础和 macOS 覆盖配置确认最小 1024×680，默认 1280×800；macOS Overlay 隐藏标题，交通灯位于 x20/y26，Windows 使用自绘窗口控制。

显示器 AppKit frame 为 1680×1050 点，可见区域 1680×946 点，backingScale=2。浏览器矩阵使用 1680×946 CSS viewport 作为代表性最大布局，不等于原生实际最大化。原生窗口测量与 CSS viewport 分开记录。没有把 DPR=2、浏览器缩放或应用字号称为系统 150%。

| 层级/组合 | 本轮状态与证据 |
|---|---|
| 14 个代表页面 × 1680×946、1440×900、1280×720、1024×680 × 浅/深 × 应用13px | 修改前/后各112组通过，`matrix-before-verified/report.json`、`matrix-after/report.json`；CSS viewport/DPR/根字号及截图逐例记录 |
| 应用18px × 1024×680/1280×720 × 两主题 × 同14页 | 56组通过，`large-font/report.json`；root 按现有 applyFontSize 映射为约22.154px |
| 额外1024×640 CSS压力视口 × 两主题 × 14页 | 28组通过，`stress-1024x640/report.json`；不是受支持的原生窗口尺寸 |
| 高风险卡片/浮层/子视图 | 问答34组、侧栏工具栏56组、设计系统378组；权限、上下文菜单、文档错误、模型、设置、组织等定向日志详见清单 |
| 真实 Theia 后端、临时磁盘工作区 | 11组通过，编辑器/Diff/终端/整体宿主/最小窗18px，`real-theia/`；主矩阵 coding-host 的 iframe 占位不计入此证据 |
| 真实原生 WKWebView 夹具 | 4组表单浅/深13/18px、实时主题变化通过；脚本记录窗口 active=false，因此不能当活跃桌面键盘焦点验收 |
| 实际系统150% | 未验证：本机未切换该设置；需要支持该缩放的真实系统重新执行，应用18px不能替代 |
| Windows 原生窗口、跨DPI/外屏拔除 | 未验证：没有对应平台/外屏会话；源码核对不能代替实机 |
| 当前源码桌面窗口、真实 Agent | 见后面的本轮最终原生结果，不并入 fixture 通过数 |

矩阵十四页：首页、会话、通用设置、模型设置、项目列表、项目详情、组织、专家、技能、连接器、插件、代码宿主、文档、专家团。组织提交/资源、项目子页、专家团各页、设置其他子页由定向脚本补齐，未假称所有状态都跑完整笛卡尔积。自动几何检查根与主容器意外横溢、重要弹层/主操作边界及渲染异常；允许内容本身的合理滚动。人工只抽查 visual-review.json 所列图片，不声称逐张人工验收。

## 连续用户流程与操作成本

| 流程 | 已执行部分、成本与边界 |
|---|---|
| A 普通任务 | 生产 App 的真实模型、工具、磁盘与 Diff 流程以最终 native 日志为准。浏览器生产组件的输入、权限、计划/修订和结果导航已执行；不能把不同 fixture 拼成一个完整真实 Agent 任务。 |
| B 异常与恢复 | 组件/生产 App 模拟 IPC 覆盖拒绝、队列、停止/继续、发送失败、不确定结果、迟到回执、任务切换及恢复；问题UX-01/02的改前红/改后绿证据完整。真实 native 的停止/入队/重启另列。 |
| C 文档 | 浏览器生产 TipTap完成预览→副本→编辑段落→保存→Word/导出交互→历史，新增失败→重试。实际 Office worker/Rust 另验证导出文件内容、格式、合并表格PDF位置、四格式本地链接及无效输入拒绝；两个层级分别通过，不能称同一原生OS保存对话框全程通过。 |
| D 代码开发 | 真实 Theia临时工作区编辑器/差异/终端通过；coding-composer 16组模拟IPC涵盖上下文添加/预览/移除、错误与模式。真实脏缓冲→Ask→修订Plan→Agent→本地验证→再编辑失效→两项目返回→Tab的7项连续原生旅程最终通过；限定范围见最终收据。 |
| E 专家团 | 创建两成员与协调员；生产组UI的成员消息、并行工作、决策、单项停止不影响其他任务、整理语义、交付定位、设置失败/重试及归档恢复通过；底层运行时模拟，未调用真实组织/成员账号。 |

待办发现的可复查路径：当前会话内提问直接可见；从其他管理页经侧栏待审批任务进入再处理；折叠侧栏先展开再选任务；预览展开时通过侧栏返回所属任务。决定步骤不删除授权确认。新增头部使用户在卡片内即可读到填写/排队数量，不必逐项翻页推断；重返会话不再重填。上述是界面路径检查，不是用户研究的平均用时。后台通知点击、失效请求及跨原生焦点起点未完成实机计步，不报告虚构数字。

## 构建和检查记录

已完成：`pnpm build`（图标、TypeScript、Vite）；前期前端产物启动检查 `production-final` 通过，仅两个严格白名单预期 Native IPC 不存在错误。Plan补修后再次通过 `plan-source-build.log` 的完整前端构建，最新原生实际启动由后述7项流程证明。前期修复相关5文件58测试通过；主操作规范6项检查通过；文档真实导出命令中2文件24测试、Rust7测试及实际文件导出通过。另保留 [Word验证稿](/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a710-8a26-70d2-baad-51e070d4c84a/echoagent-ux-review/document-artifacts/保存恢复验证稿.docx)、[PDF验证稿](/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a710-8a26-70d2-baad-51e070d4c84a/echoagent-ux-review/document-artifacts/保存恢复验证稿.pdf) 与SHA256收据；重新读取DOCX XML及PDF内容，核对“7:45”且没有旧值“6:30”。这是实际worker输出，不是原生UI操作录像。

已有UI总链首次停在中间33px方案的侧栏32px断言，纠正后侧栏56组通过；其余门槛拆分运行，除过时绿色规范已按严格正式色修正重跑外，全部执行结果保留。初始全量327文件只有Monaco三项失败；开发过程中另一次全量遇到设置和打包测试5秒超时。保留失败日志，不改超时、不删断言，最终稳定运行结果见下节与 verification.json。

动态检查：sidebar-session实际拖动/更新/键盘菜单/减少动画通过；execution-progress以 `freezeMotion:false` 检查展开、焦点、真实等待、完成和减少动画。没有进行可比的真人输入延迟/内存基准，不根据静态截图宣称性能提升。IME、原生拖放、听写设备/权限与真实通知仅按已执行层级记录，不把浏览器模拟 composition、clipboard、通知提交等当系统实机通过。

## 最终原生与全量结果

最后一次完整运行 `pnpm exec vitest run --maxWorkers=1 --no-file-parallelism`：**330文件、3702测试全部通过**，耗时548.05秒，见 `unit-final-clean.log`。之后完善的原生夹具范围与输入事件检查，以及原生实测发现的 Plan 来源/key 修复，另以4文件53项定向复验通过（含当时18项原生范围检查），后续夹具范围20项另行通过；另有Rust交付模块29项通过；未再次运行全量，不合并报告测试总数。原先打包/设置超时在这次全量中通过，GroupActions先单独5/5通过再全量通过。保持既有超时和所有断言。

macOS真实窗口最终 `native-window-complete/report.json`：**13项通过**。实际外框1440×900、1280×720、1024×680；请求1024×640仍保持680最小高度；原生Window菜单performZoom得到1680×946、`AXFullScreen=false`，随后还原。最小化/恢复、正常Quit、重启后的尺寸与位置恢复、关闭驻留、同一实例再次启动唤醒及最终正常退出均通过。仅操作新建空profile，没有未保存内容或后台任务，因此不能外推为所有退出保护通过。外框点、AppKit屏幕坐标和CSS viewport不混用；原生CSS viewport未读取，不作推算。

保留了原生脚本中间失败：首次锁屏无法等到页面；绿色按钮进入全屏与普通Zoom混淆；窗口初始位置使900高请求被系统夹到871；菜单动作后使用旧AX按钮引用不可用。最终将窗口先放在可见区域、严格检查尺寸，并用当前应用原生菜单执行对应窗口动作。没有通过修改客户端或放宽尺寸断言消除这些自动化问题。

原生核心旅程 `native-core-unlocked/run-e1l2ZD/result.json` **未通过**：内置模型与Runtime就绪，生产App实际创建任务并得到回复，但新任务正确落在自动生成的 `default/Tasks/<id>`，旧runner按固定default目录查找，因而超时并没有执行后续队列/重启。`native-core-automatic-directory-diagnosis.json`记录真实绑定目录及完成状态。此结果不等于模型不可用，也不等于该完整旅程通过；不能仅用回复标记判成功。

`native-coding-reviewed.log`保留再次锁屏时preflight拒绝的记录。用户重新解锁后的最终 `native-coding-inline-pane-final/result.json` **完整通过7项**：真实未保存缓冲区Ask、公开Plan唯一编辑器与修订后Agent执行、实际版本桥保存及Undo/Redo、两条独立npm检查、真实Theia Diff、再次编辑/保存使两条历史检查过期且交付freshness未满足、两项目切换/返回、真实模型Tab建议接受/Undo及取消丢弃。其中Plan/Agent合并为一项，Diff单列，详见原生收据的7个ID。

父驱动独立确认：实际Runtime请求收到完整修订计划，模式轨迹Ask→Plan→default(Agent)；实际服务的Theia bundle SHA256与当前资源一致；只有目标double.cjs发生受限改动，其他原始文件及另一工作区未变；磁盘重新执行 `node --test test.cjs` 退出码0；模型完成7个工具调用；App正常退出。Tab本轮实际建议1816ms后展示，接受1字符，随后原生Monaco Undo恢复用户前缀，快速输入/Escape产生取消丢弃事件，没有新建任务或保存completion.cjs。该延迟仅是此样本，不作为性能基准。

[最终原生收据](/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a710-8a26-70d2-baad-51e070d4c84a/echoagent-ux-review/native-coding-inline-pane-final/result.json)、[实际样本与磁盘校验](/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a710-8a26-70d2-baad-51e070d4c84a/echoagent-ux-review/native-code-final/receipts.json)。本轮可执行文件SHA256：`7f5a645752ab92958f1ec8d19cb83c580de19175bfd202d914a61ab61c56449a`。正常用户profile未参与验收；临时防休眠已释放。

## 剩余复验（按风险排序）

1. **P1 实机任务与交付边界**：修正native-core的前置操作，先通过生产目录选择器明确选中隔离的default目录再提交，保持原有精确cwd和Office路径断言；当前默认目录分配本身正确，不应改产品去迁就旧脚本。在该目录准备配置文件，按真实模式完成配置与说明文件、决策、Diff和产物；本轮尚未完成这一字面A旅程。不得用模型的成功文字替代实际文件/回执。
2. **P1 原生文档与退出保护**：在独立数据目录打开样本，副本→编辑→保存/同步→OS另存→重新打开；另开未保存/后台审批/录音状态逐项验证取消退出、保存失败恢复与真正退出。已有组件和Rust保存测试不能替代这些原生连续流程。
3. **P1 Windows与系统缩放**：Windows支持最小窗、自绘控制、实际150%、跨DPI/拔屏；macOS支持的真实显示设置下复查。分别保存外框、内容区、CSS viewport、DPR和系统设置证据。
4. **P2 系统输入/通知/媒体**：中文IME候选确认、原生粘贴/拖放、通知横幅与点击目标、失效请求、麦克风拒绝/缺设备/停止收尾、跨任务迟到音频结果；用专门测试权限与设备，保留原授权边界。
5. **P2 连续性能与可达性**：相同数据下录制输入、长历史阅读与任务切换，比较耗时和掉帧；原生Tab/Escape/焦点恢复与屏幕阅读器补充人工验收。当前不宣称性能或全量无障碍审计通过。原生代码驱动的 Tab/Escape 使用真实Monaco输入节点上的 DOM keydown，建议来自实际模型，未注入补全文本；这与硬件键盘/系统IME验收分开记录。

## 改动文件与启动

生产文件：`src/components/QuestionInlineCard.tsx`、`src/stores/question-store.ts`、`src/components/DocumentWorkbench.tsx`、`src/lib/document-workspace.ts`、`src/features/coding/lib/local-monaco.ts`、`src/features/coding/CodingWorkbench.tsx`、`src/features/coding/lib/plan-result.ts`、`src/styles/app.css`、`src/styles/tokens.css`、`src-tauri/src/coding/delivery.rs`。

回归/证据文件：问答组件与store测试、`DocumentWorkbench.lifecycle.test.tsx`、`document-workspace.test.ts`、`local-monaco.test.ts`；`scripts/validate-ux-matrix.mjs`、`validate-document-failure-ui.mjs`、文档及原生代码fixture、`native-coding-clarification.test.mjs`、`ui-primary-action-audit.mjs`及测试、`validate-design-system.mjs`、`build-theme-review-index.mjs`，以及本报告。没有提交、推送或发布。

项目要求 Node `^22.23.4 || ^24.22.1`，本机默认 Node20 不满足要求；本轮使用临时安装的 Node24。开发启动：

```bash
export PATH="/private/tmp/echoagent-dev-tools/node_modules/.bin:/private/tmp/node-v24.22.1-darwin-x64/bin:$PATH"
pnpm tauri dev
```

重新构建完整资源先执行项目常规构建/IDE及Office准备。此次最终 `tauri build --debug --no-bundle` 在已完成这些准备后跳过重复 beforeBuildCommand；不是发行安装包。构建后的可执行文件位于 `src-tauri/target/debug/echoagent`。此前已用正常profile启动（PID 90028），进程存活；未带评审开关，该实例于本轮结束再次只读确认存活，保持原进程运行，未强制重启；最新Plan/Rust修复由独立实例验收，不能声称已热更新进原正常实例。临时数据验证统一使用脚本的 fresh data-home，原生评审 guard 保持只允许内部生产页面，开发服务器页面被拒绝的尝试单独保留。


## 2026-11-05 后续复审修复

本节记录后续完整评审发现的三处边界修复。前文的七项问题、截图归档及测试数字仍是上一阶段快照；下面的验收不合并成虚构的单次运行结果。修改仍保留在本地，未提交、推送或发布，也没有重启普通用户 profile。

### 文档关闭与后台保存

`ToolSidePanel` 的关闭操作保留组件并设置 `hidden`，现在将明确的 `active` 状态逐层传给 `DocumentWorkbench`。关闭、切换文档或卸载都会使当前视图代数失效；旧副本、导出、剪贴板和 Agent 保存回调不能重新打开面板、显示迟到 Toast 或注入输入上下文。副本仍登记给原任务，原有 DocumentSession 保存队列继续完成。

操作锁与导航有效期分别管理：关闭后再打开时，尚未完成的操作仍保持忙碌；操作结束或失败后可解除锁，不会因视图失效而永久禁用按钮。隐藏时停止轮询和编辑，重新打开立即读取当前稿。直接组件测试和真实 ToolSidePanel 集成测试覆盖关闭后完成、关闭后重新打开再完成、失败、Word/PDF 导出、剪贴板和 Agent 上下文。

### 中断计划与用户草稿

计划结果现在区分无结果、生成中、完整完成和中断。停止、错误、输出上限、拒绝及无权威完成记录的历史回放都不能自动成为最终计划；成功结果仍通过公开答复提取器排除内部思考。结果归属以实际 Plan 提问为准，不依赖用户下一轮选择 Ask、Plan 或 Agent。

界面明确提示计划未完整生成。未被用户编辑的自动草稿会被清除，包括旧版本缓存的自动内容；用户手写草稿和旧格式用户文本保持原样，可以核对后执行。另补充了提交时序回归：中断提示与禁用旧自动计划在同一次渲染提交中生效，避免等待 effect 清理时短暂暴露可执行旧内容。该用例先复现为旧文本仍可执行，修复后通过。

### 旧任务交付语义

新增历史过期阻断仅作用于已有版本绑定的新格式检查点，沿用 `ChangeSet::verification_is_current` 对旧格式任务的兼容约定。旧任务不会仅因缺少版本字段被重新阻塞，也不会将旧记录伪装为当前版本验证通过。

真实临时 Git 仓库回归移除了历史数据中的版本字段，原代码返回 `NotSatisfied`，修复后恢复 `NotApplicable`，并验证读取报告不改写验证 JSONL。另有新格式任务持有未绑定历史的反例，以及源码变化、检查配置变化的失效回归；新格式的过期门槛仍然有效。

### 本阶段验证

- Node 24.22.1 下全量 `pnpm exec vitest run --maxWorkers=1 --no-file-parallelism`：331 文件、3743 项通过，655.78 秒。日志：`/private/tmp/echo-fix-full-vitest.log`。
- 全量运行期间补充的最终计划时序与模式边界另以三文件 36 项定向通过，不将新增用例计入上述 3743；文档与计划相关五文件定向也已通过。没有放宽超时或移除断言。
- Rust 交付模块：31 项通过，包含两项新增的旧格式兼容与新格式未绑定历史回归；该文件已运行 rustfmt。
- 最终 `pnpm build` 通过图标、TypeScript 和 Vite 检查；保留既有大于 500kB 的分包警告。日志：`/private/tmp/echo-fix-build-final.log`。
- 生产 ToolSidePanel、DocumentSession 与 TipTap 的连续关闭/保存/重新打开/Agent 交互：浅深主题 × 13px/18px，共四组通过，证据 `/private/tmp/echo-fix-document-lifecycle.V0L6dY/report.json`。
- 原有预览、段落/表格编辑、历史恢复、恢复锁、Word 同步恢复、Agent 上下文、冲突副本和 Markdown 转换通过，证据 `/private/tmp/echo-fix-document-regression.TlLRjb`。
- 保存失败、编辑稿保留及重试成功两主题通过，证据 `/private/tmp/echo-fix-document-failure.TdERMj/report.json`。
- 代码输入原有 16 组布局与交互通过；另有中断计划 8 组窄面板/字号/主题检查，确认不完整计划不可执行，用户编写后提交的文本完全一致，证据 `/private/tmp/echo-fix-coding-ui.6GiWng/plan-layouts.json`。定向截图已抽查。
- `git diff --check` 通过。`pnpm test:ui:document-workspace` 现在同时包含原有流程、失败恢复和新生命周期检查；`pnpm test:ui:coding-composer` 包含新增中断计划布局检查。

以上浏览器验收使用生产组件和隔离 IPC，不声称真实模型或原生 OS 文件对话框的连续验收。本阶段未重跑前文的真实模型完整原生旅程、Windows 实机、系统 150% 缩放和退出保护场景。
