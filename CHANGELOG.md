# Changelog

本文件记录 Echo Agent 各版本的用户可见变化。格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [Unreleased]

### Added
- 网关增加持久化回合状态账本，附着式 CLI 可用 `/status [event_id]` 查询 `running` / `waiting_approval` / `waiting_clarification` / `completed` / `incomplete` / `failed` / `interrupted` 等权威状态；断线重连优先用该账本恢复回复与终态
- TUI `/save` 正式支持 `--format md|txt|json`；JSON 导出保留本地完整回合、隐藏工具和状态事件，递归脱敏凭据字段与命令行密钥参数
- Gateway HTTP/WS 与 Webhook 支持有界的 `Idempotency-Key` / `X-Idempotency-Key` 去重；同一作用域与请求内容的重试复用原事件和响应，不同内容复用同一 key 返回冲突
- Dashboard 构建增加首屏与单 chunk 体积预算门禁，防止路由代码再次意外回流到首屏 bundle

### Changed
- Tool 扩展的规范 Python 导入入口统一为 `echo_agent.tools`；旧的 `echo_agent.agent.tools.base` 路径继续作为向后兼容 shim 保留，现有插件行为不变
- **安全/兼容性**：A2A 任务、在途执行句柄与会话按认证 token 派生的 principal 隔离；不同 token 可复用相同自定义 task id，但不能查询或取消彼此任务。依赖“任一 token 可访问全实例 A2A 任务”的集成需调整
- A2A 活跃容量按真实未结束 worker 计算；即使 worker 吞掉取消或其任务记录已被超时回收，也不会绕过全局容量上限
- A2A 文档改为准确描述当前仅接线入站服务；低层 `A2AClient` 尚未通过 `net_guard`，不会被暴露为模型可调用的出站委派能力
- 已废弃的 `echo-agent service` 兼容别名计划在 **v0.5.0** 移除；请迁移到 `echo-agent gateway <action>`
- Dashboard 的 12 个页面改为路由级懒加载；首屏 JS 从审计基线 871 KiB 降至约 345 KiB（gzip 约 109 KiB）

### Fixed
- 修复会话 reset 只清消消息历史、但保留 working memory / 快照 / episode / 未完成计划的跨任务串扰；每次 reset 现在切换持久化 conversation epoch，并在同一会话锁内清理进程内上下文
- **安全**：会话 context epoch 改为无碰撞结构编码，Gateway 拒绝客户端注入保留语法；reset 同时清除 session/session-all 工具批准，但保留用户明确设置的 always 批准
- **安全**：ToolRegistry 不再允许插件静默覆盖内置/既有工具或占用别名；工具名执行统一格式校验，可信热替换必须显式声明
- 修复关停顺序和总线 drain：停止接收并收敛排队/在途回合后才拆 Agent；重启遗留、取消、总线限流和关停拒绝都会把 `turn_runs` 收敛到终态，同一 event id 只有一个原子执行 claimant
- 修复 `spawn_task` worker 和所有本地一次性子进程的生命周期泄漏；异步/同步执行在成功、非零退出、超时、取消与任意异常下均回收整个本地进程组；`ProcessTool` 和 MCP Stdio 继续作为显式长驻 owner
- 插件激活改为可回滚的完整生命周期：工具、hook 和入站订阅在激活失败/取消时撤回，反向依赖顺序关闭并按实例精确释放；未知权限、缺失/循环/失败依赖均 fail closed，`tool.register` / `hook.register` 在资源对宿主可见前完成准入判定
- Gateway 限流 bucket 改为有界 LRU，避免长期运行时按 chat id 无界增长
- Gateway/Webhook 键等令增加独立持久 tombstone，不受每会话 500 条回合结果裁剪影响；跨进程重试可重放结果/检出参数冲突，存储不可用或未过期记录达容量上限时 fail closed
- SQLite 读写共享连接租约与串行事务边界，迁移、锁竞争、提交失败、重复取消与 rollback 失败均会在释放租约前收敛或丢弃不安全连接；MCP Streamable HTTP 修复跨 chunk UTF-8、鉴权/会话失效传播、重连与关闭资源泄漏
- OpenTelemetry provider/exporter 由 Agent 显式持有并幂等关闭，部分初始化失败会回收后台线程并降级；健康检查生命周期及关键 span/metric 路径已补齐故障测试
- `agent.maxIterations`、`multiAgent.maxIterations`、worker profile 迭代数及观测性周期配置不再接受非正数
- 源码/editable 运行时版本从权威 `pyproject.toml` 读取，wheel 仍从发行元数据读取，避免旧 editable metadata 把 0.3.8 错报为 0.3.6
- 清理审计命中的全部静默异常处理点：预期吞咽均注明安全理由，需诊断处使用脱敏 debug 日志；另修复 scheduler 解锁失败时跳过关闭文件描述符的问题
- Dashboard WebSocket/异步查询测试不再产生 React `act()` 时序告警；文档站的公网 Gateway 安全片段已真正嵌入部署页，SQLite 恢复链接改用稳定锚点并可在 strict 模式构建
- 修复“逐项执行上述优化”类指代请求可被旧记忆/旧清单劫持的问题：指代检索和独立 planner 现以紧邻对话为权威上下文，长期记忆仅作可能过期的背景；后台 consolidation 不再把一次性任务进度/完成状态提取为持久事实，已有的模型推断型任务状态也不再进入 Agent 快照、检索或记忆工具
- 修复 LLM `finish_reason=length` 仍被当作干净完成的语义错误：有部分正文时仍会展示，但回合与计划标记为 `incomplete` / 可恢复，且不再触发反思重跑覆盖现场

