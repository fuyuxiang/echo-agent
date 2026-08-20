# 升级与迁移

Echo Agent 的版本升级流程和数据库迁移操作指南。

---

## 版本策略

Echo Agent 遵循语义化版本 (SemVer)：

| 版本段 | 含义 | 数据兼容性 |
|--------|------|-----------|
| MAJOR (x.0.0) | 重大架构变更 | 可能需手动迁移 |
| MINOR (0.x.0) | 新功能 | 自动迁移 |
| PATCH (0.0.x) | Bug 修复 | 完全兼容 |

当前版本：**v0.3.7 Beta**

!!! warning "Beta 阶段注意"
    Beta 期间 MINOR 版本升级可能包含破坏性变更。升级前务必阅读 Changelog 并备份数据。

---

## 升级流程

### 升级前清单

1. **阅读 Changelog** — 确认目标版本的变更内容和破坏性变更
2. **备份数据** — 完整备份 `~/.echo-agent/` 目录
3. **检查兼容性** — 确认 Python 版本满足要求（3.11+）
4. **确认迁移需求** — 查看是否需要数据库迁移
5. **选择升级窗口** — 避免在活跃任务执行期间升级

### 执行升级

```bash
# 1. 停止服务
echo-agent gateway stop

# 2. 备份（关键步骤）
tar -czf ~/.echo-agent-backup-$(date +%Y%m%d).tar.gz ~/.echo-agent/

# 3. 升级包
pip install --upgrade echo-agent[all]

# 4. 检查迁移状态
echo-agent migrate status

# 5. 执行迁移（如需要）
echo-agent migrate run

# 6. 验证
echo-agent status

# 7. 重启服务
echo-agent gateway start
```

!!! tip "升级后验证"
    升级后建议执行 `echo-agent config validate` 检查配置文件是否与新版本兼容。

---

## 数据库迁移

### migrate 命令

Echo Agent 提供内置迁移工具，管理数据库 schema 变更：

```bash
# 查看当前迁移状态
echo-agent migrate status

# 执行待处理的迁移
echo-agent migrate run

# 回滚最近一次迁移
echo-agent migrate rollback

# 迁移 memory.md 格式（特殊迁移）
echo-agent migrate memory-md
```

### migrate status 输出示例

```
数据库版本: v12
最新版本:   v14
待执行迁移:
  - v13: 添加 knowledge_chunks 索引
  - v14: 记忆关系表重构
```

### 执行迁移

```bash
$ echo-agent migrate run
[1/2] 执行迁移 v13: 添加 knowledge_chunks 索引 ... 完成 (0.3s)
[2/2] 执行迁移 v14: 记忆关系表重构 ... 完成 (1.2s)
全部迁移完成。当前版本: v14
```

!!! danger "迁移前必须备份"
    数据库迁移修改 schema 和数据，某些迁移不可逆。在执行 `migrate run` 前必须确保有可用的备份。

### 迁移回滚

如果迁移后发现问题：

```bash
# 回滚最近一次迁移
echo-agent migrate rollback

# 多次回滚
echo-agent migrate rollback   # 回滚 v14
echo-agent migrate rollback   # 回滚 v13
```

!!! warning "回滚限制"
    并非所有迁移都支持回滚。涉及数据删除或格式转换的迁移可能标记为不可逆。此时只能从备份恢复。

### memory-md 迁移

将旧版 memory.md 格式的记忆数据迁移到结构化存储：

```bash
echo-agent migrate memory-md
```

此命令会：

1. 扫描 memory.md 文件
2. 解析记忆条目
3. 导入到 SQLite 记忆表
4. 保留原始文件作为备份

---

## 跨大版本升级

跨多个 MINOR 版本升级时，迁移将按顺序逐步执行：

```bash
# 例：从 v0.3.2 升级到 v0.3.7
$ echo-agent migrate status
待执行迁移:
  - v0.3.3: ...
  - v0.3.4: ...
  - v0.3.5: ...
  - v0.3.6: ...
  - v0.3.7: ...

$ echo-agent migrate run
# 按顺序执行所有待处理迁移
```

!!! tip "逐版本升级"
    如果跨越多个版本，建议先查阅每个中间版本的 Changelog，了解累计的破坏性变更。

---

## 配置文件变更

版本升级可能引入新的配置项或废弃旧配置：

```bash
# 验证当前配置
echo-agent config validate

# 查看配置说明（包含新增和废弃项）
echo-agent config explain

# 生成当前版本的完整默认配置
echo-agent config gen-docs
```

### 配置加载优先级

升级后如果遇到配置冲突，了解加载顺序有助于排查：

```
包默认值 → 用户 YAML (-c 或 ~/.echo-agent) → ECHO_AGENT_ 环境变量 → CLI 参数 → profile 默认值 → Pydantic 校验
```

---

## 故障恢复

### 升级失败回退

```bash
# 1. 停止服务
echo-agent gateway stop

# 2. 回退 Python 包
pip install echo-agent==0.3.6   # 回退到之前版本

# 3. 回滚数据库（如已执行迁移）
echo-agent migrate rollback

# 4. 或从备份恢复
rm -rf ~/.echo-agent
tar -xzf ~/.echo-agent-backup-20240101.tar.gz -C ~/

# 5. 重启
echo-agent gateway start
```

### 常见升级问题

| 问题 | 原因 | 解决 |
|------|------|------|
| 迁移失败中断 | Schema 冲突或磁盘空间不足 | 从备份恢复，检查磁盘空间 |
| 配置验证报错 | 废弃配置项 | 运行 `config validate` 查看具体项 |
| 服务无法启动 | 依赖版本不兼容 | 检查 Python 版本，重装依赖 |
| 记忆数据丢失 | 迁移 Bug | 从备份恢复，报告 Issue |

!!! note "没有一站式的 upgrade 命令"
    不存在 `echo-agent upgrade`。升级需按本页顺序手动执行：停止服务 → 备份 `data/echo_agent.db` → `pip install -U` → `echo-agent migrate run` → `echo-agent gateway restart`。

    源码安装是例外：重复执行 `install.sh` 即为升级，脚本会检测到已有配置并跳过配置向导。但它同样不代替数据库备份，执行前请自行复制。
