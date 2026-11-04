# EchoAgent Desktop 主题实现与验收

日期：2026-11-04。生产代码基线：`540769d046c54f5697784b13fe7940284f485188`。开始时工作区无未提交修改；本轮保留原有信息架构、权限和任务流程，没有提交、推送或发布。

## 实现

- 在 `src/styles/tokens.css` 原位维护浅深语义色和兼容别名，合并重复配色区块。主操作、选中项、状态、控件边界、分割线、焦点和禁用态分别管理，继续使用已有 ThemeProvider。
- 公共按钮、表单、导航、Toast，以及项目、组织、技能、连接器、会话、会议和代码工作台迁移到正确的颜色角色。选中筛选与创建/保存操作具有不同的视觉权重；普通会话气泡保持中性。
- 缩小组织卡片版本文字的选择器范围，修复标签前景被通用 span 覆盖的问题；交互 SVG 使用 currentColor，没有全局改写第三方 SVG。
- 技能检查完成后压缩文件选择区，保留一个主要滚动区域；大字号下风险、依赖和安装操作仍可到达。
- 项目/组织装饰背景改为中性承托，已有插画使用灰度协调；第三方 Logo、用户图片、文档纸张和语法高亮保留独立规则。
- 新增宿主到 Monaco/Theia 的颜色映射，覆盖编辑区、diff、选择、输入、终端和状态栏。真实集成中发现并修复了 TextMate 颜色索引错配，使用公开主题注册接口重建颜色表；注释色单独保证可读性。未选中的活动栏图标使用中性次要文字色，修复默认半透明强调色过淡的问题。
- 离线导出的 HTML 具有独立浅深色表，状态色与客户端一致；打印继续使用白纸配色。

## 最终颜色角色

以下为正式 token 的值；派生透明边框仍通过 CSS 在承托面上合成，测量使用最终渲染值。

| 角色 / token 后缀 | 浅色 | 深色 | 用途 |
|---|---|---|---|
| surface-canvas | #F7F8FA | #111318 | 页面画布 |
| surface-sidebar | #F0F2F5 | #151820 | 导航与侧栏 |
| surface-content | #FFFFFF | #1B1F28 | 内容与编辑区 |
| surface-raised | #FFFFFF | #242936 | 抬升表面、菜单 |
| surface-conversation | #EEF1F5 | #242936 | 中性气泡、局部承托 |
| text-strong | #20242E | #E8ECF3 | 正文、标题 |
| text-medium | #5F6877 | #B2BBCB | 次要说明 |
| text-weak | #626D7E | #9AA6B9 | 辅助文字 |
| border-default | #E1E5EB | #303746 | 普通分割线 |
| border-control | #7B8798 | #69778F | 输入边界 |
| button-primary-bg / fg | #4F5BD5 / #FFFFFF | #A5AEFF / #171B3A | 创建、确认、发送 |
| button-primary-bg-hover | #424CBD | #BAC1FF | 主操作 hover |
| button-primary-bg-active | #363F9F | #CCD1FF | 主操作 pressed |
| control-selected-bg / fg | #EEF0FD / #4F5BD5 | #292F52 / #A5AEFF | 导航、筛选、勾选 |
| control-selected-bg-hover | #E7EAFD | #343C63 | 已选 hover |
| control-selected-bg-active | #E5E8FC | #384168 | 已选 pressed |
| status-success / success-surface | #237A57 / #EBF6EF | #79D3A6 / #19382C | 成功、完成 |
| status-warning / warning-soft | #965E12 / #FFF5E4 | #E5B567 / #372C1B | 待确认、依赖缺失 |
| status-error / error-soft | #BA3A45 / #FDEEEF | #F0929C / #3D252C | 错误、危险 |
| status-info / info-soft | #3565A8 / #EDF3FD | #8FB5F0 / #202D43 | 普通信息 |
| control-disabled-bg / fg | #E9EDF3 / #798392 | #252B36 / #738199 | 中性禁用态 |
| focus-ring | #4F5BD5 | #A5AEFF | 键盘焦点 |
| code-comment | #4E6A58 | #9CB893 | Markdown、Monaco、Theia 注释 |
| code-type | #206980 | #4EC9B0 | 编辑器类型语法 |
| code-number | #237A57 | #B5CEA8 | 编辑器数字语法 |
| code-constant | #005F9F | #4FC1FF | 编辑器常量、枚举语法 |
| code-diff-added-bg | #2EA04304 | #2EA04306 | diff 新增行透明承托 |
| code-diff-removed-bg | #F8514904 | #F8514906 | diff 删除行透明承托 |
| code-diff-added-text-bg | #2EA0430C | #2EA04311 | diff 新增字符透明承托 |
| code-diff-removed-text-bg | #F851490C | #F8514911 | diff 删除字符透明承托 |
| code-match-bg | #EEF0FD | #222840 | 代码符号匹配承托 |

