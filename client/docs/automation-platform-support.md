# Browser Use / Computer Use 平台与安全边界

首页按后端实时检测展示能力。Browser Use 需要安装 Chrome、Edge 或 Chromium；Computer Use 还取决于系统截屏与输入权限。Linux 自动化底层可用不代表已有 Linux 桌面安装包。

| 平台 | Browser Use | Computer Use 条件 |
| --- | --- | --- |
| macOS Intel / Apple Silicon | 检测到 Chromium 系浏览器后可用 | EchoAgent 获得「屏幕录制」和「辅助功能」权限 |
| Windows | 检测到 Chromium 系浏览器后可用 | 桌面截图与输入能力检测通过 |
| Linux X11 | 检测到 Chromium 系浏览器后可用 | X11 会话提供 XTEST 扩展 |
| Linux Wayland | 检测到 Chromium 系浏览器后可用 | 不启用全局截屏和输入注入；需切换 X11 会话 |

macOS AX / Windows UI Automation 控件层复用同一 Computer Use 入口、Runtime 和审批。接口与读取限制见 [原生桌面控件](desktop-control/README.md)，实机判定方法见 [桌面控制验收](desktop-control/validation.md)。支持能力与特定版本的验收结果分别记录。

## 任务控制与确认

- 每个任务有独立的浏览器 Profile 和下载目录。删除任务时清理对应数据；自动化任务在应用重启后以暂停状态恢复，需要用户主动继续。
- 浏览器副作用仍由后端单独确认。电脑任务由用户的明确请求启动，任务内键鼠、启动/激活应用及原生控件连续执行，不逐步确认，不按应用名称或按钮关键词弹窗。普通 Runtime 权限不能自行制造任务执行授权；暂停、停止、切换模式和重启撤销旧令牌。
- 暂停会取消在途和待执行操作，并等待实体输入退出；原生对象身份、窗口焦点、保护字段及截图帧校验保留。用户在同一聊天发送“停一下”或“继续保存”等明确指令即可暂停或续做；暂停时的“继续任务”快捷入口走同一次消息发送，生成间隙保留暂停入口，不需要双重恢复。加载历史不会恢复执行。
- 明确电脑请求自动接入真实 Computer Use 工具配置；旧的工具菜单仍可用于手动选择能力。缺少系统权限时显示一次系统设置引导；需要原请求未包含的关键选择时，任务层提出具体问题。浏览器密码字段不会进入 DOM 快照，也不接受工具输入。
- 原生控件识别使用系统对象身份；应用本地化名称只是显示信息。操作未知时先观察，不盲目重放保存或发送。权限拒绝仍保留原有约束。
- 日期、时间和数字控件使用完整规范值（例如日期 `YYYY-MM-DD`、时间 `HH:mm`）替换填写，并回读实际值与页面校验状态；下拉选项不存在或不唯一时返回错误。字段填写成功仍需继续提交，并核对保存结果。
- 富文本框的填写验证保留实际行结构，支持连续空行、末尾换行与清空；浏览器用于放置光标的占位换行不计入正文。
- 网页内容、浏览器截图和电脑截图会作为任务上下文发给当前选用的模型服务。

## Browser Use 网络范围

受控浏览器的顶层导航、子资源及 WebSocket 经任务本地代理。默认只访问公网 HTTP/HTTPS，拒绝环回、私网、链路本地和保留地址，并校验 DNS 解析后的实际目标。用户可以为当前任务显式开启内网访问；撤销时会断开旧连接并离开已打开的内网页面。文件上传限于当前授权工作区的普通文件，披露前还需单独确认。

实现见 [`automation/mod.rs`](../src-tauri/src/automation/mod.rs)、[`network_proxy.rs`](../src-tauri/src/automation/network_proxy.rs)、[`browser.rs`](../src-tauri/src/automation/browser.rs) 和 [`computer.rs`](../src-tauri/src/automation/computer.rs)。
