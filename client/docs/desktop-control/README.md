# 原生桌面控件与截图后备

Computer Use 在原有会话绑定、认证的 `echoagent-automation` MCP 服务内增加 AX（macOS）和 Microsoft UI Automation（Windows）适配。没有外部 Python、脚本解释器或需手工运行的生产 helper。Linux 保留现有 X11 截图与键鼠路线，原生控件返回不支持。

验收准备、场景和独立核验见 [桌面控制验收](validation.md)。代码存在、编译通过、系统授权和真实桌面任务完成是不同状态；验证结果须绑定实际版本与平台。

## 任务与暂停续做

直接在聊天说明电脑任务，例如“请在 TextEdit 新建文稿并保存到指定位置”。明确请求会在可信用户消息入口选择真实 Computer Use 工具配置并建立任务执行授权，正常路径不需要先选模式、逐步确认或点击恢复按钮。说明性、否定性、代码讨论和普通文件生成请求不会仅因出现应用名称而自动授予桌面权限。旧的工具菜单保留为手动选择能力的入口。

发送“停一下”同时取消模型与实体桌面操作，后端等待在途输入退出后确认暂停。发送“继续保存”恢复同一任务，撤销旧令牌、重新观察现场并续做；历史加载和工具输出不能恢复执行。暂停时可点击“继续任务”，它与发送“继续”走同一可信消息入口，不单独恢复后台。两轮生成之间也保留暂停入口，停止与暂停采用不同文案。界面只显示简短任务进度；暂停、停止、切换模式或应用重启都撤销旧令牌。

macOS 首次需要在系统设置中授予实际运行的应用辅助功能与屏幕录制权限。Windows 需要可访问的交互桌面；系统权限、锁屏和安全桌面等限制仍由设备决定。密码及受保护控件拒绝工具读写，需要用户接管。

电脑服务按当前任务校验执行授权，不根据应用名称、按钮关键词或是否使用坐标后备重复弹窗。原生对象、原窗口归属、前台焦点、截图帧和取消租约仍在每次动作前核对。任务范围是用户指令与执行上下文，不是操作系统文件沙箱；可信用户补充的保存位置、文件名与续做要求会保留在同一上下文，重启后仍保持暂停；缺少完成任务所需的信息或出现原请求未包含的关键决定时，任务层提出具体问题。

状态接口保留原生 `supported`、`authorization`、`probe` 等诊断字段，正常任务区不展示技术探测说明。运行时拒绝格式错误的未限定桌面工具名并向模型返回纠正提示，避免把模型参数错误交给用户审批。其他 MCP 服务及浏览器独立的权限与网络策略保持各自的现有约束。

## MCP 工具

参数均沿用 camelCase。`ref`、`appRef`、`windowRef` 由后端产生，模型不能指定 PID、原生地址或数组路径。

