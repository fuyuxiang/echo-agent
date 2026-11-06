/**
 * 知识库可插拔源注册表 —— 对齐 EchoAgent `knowledge-base-panel` 的「可插拔知识源」
 * 概念(原实现绑定腾讯 Drive/文档/乐享,不可移植;这里抽象成 provider 注册表,任意
 * 知识源(本地文件夹/网盘/文档库)都可注册)。纯函数,便于单测。
 */

/** 一条知识库条目(文件/文档/页面)。 */
export interface KbEntry {
  id: string;
  title: string;
  /** 摘要/预览文本。 */
  snippet?: string;
  /** 来源标识(由 searchKb 自动填充 provider id;provider 也可自带)。 */
  source?: string;
  /** Human-readable source name supplied by semantic or external providers. */
  sourceLabel?: string;
  /** 可选 URL/路径(打开用)。 */
  url?: string;
}

/** 知识源 provider 接口(任意实现:本地文件夹、网盘、文档库)。 */
export interface KbProvider {
  id: string;
  /** 显示名。 */
  label: string;
  /** 是否可用(未授权/未配置返回 false)。 */
  isEnabled(): boolean;
  /** 列出条目(可带 query 过滤;source 可缺省,由 searchKb 填充)。
   *  允许 async(provider 可能需读文件/网络)。 */
  list(query?: string): KbEntry[] | Promise<KbEntry[]>;
  /** 重建索引 / 刷新缓存(本地文件夹内容变化后重新扫描)。
   *  无此能力的 provider(如只读网盘)可不实现。返回新的条目数(可选)。 */
  rebuild?(): Promise<number | void>;
  /** 查询当前索引覆盖范围(已索引文件数 + 最近扫描时间)。
   *  无此能力的 provider 可不实现。 */
  getStats?(): KbIndexStats | Promise<KbIndexStats>;
}

interface KbRegistry {
  providers: KbProvider[];
}

const registry: KbRegistry = { providers: [] };

/** 注册一个知识源(去重 by id)。 */
export function registerKbProvider(p: KbProvider): void {
  if (registry.providers.some((x) => x.id === p.id)) return;
  registry.providers.push(p);
}

/** 注销一个知识源(by id)。返回是否实际移除了。 */
export function unregisterKbProvider(id: string): boolean {
  const before = registry.providers.length;
  registry.providers = registry.providers.filter((p) => p.id !== id);
  const removed = registry.providers.length < before;
  if (removed) delete statsById[id];
  return removed;
}

/**
 * 重建所有已启用 provider 的索引(刷新缓存)。返回每个 provider 重建后的条目数。
 * 无 rebuild 能力的 provider 跳过(不计入结果)。
 * 同时把重建结果(条目数 + 时间戳)写入索引状态表,供 getIndexStats 查询。
 */
export async function rebuildAllKbProviders(): Promise<Array<{ id: string; count: number | undefined }>> {
  return (await rebuildAllKbProvidersWithDiagnostics()).rebuilt;
}

export async function rebuildAllKbProvidersWithDiagnostics(): Promise<{
  rebuilt: Array<{ id: string; count: number | undefined }>;
  failures: KbSearchFailure[];
}> {
  const out: Array<{ id: string; count: number | undefined }> = [];
  const failures: KbSearchFailure[] = [];
  // Source removal is allowed during a long rebuild. Do not start queued work
  // for a removed provider, or restore its stats when in-flight work settles.
  const isCurrent = (p: KbProvider) => registry.providers.includes(p) && p.isEnabled();
  for (const p of [...registry.providers]) {
    if (!isCurrent(p) || !p.rebuild) continue;
    try {
      const count = await p.rebuild();
      if (!isCurrent(p)) continue;
      const c = typeof count === "number" ? count : undefined;
      out.push({ id: p.id, count: c });
      recordIndexStats(p.id, c);
    } catch (error) {
      if (isCurrent(p)) failures.push({ id: p.id, label: p.label, message: String(error).replace(/^Error:\s*/, "") });
    }
  }
  return { rebuilt: out, failures };
}

/** 索引状态:已索引文件数 + 最近重建时间。 */
export interface KbIndexStats {
  /** 已索引文件数(provider 返回的 count;未知为 undefined)。 */
  fileCount?: number;
  /** 最近重建/首次扫描的时间戳(ms)。 */
  lastRebuiltAt?: number;
}

