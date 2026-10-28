import "@testing-library/jest-dom/vitest";
import { beforeEach } from "vitest";
import i18n from "../i18n";

// 每个测试前重置到 zh,避免切换语言的测试相互污染。
beforeEach(async () => {
  await i18n.changeLanguage("zh");
});
