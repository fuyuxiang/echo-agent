# 文档贡献指南

如何参与 Echo Agent 文档建设。

---

## 文档技术栈

- [MkDocs Material](https://squidfunk.github.io/mkdocs-material/) — 静态文档站生成器
- [mkdocs-static-i18n](https://github.com/ultrabug/mkdocs-static-i18n) — 双语支持
- Mermaid — 图表
- Markdown — 内容格式

## 本地预览

以下命令在仓库根目录执行。

```bash
pip install -e "./echo-agent-runtime[docs]"
mkdocs serve
```

访问 `http://127.0.0.1:8000` 预览。

## 文件组织

- 中文为默认语言：`docs/section/page.md`
- 英文对应版本：`docs/section/page.en.md`
- 两者结构必须保持一致

## 写作原则

1. **面向用户任务**，不是面向代码结构
2. **事实从代码获取** — 配置字段、CLI 命令等动态内容从代码生成或由测试校验
3. **不手写动态模型名单** — 只写获取方式和示例
4. **不复制整份配置** — 链接到自动生成的配置参考
5. **不用绝对承诺** — Beta 项目不写"永不丢失""完全隔离"

## 修改文档时

1. 确保中英文同步更新
2. 修改配置相关内容时更新 schema metadata
3. 运行 `mkdocs build --strict` 确认无构建错误
4. 确认内部链接有效

## 自动生成的文件

### README 架构图

项目 README 与架构概览共用 `docs/assets/readme/` 下的中英文 SVG。图表由代码维护，修改拓扑或文案后在仓库根目录执行：

```bash
python3 scripts/generate-readme-diagrams.py
python3 scripts/generate-readme-diagrams.py --check
```

生成器文件头记录了代码依据。需对照实现核对模块、连线方向、进程边界与可选功能，渲染两种语言并检查文字和箭头；`--check` 只检查产物一致性。首屏品牌图与按钮单独维护；架构图应保持为可编辑矢量，产品界面截图应来自真实应用并注明适用版本。

### 配置参考

以下文件不要手动修改（由 CI 重新生成）：

- `docs/reference/configuration.md` / `.en.md`
- `docs/assets/generated/config-example.yaml` / `.en.yaml`