主要角色采用任务建议值。独立语法角色修复了深色注释约 4.163:1，以及绿色 diff 图层上蓝色关键字约 4.334:1 的问题；类型与数字保留各自的蓝绿/绿色辨识度。实底警告/错误徽标使用较深背景与白色文字，避免将深色模式的浅状态前景直接作为实底背景。圆角沿用现有控件、卡片和弹层层级，字号继续按 `root = 16 × 偏好字号 / 13` 缩放。

## 复审修复

- 差异行和字符分成独立透明图层。新增/删除字符使用相应成功/错误色的底部标记，空范围保留原生插入/删除标记；侧边行号区域保留清楚的状态承托。宿主 Monaco 和 Theia 使用相同颜色映射和标记规则，移除会压住字符笔画的上下左右框线。测试同时检查叠加一次、叠加两次后的语法文字对比度，覆盖 Theia 的标识符蓝色。
- 编辑器像素验收等待差异 worker 实际生成行和字符装饰，再截图和采样；同时测量字符范围与整行背景的区别，以及底部标记的对比度。故意移除字符高亮的负例必须失败，不能用文字对比度通过替代差异辨识度验收。
- 每个验收服务使用独立的临时优化缓存，在正常关闭、重复关闭、启动失败和关闭失败路径回收。下次启动仅清理可确认所属进程已退出的缓存，保留活动进程、无权限探测的进程、符号链接和来源不明的目录；两个并行服务不会删除彼此的缓存。
- 品牌色验收按真实 `aria-pressed` 和 `--selected` 状态匹配控件。保留原 16 个页面，另对项目、定时任务、侧栏分别检查浅深主题下的默认、悬停、按下状态，强制验证实际覆盖数量，并用错误配色证明检查能够发现退化。补齐侧栏和定时任务已选项的 hover/active 样式，避免鼠标进入后失去选中承托。
- 新增缓存隔离、遗留缓存回收、受保护目录保留及服务异常清理的回归测试。真实 Vite 关闭测试同时确认监听端口、HEAD 快照和普通优化缓存均已回收。

下方“实际运行结果”为初轮主题验收记录。本次复审修复的独立结果如下：

| 复审检查 | 结果 |
|---|---|
| 应用构建 | Node 24.22.1 下通过图标校验、TypeScript、Vite；保留常规大 chunk 提示 |
| 主题 / 编辑器 / 缓存 / 基线定向测试 | 16 个文件、82 项通过；新增缓存回归另单独复验 9 项通过 |
| 现有 IDE 测试 | 70 项通过 |
| 真实 Vite 关闭 | 普通与 HEAD 基线两组通过，监听端口、缓存及快照释放；重复关闭通过 |
| 业务关键状态 | 90 组，0 对比度失败、0 浏览器异常 |
| 品牌 / 选中态 | 原 32 组页面及新增 18 组选中交互通过；三类控件的浅深错误配色负例均被捕获 |
| 本地 Monaco | 16 组、32 次插入/删除字符范围观察通过；48,832 个字形承托像素，最低文字对比度约 4.871:1；移除字符高亮的浅深负例均失败 |
| ThemeProvider 生命周期 | 8 组通过，关闭后模型数为 0、重开后为 2；截图等待差异装饰出现 |
| Theia 构建与暂存 | 完整 prepare 通过，最终常量颜色映射经增量编译、浏览器生产打包及 stage 后复验；使用本机已有 npm 11.21.1 |
| 真实 Theia / 完整工作台 | 11 组通过，含浅深编辑、diff、终端及 1024×680 / 18px；最低语法文字对比度约 4.815:1，0 浏览器异常 |
| 遗留缓存 | 从约 5.1 GB 降至约 351 MB；已退出进程拥有的缓存已回收，剩余为缺少进程归属的旧目录，未误删 |

复审证据：[运行摘要](/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a6b6-0b73-76e1-9f92-c52ed99076c5/echo-theme-fixes/verification.json)、[真实 IDE 浅色差异](/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a6b6-0b73-76e1-9f92-c52ed99076c5/echo-theme-fixes/echo-editor-theme/diff-light.png)、[真实 IDE 深色差异](/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a6b6-0b73-76e1-9f92-c52ed99076c5/echo-theme-fixes/echo-editor-theme/diff-dark.png)。同目录保留定向检查日志、原始测量 JSON 和选中态截图。业务 IPC 仍采用隔离数据，未执行真实 Agent 任务；本轮没有 Windows 原生界面实机结果。

