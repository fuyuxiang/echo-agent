import i18n, { type Resource } from "i18next";
import { initReactI18next } from "react-i18next";
import LanguageDetector from "i18next-browser-languagedetector";

// 用 Vite 的 glob 一次性收集所有词条:./locales/<lng>/<ns>.json。
// 新增语言或 namespace 只需加 json 文件,无需改本文件。
const modules = import.meta.glob("./locales/*/*.json", { eager: true });
export const resources: Record<string, Record<string, unknown>> = {};
for (const [path, mod] of Object.entries(modules)) {
  const m = path.match(/\.\/locales\/([^/]+)\/([^/]+)\.json$/);
  if (!m) continue;
  const [, lng, ns] = m;
  (resources[lng] ??= {})[ns] = (mod as { default: unknown }).default;
}

i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources: resources as Resource,
    fallbackLng: "zh",
    supportedLngs: ["zh", "en"],
    defaultNS: "common",
    interpolation: { escapeValue: false },
    detection: {
      order: ["localStorage", "navigator"],
      lookupLocalStorage: "echo_lang",
      caches: ["localStorage"],
    },
  });

// 同步 <html lang>,利于无障碍;切换语言时更新。
i18n.on("languageChanged", (lng) => {
  document.documentElement.lang = lng;
});
document.documentElement.lang = i18n.resolvedLanguage ?? "zh";

export default i18n;
