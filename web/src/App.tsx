import { BrowserRouter, Routes, Route } from "react-router-dom";
import { Layout } from "./components/Layout";
import { Login } from "./pages/Login";
import { Overview } from "./pages/Overview";
import { Kanban } from "./pages/Kanban";
import { Sessions } from "./pages/Sessions";

function Placeholder({ name }: { name: string }) {
  return <div className="text-xl font-bold">{name}</div>;
}

export function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/login" element={<Login />} />
        <Route element={<Layout />}>
          <Route index element={<Overview />} />
          <Route path="sessions" element={<Sessions />} />
          <Route path="memory" element={<Placeholder name="记忆管理" />} />
          <Route path="skills" element={<Placeholder name="技能管理" />} />
          <Route path="knowledge" element={<Placeholder name="知识库" />} />
          <Route path="channels" element={<Placeholder name="通道管理" />} />
          <Route path="cron" element={<Placeholder name="定时任务" />} />
          <Route path="kanban" element={<Kanban />} />
          <Route path="logs" element={<Placeholder name="日志" />} />
          <Route path="config" element={<Placeholder name="配置" />} />
          <Route path="analytics" element={<Placeholder name="统计" />} />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}