| 工具 | 参数 | 返回与用途 |
| --- | --- | --- |
| `computer_context` | `app?`、`appRef?` 或 `windowRef?` | 合并已知应用匹配、窗口发现、紧凑控件与当前菜单命令；动作已返回新状态时直接使用，避免重复枚举 |
| `computer_list_apps` | `name?`, `identifier?` | 正在运行的应用及后端 `ref`；可精确过滤已知目标，返回 unique/ambiguous/incomplete/not_found；Windows 当前枚举有顶层窗口的进程，并使用系统版本资源中的本地化名称、可执行文件名及不含扩展名的别名 |
| `computer_list_windows` | `appRef?` | 优先按目标应用查窗口；某应用超时/不可访问时保留其他有效窗口并返回 incomplete、issues，不把全局失败当作目标无窗口 |
| `computer_continue` | `cursor` | 用上页 `nextCursor` 继续读取，直到 null；保留原始 `resultId`、`snapshotId`、顺序和引用 |
| `computer_open_app` | `appPath` | 打开绝对路径的 `.app` / `.exe`，不接受启动参数或 Shell；执行后检查应用/新进程窗口是否出现 |
| `computer_click` | 原生控件 `ref`；或截图 `frameId`, `x`, `y`；`clickCount?` | 一个点击入口优先使用原生身份定位；原生点击不支持或界面没有控件时使用截图后备，两种目标表示不得混用 |
| `computer_click_control` | 控件 `ref`, `clickCount?` | macOS 按保留的控件身份获取当前位置并做命中检查后点击/双击，支持没有原生打开动作的候选行；无需截图坐标。当前其他平台返回不支持，可使用原生动作或截图后备 |
| `computer_verify_file` | `path`, `expectedText?`, `expectedTrailingNewlines?` | 只读核验当前任务明确指定的输出路径，支持后续补充及由主机解析的主目录、桌面、文档、下载和工作区位置；不创建/修改文件。可比较 UTF-8 全文，必须区分“文件存在”与“正文完全匹配” |
| `computer_activate_window` | `ref` | 激活已发现的应用或窗口；重新检查目标前台身份，返回 `verified` |
| `computer_key_window` | 窗口 `ref`, `shortcut`；或 `key`, `modifiers?` | 优先用 shortcut="cmd+n" 等字符串；两种表示只能选一种。执行前重新验证原窗口、恢复并核对前台与非受保护的键盘焦点，再发送已知快捷键；返回派发状态，保存等任务结果需继续观察 |
| `computer_file_dialog` | 原文稿窗口 `ref`, 绝对文件 `path` | 在已打开的 macOS 标准 Open/Save 面板准备目录、路径及保存文件名，精确读回并返回当前状态；不打开面板、不写文件、不点击最终提交 |
| `computer_snapshot` | `windowRef`, `maxDepth?`, `maxNodes?`, `maxText?` | 扁平节点与 `parentRef`、`snapshotId`、`truncated`、`treeTruncated`、坐标单位及可用时的真实 `documentUrl` |
| `computer_find` | `windowRef`, `role?`, `name?`, `identifier?`, `maxDepth?`, `maxNodes?` | 精确匹配；至少一个条件。返回 unique/ambiguous/incomplete/not_found、候选和 `searchComplete`；不会选首个歧义候选 |
| `computer_invoke` | `ref`, `action` | 只执行节点公布的动作；调用后重新观察，能确认的 focus/select/expand/collapse 返回 verified，其他动作须继续核验业务结果 |
| `computer_set_value` | `ref`, `value`, `inputMethod?`, `trailingNewlines?` | 替换可写文本（允许清空），原生 value 方式上限 64KB，显式 keyboard 方式最多 8KB；写入后完整读回比较，支持中文、多行和特殊字符 |
| `computer_read` | `ref`, `offset?`, `limit?` | 按 Unicode scalar 偏移读文本；返回 text、nextOffset、truncated、continuationBlocked。不得将 provider 暴露文本当作完整历史 |
| `computer_wait_for` | `windowRef`, 选择器条件, `state`, `text?`, `timeoutMs?` | 等 appears/disappears/enabled/text_equals；轮询间释放桌面；歧义或截断不能被当作条件达成；超时返回 verified=false |

节点包含 `application`、`role`、`name`、`identifier`、`value`、`enabled`、`visible`、`focused`、`selected`、`expanded`、`protected`、`writable`、`bounds`、`actions`、`textTruncated`、`metadataTruncated`。无法可靠判断可见性时，`visible` 为 null；不猜测 true。AX 角色/动作通常是 `AXTextArea` / `AXPress`，Windows 是 `edit` / `invoke` 等。使用返回值，不猜测平台名称。

可选能力/状态读取失败时，节点的 observationErrors 标出具体操作，未确认支持的写入和动作不公布；启用状态读取失败时禁止操作该节点。snapshot/find 的 issues、issueCount、metadataIncomplete 和 treeTruncated 区分缺失元数据与未读取的子树。单个子控件失败会保留可读父节点与相邻控件；无法读取的分支使搜索保持 incomplete，不能断言控件不存在或唯一。根窗口失效、权限撤销及应用退出仍中止读取。错误仅包含操作名/代码，不记录控件值。

应用启动的 appPath 必须来自用户授权、真实文件发现或已观察的信息。本工具不提供完整的已安装应用目录；`computer_list_apps` 当前只发现正在运行的应用。

建议流程：精确发现目标应用 → 按 appRef 枚举窗口 → 激活并验证 → 快照/查找控件 → 原生动作/写入 → 实际读回/等待状态。应用打开后刷新 appRef。无窗口引用时不能调用 snapshot；全局枚举不完整时先缩小到目标应用。列表的 issues 最多显示四项，issueCount 为实际数量，issuesTruncated / issueDetailsTruncated 明确标注错误列表/内容省略。

