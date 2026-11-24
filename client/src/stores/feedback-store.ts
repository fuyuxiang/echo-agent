/**
 * 消息反馈 store —— 对齐 EchoAgent `echo-chat-ui/message-feedback`。
 *
 * 按 `sessionId:messageId` 存反馈:thumbs up/down + 可选 1–5 星评分 + 文字备注,
 * 持久化到 localStorage(`echoagent.feedback`),使反馈跨刷新保留。纯本地、无后端
 * 上报(EchoAgent 是 BYOK,没有可上报的计费后端)。
 *
 * EchoAgent 的 thumbs up/down 会弹出评分条 + 反馈弹窗(rating bar + 反馈文本);
 * 这里用 `stars`(1–5) + `note` 复刻该完整评分能力。
 */
import { create } from "zustand";

export type FeedbackRating = "up" | "down";

export interface FeedbackEntry {
  rating: FeedbackRating;
  /** 1–5 星评分(可选;仅在用户展开评分条时填写)。 */
  stars?: number;
  /** 可选文字备注。 */
  note?: string;
  ts: number;
}

/** 复合 key:`sessionId:messageId`。 */
export type FeedbackMap = Record<string, FeedbackEntry>;

const STORAGE_KEY = "echoagent.feedback";

function keyOf(sessionId: string, messageId: string): string {
  return `${sessionId}:${messageId}`;
}

/** 校验 stars 落在 1–5。 */
function clampStars(stars: number | undefined): number | undefined {
  if (stars == null) return undefined;
  if (!Number.isFinite(stars)) return undefined;
  return Math.max(1, Math.min(5, Math.round(stars)));
}

function load(): FeedbackMap {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const obj = JSON.parse(raw);
    return obj && typeof obj === "object" ? (obj as FeedbackMap) : {};
  } catch {
    return {};
  }
}

function save(map: FeedbackMap) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(map));
  } catch {
    /* quota / 隐私模式 — 静默降级为仅内存 */
  }
}

interface FeedbackState {
  /** 复合 key → 反馈条目。 */
  entries: FeedbackMap;
  /** Ephemeral receipt of an explicit submission. Reading saved feedback never starts a new request. */
  lastSubmission?: { key: string; ts: number };
  /** 设置反馈(toggle:再点同向 rating 会取消)。 */
  setRating: (sessionId: string, messageId: string, rating: FeedbackRating) => void;
  /** 设置完整评分(rating + stars + note)。 */
  setDetailed: (
    sessionId: string,
    messageId: string,
    data: { rating: FeedbackRating; stars?: number; note?: string },
  ) => void;
  /** 显式清除一条反馈。 */
  clearRating: (sessionId: string, messageId: string) => void;
  /** 读取一条反馈(无则 null)。 */
  getRating: (sessionId: string, messageId: string) => FeedbackEntry | null;
  /** 测试/重置用:直接替换整个 map。 */
  __replace: (map: FeedbackMap) => void;
}

export const useFeedbackStore = create<FeedbackState>((set, get) => ({
  entries: load(),
  setRating: (sessionId, messageId, rating) =>
    set((s) => {
      const k = keyOf(sessionId, messageId);
      const prev = s.entries[k];
      // toggle:再点同向 → 取消。
      if (prev && prev.rating === rating) {
        const next = { ...s.entries };
        delete next[k];
        save(next);
        return { entries: next };
      }
      const next: FeedbackMap = {
        ...s.entries,
        [k]: { rating, ts: Date.now() },
      };
      save(next);
      return { entries: next };
    }),
  setDetailed: (sessionId, messageId, data) =>
    set((s) => {
      const k = keyOf(sessionId, messageId);
      const ts = Date.now();
      const next: FeedbackMap = {
        ...s.entries,
        [k]: {
          rating: data.rating,
          stars: clampStars(data.stars),
          note: data.note?.trim() || undefined,
          ts,
        },
      };
      save(next);
      return { entries: next, lastSubmission: { key: k, ts } };
    }),
  clearRating: (sessionId, messageId) =>
    set((s) => {
      const k = keyOf(sessionId, messageId);
      if (!(k in s.entries)) return s;
      const next = { ...s.entries };
      delete next[k];
      save(next);
      return { entries: next };
    }),
  getRating: (sessionId, messageId) => {
    return get().entries[keyOf(sessionId, messageId)] ?? null;
  },
  __replace: (map) => {
    save(map);
    set({ entries: map, lastSubmission: undefined });
  },
}));

/** stars 校验纯函数(供 UI 与测试共用)。 */
export function normalizeStars(stars: number | undefined): number | undefined {
  return clampStars(stars);
}
