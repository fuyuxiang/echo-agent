# Windows 桌面交互验收

本页是 Windows 安装包的长期验收清单。先按 [发行流程](desktop-release.md) 构建并安装待验版本，记录源码和安装包指纹；界面回归不能代替原生验收。脚本只创建临时数据根和测试项目，保留证据供审阅，不读取真实会话配置。

安装待验包后，在 Windows PowerShell 中执行（路径使用实际安装位置）：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/validate-windows-file-switch.ps1 -Executable 'C:\path\EchoAgent.exe' -TheiaBrowser 'C:\path\theia\browser'
```

入口首先核验**安装目录**中的 Git 补丁指纹，然后隔离启动应用，创建普通目录、Git 仓库、嵌套项目，通过 Win32 ProcessStartTrace、WinEventHook 记录应用进程树及窗口显示/焦点事件。短命进程的命令行可能来不及查询，但其 PID、父 PID 和进程名仍可关联。日志的 focus 事件是原生焦点事件；需结合带时间戳录屏关联实际文件激活，不能冒充编辑器文件激活事件。只记录本次应用进程树，不保存其他应用的标题或命令行。`automaticallyPassed` 始终为 false，人工签核前不得声称通过。

- 三类项目各交替单击 JS、TS、文本至少 20 次，包含首次打开和已打开标签。确认无非预期子进程窗口、无焦点抢占，滚动与未保存内容不丢失。
- 验证 Git diff/stage 与交互终端；普通目录后来执行 `git init` 后刷新应识别仓库。
- 目录 A 的历史任务、目录 B 的首页草稿相互切换；历史页目录与 `/` 中项目技能始终属于 A。首页另选 C 后创建任务，确认实际 cwd=C，取消选择无变化，选择失败可见且可重试。
- 首页与历史页执行：从外向内/从内向外拖选、Ctrl+A、侧栏拖动后选择、中文 IME Enter、Shift+Enter、附件粘贴、`@`、`/` 和草稿恢复。若空值提示蓝选或黑框重现，使用 WebView2 开发工具记录 textarea.value、selectionStart/End、document.getSelection()、activeElement 和 compositionstart/end；附录屏及系统缩放，不推断已修复。
- 1024×768、1280×800、1440×900、1920×1080、接近视频比例的大窗口，字体 13/18，缩放 100%/125%/150%，深浅主题、侧栏展开/收起/拖动和长输入。记录 `innerWidth/innerHeight/devicePixelRatio`，检查按钮不裁切、无横向滚动和重复焦点圈。
- 在临时任务中打开通知设置，测试桌面提醒，点击 OS 提醒返回该任务。普通设置页测试不能代替带任务目标的测试。测试关闭后重启仍关闭、组织策略限制、系统关闭权限或勿扰模式；UI 只声称已向系统提交，投递错误应进入本地通知记录。

macOS 也需要使用独立测试任务完成通知投递与点击返回。将原生结果、安装包/插件指纹、录屏、视口与 DPR 一同保存。禁止以 VM 的 spawn 参数测试或浏览器截图替代原生签核。

共同的输入、目录与失败恢复规则见 [产品体验约定](product-experience.md)；测试层级和产物保存方式见 [开发与验收](testing.md)。