## 引用、审批与结果

普通引用有效期 120 秒；显式 snapshot 使该窗口旧的控件引用失效，原生写操作使本会话的控件引用失效。独立 find 查询保留各自候选与祖先，不互相撤销引用；执行前仍检查真实目标及观察状态，持续无结果的查询不累积引用。原生对象保留在会话独立工作线程，macOS 保留主应用及实际 AX 提供进程身份，检查两者是否退出；控件缺少 AXWindow 时，必须沿有界 AXParent 链匹配原窗口，明确指向别的窗口则拒绝。Windows 保留进程句柄并比较 UIA 原生元素的窗口祖先。暂停、停止和模式切换释放控件上下文。

写操作在准备阶段保留目标原生对象与状态，依据当前任务授权取得一次内部执行凭证；普通任务内操作不要求用户逐次点击确认。执行前重新检查对象、所属窗口及窗口上下文、角色、名称、值预览、边界和可操作状态，并重新核验前台焦点。等待用户提供必要信息时不持有桌面队列或执行锁。目标变化返回 `target_changed` 并取消。内部单次审批凭证不在 MCP schema 中，模型传入 `_proof` 会被拒绝。普通引用过期后，只有已保留的原生目标与已授权内容仍一致的操作可继续；不会按索引重新选一个控件。

| 状态 | 含义与下一步 |
| --- | --- |
| `verified` | 已读回核验工具所声明的具体状态；仍需确认用户的整个任务目标 |
| `executed_unverified` | 原生调用返回成功；观察新控件/截图确认发送、保存等业务结果 |
| `dispatched_unverified` | 窗口绑定按键已派发；系统没有提供目标应用执行回执，executed 为 null，必须观察实际变化 |
| `result_unknown` | 调用可能执行过才失败；先观察，不能直接重放发送/保存 |
| `stale_reference` / `application_exited` | 重新发现/快照 |
| `target_changed` | 批准目标变化，重新观察并重新确认 |
| `protected` / `readonly` / `disabled` | 接管或选择支持的动作；不暗中降级为任意坐标点击 |
| `permission_denied` | 用户检查当前应用身份、系统授权/权限级别 |
| `native_timeout` / `native_failure` | 原生提供方超时/失败；写操作必须先检查是否已生效 |

工具结果通过 Runtime MCP 处理链返回；截图是 MCP image，经过 `format_mcp_image` 的 data URI 和 Runtime 图像提取链进入图像上下文。切换到 Computer Use / Browser Use 会实际重建工具配置，仅保留工具发现、MCP 调用与会话 MCP 工具；不会保留默认模式的 Shell。忙碌时拒绝需要重建的切换，不显示虚假切换成功；退出自动化模式恢复进入前的会话工具配置，并重新注册属于该会话的记忆与历史工具；进入计划模式也必须实际恢复。

应用/窗口列表、snapshot 和 find 的 JSON 每页最多 12000 UTF-8 字节，动作结果的 before/after 与附带 state 也共同受完整回包预算约束，超出的控件通过同一游标继续读取；避免 Runtime 的 20000 字节默认截断破坏 JSON。`pageTruncated` 与 `nextCursor` 表示还有已观察到的结果页；`totalItems` 是本轮已观察结果数，不能把第一页当作全部结果。`truncated` / `treeTruncated` 则表示提供方或遍历边界上的不完整。游标绑定本会话、单次消费、120 秒有效；同一窗口刷新快照或本会话原生写操作使相关控件页失效。最多 32 个待续结果、4MiB 缓存；达到上限要求完成旧页或缩小快照，不隐瞒遗漏。文本预览最多 512 scalar，展示的名称等元数据最多 256 scalar，额外裁剪会标注 `textTruncated` / `metadataPreviewTruncated`；查找在裁剪展示之前完成。

只读续页按快照创建时间计算有效期，并验证仍保留的原生窗口身份；不会因更早发现的窗口引用刚过期而提前失败，也不会延长该引用的操作权限。窗口对象已释放、应用退出、刷新快照或游标自身过期仍拒绝续页。

## 读取边界和坐标

