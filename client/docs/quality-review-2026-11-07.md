# EchoAgent Desktop 质量审查与验收（2026-11-07 至 11-08）

## 质量结论

首轮、后续验收及代码复审累计记录并修复 22 项问题，包含 14 项产品问题、7 项验收工具问题和 1 项文档引用问题。此前原生核心验收已完整通过：真实 Tauri 窗口首次启动 16/16、重启 6/6，父进程独立磁盘核验通过，`nativeVerified=true`，两次均正常退出（exit 0，无信号）。取消、权限审批后实际工具执行、队列中断和恢复已取得桌面端到端证据。

核心验收追加修复了工作目录提示、接续标题恢复和重复导航清空历史；随后修复了原生工具模式与取消边界的状态判断，并维护相关验收驱动。生产构建、3868 项前端覆盖率回归、98 项 Vitest 脚本测试与 110 项 Node 脚本测试通过。上一轮 Rust 宿主 1412 项通过（9 项既有忽略）。IDE 21 项及代码工作台 7 组真实模型流程、Browser Use 10 项两阶段检查、Computer Use 文稿与 Safari 跨应用任务均已取得实机证据。其他平台和完整发布验收仍未覆盖。

11-08 追加专家团与定时任务原生验收，首次 7/7、正常退出重启 3/3 通过；实际模型派发、协调员汇总、输出后停止、暂停恢复与真正到点执行均有 UI 和物理持久化证据。本轮没有发现新的产品执行缺陷，补齐了固定隔离验收入口和防误判校验，没有修改专家团或调度器的产品逻辑。会议录音及转写按用户后续要求跳过，不能标记为通过。

随后追加 MCP、Skills、插件和连接器验收，修复本地 MCP 启动确认未绑定主窗口的问题。最终严格原生验收首次 7/7、重启 4/4 通过：真正的取消/允许、工具与服务启停、技能安装、插件能力变化、当前回合的技能读取及审批后交付、重启后新调用和移除均已核验。Rust MCP 相关 49 项、前端相关 13 项、扩展验收守卫 11 项、系统目录选择器 15 项及完整原生守卫套件通过。

本地知识库、记忆、文件检索和来源引用随后完成严格原生验收：首次 9/9、重启 4/4，两次正常退出；父进程验证通过。修复了自动标题记忆编辑后旧事实留在标题中，触发整块记忆上下文过滤的问题。116 项相关界面回归、15 项知识验收守卫、15 项目录选择器回归、完整原生守卫及生产构建通过。

后台自动记忆随后完成加强实机验收，首次 7/7、重启 4/4：普通对话自动提取、同记录更正、空闲去重、关闭/恢复、执行中退出后的待处理恢复、自动整理及全新会话召回全部通过。修复了计时器无法在重新开启后唤醒、UUIDv7 短前缀导致来源日志冲突、当前项目事实依赖检索而遗漏这 3 项产品问题。相关 Rust 22 项通过（1 项既有忽略），前端 22 项、自动记忆守卫 15 项及完整原生守卫通过。

生命周期追加修复了等待模型响应时崩溃后漏回放最后用户请求的问题，并修正了既有标题回归的异步等待竞态。相关持久化 31 项、守护 2 项 Rust 回归及生命周期 12 项守卫通过。最终原生流程通过强制退出恢复、60 分钟墙钟时长观察、10 次真实模型探测、结束后 1 次探测及正常重启后的界面恢复检查；休眠唤醒深测按用户要求跳过，不列为完整通过。本轮已按用户要求结束，隔离测试进程均已退出。

本次未提交代码复审另修复完整来源删除隔离、记忆标题与旧数据召回、Node 22/24 契约及依赖库单测门禁 4 项问题。最终前端全量 3978 项、记忆库 364 项、宿主完整回归 1416 项通过（宿主 9 项既有忽略）；清理等价 lint 写法后，受影响模块另有 48 项通过、2 项既有忽略。生产构建、116 项浏览器检查、Node 22/24 守卫、格式及 Clippy 严格检查通过。具体日志和未重跑的实机范围见文末复审补充。

## 环境、版本与数据边界

- 基线：`main`，`d684cc79e277ba41a91e859c305abf15896690df`；开始时工作区干净。已阅读项目说明、脚本、测试及 CI，未覆盖用户已有修改。
- 实测平台：macOS Intel，12 个逻辑 CPU、16 GiB 内存；Rust/Cargo 1.98.0，pnpm 10.34.5。
- 默认 Node 20.19.0、系统 Node 22.14.0 和 npm 6.14.4 不满足项目要求。生产构建使用已有 Node 24.22.1；复审补充使用官方 Node 22.23.4，下载后校验官方 SHA-256，仅放在临时目录。npm 11.21.1 也仅安装在本次临时工具目录。
- 默认客户端配置已按要求更新：`https://new-api.kunkunout.com/v1`，`claude-opus-5-5`，Bearer + Chat Completions；本地默认 ID 为 `kunkun/claude-opus-5-5`。保留其他配置，并保存修改前备份；配置及备份权限均为 0600。密钥未加入仓库。
- 除上述授权的配置修改外，测试使用合成消息和独立数据目录，没有删除或改写真实用户会话，没有发送外部通知或操作真实账户数据。没有 commit、推送、PR 或发布。
- [本轮日志与证据根目录](/private/tmp/echo-quality-20261107/)保存在临时目录；清理临时目录后链接会失效。模型私有配置不作为可分享证据。

## 架构与验证层级

React/Zustand 管理界面与任务状态，通过 Tauri 命令及事件连接 Rust 宿主。宿主经类型化 ACP Channel 连接进程内 Agent Runtime；Theia 由宿主启动为本机服务。团队、自动化、Office 等能力通过本地 MCP 服务提供。配置、会话、项目、知识和工作区分别持久化，远端模型按连接配置推理。

以下结果区分四个层级：jsdom/Node 回归；Chrome 真实渲染但模拟 IPC 的界面验收；打包前端资源启动；完整 Tauri 窗口、真实 IPC/Runtime 与模型。模拟界面通过不能代替真实权限、工具执行或重启恢复通过。

## 缺陷与修复清单

| 编号/级别 | 现象与复现 | 根因 | 修改文件及措施 | 回归结果 |
| --- | --- | --- | --- | --- |
| QA-01 / P2 | 设置中选择“字号”后立即搜索“常见问题”，新结果在点击前消失 | 上一次定位在 requestAnimationFrame 中聚焦旧目标，抢走新查询的焦点 | `src/components/SettingsPanel.tsx`：延迟定位前检查新的非空查询；`SettingsPanel.test.tsx` 新增可控动画帧回归 | 修复前用例明确失败；修复后设置/搜索 51 项单测及 4 种布局、28 个浏览器搜索目标通过 |
| QA-02 / P2 | 同时打开工作区与回溯时间线，React 报同级组件重复 key | ToolSidePanel 与 RewindBar 都使用 sessionId，组件身份不唯一 | `src/components/ChatView.tsx`：回溯 key 加 rewind 前缀；`ChatViewYield.test.tsx` 验证无重复 key 警告且关闭时间线后工作区保留 | 修复前用例捕获警告而失败；修复后工作台/回溯 90 项通过 |
| QA-03 / P3 | CHANGELOG 的历史审计链接无法打开 | 引用了不存在的 `docs/source-audit-2026-10-24.md` | `CHANGELOG.md`：改为存在的依赖安全维护文档，并准确说明用途 | README 中英文和 CHANGELOG 的本地链接目标检查通过 |
| QA-04 / P3，验收工具 | 专家团重命名后标题更新，验收脚本立即断言表单名称，偶发读到旧值 | 标题更新先于设置组件 effect 合并草稿 | `scripts/validate-group-lifecycle-ui.mjs`：有界等待表单名称更新，保留名称和未保存目标的原始断言 | 完整生命周期、宽窄窗口及亮暗主题重跑通过 |
| QA-05 / P3，验收工具 | CSS 测试错误读取复合选择器的声明，而实际长列表滚动通过 | declarationBlock 正则未锚定行首，误匹配同名类后缀 | `src/styles/__tests__/model-connections-layout.test.ts`：锚定独立选择器，保留全部原始断言 | 3/3 通过；无生产 CSS 改动 |

| QA-06 / P3 | 选择工作目录时，系统面板没有定位到当前/默认目录 | 前端未向已支持 suggestedPath 的宿主传入目录 | `WorkspacePicker.tsx` 传递当前目录或默认目录；新增三种提示参数回归 | 修复前 2 失败；修复后相关 79 项通过；实际系统目录选择和持久授权一致 |
| QA-07 / P2 | 刚创建的接续会话载入后，临时“新方向”标题被继承历史的旧标题替换 | 标题恢复没有区分继承回合与本会话首问 | Runtime 新增可选 `title_source_prompt_index`，独立于上下文压缩边界；保留旧数据兼容 | 标准库回归修复前 3 失败/3 通过，修复后 6/6；宿主集成 11/11，完整宿主 1412 项通过；真实窗口与重启临时标题正确 |
| QA-08 / P2 | 新接续会话自动加载时，再点击同一会话，历史变空并出现导航取代错误 | 两次选择共用同一原生回放，旧调用的正常导航取消被当成加载失败，删除了成功历史缓存 | `App.tsx` 使用专门的导航取代异常；接续回调保留成功历史，真实读取失败仍清理半截缓存并提供重试 | 新回归修复前失败，修复后 App 导航相关 27 项通过，包含真实加载失败的既有回归 |

