/**
 * 通知渠道抽象 —— EchoAgent IM 渠道(微信/企微/QQ/钉钉/Slack/元宝)的本地可移植替代。
 *
 * EchoAgent 的 IM 渠道本质是「把 agent 通知/会话结果推送到某个 IM」。这些绑定腾讯/各 IM
 * 专有 SDK,不可移植。EchoAgent 用「通知渠道 provider」抽象替代:任意实现 webhook/邮件/
 * 桌面通知,provider-agnostic。纯函数核心(载荷构造 + 渠道分发 + 注册表),便于单测。
 */
import { invoke } from "@tauri-apps/api/core";

/** 通知级别。 */
export type NotifyLevel = "info" | "warn" | "error";

/** 一条通知消息。 */
export interface NotifyMessage {
  /** 标题。 */
  title: string;
  /** 正文。 */
  body?: string;
  /** 级别。 */
  level?: NotifyLevel;
  /** 可选来源会话 id。 */
  sessionId?: string;
}

/** 通知渠道类型。 */
export type ChannelKind =
  | "slack-webhook"
  | "discord-webhook"
  | "generic-webhook"
  | "email"
  | "desktop";

/** 通知渠道配置(用户在设置里配置)。 */
export interface NotifyChannel {
  /** 稳定 id。 */
  id: string;
  /** 显示名。 */
  label: string;
  /** 渠道类型。 */
  kind: ChannelKind;
  /** webhook URL 或 mailto 地址(desktop 类型忽略)。 */
  endpoint?: string;
  /** 是否启用。 */
  enabled: boolean;
}

export interface DeliveryResult {
  id: string;
  ok: boolean;
  error?: string;
}

export interface DesktopNotifyStatus {
  enabled: boolean;
  source: "default" | "configured" | "policy";
  permission: "unknown";
  policyError?: string;
}

export async function loadDesktopNotifyStatus(): Promise<DesktopNotifyStatus> {
  return invoke<DesktopNotifyStatus>("notify_desktop_status");
}

export async function setDesktopNotifyEnabled(enabled: boolean): Promise<void> {
  await invoke<void>("notify_desktop_set_enabled", { enabled });
  if (typeof window !== "undefined") window.dispatchEvent(new Event("echo:notification-settings-changed"));
}

export async function testDesktopNotify(sessionId?: string): Promise<DeliveryResult> {
  return invoke<DeliveryResult>("notify_desktop_test", { sessionId: sessionId ?? null });
}

/** Persisted backend channel operations. Webhooks are sent by Rust so WebView
 * CORS and background-session filtering cannot silently drop notifications. */
export async function loadNotifyChannels(): Promise<NotifyChannel[]> {
  return invoke<NotifyChannel[]>("notify_channels_list");
}

export async function saveNotifyChannel(channel: NotifyChannel): Promise<void> {
  await invoke<void>("notify_channel_upsert", { channel });
  if (typeof window !== "undefined") window.dispatchEvent(new Event("echo:notification-settings-changed"));
}

export async function removeNotifyChannel(id: string): Promise<void> {
  await invoke<void>("notify_channel_remove", { id });
  if (typeof window !== "undefined") window.dispatchEvent(new Event("echo:notification-settings-changed"));
}

export async function setNotifyChannelEnabled(id: string, enabled: boolean): Promise<void> {
  await invoke<void>("notify_channel_set_enabled", { id, enabled });
  if (typeof window !== "undefined") window.dispatchEvent(new Event("echo:notification-settings-changed"));
}

export async function testNotifyChannel(id: string): Promise<DeliveryResult> {
  return invoke<DeliveryResult>("notify_channel_test", { id });
}

/** 通知渠道 provider 接口(任意实现)。 */
export interface NotifyProvider {
  id: string;
  /** 渲染消息为该渠道的载荷(如 Slack 的 {text} JSON)。 */
  buildPayload(msg: NotifyMessage): unknown;
  /** 发送(运行时:fetch webhook / open mailto / 桌面通知);返回是否成功。 */
  send?(channel: NotifyChannel, payload: unknown): Promise<boolean>;
}

/** Slack webhook 载荷。 */
export interface SlackPayload {
  text: string;
  blocks?: Array<{ type: string; text: { type: string; text: string } }>;
}

