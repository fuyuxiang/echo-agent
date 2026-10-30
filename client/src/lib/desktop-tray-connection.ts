import { createTrayPublisher, type TrayAction, type TraySnapshot } from "./desktop-tray-model";

/** A deliberate cancellation consumes the click without becoming a retry loop. */
export class TrayActionCancelled extends Error {}

export interface TrayConnectionOptions {
  invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  listen: (event: string, callback: () => void) => Promise<() => void>;
  snapshot: () => TraySnapshot;
  onAction: (action: TrayAction) => Promise<void>;
  onError: (error: unknown, retry: () => void) => void;
}

/** Listen before reading pending navigation; acknowledgements survive WebView recovery. */
export function connectDesktopTray(options: TrayConnectionOptions) {
  let disposed = false, dirty = false, draining = false;
  let stop: (() => void) | undefined;
  const publisher = createTrayPublisher(
    snapshot => options.invoke("desktop_tray_sync", { snapshot }),
    error => { if (!disposed) options.onError(error, refresh); },
  );
  const drain = async () => {
    if (disposed || draining) return;
    draining = true;
    try {
      while (dirty && !disposed) {
        dirty = false;
        const action = await options.invoke<TrayAction | null>("desktop_tray_pending_action");
        if (!action || disposed) continue;
        try { await options.onAction(action); }
        catch (error) { if (!(error instanceof TrayActionCancelled)) throw error; }
        if (disposed) return;
        await options.invoke("desktop_tray_ack_action", { id: action.id });
        dirty = true;
      }
    } catch (error) {
      if (!disposed) options.onError(error, wake);
    } finally {
      draining = false;
      if (dirty && !disposed) void drain();
    }
  };
  function wake() { dirty = true; void drain(); }
  function refresh() { if (!disposed) publisher.publish(options.snapshot()); }
  let attaching = false;
  function attach() {
    if (disposed || attaching || stop) return;
    attaching = true;
    void options.listen("desktop://tray-action", wake).then(unlisten => {
      if (disposed) { unlisten(); return; }
      stop = unlisten; refresh(); wake();
    }).catch(error => { if (!disposed) options.onError(error, attach); })
      .finally(() => { attaching = false; });
  }
  attach();
  return { refresh, wake, dispose() { disposed = true; stop?.(); publisher.dispose(); } };
}
