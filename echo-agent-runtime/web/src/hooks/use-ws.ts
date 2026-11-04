import { useEffect, useMemo, useRef } from "react";
import { dashboardWS } from "../lib/ws";
import { useAuthStore } from "../stores/auth";
import { useAuthRequired } from "../stores/capabilities";

type WsEvent = { type: string; payload: unknown };

/**
 * Subscribe to dashboard WS channels for the lifetime of the calling component.
 *
 * The handler is held in a ref so a fresh closure on every render (the normal
 * case) does not re-run the effect: only the token and the channel set do.
 * Cleanup releases this component's channel references, so the shared socket
 * closes once nothing is subscribed — previously the connection stayed open
 * (and a new one was created on the next mount).
 */
export function useWsSubscribe(
  channels: string[],
  handler: (ev: WsEvent) => void,
  eventTypes: string[],
) {
  const token = useAuthStore((s) => s.token);
  const generation = useAuthStore((s) => s.generation);
  const authRequired = useAuthRequired();

  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  // Compare by content, not array identity: callers pass inline literals.
  const channelKey = channels.join(",");
  const typesKey = eventTypes.join(",");
  const channelList = useMemo(() => channelKey.split(",").filter(Boolean), [channelKey]);
  const typeList = useMemo(() => typesKey.split(",").filter(Boolean), [typesKey]);

  useEffect(() => {
    if ((authRequired !== false && !token) || channelList.length === 0) return;

    dashboardWS.openMode = authRequired === false;

    // Register listeners first: with a warm socket, events can arrive on the
    // same tick as subscribe().
    const unsubs = typeList.map((type) =>
      dashboardWS.on(type, (ev) => handlerRef.current(ev)),
    );
    const release = dashboardWS.subscribe(token || "", channelList);

    return () => {
      unsubs.forEach((u) => u());
      release();
    };
  }, [token, generation, authRequired, channelList, typeList]);
}
