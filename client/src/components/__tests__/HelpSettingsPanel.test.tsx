// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { HelpSettingsPanel } from "../SettingsSections";
import { useUpdateStore, type UpdateStatus } from "@/stores/update-store";
import { APP_VERSION } from "@/lib/app-version";

const actions = vi.hoisted(() => ({ check: vi.fn(), install: vi.fn(), open: vi.fn() }));
vi.mock("@/lib/agent-client", async original => ({
  ...await original<typeof import("@/lib/agent-client")>(),
  openExternalUrl: actions.open,
}));

beforeEach(() => {
  vi.resetAllMocks();
  actions.open.mockResolvedValue(undefined);
  useUpdateStore.setState({ status: "up-to-date", update: undefined, downloaded: 0, total: undefined,
    checkedAt: new Date(2026, 10, 7, 17, 29, 25).toISOString(), error: undefined, errorStage: undefined,
    check: actions.check, install: actions.install });
});

describe("帮助与更新", () => {
  it("集中展示产品、版本和检查状态，低频资料默认折叠", () => {
    render(<HelpSettingsPanel />);
    expect(screen.getByRole("heading", { name: "EchoAgent" })).toBeInTheDocument();
    expect(screen.getByText(`v${APP_VERSION}`)).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("当前已是最新版本");
    expect(screen.getByText(/17:29/)).not.toHaveTextContent("17:29:25");
    expect(screen.queryByText(/VPN/)).toBeNull();
    expect(screen.queryByText("帮助资源")).toBeNull();
    expect(screen.getByText("确认模型配置")).not.toBeVisible();
    expect(screen.getByRole("link", { name: /ACP 协议规范/, hidden: true })).not.toBeVisible();
    expect(actions.check).not.toHaveBeenCalled();
    expect(actions.install).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "检查更新" }));
    expect(actions.check).toHaveBeenCalledExactlyOnceWith(true);
  });

  it("下载并安装仍调用原有安装操作，更新说明可以按需展开", () => {
    useUpdateStore.setState({ status: "available", update: { version: "0.3.12", mandatory: false, notes: "修复已知问题\n改进启动速度" } });
    render(<HelpSettingsPanel />);
    expect(screen.getByRole("status")).toHaveTextContent("发现新版本 v0.3.12");
    expect(screen.getByText(/修复已知问题/)).not.toBeVisible();
    fireEvent.click(screen.getByText("更新内容"));
    expect(screen.getByText(/修复已知问题/)).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "下载并安装" }));
    expect(actions.install).toHaveBeenCalledExactlyOnceWith();
    expect(actions.check).not.toHaveBeenCalled();
  });

  it.each([
    ["checking", "检查中"], ["downloading", "下载中"], ["installing", "安装中"],
  ] as [UpdateStatus, string][])("%s 阶段禁用重复操作并准确表达进度", (status, label) => {
    useUpdateStore.setState({ status, update: { version: "0.3.12", mandatory: false }, downloaded: 40, total: 100 });
    render(<HelpSettingsPanel />);
    const button = screen.getByRole("button", { name: label });
    expect(button).toBeDisabled();
    fireEvent.click(button);
    expect(actions.check).not.toHaveBeenCalled();
    expect(actions.install).not.toHaveBeenCalled();
    if (status === "downloading") expect(screen.getByRole("progressbar", { name: "更新进度" })).toHaveAttribute("aria-valuenow", "40");
    if (status === "installing") expect(screen.getByRole("progressbar")).not.toHaveAttribute("aria-valuenow");
    if (status === "checking") expect(screen.queryByRole("progressbar")).toBeNull();
  });

  it("未知下载大小时不显示虚假的完成百分比", () => {
    useUpdateStore.setState({ status: "downloading", update: { version: "0.3.12", mandatory: false }, downloaded: 40 });
    render(<HelpSettingsPanel />);
    expect(screen.getByRole("progressbar")).not.toHaveAttribute("aria-valuenow");
    expect(screen.getByRole("status")).not.toHaveTextContent("%");
  });

  it.each([
    ["check", "检查更新失败"], ["download", "下载更新失败"], ["install", "安装更新失败"],
  ] as const)("%s 失败时保留具体错误，重试仍通过检查更新流程", (errorStage, heading) => {
    useUpdateStore.setState({ status: "error", errorStage, error: "用于排查的完整错误信息" });
    render(<HelpSettingsPanel />);
    expect(screen.getByRole("alert")).toHaveTextContent(heading);
    expect(screen.getByText("用于排查的完整错误信息")).not.toBeVisible();
    expect(Boolean(screen.queryByText(/企业内网或 VPN/))).toBe(errorStage === "check");
    fireEvent.click(screen.getByText("查看详情"));
    expect(screen.getByText("用于排查的完整错误信息")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "重新检查" }));
    expect(actions.check).toHaveBeenCalledExactlyOnceWith(true);
    expect(actions.install).not.toHaveBeenCalled();
  });

  it("启动检查失败仍保持安静，不将后台错误变成主动告警", () => {
    useUpdateStore.setState({ status: "idle", checkedAt: undefined, error: "VPN unavailable", errorStage: "check" });
    render(<HelpSettingsPanel />);
    expect(screen.getByRole("status")).toHaveTextContent("启动时自动检查更新");
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByText(/VPN/)).toBeNull();
  });

  it("文档与技术资料沿用系统浏览器打开，失败时显示可重试提示", async () => {
    render(<HelpSettingsPanel />);
    actions.open.mockRejectedValueOnce(new Error("暂时无法打开浏览器"));
    fireEvent.click(screen.getByRole("link", { name: /使用文档/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent("暂时无法打开浏览器");
    expect(actions.open).toHaveBeenLastCalledWith("https://fuyuxiang.github.io/echo-agent/");
    fireEvent.click(screen.getByRole("link", { name: /使用文档/ }));
    expect(screen.queryByRole("alert")).toBeNull();
    fireEvent.click(screen.getByText("问题排查"));
    expect(screen.getByText("确认模型配置")).toBeVisible();
    expect(screen.getByRole("link", { name: /ACP 协议规范/, hidden: true })).not.toBeVisible();
    fireEvent.click(screen.getByText("技术资料"));
    fireEvent.click(screen.getByRole("link", { name: /ACP 协议规范/ }));
    expect(actions.open).toHaveBeenLastCalledWith("https://agentclientprotocol.com/");
  });
});
