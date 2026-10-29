import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { ExternalToolRecovery } from "../panels/ExternalToolRecovery";
const api=vi.hoisted(()=>({externalToolsPending:vi.fn(),recoverExternalTool:vi.fn()}));
vi.mock("../lib/tauri-api",()=>({codingApi:api}));
beforeEach(()=>{api.externalToolsPending.mockReset().mockResolvedValue([{operationId:"operation",sessionId:"session",runId:"run",toolName:"external",localInflight:false}]);api.recoverExternalTool.mockReset().mockResolvedValue(undefined);});
it("requires a concrete audit note and explicit confirmation before recovery",async()=>{
  const onRecovered=vi.fn();
  render(<ExternalToolRecovery root="/repo" onRecovered={onRecovered}/>);
  const note=await screen.findByLabelText("核验依据 operation");
  const button=screen.getByRole("button",{name:"记录核验并恢复工程"});
  expect(button).toBeDisabled();
  fireEvent.change(note,{target:{value:"已在对应服务确认调用结束，并核对本地修改。"}});
  expect(button).toBeDisabled();
  fireEvent.click(screen.getByRole("checkbox"));
  fireEvent.click(button);
  await waitFor(()=>expect(api.recoverExternalTool).toHaveBeenCalledWith("/repo","operation","已在对应服务确认调用结束，并核对本地修改。"));
  expect(onRecovered).toHaveBeenCalledOnce();
});
it("does not offer recovery while the actual local RPC is alive",async()=>{
  api.externalToolsPending.mockResolvedValue([{operationId:"operation",runId:"run",toolName:"external",localInflight:true}]);
  render(<ExternalToolRecovery root="/repo" onRecovered={()=>{}}/>);
  expect(await screen.findByRole("status")).toHaveTextContent("本地调用仍在等待结果");
  expect(screen.queryByRole("button",{name:"记录核验并恢复工程"})).not.toBeInTheDocument();
});
