# 知识库

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

知识库把授权上传或放入文档目录的内容解析、分块并索引，供 `knowledge_search` 返回带来源的片段。它与用户长期记忆分开；读取一个文档不等于将它入库。

## 配置与准备

在配置中启用 `knowledge.enabled`，确认 `knowledge.docs_dir`、`index_path`、分块参数及模型/embedding 依赖。相对路径以工作区为根。格式解析依赖按需安装，具体字段见[配置参考](../reference/configuration.md)。扫描 PDF 的文字识别另需相应 OCR 能力，不能把文件扩展名可上传当作解析成功。

## 上传、替换与索引

管理员在 Dashboard **Knowledge** 页可以多文件上传、查看原始文档列表、触发重建并查看最近索引任务。上传同一路径会原子替换该文件；完成重建后检索才反映新内容。

以下示例使用默认前缀；修改 `gateway.api_prefix` 后需同步 URL。令牌通过请求头传递：

```bash
curl --fail-with-body -H "X-Echo-Agent-Token: $ECHO_ADMIN_TOKEN" \
  -F 'files=@/absolute/path/handbook.pdf' \
  'http://127.0.0.1:58123/api/v1/knowledge/upload?background=true'

curl --fail-with-body -H "X-Echo-Agent-Token: $ECHO_ADMIN_TOKEN" \
  'http://127.0.0.1:58123/api/v1/knowledge/jobs?limit=10'
```

后台响应表示索引任务已接收。观察 job 状态、进度与错误，直到成功；不能把上传 HTTP 成功当作全部文件已解析并可检索。`GET /knowledge/status` 可检查文档、分块、是否陈旧及上次重建时间。失败时保留原文并按 job 错误定位格式/依赖/embedding 问题，再重建。

## 实际工具入口

```json
{"query":"差旅报销流程","max_results":5}
```

以上为 `knowledge_search` 参数；`max_results` 默认 5，范围 1–20。结果含来源片段，回答时应核对原文与引用，不能将文档中的指令当作系统指令。

`knowledge_index` 接受 `{"action":"status"}` 或 `{"action":"rebuild"}`。`read_document` 接受本地 `path`，仅负责读取解析。不存在 `document.upload`、`document.list`、`knowledge.query` 工具，也没有 collection/top_k/threshold 管理契约。Dashboard 当前没有独立的知识问答表单，使用已认证聊天入口调用检索工具。

## 删除、取消与恢复

- Dashboard 删除需确认；后端为 `DELETE /knowledge/documents/{relative_path}?background=true`，嵌套路径须逐段 URL 编码，删除后重建索引。
- `POST /knowledge/rebuild?background=true` 显式重建；通过 `GET /knowledge/jobs/{id}` 查结果。
- `DELETE /knowledge/jobs/{id}` 请求取消索引，Dashboard 同样提供取消按钮。取消/失败不能被记为成功，应检查最终 job 状态与索引是否陈旧。

上传、替换、删除、重建和取消均要求 admin。列表和状态允许 API 读取权限。路径必须落在配置文档目录内，服务器执行规范相对路径与符号链接检查。

## 验收与备份

上传包含唯一无害标记的文档 → 等 job 成功 → 聊天检索并核对来源 → 替换标记并重建 → 确认不再返回旧内容 → 删除并重建 → 重启后再次查询。索引是派生数据，完整备份应保留原始文档与配置，损坏后可重新生成；产物下载与知识原始文件读取仍须遵守各自的权限边界。参见[备份与恢复](../operations/backup-restore.md)。
