import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

const mocks = vi.hoisted(() => ({ install: vi.fn(), check: vi.fn() }));
vi.mock("@/lib/app-updater", async (importOriginal) => ({
  ...await importOriginal<typeof import("@/lib/app-updater")>(),
  installAppUpdate: mocks.install,
  checkAppUpdate: mocks.check,
}));

import { UpdateDialog } from "../UpdateDialog";
import { useUpdateStore } from "@/stores/update-store";

describe("UpdateDialog installation failures", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    useUpdateStore.setState({ status: "available", currentVersion: "0.3.11",
      update: { version: "0.3.12", mandatory: false }, downloaded: 0,
      total: undefined, error: undefined, errorStage: undefined });
  });

  it.each([
    ["check", "服务器版本已变更，请重新确认更新", "检查更新失败"],
    ["check", "更新服务返回 HTTP 410，暂时无法检查更新。请联系管理员检查 123.56.188.16:8787 的客户端更新发布路由，然后重试。", "检查更新失败"],
    ["download", "更新下载或签名校验失败", "下载更新失败"],
    ["install", "更新安装失败：目标不可写", "安装更新失败"],
  ])("点击安装后准确展示 %s 阶段错误，并允许重新检查", async (stage, message, heading) => {
    mocks.install.mockRejectedValueOnce({ stage, message });
    mocks.check.mockResolvedValueOnce({ currentVersion: "0.3.11", checkedAt: "2026-09-30T00:00:00Z" });
    render(<UpdateDialog open onClose={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "下载并安装" }));
    expect(await screen.findByRole("heading", { name: heading })).toBeInTheDocument();
    expect(screen.getByText(message)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "关闭更新窗口" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "重新检查" }));
    expect(await screen.findByRole("heading", { name: "已是最新版本" })).toBeInTheDocument();
  });
});
