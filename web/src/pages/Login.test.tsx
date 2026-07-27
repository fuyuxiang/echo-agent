import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { Login } from "./Login";
import * as api from "../lib/api";
import { TOKEN_STORAGE_KEY, RETURN_TO_STORAGE_KEY } from "../lib/api";
import { useAuthStore } from "../stores/auth";

const navigate = vi.fn();
vi.mock("react-router", async () => {
  const actual = await vi.importActual<typeof import("react-router")>("react-router");
  return { ...actual, useNavigate: () => navigate };
});

function submit(token: string) {
  fireEvent.change(screen.getByLabelText("Admin Token"), { target: { value: token } });
  fireEvent.click(screen.getByRole("button", { name: "登录" }));
}

beforeEach(() => {
  localStorage.clear();
  sessionStorage.clear();
  navigate.mockClear();
  useAuthStore.setState({ token: null });
  render(<MemoryRouter><Login /></MemoryRouter>);
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("Login 页", () => {
  it("用 /stats 校验令牌 —— 与 dashboard 其余页面同一道闸门", async () => {
    const spy = vi.spyOn(api, "apiFetch").mockResolvedValue({});

    submit("api-token");

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/stats"));
    // 用 /config 校验会让只配了普通 api token 的部署完全登不进来。
    expect(spy).not.toHaveBeenCalledWith("/config");
  });

  it("成功后落盘令牌并跳转概览", async () => {
    vi.spyOn(api, "apiFetch").mockResolvedValue({});

    submit("good");

    await waitFor(() => expect(navigate).toHaveBeenCalledWith("/", { replace: true }));
    expect(localStorage.getItem(TOKEN_STORAGE_KEY)).toBe("good");
    expect(useAuthStore.getState().token).toBe("good");
  });

  it("回到 401 打断的那个页面,而不是总回概览", async () => {
    vi.spyOn(api, "apiFetch").mockResolvedValue({});
    sessionStorage.setItem(RETURN_TO_STORAGE_KEY, "/cron?x=1");

    submit("good");

    await waitFor(() => expect(navigate).toHaveBeenCalledWith("/cron?x=1", { replace: true }));
    expect(sessionStorage.getItem(RETURN_TO_STORAGE_KEY)).toBeNull();
  });

  it("拒绝站外跳转目标,退回概览", async () => {
    vi.spyOn(api, "apiFetch").mockResolvedValue({});
    sessionStorage.setItem(RETURN_TO_STORAGE_KEY, "//evil.example.com");

    submit("good");

    await waitFor(() => expect(navigate).toHaveBeenCalledWith("/", { replace: true }));
  });

  it("校验失败时回滚令牌,不让无效 token 残留在 localStorage", async () => {
    vi.spyOn(api, "apiFetch").mockRejectedValue(new Error("401"));

    submit("bad");

    expect(await screen.findByRole("alert")).toHaveTextContent("Token 无效或服务不可达");
    expect(localStorage.getItem(TOKEN_STORAGE_KEY)).toBeNull();
    expect(useAuthStore.getState().token).toBeNull();
    expect(navigate).not.toHaveBeenCalled();
  });

  it("校验失败时恢复原先的令牌", async () => {
    localStorage.setItem(TOKEN_STORAGE_KEY, "old");
    useAuthStore.setState({ token: "old" });
    vi.spyOn(api, "apiFetch").mockRejectedValue(new Error("401"));

    submit("bad");

    await waitFor(() => expect(localStorage.getItem(TOKEN_STORAGE_KEY)).toBe("old"));
    expect(useAuthStore.getState().token).toBe("old");
  });

  it("令牌输入框用 password 类型,不在屏上明文显示", () => {
    expect(screen.getByLabelText("Admin Token")).toHaveAttribute("type", "password");
  });
});
