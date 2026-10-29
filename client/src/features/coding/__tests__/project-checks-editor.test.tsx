import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { ProjectChecksEditor } from "../panels/ProjectChecksEditor";
const api=vi.hoisted(()=>({verificationConfig:vi.fn(),saveVerificationConfig:vi.fn()}));
vi.mock("../lib/tauri-api",()=>({codingApi:api}));
beforeEach(()=>{
  api.verificationConfig.mockResolvedValue({checks:[{id:"check",label:"Test",kind:"test",command:"test",cwd:".",env:{FOO:"1"},autoRun:false}]});
  api.saveVerificationConfig.mockReset().mockResolvedValue({});
});
it("preserves Enter and incomplete environment drafts until save",async()=>{
  render(<ProjectChecksEditor root="/repo" onSaved={()=>{}}/>);
  const env=await screen.findByLabelText(/非敏感环境覆盖/);
  fireEvent.change(env,{target:{value:"FOO=1\n"}});
  expect(env).toHaveValue("FOO=1\n");
  fireEvent.change(env,{target:{value:"FOO=1\nSECOND"}});
  expect(env).toHaveValue("FOO=1\nSECOND");
  fireEvent.click(screen.getByRole("button",{name:"保存配置"}));
  expect(await screen.findByRole("alert")).toHaveTextContent("第 2 行");
  expect(screen.queryByRole("button",{name:"重试读取"})).not.toBeInTheDocument();
  expect(api.saveVerificationConfig).not.toHaveBeenCalled();
  fireEvent.change(env,{target:{value:"FOO=1\nSECOND=a=b"}});
  fireEvent.change(screen.getByLabelText("超时（秒）"),{target:{value:"3600"}});
  fireEvent.click(screen.getByRole("button",{name:"保存配置"}));
  await waitFor(()=>expect(api.saveVerificationConfig).toHaveBeenCalledWith("/repo",{checks:[expect.objectContaining({env:{FOO:"1",SECOND:"a=b"},timeoutSecs:3600})]}));
  expect(await screen.findByRole("status")).toHaveTextContent("检查配置已保存");
});
it("does not discard a valid legacy check with an unspecified timeout",async()=>{
  render(<ProjectChecksEditor root="/repo" onSaved={()=>{}}/>);
  await screen.findByLabelText(/非敏感环境覆盖/);
  fireEvent.click(screen.getByRole("button",{name:"保存配置"}));
  await waitFor(()=>expect(api.saveVerificationConfig).toHaveBeenCalledWith("/repo",{checks:[expect.objectContaining({timeoutSecs:300})]}));
});
it("rejects duplicate environment keys without overwriting the draft",async()=>{
  render(<ProjectChecksEditor root="/repo" onSaved={()=>{}}/>);
  const env=await screen.findByLabelText(/非敏感环境覆盖/);
  fireEvent.change(env,{target:{value:"FOO=1\nFOO=2"}});
  fireEvent.click(screen.getByRole("button",{name:"保存配置"}));
  expect(await screen.findByRole("alert")).toHaveTextContent("重复");
  expect(env).toHaveValue("FOO=1\nFOO=2");
  expect(api.saveVerificationConfig).not.toHaveBeenCalled();
});
