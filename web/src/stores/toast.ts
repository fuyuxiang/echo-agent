import { create } from "zustand";

export type ToastKind = "success" | "error" | "info";

export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
}

interface ToastState {
  toasts: Toast[];
  push: (kind: ToastKind, message: string) => void;
  dismiss: (id: number) => void;
}

let seq = 0;

export const useToastStore = create<ToastState>((set) => ({
  toasts: [],
  push: (kind, message) => {
    const id = ++seq;
    set((s) => ({ toasts: [...s.toasts, { id, kind, message }] }));
    // 自动消失:错误停留久一点(6s),其余 3s。
    const ttl = kind === "error" ? 6000 : 3000;
    setTimeout(() => {
      set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
    }, ttl);
  },
  dismiss: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));

// 非组件上下文(store/事件回调)也能直接调用。
export const toast = {
  success: (m: string) => useToastStore.getState().push("success", m),
  error: (m: string) => useToastStore.getState().push("error", m),
  info: (m: string) => useToastStore.getState().push("info", m),
};

/**
 * 统一包裹写操作(POST/PUT/DELETE):失败时弹 error toast 并把异常吞掉(返回
 * false),避免未处理的 promise rejection;成功可选弹 success。调用方据返回值
 * 决定是否 refetch,无需再写 try-catch。
 */
export async function runMutation(
  fn: () => Promise<unknown>,
  opts: { success?: string; error?: string } = {},
): Promise<boolean> {
  try {
    await fn();
    if (opts.success) toast.success(opts.success);
    return true;
  } catch (e) {
    const detail = e instanceof Error ? e.message : String(e);
    toast.error(opts.error ? `${opts.error}：${detail}` : detail);
    return false;
  }
}