默认快照深度 8、250 个节点、24000 个值字符；上限深度 20、1000 节点、64000 值字符。每轮遍历约 3 秒预算（单次原生调用可超过剩余预算）；原生内部值预览约 4096 字符，模型展示另有上述字节限制。树或文本限制、未加载内容、分页和虚拟列表都不能当作完整数据。树或选择器元数据截断时 find 不声称唯一匹配或完整不存在；原生名称和标识符最多约 1024 UTF-16 单元。

`computer_read` 默认请求每段 8000，上限 64000 scalar，offset 上限 2000000；实际返回受 12000 字节上限约束，必须用返回的 nextOffset 继续，不能自行累加请求 limit。Rust 侧单次文本复制限制约 2000000 UTF-16 单元；原生提供方仍可能先返回完整原生值并占用更多内存；遇到 `continuationBlocked` / nextOffset=null，需要滚动、分页或改变读取目标后重新观察。此接口尚无 provider 原生的无限长度 text-range 游标，不能声称完整读取任意长文档。

macOS AX bounds 是全局桌面逻辑点；Windows UIA bounds 是全局桌面物理像素。原点为主显示器左上，允许负坐标。原生 bounds 不能直接当作压缩截图坐标。截图键鼠继续使用自身 frameId 的 origin、logicalWidth/Height、imageWidth/Height 做转换，保留原来的 30 秒期限和画面变化校验。审批等候导致截图过期时取消并要求新截图，不延长旧帧寿命。

已知菜单/对话框快捷键可以使用 computer_key_window 的原生窗口引用。用户接管或切换应用会改变焦点，因此该工具在每次派发前恢复并验证已授权的原窗口，拒绝焦点不明或受保护字段；不会以旧截图证明当前目标。收到 dispatched_unverified 后重新观察对话框或文件结果，不能把按下保存快捷键当作保存成功。

优先使用 shortcut="cmd+n"、shortcut="cmd+s" 或 shortcut="cmd+shift+s"；也兼容分开的 key="n", modifiers=["cmd"]。混用两种表示、未知修饰键或将 command+n 整串作为 key 会在准备执行之前被拒绝。不把模型传来的任意对象自动转换为按键数组。

文本 value 是普通 Unicode 字符串，不做额外反转义。模型应传真正换行与原始字符；JSON 结果中显示的转义不代表文档中有字面转义符。verified 只证明读回等于该次入参，仍须与用户要求逐字比较；不能用工具校验通过代替整个任务验收。

全局桌面队列串行化所有会话的原生调用、截图和键鼠操作。等待条件轮询间与审批期间不占用桌面。它按操作协调，不会让一个多步任务无限独占用户桌面。工作线程最多 16 个；120 秒无请求后释放。排队取消和异常退出通过 RAII 释放占用。原生请求本身持有桌面与会话执行门的 owned guard；HTTP 调用方被取消时，后台原生操作结束前不会提前释放，也不会提前确认暂停。

## 文件对话框与跨应用流程

使用窗口发现、控件快照查找真实系统对话框的目录/文件名框、列表和确认按钮，以返回的支持动作输入、选择并等待目标应用读回结果。原生接口不支持的动作，重新截图并走同一桌面授权门的键鼠后备。API 返回成功不是已保存文件的证明；应核验对话框消失、应用状态以及授权输出文件的独立内容。

桌面应用以当前系统用户运行；任务工作区授权不会自动约束它们内部打开的所有路径。实际任务只处理用户明确要求的资料；验收只使用合成文件。跨应用从 A 读回、切换 B、输入再读回的每一步都保留实际结果，不复用失效引用。

## 平台限制

- 部分 canvas、自绘控件、Electron 未开启辅助功能的内容、虚拟列表只暴露有限信息；截图后备仍需要系统权限。
- macOS 每个 AX 元素设置 150ms 消息超时，原生对象由 CFRetain/CFRelease 管理。应用列表可由 AppKit 返回；窗口与控件实际 AX 读取仍要求授权。
- Windows 在 MTA 初始化 COM，使用 UIA8 connection 250ms / transaction 500ms 超时；UIA 对象在同一线程创建、调用、释放。高权限、UAC、跨桌面和 provider 问题返回明确原因。单个进程或窗口不可访问时保留已读取的窗口并标记不完整；桌面根节点本身不可访问仍返回错误。优先按可访问 appRef 查询。
- 异常 provider 若无视系统超时并卡在进程内，当前实现不能强制终止单次原生调用；暂停完成会等待它退出。尚未实现有签名与强制超时管理的隔离 helper，不能承诺抵御所有 provider 硬挂起。
- 各平台和真实任务按验收指南分别验证；不承诺任意软件所有界面均可自动操作。

