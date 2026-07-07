import { BrowserRouter, Routes, Route } from "react-router-dom";
import { Layout } from "./components/Layout";
import { Login } from "./pages/Login";
import { Overview } from "./pages/Overview";
import { Kanban } from "./pages/Kanban";
import { Sessions } from "./pages/Sessions";
import { Memory } from "./pages/Memory";
import { Skills } from "./pages/Skills";
import { Knowledge } from "./pages/Knowledge";
import { Channels } from "./pages/Channels";

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
          <Route path="memory" element={<Memory />} />
          <Route path="skills" element={<Skills />} />
          <Route path="knowledge" element={<Knowledge />} />
          <Route path="channels" element={<Channels />} />
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