/** 索引状态表:id → stats(按 provider 维护)。 */
const statsById: Record<string, KbIndexStats> = {};

/** 记录一个 provider 的索引状态(重建/首次扫描后调用)。 */
export function recordIndexStats(id: string, fileCount: number | undefined, ts: number = Date.now()): void {
  statsById[id] = { fileCount, lastRebuiltAt: ts };
}

/** 取一个 provider 的索引状态(未记录返回 {})。 */
export function getIndexStats(id: string): KbIndexStats {
  return statsById[id] ?? {};
}

/** 取所有已启用 provider 的索引状态(id → stats)。 */
export function getAllIndexStats(): Record<string, KbIndexStats> {
  const out: Record<string, KbIndexStats> = {};
  for (const p of registry.providers) {
    if (p.isEnabled() && statsById[p.id]) out[p.id] = statsById[p.id];
  }
  return out;
}

/** 清空索引状态表(测试用;resetKbRegistry 也会清)。 */
export function clearIndexStats(): void {
  for (const k of Object.keys(statsById)) delete statsById[k];
}

/** 清空所有 provider(测试用)。 */
export function resetKbRegistry(): void {
  registry.providers = [];
  for (const k of Object.keys(statsById)) delete statsById[k];
}

/** 列出已启用 provider 的 id + label。 */
export function listKbProviders(): Array<{ id: string; label: string }> {
  return registry.providers.filter((p) => p.isEnabled()).map((p) => ({ id: p.id, label: p.label }));
}

/** 列出已启用 provider 的 id + label + 索引状态(getStats 异步聚合)。 */
export async function listKbProvidersWithStats(): Promise<
  Array<{ id: string; label: string; stats: KbIndexStats }>
> {
  const out: Array<{ id: string; label: string; stats: KbIndexStats }> = [];
  for (const p of registry.providers) {
    if (!p.isEnabled()) continue;
    let stats: KbIndexStats = getIndexStats(p.id);
    if (p.getStats) {
      try {
        const s = await p.getStats();
        stats = { ...stats, ...s };
      } catch {
        /* noop */
      }
    }
    out.push({ id: p.id, label: p.label, stats });
  }
  return out;
}

/**
 * 跨所有启用 provider 搜索知识库条目(每条带 source 标识),按 provider 顺序返回。
 * 支持 async provider(list 可能返回 Promise)。
 */
export async function searchKb(query: string): Promise<KbEntry[]> {
  return (await searchKbWithDiagnostics(query)).entries;
}

/** A provider-level search failure that callers can surface without losing
 * results returned by healthy providers. */
export interface KbSearchFailure {
  id: string;
  label: string;
  message: string;
}

export interface KbSearchResult {
  entries: KbEntry[];
  failures: KbSearchFailure[];
  successfulProviders: number;
}

/**
 * Search every provider while retaining provider-level failures. The legacy
 * `searchKb` helper intentionally returns entries only; interactive surfaces
 * should use this diagnostic variant so an outage is not presented as a true
 * empty result.
 */
export async function searchKbWithDiagnostics(query: string): Promise<KbSearchResult> {
  const out: KbEntry[] = [];
  const failures: KbSearchFailure[] = [];
  let successfulProviders = 0;
  for (const p of registry.providers) {
    try {
      if (!p.isEnabled()) continue;
      const list = await p.list(query);
      for (const e of list) {
        out.push({ ...e, source: e.source || p.id });
      }
      successfulProviders += 1;
    } catch (error) {
      failures.push({
        id: p.id,
        label: p.label,
        message: String(error).replace(/^Error:\s*/, "") || "未知错误",
      });
    }
  }
  return { entries: out, failures, successfulProviders };
}

/** 统计:启用 provider 数 + 条目总数(无 query 时 list() 返回全部)。 */
export async function kbStats(): Promise<{ providers: number; entries: number }> {
  const enabled = registry.providers.filter((p) => p.isEnabled());
  let entries = 0;
  for (const p of enabled) {
    try {
      const list = await p.list();
      entries += list.length;
    } catch {
      /* noop */
    }
  }
  return { providers: enabled.length, entries };
}
