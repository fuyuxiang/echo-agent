# 主题设计与维护

浅色、深色和跟随系统共用 [语义变量](../src/styles/tokens.css) 与现有 ThemeProvider。组件按颜色角色取值，兼容别名在变量层维护；具体数值以源码为准，避免文档中的颜色快照过期。

## 颜色角色

下表省略变量的 `--echo-` 前缀。

| 角色 | 变量与用途 |
| --- | --- |
| 页面层级 | `surface-canvas`、`surface-sidebar`、`surface-content`、`surface-raised`；区分画布、导航、内容和弹层 |
| 会话承托 | `surface-conversation`；普通消息使用中性色 |
| 文字层级 | `text-strong`、`text-medium`、`text-weak`；正文、说明与辅助文字 |
| 边界 | `border-default` 用于分隔，`border-control` 用于可识别的控件边界 |
| 主操作 | `button-primary-*`；创建、确认、保存与发送 |
| 选择 | `control-selected-*`；导航、筛选、勾选及其 hover/active |
| 业务状态 | `status-success`、`status-warning`、`status-error`、`status-info` 及各自承托色 |
| 禁用与焦点 | `control-disabled-*`、`focus-ring`；禁用保持中性，键盘焦点可见 |
| 编辑器 | `code-*`；语法、注释、匹配与 Diff 叠层，不套用普通按钮色 |

主操作的固定颜色契约由 [颜色验收工具](../scripts/ui-primary-action-audit.mjs) 独立核对：

| 状态 | 浅色背景 | 深色背景 |
| --- | --- | --- |
| 默认 | `#4F5BD5` | `#A5AEFF` |
| hover | `#424CBD` | `#BAC1FF` |
| pressed | `#363F9F` | `#CCD1FF` |
| 前景 | `#FFFFFF` | `#171B3A` |

实底状态徽标使用配套前景，不能将深色模式的浅状态文字直接当成实底背景。必要 SVG 跟随 `currentColor`；第三方 Logo、用户图片、文档纸张和语法高亮保留各自规则。

## 布局、字号与焦点

偏好字号通过 `root = 16 × 偏好字号 / 13` 缩放。控件、卡片和弹层沿用现有圆角层级；长内容允许自然增高，保留主要滚动区，大字号下确认和恢复操作仍可到达。

已选筛选与创建/保存操作具有不同视觉权重；验收使用真实 `aria-pressed` 或已选状态，覆盖默认、悬停、按下、聚焦与禁用。复合输入框已有外层焦点时，内部 textarea 不重复描边；其他控件保留焦点兜底，高对比主题使用系统色。

## 编辑器与导出

宿主 Monaco 与 Theia 共用 [编辑器颜色映射](../src/lib/editor-theme.ts)。主题切换应更新语法颜色表、编辑区、输入、终端和状态栏；关闭后重新打开 Diff 仍使用当前主题，先清理 widget 再释放其独立模型。

Diff 的行与字符背景可叠加，新增/删除字符另有底部标记。验收等待 worker 真正产生装饰，分别检查行、字符和标记；仅文字可读不能证明差异清楚。复杂编辑器背景用实际字形承托像素核验，不能只取祖先元素的背景。

离线 HTML 导出维护独立的浅深色表并与客户端状态色一致；打印保持白纸配色。文档预览保留内容自身的纸张与格式。

## 验收要求

普通活动文字的项目阈值为 4.5:1，必要图形和焦点为 3:1；大字、禁用、装饰和用户内容按验收工具的对应规则记录。计算包含实际透明合成，使用原始精度，不以四舍五入后的显示值放行。原生 Select、复杂前景和第三方阅读器的人工项须单独核对。

覆盖浅深与系统切换、最小窗口、大字号、长内容、菜单、审批、错误、未保存、风险及依赖状态。检查中应加入错误颜色或缺失字符高亮的负例，证明断言能够发现退化。

```sh
pnpm test:ui:theme-states
pnpm test:ui:contrast
pnpm test:ui:contrast:complex
pnpm test:ui:theme-lifecycle
pnpm test:ui:brand-palette
pnpm test:ui:editor-paint
pnpm ide:prepare
pnpm test:ui:editor-theme
```

浏览器组件、真实 Theia、macOS WKWebView 与 Windows WebView2 的结果分开记录。布局夹具中的占位 iframe 不代表实际 IDE 已通过；已测语言和 ANSI 样本也不代表任意插件或真彩色组合。证据输出与平台签核见 [开发与验收](testing.md)。
