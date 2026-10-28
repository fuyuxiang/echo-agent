import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { DashboardWS } from "./ws";
import { useAuthStore } from "../stores/auth";

/** Minimal WebSocket double: records every instance so the tests can assert
 *  how many sockets were opened, and drives the handshake by hand. */
class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  readyState = FakeWebSocket.CONNECTING;
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;

  constructor(public url: string) {
    FakeWebSocket.instances.push(this);
  }

  send(data: string) {
    this.sent.push(data);
  }

  close() {
    if (this.readyState === FakeWebSocket.CLOSED) return;
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.();
  }

  /** open + auth_ok, i.e. a fully established session */
  handshake() {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
    this.onmessage?.({ data: JSON.stringify({ type: "auth_ok" }) });
  }

  emit(type: string, payload: unknown) {
    this.onmessage?.({ data: JSON.stringify({ type, payload }) });
  }

  get frames(): Array<{ type: string; channels?: string[] }> {
    return this.sent.map((s) => JSON.parse(s));
  }
}

const original = globalThis.WebSocket;

beforeEach(() => {
  vi.useFakeTimers();
  FakeWebSocket.instances = [];
  (globalThis as any).WebSocket = FakeWebSocket;
  useAuthStore.setState({ token: "t1" });
});

afterEach(() => {
  vi.useRealTimers();
  (globalThis as any).WebSocket = original;
  localStorage.clear();
});

describe("DashboardWS 连接复用", () => {
  it("重复订阅同一频道只开一条连接", () => {
    const ws = new DashboardWS();
    ws.subscribe("t1", ["tasks"]);
    ws.subscribe("t1", ["tasks"]);
    expect(FakeWebSocket.instances).toHaveLength(1);
  });

  it("事件只分发一次,不因二次订阅重复", () => {
    const ws = new DashboardWS();
    const seen: unknown[] = [];
    ws.on("task_updated", (ev) => seen.push(ev.payload));
    ws.subscribe("t1", ["tasks"]);
    ws.subscribe("t1", ["tasks"]);
    FakeWebSocket.instances[0].handshake();
    FakeWebSocket.instances[0].emit("task_updated", { id: "a" });
    expect(seen).toEqual([{ id: "a" }]);
  });

  it("最后一个订阅者退订后才关闭连接", () => {
    const ws = new DashboardWS();
    const releaseA = ws.subscribe("t1", ["tasks"]);
    const releaseB = ws.subscribe("t1", ["tasks"]);
    const socket = FakeWebSocket.instances[0];
    socket.handshake();

    releaseA();
    vi.advanceTimersByTime(500);
    expect(socket.readyState).toBe(FakeWebSocket.OPEN);
    releaseB();
    // 关闭有一段空闲宽限期,给 StrictMode 的立即重挂载留出复用窗口。
    expect(socket.readyState).toBe(FakeWebSocket.OPEN);
    vi.advanceTimersByTime(500);
    expect(socket.readyState).toBe(FakeWebSocket.CLOSED);
  });

  it("宽限期内重新订阅复用同一条连接", () => {
    const ws = new DashboardWS();
    const release = ws.subscribe("t1", ["tasks"]);
    const socket = FakeWebSocket.instances[0];
    socket.handshake();
    release();
    ws.subscribe("t1", ["tasks"]);
    vi.advanceTimersByTime(500);
    expect(FakeWebSocket.instances).toHaveLength(1);
    expect(socket.readyState).toBe(FakeWebSocket.OPEN);
  });

  it("重复调用同一个 release 不会误减引用计数", () => {
    const ws = new DashboardWS();
    const releaseA = ws.subscribe("t1", ["tasks"]);
    ws.subscribe("t1", ["tasks"]);
    const socket = FakeWebSocket.instances[0];
    socket.handshake();
    releaseA();
    releaseA();
    vi.advanceTimersByTime(500);
    expect(socket.readyState).toBe(FakeWebSocket.OPEN);
  });

  it("subscribe 帧在 auth_ok 之后才发,且只发增量频道", () => {
    const ws = new DashboardWS();
    ws.subscribe("t1", ["tasks"]);
    const socket = FakeWebSocket.instances[0];
    socket.readyState = FakeWebSocket.OPEN;
    socket.onopen?.();
    expect(socket.frames.map((f) => f.type)).toEqual(["auth"]);

    socket.onmessage?.({ data: JSON.stringify({ type: "auth_ok" }) });
    expect(socket.frames[1]).toEqual({ type: "subscribe", channels: ["tasks"] });

    ws.subscribe("t1", ["sessions"]);
    expect(socket.frames[2]).toEqual({ type: "subscribe", channels: ["sessions"] });
  });

  it("部分退订只发 unsubscribe,连接保持", () => {
    const ws = new DashboardWS();
    ws.subscribe("t1", ["tasks"]);
    const socket = FakeWebSocket.instances[0];
    socket.handshake();
    const release = ws.subscribe("t1", ["sessions"]);
    release();
    expect(socket.frames.at(-1)).toEqual({ type: "unsubscribe", channels: ["sessions"] });
    expect(socket.readyState).toBe(FakeWebSocket.OPEN);
  });
});

