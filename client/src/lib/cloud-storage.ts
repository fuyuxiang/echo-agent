/**
 * 云存储抽象 —— 腾讯文档/乐享/Drive/IMA 的本地可移植替代。
 *
 * EchoAgent 用腾讯 Drive(@tencent/drive-sdk)、腾讯文档(@tencent/docs-engine)、
 * 乐享(IMA)做云文件管理;这些绑定腾讯专有 SDK。EchoAgent 用「云存储 provider 抽象」
 * 替代:任意实现(WebDAV/S3/本地文件夹/Google Drive)都可注册。
 *
 * 纯函数核心(路径操作 + 文件元信息 + provider 注册表),HTTP 依赖注入便于单测。
 */
import { invoke } from "@tauri-apps/api/core";

/** 文件/目录元信息。 */
export interface StorageEntry {
  /** 路径(相对 root)。 */
  path: string;
  /** 文件名。 */
  name: string;
  /** 是否目录。 */
  isDir: boolean;
  /** 字节大小(文件)。 */
  size?: number;
  /** 最后修改时间(ms)。 */
  modifiedAt?: number;
  /** MIME 类型(可选)。 */
  mimeType?: string;
}

/** 云存储 provider 接口(任意实现:WebDAV / S3 / 本地 / Google Drive)。 */
export interface StorageProvider {
  id: string;
  label: string;
  isEnabled(): boolean;
  /** 列出目录内容。 */
  list(path: string): Promise<StorageEntry[]>;
  /** 读取文件文本。 */
  readText(path: string): Promise<string | null>;
  /** 写入文件文本。 */
  writeText(path: string, content: string): Promise<boolean>;
  /** 仅在目标不存在时创建文本。 */
  createText(path: string, content: string): Promise<boolean>;
  /** 删除文件/目录。 */
  delete(path: string): Promise<boolean>;
  /** 创建目录。 */
  makeDir(path: string): Promise<boolean>;
  /** 将用户选择的本地文件流式上传。 */
  uploadFile?(path: string, localPath: string): Promise<number>;
  /** 将远程文件流式下载到用户选择的位置。 */
  downloadFile?(path: string, localPath: string): Promise<number>;
}

// ---------- WebDAV provider(最常见的自托管云存储)----------

/** WebDAV 配置。 */
export interface WebDavConfig {
  /** WebDAV 服务器 URL(如 https://dav.example.com/remote.php/dav/files/user)。 */
  baseUrl: string;
  /** 用户名。 */
  username?: string;
  /** 密码/应用令牌。 */
  password?: string;
}

export interface StorageProviderConfig extends WebDavConfig {
  id: string;
  label: string;
  kind: "webdav";
  enabled: boolean;
}

export async function loadStorageProviderConfigs(): Promise<StorageProviderConfig[]> {
  return invoke<StorageProviderConfig[]>("storage_providers_list");
}

export async function saveStorageProviderConfig(config: StorageProviderConfig): Promise<void> {
  await invoke<void>("storage_provider_upsert", { config });
}

export async function removeStorageProviderConfig(id: string): Promise<void> {
  await invoke<void>("storage_provider_remove", { id });
}

export async function testStorageProviderConfig(id: string): Promise<void> {
  await invoke<void>("storage_provider_test", { id });
}

/** Construct a real provider backed by Rust reqwest/WebDAV commands. */
const persistedProviders = new WeakSet<StorageProvider>();

export function createTauriWebDavProvider(config: StorageProviderConfig): StorageProvider {
  const provider: StorageProvider = {
    id: config.id,
    label: config.label,
    isEnabled: () => config.enabled,
    list: (path) => invoke<StorageEntry[]>("storage_list", { id: config.id, path }),
    readText: (path) => invoke<string>("storage_read_text", { id: config.id, path }),
    writeText: async (path, content) => {
      await invoke<void>("storage_write_text", { id: config.id, path, content });
      return true;
    },
    createText: async (path, content) => {
      await invoke<void>("storage_create_text", { id: config.id, path, content });
      return true;
    },
    delete: async (path) => {
      await invoke<void>("storage_delete", { id: config.id, path });
      return true;
    },
    makeDir: async (path) => {
      await invoke<void>("storage_make_dir", { id: config.id, path });
      return true;
    },
    uploadFile: (path, localPath) => invoke<number>("storage_upload_file", {
      id: config.id,
      path,
      localPath,
    }),
    downloadFile: (path, localPath) => invoke<number>("storage_download_file", {
      id: config.id,
      path,
      localPath,
    }),
  };
  persistedProviders.add(provider);
  return provider;
}

