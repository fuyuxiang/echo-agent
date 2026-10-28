import { existsSync, readFileSync } from "node:fs";
import { waitFor } from "@testing-library/dom";
import { afterEach, describe, expect, it, vi } from "vitest";

// Exercise the actual shipped fallback page, including its DOM event handlers.
const pagePath = ["../echo_agent/gateway/static/index.html", "echo_agent/gateway/static/index.html"]
  .find(path => existsSync(path))!;
const html = readFileSync(pagePath, "utf8");
const script = html.match(/<script>([\s\S]*?)<\/script>/)![1];

function page(options: { storage?: Map<string, string>; noStorage?: boolean } = {}) {
  document.body.innerHTML = html.replace(/<script>[\s\S]*?<\/script>/, "");
  const storage = options.storage ?? new Map<string, string>();
  const sockets: Socket[] = [];
  class Socket {
    static OPEN = 1;
    readyState = 0;
    frames: Record<string, unknown>[] = [];
    onopen = () => {};
    onclose = () => {};
    onerror = () => {};
    onmessage = (_event: { data: string }) => {};
    constructor(_url: string) { sockets.push(this); }
    send(frame: string) { this.frames.push(JSON.parse(frame)); }
    close() { this.readyState = 3; }
    open() { this.readyState = 1; this.onopen(); }
    receive(frame: Record<string, unknown>) { this.onmessage({ data: JSON.stringify(frame) }); }
    authenticate() { this.open(); this.receive({ type: "auth_ok", session_key: this.frames[0].session_key }); }
  }
  const fetch = vi.fn(async () => ({ ok: true, json: async () => ({
    api_prefix: "/api/v1", interactive_ws_path: "/ws", auth_required: false,
  }) }));
  const context = {
    document, fetch, WebSocket: Socket, AbortController,
    location: { protocol: "http:", host: "localhost" },
    sessionStorage: {
      getItem: (key: string) => { if (options.noStorage) throw Error("disabled"); return storage.get(key); },
      setItem: (key: string, value: string) => { if (options.noStorage) throw Error("disabled"); storage.set(key, value); },
    },
  };
  const api = new Function(...Object.keys(context), script + "; return { connect, sendMessage };")
    (...Object.values(context)) as { connect: () => Promise<void>; sendMessage: () => Promise<void> };
  return { sockets, fetch, storage, api };
}

function input(id: string) { return document.getElementById(id) as HTMLInputElement; }
function button(id: string) { return document.getElementById(id) as HTMLButtonElement; }
function changeKey(key: string) {
  input("clientSessionKey").value = key;
  input("clientSessionKey").dispatchEvent(new Event("change"));
}
async function connect(p: ReturnType<typeof page>) {
  const connected = p.api.connect();
  await waitFor(() => expect(p.sockets).toHaveLength(1));
  p.sockets[0].authenticate();
  await connected;
}

afterEach(() => { document.body.replaceChildren(); });

describe("Playground conversation lifecycle", () => {
  it("persists its initial conversation across reloads", () => {
    const first = page();
    const key = input("clientSessionKey").value;
    expect(key).toMatch(/^cli:playground:/);
    page({ storage: first.storage });
    expect(input("clientSessionKey").value).toBe(key);
  });

  it("reconnects after a key change and ignores the old socket's messages and close", async () => {
    const p = page();
    await connect(p);
    p.sockets[0].receive({ type: "message", text: "old conversation" });
    changeKey("cli:new");
    await waitFor(() => expect(p.sockets).toHaveLength(2));
    expect(button("sendBtn")).toBeDisabled();
    p.sockets[1].authenticate();
    p.sockets[0].receive({ type: "message", text: "late private answer" });
    p.sockets[0].onclose();
    await waitFor(() => expect(button("sendBtn")).toBeEnabled());
    expect(document.getElementById("log")!.textContent).not.toMatch(/old conversation|late private/);
    expect(document.getElementById("sessionKey")).toHaveTextContent("session: cli:new");
    input("message").value = "new message";
    await p.api.sendMessage();
    expect(p.sockets[1].frames).toEqual([
      expect.objectContaining({ type: "auth", session_key: "cli:new" }),
      { type: "message", text: "new message" },
    ]);
    expect(p.sockets[0].frames).toHaveLength(1);
  });

  it("keeps the latest key authoritative during rapid handshake switches", async () => {
    const p = page();
    await connect(p);
    changeKey("cli:B");
    await waitFor(() => expect(p.sockets).toHaveLength(2));
    changeKey("cli:C");
    await waitFor(() => expect(p.sockets).toHaveLength(3));
    p.sockets[1].open();
    p.sockets[1].receive({ type: "auth_ok", session_key: "cli:B" });
    p.sockets[1].onclose();
    expect(button("sendBtn")).toBeDisabled();
    p.sockets[2].authenticate();
    await waitFor(() => expect(document.getElementById("status")).toHaveTextContent("Connected"));
    expect(document.getElementById("sessionKey")).toHaveTextContent("cli:C");
  });

  it("switches correctly even when sessionStorage is unavailable", async () => {
    const p = page({ noStorage: true });
    await connect(p);
    button("newSessionBtn").click();
    await waitFor(() => expect(p.sockets).toHaveLength(2));
    p.sockets[1].authenticate();
    expect(p.sockets[1].frames[0].session_key).not.toBe(p.sockets[0].frames[0].session_key);
  });

  it("rejects an invalid session without continuing to send on the old socket", async () => {
    const p = page();
    await connect(p);
    changeKey("gateway:other:user");
    await waitFor(() => expect(document.getElementById("status")).toHaveTextContent("must start with cli:"));
    input("message").value = "private";
    await expect(p.api.sendMessage()).rejects.toThrow("must start with cli:");
    expect(p.sockets[0].readyState).toBe(3);
    expect(p.sockets[0].frames).toHaveLength(1);
  });

  it("ignores an HTTP reply that arrives after the user starts a new conversation", async () => {
    const p = page();
    let respond: (value: unknown) => void = () => {};
    p.fetch.mockImplementationOnce(() => new Promise(resolve => { respond = resolve as typeof respond; }));
    input("message").value = "old request";
    const sending = p.api.sendMessage();
    await waitFor(() => expect(p.fetch).toHaveBeenCalledTimes(2));
    button("newSessionBtn").click();
    respond({ ok: true, json: async () => ({ session_key: "cli:old", reply: { text: "late HTTP answer" } }) });
    await sending;
    expect(document.getElementById("log")).not.toHaveTextContent("late HTTP answer");
    expect(document.getElementById("sessionKey")).not.toHaveTextContent("cli:old");
    expect(button("sendBtn")).toBeEnabled();
  });
});
