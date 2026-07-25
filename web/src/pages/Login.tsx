import { useState } from "react";
import { useNavigate } from "react-router";
import { useTranslation } from "react-i18next";
import { useAuthStore } from "../stores/auth";
import { apiFetch } from "../lib/api";

export function Login() {
  const { t } = useTranslation("login");
  const [token, setToken] = useState("");
  const [error, setError] = useState("");
  const navigate = useNavigate();
  const setStoreToken = useAuthStore((s) => s.setToken);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    // apiFetch 从 localStorage 读 token,所以必须先落盘再校验;失败再回滚,
    // 否则无效 token 也会残留。/health 无需鉴权,验不出 token,这里改用
    // /config —— 它走 admin token 校验,能真正验证输入的 admin token 是否有效。
    const prev = localStorage.getItem("echo_token");
    setStoreToken(token);
    try {
      await apiFetch("/config");
      navigate("/", { replace: true });
    } catch {
      if (prev !== null) {
        localStorage.setItem("echo_token", prev);
      } else {
        localStorage.removeItem("echo_token");
      }
      useAuthStore.setState({ token: prev });
      setError(t("invalidToken"));
    }
  };

  return (
    <div className="flex items-center justify-center h-screen bg-gray-50">
      <form onSubmit={handleSubmit} className="bg-white p-8 rounded-lg shadow-md w-96">
        <h1 className="text-xl font-bold mb-4">{t("title")}</h1>
        <label className="block text-sm text-gray-600 mb-1">{t("tokenLabel")}</label>
        <input
          type="password"
          value={token}
          onChange={(e) => setToken(e.target.value)}
          className="w-full border rounded-md px-3 py-2 mb-4"
          placeholder={t("tokenPlaceholder")}
        />
        {error && <p className="text-red-500 text-sm mb-4">{error}</p>}
        <button type="submit" className="w-full bg-blue-600 text-white py-2 rounded-md hover:bg-blue-700">
          {t("submit")}
        </button>
      </form>
    </div>
  );
}
