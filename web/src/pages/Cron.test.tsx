import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { Cron } from "./Cron";
import { ConfirmProvider } from "../components/ConfirmDialog";
import * as api from "../lib/api";
import { useAuthStore } from "../stores/auth";

const JOB = {
  id: "j1",
  name: "每日汇报",
  cron_expr: "0 9 * * *",
  enabled: true,
  status: "active",
  last_status: "completed",
  next_run_ms: 1_700_000_000_000,
  payload: {
    command: "汇总今天的任务",
    deliver_channel: "feishu",
    deliver_chat_id: "oc_1",
    source_session_key: "feishu:oc_1",
  },
};

function mockApi(overrides: Record<string, unknown> = {}) {
  return vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
    if (path === "/cron") return { jobs: [JOB] } as never;
    if (path in overrides) return overrides[path] as never;
    return {} as never;
  });
}

function renderCron() {
  return render(
    <MemoryRouter>
      <ConfirmProvider>
        <Cron />
      </ConfirmProvider>
    </MemoryRouter>,
  );
}

beforeEach(() => {
  // useWsSubscribe 只在有 token 时才连接;默认不设,避免测试里开真实 socket。
  useAuthStore.setState({ token: "" });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("Cron 页", () => {
  it("渲染任务行与最近结果", async () => {
    mockApi();
    renderCron();

    expect(await screen.findByText("每日汇报")).toBeInTheDocument();
    expect(screen.getByText("0 9 * * *")).toBeInTheDocument();
    expect(screen.getByText("completed")).toBeInTheDocument();
  });

  it("编辑时用现有 payload 回填表单", async () => {
    mockApi();
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: "编辑定时任务 每日汇报" }));

    expect(screen.getByText("编辑定时任务")).toBeInTheDocument();
    expect(screen.getByLabelText("任务名称")).toHaveValue("每日汇报");
    expect(screen.getByLabelText("cron 表达式")).toHaveValue("0 9 * * *");
    expect(screen.getByLabelText(/任务内容/)).toHaveValue("汇总今天的任务");
    // PUT 会整体替换 payload,投递字段必须一起回填,否则保存即静默丢失。
    expect(screen.getByLabelText(/投递渠道/)).toHaveValue("feishu");
    expect(screen.getByLabelText(/会话\/群 ID/)).toHaveValue("oc_1");
    expect(screen.getByLabelText(/source_session_key/)).toHaveValue("feishu:oc_1");
  });

  it("编辑保存走 PUT 而不是新建", async () => {
    const spy = mockApi();
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: "编辑定时任务 每日汇报" }));
    fireEvent.change(screen.getByLabelText(/任务内容/), { target: { value: "换个指令" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("/cron/j1", {
        method: "PUT",
        body: JSON.stringify({
          name: "每日汇报",
          cron_expr: "0 9 * * *",
          payload: {
            command: "换个指令",
            deliver_channel: "feishu",
            deliver_chat_id: "oc_1",
            source_session_key: "feishu:oc_1",
          },
        }),
      }),
    );
    expect(spy).not.toHaveBeenCalledWith("/cron", expect.objectContaining({ method: "POST" }));
  });

  it("保存成功后关闭表单", async () => {
    mockApi();
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: "编辑定时任务 每日汇报" }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(screen.queryByText("编辑定时任务")).not.toBeInTheDocument());
  });

  it("取消编辑后重新点新建不会残留旧值", async () => {
    mockApi();
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: "编辑定时任务 每日汇报" }));
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    fireEvent.click(screen.getByRole("button", { name: /新建/ }));

    expect(screen.getByLabelText("任务名称")).toHaveValue("");
    expect(screen.getByLabelText(/任务内容/)).toHaveValue("");
    expect(screen.queryByText("编辑定时任务")).not.toBeInTheDocument();
  });

  it("状态列点击切换启用,只发 enabled 而不重建任务", async () => {
    const spy = mockApi();
    renderCron();

    fireEvent.click(await screen.findByRole("switch", { name: "停用定时任务 每日汇报" }));

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("/cron/j1", {
        method: "PUT",
        body: JSON.stringify({ enabled: false }),
      }),
    );
  });

  it("运行历史抽屉按 job 拉取 runs", async () => {
    const spy = mockApi({
      "/cron/j1/runs": {
        runs: [{ ts: "2026-07-27T09:00:00Z", status: "completed", error: "", run_count: 12 }],
      },
    });
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: "查看 每日汇报 的运行历史" }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/cron/j1/runs"));
    // 调度器只留最近一次汇总,抽屉必须把这个局限说清楚。
    expect(await screen.findByText(/只保留最近一次/)).toBeInTheDocument();
  });

  it("删除确认里提示可改用停用,避免被逼做破坏性操作", async () => {
    mockApi();
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: "删除" }));

    expect(await screen.findByText(/临时停止/)).toBeInTheDocument();
  });
});