| QA-09 / P3，验收工具 | IDE 隔离配置缺模型、Browser/Computer Use 假设旧目录和工具栏、电脑任务校验硬编码其他模型，阻挡真实验收 | 固定驱动未跟随现有任务目录、设置入口和显式模型契约 | 隔离模型配置；真实系统选目录并校验 PID/授权；安全设置与会话控制入口；严格实际模型绑定；失败报告保留与自有进程收尾 | IDE 21 项、代码工作台 7 组、Computer Use 独立证据已通过；Node 110 项、Vitest scripts 98 项及原生守卫通过；浏览器最终 10 项通过，见下文 |
| QA-10 / P2 | 网页任务回合结束后输入“停一下”，界面认为普通对话空闲，浏览器权限未暂停；恢复入口也可能缺失 | Runtime 编码模式通知 default 被错误用于工具模式；App 在原生取消前再次按该旧值判空闲 | `ChatView.tsx` 优先采用对应会话的原生工具状态，普通空闲提示仍由该入口处理；`App.tsx` 对已请求的控制始终执行宿主取消屏障并等待确认 | 修复前 5 项回归失败；修复后 183 项相关回归通过；真实失效现场 `browser-e2e3/first-launch.json` 保留，修复后真实 Browser 首次 7/7、重启 3/3 通过（`browser-e2e4/`）；同一最终二进制的 Computer Use 文稿复验通过 |
| QA-11 / P2，确认可靠性 | 配置 stdio MCP 时，首轮无法在客户端窗口中操作并核验启动取消 | 确认未设置 parent；当前 macOS rfd 走 CFUserNotification 系统通知路径，弹窗不属于主窗口 | `src-tauri/src/mcp.rs` 绑定实际主窗口；无主窗口拒绝；仅接受明确“允许本次”的 Custom 结果，并记录不含命令/凭据的确认结果 | 原始失败 `extensions-e2e1/` 保留；修复后原生取消不写 canonical/mirror 配置且不启动进程，允许及重新启用均有真实点击与日志；49 项 Rust 回归通过。未据原失败断言存在未经用户同意的启动 |
| QA-12 / P3，验收工具 | JSON 字段顺序导致误判；后台 ACK 先于控件恢复；目录选择器层级和技能调用假设不符；历史工具回放混入收集 | 固定脚本未充分遵循当前 UI 忙碌状态、macOS 路径输入和实际 SKILL.md 加载机制 | 完整 JSON 结构比较；等待控件启用；只操作自有 PID、每次输入并回读完整目录；按当前方式加载技能；按 promptId 排除 replay，并联合核对真实读取、审批调用 ID 和物理文件 | 各轮失败分别保存于 `extensions-e2e2/3/4/6/`；最终 `extensions-e2e8/result.json` 严格 11/11，扩展守卫 11/11，目录守卫 15/15，原生守卫全部通过 |
| QA-13 / P2 | 通过记忆面板修改内容后，新会话无法自动召回新值 | 自动生成的标题仍保留完整旧正文；旧正文已进入替换排除列表，标题使 MemoryRequestPolicy 过滤整块记忆上下文 | `PersonalMemoryPanel.tsx` 在编辑开始时识别自动标题，保存时随正文更新；自定义标题保留 | `knowledge-e2e4/` 为真实失败，`knowledge-before-fix/cause.json` 核对磁盘因果链；新增回归修复前 1 失败/18 通过，修复后相关 116 项通过；最终两阶段 13/13 通过 |
| QA-14 / P2 | 关闭记忆后重新开启，不发送新消息时后台提取不恢复；配置为 5 秒但等待 180 秒仍未执行 | `select!` 在关闭时排除两个计时器分支，配置文件变回开启不会唤醒该分支 | Runtime `run_loop.rs` 保持已配置计时器轮询，每次到点检查实时开关后再派发；关闭期间不启动提取/整理 | `memoryauto-e2e3/` 为真实失败，`memoryauto-before-fix/` 保存原二进制与状态；最终无新消息时取得新的真实后台请求完成记录 |
| QA-15 / P2 | 两个实机 UUIDv7 ID 前 8 位相同，同名自动日志互相覆盖；整理排除当前会话时也会误排除另一来源 | 日志文件名、整理排除和遗忘来源匹配仅使用 ID 前 8 位 | 新日志使用安全的完整 ID 与明确 `--sid-` 后缀；整理与遗忘共用匹配器，旧短 ID 日志保守兼容 | `memoryauto-source-identity-red.log` 修复前 1 项明确失败；修复后验证日志独立、只排除当前来源、遗忘隔离、旧格式及安全文件名；同前缀真实双会话完成最终原生验收 |
| QA-16 / P2 | 记录与整理文档已有项目复核规则，问答仍回答没有该记忆 | 未固定的项目事实未进入基础上下文，只依赖检索；该次检索为 `EmbeddingFallback`、0 结果 | 在既有 6000 字符预算内，先保留原个人/固定规则，再加入当前工作区有效项目事实；全局参考资料仍通过检索，遗忘记录排除 | `memoryauto-e2e7/` 与 `memoryauto-project-before-fix/cause.json` 保留失败；新增用例修复前失败；修复后全新会话准确返回四个当前值，Rust 22 项通过 |
| QA-17 / P2 | 等待模型首个响应时强制退出，恢复界面遗漏最后一条已接受的用户请求 | 用户 ACP 消息留在文本合并缓冲；运行时 chat_history 已落盘，但 updates 回放日志尚未落盘 | 用户消息立即按 FIFO 排空至回放日志；保留模型文本合并及失败写入重试，不自动重发请求或工具 | 修复前 `lifecycle-history-red2/` 原生失败；修复后两份日志与真实恢复界面均含中断请求；最终 60 分钟有限观察与重启界面检查通过 |
| QA-18 / P3，验收工具 | 标题取消固定的回归偶发称远端会话标题没有清空 | 测试只等待第一笔 POST 到达，却立即断言串行等待该 POST 完成后才发送的 PUT | 等待原有精确 POST 与 PUT 结果均到达；保留结果断言和 5 秒超时，不修改产品同步逻辑 | 首轮相关回归 30 通过/1 失败；修复测试等待条件后 31/31 通过 |

前两项均按“复现失败 → 最小修复 → 针对性回归 → 完整前端覆盖率回归”完成。没有通过删除断言、放宽安全边界或吞掉生产错误使测试通过。

## 功能覆盖矩阵