describe("DashboardWS 重连", () => {
  it("断线后按退避重连,并恢复全部频道", () => {
    const ws = new DashboardWS();
    ws.subscribe("t1", ["tasks"]);
    ws.subscribe("t1", ["sessions"]);
    FakeWebSocket.instances[0].handshake();

    FakeWebSocket.instances[0].close();
    expect(FakeWebSocket.instances).toHaveLength(1); // 不立即重连

    vi.advanceTimersByTime(1000);
    expect(FakeWebSocket.instances).toHaveLength(2);
    FakeWebSocket.instances[1].handshake();
    const subscribed = FakeWebSocket.instances[1].frames.find((f) => f.type === "subscribe");
    expect(subscribed?.channels?.sort()).toEqual(["sessions", "tasks"]);
  });

  it("连续失败时退避递增,不是固定 3 秒", () => {
    const ws = new DashboardWS();
    ws.subscribe("t1", ["tasks"]);
    FakeWebSocket.instances[0].close();
    vi.advanceTimersByTime(1000);
    expect(FakeWebSocket.instances).toHaveLength(2);

    FakeWebSocket.instances[1].close();
    vi.advanceTimersByTime(1000);
    expect(FakeWebSocket.instances).toHaveLength(2); // 第二次要等更久
    vi.advanceTimersByTime(1000);
    expect(FakeWebSocket.instances).toHaveLength(3);
  });

  it("auth_ok 后重置退避", () => {
    const ws = new DashboardWS();
    ws.subscribe("t1", ["tasks"]);
    FakeWebSocket.instances[0].close();
    vi.advanceTimersByTime(1000);
    FakeWebSocket.instances[1].handshake();

    FakeWebSocket.instances[1].close();
    vi.advanceTimersByTime(1000);
    expect(FakeWebSocket.instances).toHaveLength(3);
  });

  it("token 失效收到 auth_error 后停止重连并回调", () => {
    const ws = new DashboardWS();
    const onAuthFailure = vi.fn();
    ws.onAuthFailure = onAuthFailure;
    ws.subscribe("t1", ["tasks"]);
    const socket = FakeWebSocket.instances[0];
    socket.readyState = FakeWebSocket.OPEN;
    socket.onopen?.();
    socket.onmessage?.({ data: JSON.stringify({ type: "auth_error", message: "invalid token" }) });

    expect(onAuthFailure).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(60_000);
    expect(FakeWebSocket.instances).toHaveLength(1);
  });

  it("store 里 token 已清空时不再重连", () => {
    const ws = new DashboardWS();
    ws.subscribe("t1", ["tasks"]);
    FakeWebSocket.instances[0].handshake();
    useAuthStore.setState({ token: null });
    FakeWebSocket.instances[0].close();
    vi.advanceTimersByTime(60_000);
    expect(FakeWebSocket.instances).toHaveLength(1);
  });

  it("退订后断线不再重连", () => {
    const ws = new DashboardWS();
    const release = ws.subscribe("t1", ["tasks"]);
    FakeWebSocket.instances[0].handshake();
    release();
    vi.advanceTimersByTime(60_000);
    expect(FakeWebSocket.instances).toHaveLength(1);
    expect(FakeWebSocket.instances[0].readyState).toBe(FakeWebSocket.CLOSED);
  });

  it("换 token 时丢弃旧连接,旧连接的 onclose 不触发重连", () => {
    const ws = new DashboardWS();
    ws.subscribe("t1", ["tasks"]);
    const first = FakeWebSocket.instances[0];
    first.handshake();

    ws.subscribe("t2", ["tasks"]);
    expect(first.readyState).toBe(FakeWebSocket.CLOSED);
    expect(FakeWebSocket.instances).toHaveLength(2);

    vi.advanceTimersByTime(60_000);
    expect(FakeWebSocket.instances).toHaveLength(2);
    FakeWebSocket.instances[1].handshake();
    expect(JSON.parse(FakeWebSocket.instances[1].sent[0])).toEqual({
      type: "auth",
      token: "t2",
    });
  });

  it("非 JSON 帧被忽略,不影响后续事件", () => {
    const ws = new DashboardWS();
    const seen: unknown[] = [];
    ws.on("task_updated", (ev) => seen.push(ev.payload));
    ws.subscribe("t1", ["tasks"]);
    const socket = FakeWebSocket.instances[0];
    socket.handshake();
    expect(() => socket.onmessage?.({ data: "<html>" })).not.toThrow();
    socket.emit("task_updated", { id: "b" });
    expect(seen).toEqual([{ id: "b" }]);
  });

  it("close() 后续断线不触发重连", () => {
    const ws = new DashboardWS();
    ws.subscribe("t1", ["tasks"]);
    FakeWebSocket.instances[0].handshake();
    ws.close();
    vi.advanceTimersByTime(60_000);
    expect(FakeWebSocket.instances).toHaveLength(1);
  });
});
