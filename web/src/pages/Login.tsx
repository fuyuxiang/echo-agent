import { useState } from "react";
import { useNavigate } from "react-router";
import { useTranslation } from "react-i18next";
import { useAuthStore } from "../stores/auth";
import { apiFetch, TOKEN_STORAGE_KEY } from "../lib/api";

export function Login() {
  const { t } = useTranslation("login");
  const [token, setToken] = useState("");
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const navigate = useNavigate();
  const setStoreToken = useAuthStore((s) => s.setToken);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    // apiFetch 从 localStorage 读 token,所以必须先落盘再校验;失败再回滚,
    // 否则无效 token 也会残留。/health 无需鉴权,验不出 token。这里用 /stats:
    // 它走 _require_api_token,和 dashboard 其余页面(sessions/tasks/cron/logs/
    // skills/channels/analytics)是同一道闸门。此前用的 /config 走 admin 守卫,
    // 一旦部署配了独立 admin_tokens,普通 api token 会被判为无效而完全登不进来,
    // 尽管它对整个 dashboard 都是有效的。
    const prev = localStorage.getItem(TOKEN_STORAGE_KEY);
    setStoreToken(token);
    setSubmitting(true);
    try {
      await apiFetch("/stats");
      navigate("/", { replace: true });
    } catch {
      if (prev !== null) {
        localStorage.setItem(TOKEN_STORAGE_KEY, prev);
      } else {
        localStorage.removeItem(TOKEN_STORAGE_KEY);
      }
      useAuthStore.setState({ token: prev });
      setError(t("invalidToken"));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="flex items-center justify-center h-screen bg-gray-50">
      <form onSubmit={handleSubmit} className="bg-white p-8 rounded-lg shadow-md w-96">
        <h1 className="text-xl font-bold mb-4">{t("title")}</h1>
        <label htmlFor="echo-token" className="block text-sm text-gray-600 mb-1">
          {t("tokenLabel")}
        </label>
        <input
          id="echo-token"
          name="token"
          type="password"
          autoComplete="current-password"
          value={token}
          onChange={(e) => setToken(e.target.value)}
          className="w-full border rounded-md px-3 py-2 mb-4"
          placeholder={t("tokenPlaceholder")}
        />
        {error && <p role="alert" className="text-red-500 text-sm mb-4">{error}</p>}
        <button
          type="submit"
          disabled={submitting}
          className="w-full bg-blue-600 text-white py-2 rounded-md hover:bg-blue-700 disabled:opacity-60"
        >
          {t("submit")}
        </button>
      </form>
    </div>
  );
}
