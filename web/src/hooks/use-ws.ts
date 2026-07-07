import { useEffect } from "react";
import { dashboardWS } from "../lib/ws";
import { useAuthStore } from "../stores/auth";

export function useWsSubscribe(channels: string[], handler: (ev: any) => void, eventTypes: string[]) {
  const token = useAuthStore((s) => s.token);

  useEffect(() => {
    if (!token) return;
    dashboardWS.connect(token, channels);

    const unsubs = eventTypes.map((type) => dashboardWS.on(type, handler));
    return () => {
      unsubs.forEach((u) => u());
    };
  }, [token, channels.join(",")]);
}
