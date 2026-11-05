import { expect, it } from "vitest";
import cases from "./group-intent-cases.json";
import { groupDiscussionOnly, groupRecipients, groupSourceAttachments, type Group, type GroupMessage } from "../group-client";

it.each(cases)("只读语义：$text", ({ text, discussion }) => {
  expect(groupDiscussionOnly(text)).toBe(discussion);
});

it("明确收件人优先于通知关键词，默认通知仍交给协调员", () => {
  const group = { coordinatorId: "c", members: [{ id: "c", name: "协调员", expert: null }, { id: "a", name: "专家A", expert: "A" }, { id: "b", name: "专家B", expert: "B" }], messages: [] } as unknown as Group;
  expect(groupRecipients(group, "@全体 更新：请立即生成本周的周报", [], null)).toEqual(["a", "b"]);
  expect(groupRecipients(group, "通知：交付日期改到周五", ["a", "b"], null)).toEqual(["a", "b"]);
  expect(groupRecipients(group, "通知：交付日期改到周五", [], null)).toEqual(["c"]);
});

it("继承附件去重，并保留前轮排除项，不会沿关联链重新引入", () => {
  const message = (id: string, attachments: string[], extra = {}): GroupMessage => ({ id, attachments, ...extra } as GroupMessage);
  const group = { messages: [message("first", ["/a", "/b"]), message("second", ["/c"], { sourceRequestId: "first", sourceAttachmentPaths: ["/b"] }), message("third", ["/d"], { sourceRequestId: "second", sourceAttachmentPaths: [] })] } as Group;
  expect(groupSourceAttachments(group, "second")).toEqual(["/c", "/b"]);
  expect(groupSourceAttachments(group, "third")).toEqual(["/d"]);
  expect(groupSourceAttachments({ ...group, messages: [message("first", ["/a"], { sourceRequestId: "second" }), message("second", ["/a", "/b"], { sourceRequestId: "first" })] }, "first")).toEqual(["/a", "/b"]);
});
