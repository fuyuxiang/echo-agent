# Execution Backends

Echo Agent provides three execution backends for running code and commands. Each backend offers different tradeoffs in isolation, performance, and capabilities. Choose the appropriate backend based on your requirements.

## Overview

| Backend | Tools | Isolation | Performance | Use Case |
|---------|-------|-----------|-------------|----------|
| Shell | `shell`, `code_exec` | None (full system access) | Highest | Quick command execution, scripting |
| Container | Docker container | Strong (sandboxed) | Moderate (startup overhead) | Untrusted code, multi-tenant environments |
| Process | `process` | Process-level | High | Long-running services, interactive processes |

## Shell Backend

The Shell backend executes commands and code directly on the host system via the `shell` and `code_exec` tools.

!!! warning "Security Warning"
    The Shell backend has full system access. Executed commands can read/write arbitrary files, access the network, and install packages. Only use this backend in trusted environments.

### Tools

- **shell tool**: Executes shell commands directly (`risk_level = "exec"`)
- **code_exec tool**: Executes code snippets in various languages (`risk_level = "exec"`)

### Configuration

```yaml
execution:
  backend: shell
  shell:
    # Default shell path
    command: /bin/bash
    # Command execution timeout (seconds)
    timeout: 30
    # Working directory
    working_dir: /workspace
    # Environment variables
    env:
      PATH: /usr/local/bin:/usr/bin:/bin
      LANG: en_US.UTF-8
```

### Timeout Control

The Shell backend supports command-level timeout settings to prevent commands from hanging indefinitely:

```yaml
execution:
  shell:
    timeout: 30          # Default timeout: 30 seconds
    max_timeout: 300     # Maximum allowed timeout: 5 minutes
```

## Container Backend

The Container backend provides fully isolated execution environments via Docker. It is suitable for running untrusted code or scenarios requiring environment consistency.

!!! question "Needs Maintainer Confirmation"
    The following container configuration details (default resource limits, network policies, image pull policies) need confirmation from maintainers based on the actual deployment environment.

### Docker Setup

Ensure Docker is installed on the host and the Echo Agent process has permission to access the Docker daemon:

```bash
# Verify Docker is available
docker info

# Confirm user is in the docker group
groups | grep docker
```

### Configuration

```yaml
execution:
  backend: container
  container:
    # Base image
    image: echo-agent/sandbox:latest
    # Resource limits
    resources:
      memory: 512m
      cpu_count: 2
      pids_limit: 100
    # Network configuration
    network: none          # Disable network access
    # Auto cleanup
    auto_remove: true
    # Execution timeout
    timeout: 60
    # Volume mounts (read-only)
    volumes:
      - source: /data/shared
        target: /mnt/shared
        read_only: true
```

### Image Configuration

!!! question "Needs Maintainer Confirmation"
    The build process and pre-installed toolchain for the default sandbox image need confirmation.

```yaml
execution:
  container:
    image: echo-agent/sandbox:latest
    # Image pull policy
    pull_policy: if_not_present  # always | never | if_not_present
```

### Resource Limits

The Container backend allows precise control over resource usage:

```yaml
execution:
  container:
    resources:
      memory: 512m         # Memory limit
      cpu_count: 2         # CPU cores
      pids_limit: 100      # Maximum process count
      disk_size: 1g        # Disk quota
```

### Volume Mounts

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

!!! warning "Security Warning"
    Avoid mounting sensitive directories (such as `/etc`, `~/.ssh`, `~/.aws`) as writable into containers. Always use `read_only: true` unless write access is genuinely required.

## Process Backend

The Process backend manages long-running subprocesses with stdin/stdout interactive communication. It is suitable for running services, REPLs, or programs requiring ongoing interaction.

### Tools

- **process tool**: Manages subprocess lifecycle (`risk_level = "exec"`), supporting start, stop, send input, and read output operations

### Lifecycle Management

The Process backend manages the full lifecycle of a process:

1. **Start**: Creates a subprocess and assigns an ID
2. **Interact**: Sends input via stdin, reads output from stdout/stderr
3. **Monitor**: Checks process status and resource usage
4. **Terminate**: Gracefully stops or forcefully kills the process

### Configuration

```yaml
execution:
  backend: process
  process:
    # Maximum concurrent processes
    max_concurrent: 5
    # Process idle timeout (seconds)
    idle_timeout: 300
    # stdin/stdout buffer size
    buffer_size: 65536
    # Default working directory
    working_dir: /workspace
```

### stdin/stdout Handling

The Process backend communicates with subprocesses via pipes:

```yaml
execution:
  process:
    # I/O encoding
    encoding: utf-8
    # stdout read timeout
    read_timeout: 10
    # Whether to merge stderr into stdout
    merge_stderr: false
```

## Backend Comparison

| Feature | Shell | Container | Process |
|---------|-------|-----------|---------|
| **Isolation Level** | None | Container-level (strong) | Process-level (weak) |
| **Startup Speed** | Instant | Slower (container creation) | Fast |
| **Resource Control** | None | Precise (cgroups) | Limited |
| **Network Access** | Full | Configurable | Full |
| **Filesystem Access** | Full | Restricted (mounts) | Full |
| **Persistence** | None (one-shot) | None (destroyed by default) | Yes (process lifetime) |
| **Interactivity** | Single command | Single command | Continuous interaction |
| **Best For** | Quick scripts | Sandboxed execution | Services/REPLs |

## Security Best Practices

### General Recommendations

1. **Principle of Least Privilege**: Prefer the Container backend for untrusted code execution
2. **Timeout Configuration**: Always configure timeouts to prevent resource exhaustion
3. **Resource Limits**: Set reasonable memory and CPU limits for Container backends
4. **Audit Logging**: Log all execution operations for post-hoc auditing

### Shell Backend Security

!!! warning "Security Warning"
    The Shell backend provides no isolation. The following measures can only reduce risk, not eliminate it.

- Restrict commands to an allowlist
- Set strict timeout values
- Avoid running as root
- Use `working_dir` to constrain the working directory

### Container Backend Security

- Use `network: none` to disable networking unless required
- Set the root filesystem to `read_only`
- Limit `pids_limit` to prevent fork bombs
- Never mount the Docker socket
- Regularly update base images

### Process Backend Security

- Limit the maximum number of concurrent processes
- Set idle timeouts for automatic cleanup
- Monitor process resource usage
- Avoid running subprocesses with elevated privileges
