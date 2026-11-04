import { useEffect, useRef, useState } from "react";
import { automationRunResult, type AutomationRunResult } from "@/lib/agent-client";
import type { AutomationRunRecord } from "@/lib/types";
import { runBusy } from "./task-presentation";

/** One occurrence owns every request, timer and result, even in a reused chat. */
export function useRunPreview(run?: AutomationRunRecord) {
  const id = run?.id;
  const sessionId = run?.sessionId;
  const busy = Boolean(run && runBusy(run));
  const [reload, setReload] = useState(0);
  const [snapshot, setSnapshot] = useState<{
    id?: string; result: AutomationRunResult | null; error: string; loading: boolean;
  }>({ result: null, error: "", loading: false });
  const previous = useRef<{ id?: string; busy: boolean }>({ busy: false });

  useEffect(() => {
    const wasBusy = previous.current.id === id && previous.current.busy;
    previous.current = { id, busy };
    let active = true;
    let inFlight = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const finished = Date.parse(run?.finishedAt ?? "");
    // A terminal event can precede the final disk flush. Reconcile that brief
    // handoff without polling old historical records indefinitely.
    let finalReads = !busy && (wasBusy || Number.isFinite(finished) && Date.now() - finished < 10_000) ? 2 : 0;
    if (!id || !sessionId) {
      setSnapshot({ id, result: null, error: "", loading: false });
      return;
    }
    setSnapshot(current => current.id === id
      ? { ...current, loading: !current.result }
      : { id, result: null, error: "", loading: true });

    const schedule = () => {
      if (!active || !busy && finalReads <= 0) return;
      const wait = busy ? 2_000 : 750;
      timer = setTimeout(() => {
        if (document.visibilityState === "hidden") { schedule(); return; }
        if (!busy) finalReads--;
        void read();
      }, wait);
    };
    const read = async () => {
      if (!active || inFlight) return;
      clearTimeout(timer);
      inFlight = true;
      try {
        const result = await automationRunResult(id);
        if (active) setSnapshot(current => {
          if (current.id === id && !current.error && !current.loading
            && current.result?.text === result.text && current.result.truncated === result.truncated
            && JSON.stringify(current.result.progress) === JSON.stringify(result.progress)) return current;
          return { id, result, error: "", loading: false };
        });
      } catch (error) {
        if (active) setSnapshot(current => ({ id,
          result: current.id === id ? current.result : null,
          error: String(error).replace(/^Error:\s*/, ""), loading: false }));
      } finally {
        inFlight = false;
        schedule();
      }
    };
    const refreshVisible = () => { if (document.visibilityState !== "hidden") void read(); };
    window.addEventListener("focus", refreshVisible);
    document.addEventListener("visibilitychange", refreshVisible);
    void read();
    return () => {
      active = false;
      clearTimeout(timer);
      window.removeEventListener("focus", refreshVisible);
      document.removeEventListener("visibilitychange", refreshVisible);
    };
  }, [id, sessionId, busy, run?.status, run?.finishedAt, reload]);

  return {
    result: snapshot.id === id ? snapshot.result : null,
    error: snapshot.id === id ? snapshot.error : "",
    loading: snapshot.id === id && snapshot.loading,
    retry: () => setReload(value => value + 1),
  };
}