| 模块 | 用户操作与预期 | 方式 | 结果 | 问题/状态 | 证据（位于证据根目录，特殊目录另列） |
| --- | --- | --- | --- | --- | --- |
| 构建和资源 | 类型检查、图标一致性、生产打包和加载 | 真实构建 + Chrome 加载 dist | 通过 | 保留大 chunk 警告 | `build-final.log`、`production-final.log` |
| 模型配置 | 默认模型正确，密钥私有，真实请求可返回指定模型 | 本机配置解析 + 真实 HTTPS；普通 Tauri 会话 | 通过 | 直接探测 HTTP 200、回复 OK；Tauri 返回精确模型标记 | `model-probe.json`、`native-manual-journey.json` |
| 初次使用 | 原生窗口、首页、设置、Runtime 就绪、独立任务目录 | 真实 Tauri / IPC | 通过（核心场景） | 普通隔离窗口可创建并完成真实任务；后续严格核心 22 项通过；基础旧夹具失败记录保留 | `native-core-followup6/run-efxFrj/result.json`、`native-warm.log` |
| 多轮对话和持久化 | 首轮回复、第二轮记住前文、完成事件与落盘一致 | 普通 Tauri + 真实模型；UI 与物理文件分别核对 | 通过（限定场景） | 首轮和第二轮标记精确匹配；三个回合正常 end_turn | `native-manual-round2-ui.json`、`native-manual-journey.json` |
| 重启恢复 | 正常退出后重新启动，原会话和回复可恢复 | 普通 Tauri 真实侧边栏操作 + 历史 SHA256 核对 | 通过（限定场景） | 两轮回复重新显示，原历史字节未改变 | `native-manual-restart-ui.json`、`native-manual-journey.json` |
| 异常退出、休眠与长时间稳定性 | 真正 SIGKILL、生产守护自动恢复、请求/草稿/文件保留、60 分钟监测 | 固定原生测试 + 真实模型/权限卡 + 物理日志/文件/资源核验 | 强制退出、60 分钟有限稳定性与重启界面通过；休眠跳过 | 墙钟时长含锁屏与系统睡眠暂停；10 次期间探测、1 次结束后探测；不外推全天资源稳定性 | `lifecycle-history-red2/`、`lifecycle-e2e3/`、`lifecycle-persistence-regression2.log` |
| 取消、队列、审批与续跑 | 停止后不继续执行，审批后按原权限续跑，排队不重复 | 真实 Tauri、真实模型、UI 审批；磁盘独立核验 | 通过 | stop/send_now 均在实际输出后取消，无迟到片段；三次允许一次后真实读取；恢复及队列顺序正确 | `native-core-followup6/run-efxFrj/first-launch.json`、`first-launch-filesystem.json` |
| 会话/项目/回溯 | 查找、重命名、分叉、归档恢复、工作区与项目切换 | 核心实机 UI、Runtime 和物理文件；另有浏览器回归 | 通过（已测核心场景） | QA-02/07/08；接续历史、临时标题、原文件哈希、项目上下文和重启恢复通过；非所有跨工作区场景 | `native-core-followup6/run-efxFrj/`、`continuation-ui.log` |
| 设置、模型编辑与可访问性 | 连续搜索、IME、键盘、焦点、草稿保护、连接保存和失败反馈 | Chrome 模拟 IPC + jsdom | 通过 | QA-01；全部 7 个设置验收脚本通过 | `settings-after.log`、`search-race-before.log` |
| 通知、安全、高级与帮助 | 渠道和隐私配置、错误恢复、记录过滤、常用控件 | Chrome 模拟 IPC | 通过（界面层） | 306 次布局或流程观察，指标不等于独立业务用例数 | `settings-after.log` |
| 本地知识与检索 | 系统选目录、重复添加、MD/TXT/DOCX 搜索、更新索引、移除及重启 | 真实 Tauri UI + 本地 BGE 混合检索/重排 + 独立文件核验 | 通过（本轮已测场景） | 实际 `hybrid-reranked`；移除源不再出现在搜索中，原文件保留 | `knowledge-e2e9/`、既有 `retrieval-offline.log` |
| 个人记忆与来源引用 | 新增、编辑、忘记；新会话召回；来源编号/路径/行号/原文打开；重启恢复 | 真实 UI + 指定模型 + Typora AXDocument + 磁盘记录 | 通过（本轮已测场景） | QA-13 已修复；忘记后新会话回答未知，独立记忆仍可召回；历史引用保留 | `knowledge-e2e9/`、`knowledge-memory-red.log`、`knowledge-ui-regression2.log` |
| 后台自动记忆提取与整理 | 普通对话提取、更新去重、开关恢复、同前缀来源、退出恢复、后台整理、新任务召回 | 真实 Tauri/模型/后台计时器 + 来源与待处理/文档物理核验 | 通过（加强 11 项） | QA-14/15/16 已修复；没有通过手动提取/整理或直接写记忆制造结果 | `memoryauto-e2e8/`、`memoryauto-rust-memory-final2.log` |
| Browser Use | 私有网络审批、表单提交与下载、拒绝删除、自然暂停续做、清理及重启 | 真实 Tauri + 真实模型/Chrome；服务器请求与文件字节独立核验 | 通过（10 项） | QA-10 已修复；仅本机合成站点，无真实登录或远端业务站点 | `browser-e2e4/` |
| Computer Use | 新建文稿、精确输入/读回、暂停与续做、保存、Safari 文件选择 | 真实模型 + macOS Native MCP；实际文件与 Native journal 独立核验 | 通过（已测场景） | 36 次 Native 调用的跨应用任务；最终构建另 9 次调用复验文稿；不等于任意应用全覆盖 | `computer-e2e1/verification.json`、`computer-e2e2/verification.json` |
| Office 交付 | DOCX/PDF/XLSX/PPTX 实际文件，中文、公式、跨表、图表和页数正确 | 打包 Worker 真执行 + 独立解析 | 通过 | 非法输入和内容保真也已检查 | `office.log` |
| 团队/专家团 | 创建、成员路由、派发、汇总、停止恢复与持久化；其他管理操作 | 真实 Tauri + 指定模型；其他管理分支为 Chrome 模拟 IPC | 通过（限定场景） | 两位专家同一显式连接并行生成，协调员实际汇总；跨提供商组合及复杂工具协作未实测 | `collaboration-e2e1/result.json`、`domain-ui-results.json`、`group-lifecycle-after.log` |
| MCP / 连接器 | 配置校验、原生执行确认、发现、工具开关、服务启停、审批后真实调用、重启及删除 | 真实 Tauri + Runtime + stdio MCP 服务 + 指定模型；父进程独立核验 | 通过（合成服务） | 取消后无进程、无配置落盘；允许后真实 initialize/tools/list；两次调用各审批一次、各生成一个文件；最终 canonical 与镜像均删除 | `extensions-e2e8/result.json`、两份阶段报告及源 profile 的协议审计 |
| Skills | 原生选目录、检查、受管安装、停启、实际加载、重启后重新加载及卸载 | 真实 Tauri / 本地安装器 / 模型 read_file | 通过（提示词技能） | 两个阶段分别取得本轮 SKILL.md 的实际读取输出，排除历史回放；受管副本最终不存在，源包不变 | `extensions-e2e8/first-launch.json`、`restart.json`、父进程 SHA256 核验 |
| 插件 | 配置目录加载、停启及贡献技能变化、真实模型使用、重启与移除配置 | 真实 Tauri + 本地插件 + 指定模型 | 通过（本地配置插件） | 停用后贡献技能消失，启用后恢复；移除配置后插件与贡献技能均消失，原始文件保留；未把预置插件源表述为界面安装 | `extensions-e2e8/result.json`、`first-launch.json`、`restart.json` |
| IDE、Git、Diff、终端 | 编辑、搜索、终端、SCM、Diff、工作树、断线恢复、代码任务 | 真实 Tauri/Theia 两阶段；真实模型与独立文件检查；另有 Node 回归 | 通过（本次场景） | 21 项 IDE 检查与 7 组代码任务通过；外部插件全场景及复杂工程未测 | `ide-e2e2/`、`coding-workspace-e2e1.log` |
| 原生桌面权限与环境 | 显示器、未锁屏会话、macOS AX 后端和浏览器可用性 | 真实只读 host_probe + 模块测试 | 通过（仅探测） | 132 项模块测试、额外 2 项实际诊断；不代表真实桌面输入成功 | `desktop-native-final.log`、`desktop-probes.log` |
| 主题、字号与视觉 | 宽窄窗口、亮暗主题、字体、搜索结果与表单状态 | Chrome 测量/截图；活动 WKWebView 表单测试 | 自动部分通过 | 78 条原生 Select 人工复核项未全部逐一验收 | `product.log`、`contrast.log`、`webkit.log` |
| 定时任务 | 界面创建未来单次计划、实际到点执行、结果显示、正常退出与重启防重复 | 真实 Tauri + 本机调度器 + 指定模型，父进程独立核对磁盘 | 通过（限定场景） | scheduledFor 精确匹配未来计划时间；执行后计划结束，重启仍只有一条成功记录；无手动运行、时钟覆盖、外部推送 | `collaboration-e2e1/result.json`、`collaboration-e2e1/scheduled-completed.png` |
| 会议、组织、云服务 | 录音/转写、真实账户授权、网络中断恢复 | 未运行真实外部流程 | 未验证 | 会议录音及转写按用户要求跳过；真实外部账户流程未验收 | 测试边界 |

## 用户体验与产品评估

| 页面/功能 | 当前行为 → 用户场景 → 影响 | 建议与优先级 | 处理 |
| --- | --- | --- | --- |
| 连续设置搜索 | 旧定位抢新查询焦点 → 高频用户快速切换设置 → 结果消失、操作被打断 | 新查询优先，保留输入及结果；P2 | 已修复，单元与浏览器复验通过 |
| 回溯与工作区 | 同级组件 key 冲突 → 开发者同时检查文件与历史 → 组件身份存在歧义 | 保留按会话重置，分别命名 key；P2 | 已修复；未声称已观察到实际数据丢失 |
| 默认独立任务目录 | 首页明确提示“创建时自动分配独立文件夹” → 新任务自动隔离文件 → 实际 cwd 与固定根目录不同 | 保留产品默认；验收应匹配实际用户选择与目录授权，不应改产品来满足旧夹具；P3（验收维护） | 普通实机创建验证通过；严格核心已通过真实系统选择器显式选定私有根目录，保持精确授权断言 |
| 初次任务反馈 | 初轮高负载原生截图停留“正在创建任务…” → 用户难以判断等待时间 | 若低负载实机仍重复长等待，再增加阶段反馈/重试入口；P2 待观察 | 普通窗口后续实际任务成功；现有证据不足以认定创建逻辑缺陷，未贸然改写协议 |
| 可发现性与设置结构 | 首页提供任务入口，设置提供统一搜索，连接管理与默认模型分层 | 保留现有结构，以搜索、焦点和草稿保护回归防止退化；P3 | 已完成界面检查，无大范围重设计 |
| 文档追溯 | 历史审计链接缺失 → 维护者查验安全依据 → 引用不可用 | 使用存在且含义准确的维护文档；P3 | 已修复 |

## 命令与执行结果

以下记录保留首轮失败与最终复验的区别；不同测试层级的计数没有合并成“全部业务通过”。