### Removed
- **破坏性**：移除不再使用的桌面客户端专用网关扩展，包括聊天附件上传与 WebSocket 附件、WebSocket 技能管理帧、远程关停 API、进程 ready 标准输出信号，以及仅供该客户端消费的结构化空文本进度事件。通用 HTTP/WS 消息入口、内置 Dashboard、附着式 CLI/TUI、IM 媒体处理和本地文档解析保持不变
- `desktop` 不再是 `gateway.known_platforms` 的内置平台，`channels.stream_optimistic_channels` 默认仅保留能够原地重绘的 `gateway:cli`

## [0.3.8] - 2026-08-26

### Added
- `models.providers.*.api_key_env`：从指定环境变量读取 API 密钥，宿主进程可注入临时凭据而不把密钥写入配置文件。优先级为 `api_key` > `api_key_env` > 约定的 `OPENAI_API_KEY` 等环境变量名，未配置时行为与此前完全一致
- `ECHO_AGENT_*` 环境变量现在可以设置 list / dict 类型的配置项（值按 JSON 解析），例如 `ECHO_AGENT_GATEWAY__AUTH__ADMIN_TOKENS='["token"]'`。此前这类配置项只能写在 YAML 里，用环境变量设置必然校验失败
- 任务创建接口补齐 `parent_task_id` 与 `metadata` 透传（`TaskManager.create` 早有这两个参数，此前 API 层丢弃）；`metadata` 非对象时返回 400
- 建立完整文档站（MkDocs Material + 中英文双语）
- 会话历史接口区分「人类可读记录」与「LLM 上下文」：过滤掉压缩注入的摘要对，工具调用 / 结果带 `internal` 标记由 Dashboard 折叠显示
- 技能脚本可通过 `metadata.echo.requires.env` 声明所需的凭据环境变量，`skill_run` 只透传声明过的键。读一遍 SKILL.md 即可确定该技能能接触哪些密钥，不再需要把 token 塞进命令行参数
- `skill_view` 会在返回内容末尾提示缺失的 pip 依赖与未设置的环境变量（此前依赖缺失要等脚本运行失败才知道）
- `SkillStore.write_file_bytes()`：技能的 `assets/` 目录终于能装图片和字体
- `gateway.known_platforms`：客户端自报的 `platform` 会拼进通道名 `gateway:{platform}`，而通道名在别处承载能力判定（`channels.stream_optimistic_channels` 断言该通道可就地重绘）。不在此列表的取值折叠为 `ws` 而非直接拒绝，避免打断已有的第三方接入；留空可关闭折叠。默认列表含全部已实现通道与附着式 CLI —— 授权数据按 platform 存储（allowlist 的 `feishu:123` 写法、配对的 `{platform}_approved.json`），漏掉真实通道会让已有批准静默失效

