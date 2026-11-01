import React from "react";
import { createRoot } from "react-dom/client";
import { Button } from "../../src/components/Button";
import { ThemeProvider, useTheme, THEME_STORAGE_KEY } from "../../src/components/ThemeProvider";
import "../../src/styles/index.css";

const query=new URLSearchParams(location.search);
// The review uses scripted controls and must never collect user keystrokes.
document.addEventListener("keydown",event=>event.preventDefault(),true);
localStorage.setItem(THEME_STORAGE_KEY,query.get("theme") || "light");
document.documentElement.style.fontSize=`${16*Number(query.get("font") || 13)/13}px`;
// Isolated native paint samples use the production settings scope and controls.
// Frame geometry is review-only; colors come entirely from production styles.
function NativeColorReview() {
  const {toggle}=useTheme();
  return <main className="app" style={{height:"100%",overflow:"auto",padding:24}}>
    <section className="settings-modal" style={{position:"static",width:"100%",height:"auto",maxHeight:"none",display:"block",padding:24}}>
      <h1 style={{fontSize:"1.25rem",margin:"0 0 20px"}}>原生控件与主题</h1>
      <div style={{display:"flex",gap:20,flexWrap:"wrap",marginBottom:20}}>
        <label><input id="native-check" type="checkbox" defaultChecked /> 接收任务通知</label>
        <label><input type="checkbox" disabled defaultChecked /> 暂不可用</label>
        <label><input type="radio" name="mode" defaultChecked /> 标准模式</label>
        <label><input type="radio" name="mode" /> 详细模式</label>
      </div>
      <div style={{display:"grid",gap:16,marginBottom:20}}>
        <input className="form-control" placeholder="输入名称" aria-label="名称" />
        <select className="form-control" aria-label="工作区"><option>团队共享文档与项目交付工作区</option></select>
        <input className="form-control" aria-label="只读名称" value="已保存的工作区" readOnly />
        <input className="form-control" aria-label="错误名称" defaultValue="需要修正的名称" aria-invalid="true" />
      </div>
      <div style={{display:"flex",gap:12,flexWrap:"wrap"}}>
        <Button variant="primary">保存设置</Button><Button variant="danger">删除记录</Button>
        <Button id="switch-theme" onClick={toggle}>切换主题</Button><Button disabled>不可用</Button>
      </div>
    </section>
  </main>;
}
createRoot(document.getElementById("root")!).render(<ThemeProvider><NativeColorReview /></ThemeProvider>);
