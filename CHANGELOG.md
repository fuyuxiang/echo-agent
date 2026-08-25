# Changelog

本文件记录 Echo Agent 各版本的用户可见变化。格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [Unreleased]

### Added
- 建立完整文档站（MkDocs Material + 中英文双语）
- 会话历史接口区分「人类可读记录」与「LLM 上下文」：过滤掉压缩注入的摘要对，工具调用 / 结果带 `internal` 标记由 Dashboard 折叠显示
- 技能脚本可通过 `metadata.echo.requires.env` 声明所需的凭据环境变量，`skill_run` 只透传声明过的键。读一遍 SKILL.md 即可确定该技能能接触哪些密钥，不再需要把 token 塞进命令行参数
- `skill_view` 会在返回内容末尾提示缺失的 pip 依赖与未设置的环境变量（此前依赖缺失要等脚本运行失败才知道）
- `SkillStore.write_file_bytes()`：技能的 `assets/` 目录终于能装图片和字体

### Changed
- 技能清单注入系统提示时增加字符预算（约 6000 字符）。此前 35 个内置技能已占约 1.2k tokens 且无上限，装到几百个会给每一轮请求都加上固定的五位数 token 开销。超预算时先截断描述，仍不够才丢弃条目，并明确告知省略了多少个、指向 `skills_list`——静默省略会让模型笃定某个技能不存在
- 移除 `SkillManager` 及其 `manifest.json`/`.status`/`config.json` 一套并行技能模型：生产代码从未构造它（`app.py` 恒传 `skill_manager=None`），磁盘上从未产生过对应文件，仅测试实例化。运行时一直只用 `SkillStore`，两套模型并存迟早漂移
- **破坏性**：`tools.exec.enabled = false` 现在会同时停用技能脚本执行。跑技能脚本本身就是代码执行，此前它是 exec 总开关唯一漏掉的执行路径
- **破坏性**：`security.profile` 为 `daemon` 或 `public_gateway` 时 `skill_run` 默认被拒。这两档本就拒掉了 `exec`/`execute_code`/`process`，需要保留的部署请显式配置 `tools.also_allow: [skill_run]`
- **破坏性**：`skill_view` 不再自动安装依赖（此前在可信 CLI 上会静默 pip install）。它声明 `risk_level="read_only"` 却能触发装包，而 `install_authorized()` 不遵守 SKILL_DEPS 白名单、`skills.allow_lazy_installs` 与 `ECHO_AGENT_DISABLE_LAZY_INSTALLS`，等于「看一眼技能」就是供应链执行入口。依赖安装改由 `skill_run` 在授权后执行
- 技能脚本运行环境从 `env={}` 改为白名单透传（`PATH`/`HOME`/语言/代理/TLS 证书路径 + 技能声明的凭据键）。空环境让 `requires.bins` 成为纯装饰，也让凭据型技能（如 `image-gen`）必然启动失败
- `skill_run` 的永久批准粒度细化为 `skill_run:<技能>/<脚本>`，此前 `tool:skill_run` 一次批准等于永久批准任意技能的任意脚本
- 内置 `workflow-chain` 不再使用 `shell=True`：命令按 `shlex` 解析后以参数列表执行，`;`/`&&`/`|` 成为字面参数。需要管道的步骤请显式写 `sh -c '...'`

