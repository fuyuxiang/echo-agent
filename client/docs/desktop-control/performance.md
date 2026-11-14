# 桌面任务性能验证

性能以真实 Runtime 会话、独立文件核验和完整任务条件为准。比较前先确认两轮功能均通过；单轮快样本不代表稳定分位数，也不能用更少观察掩盖错误或未完成的保存。

## 比较条件

固定硬件、操作系统、模型和提供方、权限、应用状态与任务输入。分别记录冷启动和热启动；正文长度、末尾换行、暂停静默及跨应用步骤保持一致。

采用 `--task-flow` 或 `--universal-flow` 生成两份新的验收目录，并分别按 [桌面控制验收](validation.md) 独立核验，再统计原始 Runtime journal：

```sh
BASELINE_ACCEPTANCE_HOME='/path/to/baseline-home'
CANDIDATE_ACCEPTANCE_HOME='/path/to/candidate-home'
node scripts/verify-native-desktop-task-flow.mjs "$BASELINE_ACCEPTANCE_HOME"
node scripts/verify-native-desktop-task-flow.mjs "$CANDIDATE_ACCEPTANCE_HOME"
node scripts/compare-desktop-task-performance.mjs "$BASELINE_ACCEPTANCE_HOME" "$CANDIDATE_ACCEPTANCE_HOME"
```

比较文件写入候选目录中的 `task-performance-comparison.json`。两份原始目录和二进制来源一并保留到 CI artifacts 或专用证据存储，不将某一轮 `latest-*` 输出作为当前产品能力说明。

## 统计口径

- 任务耗时包含模型思考、工具往返、要求的暂停静默和最终核验；正文读回、继续后保存、跨应用任务分别统计。
- 调用数来自完整 journal，包含失败及客户端/传输拒绝；尚未返回的调用不冒充已完成。工具 schema 搜索单列。
- 同时观察发现/上下文/截图/续页次数、过期引用、未知结果、未验证写入、动作后状态错误、保存面板重开及输入重试。
- 标注确认卡数量、实际模型、源码与签名后二进制指纹。保存、内容、暂停和同任务续做的判定标准保持一致。
- 保留慢轮、失败和纠正成本。重复多轮后再报告稳定趋势或分位数，分别说明网络、冷启动与模型差异。

## 执行器的效率约束

`computer_context` 合并应用匹配、窗口与紧凑控件观察、当前菜单命令。动作已附带新状态时直接使用，只有目标缺失、截断或需要视觉判断时再发现/截图；紧凑视图优先输入、可操作目标和内容，完整结构仍可通过 snapshot 查询。

`computer_file_dialog` 在已打开的 macOS 或标准 Windows 系统面板内准备目录、路径与文件名，逐步核对且可取消；它不重开面板、不创建文件、不点击最终提交按钮。模型依据返回状态提交后核验结果，不按宿主应用名称硬编码流程。

应用、窗口和控件身份在操作前重新核验。暂停撤销执行令牌，继续时重新观察；只读身份不能变成执行授权。菜单快捷键来自当前原生菜单，不猜测保存、副本或格式命令。

瞬时观察错误可有界重读，物理操作遇到未知结果先观察，不盲目重放。参数兼容只解释已支持的数字、快捷键与有限封装，不改变文本、引用和权限含义；不能通过延长旧截图寿命、放宽焦点或取消保护来换取速度。

平台限制和读取预算见 [原生控件说明](README.md)，任务交互约定见 [桌面任务体验](task-experience-design.md)。

任务工具按会话顺序执行、仅前台输入按物理桌面串行，后台定向动作按应用协调。原生 provider 使用独立进程复用上下文，浏览器复用页面 CDP 连接；这些是实现方式，未测量前不据此宣称任务耗时或成功率改善。扩展暂停的确认等待、首次 helper 启动与私有检查点落盘成本均须计入实测。
