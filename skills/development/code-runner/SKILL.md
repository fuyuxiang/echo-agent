---
name: code-runner
description: "Execute Python code snippets in a sandboxed environment. Supports data analysis, visualization, and quick scripts."
version: 1.0.0
metadata:
  echo:
    tags: [Python, Code, Execute, Sandbox, DataAnalysis]
---

# Code Runner

Safe Python code execution with resource limits and import restrictions.

## Usage

```bash
python3 scripts/safe_exec.py "print(sum(range(100)))"
python3 scripts/safe_exec.py --file script.py
python3 scripts/safe_exec.py --timeout 10 "import time; time.sleep(5); print('done')"
```

## Security

Best-effort guards (NOT a true sandbox — code can still read the filesystem):
- **Timeout**: default 30 seconds, configurable
- **Memory limit**: 256MB on Linux (via resource.setrlimit); not enforced on macOS
- **Blocked patterns**: `os.system`, `subprocess`, `shutil.rmtree`, `__import__('os')`
- **AST check**: scans code for dangerous import patterns before execution
- **Isolated working dir**: runs in a fresh tmpdir, cleaned up after execution
- **Minimal env**: only PATH/HOME/LANG passed to subprocess

**Limitations**: This is NOT a security sandbox. The child process can read arbitrary
files on the host filesystem. For untrusted code, use a container-based executor
(e.g., docker-manage skill) instead.

## Allowed Libraries

Safe for use (common data/analysis):
- `math`, `statistics`, `decimal`, `fractions`
- `json`, `csv`, `re`, `datetime`, `collections`
- `pandas`, `numpy` (if installed)
- `matplotlib` (saves to file, no display)

## Blocked Patterns

```python
BLOCKED = [
    "os.system", "os.exec", "os.popen", "os.remove",
    "subprocess", "shutil.rmtree", "importlib",
    "__import__", "eval(", "exec(",
    "open('/etc", "open('/root",
]
```

## Example Workflows

Data analysis:
```python
import pandas as pd
df = pd.read_csv("/tmp/data.csv")
print(df.describe())
print(df.groupby("category")["amount"].sum())
```

Quick plot (saved to file):
```python
import matplotlib.pyplot as plt
plt.plot([1,2,3,4], [1,4,2,3])
plt.savefig("/tmp/plot.png")
print("Plot saved to /tmp/plot.png")
```
