# 升级与卸载

## 升级

=== "pip 升级"

    ```bash
    pip install --upgrade echo-agent[all]
    ```

    升级到指定版本：

    ```bash
    pip install "echo-agent[all]==0.3.8"
    ```

=== "源码升级"

    ```bash
    cd echo-agent
    git pull origin master
    cd echo-agent-runtime
    (cd web && pnpm install --frozen-lockfile && pnpm build)
    pip install -e ".[all]"
    ```

只下载了 Runtime 源码或发布压缩包时，保留独立工作区和配置，替换源码后重新安装；没有 `.git` 的解压目录不执行 `git pull`。正式发布压缩包已带 Dashboard，跳过前端构建。

---

### 升级前检查

!!! warning "升级注意事项"
    Beta 阶段版本间可能存在破坏性变更。升级前请：

    1. 阅读 [CHANGELOG](https://github.com/fuyuxiang/echo-agent/blob/master/echo-agent-runtime/CHANGELOG.md) 了解变更内容
    2. 备份数据目录
    3. 停止服务后备份完整工作区，再升级并验证配置

**备份数据：**

```bash
# 数据目录默认位置
tar -czf "$HOME/echo-agent-backup-$(date +%Y%m%d).tar.gz" -C "$HOME" .echo-agent
```

---

### 数据迁移

SQLite 表结构迁移在数据库初始化时自动执行。`echo-agent migrate` 只处理 USER 记忆归属键及旧记忆分片导入，不执行数据库表结构迁移。需要迁移记忆时，先运行 `echo-agent migrate status` 和 `echo-agent migrate run --dry-run`。完整流程见[升级与数据迁移](../operations/upgrade-migrations.md)。

---

### 检查点恢复

检查点可恢复 Agent 修改过的工作区文件：

```bash
# 查看可用的检查点
echo-agent checkpoint list

# 恢复到指定检查点
echo-agent checkpoint restore <checkpoint-id>
```

---

## 卸载

### 仅卸载包

```bash
pip uninstall echo-agent
```

### 完全清理

先停止并注销已安装的 Gateway 后台服务。确认已备份所需数据，再卸载包并删除默认工作区：

```bash
echo-agent gateway stop
echo-agent gateway uninstall
pip uninstall echo-agent
rm -rf ~/.echo-agent
rm -f ~/.local/bin/echo-agent
```

一键安装脚本默认将源码和虚拟环境放在 `~/.echo-agent/echo-agent/venv`；删除默认工作区时会一并删除。如果使用了自定义 `ECHO_INSTALL_DIR`，应先确认该目录内容，再单独处理。`pip uninstall` 只卸载当前 Python 环境中的包。

!!! warning "数据不可恢复"
    删除 `~/.echo-agent` 目录将永久清除所有数据，包括：

    - 配置文件 (`config.yaml`)
    - SQLite 数据库、会话文件与记忆文件
    - 积累的技能和进化记录
    - 定时任务配置

    请确保在删除前备份重要数据。

---

### 清理 Playwright 浏览器

如果安装了 Playwright 浏览器依赖：

Playwright 浏览器缓存可能由其他项目共用；清理前先查看当前安装版本的管理命令和缓存位置，避免影响其他项目。

---

### 清理前端构建产物

如果从源码安装并构建了前端：

```bash
cd echo-agent/echo-agent-runtime/web
rm -rf node_modules dist
```

---

## 版本降级

如果新版本存在问题需要回退：

安装指定旧版本：

```bash
pip install "echo-agent[all]==0.3.6"
```

!!! warning "checkpoint 不含数据库"
    `echo-agent checkpoint` 是工作区**文件**的影子 Git 快照，其排除范围包含 SQLite 数据库、会话目录、记忆目录与日志目录，因此 `checkpoint restore` 无法恢复这些数据。

    降级包不会逆向迁移数据库。应停止服务并恢复与旧版本对应的完整备份；`echo-agent migrate rollback` 只恢复最近的 USER 记忆文件迁移备份。参见[升级与数据迁移](../operations/upgrade-migrations.md)和[备份与恢复](../operations/backup-restore.md)。
