# 核心概念

本节解释系统如何工作，以及各项机制解决什么问题。先阅读[架构总览](architecture.md)建立两端边界，再根据问题选择专题；具体命令和配置在使用指南与参考手册中查询。

| 你想了解的问题 | 对应主题 | 适用范围 |
| --- | --- | --- |
| Desktop 与 Runtime 分别负责什么 | [架构总览](architecture.md) | 两端 |
| 桌面界面如何协调执行、IDE 与自动化 | [桌面端架构与数据流](desktop-architecture.md) | Desktop |
| 一次请求如何变成实际结果 | [Agent 循环](agent-loop.md) | 共同方法，重点说明 Runtime |
| 多入口如何复用执行过程 | [事件与投递](events-delivery.md) | Runtime |
| 对话、记忆和访问身份如何划分 | [工作区、会话与身份](workspace-session-identity.md) | 两端，分别说明 |
| 如何保留并召回长期事实 | [记忆系统](memory-system.md) | Runtime；对比 Desktop |
| 长对话和超长工具输出如何处理 | [上下文压缩与输出保全](context-compression-spill.md) | Runtime |
| 应选择工具、技能还是插件 | [扩展机制](skills-tools-plugins.md) | 两端，格式分别管理 |
| 权限控制与实际隔离有什么区别 | [安全模型](security-model.md) | 两端 |
| 执行经验如何转化为技能 | [进化与评估](evolution-evaluation.md) | Runtime |
| 多个 Agent 如何分工并汇总 | [多 Agent](multi-agent.md) | 两端，机制分别说明 |

## 图表约定

架构采用 C4 风格视图，在同一张图中明确描述层级、系统边界和关系。交互顺序采用 UML 时序图，生命周期采用 UML 状态图。处理流程采用端点、处理矩形和判断菱形；判断出口标注条件。数据流采用 Yourdon / DeMarco 记法：外部实体用矩形，处理过程用圆形，数据存储用平行线，有向箭头标注数据名称。

数据流图不表达时间顺序，架构依赖不表示数据自动共享，图中也不省略会影响使用判断的可选条件。完整绘图约定见[文档贡献指南](../development/documentation.md)。

点击 SVG 图表可查看原尺寸。
