# Task 5A 报告 — security 模块表征测试

> 完成时间：2026-06-24

## 分组汇总

### 1. security/tokenizer.py（行 85-111）
- **测试文件**：`tests/test_coverage_security_tokenizer.py`（新建）
- **commit**：`2245253`
- **新增测试数**：17
- **覆盖分支**：
  - 管道 `|`（单管道、三段链）
  - 分号 `;`（单、多）
  - `&&`（单、三段链）
  - `||`
  - 反引号 `` `cmd` ``（简单、赋值形式）
  - `$(cmd)` 子shell（简单、嵌套、含管道）
  - `<(cmd)` 进程替换（双参数）
  - 混合（管道+分号、子shell+管道）
  - 空命令兜底、纯命令透传
- **疑似 bug**：无

---

### 2. security/smart_approval.py（行 75-88）
- **测试文件**：`tests/test_coverage_smart_approval.py`（新建）
- **commit**：`e489b66`
- **新增测试数**：13
- **覆盖分支**：
  - `APPROVE` → `"approve"`
  - `DENY` → `"deny"`
  - `ESCALATE` → `"escalate"`
  - 无法识别响应（"MAYBE"）→ `"escalate"`（兜底升级）
  - 空/仅空白响应 → `"unavailable"`
  - provider 抛 `RuntimeError` → `"unavailable"`
  - provider 抛 `TimeoutError` → `"unavailable"`
  - `router=None` 时直接用原 provider
  - router 无 `resolve` 属性时忽略
  - `router.resolve` 返回 `(None, model)` 时降级回原 provider
  - `router.resolve` 返回新 provider 时替换
- **注意**：brief 中提到返回 `SmartApprovalResult.decision`，但实际 `smart_approve()` 直接返回字符串字面量，无 dataclass 包装。以实际行为写断言，无 bug。

---

### 3. security/tool_policy.py（行 117）+ security/path_policy.py（行 218）
- **测试文件**：`tests/test_coverage_tool_path_policy.py`（新建）
- **commit**：`c742876`
- **新增测试数**：9
- **覆盖分支**：
  - tool_policy：deny 列表命中 → `False`（高优先级）
  - tool_policy：deny 列表覆盖 allow 列表（deny 优先）
  - tool_policy：deny 只拦截命中工具，不影响其他
  - tool_policy：deny 列表接受任意工具名（含 Tool 对象）
  - tool_policy：空 deny 列表不拒绝
  - path_policy：`~/.ssh/my_custom_key` 靠目录前缀拦截（非标准文件名）
  - path_policy：`~/.gnupg/` 子目录文件拦截
  - path_policy：`~/.aws/some-profile` 拦截
  - path_policy：tmp_path 下普通文件不被拒绝
- **疑似 bug**：无

---

### 4. security/guards.py（行 198-199）
- **测试文件**：`tests/test_coverage_guards_double_scan.py`（新建）
- **commit**：`2442c0f`
- **新增测试数**：7
- **覆盖分支**：
  - 原始串含 `/etc/passwd` → `sensitive_account_file` hard_block
  - ANSI-C quote 包装的 `rm -rf /` → hard_block（normalize 解码后命中）
  - percent-encode 命令（`command != normalized`）走二次扫描路径
  - `seen_keys` 去重：同 key 不重复记录（验证双扫不产生重复 finding）
  - 安全命令无 finding
  - `rm -rf /` 直接命中 `root_rm` hard_block
  - `rm -rf /tmp/data` 命中 `recursive_delete` approval（非 hard_block）
- **疑似 bug**：无

---

## 总计

| 文件 | 新增测试数 | commit |
|---|---|---|
| test_coverage_security_tokenizer.py | 17 | 2245253 |
| test_coverage_smart_approval.py | 13 | e489b66 |
| test_coverage_tool_path_policy.py | 9 | c742876 |
| test_coverage_guards_double_scan.py | 7 | 2442c0f |
| **合计** | **46** | |

## 跳过的项

无跳过项。所有 5 个缺口均已覆盖（item 2 & 3 合并为一个文件，item 3 & 4 实为 tool_policy + path_policy 两个模块合并提交）。

## 发现的疑似 bug

无。
