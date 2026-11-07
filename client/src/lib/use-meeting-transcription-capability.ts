import { isRemovedModelId, isRemovedModelProvider } from "@/lib/retired-models";
import { useCallback, useEffect, useState } from "react";
import { isMiniMaxMeetingModel, meetingCheckConnection } from "./meeting-minutes";

interface MeetingModel {
  id: string;
  providerId?: string;
  remoteModelId?: string;
  providerKind?: string;
  source?: "personal" | "organization" | "legacy";
}

type ProbeState = "ineligible" | "checking" | "available" | "unavailable";

/** The model slug narrows candidates; the live endpoint/key check decides availability. */
export function useMeetingTranscriptionCapability(model?: MeetingModel, refreshToken?: unknown) {
  const eligible = Boolean(model?.providerId
    && !isRemovedModelProvider({ id: model.providerId ?? "", source: model.source })
    && !isRemovedModelId(model.id)
    && isMiniMaxMeetingModel(model));
  const key = eligible ? `${model!.id}\u0000${model!.providerId}\u0000${model!.remoteModelId ?? ""}` : "";
  const [attempt, setAttempt] = useState(0);
  const [probe, setProbe] = useState<{ key: string; state: ProbeState; error?: string }>({
    key: "",
    state: "ineligible",
  });

  useEffect(() => {
    if (!eligible || !model?.providerId) {
      setProbe({ key: "", state: "ineligible" });
      return;
    }
    let canceled = false;
    const modelId = model.id;
    const providerId = model.providerId;
    setProbe({ key, state: "checking" });
    void meetingCheckConnection(modelId, providerId).then(() => {
      if (!canceled) setProbe({ key, state: "available" });
    }).catch((error) => {
      if (!canceled) setProbe({ key, state: "unavailable", error: String(error).replace(/^Error:\s*/, "") });
    });
    return () => { canceled = true; };
  }, [key, eligible, refreshToken, attempt]);

  const retry = useCallback(() => setAttempt((value) => value + 1), []);
  const current = probe.key === key ? probe : { key, state: eligible ? "checking" as const : "ineligible" as const };
  return {
    eligible,
    available: current.state === "available",
    state: current.state,
    error: current.error,
    retry,
  };
}