| 实际命令/执行方式 | 结果 | 关键说明与日志 |
| --- | --- | --- |
| `pnpm build` | 通过 | 最终 Node 24：tsc、图标检查、4923 模块打包，约 1m56s；`build-final.log` |
| `pnpm test` | 首轮 3939/3942 | 2 项 HTML 动态冷加载 5 秒超时、1 项 CSS 测试解析问题；HTML 26/26 低并发复验通过；CSS 问题已修复。未声称原完整命令同批重跑全绿 |
| `AUDIT_COVERAGE_DIR=… pnpm test:coverage --maxWorkers=1` | 最终 3868/3868 通过 | 326 文件；语句 80.38%、分支 75.46%、函数 76.67%、行 84.22%，包含未导入的生产模块；`coverage-desktop-final.log`、`coverage-desktop-final/coverage-summary.json` |
| `pnpm exec vitest run --project scripts` | 最终 98/98 通过 | 15 文件，包含 13 项固定系统目录交接守卫；`desktop-e2e-vitest-final.log` |
| `pnpm test:scripts` | 最终 110 项通过 | 与 Vitest scripts project 为不同运行器；Node 24，回环监听需沙箱外运行；`desktop-e2e-node-final.log` |
| `pnpm test:ui` | 首次链式运行失败，分项补验 | 检索 28 布局通过；图标初载超时后独立通过。9 组领域验收逐项执行，专家团生命周期修复后通过；未声称原链式命令同批全绿 |
| `pnpm test:ui:settings` | 全部 7 脚本通过 | 导航 28、通知 37、控件 116、模型 45、安全 22、高级 18、帮助 40 次布局或流程观察；`settings-after.log` |
| `pnpm test:ui:product-experience` | 通过 | 4 组窗口/主题/字号，28 张截图，无 pageerror；`product.log` |
| `pnpm test:ui:contrast` | 自动部分通过 | 71 类界面 × 10 配置，1590 次观察、40658 条文字测量、0 自动失败；78 条原生 Select 待人工复核；`contrast.log` |
| `node scripts/validate-color-webkit.mjs --active` | 通过 | 真实活动 WKWebView，4 配置，Checkbox/Radio/Select、主题切换和可见焦点；不替代逐项复核全部 78 条；`webkit.log` |
| `pnpm test:production-startup` | 通过 | 最终 dist 重测；浏览器中 2 个预期 native IPC 错误，未伪称桌面通信成功；`production-final.log` |
| `pnpm test:task-recovery` | 通过 | 真实浏览器输入与下载核验，另有模拟 IPC 恢复 UI；`recovery.log` |
| `pnpm office:test` | 通过 | 四格式实际文件与结构化结果独立解析；`office.log` |
| `pnpm test:security` | 30/30 通过 | 使用符合要求的 Node/npm、降低并发；首次超时记录保留；`security-final.log`。不等同于完整在线 CVE 审计 |
| `pnpm test:ide` | 70/70 通过 | VSIX、SCM、Git、编辑桥接等；`ide.log` |
| `pnpm test:rust` | 最终 1412 passed、9 ignored、0 failed | 17 个签名宿主测试程序，后续编译 10m56s；接续集成 11/11（含新增 6 项规则及实际复制/压缩场景）；每个 harness 仅计最终摘要，排除嵌套子进程重复统计；`rust-followup-final.log`、`rust-followup-summary.json` |
| 固定 Runtime 无模型冒烟 | 5 场景通过 | 显式个人模型、Auto 无偏好、Auto 空偏好、退役模型过滤、常驻 Skill 刷新；`runtime-smoke.log` |
| 固定检索离线验收 | 2 项通过 | 内置资源首次推理 30.81s、离线导入后推理 104.19s；`retrieval-offline.log` |
| `CARGO_BUILD_JOBS=1 pnpm test:desktop-native` | 132 passed、2 ignored | 原生适配模块，无真实输入派发；`desktop-native-final.log` |
| `cargo test --locked --manifest-path scripts/desktop-native-check/Cargo.toml host_probe:: -- --ignored --test-threads=1` | 2/2 通过 | 额外真实只读诊断，3.57s；`desktop-probes.log` |
| `pnpm test:native-guards` | 通过 | 最终包含 37 个核心守卫、38 个 coding guard 场景及其余既有守卫；合成报告不能替代真实原生任务；`desktop-e2e-native-guards-final.log` |
| `cargo fmt --manifest-path src-tauri/Cargo.toml --check` | 失败（基线） | 5 个 Rust 文件、34 处格式差异；未进行无关批量格式改动；`fmt.log` |
| `cargo build --locked --manifest-path src-tauri/Cargo.toml --features tauri/custom-protocol --bin echoagent -j2` | 通过 | 用于真实内嵌页面原生运行；未放宽开发服务器页面拒绝规则；`native-build.log` |
| `node scripts/validate-native-review.mjs src-tauri/target/debug/echoagent` | 自动验收失败 | 冷启动/预热两次；模型就绪、首页与设置等部分通过，整体未通过；`native.log`、`native-warm.log` |
| `node scripts/validate-native-core-review.mjs src-tauri/target/debug/echoagent` | 最终通过 | 首次 16/16、重启 6/6；`nativeVerified=true`、`osPickerVerified=true`、两次 exit 0；`native-core-followup6/run-efxFrj/result.json` |
| 后续前端生产构建与导航回归 | 通过 | `build-native-core-followup10.log`；App 导航 27 项、WorkspacePicker 11 项（相关工作区组合 79 项）；失败前后记录 `fork-navigation-before.log`、`fork-navigation-after.log` |
| `node scripts/validate-native-ide-git-review.mjs …` | 21/21 通过 | 首次 17、重启 4；真实无远端临时 Git 仓库与 Theia；`ide-e2e2/result.json` |
| `node scripts/validate-native-coding-workspace.mjs …` | 7 组通过 | 真实 Ask/Plan/Agent、两项原生检查、Diff、项目切换与 Tab；`coding-workspace-e2e1.log` 记录实际输出目录 |
| `node scripts/validate-native-browser-review.mjs …` | 10/10 通过 | 首次 7、重启 3；父进程独立文件/请求证明；`browser-e2e4/result.json` |
| `node scripts/validate-native-desktop-controls.mjs … --universal-flow` + 独立 verifier | 通过 | 原生文稿与 Safari 跨应用；`computer-e2e1/verification.json` |
| 最终构建 `… --task-flow` + 独立 verifier | 通过 | 9 次 Native 调用；字节、原生 URL、暂停静默与同任务恢复；`computer-e2e2/verification.json` |
| QA-10 针对性回归与最终构建 | 通过 | 修复前 5 失败；修复后 183 项通过；`browser-native-mode-before.log`、`browser-native-mode-after.log`、`build-desktop-mode-fix.log`、`native-build-desktop-e2e4.log` |
| `node scripts/validate-native-collaboration-review.mjs …` | 10/10 通过 | 首次 7、重启 3；真实专家团、单次计划与物理数据库/记录核验，两次 exit 0；`collaboration-e2e1/result.json` |
| 本轮原生构建及验收守卫 | 通过 | `native-build-collaboration2.log`；新增防误判 12/12、系统目录选择器 14/14，完整 `pnpm test:native-guards` 通过；`collaboration-guard-tests.log`、`collaboration-picker-tests.log`、`collaboration-native-guards.log` |
| `node scripts/validate-native-extensions-review.mjs …` | 严格 11/11 通过 | 最终首次 7、重启 4；实际技能读取、真实 MCP、审批及物理文件/进程/配置核验；`extensions-e2e8/result.json`；同一二进制 `extensions-e2e7/` 亦通过 |
| MCP 修复回归与构建 | 通过 | `cargo test … --lib mcp::tests --features tauri/custom-protocol -j2`：49 通过，包含显式确认结果守卫；相关前端 13 通过；`extensions-rust-mcp-tests.log`、`extensions-ui-regression.log`、`native-build-extensions7.log` |
| 扩展验收与路径守卫 | 通过 | 新增 Node 11/11、目录选择 15/15、完整 `pnpm test:native-guards` 通过；`extensions-guard-tests-final.log`、`extensions-picker-tests.log`、`extensions-native-guards-final.log`；`mcp.rs` 与新增扩展验收模块格式检查通过，整体格式检查的既有边界仍保留 |

Runtime 冒烟及离线检索采用原固定场景脚本的临时副本，仅替换重复编译步骤，复用刚编译且验证签名的宿主测试程序。环境隔离、用例过滤、零测试拒绝和原断言均保留。原脚本与二进制 SHA256、精确变换见 `prebuilt-runtime-provenance.json`；未把它们表述为原 pnpm 命令原样执行。

## 真实桌面验收与失败界限

最终严格核心验收使用当前源码构建的内嵌页面、用户指定的真实模型、真实 Tauri IPC 和 Runtime。驱动核对二进制与固定夹具 SHA256，使用独立 owner-only 数据目录，通过系统文件夹选择器选择精确私有工作区，核对持久目录授权。使用真实 IPC、会话状态、工具结果和模型回复。前端操作由固定驱动执行，真实系统选择器由 macOS 辅助功能确认，未切换用户输入法。

最终证据目录为 [native-core-followup6/run-efxFrj](/private/tmp/echo-quality-20261107/native-core-followup6/run-efxFrj/)。`result.json` 记录首次 16/16、重启 6/6、两次 exit 0/无信号、无错误、`passed=true`、`nativeVerified=true` 和 `osPickerVerified=true`。检查 ID 中保留的 qwen 字样是历史标识，本次实际模型均按私有配置绑定 `claude-opus-5-5`，并核对实际模型 ID。 已保留实际通过验收的二进制 `/private/tmp/echo-quality-20261107/native-tested/echoagent`，SHA256 与 `result.json` 一致；`native-tested/provenance.json` 记录出处，避免宿主回归重新链接默认产物后混淆证据。