## 已知问题关闭记录

下表区分当前 HEAD 中实际复现的问题与旧原型的回归风险；没有把旧原型的比值冒充当前基线的测量值。表中比值仅为便于阅读的显示值，判定使用未四舍五入的原始值。

| 项目 | 当前基线复核及修改 | 修改后证据 |
|---|---|---|
| 会议按钮、播放图形 | 实际复现深色约 2.064:1；`meeting-workbench.css` 统一主操作配对并补 hover/active | 新建/结束、完成播放与录音错误；已测按钮/播放图形最低约 5.541:1 |
| 设置动态反馈 | 实际复现深色 2.593:1；`SettingsSections.tsx` 区分成功、输入无效和保存失败；消息改用状态色对并关联 aria-invalid/描述 | success / invalid / error 三种真实操作反馈，最低约 4.749:1 |
| 知识源选择勾号 | HEAD 未复现旧原型 1:1；正式样式明确使用 selected-bg + selected-fg，并加入不可见勾号负例 | KnowledgePicker 实际 SVG，最低约 4.886:1 |
| 项目引用勾号 | HEAD 未复现旧原型 1:1；共用选择组件修正角色混用 | 专家、技能、连接器三种 RefPickerDialog 实际 SVG，最低约 4.886:1 |
| 侧栏筛选勾号 | 实际复现浅色约 2.008:1；`Sidebar.tsx` 去掉固定 fill，选中项用语义色 | 两个已选筛选 SVG 都参与测量，最低约 4.886:1 |
| 计划等待确认 | HEAD 当前配色已达标，旧原型风险未复现；去掉局部 12% 自行混底，统一警告色对 | 真正计划审批和暂停队列，文字最低约 4.974:1 |
| 技能风险与依赖 | HEAD 未复现旧原型低比值；正式迁移配套 warning 色，展开实际检查详情 | 中风险、缺少依赖、待连接/授权均可见并测量，最低约 4.974:1 |
| MCP 未保存 | HEAD 未复现旧原型低比值；去掉局部 15% 混底 | 编辑配置后“未保存”约 4.974:1；无效 JSON 错误另测 |
| 工作目录数量 | 已选数量文字跟随 selected-fg，选中承托统一 | 真实工作目录菜单中数量最低约 4.886:1 |
| 组织技能范围 | 将版本文字限制为 `.org-skill-card__version`，不再覆盖范围徽标；personal/team 使用明确状态色对 | Skills 已发布、我的提交及范围徽标，最低约 5.265:1 |
| 项目详情筛选 | `.pd-pill--on` 改用 selected，创建等操作保留 primary；测试明确要求两者背景不同 | 项目三个页签、六种配置抽屉、模型已配置/展开/不可用 |
| 旧品牌色残留 | Toast 动作/焦点、插件边框、完成计划提示、装饰背景和插画协调处理 | Toast/菜单、完成计划、插件、项目/组织截图与角色测试 |
| 组织搜索边界 | 搜索框与相邻下拉框统一 control border | 组织资源页浅深截图；没有把普通装饰分割线一律判为 3:1 失败 |

实现过程的复测额外发现并修正：浅色活动侧栏的辅助文字/会话时间约 4.097:1、深色编辑器注释约 4.163:1，以及真实 Theia 自定义主题重用旧编码颜色表导致的语法颜色错配。另修复了关闭 diff 后切换主题再打开仍为白底的问题，以及 React 编辑器适配器先释放模型再释放 widget 造成的异常；只读 diff 使用独有模型 URI，并在 widget 清理后释放模型，复测关闭后模型数为 0、重开后为 2。大型文件徽标、录音状态图形及公式错误等同类场景也纳入现有颜色系统夹具复验；不将未复现的旧原型问题记为新增生产缺陷。

## 页面与状态覆盖