### Changed
- 技能清单注入系统提示时增加字符预算（约 6000 字符）。此前 35 个内置技能已占约 1.2k tokens 且无上限，装到几百个会给每一轮请求都加上固定的五位数 token 开销。超预算时先截断描述，仍不够才丢弃条目，并明确告知省略了多少个、指向 `skills_list`——静默省略会让模型笃定某个技能不存在
- 移除 `SkillManager` 及其 `manifest.json`/`.status`/`config.json` 一套并行技能模型：生产代码从未构造它（`app.py` 恒传 `skill_manager=None`），磁盘上从未产生过对应文件，仅测试实例化。运行时一直只用 `SkillStore`，两套模型并存迟早漂移
- **破坏性**：`tools.exec.enabled = false` 现在会同时停用技能脚本执行。跑技能脚本本身就是代码执行，此前它是 exec 总开关唯一漏掉的执行路径
- **破坏性**：`security.profile` 为 `daemon` 或 `public_gateway` 时 `skill_run` 默认被拒。这两档本就拒掉了 `exec`/`execute_code`/`process`，需要保留的部署请显式配置 `tools.also_allow: [skill_run]`
- **破坏性**：`skill_view` 不再自动安装依赖（此前在可信 CLI 上会静默 pip install）。它声明 `risk_level="read_only"` 却能触发装包，而 `install_authorized()` 不遵守 SKILL_DEPS 白名单、`skills.allow_lazy_installs` 与 `ECHO_AGENT_DISABLE_LAZY_INSTALLS`，等于「看一眼技能」就是供应链执行入口。依赖安装改由 `skill_run` 在授权后执行
- 技能脚本运行环境从 `env={}` 改为白名单透传（`PATH`/`HOME`/语言/代理/TLS 证书路径 + 技能声明的凭据键）。空环境让 `requires.bins` 成为纯装饰，也让凭据型技能（如 `image-gen`）必然启动失败
- `skill_run` 的永久批准粒度细化为 `skill_run:<技能>/<脚本>`，此前 `tool:skill_run` 一次批准等于永久批准任意技能的任意脚本
- 内置 `workflow-chain` 不再使用 `shell=True`：命令按 `shlex` 解析后以参数列表执行，`;`/`&&`/`|` 成为字面参数。需要管道的步骤请显式写 `sh -c '...'`
- 任务接口收紧字段类型校验：`priority` 必须是整数（布尔值不接受）、`labels` 必须是字符串数组、`metadata` 必须是对象，否则返回 400。`TaskRecord` 是普通 dataclass 不做类型收敛，此前非法值会直接落盘并被原样读回，而看板前端把 `priority` 当 number、`labels` 当数组用。`POST` 与 `PATCH` 两条路径同口径，`PATCH` 只校验请求里实际出现的字段

### Fixed
- **安全**：配置文件用驼峰键（`networkPolicy`）写的配置项无法被环境变量覆盖。schema 同时接受驼峰与下划线两种写法，但它们是两个不同的字典键，深合并后两者并存而 pydantic 取驼峰值，环境变量被静默丢弃。包内 `default.yaml` 本身就用驼峰，因此 `ECHO_AGENT_EXECUTION__NETWORK_POLICY=deny` 这类收紧设置一直不生效且无任何提示（fail-open）。现在各来源在合并前统一归一到字段名；归一只在读取时的内存中进行，不改写配置文件。受影响的还有向导写入的 `apiKey`/`apiBase`/`defaultModel`/`modelWindows`/`idleTimeoutMinutes`/`dailyResetHour` 等
- 环境变量是否按 JSON 解析改为依据 schema 声明的字段类型，而非值的外观。曾按值的外观判断，导致取值恰好是 `false`/`true`/`null` 的字符串配置项（密码、token 等）被转成 bool/None 并使进程启动失败——schema 中有 121 个字符串字段受影响
- 媒体下载在测试替身（AsyncMock）下会遗留未 await 的协程并触发 RuntimeWarning，回退到 `resp.read()` 前显式关闭
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

[Unreleased]: https://github.com/fuyuxiang/echo-agent/compare/v0.3.8...HEAD
[0.3.8]: https://github.com/fuyuxiang/echo-agent/releases/tag/v0.3.8
[0.3.7]: https://github.com/fuyuxiang/echo-agent/releases/tag/v0.3.7