- **取消和队列**：在真实流式输出至少 32 字符后点击停止或立即发送；磁盘分别记录 99 和 107 个输出字符，再出现相同回合的 cancelled ACK。取消后无该回合迟到非空输出；停止后续聊完成，立即发送先于原排队消息，均无重复。见 `first-launch.json` 与 `first-launch-filesystem.json`，重启后再次从原始 journal 核对。
- **权限审批与工具执行**：测试私有配置仅为三份合成文件增加 Read ask 规则，保留 Ask 模式。三个真实权限卡片均点击“允许一次”，审批前相应工具未完成；审批 ID、工具调用 ID、精确文件路径和真实完成输出互相匹配。XLSX 保留 20 位文本编号，EPUB 按 spine 正文顺序且排除导航，ODS 保留空列及重复单元/行。源文件 SHA256 在执行和重启后均不变。见 `first-launch.json`、两份 `*-filesystem.json`。
- **接续、项目与持久化**：实际重命名和内容搜索，接续后显示继承历史与临时标题；接续操作前后原会话 history/updates SHA256 均不变。项目接续继承上下文且仅消费一次，原项目计划/任务不复制；归档恢复、仅删除自有会话通过。重启后源会话、只读接续与项目接续的 chat_history SHA256 均与退出前一致。见 `fork-physical-proof.json`、`restart.json` 及两份文件系统证据。
- **其他核心检查**：真实 task 子代理按固定请求返回指纹，子代理 journal 无任何工具调用；工作区“任务 → 运行”显示完成。个人模型连接在设置中真实测试、保存并实际回复；自动化仅编辑草稿，离开页面恢复、取消退出、舍弃草稿均通过，磁盘没有创建定时任务。

首次失败与最终通过分开保留。早期夹具仍假设默认任务直接位于固定根目录，并引用旧队列、分叉、归档和面板入口；现已通过真实当前 UI 选择工作区并更新入口。followup3/4 实机暴露接续标题和重复导航清空历史两个产品问题，按回归复现后修复。followup5 的 22 项 UI 均通过但父进程因取消 ACK 字段读取错误拒绝；修正为流式 `_meta.promptId` 与完成 `update.prompt_id` 联合匹配后，先独立复查旧证据，再完整重跑 followup6，通过全部原始及加严断言。

followup2 中模型曾请求额外终端工具，被固定范围守卫拒绝且未执行；没有扩大工具授权来绕过失败。这次拒绝通过 Native 清理接口完成，不能作为“用户点击拒绝按钮”的实机验收。此前普通隔离窗口的多轮记忆与正常退出恢复也已通过，证据 `native-manual-journey.json` 保留。

## IDE、Browser Use 与 Computer Use 追加实机验收

追加任务保持同一工作区与既有修改，不提交或推送项目。使用真实 Tauri 内嵌页面、真实 Theia 服务/编辑器、实际模型与本机应用；Git 仅在没有远端的合成临时仓库内执行，个人 Git 配置与原项目历史未改写。不同轮次的二进制 SHA256、配置和原始失败分别保存。

- **IDE Git/编辑器**：[ide-e2e2/result.json](/private/tmp/echo-quality-20261107/ide-e2e2/result.json) 首次 17 项、重启 4 项通过。真实编辑、搜索、终端进程及输出文件、SCM Diff/暂存/提交、HTTP 预览、未保存离开、后端断线与恢复、隔离工作树和项目切换均通过；重启前后物理状态一致，原任务仍存在。此组只配置模型连接供生产就绪检查，不执行模型任务。
- **代码工作台真实模型**：[实际结果](/private/var/folders/yj/cmk3vp190sdb502fg81sryxw0000gn/T/echo-native-workspace-evidence-eTqSVc/result.json) 的 7 组检查通过。Ask 接收未保存缓冲区、选区和版本；用户编辑后的完整计划进入实际 Agent 请求，Runtime 真正修改 `double.cjs`，两个原生检查及独立 Node 检查通过，Theia Diff、用户后续编辑使旧验证失效、两个项目隔离与返回、真实 Tab 展示/接受/Undo/丢弃通过。实际 Theia HTTP bundle 哈希也与已构建资源匹配。
- **Computer Use 文稿与跨应用**：[独立核验](/private/tmp/echo-quality-20261107/computer-e2e1/verification.json) 通过，36 个实际 Native MCP 工具调用。TextEdit 明确新建文稿，空编辑器精确输入并原生读回；“停一下”撤销授权并静默 5 秒，“继续保存”在相同任务恢复；系统保存面板实际生成 69 字节文件，内容及末尾换行精确匹配，原生文档 URL 与目标文件一致。Safari 新标签页原生输入、更新结果和系统文件选择完成，真实页面标记 `ECHO-CROSS-PASS-1V4he9` 可由原生结果核对。保存和 Safari 两轮模型自然结束（另有一次真实用户暂停取消），重复审批 0、Runtime 权限请求 0；没有 Shell/文件写工具/CDP 替代操作。跨应用段耗时 718.253 秒，包含模型处理截图与参数错误后的恢复，不能将所有调用表述为一次成功。 [最终构建文稿复验](/private/tmp/echo-quality-20261107/computer-e2e2/verification.json) 也通过：9 次真实 Native 调用、相同的精确文件与暂停/续做断言、模型自然结束；源二进制 SHA256 与 Browser 最终验收一致，前一份合成文稿哈希不变。两个电脑测试客户端在资格核验结束后被单独终止清理，这不是客户端正常退出/重启证据。
- **Browser Use**：[最终独立结果](/private/tmp/echo-quality-20261107/browser-e2e4/result.json) 首次 7/7、重启 3/3 全部通过，`nativeVerified=true`。真实 Chrome 对本机站点各提交/下载一次，下载字节哈希匹配；一次真实 UI 拒绝后，删除请求为 0。自然暂停真正锁住 Native 状态，继续后有真实 browser_snapshot；清理确认的取消保留数据，确认后实际移除 profile；重启没有复活浏览器数据或内网授权，同一模型重选与只读回复通过。两次 exit 0、无信号，本机服务器已关闭。

Browser/Computer Use 的固定目录均通过真正系统选择器选定，驱动核对自有 PID、精确目录与持久授权。浏览器授权在“设置 → 安全与权限 → 任务浏览器”完成，数据清理和会话暂停均使用现有产品入口。首次网络错误保留为单次观察；独立探测中代理与直连均 HTTP 200，未把它归因为确定的代理或远端故障。验收环境保留调用方外部传输配置，回环请求使用 NO_PROXY。

## 专家团与定时任务追加实机验收（11-08）

证据为 [collaboration-e2e1/result.json](/private/tmp/echo-quality-20261107/collaboration-e2e1/result.json)，首次进程 PID 69808、重启 PID 70171 均正常退出（exit 0，无信号）。实际执行二进制 SHA256 为 `947bc59dd4a0113666b07208e14449d0001c953e7d4a039b9b3dc418c9b49812`，固定夹具 SHA256 为 `cac28c10ebf2163d81c48648bc76edba79dd9bcc6cc0eadd45294395295405f6`；通过验收的可执行文件已冻结在 `/private/tmp/echo-quality-20261107/collaboration-tested/echoagent`，并再次核对哈希。父进程启动前逐字核对当前夹具已嵌入二进制；原生入口只接受当前内部主窗口、owner-only `/private/tmp/echo-native-review-collaboration-*` 目录、固定清单及明确绑定的模型。

- **专家团**：以合成定义作为本地专家输入，经真实界面选择两位专家、创建群、选择“只讨论”和“@全体”。两条独立会话均实际生成带本轮 nonce 的回答，协调员另行生成汇总；记录有真实 token 用量。随后另起长任务，实际流出 158 字符后点击“停止本次任务”，原生工作状态为 cancelled，3.5 秒静默前后输出不变；从“查看工作”暂停后续派发并恢复，再另起协调员汇总任务，实际产生新的回复。退出后数据库保留 5 项工作（4 completed、1 cancelled）和 9 条消息；重启显示最后回复，等待后没有新增派发。
- **定时任务**：经实际名称/要求、单次日期/时间、高级模型设置和创建按钮保存 ACTIVE 任务，使用隔离目录和指定模型，保留默认权限、关闭外部推送。计划时间为 `2026-11-08 00:42:00 +08:00`；没有调用 `automations_run`、调度 tick 或覆盖时钟。真实后台在 `00:42:02.608350` 创建带相同 scheduledFor 的记录，`00:42:09.458616` 完成；实际回复带本轮标记，工具调用数为 0。界面显示“已完成 · 定时触发”、精确计划时间、实际回复及一条运行历史，已查看 [完成截图](/private/tmp/echo-quality-20261107/collaboration-e2e1/scheduled-completed.png)。单次计划自动结束；静默观察及重启后仍只有同一条成功记录。
- **独立核验**：首次和重启正常退出后，父进程以只读连接检查实际 `collaboration.sqlite` 的群、成员工作和消息，并读取实际自动化文件核对任务、记录、工作区、模型、sessionId 与 promptId。没有写入虚构回复、替换 IPC 或修改产品存储来制造成功。原生目录选择器授权也由父进程独立核对。

原有 `native-automation-review.js` 仅验证 PAUSED 任务的手动运行与恢复，本轮新增固定驱动补齐真正到点触发，保留旧脚本的原始边界。新增防误判回归拒绝错误 PID、旧报告、单专家伪装并行、输出前停止、手动运行伪装调度、缺少 scheduledFor、提前运行、错误模型及缺少输出标记。原生实际流程首次即通过，没有通过削弱断言或扩大权限取得通过。

会议录音及转写在用户明确要求后停止安排，没有申请麦克风/系统录音权限或采集音频。日志保留未配置搜索及初始记忆/隐私过滤的警告，本次不把专家团文字任务通过扩大为搜索和长期记忆能力的完整验收。

## MCP、Skills、插件与连接器追加实机验收（11-08）

