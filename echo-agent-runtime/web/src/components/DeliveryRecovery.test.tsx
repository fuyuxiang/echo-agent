import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { DeliveryRecovery } from "./DeliveryRecovery";
import { ConfirmProvider } from "./ConfirmDialog";
import * as api from "../lib/api";

const delivery = { event_id: "reply:one", channel: "dingtalk", chat_id: "room", status: "unknown",
  preview: "报告正文", attachments: 1, confirmed_parts: 2, unknown_parts: 1 };

function setup(message = delivery) {
  let recovered = false;
  const fetch = vi.spyOn(api, "apiFetch").mockImplementation(async (_path, options) => {
    if (options?.method === "POST") { recovered = true; return { success: true } as never; }
    return { durable: true, messages: recovered ? [] : [message] } as never;
  });
  render(<ConfirmProvider><DeliveryRecovery /></ConfirmProvider>);
  return fetch;
}

afterEach(() => vi.restoreAllMocks());

describe("投递恢复", () => {
  it("未知发送必须先核对，取消核对不会重发", async () => {
    const fetch = setup();
    fireEvent.click(await screen.findByRole("button", { name: "继续发送未送达的内容" }));
    expect(screen.getByRole("dialog")).toHaveTextContent("避免接收人收到重复内容");
    expect(fetch.mock.calls.some(([, options]) => options?.method === "POST")).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(fetch.mock.calls.some(([, options]) => options?.method === "POST")).toBe(false);
  });

  it("确认未送达后只提交一次恢复，完成后刷新列表", async () => {
    const fetch = setup();
    fireEvent.click(await screen.findByRole("button", { name: "继续发送未送达的内容" }));
    fireEvent.click(screen.getByRole("button", { name: "已核对未送达，继续发送" }));
    await screen.findByText("暂无需要处理的投递");
    const posts = fetch.mock.calls.filter(([, options]) => options?.method === "POST");
    expect(posts).toHaveLength(1);
    expect(posts[0][0]).toBe("/deliveries/reply%3Aone/recover");
    expect(JSON.parse(String(posts[0][1]?.body))).toMatchObject({ action: "retry", confirmed_not_delivered: true });
  });

  it("确认完整收到时只登记结果，不请求重发", async () => {
    const fetch = setup();
    fireEvent.click(await screen.findByRole("button", { name: "已核对收到完整消息" }));
    fireEvent.click(screen.getByRole("button", { name: "已核对完整收到" }));
    await waitFor(() => expect(fetch.mock.calls.some(([, options]) => options?.method === "POST")).toBe(true));
    const post = fetch.mock.calls.find(([, options]) => options?.method === "POST")!;
    expect(JSON.parse(String(post[1]?.body))).toMatchObject({ action: "confirm_delivery", confirmed_delivered: true });
  });

  it("发送中的记录不能触发重试", async () => {
    setup({ ...delivery, status: "retrying" });
    expect(await screen.findByRole("button", { name: "继续发送未送达的内容" })).toBeDisabled();
  });
});