- 13 个业务视图族：首页、会话/任务/工作台、项目、组织、专家、专家团、技能、连接器、插件、定时任务、知识库、会议、代码开发。
- 14 个有效设置视图：general、shortcuts、notify-channels、agent-mail、model、memory、security、agent-settings、weixin-channel、cloud-storage、usage、data、archived、help。personalize 和 personal-memory 仅作为兼容入口测试，不计作独立页面。
- 项目三个业务页签、六类默认配置、三种引用、已配置模型菜单和不可用模型；组织四个资源页及“我的提交”；真实技能检查和风险/依赖；MCP 脏配置及错误；设置三类操作反馈。
- 会话计划审批、暂停队列、工作目录、权限和上下文菜单；专家团创建、有数据、权限确认、失败；定时任务有数据和执行记录；会议完成、播放、录音连接错误。
- Markdown、代码、图表、Word、表格、PDF 外壳、读取错误、长内容及滚动。文档内容保持自身纸张/格式，PDF 原生阅读器内部控件不等同于已全面测量的应用控件。
- 常用 1280×800、声明的最小 1024×680；默认 13px 与 18px。通用颜色审查另覆盖 11px 和 1920×1080。关键状态矩阵按正确的 rem 比例缩放，检查 viewport 溢出和必要文字/勾号确实参与测量。

## 实际运行结果

环境：macOS 15.8.2 / x86_64，Node 24.22.1，pnpm 10.34.5，Chromium 157；原生验证使用本机 WKWebView 与 macOS 15.5 SDK。

| 检查 | 结果 |
|---|---|
| `pnpm build` | 通过：图标校验、TypeScript、Vite；有常规大 chunk 提示 |
| 主题/设置/编辑器/HTML 导出/基线相关 Vitest | 16 个文件、121 项通过 |
| `pnpm test:ide` | 70 项通过 |
| `node --test scripts/test-ui-review-shutdown.mjs` | 通过 |
| `node scripts/validate-enterprise-theme.mjs` | 360 组状态，10,103 条活动文字、96 次必要图形/控件观察，0 对比度失败、0 浏览器异常 |
| 同脚本 `--baseline` | 344 组共同状态，基线 32 次失败；修改版多出的 16 组为真实本地 Monaco/diff 及运行中切换，未冒充共同前后页面 |
| 通用完整颜色矩阵 | 1,590 次观察、41,445 条文字、1,016 次控件/图形观察，0 对比度失败；308 条复杂记录由后述像素/原生检查及品牌字标人工复核处理 |
| 颜色角色/开关/按钮 | 32 组页面及交互通过，保留原来的 16 个页面覆盖 |
| 设置公共控件 | 124 组页面/编辑器、hover/pressed/focus、禁用语义配对及项目操作配色检查通过 |
| 复杂背景像素采样 | 96 组、217,522 个承托像素通过；最低约 4.955:1，另有 30 条侧栏品牌字标的渐变前景记录，单独目视复核 |
| 附件预览 | 16 组布局通过；原图缩放、Markdown/Word/表格/PDF、Escape、焦点、发送及编辑重发均为隔离操作 |
| 离线 HTML 导出 | 6 组阅读布局通过；字体、图表、状态色、取消重试和下载 |
| Mermaid | 54 组通过，含 14 组真实 WKWebView；几何、文字、连线、导出及错误恢复 |
| ThemeProvider 生命周期 | 8 组通过：浅深、系统变化、保持弹层、刷新恢复、挂载时一致及 diff 关闭/重开，无模型泄漏和浏览器异常 |
| 复合输入焦点 | 64 组居中布局、64 组长草稿布局通过；容器自身轮廓实测至少 3:1，另做 100 次相关页面观察，0 失败 |
| 本地 diff 实际像素 | 16 组首次打开/实时切换，48,832 个实际字形承托像素，0 失败；最低约 4.749:1 |
| 生产构建首帧 | 4 组浅/深/系统组合，在真实模块延迟期间检查首帧背景，再放行模块确认应用挂载；通过。独立浏览器缺少原生 IPC 的 2 条预期诊断单独记录 |
| 原生表单 `validate-color-webkit.mjs --active` | 4 组通过；真实 Checkbox/Radio/Select、浅深切换、真实 Tab 键盘焦点与截图 |
| 真实 Theia | 11 组真实后端和完整 CodingWorkbench 截图/检查，含最小窗口 18px；276 个语法片段、34,756 个实际字形承托像素，最低约 4.563:1；普通及高亮 ANSI 状态色、56 次活动栏图标观察（最低约 4.941:1）均通过，见 real-theia/evidence.json |

文字阈值按 [WCAG 文字对比度说明](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html)；必要图形/焦点按 [非文字对比度说明](https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html)。普通活动文字至少 4.5:1，大字采用相应阈值，必要图形至少 3:1。计算保留透明合成和原始精度；低于阈值但显示会四舍五入为 4.500 的负例仍失败。禁用、装饰、用户内容及复杂前景分别记录，不以 controls=0 或 failures=0 推断所有图标均已验收。

