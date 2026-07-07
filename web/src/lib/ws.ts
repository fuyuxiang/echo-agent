type Listener = (event: { type: string; payload: unknown }) => void;

export class DashboardWS {
  private ws: WebSocket | null = null;
  private listeners: Map<string, Set<Listener>> = new Map();
  private reconnectTimer: number | null = null;
  private channels: string[] = [];

  connect(token: string, channels: string[]) {
    this.channels = channels;
    const protocol = location.protocol === "https:" ? "wss:" : "ws:";
    this.ws = new WebSocket(`${protocol}//${location.host}/ws/dashboard`);

    this.ws.onopen = () => {
      this.ws!.send(JSON.stringify({ type: "auth", token }));
    };

    this.ws.onmessage = (ev) => {
      const data = JSON.parse(ev.data);
      if (data.type === "auth_ok") {
        this.ws!.send(JSON.stringify({ type: "subscribe", channels }));
        return;
      }
      const listeners = this.listeners.get(data.type);
      if (listeners) {
        listeners.forEach((fn) => fn(data));
      }
    };

    this.ws.onclose = () => {
      this.reconnectTimer = window.setTimeout(() => this.connect(token, this.channels), 3000);
    };
  }

  on(type: string, fn: Listener) {
    if (!this.listeners.has(type)) this.listeners.set(type, new Set());
    this.listeners.get(type)!.add(fn);
    return () => this.listeners.get(type)?.delete(fn);
  }

  close() {
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.ws?.close();
  }
}

export const dashboardWS = new DashboardWS();
