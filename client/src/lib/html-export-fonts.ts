// Loaded only while building an HTML export; exported files need no font server.
import css from "katex/dist/katex.min.css?raw";
import font0 from "katex/dist/fonts/KaTeX_AMS-Regular.woff2?inline";
import font1 from "katex/dist/fonts/KaTeX_Caligraphic-Bold.woff2?inline";
import font2 from "katex/dist/fonts/KaTeX_Caligraphic-Regular.woff2?inline";
import font3 from "katex/dist/fonts/KaTeX_Fraktur-Bold.woff2?inline";
import font4 from "katex/dist/fonts/KaTeX_Fraktur-Regular.woff2?inline";
import font5 from "katex/dist/fonts/KaTeX_Main-Bold.woff2?inline";
import font6 from "katex/dist/fonts/KaTeX_Main-BoldItalic.woff2?inline";
import font7 from "katex/dist/fonts/KaTeX_Main-Italic.woff2?inline";
import font8 from "katex/dist/fonts/KaTeX_Main-Regular.woff2?inline";
import font9 from "katex/dist/fonts/KaTeX_Math-BoldItalic.woff2?inline";
import font10 from "katex/dist/fonts/KaTeX_Math-Italic.woff2?inline";
import font11 from "katex/dist/fonts/KaTeX_SansSerif-Bold.woff2?inline";
import font12 from "katex/dist/fonts/KaTeX_SansSerif-Italic.woff2?inline";
import font13 from "katex/dist/fonts/KaTeX_SansSerif-Regular.woff2?inline";
import font14 from "katex/dist/fonts/KaTeX_Script-Regular.woff2?inline";
import font15 from "katex/dist/fonts/KaTeX_Size1-Regular.woff2?inline";
import font16 from "katex/dist/fonts/KaTeX_Size2-Regular.woff2?inline";
import font17 from "katex/dist/fonts/KaTeX_Size3-Regular.woff2?inline";
import font18 from "katex/dist/fonts/KaTeX_Size4-Regular.woff2?inline";
import font19 from "katex/dist/fonts/KaTeX_Typewriter-Regular.woff2?inline";
const fonts: Record<string, string> = {
  "KaTeX_AMS-Regular.woff2": font0,
  "KaTeX_Caligraphic-Bold.woff2": font1,
  "KaTeX_Caligraphic-Regular.woff2": font2,
  "KaTeX_Fraktur-Bold.woff2": font3,
  "KaTeX_Fraktur-Regular.woff2": font4,
  "KaTeX_Main-Bold.woff2": font5,
  "KaTeX_Main-BoldItalic.woff2": font6,
  "KaTeX_Main-Italic.woff2": font7,
  "KaTeX_Main-Regular.woff2": font8,
  "KaTeX_Math-BoldItalic.woff2": font9,
  "KaTeX_Math-Italic.woff2": font10,
  "KaTeX_SansSerif-Bold.woff2": font11,
  "KaTeX_SansSerif-Italic.woff2": font12,
  "KaTeX_SansSerif-Regular.woff2": font13,
  "KaTeX_Script-Regular.woff2": font14,
  "KaTeX_Size1-Regular.woff2": font15,
  "KaTeX_Size2-Regular.woff2": font16,
  "KaTeX_Size3-Regular.woff2": font17,
  "KaTeX_Size4-Regular.woff2": font18,
  "KaTeX_Typewriter-Regular.woff2": font19,
};
export const htmlExportMathStyles = css.replace(/src:[^;]+;/g, source => {
  const filename = source.match(/fonts\/([^)'"\s]+\.woff2)/)?.[1];
  if (!filename || !fonts[filename]) throw new Error("缺少公式导出字体");
  return `src:url("${fonts[filename]}") format("woff2");`;
});
