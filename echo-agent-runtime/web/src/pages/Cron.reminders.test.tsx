import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { Cron } from "./Cron";
import { ConfirmProvider } from "../components/ConfirmDialog";
import * as api from "../lib/api";
import { useAuthStore } from "../stores/auth";

afterEach(() => vi.restoreAllMocks());

function setup(job?: Record<string, unknown>) {
  useAuthStore.setState({ token: "" });
  const spy = vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) =>
    (path === "/cron" ? { jobs: job ? [job] : [] } : {}) as never);
  render(<MemoryRouter><ConfirmProvider><Cron /></ConfirmProvider></MemoryRouter>);
  return spy;
}

describe("单次附件提醒", () => {
  it("显示具体时间和附件，编辑保存不会转为年度重复", async () => {
    const at = Date.now() + 86400000;
    const spy = setup({ id: "once", name: "面试", trigger: "once", at_ms: at, cron_expr: "",
      status: "active", enabled: true, next_run_ms: at, last_status: "",
      payload: { command: "15:00面试，腾讯会议123", delivery_text: "15:00面试，腾讯会议123",
        delivery_mode: "reminder", attachment_ids: ["att_saved"], source_session_key: "weixin:user" } });
    expect(await screen.findByText("仅执行一次")).toBeInTheDocument();
    expect(screen.getByText("已绑定 1 份原始附件")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "编辑定时任务 面试" }));
    expect(screen.getByLabelText("执行方式")).toHaveValue("once");
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("/cron/once", expect.anything()));
    const request = JSON.parse((spy.mock.calls.find(([path]) => path === "/cron/once")![1] as { body: string }).body);
    expect(request).not.toHaveProperty("cron_expr");
    expect(request).not.toHaveProperty("at"); // unchanged time is preserved server-side
    expect(request.payload.delivery_text).toContain("腾讯会议123");
    expect(request.payload).not.toHaveProperty("attachment_ids");
  });

  it("创建直接提醒使用具体时间，不生成年度 cron", async () => {
    const spy = setup();
    fireEvent.click(await screen.findByRole("button", { name: /新建/ }));
    fireEvent.change(screen.getByLabelText("执行方式"), { target: { value: "once" } });
    fireEvent.change(screen.getByLabelText("提醒时间"), { target: { value: "2099-11-15T14:00:00" } });
    fireEvent.click(screen.getByLabelText("直接发送提醒内容（无需模型生成）"));
    fireEvent.change(screen.getByRole("textbox", { name: "提醒内容（届时原文发送）" }), { target: { value: "15:00面试" } });
    fireEvent.change(screen.getByLabelText(/source_session_key/), { target: { value: "weixin:user" } });
    fireEvent.click(screen.getByRole("button", { name: "创建" }));
    await waitFor(() => expect(spy.mock.calls.some(([path, init]) => path === "/cron" && init?.method === "POST")).toBe(true));
    const request = JSON.parse((spy.mock.calls.find(([path, init]) => path === "/cron" && init?.method === "POST")![1] as { body: string }).body);
    expect(request.trigger).toBe("once");
    expect(request).not.toHaveProperty("cron_expr");
    expect(request.payload.delivery_mode).toBe("reminder");
  });

  it("已触发但附件待补发时显示真实投递状态，并允许停止补发", async () => {
    const spy = setup({ id: "pending", name: "待补发面试", trigger: "once", at_ms: Date.now() - 60000,
      cron_expr: "", status: "completed", enabled: true, next_run_ms: null,
      last_status: "delivery_pending", delivery_status: "awaiting_context",
      payload: { command: "提醒", delivery_mode: "reminder", source_session_key: "weixin:user" } });
    expect(await screen.findByText("等待恢复会话")).toBeInTheDocument();
    expect(screen.getByText("等待补发（点击停止）")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("switch", { name: /待补发面试/ }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("/cron/pending", expect.objectContaining({
      method: "PUT", body: JSON.stringify({ enabled: false }),
    })));
  });

  it("完成的单次提醒可以改名，保留已经过去的执行时间", async () => {
    const spy = setup({ id: "past", name: "旧提醒", trigger: "once", at_ms: Date.now() - 86400000,
      cron_expr: "", status: "completed", enabled: true, next_run_ms: null, last_status: "completed",
      delivery_status: "delivered", payload: { command: "原文", delivery_text: "原文", delivery_mode: "reminder" } });
    expect(await screen.findByText("已触发，调度结束")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "编辑定时任务 旧提醒" }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("/cron/past", expect.anything()));
    const body = JSON.parse((spy.mock.calls.find(([path]) => path === "/cron/past")![1] as { body: string }).body);
    expect(body).not.toHaveProperty("at");
    expect(body).not.toHaveProperty("cron_expr");
  });


  it("按文件名解除提醒关联，并说明原件仍然保留", async () => {
    const spy = setup({ id: "files", name: "带文件提醒", trigger: "once", at_ms: Date.now() + 86400000,
      cron_expr: "", status: "active", enabled: true, next_run_ms: null, last_status: "",
      attachment_details: [{ id: "att_saved", name: "候选人简历.pdf", available: true }],
      payload: { command: "面试", delivery_mode: "reminder", attachment_ids: ["att_saved"] } });
    fireEvent.click(await screen.findByRole("button", { name: "编辑定时任务 带文件提醒" }));
    expect(screen.getByText(/取消勾选只解除本提醒/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("checkbox", { name: "候选人简历.pdf" }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("/cron/files", expect.anything()));
    const body = JSON.parse((spy.mock.calls.find(([path]) => path === "/cron/files")![1] as { body: string }).body);
    expect(body.payload.attachment_ids).toEqual([]);
    expect(body.payload.await_attachment).toBe(false);
  });

});
