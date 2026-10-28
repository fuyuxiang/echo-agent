import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { StrictMode } from "react";
import { act, render, cleanup } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { useWsSubscribe } from "./use-ws";
import { dashboardWS } from "../lib/ws";
import { useAuthStore } from "../stores/auth";
import { useCapabilitiesStore } from "../stores/capabilities";

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
  handshake() {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
    this.onmessage?.({ data: JSON.stringify({ type: "auth_ok" }) });
  }
  emit(type: string, payload: unknown) {
    this.onmessage?.({ data: JSON.stringify({ type, payload }) });
  }
}

const original = globalThis.WebSocket;

function Probe({ onEvent }: { onEvent: (ev: unknown) => void }) {
  useWsSubscribe(["tasks"], onEvent, ["task_updated"]);
  return null;
}

beforeEach(() => {
  FakeWebSocket.instances = [];
  (globalThis as any).WebSocket = FakeWebSocket;
  useAuthStore.setState({ token: "t1" });
  // This suite exercises WebSocket timing, not the asynchronous capabilities
  // probe. Pin its already-known result so a background HTTP rejection cannot
  // update the mounted Probe after the assertion has finished.
  useCapabilitiesStore.setState({
    admin: true,
    authRequired: true,
    inflight: null,
  });
});

afterEach(() => {
  cleanup();
  dashboardWS.close();
  (globalThis as any).WebSocket = original;
  localStorage.clear();
});

describe("useWsSubscribe", () => {
  it("StrictMode 双挂载只开一条连接,事件不重复", () => {
    const onEvent = vi.fn();
    render(
      <StrictMode>
        <MemoryRouter>
          <Probe onEvent={onEvent} />
        </MemoryRouter>
      </StrictMode>,
    );
    expect(FakeWebSocket.instances).toHaveLength(1);
    act(() => {
      FakeWebSocket.instances[0].handshake();
      FakeWebSocket.instances[0].emit("task_updated", { id: "a" });
    });
    expect(onEvent).toHaveBeenCalledTimes(1);
  });

  it("卸载后关闭连接,重新挂载不留旧连接", () => {
    vi.useFakeTimers();
    try {
      const { unmount } = render(
        <MemoryRouter>
          <Probe onEvent={() => {}} />
        </MemoryRouter>,
      );
      const first = FakeWebSocket.instances[0];
      first.handshake();
      unmount();
      // 空闲宽限期过后才真正关闭。
      vi.advanceTimersByTime(500);
      expect(first.readyState).toBe(FakeWebSocket.CLOSED);

      render(
        <MemoryRouter>
          <Probe onEvent={() => {}} />
        </MemoryRouter>,
      );
      expect(FakeWebSocket.instances).toHaveLength(2);
      expect(
        FakeWebSocket.instances.filter((s) => s.readyState !== FakeWebSocket.CLOSED),
      ).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("handler 变化不重建连接", () => {
    const { rerender } = render(
      <MemoryRouter>
        <Probe onEvent={() => {}} />
      </MemoryRouter>,
    );
    FakeWebSocket.instances[0].handshake();
    rerender(
      <MemoryRouter>
        <Probe onEvent={() => {}} />
      </MemoryRouter>,
    );
    expect(FakeWebSocket.instances).toHaveLength(1);
  });

  it("最新 handler 收到事件,不是首次渲染那个闭包", () => {
    const first = vi.fn();
    const second = vi.fn();
    const { rerender } = render(
      <MemoryRouter>
        <Probe onEvent={first} />
      </MemoryRouter>,
    );
    FakeWebSocket.instances[0].handshake();
    rerender(
      <MemoryRouter>
        <Probe onEvent={second} />
      </MemoryRouter>,
    );
    FakeWebSocket.instances[0].emit("task_updated", { id: "b" });
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
  });

  it("auth_error 时清空 token", () => {
    render(
      <MemoryRouter>
        <Probe onEvent={() => {}} />
      </MemoryRouter>,
    );
    const socket = FakeWebSocket.instances[0];
    socket.readyState = FakeWebSocket.OPEN;
    act(() => {
      socket.onopen?.();
      socket.onmessage?.({ data: JSON.stringify({ type: "auth_error", message: "invalid token" }) });
    });
    expect(useAuthStore.getState().token).toBeNull();
  });

  it("无 token 时不建立连接", () => {
    useAuthStore.setState({ token: null });
    render(
      <MemoryRouter>
        <Probe onEvent={() => {}} />
      </MemoryRouter>,
    );
    expect(FakeWebSocket.instances).toHaveLength(0);
  });
});