### Fixed
- **安全**：`skill_run` 只回收直接子进程，技能脚本自己派生的孙进程（ffmpeg、pip、另一个脚本）在超时和外层取消两条路径上都会脱离管控继续运行。现改为经 `proc_lifecycle.spawn_exec`/`terminate_tree` 走进程组回收，与 `shell`/`code_exec`/`process` 一致——此前 `skill_run` 是唯一没走这条公共通道的执行工具
- **安全**：`skill_run` 是 exec 策略的绕过通道。它声明 `risk_level="exec"` 却没有注册任何 capability，因此 `daemon`/`public_gateway` 精心构造的 `process.exec`/`code.exec` 拒绝集合对它一律失效，而内置 `workflow-chain` 提供了一个通用 shell 执行器。现已补齐 capabilities、策略名单、风险表与 guards 门禁
- **安全**：技能的「禁用」此前只是隐藏。`list_all()` 过滤了禁用集合，但 `_find_skill_dir()` 没有，因此 `read_skill`/`read_file`/`skill_run` 照样解析得到——包括 evolution gate 用 `persist_disable()` 停用的作恶技能。禁用判据已下沉到唯一解析入口；管理操作（修复、删除、重新启用）显式绕过
- **安全**：网关本地导入用 SKILL.md 里未校验的 `name`/`category` 拼接目标路径，`name: /tmp/x` 可写出 `user_dir` 之外。现按单段路径规则校验并对解析后的路径二次确认
- **安全**：`skill_install` 的 URL 源缺少 SSRF 校验、归档大小上限与 zip-slip 成员校验；`subdirectory` 未做 containment 检查，`../outside` 可逃出下载目录
- **安全**：外部安装的 SKILL.md 未经注入扫描即进入系统提示。现按「操纵模型的内容」拒装、「命令形态的内容」告警分层处理——直接套用 memory 规则集会误拒 12/35 个内置技能，因为 `~/.echo-agent/config.yaml` 在文档里是正常的配置路径
- **安全**：注入检测的英文模式要求限定词与 `instructions` 紧邻，漏掉了最典型的 `ignore all previous instructions` 及带插入词的变体（中文模式本已允许插入词）。此缺口对 memory 写入同样存在
- **安全**：审计脱敏只递归字典，不处理数组，`["--token", "secret"]` 原样进入审计日志——而文档此前正引导用户用参数传密钥
- `skill_run` 的超时分层自相矛盾：参数上限 600 秒而工具总超时 120 秒，任何跑过两分钟的技能都会被外层取消；且取消路径不杀子进程，必然留下脱离管控仍在执行的孤儿进程
- `skill_install` 不是事务性的：遇到二进制资产（PNG/字体）会在复制中途因 UTF-8 解码失败而中断，且不回滚，留下半安装技能。现改为先全量校验读入再落盘，失败回滚到安装前状态
- `skill_install` 的 `name` 覆盖只改目录名不改 frontmatter，导致一个技能有两个可解析的名字：按 `my-alias` 安装的技能在列表里显示为原名，还会与真正的原名撞名，且按其中一个名字禁用会留另一个可用
- 一个 SKILL.md 的 frontmatter 若解析成 list 或 str，`list_all()` 会抛 `AttributeError`，导致整段技能上下文消失——即所有技能因一个坏文件而全部不可见。非 UTF-8 的 SKILL.md 有同样效果
- `skills.enabled` 是死配置：全仓库无任何读取点，设为 false 后技能工具照常注册、技能清单照常注入。现与 `memory.enabled`/`knowledge.enabled` 一致生效
- `skill_install` 只读取 `metadata.echo.install`，忽略了 `requires.pip`（35 个内置技能中仅 3 个用前者），且依赖安装失败仍返回成功。两种声明现已合并，依赖失败会反映到调用结果
- 修复 5 个内置技能的依赖缺陷：`meme-gen` 错误请求整个 OCR 依赖栈；`ocr-document`/`calendar`/`file-convert` 在 `require()` 之前就 import 可选包，使懒安装来不及介入；`ocr-document` 的 DOCX 路径错误请求 `skill.excel-author`，`file-convert` 的 Markdown 路径用错 feature key
- `calculator` 的 SKILL.md 首个示例缺少 `eval` 子命令，照抄会失败
- 技能文档声称有「匹配度计算」与「统一环境检查」，实际是把技能清单交给模型选择。文档已改为描述真实机制
- 会话历史接口的 `limit` 增加范围校验（1–500）：此前 `limit=0` / 负数经 Python 切片会返回全部历史；`total` 改为整段历史条数，新增 `returned` 表示本次返回条数
- **安全**：`gateway.host` 留空曾被当作本机回环，导致未配置 token 的网关可以启动并暴露到所有网卡。空字符串与 `::` 现按通配绑定处理，与 `0.0.0.0` 一致
- 绑定地址判定统一到 `gateway/host_rules.py`（此前向导、`_check_bind_safety`、启动告警、`GatewayAuth` 各持一份判据且互相矛盾）；`127.0.0.0/8` 全段与 `[::1]` 不再被误判为非本机
- `auth.allowed_hosts` 条目在比对前规范化：大小写、端口、IPv6 方括号不再影响匹配，从地址栏粘贴的值可以直接用；通配地址不再作为有效条目被静默接受
- 向导在通配绑定下不再用绑定地址预填 `allowed_hosts`（`[0.0.0.0]` 匹配不到任何浏览器请求，却会让告警闭嘴），改为探测本机地址作建议值
- 向导在绑定改回本机时会提示清理遗留的 `allowed_hosts`——显式白名单会覆盖默认的本机放行规则，此前会让本机 Dashboard 的管理页面全部 403

## [0.3.7] - 2026-08-16

### Fixed
- 修复通道体系 5 项 P1 与 4 项 P2 安全及数据一致性缺陷
- 修复通道体系剩余 5 项缺陷与体验改善
- 修复通道体系 13 项 P0/P1/P2 安全与数据一致性缺陷
- 修复 Dashboard 9 项 P0/P1 数据一致性与安全缺陷

## [0.3.6] 及更早版本

> 历史版本变更记录待补录。欢迎贡献者协助从 git 历史中整理。

[Unreleased]: https://github.com/fuyuxiang/echo-agent/compare/v0.3.7...HEAD
[0.3.7]: https://github.com/fuyuxiang/echo-agent/releases/tag/v0.3.7