export async function hydrateStorageProviders(): Promise<StorageProviderConfig[]> {
  const configs = await loadStorageProviderConfigs();
  const configuredIds = new Set(configs.map((config) => config.id));
  for (const provider of [...providers]) {
    if (persistedProviders.has(provider) && !configuredIds.has(provider.id)) {
      unregisterStorageProvider(provider.id);
    }
  }
  for (const config of configs) registerStorageProvider(createTauriWebDavProvider(config));
  return configs;
}

/** HTTP 客户端(注入)。 */
export interface HttpClient {
  request(method: string, url: string, opts?: { headers?: Record<string, string>; body?: string }): Promise<{
    ok: boolean;
    status: number;
    text: string;
  }>;
}

/** 构造 WebDAV PROPFIND 请求体(XML,请求文件元信息)。 */
export function buildPropfindBody(): string {
  return `<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:displayname/>
    <D:resourcetype/>
    <D:getcontentlength/>
    <D:getlastmodified/>
    <D:getcontenttype/>
  </D:prop>
</D:propfind>`;
}

/** 解析 PROPFIND 响应(XML → StorageEntry[])。简化解析(正则提取,避免 DOM 依赖)。 */
export function parsePropfindResponse(xml: string, basePath: string): StorageEntry[] {
  const entries: StorageEntry[] = [];
  // 按 <D:response> 或 <d:response> 分割(大小写不敏感)。
  const responseRe = /<(?:D|d):response>([\s\S]*?)<\/(?:D|d):response>/g;
  let m: RegExpExecArray | null;
  while ((m = responseRe.exec(xml)) !== null) {
    const block = m[1];
    const href = extractXmlTag(block, "href") ?? "";
    // 跳过自身(basePath)。
    const decodedHref = decodeURIComponent(href).replace(/\/$/, "");
    const decodedBase = basePath.replace(/\/$/, "");
    if (decodedHref === decodedBase || decodedHref === decodedBase + "/") continue;
    const name = decodedHref.split("/").pop() ?? "";
    if (!name) continue;
    const isDir = block.includes("collection");
    const sizeStr = extractXmlTag(block, "getcontentlength");
    const modified = extractXmlTag(block, "getlastmodified");
    const mime = extractXmlTag(block, "getcontenttype");
    entries.push({
      path: decodedHref,
      name: decodeURIComponent(name),
      isDir,
      size: sizeStr ? parseInt(sizeStr, 10) : undefined,
      modifiedAt: modified ? Date.parse(modified) || undefined : undefined,
      mimeType: mime ?? undefined,
    });
  }
  return entries;
}

function extractXmlTag(xml: string, tag: string): string | null {
  const re = new RegExp(`<(?:D|d):${tag}[^>]*>([^<]*)<`, "i");
  const m = xml.match(re);
  return m?.[1]?.trim() ?? null;
}

/** 规整路径:确保以 / 开头,去多余斜杠。 */
export function normalizePath(path: string): string {
  let p = path.trim().replace(/\\/g, "/");
  if (!p.startsWith("/")) p = "/" + p;
  p = p.replace(/\/+/g, "/");
  if (p.length > 1 && p.endsWith("/")) p = p.slice(0, -1);
  return p;
}

/** joinPath:拼接 base + relative。 */
export function joinStoragePath(base: string, relative: string): string {
  return normalizePath(base + "/" + relative);
}

// ---------- provider 注册表 ----------

const providers: StorageProvider[] = [];

/** 注册云存储 provider(去重 by id)。 */
export function registerStorageProvider(p: StorageProvider): void {
  const index = providers.findIndex((x) => x.id === p.id);
  if (index >= 0) providers[index] = p;
  else providers.push(p);
}

/** 注销。 */
export function unregisterStorageProvider(id: string): boolean {
  const before = providers.length;
  const idx = providers.findIndex((p) => p.id === id);
  if (idx >= 0) providers.splice(idx, 1);
  return providers.length < before;
}

/** 列出已启用 provider。 */
export function listStorageProviders(): Array<{ id: string; label: string }> {
  return providers.filter((p) => p.isEnabled()).map((p) => ({ id: p.id, label: p.label }));
}

/** 清空(测试用)。 */
export function resetStorageProviders(): void {
  providers.length = 0;
}

/** 按 id 取 provider。 */
export function getStorageProvider(id: string): StorageProvider | null {
  return providers.find((p) => p.id === id && p.isEnabled()) ?? null;
}
