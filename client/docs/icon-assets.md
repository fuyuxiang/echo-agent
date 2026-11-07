# 应用图标维护

`app-icon.png` 是完整彩色图标的源文件。标题栏、关于窗口、浏览器页签和聊天头像使用 `public/app-icon.png`；README 使用根目录源图。

更换源图后执行：

```sh
pnpm icons:generate
pnpm icons:check
pnpm test:icons
pnpm test:ui:app-icons
```

生成命令使用项目已安装的 Tauri CLI，更新公共图片、Windows ICO、macOS ICNS、桌面 PNG 和两个平台的托盘 PNG，无需浏览器工具。只输出本项目使用的桌面资源。

ICNS 在写入前会固定内部条目顺序并保留原始图像数据。同一源图重复生成时，全部输出文件和清单保持字节一致。SVG 模板摘要统一按 LF 换行计算，兼容已有 CRLF 工作区；Git 同时将该模板固定为 LF，实际图案变化仍会被校验拦截。

界面验证使用项目现有的 Playwright 工具，可通过 `CHROME_BIN` 指定浏览器，通过 `UI_REVIEW_OUTPUT` 指定证据目录；它检查浅深色、两种像素密度、18–24px 模型/聊天图标，以及等待、完成、等待确认和错误状态的动画生命周期，并输出截图。`pnpm test:ui` 包含此验证。

CI 的应用图标作业在 Linux、macOS、Windows 上运行真实生成流程、LF/CRLF 回归与界面验证，并保留截图和检查结果。`pnpm test:icons` 在隔离的临时副本中运行，不修改工作区图标。

Windows 托盘直接缩小完整图标。macOS 菜单栏使用 `src-tauri/icons/tray/mark.svg` 的单色透明模板，由系统适配浅深色背景；该模板保留源图中的 E、倾斜轨道和小球。源图构图变化时应同步调整模板，再生成 PNG。原生代码继续将 macOS 图标标记为 template。

`src-tauri/icons/source.json` 记录源图、模板和生成资源的 SHA-256。正式构建和 CI 校验内容一致性、PNG 尺寸以及 ICO/ICNS 格式，防止源图更新后安装包或托盘仍携带旧图。图标变更也会触发三平台打包验证。

聊天等待状态仅对图标外的光环使用轻微呼吸动画，图案保持静止。滚出可视区域、切到后台、结束等待或启用减少动态效果时停止动画；历史消息图标保持静止。
