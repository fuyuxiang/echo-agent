# 知识库使用

Echo Agent 知识库系统为对话提供长期文档检索能力，通过向量索引实现高效语义搜索。用户上传文档后，系统自动完成提取、分块、嵌入和索引，供 Agent 在对话中按需查询。

分块默认按 1200 字符切分、相邻块重叠 120 字符（`knowledge.chunkSize` 与 `knowledge.chunkOverlap`），单位是字符而非 token。

## 架构概览

知识库系统由以下组件构成：

| 组件 | 职责 |
|------|------|
| document 工具 | 文档上传与管理（risk_level: read_only） |
| knowledge 工具 | 向量检索查询（risk_level: read_only） |
| 文档提取器 | 将不同格式文件转为纯文本 |
| 分块引擎 | 将长文本切分为语义片段 |
| 嵌入模型 | 将文本片段转为向量表示 |
| FAISS 索引 | 向量相似度检索引擎 |

```
上传文档 → 提取文本 → 分块 → 嵌入 → FAISS 索引 → 查询检索
```

## 支持的文件格式

| 格式 | 扩展名 | 提取器 | 说明 |
|------|--------|--------|------|
| PDF | `.pdf` | PDF 提取器 | 支持文本型 PDF，扫描件需 OCR 预处理 |
| Word | `.docx` | DOCX 提取器 | 仅支持 `.docx`，不支持旧版 `.doc` |
| Excel | `.xlsx` | XLSX 提取器 | 按 Sheet 提取，表格转为文本行 |
| PowerPoint | `.pptx` | PPTX 提取器 | 提取幻灯片文本内容与备注 |

!!! warning "格式限制"
    - 不支持旧版 Office 格式（`.doc`、`.xls`、`.ppt`）
    - 扫描型 PDF 需先进行 OCR 处理后再上传
    - 加密或受密码保护的文件无法提取
    - 单文件大小上限需根据部署环境确认

## 文档上传流程

通过 `document` 工具上传文件，系统自动执行完整处理流水线：

### 1. 上传

使用 document 工具将文件提交到知识库：

```
document.upload(file="产品手册.pdf", collection="default")
```

### 2. 文本提取

系统根据文件扩展名选择对应提取器，将文件内容转为纯文本。

### 3. 分块（Chunking）

长文本被切分为固定大小的片段，相邻片段保留重叠区域以保持上下文连贯性。

默认参数：

| 配置项 | 默认值 | 单位 |
|--------|--------|------|
| `knowledge.chunkSize` | 1200 | 字符 |
| `knowledge.chunkOverlap` | 120 | 字符 |

### 4. 向量嵌入（Embedding）

每个文本片段通过嵌入模型转为高维向量表示，捕获语义信息。

嵌入相关配置位于 `memory` 节，与记忆系统共用：

| 配置项 | 默认值 | 说明 |
|--------|--------|------|
| `memory.embeddingBackend` | `auto` | `auto` 启动时探测 provider，失败静默回退本地模型；`local` 直接用本地模型；`provider` 强制 provider，探测失败即报错 |
| `memory.embeddingModel` | 空 | provider 侧嵌入模型，留空则由 provider 决定 |
| `memory.localEmbeddingModel` | `BAAI/bge-small-zh-v1.5` | 本地 fastembed 兜底模型，空串禁用兜底 |

### 5. 向量索引

分块向量交由 FAISS 建立索引，用于语义相似度检索。

## 向量索引

知识库的检索由两部分组成：JSON 索引文件保存分块文本与元数据（含 file manifest，用于检测文件的删除与重命名），向量则保存在紧邻它的 `.npz` sidecar 文件中，与记忆系统的向量表物理隔离。

实现特性：

