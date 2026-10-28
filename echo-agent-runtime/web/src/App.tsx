import { lazy, Suspense, type ReactNode } from "react";
import { BrowserRouter, Link, Routes, Route } from "react-router";
import { useTranslation } from "react-i18next";
import { Layout } from "./components/Layout";
import { Toaster } from "./components/Toaster";
import { ConfirmProvider } from "./components/ConfirmDialog";

// Keep the authenticated shell eager, but load each page only when its route
// is visited. A named-export adapter keeps the page modules' public API intact
// while still giving React.lazy the default export shape it requires.
const Login = lazy(() => import("./pages/Login").then((m) => ({ default: m.Login })));
const Overview = lazy(() => import("./pages/Overview").then((m) => ({ default: m.Overview })));
const Kanban = lazy(() => import("./pages/Kanban").then((m) => ({ default: m.Kanban })));
const Sessions = lazy(() => import("./pages/Sessions").then((m) => ({ default: m.Sessions })));
const Memory = lazy(() => import("./pages/Memory").then((m) => ({ default: m.Memory })));
const Skills = lazy(() => import("./pages/Skills").then((m) => ({ default: m.Skills })));
const Knowledge = lazy(() => import("./pages/Knowledge").then((m) => ({ default: m.Knowledge })));
const Channels = lazy(() => import("./pages/Channels").then((m) => ({ default: m.Channels })));
const Cron = lazy(() => import("./pages/Cron").then((m) => ({ default: m.Cron })));
const Logs = lazy(() => import("./pages/Logs").then((m) => ({ default: m.Logs })));
const Config = lazy(() => import("./pages/Config").then((m) => ({ default: m.Config })));
const Analytics = lazy(() => import("./pages/Analytics").then((m) => ({ default: m.Analytics })));

function RouteLoading() {
  const { t } = useTranslation("common");
  return (
    <div role="status" aria-live="polite" className="text-gray-400 text-sm p-4">
      {t("loading")}
    </div>
  );
}

/** A boundary per route keeps the already-rendered Sidebar/Layout visible
 * while the next page chunk is fetched. */
function LazyRoute({ children }: { children: ReactNode }) {
  return <Suspense fallback={<RouteLoading />}>{children}</Suspense>;
}

function NotFound() {
  const { t } = useTranslation("common");
  return <div className="h-full grid place-items-center text-center">
    <div><div className="text-5xl font-bold text-gray-200">404</div>
      <p className="mt-2 text-gray-600">{t("notFound")}</p>
      <Link to="/" className="inline-block mt-3 text-sm text-blue-600 hover:underline">{t("backHome")}</Link></div>
  </div>;
}

export function App() {
  return (
    <BrowserRouter>
      <ConfirmProvider>
        <Toaster />
        <Routes>
          <Route path="/login" element={<LazyRoute><Login /></LazyRoute>} />
          <Route element={<Layout />}>
            <Route index element={<LazyRoute><Overview /></LazyRoute>} />
            <Route path="sessions" element={<LazyRoute><Sessions /></LazyRoute>} />
            <Route path="memory" element={<LazyRoute><Memory /></LazyRoute>} />
            <Route path="skills" element={<LazyRoute><Skills /></LazyRoute>} />
            <Route path="knowledge" element={<LazyRoute><Knowledge /></LazyRoute>} />
            <Route path="channels" element={<LazyRoute><Channels /></LazyRoute>} />
            <Route path="cron" element={<LazyRoute><Cron /></LazyRoute>} />
            <Route path="kanban" element={<LazyRoute><Kanban /></LazyRoute>} />
            <Route path="logs" element={<LazyRoute><Logs /></LazyRoute>} />
            <Route path="config" element={<LazyRoute><Config /></LazyRoute>} />
            <Route path="analytics" element={<LazyRoute><Analytics /></LazyRoute>} />
            <Route path="*" element={<NotFound />} />
          </Route>
        </Routes>
      </ConfirmProvider>
    </BrowserRouter>
  );
}
