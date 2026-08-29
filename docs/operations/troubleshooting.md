# 故障排查

按症状索引的故障排查指南。每个问题按 **症状 → 原因 → 检查 → 修复** 的结构组织。

---

## Gateway 无法启动

### 症状

执行 `echo-agent gateway start` 后服务立即退出，`status` 显示 inactive。

### 常见原因

| 原因 | 可能性 |
|------|--------|
| 端口被占用 | 高 |
| 配置文件语法错误 | 高 |
| Python 依赖缺失 | 中 |
| 数据目录权限不足 | 中 |
| 数据库文件损坏 | 低 |

### 检查步骤

```bash
# 1. 查看服务日志
echo-agent gateway logs

# 2. 尝试前台启动（直接看报错）
echo-agent run

# 3. 检查端口占用
ss -tlnp | grep 58123    # Linux
lsof -i :58123           # macOS

# 4. 验证配置
echo-agent config validate

# 5. 检查依赖
echo-agent deps status
```

### 修复

```bash
# 端口占用：修改端口或停止占用进程
echo-agent gateway stop
kill $(lsof -t -i :58123)

# 配置错误：修复 YAML 语法
echo-agent config validate   # 会指出具体行号

# 依赖缺失：重装
echo-agent deps install

# 权限修复
chmod 700 ~/.echo-agent
chmod 700 ~/.echo-agent/data
```

---

## CLI 连接 Gateway 失败

### 症状

执行 `echo-agent cli` 报错 "Connection refused" 或 "Authentication failed"。

### 常见原因

| 原因 | 表现 |
|------|------|
| Gateway 未运行 | Connection refused |
| URL 配置错误 | Connection refused |
| Token 无效 | 401 Unauthorized |
| Origin/Host 被拒 | 403 Forbidden |

### 检查步骤

```bash
# 1. 确认 Gateway 在运行
echo-agent gateway status

# 2. 测试网络连通性
curl -v http://localhost:58123/health

# 3. 测试认证
curl -H "X-Echo-Agent-Token: your-token" http://localhost:58123/health

# 4. 检查 Gateway 日志中的拒绝记录
echo-agent gateway logs | grep -i "rejected\|forbidden\|unauthorized"
```

### 修复

```bash
# Gateway 未运行
echo-agent gateway start

# Token 无效：确认 token 在配置中
echo-agent config dump | grep api_tokens

# Origin 被拒：添加客户端来源到白名单
# 编辑 ~/.echo-agent/config.yaml 的 allowed_origins
```

---

## 模型调用超时

### 症状

Agent 响应极慢或报错 "Model request timeout"。

### 常见原因

| 原因 | 检查方式 |
|------|---------|
| 网络问题 | `curl` 测试 API 端点 |
| API Key 无效 | 查看模型服务返回码 |
| 模型服务过载 | 检查提供商状态页 |
| 代理配置错误 | 检查 proxy 设置 |
| 上下文过长 | 检查 token 使用量 |

### 检查步骤

```bash
# 1. 查看详细日志
echo-agent run --log-level DEBUG

# 2. 测试 API 连通性（以 OpenAI 为例）
curl -H "Authorization: Bearer $OPENAI_API_KEY" \
  https://api.openai.com/v1/models

# 3. 检查代理设置
env | grep -i proxy

# 4. 查看最近的成本数据（判断是否 token 过量）
echo-agent cost
```

### 修复

```bash
# 网络超时：增加超时时间
# ~/.echo-agent/config.yaml
# network:
#   timeout:
#     read: 120

# API Key 问题：更新密钥
echo-agent setup   # 重新配置

# 上下文过长：限制历史长度
# runtime:
#   memory:
#     session_history_limit: 30
```

---

## 记忆检索不到预期内容

### 症状

Agent 无法回忆起之前的对话内容或知识。

### 常见原因

| 原因 | 说明 |
|------|------|
| 记忆未持久化 | 前台模式异常退出 |
| 检索阈值过高 | 相似度得分不足 |
| 会话隔离 | 记忆属于其他会话/工作区 |
| 数据库损坏 | 意外断电或强制终止 |

### 检查步骤

```bash
# 1. 确认数据库完整
sqlite3 ~/.echo-agent/data/echo_agent.db "PRAGMA integrity_check;"

# 2. 查看记忆条目数
sqlite3 ~/.echo-agent/data/echo_agent.db "SELECT COUNT(*) FROM memory;"

# 3. 搜索特定关键词
sqlite3 ~/.echo-agent/data/echo_agent.db \
  "SELECT id, content FROM memory WHERE content LIKE '%关键词%' LIMIT 5;"

# 4. 检查日志中的记忆写入记录
echo-agent gateway logs | grep -i "memory.*save\|memory.*store"
```

### 修复