- **精确检索**：使用 `IndexFlatIP` 做全量内积比对，非近似最近邻；向量经 L2 归一化，内积即余弦相似度
- **持久化**：sidecar 随索引一同落盘，重启后加载
- **变更检测**：sidecar 记录每个分块的内容哈希，分块 id 不变而文本改动时也能识别出向量已过期
- **可降级**：未安装 `faiss` 或 `numpy` 时向量检索返回空结果，检索退回关键词路径，功能不中断

!!! note "索引是派生物"
    JSON 索引与 sidecar 均可从源文档重新生成。索引损坏时直接重建，不需要修复。

## 查询知识库

使用 `knowledge` 工具进行语义检索：

```
knowledge.query(query="如何配置数据库连接", top_k=5)
```

参数说明：

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `query` | 自然语言查询文本 | 必填 |
| `top_k` | 返回最相关的片段数 | 5 |
| `collection` | 指定知识库集合 | `"default"` |
| `threshold` | 相似度阈值，低于此值的结果被过滤 | 0.7 |

返回结果包含匹配的文本片段、来源文件名、相似度分数。

## 文档管理

通过 `document` 工具管理已上传的文档：

```
# 查看已上传文档列表
document.list(collection="default")

# 删除指定文档（同时移除对应索引）
document.delete(doc_id="xxx")

# 上传并替换同名文档
document.upload(file="产品手册v2.pdf", collection="default", replace=true)
```

## 配置项

知识库相关配置位于系统配置中：

```yaml
knowledge:
  enabled: true
  docs_dir: data/knowledge
  index_path: data/knowledge_index.json
  chunk_size: 1200
  chunk_overlap: 120
  max_results: 5
  auto_index: true
  allowed_extensions:
    - .md
    - .txt
    - .pdf
```

`KnowledgeConfig` 只有以上 8 个字段。索引路径由工作区结构决定，没有 `faiss_index_path` 配置项；受支持的扩展名字段是 `allowed_extensions`，不是 `supported_formats`；也没有 `embedding_model` 与 `max_file_size_mb` —— 嵌入相关配置在 `memory` 节。

以上配置项可写入配置文件，或用 `echo-agent config explain knowledge.<字段>` 查看单项的类型、默认值与说明。

## Dashboard Knowledge 页面

在 Dashboard 左侧导航中选择 **Knowledge** 页面可以：

- 查看所有已上传文档及其状态（处理中、已索引、失败）
- 手动上传新文档
- 删除或替换已有文档
- 查看文档的分块数量与索引状态
- 测试查询功能，预览检索结果

<!-- 截图占位：Knowledge 页面全貌，展示文档列表与上传区域 -->

## 使用示例

### 上传产品文档

```
# 上传 PDF 产品手册
document.upload(file="echo-agent-manual.pdf", collection="product")

# 上传 Word 格式 FAQ
document.upload(file="常见问题.docx", collection="faq")
```

### 查询知识库

```
# 查询产品功能
knowledge.query(query="Echo Agent 支持哪些通道", collection="product", top_k=3)

# 查询 FAQ
knowledge.query(query="如何重置密码", collection="faq")
```

### 批量上传

```
# 按集合组织多份文档
document.upload(file="api-reference.pdf", collection="technical")
document.upload(file="deployment-guide.docx", collection="technical")
document.upload(file="release-notes.xlsx", collection="changelog")
```

## 最佳实践

### 文档准备

- **结构清晰**：使用标题、段落、列表等结构化元素，有助于分块质量
- **避免纯图片**：确保文档中的关键信息以文本形式存在
- **适当粒度**：单份文档不宜过大，建议按主题拆分
- **命名规范**：使用有意义的文件名，便于在文档列表中识别

### 集合管理

- 按业务域划分集合（如 `product`、`technical`、`faq`）
- 定期清理过期文档，保持索引质量
- 版本更新时使用替换而非新增，避免重复内容

### 查询优化

- 使用自然语言描述问题，避免过短的关键词
- 合理设置 `top_k`，过大会引入噪声
- 利用 `collection` 参数缩小检索范围
- 设置适当的 `threshold` 过滤低相关结果