原生 API 依据：[Apple AX 消息超时](https://developer.apple.com/documentation/applicationservices/1459345-axuielementsetmessagingtimeout)、[Microsoft UIA transaction timeout](https://learn.microsoft.com/en-us/windows/win32/api/uiautomationclient/nf-uiautomationclient-iuiautomation2-get_transactiontimeout)。


## 文本控件的实际键盘输入

`computer_set_value` 默认 `inputMethod="value"`，调用原生 AX/UIA 设置并精确读回。若控件 setter 不支持或读回未改变，可在重新观察后显式传 `inputMethod="keyboard"`（最多 8KB），无需 frameId。该方式激活原窗口、聚焦并核对准确的原控件，再全选替换、发送 Unicode、核对实际完整值。它只接受可聚焦文本控件；同一任务授权与桌面租约仍适用。失焦、受保护焦点、暂停或桌面不可用会停止后续输入；部分执行不自动重试。读回相等仅证明符合这次入参，仍需与用户要求比较。

macOS 的 `desktopAvailable` 独立于系统授权；合盖、锁定或离开图形会话时不会继续原生操作或把 AXApplication 当作窗口。单个 AX 提供方不可访问时保留其他有效窗口，避免把局部错误误报为全局权限撤销；焦点为原窗口所属的嵌套 AXSheet 时，按真实窗口和有界父链核验归属。

## 文档位置与文字结尾

窗口标题可以只是文件名，哪怕看起来像绝对路径，也不代表文件保存到了该目录。macOS 窗口列表和快照现在返回系统提供的本地 `documentUrl`；保存后核对实际地址，再通过只读 `computer_verify_file` 检查用户指定输出文件。存在检查不证明内容匹配；`expectedText` 核验逐字符比较 UTF-8 正文，超过 1 MiB 或不支持的编码明确返回限制。该工具不代替桌面输入和保存。

`computer_set_value` 可显式传 `trailingNewlines`（0–16），`computer_verify_file` 对应 `expectedTrailingNewlines`。省略时原文完整保留；传入时只按明确的数量重设末尾 LF，不改变其他字符，避免模型/供应商处理参数边界时遗漏结尾换行。核验失败返回实际/期望字符数和末尾换行数，不能为迁就错误的核验参数修改已经正确的文稿。

当前观察范围：macOS 中若指定原窗口拥有当前聚焦的模态 sheet，快照/查找优先读取该 sheet；返回 focusedDialog / observationRootRole，windowRef 与所有写入校验仍绑定原文稿窗口。关闭弹层后恢复读取窗口。深度、节点、文本预算及截断提示仍有效。

提供方兼容：明确的无符号整数选项（点击次数、终止换行数、观察深度/数量、文本分段位置等）兼容纯十进制字符串，随后继续执行原有范围检查。不会解释任意表达式或改变文本、坐标、引用、授权含义。

## 验证入口

- `pnpm test`：完整前端回归，包含暂停、继续、权限边界及失败保留状态。
- `pnpm test:scripts`：包含真实 Runtime 日志资格验证、短引用、分页证据及验收计时回归。
- `pnpm test:desktop-native`：直接编译生产原生模块；macOS 点击事件对象测试需要可访问图形会话，测试不会派发输入。
- `cargo check --locked --manifest-path src-tauri/Cargo.toml --lib`：完整主程序及 Runtime 类型检查。
- `cargo test --locked --manifest-path vendor/echo-agent-build/Cargo.toml -p echo-agent-runtime --lib prompt_mode_transition_tests`：SessionActor 的工具名称、模式事件及自动化到默认、询问、计划模式切换回归。
- `pnpm test:runtime-smoke`：在隔离数据中启动真实 ACP Runtime 与本地 MCP，检查启停、模型选择及常驻技能刷新，不发送模型推理请求或实体桌面输入。

主程序与 Runtime 使用各自的 Cargo 锁文件；独立原生检查不能代替完整主程序检查。真实任务与输出核验见 [桌面控制验收](validation.md)，交互约定见 [桌面任务体验](task-experience-design.md)。上述自动化检查不替代特定应用与平台的实机验收。