数据库损坏时从 SQLite 备份恢复，参见[备份与恢复](backup-restore.md#restore-sqlite-backup)。`echo-agent checkpoint` 是工作区文件的影子 Git 快照，其排除范围包含数据库与记忆目录，无法用于恢复数据库。

检索召回不足时，调整记忆检索与知识库的相关配置：

```yaml
memory:
  rerankTopK: 20          # 扩大参与精排的候选数（默认 10）
  rerankMinScore: 0.0     # 相关性下限，0 表示只重排不剔除
knowledge:
  maxResults: 10          # 知识检索返回的最大结果数（默认 5）
  chunkSize: 800          # 缩小分块粒度，提高命中精度（默认 1200）
```

---

## 磁盘空间不足

### 症状

服务报错 "disk full" 或写入操作失败。

### 常见原因

| 路径 | 增长原因 |
|------|---------|
| `data/logs/` | 日志未轮转 |
| `data/spill/` | 溢写文件未清理 |
| `data/echo_agent.db` | 数据库膨胀 |
| `data/checkpoints/` | 检查点积累 |

### 检查步骤

```bash
# 1. 查看数据目录大小分布
du -sh ~/.echo-agent/data/*

# 2. 查看磁盘整体使用
df -h

# 3. 找出最大文件
find ~/.echo-agent -type f -size +100M
```

### 修复

```bash
# 清理溢写文件
rm -rf ~/.echo-agent/data/spill/*

# 清理旧检查点
echo-agent checkpoint prune

# 压缩数据库
sqlite3 ~/.echo-agent/data/echo_agent.db "VACUUM;"

# 清理旧日志（保留最近 7 天）
find ~/.echo-agent/data/logs -name "*.gz" -mtime +7 -delete
```

!!! tip "预防措施"
    收紧溢写保留策略，避免磁盘空间耗尽：
    ```yaml
    spill:
      max_total_mb: 512      # 溢写目录总大小上限
      retention_days: 7      # 保留天数
      sweep_interval_hours: 6
    ```
    日志轮转由代码内的 loguru 配置决定，没有对应的配置字段；日志目录是 `storage.logs_dir`。

---

## 服务停止超时

### 症状

`echo-agent gateway stop` 长时间无响应，最终被强制终止。

### 原因

Gateway 停止时等待当前任务完成，超时 60 秒。长时间运行的工具或模型调用可能导致等待。

### 检查步骤

```bash
# 查看当前活跃任务
echo-agent gateway status

# 查看进程状态
ps aux | grep echo-agent
```

### 修复

```bash
# 等待 60 秒自动强制终止
# 或手动强制停止
echo-agent gateway stop
# 如果仍未停止：
kill -9 $(pgrep -f "echo-agent.*gateway")
```

!!! danger "强制终止风险"
    `kill -9` 会跳过优雅关闭流程，可能导致数据库写入中断。恢复后建议检查数据完整性：
    ```bash
    sqlite3 ~/.echo-agent/data/echo_agent.db "PRAGMA integrity_check;"
    ```

---

## 配置加载异常

### 症状

配置修改后未生效，或启动时报配置错误。

### 常见原因

| 原因 | 说明 |
|------|------|
| YAML 语法错误 | 缩进、冒号、引号问题 |
| 高优先级覆盖 | 环境变量或 CLI 参数覆盖了文件配置 |
| 配置路径错误 | `-c` 指向了错误的文件 |
| 旧版配置项 | 升级后配置项名称变更 |

### 检查步骤

```bash
# 1. 验证配置语法
echo-agent config validate

# 2. 查看最终生效的配置
echo-agent config dump

# 3. 查看配置解释（含来源）
echo-agent config explain

# 4. 检查环境变量覆盖
env | grep ECHO_AGENT_
```

### 修复

```bash
# 查看配置加载优先级
# 包默认值 → 用户 YAML → ECHO_AGENT_ 环境变量 → CLI 参数 → profile → Pydantic 校验

# 移除冲突的环境变量
unset ECHO_AGENT_GATEWAY_PORT

# 修复 YAML 语法
echo-agent config validate   # 报错会指出行号
```

---

## 工具执行失败

### 症状

Agent 调用工具时报错 "Tool execution failed" 或返回空结果。

### 常见原因

| 原因 | 检查方式 |
|------|---------|
| 工具被禁用 | 检查 tools.profile |
| 权限不足 | 检查 security.profile |
| 外部依赖缺失 | 检查工具所需的命令/API |
| 超时 | 检查工具执行耗时 |

### 检查步骤

```bash
# 1. 查看已启用的工具
echo-agent plugin list

# 2. 检查工具详情
echo-agent plugin info <tool-name>

# 3. 检查工具健康状态
echo-agent plugin check

# 4. 查看 DEBUG 日志中的工具调用细节
echo-agent run --log-level DEBUG
```

### 修复

```bash
# 启用被禁用的工具
echo-agent plugin enable <tool-name>

# 提升 tools profile
# tools:
#   profile: coding

# 安装缺失依赖
echo-agent deps install
```

---

## 诊断信息收集

报告问题时，请收集以下信息：

```bash
# 版本信息
echo-agent --version

# 运行状态
echo-agent status

# 配置摘要（注意脱敏）
echo-agent config dump | grep -v "token\|key\|secret"

# 最近日志
echo-agent gateway logs 2>&1 | tail -50

# 依赖状态
echo-agent deps status

# 迁移状态
echo-agent migrate status
```

!!! warning "日志脱敏"
    分享日志前请检查并移除 API Key、Token 等敏感信息。
