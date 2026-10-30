import { useEffect, useRef, useState } from "react";
import { agentSessionInfo, agentSessionUsage } from "./agent-client";
import type { ContextInfo, SessionUsage } from "./types";

const REFRESH_INTERVAL_MS = 3_000;

/** Runtime context occupancy is separate from cumulative, billable token usage. */
export function useContextUsage(sessionId: string | undefined, active: boolean, detailsOpen: boolean, refreshSignal: unknown) {
  const [context, setContext] = useState<{ sessionId: string; value: ContextInfo }>();
  const [usage, setUsage] = useState<{ sessionId: string; value: SessionUsage }>();
  const refreshRef = useRef<() => void>();
  const detailsRef = useRef(detailsOpen);
  detailsRef.current = detailsOpen;

  useEffect(() => {
    setContext(undefined);
    setUsage(undefined);
    if (!sessionId) return;
    let disposed = false;

    // Coalesce invalidations during a slow read into one trailing read. Each
    // endpoint has its own queue, so usage details never delay the ring.
    function reader<T>(read: () => Promise<T>, accept: (value: T) => void) {
      let running = false;
      let pending = false;
      return () => {
        pending = true;
        if (running) return;
        running = true;
        void (async () => {
          try {
            while (pending && !disposed && document.visibilityState !== "hidden") {
              pending = false;
              try {
                const value = await read();
                if (!disposed) accept(value);
              } catch { /* Keep the last known snapshot; the next refresh retries. */ }
            }
          } finally {
            running = false;
          }
        })();
      };
    }

    const refreshContext = reader(() => agentSessionInfo(sessionId), (info) => {
      if (info?.context?.total > 0) setContext({ sessionId, value: info.context });
    });
    const refreshUsage = reader(() => agentSessionUsage(sessionId), (value) => {
      setUsage({ sessionId, value });
    });
    const refresh = () => {
      if (disposed || document.visibilityState === "hidden") return;
      refreshContext();
      if (detailsRef.current) refreshUsage();
    };
    refreshRef.current = refresh;
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      disposed = true;
      refreshRef.current = undefined;
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [sessionId]);

  // Includes turn start/end and completed session/model loading. Do not depend
  // on streamed text: one token must not produce one native/runtime request.
  useEffect(() => {
    refreshRef.current?.();
  }, [sessionId, active, detailsOpen, refreshSignal]);

  useEffect(() => {
    if (!sessionId || (!active && !detailsOpen)) return;
    const timer = window.setInterval(() => refreshRef.current?.(), REFRESH_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [sessionId, active, detailsOpen]);

  return {
    ctx: context?.sessionId === sessionId ? context?.value : undefined,
    usage: usage?.sessionId === sessionId ? usage?.value : undefined,
  };
}
