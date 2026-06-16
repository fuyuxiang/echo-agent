# M1 — 安全默认档与凭证密钥 设计文档

> 日期：2026-06-16
> 范围：`docs/review-2026-06.md` 里程碑 M1 中的 P0-2（凭证密钥）+ P0-4（服务端默认安全档）
> 类型：默认值 + 接线止血，非重写。SSRF（P1-6）留到 M4，不在本范围。

## 背景与定位

评审报告把 Echo 与生产级的差距归为三类低成本高收益的修补：接线、默认值、门控。M1 聚焦「默认值类」——机制都在，缺的是安全默认档。

复核现状时发现，P0-2 / P0-4 的现状比评审报告（同日）记录的更靠前，因此本设计基于**实测代码**重新定位缺口：

- **P0-2**：`require_encryption` 默认已是 `True`（`config/schema.py:357`），loop 也已 config 驱动（`agent/loop.py:160`），「静默明文落盘」基本已堵上。**真正剩余的缺口有两个**：
  1. 弱 KDF：`sha256(secret)` 单轮无盐派生 Fernet key（`permissions/manager.py:341`）。
  2. setup 向导不生成、不引导 `ECHO_AGENT_CREDENTIAL_KEY`——在 `require_encryption=True` 下，新用户一存凭证就因无 key 抛错，**可用性断裂**。
- **P0-4**：`public_gateway` / `daemon` 收紧档机制完整且生效（`security/tool_policy.py:127-139` 有 DENY 清单 + capability 拦截）。**真正剩余的缺口是**：`run_gateway`（`app.py:356`）只强制 `gateway.enabled=True`，**没切 `security.profile`**——公网 gateway 入口默认跑在最宽松的 `personal_cli` 档。

两块相互独立，合成一个 PR 收口。每块都补「修复前失败、修复后通过」的回归测试作护栏。

## P0-2 — 凭证密钥管理

### 现状取证

- `CredentialManager.__init__(require_encryption=False)` 是函数默认（`permissions/manager.py:319`），但运行路径走 config，schema 默认 `require_encryption=True`（`config/schema.py:357`），loop 传入 `config.credentials.require_encryption`（`agent/loop.py:160`）。
- `_fernet()` 用 `base64.urlsafe_b64encode(hashlib.sha256(secret.encode()).digest())` 单轮无盐派生 Fernet key（`permissions/manager.py:341`）——弱 KDF。
- `_encode_secret` 在无 key 时仅 warning 后 `return "plain", value`（`permissions/manager.py:347-350`）；但因 `require_encryption=True`，`_fernet()` 会先在 `:331-334` 抛错，明文回退路径实际不可达。
- setup 向导（`cli/setup.py`）将各 provider 的 API key 存入配置文件的 `apiKey` 字段，**全程不涉及** `ECHO_AGENT_CREDENTIAL_KEY` 的生成或引导。
- 版本 0.2.3（`pyproject.toml`），早期阶段，复核未发现存量凭证存储文件。

后果：开箱即用断裂——新用户启用凭证存储即崩；即便配了 key，KDF 也偏弱。

### 设计决策（已与用户确认）

- key 来源：**setup 自动生成并落盘**。
- KDF：**改为直存合法 Fernet key**，不再过 KDF（用户不需记忆口令，从根上消除弱 KDF 问题）。
- 存量：**不做迁移**（无存量可迁）；旧 sha256-KDF 密文与新 key 不兼容，遇到时**清晰报错引导重录**，不静默吞。
- key 文件位置：工作区根目录 `.credential_key`（与会话/记忆/凭证同处工作区，本地优先）。

### 改动

1. **新增模块 `echo_agent/permissions/credential_key.py`**，单一职责——解析或生成一个合法 Fernet key：

   ```
   resolve_or_create_key(workspace: Path, env_name="ECHO_AGENT_CREDENTIAL_KEY") -> bytes
     1. env 有值 → 校验是合法 Fernet key（44 字节 urlsafe-base64）；合法返回，非法抛清晰错误
     2. 工作区 .credential_key 存在 → 读取并校验后返回
     3. 都没有 → Fernet.generate_key()，以 0600 权限写入 <workspace>/.credential_key，返回
   ```

   纯函数式职责：只负责「拿到一个能用的 Fernet key」，不碰凭证存储本身。被 `CredentialManager` 与 setup 共用而不耦合。

2. **`CredentialManager._fernet()`**：删掉 `sha256` 派生那行（`manager.py:341`），改为用 `resolve_or_create_key()` 拿到的合法 key 直接 `Fernet(key)`。`require_encryption` 兜底语义保留——当连生成落盘都失败（如磁盘只读）时仍抛清晰错误。

3. **`CredentialManager._decode_secret()`**：`encoding=="fernet"` 解密失败时，抛清晰错误指明「凭证密钥已变更或损坏，请重新录入凭证」，而非静默返回空。不保留旧 sha256-KDF 解密分支（无存量可迁）。

