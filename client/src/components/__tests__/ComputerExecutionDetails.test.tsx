import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ExecutionProcess } from "../ExecutionProcess";
import { ThemeProvider } from "../ThemeProvider";
import { computerTaskRunning, computerTaskLabel } from "@/lib/computer-task";
import type { AutomationStatus } from "@/lib/automation-client";

const status: AutomationStatus = {
  sessionId:"a", mode:"computer_use", paused:false, allowPrivateNetwork:false,
  browser:{available:true}, computer:{available:true,platform:"macOS",screenCapture:true,inputControl:true},
  browserRunning:false, browserHasData:false,
  task:{phase:"running",reason:null,scope:"整理文稿",authorized:true,generation:1,promptId:"turn-a"},
};
describe("应用操作复用消息过程", () => {
  it("详情随现有过程展开，只有记录没有重复控制按钮", () => {
    render(<ThemeProvider><ExecutionProcess sessionId="a" messageId="message-a" parts={[]} active automationStatus={status}/></ThemeProvider>);
    expect(screen.queryByLabelText("应用操作情况")).toBeNull();
    fireEvent.click(screen.getByRole("button",{name:/展开过程/}));
    expect(screen.getByLabelText("应用操作情况")).toBeVisible();
    for (const label of ["暂停并接管","停止","继续任务","悬浮"]) expect(screen.queryByRole("button",{name:label})).toBeNull();
    fireEvent.click(screen.getAllByRole("button",{name:/收起过程/})[0]);
    expect(screen.queryByLabelText("应用操作情况")).toBeNull();
  });
  it("完成或撤销执行权后不再显示正在运行的停止状态", () => {
    expect(computerTaskRunning(status)).toBe(true);
    expect(computerTaskRunning({...status,task:{...status.task!,phase:"verifying"}})).toBe(true);
    expect(computerTaskRunning({...status,task:{...status.task!,phase:"result_unknown",authorized:false}})).toBe(false);
    expect(computerTaskRunning({...status,paused:true,task:{...status.task!,phase:"stopped"}})).toBe(false);
    expect(computerTaskRunning({...status,task:{...status.task!,phase:"completed"}})).toBe(false);
  });
  it("缺失消息正文和输入争用有明确说明，仍通过普通对话续接", () => {
    const waiting={...status,activeWindow:{application:"自绘应用"},task:{...status.task!,phase:"waiting_user" as const,authorized:false,reason:"message_text_required",outcome:"请告诉我需要发送的消息内容。"}};
    expect(computerTaskLabel(waiting)).toBe("等待消息内容");
    expect(computerTaskRunning(waiting)).toBe(false);
    render(<ThemeProvider><ExecutionProcess sessionId="a" messageId="waiting" parts={[]} active={false} automationStatus={waiting}/></ThemeProvider>);
    fireEvent.click(screen.getByRole("button",{name:/查看过程/}));
    expect(screen.getByText("请告诉我需要发送的消息内容。")).toBeVisible();
    expect(screen.getByText("当前应用：自绘应用")).toBeVisible();
    expect(screen.queryByRole("button",{name:"继续任务"})).toBeNull();
    expect(computerTaskLabel({...waiting,task:{...waiting.task,reason:"input_conflict"}})).toBe("等待你继续");
  });

});