最终结果为 [extensions-e2e8/result.json](/private/tmp/echo-quality-20261107/extensions-e2e8/result.json)：首次 PID 77596、重启 PID 77783，7/7 + 4/4 全部通过，`nativeVerified=true`、`osPickerVerified=true`，两次 exit 0/无信号。使用同一指定 Claude 连接、真实生产 UI、Tauri IPC、Runtime 和本地 stdio MCP 协议；服务只接受本轮 nonce 与 first/restart 阶段，只能写固定的合成文件。没有替换 IPC、注入模型回复或直接改产品存储制造成功。

实际二进制 SHA256 为 `cca7bf267e4682099f2d5533689fe305795b0913d7a103ac3b560ed97b8e3fbf`，固定夹具 SHA256 为 `f55cb7f81cda3397f5aaa999378be763d4e932b0b29290157a0a6ec2937bf192`。已冻结 `/private/tmp/echo-quality-20261107/extensions-tested/echoagent`。启动前校验夹具确实嵌入二进制，入口只接受内部主窗口、专用私有 home、固定清单及明确模型；外部凭据环境被清理，OS HOME 保持原值。

- **原生启动确认**：真实编辑器输入无效 JSON，配置结构不变。随后新增 stdio 配置，父进程只在自有 PID、精确 Node 命令、服务脚本和 nonce 匹配的确认框点击“取消”；在允许前独立核对没有 `mcp.json`、canonical 中没有此服务、没有 MCP 进程审计文件。再次保存并点击“允许本次”，随后才有真实进程启动、initialize 和 tools/list。停用再启用时再次明确允许。宿主日志记录 requested/resolved，结果依次为 false、true、true；没有把旧的系统通知式确认失败描述为确定的授权绕过。
- **连接器与工具管理**：实际展开服务，停用并恢复单个工具，实际停用并重新启用整个服务，分别等待原生状态和 UI 控件就绪；真实服务恢复 ready，两个工具可见。删除通过真实菜单和确认弹窗，最终 session 中不再有服务，canonical TOML 与 `mcp.json` 镜像均无该服务。
- **技能安装与插件加载**：用实际系统目录选择器选择完整私有路径，读取输入值核对后进入；检查并安装本地提示词技能，真实受管副本与源文件字节一致。启停技能，启停本地配置插件并核对其贡献技能消失/恢复。插件输入目录和配置是隔离测试准备，插件页面本身没有本地安装按钮，本次验证加载/启停/移除配置，不声称验证了插件安装 UI。
- **当前回合真实执行**：首次和重启后均明确要求重新读取两个技能当前版本，取得精确 SKILL.md 路径、completed 输出及其中的私有合成标记；只接受相同 sessionId、当前 completion 的 promptId 且非 isReplay 的事件。随后实际 search_tool/use_tool 调用 MCP，两个阶段各出现一次真实“允许一次”，批准的 toolCallId 与本轮 UseTool 完成事件一致；没有终端或文件写工具替代交付，也没有读取其他文件。
- **物理产物与收尾**：协议审计每个阶段恰好一次 echo_deliver，进程 PPID 对应真实客户端；生成 first 70 字节、restart 72 字节文件，SHA256 分别为 `552c5d675b7b4f63cb056b4bd1ec74774101c23077bc6edcca74093e7511f3fe`、`252a75136f9923acd1504abde73574e6d607074509f60239158b04ea36e495aa`。重启保留首份产物，重新加载后生成第二份；随后从 UI 卸载受管技能、移除插件路径和删除连接器。受管目录实际不存在，五份源文件哈希均不变，两次客户端退出后所有自有 MCP 进程均不存在。

初版 `extensions-e2e5/` 按当时断言通过；补截图复跑 `extensions-e2e6/` 中模型拒绝了“不许读文件却要求 Skill 工具”的矛盾提示，进一步核实当前加载机制是读取 SKILL.md。现已修正提示，并加严为当前回合真实读取及审批调用联合验证；同一最终二进制 `extensions-e2e7/` 严格通过，父进程完整路径输入守卫收尾后再次完整运行 e2e8 通过。没有把历史回放当作重启后新执行。

已查看两张自有窗口截图：[首次交付时](/private/tmp/echo-quality-20261107/extensions-e2e8/first-launch-native-delivery.png)、[重启阶段](/private/tmp/echo-quality-20261107/extensions-e2e8/restart-native-delivery.png)。截图在物理文件出现时采集，可能仍显示运行中或历史滚动位置；新回合成功以当前回合工具事件、审批和物理文件联合证明，不能单凭截图判定。

边界：本次为合成 stdio 服务、提示词/工作流 Skill 和本地配置插件；外部 OAuth/真实账户、其他传输与认证组合、远端插件市场安装/升级/签名和可执行 Skill 的所有运行契约未在本轮逐一实机覆盖。会议录音继续按用户要求跳过；未发送真实业务消息或操作外部账户。

## 本地知识库、记忆与来源引用原生补充验收（11-08）

最终证据为 [knowledge-e2e9/result.json](/private/tmp/echo-quality-20261107/knowledge-e2e9/result.json)：`passed=true`、`nativeVerified=true`、`osPickerVerified=true`。首次 PID 84441、9/9，重启 PID 84590、4/4；分别约 40.2 秒与 16.6 秒，均 exit 0、signal null。使用指定 `claude-opus-5-5`，没有替换 IPC、模型输出或 Runtime 工具；问答没有执行额外工具，也没有未处理的审批请求。

| 流程 | 实际验证 |
| --- | --- |
| 本地来源维护 | 三次真实系统目录选择：添加两个合成来源，再次选择同一目录不重复添加；原生授权目录与持久来源目录相符 |
| 文件检索 | Markdown、TXT、有效 DOCX 中的独立验收码经真实本地提取、向量检索和重排返回；首次完整索引 4 文件/4 已嵌入分块；扫描期间的正常关键词回退没有冒充语义通过 |
| 更新与移除 | 修改 Markdown 后刷新，搜索返回新内容、不返回旧验收码；移除另一来源后立即搜索及重启搜索均不返回该来源；原文件和其他输入的字节核验通过 |
| 来源引用 | 模型真实回答 650 元及资料中隐藏的校验码；数字引用指向确实包含该事实的条目；当前 promptId 的来源路径、行号、片段与原文件对应；点击真实来源按钮后，Typora 的 AXDocument 精确匹配原文件完整路径 |
| 记忆修改 | 在 UI 新增固定的个人记忆，首个真实模型任务召回；UI 编辑正文后全新会话返回新验收码、不返回旧码；自动标题更新，自定义标题回归保留 |
| 忘记与恢复 | UI 确认忘记并保留另一条独立记录；重启后忘记状态和独立记录均保留；新会话只召回独立记录，对已忘记内容回答“未知”，不输出新旧被忘记值 |
| 历史与新任务 | 重启载入原会话，真实历史回答和完整来源记录恢复；新的自动任务位于私有工作区 `Tasks/UUID`，重新检索且生成新的完成/来源记录 |

父进程分别独立读取来源目录清单、输入文件 SHA-256、原 promptId 的持久来源 JSON、记忆 `records.json` 与活动 `REMEMBERED.md`，确认被忘记值不在活动投影中。SQLite 文件按照 `personal-knowledge/active.json` 的当前代次定位并核验；不会将旧代次路径不存在误记成产品失败。来源中的合成恶意指令没有使最终回答输出其完整诱导标记。

QA-13 的原始复现在 `knowledge-e2e4/`，旧记录的标题等于已替换正文，活动投影同时含旧标题和新正文，见 [因果核对](/private/tmp/echo-quality-20261107/knowledge-before-fix/cause.json)。旧版本模型明确没有召回新值；最小修复后，全新 nonce 与真实会话多次返回新值。新增组件用例修复前失败（其余 18 项通过）；修复后相关 8 文件 116 项通过，生产构建和原生构建通过。15 项知识验收守卫、15 项系统目录选择器回归及完整 `test:native-guards` 通过。日志分别为 `knowledge-memory-red.log`、`knowledge-ui-regression2.log`、`knowledge-production-build.log`、`native-build-knowledge12.log`、`knowledge-guard-tests-final.log`、`knowledge-picker-regression.log`、`knowledge-native-guards-final.log`。

验收迭代的原始失败保留在 `knowledge-e2e1/` 至 `knowledge-e2e8/`。其中菜单定位、索引后台就绪、展开执行过程、当前索引代次、历史消息先回放后建立可信绑定及自动任务目录的假设均在固定脚本中纠正；没有为通过检查扩大原生权限，只有本次私有工作区及其直接 `Tasks/UUID` 目录可作为任务证据。

验证二进制冻结在 [knowledge-tested/echoagent](/private/tmp/echo-quality-20261107/knowledge-tested/echoagent)，SHA-256 `4734aae1c9ee6e043303b49004415b3758214fdad3088f885e868db1d9a81401`，夹具 SHA-256 `b72ae4dbdff1b23f94a85e99a948e46ed892736bc4a5b65007def24efe61f290`；[provenance.json](/private/tmp/echo-quality-20261107/knowledge-tested/provenance.json) 关联两个进程与物理证据。

已检查两张自有窗口抓拍：首次抓拍已进入记忆编辑界面；重启抓拍恰逢切换，显示过渡空白帧。两张均不能单独证明来源引用或完成结果，通过结论采用实际 DOM 条目、当前 promptId、真实阅读器文档路径及磁盘的联合核验。