/** 把消息渲染成 Slack webhook 载荷。纯函数。 */
export function buildSlackPayload(msg: NotifyMessage): SlackPayload {
  const emoji = msg.level === "error" ? "🔴" : msg.level === "warn" ? "🟡" : "🔵";
  return {
    text: `${emoji} ${msg.title}`,
    blocks: [
      {
        type: "section",
        text: { type: "mrkdwn", text: `*${emoji} ${msg.title}*${msg.body ? `\n${msg.body}` : ""}` },
      },
    ],
  };
}

/** Discord webhook 载荷。 */
export interface DiscordPayload {
  content: string;
  embeds?: Array<{ title: string; description?: string; color: number }>;
}

/** 把消息渲染成 Discord webhook 载荷。纯函数。 */
export function buildDiscordPayload(msg: NotifyMessage): DiscordPayload {
  const color = msg.level === "error" ? 0xdc2626 : msg.level === "warn" ? 0xd97706 : 0x0ea5e9;
  return {
    content: msg.title,
    embeds: [{ title: msg.title, description: msg.body, color }],
  };
}

/** 通用 webhook(JSON POST {title, body, level})。纯函数。 */
export function buildGenericPayload(msg: NotifyMessage): Record<string, unknown> {
  return { title: msg.title, body: msg.body ?? "", level: msg.level ?? "info", ts: Date.now() };
}

/** 邮件:构造 mailto URL(subject + body)。纯函数。 */
export function buildMailtoUrl(channel: NotifyChannel, msg: NotifyMessage): string {
  const addr = channel.endpoint ?? "";
  const params = new URLSearchParams();
  params.set("subject", msg.title);
  if (msg.body) params.set("body", msg.body);
  return `mailto:${addr}?${params.toString()}`;
}

/** 按 channel.kind 选择对应的载荷构造器。 */
export function buildPayload(channel: NotifyChannel, msg: NotifyMessage): unknown {
  switch (channel.kind) {
    case "slack-webhook":
      return buildSlackPayload(msg);
    case "discord-webhook":
      return buildDiscordPayload(msg);
    case "email":
      return buildMailtoUrl(channel, msg);
    case "generic-webhook":
    case "desktop":
    default:
      return buildGenericPayload(msg);
  }
}

// ---------- 渠道注册表 + 分发 ----------

interface NotifyRegistry {
  channels: NotifyChannel[];
}

const registry: NotifyRegistry = { channels: [] };

/** 注册一个通知渠道(去重 by id)。 */
export function registerNotifyChannel(c: NotifyChannel): void {
  if (registry.channels.some((x) => x.id === c.id)) return;
  registry.channels.push(c);
}

/** 注销一个通知渠道(by id)。 */
export function unregisterNotifyChannel(id: string): boolean {
  const before = registry.channels.length;
  registry.channels = registry.channels.filter((c) => c.id !== id);
  return registry.channels.length < before;
}

/** 列出已启用渠道。 */
export function listNotifyChannels(): NotifyChannel[] {
  return registry.channels.filter((c) => c.enabled);
}

/** 列出全部渠道(含禁用,供设置页展示)。 */
export function listAllNotifyChannels(): NotifyChannel[] {
  return [...registry.channels];
}

/** 清空(测试用)。 */
export function resetNotifyChannels(): void {
  registry.channels = [];
}

/**
 * 向所有启用渠道分发通知。返回每个渠道的发送结果。
 * 依赖注入 `sender`，便于纯函数单测。产品通知一律由 Rust 后端发送，
 * 不在 WebView 内直接请求 webhook 或打开外部 URL。
 */
export async function dispatchNotification(
  msg: NotifyMessage,
  deps: { sender?: (channel: NotifyChannel, payload: unknown) => Promise<boolean> } = {},
): Promise<Array<{ id: string; ok: boolean }>> {
  const sender = deps.sender ?? (async () => false);
  const out: Array<{ id: string; ok: boolean }> = [];
  for (const channel of listNotifyChannels()) {
    const payload = buildPayload(channel, msg);
    try {
      const ok = await sender(channel, payload);
      out.push({ id: channel.id, ok });
    } catch {
      out.push({ id: channel.id, ok: false });
    }
  }
  return out;
}
