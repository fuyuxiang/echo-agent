import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

vi.mock("@/lib/agent-client", () => ({
  agentListPendingInteractions: vi.fn(),
  folderTrustRespond: vi.fn(),
}));

import {
  agentListPendingInteractions,
  folderTrustRespond,
  type FolderTrustRequest,
} from "@/lib/agent-client";
import { FolderTrustDialog } from "../FolderTrustDialog";

const first: FolderTrustRequest = {
  requestId: "trust-1",
  sessionId: "session-1",
  cwd: "/work/one",
  workspace: "/work/one",
  configKinds: ["MCP"],
};
const second: FolderTrustRequest = {
  requestId: "trust-2",
  sessionId: "session-2",
  cwd: "/work/two",
  workspace: "/work/two",
  configKinds: ["hooks"],
};

const pending = (folderTrustRequests: FolderTrustRequest[]) => ({
  permissions: [],
  questions: [],
  planApprovals: [],
  folderTrustRequests,
});

beforeEach(() => {
  vi.mocked(agentListPendingInteractions).mockReset();
  vi.mocked(folderTrustRespond).mockReset().mockResolvedValue(true);
});

describe("FolderTrustDialog", () => {
  it("读取待决队列失败时显示明确错误并可重试", async () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    vi.mocked(agentListPendingInteractions)
      .mockRejectedValueOnce(new Error("通道断开"))
      .mockResolvedValueOnce(pending([first]));
    const onResolve = vi.fn();
    render(<FolderTrustDialog request={{ cwd: first.cwd }} onResolve={onResolve} />);

    expect(await screen.findByRole("alert")).toHaveTextContent("读取待处理信任请求失败：通道断开");
    const retry = screen.getByRole("button", { name: "重试" });
    expect(retry).toHaveFocus();
    expect(screen.getByRole("alertdialog")).toHaveAttribute("aria-busy", "false");
    fireEvent.click(retry);

    expect(await screen.findByText("/work/one")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "不信任" })).toHaveFocus();
    expect(onResolve).not.toHaveBeenCalled();
    expect(errorSpy).toHaveBeenCalledWith(
      "list pending folder trust requests failed",
      expect.objectContaining({ message: "通道断开" }),
    );
    errorSpy.mockRestore();
  });

  it("按后端队列逐项消费，仅在全部处理后解除父级提示", async () => {
    vi.mocked(agentListPendingInteractions)
      .mockResolvedValueOnce(pending([first, second]))
      .mockResolvedValueOnce(pending([second]))
      .mockResolvedValueOnce(pending([]));
    const onResolve = vi.fn();
    render(<FolderTrustDialog request={{ cwd: first.cwd }} onResolve={onResolve} />);

    expect(await screen.findByText("/work/one")).toBeInTheDocument();
    expect(screen.getByText("还有 1 个信任请求等待处理")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "不信任" })).toHaveFocus();

    fireEvent.click(screen.getByRole("button", { name: "不信任" }));
    expect(await screen.findByText("/work/two")).toBeInTheDocument();
    expect(onResolve).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: /信任并加载/ }));
    await waitFor(() => expect(onResolve).toHaveBeenCalledTimes(1));
    expect(folderTrustRespond).toHaveBeenNthCalledWith(1, "trust-1", false);
    expect(folderTrustRespond).toHaveBeenNthCalledWith(2, "trust-2", true);
  });

  it("Escape 以不信任方式安全处理当前请求", async () => {
    vi.mocked(agentListPendingInteractions)
      .mockResolvedValueOnce(pending([first]))
      .mockResolvedValueOnce(pending([]));
    render(<FolderTrustDialog request={{ cwd: first.cwd }} onResolve={vi.fn()} />);
    await screen.findByText("/work/one");

    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(folderTrustRespond).toHaveBeenCalledWith("trust-1", false));
  });

  it("空队列隐藏后卸载焦点圈定，不吞掉全局 Tab", async () => {
    vi.mocked(agentListPendingInteractions).mockResolvedValueOnce(pending([]));
    const onResolve = vi.fn();
    render(<FolderTrustDialog request={{ cwd: "/work/none" }} onResolve={onResolve} />);

    await waitFor(() => expect(onResolve).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("alertdialog", { name: "文件夹信任" })).not.toBeInTheDocument();
    const tab = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
    document.dispatchEvent(tab);
    expect(tab.defaultPrevented).toBe(false);
  });

  it("无提示的后台查询不闪现弹窗，后续请求出现时重新安装焦点圈定", async () => {
    vi.mocked(agentListPendingInteractions)
      .mockResolvedValueOnce(pending([]))
      .mockResolvedValueOnce(pending([first]));
    const { rerender } = render(<FolderTrustDialog request={null} onResolve={vi.fn()} />);
    await waitFor(() => expect(agentListPendingInteractions).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("alertdialog", { name: "文件夹信任" })).not.toBeInTheDocument();

    rerender(<FolderTrustDialog request={{ cwd: first.cwd }} onResolve={vi.fn()} />);
    expect(await screen.findByText("/work/one")).toBeInTheDocument();
    const deny = screen.getByRole("button", { name: "不信任" });
    const trust = screen.getByRole("button", { name: /信任并加载/ });
    expect(deny).toHaveFocus();
    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(trust).toHaveFocus();
  });
});
