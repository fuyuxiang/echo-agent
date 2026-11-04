import { useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import { dashboardWS } from "../lib/ws";
import { useWsSubscribe } from "../hooks/use-ws";

/** Keep one shell subscription alive, including on pages with no live data. */
export function ConnectionStatus() {
  const { t } = useTranslation("common");
  useWsSubscribe(["tasks"], () => {}, []);
  const state = useSyncExternalStore(dashboardWS.subscribeConnectionState, dashboardWS.getConnectionState);
  if (state === "idle" || state === "connected") return null;
  return <div role="status" className="mb-3 rounded border border-amber-200 bg-amber-50 p-2 text-sm text-amber-800">
    {t(state === "connecting" ? "liveConnecting" : "liveDisconnected")}
  </div>;
}
