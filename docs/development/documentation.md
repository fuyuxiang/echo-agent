# 文档贡献指南

文档站使用 MkDocs Material、双语 Markdown 与 SVG / Mermaid 图表生成。中文是默认语言，英文页面与中文保持相同主题和功能边界。

## 内容分工

| 类型 | 应写什么 | 应避免什么 |
| --- | --- | --- |
| 快速开始 | 前提、最短步骤、成功标准、常见失败 | 不可验证的时间承诺、隐藏必要条件 |
| 使用指南 | 具体目标、操作、结果与下一步 | 只有功能清单，没有使用方法 |
| 核心概念 | 设计目的、职责、交互、数据流、边界 | 源码片段、类字段清单、代码路径 |
| 参考手册 | 准确命令、配置、接口、默认值 | 手写复制大量可自动生成的数据 |
| 参与开发 | 工程位置、扩展方式、构建与验证 | 将开发细节混入普通用户流程 |

每页说明产品范围。Desktop 的 TOML 与内嵌 Rust 引擎、独立 Runtime 的 YAML 与 Python 引擎分别核对；不能只因名称相同就宣称共用实现。功能存在于辅助模块不等于已经接入用户流程。

## 事实核对

安装要求对照依赖声明和发行脚本；命令与参数对照实际解析器；配置示例对照 schema 和加载顺序；权限说明对照实际执行路径；界面用词对照当前组件。默认关闭、依赖外部服务、仅入站、人工回滚等条件应就近说明。

动态模型名称以服务实际提供的结果为准。配置字段参考由生成器维护，手写指南只给出必要示例。运行状态、操作成功与结果投递应分开描述，避免用模型回复代替执行证据。

## 图表规范

| 目的 | 图种 | 记法要求 |
| --- | --- | --- |
| 描述系统与外部依赖 | C4 系统上下文图 | 标注 Person、Software System 与关系含义，不混入源码模块 |
| 展开某个运行单元 | C4 风格组件图 | 声明视图范围，区分组件、外部实体、存储和可选部分 |
| 解释消息先后 | UML 时序图 | 参与者与生命线，实线请求、虚线返回，使用 alt / opt / loop / par 片段 |
| 解释生命周期 | UML 状态图 | 初态、状态、带事件或条件的转换；终态仅用于实际终止 |
| 解释控制分支 | 标准流程图 | 开始/结束端点、处理矩形、判断菱形；出口标注条件 |
| 解释数据流动 | Yourdon / DeMarco 数据流图 | 外部实体矩形、处理圆形、平行线存储；箭头使用数据名 |

每张图说明适用范围和必要图例。数据流编号不是步骤顺序；架构依赖不是默认共享状态；流程图不应把条件藏在无标签箭头里。实际不支持的方向不画连接。颜色只作辅助，不单独承载语义。

SVG 使用明确文字、箭头和矢量形状；Mermaid 采用对应图种语法。检查中英文长标签、箭头交叉、边框、字体和窄屏缩放；生成器一致性检查不能代替视觉检查。

## 本地生成与预览

以下命令从仓库根目录执行：

```bash
pip install -e "./echo-agent-runtime[docs]"
python -m echo_agent.config.docgen --output-dir docs/reference --lang both
python3 scripts/generate-readme-diagrams.py
mkdocs serve
```

访问 <http://127.0.0.1:8000/>。配置参考是生成产物，在预览和严格构建之前生成，不手动维护。

## 提交前验证

```bash
python3 scripts/generate-readme-diagrams.py --check
python -m pytest scripts/tests/test_docs_structure.py echo-agent-runtime/tests/test_docs_consistency.py
mkdocs build --strict
```

还应检查生成站点的内部链接，并在浏览器确认中英文首页、入门页和图表页面。仓库文档工作流会在构建后检查内部链接；推送到主分支后才部署 GitHub Pages。构建成功不代表线上已经更新。

## 文件与生成产物

中文页面使用 `docs/section/page.md`，英文使用同位置的 `page.en.md`，导航在 `mkdocs.yml` 中维护。稳定页面地址尽量保留，新增内容从相关入口链接。

`docs/reference/configuration.md` 与英文版本、`docs/assets/generated/` 的配置示例由配置生成器维护。`docs/assets/readme/` 的架构、流程和数据流 SVG 由 `scripts/generate-readme-diagrams.py` 维护，修改关系时应同步更新生成器与说明。
