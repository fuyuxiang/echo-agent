// Run in the browser: inspect resolved sRGB colors, including nested alpha and
// group opacity. Raster/gradient/SVG content is reported for visual review.
export function inspectRenderedColors() {
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = 1;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  const colors = new Map();
  const rgba = value => {
    if (!colors.has(value)) {
      // Preserve computed alpha precision; 8-bit canvas alpha rounds .5 to
      // 128/255 and can move borderline WCAG measurements across a threshold.
      const channels = value.match(/[\d.]+/g)?.map(Number);
      if (/^rgba?\(/.test(value)) {
        colors.set(value, [...channels.slice(0, 3), channels[3] ?? 1]);
      } else if (/^color\(srgb /.test(value)) {
        colors.set(value, [...channels.slice(0, 3).map(c => c * 255), channels[3] ?? 1]);
      } else {
      ctx.clearRect(0, 0, 1, 1);
      ctx.fillStyle = value; ctx.fillRect(0, 0, 1, 1);
      const p = [...ctx.getImageData(0, 0, 1, 1).data];
      colors.set(value, [p[0], p[1], p[2], p[3] / 255]);
      }
    }
    return colors.get(value);
  };
  const over = (a, b) => {
    const alpha = a[3] + b[3] * (1 - a[3]);
    return [...[0, 1, 2].map(i => alpha ? (a[i] * a[3] + b[i] * b[3] * (1 - a[3])) / alpha : 0), alpha];
  };
  const luminance = c => c.slice(0, 3).map(v => v / 255).map(v => v <= .04045 ? v / 12.92 : ((v + .055) / 1.055) ** 2.4).reduce((s, v, i) => s + v * [.2126, .7152, .0722][i], 0);
  const ratio = (a, b) => (Math.max(luminance(a), luminance(b)) + .05) / (Math.min(luminance(a), luminance(b)) + .05);
  const styleCache = new Map();
  const style = e => { if (!styleCache.has(e)) styleCache.set(e, getComputedStyle(e)); return styleCache.get(e); };
  const pixel = (element, foreground) => {
    let result = foreground;
    for (let e = element; e; e = e.parentElement) {
      const s = style(e);
      result = over(result, rgba(s.backgroundColor));
      result = [...result.slice(0, 3), result[3] * Number(s.opacity)];
    }
    // The browser canvas is white when no authored opaque background exists.
    return over(result, [255, 255, 255, 1]);
  };
  const visible = e => {
    const s = style(e), r = e.getBoundingClientRect();
    if (Number(s.opacity) === 0 || s.visibility !== "visible" || s.display === "none" || !r.width || !r.height) return false;
    let box = { left: Math.max(0, r.left), top: Math.max(0, r.top), right: Math.min(innerWidth, r.right), bottom: Math.min(innerHeight, r.bottom) };
    for (let p = e.parentElement; p; p = p.parentElement) {
      if (Number(style(p).opacity) === 0) return false;
      if (/(hidden|clip|auto|scroll)/.test(style(p).overflow)) {
        const b = p.getBoundingClientRect();
        box = { left: Math.max(box.left, b.left), top: Math.max(box.top, b.top), right: Math.min(box.right, b.right), bottom: Math.min(box.bottom, b.bottom) };
      }
    }
    if (box.right <= box.left || box.bottom <= box.top) return false;
    const hit = document.elementFromPoint((box.left + box.right) / 2, (box.top + box.bottom) / 2);
    return hit && (e.contains(hit) || hit.contains(e));
  };
  const label = e => `${e.tagName.toLowerCase()}${e.id ? `#${e.id}` : ""}${[...e.classList].map(c => `.${c}`).join("")}`;
  const text = [], manual = [], disabled = [];
  for (const e of document.querySelectorAll("body *")) {
    if (!(e instanceof HTMLElement) || !visible(e) || e.closest("script,style,svg,.katex-mathml")) continue;
    const direct = [...e.childNodes].filter(n => n.nodeType === Node.TEXT_NODE).map(n => n.textContent).join("").trim();
    const placeholder = (e instanceof HTMLInputElement || e instanceof HTMLTextAreaElement) && !e.value && e.placeholder;
    const value = e instanceof HTMLTextAreaElement || e instanceof HTMLInputElement && !["checkbox", "radio", "range", "hidden"].includes(e.type)
      ? e.value : e instanceof HTMLSelectElement ? [...e.selectedOptions].map(option => option.label).join(" ") : "";
    if (!direct && !placeholder && !value) continue;
    const s = placeholder ? getComputedStyle(e, "::placeholder") : style(e);
    const complex = [];
    if (e.closest(".monaco-diff-editor .view-lines")) complex.push("layered-editor-background: use editor-paint-audit");
    for (let p = e; p; p = p.parentElement) if (style(p).backgroundImage !== "none" || style(p).filter !== "none" || style(p).mixBlendMode !== "normal") complex.push(label(p));
    const fg = [...rgba(s.color)];
    if (placeholder) fg[3] *= Number(s.opacity);
    const background = pixel(e, [0, 0, 0, 0]);
    const foreground = pixel(e, fg);
    const threshold = parseFloat(s.fontSize) >= 24 || (parseFloat(s.fontSize) >= 18.667 && parseInt(s.fontWeight) >= 700) ? 3 : 4.5;
    const item = { selector: label(e), text: String(placeholder || value || direct).slice(0, 90), color: s.color, foreground: foreground.slice(0, 3).map(Math.round), background: background.slice(0, 3).map(Math.round), ratio: ratio(foreground, background), threshold, placeholder: !!placeholder };
    if (e.closest(":disabled,[aria-disabled=true]") || e.matches("label:has(input:disabled)")) disabled.push(item);
    else if (complex.length) manual.push({ ...item, complex });
    else text.push(item);
  }
  const controls = [];
  for (const e of document.querySelectorAll(".form-control,.settings-input,.capability-search input,.echo-checkbox-box,.echo-switch,.meeting-panel__create input,.meeting-panel__create select,.meeting-source__check")) {
    if (!visible(e) || e.closest(":disabled,[aria-disabled=true],.echo-switch--disabled,.echo-checkbox--disabled")) continue;
    const s = style(e);
    const background = pixel(e, [0, 0, 0, 0]);
    const boundaryColor = e.matches(".echo-switch") ? s.boxShadow.includes("inset") ? style(e.querySelector(".echo-switch-thumb")).backgroundColor : s.backgroundColor : s.borderTopColor;
    controls.push({selector:label(e),role:"boundary",ratio:ratio(pixel(e,rgba(boundaryColor)), e.matches(".echo-switch") ? pixel(e.parentElement,[0,0,0,0]) : background),threshold:3});
    if (s.outlineStyle !== "none" && parseFloat(s.outlineWidth) > 0) controls.push({selector:label(e),role:"focus",ratio:ratio(pixel(e.parentElement,rgba(s.outlineColor)),pixel(e.parentElement,[0,0,0,0])),threshold:3});
  }
  for (const e of document.querySelectorAll(".echo-composer,.echo-theia-agent__composer-box")) {
    if (!visible(e)) continue;
    const s = style(e);
    if (s.outlineStyle !== "none" && parseFloat(s.outlineWidth) > 0) controls.push({selector:label(e),role:"focus",ratio:ratio(pixel(e.parentElement,rgba(s.outlineColor)),pixel(e.parentElement,[0,0,0,0])),threshold:3});
  }
  for (const e of document.querySelectorAll('svg [fill*="--echo-status-"],.meeting-recorder__pulse svg,.meeting-global-recorder__dot svg')) {
    if (!visible(e) || e.closest(":disabled,[aria-disabled=true]")) continue;
    const s = style(e), filled = s.fill !== "none";
    const paint = [...rgba(filled ? s.fill : s.stroke)];
    paint[3] *= Number(filled ? s.fillOpacity : s.strokeOpacity);
    controls.push({selector:label(e),role:"status-icon",ratio:ratio(pixel(e,paint),pixel(e,[0,0,0,0])),threshold:3});
  }
  // Essential selection marks and action glyphs need their actual SVG paint,
  // not just the containing button's label or border.
  for (const svg of document.querySelectorAll(".knowledge-picker__checkbox svg,.proj-picker-check--on svg,.task-filter-menu__option-check svg,.meeting-player__play svg,.echo-composer__send svg")) {
    if (!visible(svg) || svg.closest(":disabled,[aria-disabled=true]")) continue;
    for (const e of svg.querySelectorAll("path,polyline,polygon,line,circle,rect")) {
      const s = style(e);
      const fill = s.fill !== "none" && rgba(s.fill)[3] > 0;
      const stroke = s.stroke !== "none" && parseFloat(s.strokeWidth) > 0;
      if (!fill && !stroke) continue;
      const paint = [...rgba(fill ? s.fill : s.stroke)];
      paint[3] *= Number(fill ? s.fillOpacity : s.strokeOpacity);
      const option = svg.closest(".task-filter-menu__option");
      controls.push({selector:label(svg.parentElement),role:svg.closest(".meeting-player__play,.echo-composer__send") ? "action-icon" : "selection-mark",
        ...(option && { text: option.textContent.trim() }),
        ratio:ratio(pixel(e,paint),pixel(e,[0,0,0,0])),threshold:3});
    }
  }
  return { text, failures: text.filter(x => x.ratio < x.threshold), controls, controlFailures: controls.filter(x => x.ratio < x.threshold), disabled, manual };
}