边界：本轮验证手动记忆管理及其自动召回，隔离配置关闭后台自动提取、整理与梦境，避免其改写合成资料；当时这些后台功能未标记通过；随后已在下节单独完成加强验收。文件格式实机覆盖 MD/TXT/DOCX，未穷举其他 Office 格式、大型资料库或长期文件监控。没有测试真实个人资料、远程知识服务或其他平台。会议录音继续按用户要求跳过。

## 后台自动记忆提取与整理补充验收（11-08）

最终证据：[memoryauto-e2e8/result.json](/private/tmp/echo-quality-20261107/memoryauto-e2e8/result.json)，`passed=true`、`nativeVerified=true`、`osPickerVerified=true`。首次 PID 94789、7/7、约 119.3 秒；重启 PID 95095、4/4、约 26.0 秒；均 exit 0、signal null。前台只进行普通对话及正常设置操作，没有调用 `memory_record_save`、`memory_flush` 或 `memory_dream` 来替代后台链路；模型和 IPC 未替换。

系统临时工作区会被产品主动排除自动记忆，因此通过真实系统目录选择，将合成工作区放在仓库被忽略的 `src-tauri/target/native-memory-review/<nonce>/workspace`。用户配置仍在全新、0700 的独立数据目录。使用已授权的指定模型，原始真实用户会话及默认记忆设置没有被改写。

| 场景 | 实际证据 |
| --- | --- |
| 自动提取 | 普通对话里的跨项目偏好与本工作区项目事实分别进入 global/workspace；记录 `origin=automatic`、未固定，来源 ID 对应真实对话，证据包含用户原文中的独立验收码；管理界面显示“从对话提取” |
| 更新与去重 | 后续普通对话更正后保留原记录 ID、增加修订号；没有独立的旧有效事实；经过额外空闲周期，记录与修订号稳定。有效记录允许说明“旧值已作废”，不将该说明误判成独立旧事实 |
| 关闭与恢复 | 关闭时普通对话不产生记忆写入；重新开启后不发送新消息，也取得新的后台模型请求完成状态。模型可返回“无新增”；项目变更再以开启后的普通对话核验自动学习，不强制要求追记关闭期间的信息 |
| 同前缀来源 | 使用真实时钟等待 UUIDv7 时间窗口，两个独立实机会话 `01a1b774-008a-7ea2-96b7-90b115f147ee`、`01a1b774-3814-7591-8d55-ced6838f8199` 确实共享前 8 位；父进程独立确认各有完整 ID 日志，同名 `task_completed` 不再覆盖 |
| 执行中退出 | 最后一条真实后台提取已开始，待处理内容已落盘但新事实尚未写入时，正常退出；父进程确认 1 个包含本轮来源和验收码的真实 pending 检查点，3 条已有自动记录 |
| 重启恢复 | 正常载入原会话后，待处理内容通过真实模型继续提取，pending 由 1 变 0，自动记录由 3 变 4；当前事实保留，没有恢复独立的作废事实 |
| 自动整理 | 在正常退出后，隔离配置将整理会话门槛从 100 调整为 1；重启后的正常会话/计时器通过门槛，真实模型生成工作区 `MEMORY.md`。文档包含另一来源的复核流程验收码；磁盘文档、整理锁时间与运行日志共同核验，无手动整理调用 |
| 全新会话召回 | 重启后从最近工作区选择同项目，再创建第三个真实会话 `01a1b775-3c1c-7183-b1ec-16b31a328463`。问题不提供任何验收码、不调用工具，模型准确返回四个当前值，未返回作废值；不是复用来源会话的直接聊天上下文 |

QA-14 原始桌面失败为 `memoryauto-e2e3/`（完整 180 秒等待），旧二进制和实时配置/记录快照保存在 `memoryauto-before-fix/`。QA-15 回归使用此前实机的两个同前缀 UUID，修复前确实写到同一日志路径；修复后额外检查整理排除和遗忘来源隔离，旧格式维持保守匹配，未迁移或重建原有日志。QA-16 的真实遗漏和持久记录见 [cause.json](/private/tmp/echo-quality-20261107/memoryauto-project-before-fix/cause.json)，其中项目事实已保存且整理文档存在，检索状态为 0 结果；基础上下文回归修复前明确失败，修复后还验证了无关全局 reference 不强制注入、已忘记的项目事实不再注入。

回归结果：`memoryauto-rust-memory-final2.log` 为宿主按 memory 筛选的 22 通过、1 项既有忽略；`memoryauto-ui-regression.log` 为 22 项通过；`memoryauto-guards-final.log` 为 15 项通过；`memoryauto-native-guards-final.log` 为完整原生验收守卫通过。原生最终构建日志 `native-build-memoryauto-core-fix.log`。新增回归的明确失败分别保留在 `memoryauto-source-identity-red.log`、`memoryauto-project-core-red.log`；其他开发迭代失败及模型对关闭期间信息返回“无新增”的情况保留在 `memoryauto-e2e1/2/5/6/7/`，没有删除原始失败。

最终已测二进制冻结为 [memoryauto-final-tested/echoagent](/private/tmp/echo-quality-20261107/memoryauto-final-tested/echoagent)，SHA-256 `fec294fa82bdd81dae2e9e7a955a13cb5bac84a6434d63e31ab4286bcb83fb52`；固定夹具 SHA-256 `448afd0db1776c1f9d55f0d3e8a9bb5052b0bd1d213d17ad767f150ff0c7c206`。[provenance.json](/private/tmp/echo-quality-20261107/memoryauto-final-tested/provenance.json) 关联二进制、两个进程、来源/待处理文件及整理文档的物理证据。此前 `memoryauto-e2e4/` 是计时器修复后的普通双来源通过记录；最终以加强后的 e2e8 为准。

截图已检查：首次抓拍位于第二个任务创建中，重启抓拍恰逢过渡空白帧；它们不能单独证明自动提取、整理或召回成功。成功结论来自真实 UI 控件与来源展示、后台模型日志、当前完成事件、独立磁盘数据及全新会话的联合核验。

边界：本轮以受支持配置将空闲检查与整理检查加速至 5 秒、整理最小小时设为 0，并在正常退出后调整会话门槛；没有模拟系统时钟或手造记忆结果。默认长周期等待、休眠/断电、长时间压力和其他平台不在本轮通过范围内。会议录音继续按用户要求跳过。未提交或推送。

## 异常退出、休眠唤醒与稳定性追加验收（11-08，已结束）

用户确认先运行 60 分钟，并同意手动唤醒解锁；随后要求跳过休眠深测并结束本轮。没有更改系统电源设置或取消既有唤醒计划；测试的临时空闲睡眠断言已随观察进程退出释放。未提交或推送。

本轮复用生产 `host_watchdog` 的启动与自动重启逻辑，只在固定私有验收入口中开启调试构建守护。入口验证主窗口来源、私有目录、固定仓库工作区、模型配置、nonce 和阶段；父进程只对确认的测试主 PID 发出 SIGKILL，恢复由实际守护进程执行，父驱动未启动替代主进程。

QA-17 修复前：`lifecycle-history-red2/result.json` 的 `chatHasRequest=true`、`replayHasRequest=false`；真正自动恢复后，界面“Interrupted request restored in real UI”检查失败。原二进制与原生失败报告保留。修复后 `lifecycle-e2e3/` 两份历史与实际界面均保留中断请求，守护约 8 秒恢复；草稿、已完成文件内容和修改时间均保持一致，恢复后的静默窗口没有自动重发前台请求或工具操作。

| 检查 | 最终证据 |
| --- | --- |
| 原生阶段 | 首次准备 4 项、恢复及持续运行 4 项、最后重启 2 项；最终驱动 `passed=true`、`nativeVerified=true` |
| 持续运行 | 同一主 PID 2645，墙钟 3,607,203 ms（60 分 7 秒）；10 次期间真实模型探测、1 次结束后探测均正常结束 |
| 界面及数据 | 499 次界面观测，窗口代次始终为 1；草稿保留，交付文件哈希、大小、修改时间与崩溃前相同；重启界面检查通过 |
| 主进程资源 | 110 次样本；RSS 约 119.8–952.4 MiB，最后约 353.5 MiB；文件句柄 29–30；CPU 最大观测 11.3%。仅覆盖宿主，不含所有 WebKit XPC 进程 |
| 锁屏恢复 | 约第 38 分钟控制台锁定，界面计时器暂停；解锁后原主 PID/窗口代次保留，真实模型探测继续完成，没有据旧界面记录声称持续响应 |
| 休眠边界 | 用户确认可配合后执行过一次 `pmset sleepnow`，系统睡眠计数 16→17，并记录 Software Sleep 和 DarkWake。完整唤醒专项核对按用户要求停止，休眠唤醒深测记为跳过/未完整验收 |
| 相关回归 | 持久化模块 31/31、守护 Rust 2/2、生命周期守卫 12/12；完整原生守卫通过 |
| 收尾 | 用户结束本轮前自动主流程已完成；没有启动追加测试。已记录的宿主、守护与空闲断言进程均已退出 |

最终记录为 [result.json](/private/tmp/echo-quality-20261107/lifecycle-e2e3/result.json)、[结束汇总](/private/tmp/echo-quality-20261107/lifecycle-e2e3/end-of-round-summary.json)、`soak.json`、`final.json`；候选二进制 `/private/tmp/echo-quality-20261107/lifecycle-tested/echoagent` 的哈希与嵌入脚本哈希见同目录 `provenance.json`。墙钟时长含锁屏及系统睡眠暂停，不等同于连续 60 分钟前台活跃负载，也不能据本次有限观察排除所有内存泄漏或外推全天运行。

