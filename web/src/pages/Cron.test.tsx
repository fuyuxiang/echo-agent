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
    // 投递字段必须一起回填,否则保存时它们会被当成"用户清空了"而置空。
    expect(screen.getByLabelText(/投递渠道/)).toHaveValue("feishu");
    expect(screen.getByLabelText(/会话\/群 ID/)).toHaveValue("oc_1");
    expect(screen.getByLabelText(/source_session_key/)).toHaveValue("feishu:oc_1");
  });

  it("编辑不重发表单外的 payload 字段,交由后端合并保留", async () => {
    // 关键回归:PUT 曾整体替换 payload,把表单没有的字段全部丢掉,一次改名就顺带
    // 改掉任务的行为。现在前端只声明自己管的字段,其余留在服务端。
    // 注:payload 里的 unattended_authorized 早已不再有任何作用——授权是
    // ScheduledJob 的一等字段,只能由 authorize_unattended 这个显式标记签发;
    // 这里留着它只是当作一个"前端不该碰的既有键"来验证合并行为。
    const guarded = {
      ...JOB,
      payload: {
        ...JOB.payload,
        unattended_authorized: false,
        is_group: true,
        _inspection_tick: 3,
      },
    };
    const spy = vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/cron") return { jobs: [guarded] } as never;
      return {} as never;
    });
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: "编辑定时任务 每日汇报" }));
    fireEvent.change(screen.getByLabelText("任务名称"), { target: { value: "改个名" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/cron/j1", expect.anything()));
    const [, init] = spy.mock.calls.find(([p]) => p === "/cron/j1")!;
    const sent = JSON.parse((init as { body: string }).body);
    // 不重发 = 不会被前端的省略清掉(后端合并语义),也不会被前端改写。
    expect(sent.payload).not.toHaveProperty("unattended_authorized");
    expect(sent.payload).not.toHaveProperty("is_group");
    expect(sent.payload).not.toHaveProperty("_inspection_tick");
    expect(sent.name).toBe("改个名");
  });

  it("任务用 message 作指令键时保存仍用 message", async () => {
    // 后端两个键都接受,改写键名会让 fire-time 读到另一个槽位。
    const withMessage = {
      ...JOB,
      payload: { message: "旧指令", deliver_channel: "feishu" },
    };
    const spy = vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/cron") return { jobs: [withMessage] } as never;
      return {} as never;
    });
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: "编辑定时任务 每日汇报" }));
    expect(screen.getByLabelText(/任务内容/)).toHaveValue("旧指令");
    fireEvent.change(screen.getByLabelText(/任务内容/), { target: { value: "新指令" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/cron/j1", expect.anything()));
    const [, init] = spy.mock.calls.find(([p]) => p === "/cron/j1")!;
    const sent = JSON.parse((init as { body: string }).body);
    expect(sent.payload.message).toBe("新指令");
    expect(sent.payload).not.toHaveProperty("command");
  });

  it("任务用别名键存投递目标时能回填,且不写出重复的 deliver_* 键", async () => {
    // 后端 delivery 同时接受 channel / chat_id / session_key 这组别名。表单原来只读
    // deliver_* 主键:别名任务的投递目标既显示不出来,保存时还会在别名旁边多存三个
    // 空串主键——空串会盖掉别名值,等于一次改名就把投递目标弄丢。
    const aliased = {
      ...JOB,
      payload: { command: "汇总", channel: "feishu", chat_id: "oc_1", session_key: "feishu:oc_1" },
    };
    const spy = vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/cron") return { jobs: [aliased] } as never;
      return {} as never;
    });
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: "编辑定时任务 每日汇报" }));
    expect(screen.getByLabelText(/投递渠道/)).toHaveValue("feishu");
    expect(screen.getByLabelText(/会话\/群 ID/)).toHaveValue("oc_1");
    expect(screen.getByLabelText(/source_session_key/)).toHaveValue("feishu:oc_1");

    fireEvent.change(screen.getByLabelText("任务名称"), { target: { value: "改个名" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/cron/j1", expect.anything()));
    const [, init] = spy.mock.calls.find(([p]) => p === "/cron/j1")!;
    const sent = JSON.parse((init as { body: string }).body);
    // 写回原来的键,不产生第二份主键。
    expect(sent.payload.channel).toBe("feishu");
    expect(sent.payload.chat_id).toBe("oc_1");
    expect(sent.payload.session_key).toBe("feishu:oc_1");
    expect(sent.payload).not.toHaveProperty("deliver_channel");
    expect(sent.payload).not.toHaveProperty("deliver_chat_id");
    expect(sent.payload).not.toHaveProperty("source_session_key");
  });

  it("原本没有投递字段的任务,编辑时不会被塞进三个空串键", async () => {
    const bare = { ...JOB, payload: { command: "汇总" } };
    const spy = vi.spyOn(api, "apiFetch").mockImplementation(async (path: string) => {
      if (path === "/cron") return { jobs: [bare] } as never;
      return {} as never;
    });
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: "编辑定时任务 每日汇报" }));
    fireEvent.change(screen.getByLabelText("任务名称"), { target: { value: "改个名" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/cron/j1", expect.anything()));
    const [, init] = spy.mock.calls.find(([p]) => p === "/cron/j1")!;
    const sent = JSON.parse((init as { body: string }).body);
    expect(Object.keys(sent.payload)).toEqual(["command"]);
  });

  it("清空已有的投递字段仍然发空串,才能真的清掉", async () => {
    // 合并语义下省略等于沿用旧值,所以"用户手动清空"必须发空串。
    const spy = mockApi();
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: "编辑定时任务 每日汇报" }));
    fireEvent.change(screen.getByLabelText(/投递渠道/), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(spy).toHaveBeenCalledWith("/cron/j1", expect.anything()));
    const [, init] = spy.mock.calls.find(([p]) => p === "/cron/j1")!;
    const sent = JSON.parse((init as { body: string }).body);
    expect(sent.payload.deliver_channel).toBe("");
    expect(sent.payload.deliver_chat_id).toBe("oc_1");
  });

  it("新建任务时空的投递字段不写入 payload", async () => {
    const spy = mockApi();
    renderCron();

    fireEvent.click(await screen.findByRole("button", { name: /新建/ }));
    fireEvent.change(screen.getByLabelText("任务名称"), { target: { value: "新任务" } });
    fireEvent.change(screen.getByLabelText("cron 表达式"), { target: { value: "0 8 * * *" } });
    fireEvent.change(screen.getByLabelText(/任务内容/), { target: { value: "跑一下" } });
    fireEvent.click(screen.getByRole("button", { name: "创建" }));

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("/cron", expect.objectContaining({ method: "POST" })),
    );
    const call = spy.mock.calls.find(
      ([p, init]) => p === "/cron" && (init as { method?: string } | undefined)?.method === "POST",
    )!;
    const sent = JSON.parse((call[1] as { body: string }).body);
    expect(Object.keys(sent.payload)).toEqual(["command"]);
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
