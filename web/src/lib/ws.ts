import { useAuthStore } from "../stores/auth";

type Listener = (event: { type: string; payload: unknown }) => void;

/** Backoff schedule for reconnects, in ms. A fixed 3s retry meant a stale
 *  token (server sends auth_error then closes) span a reconnect every 3s
 *  forever; growing the delay bounds that, and onAuthFailure stops it. */
const RECONNECT_DELAYS = [1000, 2000, 5000, 10000, 30000];

/** Grace period before closing an idle socket. React StrictMode (and any
 *  remount, e.g. navigating away and back) tears the subscription down and
 *  immediately re-creates it; closing synchronously would churn a connection
 *  each time. */
const IDLE_CLOSE_DELAY = 250;

/**
 * Single multiplexed dashboard socket.
 *
 * One socket is shared by every subscriber. `subscribe()` reference-counts
 * channels so a page mounting twice (React StrictMode) or being revisited
 * reuses the live socket instead of opening another — the previous version
 * overwrote `this.ws` on each connect(), leaking the old socket (still
 * delivering events, so every handler fired twice) and leaving the server's
 * client table to grow until those sockets happened to drop.
 */
export class DashboardWS {
  private ws: WebSocket | null = null;
  private listeners: Map<string, Set<Listener>> = new Map();
  private reconnectTimer: number | null = null;
  private attempt = 0;
  /** channel → number of live subscribers */
  private channelRefs: Map<string, number> = new Map();
  /** channels the *server* has been told about on the current socket */
  private sentChannels: Set<string> = new Set();
  private token = "";
  private authFailed = false;
  /** true once auth_ok arrived on the current socket — the server rejects
   *  subscribe frames sent before that, so channel sync has to wait. */
  private authed = false;
  /** sockets we closed on purpose; their onclose must not trigger a reconnect */
  private retired: WeakSet<WebSocket> = new WeakSet();
  private idleTimer: number | null = null;
  /** When true the server requires no token; reconnect even with an empty one. */
  openMode = false;

  /** Called when the server rejects the token, so the UI can send the user
   *  back to the login screen instead of silently retrying forever. */
  onAuthFailure: (() => void) | null = null;

  private get channels(): string[] {
    return [...this.channelRefs.keys()];
  }

  /**
   * Register interest in `channels`. Returns an unsubscribe function that
   * releases exactly this subscriber's references; the socket closes once the
   * last one is gone.
   */
  subscribe(token: string, channels: string[]): () => void {
    // A different token means a different identity — drop the old socket so we
    // never keep streaming under stale credentials.
    if (this.token && this.token !== token) {
      this.reset();
    }
    this.token = token;
    this.authFailed = false;
    this.cancelIdleClose();

    for (const ch of channels) {
      this.channelRefs.set(ch, (this.channelRefs.get(ch) ?? 0) + 1);
    }

    this.ensureConnected();
    this.syncChannels();

    let released = false;
    return () => {
      if (released) return;
      released = true;
      for (const ch of channels) {
        const next = (this.channelRefs.get(ch) ?? 0) - 1;
        if (next > 0) this.channelRefs.set(ch, next);
        else this.channelRefs.delete(ch);
      }
      if (this.channelRefs.size === 0) this.scheduleIdleClose();
      else this.syncChannels();
    };
  }

  /** Close the socket if nothing has re-subscribed within the grace period. */
  private scheduleIdleClose() {
    if (this.idleTimer !== null) return;
    this.idleTimer = window.setTimeout(() => {
      this.idleTimer = null;
      if (this.channelRefs.size === 0) this.reset();
    }, IDLE_CLOSE_DELAY);
  }

  private cancelIdleClose() {
    if (this.idleTimer !== null) {
      clearTimeout(this.idleTimer);
      this.idleTimer = null;
    }
  }