4. **setup 向导（`cli/setup.py`）**：在凭证/安全相关步骤调用 `resolve_or_create_key()`；检测到新生成时提示用户「已生成凭证加密密钥并保存到 `<path>`（权限 0600），请勿删除或提交版本库」。

### 回归测试（新增 `tests/test_credential_key.py`）

- `resolve_or_create_key`：env 合法 key → 直接返回；env 非法 → 抛清晰错误；env/文件都无 → 生成落盘，断言文件存在且权限 0600；文件已存在 → 复用不重新生成。
- `CredentialManager` 往返：存凭证 → 新实例读回值一致，落盘 `encoding=="fernet"` 且无明文。
- 旧密文不兼容：构造一条旧 sha256-KDF 加密凭证 → 新逻辑解 → 断言抛清晰错误（非静默返回空）。

## P0-4 — gateway 默认安全档

### 现状取证

- profile 机制完整且生效：`security.profile: Literal["personal_cli","daemon","public_gateway"] = "personal_cli"`（`config/schema.py:352`）；在 `tool_policy.py:127-139`、`approval_gate.py:275`、`loop.py:525` 真实生效。
- `public_gateway` 档 deny 高危执行/写入/技能安装类工具（`tool_policy.py:58-74`：`HIGH_RISK_TOOLS` ∪ `edit_file/write_file/patch/workflow/knowledge_index`，及 `code.exec/fs.write/process.exec/process.manage/scheduler.write/skill.install/skill.write/workflow.write` 等 capability），保留读取、检索、对话——切档后 gateway 仍可正常服务。
- `run_gateway`（`app.py:341-371`）仅强制 `ctx.config.gateway.enabled=True`（`:356`），**未触碰 `security.profile`**——公网入口默认跑 `personal_cli`。
- 配置加载：`load_config` 先把 default.yaml + 用户 yaml + env + overrides 全部 `_deep_merge` 成 dict 再 `model_validate`（`config/loader.py:106-128`）。合并后 `security.profile` 总有值，单看 Config 无法分辨「用户显式配置」与「schema 默认填充」。

### 设计决策（已与用户确认）

- 切档力度：**未显式配置才切**——用户未显式设 `security.profile` 时，gateway 入口自动切 `public_gateway` 并 warning；用户显式写了任何 profile 则尊重不覆盖（保留逃生口）。
- 判定「显式」的实现：**独立窄纯函数**（非在 Config 上挂可变隐藏状态）。理由：无状态、可独立测试、将来好删；避免为假想复用引入全局可变状态（YAGNI）。

### 改动

1. **`config/loader.py` 新增纯函数**：

   ```
   def profile_explicitly_set(config_path: Path | None) -> bool:
       # 只看用户来源（用户 yaml 文件 + ECHO_AGENT_* env），
       # 不含 schema / default.yaml；检查 security.profile 这个 key 是否出现。
       # 复用已有的 _load_yaml_file / _env_overrides。
   ```

   纯函数：给定输入只读、不改任何对象状态、不依赖加载顺序。

2. **`app.py` 的 `run_gateway`**（`:355` bootstrap 完成后）：

   ```
   若 not profile_explicitly_set(config_path):
       ctx.config.security.profile = "public_gateway"
       logger.warning("Gateway 入口未显式配置 security.profile，已默认切到 public_gateway 收紧档；"
                      "如需放开请在配置中显式设置 security.profile")
   否则：尊重用户配置，不覆盖
   ```

   只动 `gateway` 子命令入口，不影响 `run`（CLI）入口的 `personal_cli` 默认体验。

### 回归测试

- `profile_explicitly_set`：用户 yaml 含 `security.profile` → True；不含 → False；env `ECHO_AGENT_SECURITY__PROFILE` 设了 → True。
- 切档行为：未显式配置 → `run_gateway` 后 `config.security.profile == "public_gateway"`；显式配 `personal_cli` → 保持不变。
- 工具收紧验证：`public_gateway` 档下 `write_file` / `exec` 被 deny、`read_file` / 检索类放行——锚定切档真的生效。

## 验证

- `ruff check .` 与 `pytest` 全绿（CI 会在 PR 上跑同样检查）。
- 新增回归测试逐条确认「未应用对应修复时会失败」，确保它们真的钉住行为。

## 非目标（明确排除）

- 不做凭证迁移工具（无存量可迁）。
- 不引入 KeyVault / 密钥轮转 / 统一密钥服务（属过度设计，超 M1 范围）。
- 不动 SSRF redirect 重校验 / DNS rebinding / proxy 旁路（P1-6 留 M4）。
- 不动 `personal_cli` 的 CLI 默认体验；不改 `daemon` 档语义。
- 不在 Config 对象上引入「显式键集合」等全局可变状态（用窄纯函数替代）。
