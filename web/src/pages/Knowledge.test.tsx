import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { Knowledge } from "./Knowledge";
import { ConfirmProvider } from "../components/ConfirmDialog";
import * as api from "../lib/api";
import { useCapabilitiesStore } from "../stores/capabilities";

const DOCS = {
  documents: [
    { path: "guide.md", size: 2048, modified: 0 },
    { path: "sub dir/nested doc.md", size: 1024, modified: 0 },
  ],
};

const STATUS = { documents: 2, chunks: 17, stale: true, last_rebuild: null };

function mockApi(admin: boolean) {
  return vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
    if (path === "/capabilities") return { admin } as never;
    if (path === "/knowledge/documents") return DOCS as never;
    if (path === "/knowledge/status") return STATUS as never;
    return {} as never;
  });
}

function renderKnowledge() {
  return render(
    <ConfirmProvider>
      <Knowledge />
    </ConfirmProvider>,
  );
}

beforeEach(() => {
  useCapabilitiesStore.getState().reset();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("Knowledge 页", () => {
  it("状态行读的是后端真实字段,不再恒为 0", async () => {
    mockApi(true);
    renderKnowledge();

    expect(await screen.findByText(/已索引 2 篇 · 17 个片段/)).toBeInTheDocument();
    expect(screen.getByText(/最后重建: 从未/)).toBeInTheDocument();
    expect(screen.getByText(/索引已过期/)).toBeInTheDocument();
  });

  it("非 admin 令牌下禁用上传与删除,但重建仍可用", async () => {
    mockApi(false);
    renderKnowledge();

    await waitFor(() => expect(screen.getByText("guide.md")).toBeInTheDocument());
    expect(screen.getByRole("button", { name: /上传文档/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "删除文档 guide.md" })).toBeDisabled();
    // rebuild 不走 _admin_guard,不该被一起禁掉。
    expect(screen.getByRole("button", { name: /重建索引/ })).toBeEnabled();
    expect(screen.getAllByText(/需要管理员令牌/).length).toBeGreaterThan(0);
  });

  it("admin 令牌下上传与删除可用", async () => {
    mockApi(true);
    renderKnowledge();

    await waitFor(() => expect(screen.getByText("guide.md")).toBeInTheDocument());
    expect(screen.getByRole("button", { name: /上传文档/ })).toBeEnabled();
    expect(screen.getByRole("button", { name: "删除文档 guide.md" })).toBeEnabled();
  });

  it("删除确认里说明会重建整库,取消则不发请求", async () => {
    const spy = mockApi(true);
    renderKnowledge();

    fireEvent.click(await screen.findByRole("button", { name: "删除文档 guide.md" }));
    expect(await screen.findByText(/重建整个知识库索引/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    await waitFor(() =>
      expect(spy).not.toHaveBeenCalledWith(
        "/knowledge/documents/guide.md",
        expect.anything(),
      ),
    );
  });

  it("嵌套路径逐段编码,保留真实斜杠", async () => {
    const spy = mockApi(true);
    renderKnowledge();

    fireEvent.click(
      await screen.findByRole("button", { name: "删除文档 sub dir/nested doc.md" }),
    );
    fireEvent.click(await screen.findByRole("button", { name: "删除" }));

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith(
        "/knowledge/documents/sub%20dir/nested%20doc.md",
        { method: "DELETE" },
      ),
    );
  });

  it("删除成功后同时刷新文档列表与状态行", async () => {
    const spy = mockApi(true);
    renderKnowledge();

    fireEvent.click(await screen.findByRole("button", { name: "删除文档 guide.md" }));
    fireEvent.click(await screen.findByRole("button", { name: "删除" }));

    await waitFor(() => {
      const calls = spy.mock.calls.filter((c) => c[0] === "/knowledge/status");
      expect(calls.length).toBeGreaterThan(1);
    });
  });
});