  /** Open the socket if there isn't already one connecting or open. */
  private ensureConnected() {
    if (this.authFailed) return;
    if (this.ws && (this.ws.readyState === WebSocket.OPEN || this.ws.readyState === WebSocket.CONNECTING)) {
      return;
    }
    const protocol = location.protocol === "https:" ? "wss:" : "ws:";
    const ws = new WebSocket(`${protocol}//${location.host}/ws/dashboard`);
    this.ws = ws;
    this.authed = false;

    ws.onopen = () => {
      this.sentChannels = new Set();
      ws.send(JSON.stringify({ type: "auth", token: this.token }));
    };

    ws.onmessage = (ev) => {
      let data: { type?: string; payload?: unknown };
      try {
        data = JSON.parse(ev.data);
      } catch {
        return; // ignore non-JSON frames rather than throwing inside onmessage
      }
      if (data.type === "auth_ok") {
        this.attempt = 0;
        this.authed = true;
        this.syncChannels();
        return;
      }
      if (data.type === "auth_error") {
        // The server closes right after this frame. Retrying would spin, so
        // latch the failure and hand control back to the app.
        this.authFailed = true;
        this.reset();
        this.onAuthFailure?.();
        return;
      }
      if (!data.type) return;
      this.listeners.get(data.type)?.forEach((fn) => fn(data as { type: string; payload: unknown }));
    };

    ws.onclose = () => {
      // Only the socket we currently track may drive state. A retired socket's
      // late onclose must not clear a newer socket or start a reconnect.
      if (this.retired.has(ws)) return;
      if (this.ws !== ws) return;
      this.ws = null;
      this.authed = false;
      this.sentChannels = new Set();
      if (this.authFailed) return;
      if (this.channelRefs.size === 0) return;
      if (!useAuthStore.getState().token && !this.openMode) return;
      this.scheduleReconnect();
    };
  }

  private scheduleReconnect() {
    if (this.reconnectTimer !== null) return;
    const delay = RECONNECT_DELAYS[Math.min(this.attempt, RECONNECT_DELAYS.length - 1)];
    this.attempt += 1;
    this.reconnectTimer = window.setTimeout(() => {
      this.reconnectTimer = null;
      const current = useAuthStore.getState().token;
      if (!current && !this.openMode) return;
      if (this.channelRefs.size === 0) return;
      this.token = current || "";
      this.ensureConnected();
    }, delay);
  }

  /** Push the union of all subscribers' channels to the server, once open.
   *  Sends only the delta so a re-subscribe doesn't re-send the whole set. */
  private syncChannels() {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN || !this.authed) return;
    const wanted = new Set(this.channels);
    const added = [...wanted].filter((ch) => !this.sentChannels.has(ch));
    const removed = [...this.sentChannels].filter((ch) => !wanted.has(ch));
    if (added.length) this.ws.send(JSON.stringify({ type: "subscribe", channels: added }));
    if (removed.length) this.ws.send(JSON.stringify({ type: "unsubscribe", channels: removed }));
    this.sentChannels = wanted;
  }

  on(type: string, fn: Listener) {
    if (!this.listeners.has(type)) this.listeners.set(type, new Set());
    this.listeners.get(type)!.add(fn);
    return () => {
      const set = this.listeners.get(type);
      set?.delete(fn);
      if (set && set.size === 0) this.listeners.delete(type);
    };
  }

  /** Tear down the socket and any pending reconnect, keeping listeners. */
  private reset() {
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.cancelIdleClose();
    this.attempt = 0;
    this.authed = false;
    this.sentChannels = new Set();
    const ws = this.ws;
    this.ws = null;
    if (ws) {
      this.retired.add(ws);
      if (ws.readyState !== WebSocket.CLOSED) ws.close();
    }
  }

  /** Full teardown, e.g. on logout. */
  close() {
    this.channelRefs.clear();
    this.token = "";
    this.reset();
  }
}

export const dashboardWS = new DashboardWS();