QA-18 为既有回归等待竞态：ClearTitle 先 POST 元数据，再 PUT 会话行；测试等待第一笔请求后立即断言第二笔请求，首轮 30 通过/1 失败。改为等待两项原有精确结果，保留最终断言与 5 秒超时后 31/31 通过。Runtime 独立工作区使用既有锁文件；未升级依赖或修改产品标题同步逻辑。

验收开发中另修正了草稿 `{version,data}` envelope 的读取，并将误匹配上一回合 chunk 的“流式中断”表述收紧为实际的“等待模型响应期间中断”。前面的 `lifecycle-e2e2/` 在发现历史缺陷后主动停止，不计作完整时长通过；一次重命名二进制被原生进程身份守卫拒绝，恢复原名后得到有效红色证据，没有放宽身份检查。

补充的直接子进程退出码脚本已准备，但用户结束本轮后未运行；本次不额外声称取得该脚本的 exit 0/无信号证明。原生正常重启的界面检查与本轮相关进程退出已有独立记录。会议录音继续按用户要求跳过。

## 视觉证据

已人工查看自有原生首次任务窗口、1024 暗色模型连接管理、1024 暗色大字号重名任务搜索；后两者为模拟 IPC，不能作为实际后台成功证据。目录区分、文字层级和可读性没有发现需大规模改版的证据。

- 普通原生会话恢复：[自有窗口截图](/private/tmp/echo-quality-20261107/native-manual-restored.png)，界面实际显示模型名称；两轮标记另由辅助功能记录与磁盘核验。
- 产品截图：`/private/tmp/echo-product-after/`。
- 最终生产启动：`/var/folders/yj/cmk3vp190sdb502fg81sryxw0000gn/T/echo-production-startup-abdIwd/`。
- 对比度：`/var/folders/yj/cmk3vp190sdb502fg81sryxw0000gn/T/echo-color-audit/after/evidence.json`；78 条人工项为 cloud-storage 20、controls 32、color-system 26，均为原生 Select。
- 真实 WKWebView 表单：`/var/folders/yj/cmk3vp190sdb502fg81sryxw0000gn/T/echo-native-colors-nTv4Vk/`。

## 遗留风险与后续验收

1. 核心取消、审批后读取与重启，以及 Browser 真实“拒绝”按钮已通过；执行中工具取消、权限超时等其余分支仍不在本次实机通过范围内，不能将这些合成任务通过等同于完整发布验收。
2. IDE、Browser/Computer Use、专家团、单次到点任务及上述 MCP/技能/本地插件、本地知识库/手动与自动记忆/来源引用已覆盖真实合成任务；远端业务站点登录/上传/发布、其他任意桌面应用、复杂工程、外部 OAuth/插件市场与可执行 Skill 的完整分支、专家团跨提供商组合和复杂工具协作、周期调度/休眠唤醒/系统重启及外部账户服务未完成端到端验证。Computer Use 本次没有单独验证其会话的重启交互。会议录音及转写按用户要求暂不测试。
3. 仅实测 macOS Intel；本轮强制退出恢复、60 分钟有限稳定性与重启界面已验证，完整休眠深测按用户要求跳过。Windows、Apple Silicon、安装包签名/升级及全天资源稳定性未验证。
4. 78 条原生 Select 人工对比度项没有全部逐一复核；活动 WKWebView 补充只证明其中已测控件行为与主题切换。
5. 复审已清理 Rust 格式与 Clippy 基线告警，两项严格检查通过。完整在线依赖漏洞扫描未在本轮完成，构建大 chunk 警告仍保留。
6. 单机高并发会放大冷启动与动态导入超时；保留原始失败，采用低并发复验，不通过删除断言或扩大权限规避失败。

所有已确认修复都保留现有架构；需要产品取舍的项只提出有证据的建议。最终模型配置及备份权限均为 0600；对 11516 个项目文件和证据目录内的日志/JSON（排除私有模型配置）的密钥扫描未发现本次密钥。密钥不出现在本报告中。

## 代码复审修复与补充验收（2026-11-08）

本节记录对上述未提交代码复审后新增的修复。前文真实模型、守护与稳定性证据属于当时的候选二进制；本轮不将它们当作修改后全部原生流程已重跑的证明。

| 编号 | 问题与实际修复 |
| --- | --- |
| QA-19 / P1 | 新日志使用完整来源 ID，但删除仍按八位前缀匹配。删除与遗忘现共用记忆库路径匹配器，新格式严格匹配完整身份，兼容历史快照、原有 UUID/hash 归档和旧短 ID 日志。归档重名使用明确的 `.archive-` 后缀，带保留分隔符的来源 ID 使用安全哈希。宿主回归实际创建两个同前缀来源及独立索引，检查删除目标的全部副本、保留另一来源的文件字节和检索结果。 |
| QA-20 / P2 | 自动标题判定与旧事实过滤使用不同文本比较规则。界面和后端统一标点、空白、大小写及 Unicode 标题处理；后端覆盖其他调用者的保存入口。旧标题记录在读取视图中修复，读取不写文件、不改正文、来源或并发版本。只改格式不再制造旧事实排除项，旧的同义排除项也能恢复；空归一化结果不再阻断所有记忆。新增、编辑及导入统一按 Unicode 字符截取标题，保留旧 UTF-16 标题兼容与普通自定义主题。 |
| QA-21 / P2 | 新扩展验收脚本硬性要求 Node 24，导致使用 Node 22.23.4 的 CI 失败。改用项目已有的版本契约校验，资源创建前先校验环境；在真实 Node 22.23.4 和 24.22.1 下分别验证完整守卫。 |
| QA-22 / P2 | 记忆库文件名断言仍是旧格式，宿主测试又不执行依赖库自身单测。更新为新格式的精确断言，新增 `pnpm test:memory:rust`：使用隔离数据、固定依赖库测试目标和自有产物验证，在 macOS 本地签名完整测试程序；该命令已接入 CI，并维护中英文说明。 |

本轮还清理了阻挡宿主格式检查的既有格式差异；Clippy 首轮另外检出 8 处既有多余引用和可合并条件告警，均改为等价写法，保留业务逻辑，未放宽检查规则。记忆编辑器继续只要求填写正文；保存范围、来源和并发保护保留。已查看 1024×680 亮暗主题下的实际浏览器编辑器截图，未见横向溢出或按钮遮挡。

| 本轮验证 | 结果与证据 |
| --- | --- |
| 记忆库完整单测 | 364 passed，0 failed；`/private/tmp/echo-fix-memory-rust-final.log` |
| 记忆界面、导入与标题回归 | 31 passed；`/private/tmp/echo-fix-memory-ui-final2.log` |
| Node 22 / 24 原生守卫 | 两个版本各 75 passed，两个既有 self-test 也通过；`echo-fix-native-guards-node22.log`、`echo-fix-native-guards.log` |
| Node 22 脚本回归 | 110 passed；`/private/tmp/echo-fix-scripts-node22.log` |
| 设置与编辑器浏览器检查 | 116 项通过，覆盖亮暗主题、宽窄窗口、大字号与控件状态；`/private/tmp/echo-fix-settings-ui.log` |
| 生产构建 | 通过，含图标一致性、TypeScript 类型检查和 Vite 构建；`/private/tmp/echo-fix-production-build-final.log`。保留既有大 chunk 提示 |
| 格式、版本和 CI 配置 | 宿主格式检查、差异空白检查、源码版本/供应商校验及 CI YAML 解析通过 |
| 最终前端全量回归 | 342 个测试文件、3978 passed、0 failed；`/private/tmp/echo-fix-vitest-frozen.log` |
| 宿主完整回归（lint 整理前） | 17 个程序、1416 passed、0 failed、9 项既有忽略；`/private/tmp/echo-fix-host-rust.log`。每个程序取最终汇总，不重复计算嵌套子进程用例，计数见 `/private/tmp/echo-fix-host-summary.json` |
| 宿主受影响模块（lint 整理后） | 重新编译后会话管理 34、文档解析 7、模型状态 7 项通过，0 failed、2 项既有忽略；`/private/tmp/echo-fix-lint-regressions.log`。来源删除及邻居索引保护再次通过 |
| Clippy 静态检查 | `cargo clippy --locked --manifest-path src-tauri/Cargo.toml --lib -- -D warnings` 通过；`/private/tmp/echo-fix-clippy-final.log` |

首次全量 Vitest 与导入模块修复同时执行，新增 Unicode 导入断言读到了旧模块，记录为 3976 passed / 1 failed，不计作最终通过。随后停止并行修改测试依赖，在冻结版本完整复跑得到 3977 passed；最后补充纯符号等值判定后，再次冻结源码执行全量回归，342 个文件、3978 项全部通过（`/private/tmp/echo-fix-vitest-frozen.log`）。保留原始失败、定向通过及最终日志，不删除或弱化断言。

Clippy 原始 8 项失败保留于 `/private/tmp/echo-fix-clippy.log`；宿主全量 1416 项执行于等价 lint 整理前，整理后另重编译并复跑受影响模块。Cargo 的全局缓存读写权限提示没有通过修改全局目录权限规避。

本轮只使用隔离测试数据与合成输入。浏览器检查使用模拟 IPC，不能等同于新的真实模型验收；Windows、安装包签名/升级与额外长时间稳定性验收未在本轮重跑。未提交或推送。
