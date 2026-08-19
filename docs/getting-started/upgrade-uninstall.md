# 升级与卸载

## 升级

=== "pip 升级"

    ```bash
    pip install --upgrade echo-agent[all]
    ```

    升级到指定版本：

    ```bash
    pip install echo-agent[all]==0.3.7
    ```

=== "源码升级"

    ```bash
    cd echo-agent
    git pull origin master
    pip install -e ".[all]"
    ```

---

### 升级前检查

!!! warning "升级注意事项"
    Beta 阶段版本间可能存在破坏性变更。升级前请：

    1. 阅读 [CHANGELOG](https://github.com/fuyuxiang/echo-agent/blob/master/CHANGELOG.md) 了解变更内容
    2. 备份数据目录
    3. 执行数据库迁移（如需要）

**备份数据：**

```bash
# 数据目录默认位置
cp -r ~/.echo-agent ~/.echo-agent.backup.$(date +%Y%m%d)
```

---

### 数据库迁移

版本升级后如果数据库 schema 有变更，需要运行迁移命令：

```bash
echo-agent migrate
```

!!! note "自动迁移提示"
    `echo-agent run` 启动时会检测 schema 版本。如果需要迁移，会给出提示并拒绝启动，此时运行 `echo-agent migrate` 即可。

---

### 检查点恢复

如果升级后出现问题，可以回滚到之前的检查点：

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

卸载包并删除所有数据：

```bash
# 卸载 Python 包
pip uninstall echo-agent

# 删除数据目录（包含配置、数据库、记忆）
rm -rf ~/.echo-agent

# 如果使用了一键安装脚本，还需删除虚拟环境
rm -rf ~/.echo-agent/venv
rm -f ~/.local/bin/echo-agent
```

!!! warning "数据不可恢复"
    删除 `~/.echo-agent` 目录将永久清除所有数据，包括：

    - 配置文件 (`config.yaml`)
    - 对话历史和记忆数据库
    - 积累的技能和进化记录
    - 定时任务配置

    请确保在删除前备份重要数据。

---

### 清理 Playwright 浏览器

如果安装了 Playwright 浏览器依赖：

```bash
# 查看已安装的浏览器
playwright install --list

# 删除所有 Playwright 浏览器
rm -rf ~/.cache/ms-playwright        # Linux
rm -rf ~/Library/Caches/ms-playwright # macOS
```

---

### 清理前端构建产物

如果从源码安装并构建了前端：

```bash
cd echo-agent/web
rm -rf node_modules dist
```

---

## 版本降级

如果新版本存在问题需要回退：

```bash
# 安装指定旧版本
pip install echo-agent[all]==0.3.6

# 恢复数据库（如果已执行迁移）
echo-agent checkpoint restore <upgrade前的checkpoint-id>
```

!!! question "需维护者确认"
    数据库 schema 降级是否完全支持？当前 `migrate` 命令是否提供 `--downgrade` 选项？