验收中出现过依赖预编译热重载、截图/导航超时及不完整证据。已将夹具统一为隔离路由、预编译完整的第三方依赖，并为不同场景使用独立页面生命周期；最终结果以上表及最终 JSON 为准，没有提高颜色阈值容差或删除原覆盖。设置旧测试的“禁用后仍为危险前景”断言改为明确检查中性禁用配对。

## 截图、复现与限制

本机结果目录：

`/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a5aa-b236-7c01-8235-d5b047c349fe/echo-theme-review`

[可搜索的前后截图索引](/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a5aa-b236-7c01-8235-d5b047c349fe/echo-theme-review/index.html)，[最终检查及日志清单](/Users/fuyuxiang/.codex/visualizations/2026/11/04/01a1a5aa-b236-7c01-8235-d5b047c349fe/echo-theme-review/verification.json)。before/after 为相同场景的 HEAD/工作区；before-targeted/after-targeted 为相同受控数据、尺寸、字号和操作位置。自动产生的时刻字段不用于像素差分评分。visual-review 中保存实际打开查看的联系表，另查看了技能导入、大字号、项目模型菜单、真实 IDE 和原生表单等原图。索引共 448 组截图；已用浏览器验证 1,717 个本地链接、图片加载与搜索，未发现断链或页面异常。早期失败实验保留在 diagnostics 和历史检查 JSON 中，最终状态以 verification.json 指向的复验为准。

代码工作台旧布局夹具的 iframe 是占位文档；它只用于宿主布局检查，不作为 IDE 已通过的证据。真实 IDE 证据仅取 real-theia 中连接实际后端的截图和测量。原生表单和 Mermaid 的 WKWebView 结果也只代表各自明确的场景。

复现入口（使用项目要求的 Node 版本）：

```sh
pnpm build
pnpm test:ui:theme-states
pnpm test:ui:contrast
pnpm test:ui:contrast:complex
pnpm test:ui:theme-lifecycle
pnpm ide:prepare
pnpm test:ui:editor-theme
pnpm test:ui:editor-paint
pnpm test:ui:brand-palette
node scripts/validate-settings-controls-ui.mjs
node scripts/validate-attachment-preview-ui.mjs
node scripts/validate-share-export-ui.mjs
node scripts/validate-mermaid-render.mjs
```

支持 `UI_REVIEW_OUTPUT` 的脚本可用该变量指定证据目录；定时任务等旧脚本仍使用自身输出目录，本轮已将结果复制到汇总目录。索引由 `node scripts/build-theme-review-index.mjs <结果目录>` 生成。该机 Swift 默认 SDK 曾不匹配，原生成功命令显式使用 `SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX15.5.sdk`，表单缓存可通过 `NATIVE_COLOR_MODULE_CACHE` 指定。

分层编辑器的背景不一定在文字祖先节点上：普通 DOM 审查将它标记为需要像素检查，不能用祖先合成的比值作结论。本轮保留修复前 4.334:1 的像素证据，并用隐藏字形前后的图像差异定位实际字形覆盖位置；不把与字形无关的边框当承托。最终快速状态复验 90 组已直接整合此检查。语法验证使用固定 TypeScript 片段，未穷举全部语言、插件或用户自定义语法色；终端检查固定普通/高亮 ANSI 样本，未穷举真彩色和任意前后景组合。

组织补充验收 32 组布局及搜索、分页、独立滚动、提交/安装、焦点等隔离流程；问题选择 34 组布局及保留答案、提交、到期、重试通过；定时任务 12 组布局及待确认/进度/读取失败/最终结果通过，另补充 8 组待确认和“已超时 · 清理未完成”的浅深/最小窗口大字号截图与颜色检查，转录回放 4 组平台主题组合通过；文档预览/编辑、持久保存、恢复锁、同步失败与冲突流程通过。

业务状态使用生产组件和模拟 IPC，没有安装真实技能、发送外部消息、调用真实模型或发布。真实 Theia 的文件/终端属于专门创建的临时目录；任务、模型凭据及原生业务 IPC 仍模拟。真实 IDE 日志保留两条临时工作区之外的写入被隔离保护拒绝的记录；没有据此声称插件初始化或真实 Agent 任务全流程均通过。Windows WebView2、Windows 原生标题栏/托盘/文件对话框尚未运行；macOS 原生标题栏、托盘和文件对话框也没有完成全流程交互矩阵。未将这些独立边界写为全应用全部通过。
