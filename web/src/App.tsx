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
import { Cron } from "./pages/Cron";
import { Logs } from "./pages/Logs";
import { Config } from "./pages/Config";
import { Analytics } from "./pages/Analytics";

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
          <Route path="cron" element={<Cron />} />
          <Route path="kanban" element={<Kanban />} />
          <Route path="logs" element={<Logs />} />
          <Route path="config" element={<Config />} />
          <Route path="analytics" element={<Analytics />} />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}
