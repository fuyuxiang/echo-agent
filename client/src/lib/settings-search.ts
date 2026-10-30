import type { SettingsSectionId } from "@/components/SettingsPanel";

export interface SettingsSearchEntry {
  section: SettingsSectionId;
  title: string;
  category: string;
  keywords: string;
  target?: string;
}

// Entries point to real pages and stable setting IDs. No configuration values,
// credentials or conversation content enter this index.
const SETTINGS_SEARCH: SettingsSearchEntry[] = [
  { section: "general", title: "外观模式", category: "通用", keywords: "主题 浅色 深色 系统 appearance theme", target: "appearance" },
  { section: "general", title: "字体大小", category: "通用", keywords: "字号 缩放 阅读 font size", target: "font-size" },
  { section: "general", title: "关闭窗口后继续运行", category: "通用", keywords: "后台运行 托盘 退出 任务 自动化", target: "background" },
  { section: "shortcuts", title: "快捷键", category: "通用", keywords: "键盘 shortcut 发送 换行" },
  { section: "notify-channels", title: "桌面通知", category: "通知", keywords: "桌面提醒 系统 勿扰 权限 测试", target: "desktop-notifications" },
  { section: "notify-channels", title: "在通知中显示任务标题", category: "通知", keywords: "名称 隐私 锁屏 预览", target: "notification-content" },
  { section: "notify-channels", title: "推送到其他应用", category: "通知", keywords: "外部 通知渠道 Slack Discord Webhook", target: "external-notifications" },
  { section: "agent-mail", title: "通知记录", category: "通知", keywords: "提醒 历史 未读 失败 授权" },
  { section: "model", title: "模型与连接", category: "智能体", keywords: "模型服务 API Key Base URL OpenAI Anthropic 默认 模型名称 接口" },
  { section: "memory", title: "使用记忆", category: "记忆", keywords: "长期记忆 偏好 记住 关闭 开关", target: "memory-preference" },
  { section: "memory", title: "管理记忆", category: "记忆", keywords: "个人信息 项目 约定 跟进 更正 忘记 导入 导出" },
  { section: "security", title: "浏览器与电脑操作", category: "安全与权限", keywords: "网页 系统授权 系统权限 辅助功能 屏幕录制", target: "automation-permissions" },
  { section: "security", title: "浏览器数据与访问范围", category: "安全与权限", keywords: "缓存 登录 本机 内网 隐私", target: "browser-data" },
  { section: "security", title: "自定义工具规则", category: "安全与权限", keywords: "允许 拒绝 询问 权限规则 工具权限 自定义权限 bash 命令", target: "permission-rules" },
  { section: "agent-settings", title: "子代理最大层级", category: "高级设置", keywords: "嵌套 深度 分派 agent", target: "subagents" },
  { section: "agent-settings", title: "重新加载配置", category: "高级设置", keywords: "刷新 MCP 连接器 技能 模型", target: "reload" },
  { section: "agent-settings", title: "诊断数据上报", category: "高级设置", keywords: "遥测 OTLP 端点 日志 排查", target: "diagnostics" },
  { section: "weixin-channel", title: "微信连接", category: "连接与数据", keywords: "远程 对话 消息通道 绑定 扫码" },
  { section: "cloud-storage", title: "云存储", category: "连接与数据", keywords: "WebDAV 文件 传输 上传 下载 存储源" },
  { section: "usage", title: "模型用量", category: "连接与数据", keywords: "用量统计 tokens 费用 估算 请求 导出" },
  { section: "data", title: "数据备份与恢复", category: "数据管理", keywords: "导出 备份文件 备份恢复 恢复 本机 数据", target: "backups" },
  { section: "data", title: "本地数据目录", category: "数据管理", keywords: "文件夹 存储位置 路径", target: "data-directory" },
  { section: "archived", title: "已归档会话", category: "数据管理", keywords: "任务 恢复 删除 聊天 历史" },
  { section: "help", title: "版本与更新", category: "帮助与更新", keywords: "升级 检查 安装 下载 关于", target: "updates" },
  { section: "help", title: "常见问题与使用帮助", category: "帮助与更新", keywords: "文档 排查 支持 问题", target: "help-faq" },
];

export function searchSettings(query: string): SettingsSearchEntry[] {
  const terms = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
  if (!terms.length) return [];
  return SETTINGS_SEARCH.filter(entry => {
    const text = `${entry.title} ${entry.category} ${entry.keywords}`.toLocaleLowerCase();
    return terms.every(term => text.includes(term));
  }).sort((a, b) => Number(b.title.includes(query.trim())) - Number(a.title.includes(query.trim())));
}
