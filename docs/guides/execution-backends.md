# 执行后端

Echo Agent 提供三种执行后端，用于运行代码和命令。每种后端在隔离性、性能和适用场景上各有侧重，开发者应根据实际需求选择合适的后端。

## 概览

| 后端 | 工具 | 隔离性 | 性能 | 适用场景 |
|------|------|--------|------|----------|
| Shell | `shell`、`code_exec` | 无隔离，完全系统访问 | 最高 | 快速命令执行、脚本运行 |
| Container | Docker 容器 | 强隔离（沙箱） | 中等（启动开销） | 不可信代码、多租户环境 |
| Process | `process` | 进程级隔离 | 高 | 长时间运行的服务、交互式进程 |

## Shell 后端

Shell 后端通过 `shell` 和 `code_exec` 工具直接在宿主系统上执行命令和代码。

!!! warning "安全警告"
    Shell 后端拥有完全的系统访问权限。执行的命令可以读写任意文件、访问网络、安装软件包。仅在可信环境中使用此后端。

### 工具说明

- **shell 工具**：直接执行 shell 命令（`risk_level = "exec"`）
- **code_exec 工具**：执行多种语言的代码片段（`risk_level = "exec"`）

### 配置示例

```yaml
execution:
  backend: shell
  shell:
    # 默认 shell 路径
    command: /bin/bash
    # 命令执行超时（秒）
    timeout: 30
    # 工作目录
    working_dir: /workspace
    # 环境变量
    env:
      PATH: /usr/local/bin:/usr/bin:/bin
      LANG: en_US.UTF-8
```

### 超时控制

Shell 后端支持命令级别的超时设置，防止命令无限期挂起：

```yaml
execution:
  shell:
    timeout: 30          # 默认超时 30 秒
    max_timeout: 300     # 最大允许超时 5 分钟
```

## Container 后端

Container 后端通过 Docker 提供完全隔离的执行环境，适用于运行不可信代码或需要环境一致性的场景。

!!! question "需维护者确认"
    以下容器配置细节（资源限制默认值、网络策略、镜像拉取策略）需要维护者根据实际部署环境确认。

### Docker 环境准备

确保宿主机已安装 Docker 并且 Echo Agent 进程有权限访问 Docker daemon：

```bash
# 验证 Docker 可用
docker info

# 确认用户在 docker 组中
groups | grep docker
```

### 配置示例

```yaml
execution:
  backend: container
  container:
    # 基础镜像
    image: echo-agent/sandbox:latest
    # 资源限制
    resources:
      memory: 512m
      cpu_count: 2
      pids_limit: 100
    # 网络配置
    network: none          # 禁用网络访问
    # 自动清理
    auto_remove: true
    # 执行超时
    timeout: 60
    # 卷挂载（只读）
    volumes:
      - source: /data/shared
        target: /mnt/shared
        read_only: true
```

### 镜像配置

!!! question "需维护者确认"
    默认沙箱镜像的构建方式与预装工具链需确认。

```yaml
execution:
  container:
    image: echo-agent/sandbox:latest
    # 镜像拉取策略
    pull_policy: if_not_present  # always | never | if_not_present
```

### 资源限制

Container 后端可以精确控制资源使用：

```yaml
execution:
  container:
    resources:
      memory: 512m         # 内存上限
      cpu_count: 2         # CPU 核数
      pids_limit: 100      # 最大进程数
      disk_size: 1g        # 磁盘配额
```

### 卷挂载

```yaml
execution:
  container:
    volumes:
      - source: ./workspace
        target: /workspace
        read_only: false
      - source: /etc/ssl/certs
        target: /etc/ssl/certs
        read_only: true
```

!!! warning "安全警告"
    避免将敏感目录（如 `/etc`、`~/.ssh`、`~/.aws`）以可写方式挂载到容器中。始终使用 `read_only: true`，除非确实需要写入。

## Process 后端

Process 后端用于管理长时间运行的子进程，支持 stdin/stdout 交互式通信，适合运行服务、REPL 或需要持续交互的程序。

### 工具说明

- **process 工具**：管理子进程的生命周期（`risk_level = "exec"`），支持启动、停止、发送输入、读取输出

### 生命周期管理

Process 后端管理进程的完整生命周期：

1. **启动**：创建子进程并分配 ID
2. **交互**：通过 stdin 发送输入，从 stdout/stderr 读取输出
3. **监控**：检查进程状态、资源使用
4. **终止**：优雅停止或强制终止进程

### 配置示例

```yaml
execution:
  backend: process
  process:
    # 最大并发进程数
    max_concurrent: 5
    # 进程空闲超时（秒）
    idle_timeout: 300
    # stdin/stdout 缓冲区大小
    buffer_size: 65536
    # 默认工作目录
    working_dir: /workspace
```

### stdin/stdout 处理

Process 后端通过管道与子进程通信：

```yaml
execution:
  process:
    # I/O 编码
    encoding: utf-8
    # stdout 读取超时
    read_timeout: 10
    # 是否合并 stderr 到 stdout
    merge_stderr: false
```

## 后端对比

| 特性 | Shell | Container | Process |
|------|-------|-----------|---------|
| **隔离级别** | 无 | 容器级（强） | 进程级（弱） |
| **启动速度** | 即时 | 较慢（容器创建） | 快速 |
| **资源控制** | 无 | 精确（cgroups） | 有限 |
| **网络访问** | 完全 | 可配置 | 完全 |
| **文件系统访问** | 完全 | 受限（挂载） | 完全 |
| **持久性** | 无（一次性） | 无（默认销毁） | 有（进程存活期间） |
| **交互能力** | 单次命令 | 单次命令 | 持续交互 |
| **适用场景** | 快速脚本 | 沙箱执行 | 服务/REPL |

## 安全最佳实践

### 通用建议

1. **最小权限原则**：优先使用 Container 后端执行不可信代码
2. **超时设置**：始终配置超时，防止资源耗尽
3. **资源限制**：为 Container 后端设置合理的内存和 CPU 限制
4. **日志审计**：记录所有执行操作以便事后审计

### Shell 后端安全

!!! warning "安全警告"
    Shell 后端不提供任何隔离。以下措施仅能降低风险，不能消除风险。

- 限制可执行的命令白名单
- 设置严格的超时时间
- 避免以 root 身份运行
- 使用 `working_dir` 限制工作目录

### Container 后端安全

- 使用 `network: none` 禁用网络（除非必需）
- 设置 `read_only` 根文件系统
- 限制 `pids_limit` 防止 fork 炸弹
- 不挂载 Docker socket
- 定期更新基础镜像

### Process 后端安全

- 限制最大并发进程数
- 设置空闲超时自动清理
- 监控进程资源使用
- 避免以提升权限运行子进程
