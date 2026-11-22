function editArtifact(message) {
  const handle = globalThis.__echoArtifactEditor ||= (() => {
    let running = false;
    let selected;
    let outline;
    let sequence = 0;
    const parse = source => new DOMParser().parseFromString(source, "text/html");
    const atPath = (doc, path) => path.reduce((node, index) => node?.children[index], doc.body);
    const startTags = tag => new RegExp(`<${tag}(?=[\\s/>])(?:"[^"]*"|'[^']*'|[^'">])*?>`, "gi");
    const explicitEndTag = tag => new RegExp(`</${tag}\\s*>`, "gi");
    const editableTag = element => element instanceof HTMLElement
      && !["html", "body", "script", "style", "link", "meta", "base", "template", "noscript", "iframe"].includes(element.localName);
    const position = () => {
      if (!outline || !selected?.element.isConnected) {
        outline?.remove();
        return;
      }
      const rect = selected.element.getBoundingClientRect();
      Object.assign(outline.style, {
        left: rect.left + "px", top: rect.top + "px",
        width: rect.width + "px", height: rect.height + "px",
      });
    };
    const intercept = event => {
      event.preventDefault();
      event.stopImmediatePropagation();
    };
    function select(event) {
      const element = event.target;
      if (!editableTag(element)) return;
      intercept(event);
      let node = element;
      const path = [];
      while (node && node !== document.body) {
        if (!node.parentElement) return;
        path.unshift(Array.prototype.indexOf.call(node.parentElement.children, node));
        node = node.parentElement;
      }
      if (!path.length || path.length > 64) return;
      const style = getComputedStyle(element);
      const color = style.color.match(/\d+/g)?.slice(0, 3).map(number => Number(number).toString(16).padStart(2, "0")).join("");
      const token = (crypto.randomUUID?.() ?? String(Math.random())) + "-" + (++sequence);
      selected = { element, signature: element.outerHTML, path, details: {
        token, tag: element.localName, id: element.id.slice(0, 160),
        label: element.textContent.trim().slice(0, 140),
        text: element.childElementCount === 0 && !element.matches("img,input,textarea,select,br,hr")
          ? element.textContent.slice(0, 4000) : null,
        color: color?.length === 6 ? "#" + color : "#171717",
        fontSize: Math.round(parseFloat(style.fontSize)) || 16,
        bold: Number(style.fontWeight) >= 600,
        image: element.matches("img") ? element.getAttribute("src") || "" : null,
      } };
      if (!outline) {
        outline = document.createElement("div");
        outline.setAttribute("aria-hidden", "true");
        outline.style.cssText = "pointer-events:none;position:fixed;z-index:2147483647;outline:2px solid #5270ff;background:#5070ff1a";
      }
      // Keep the indicator outside the body so element indices remain stable.
      document.documentElement.append(outline);
      position();
    }
    function stop() {
      if (running) {
        for (const type of ["pointerdown", "pointerup", "mousedown", "mouseup", "auxclick", "contextmenu"]) {
          window.removeEventListener(type, intercept, true);
        }
        window.removeEventListener("click", select, true);
        window.removeEventListener("scroll", position, true);
        window.removeEventListener("resize", position);
      }
      outline?.remove();
      outline = selected = undefined;
      running = false;
    }
    return ({ kind, source, token, change }) => {
      if (kind === "start" && !running) {
        running = true;
        for (const type of ["pointerdown", "pointerup", "mousedown", "mouseup", "auxclick", "contextmenu"]) {
          window.addEventListener(type, intercept, true);
        }
        window.addEventListener("click", select, true);
        window.addEventListener("scroll", position, true);
        window.addEventListener("resize", position);
      }
      if (kind === "stop") stop();
      if (kind !== "apply") return selected?.element.isConnected ? selected.details : null;
      if (!running || token !== selected?.details.token || !selected.element.isConnected
        || selected.element.outerHTML !== selected.signature) {
        return { error: "网页元素已变化，请重新选择" };
      }
      const doc = parse(source);
      const element = atPath(doc, selected.path);
      if (!editableTag(element) || element.outerHTML !== selected.signature) {
        return { error: "此元素由脚本生成或源码已变化。可以让 Agent 修改原始 HTML、CSS 或脚本。" };
      }
      const original = element.cloneNode(true);
      if (change.text != null) {
        if (selected.details.text == null) return { error: "请选择只包含文字的元素" };
        element.textContent = change.text;
      }
      if (change.color != null) element.style.color = change.color;
      if (change.fontSize != null) element.style.fontSize = change.fontSize + "px";
      if (change.bold != null) element.style.fontWeight = change.bold ? "700" : "400";
      if (change.image != null) {
        if (!element.matches("img")) return { error: "请选择图片元素" };
        element.setAttribute("src", change.image);
        element.removeAttribute("srcset");
      }
      if (original.outerHTML === element.outerHTML) return source;

      // Ask the browser's parser which explicit start tag produced this DOM
      // node. This also handles case, quoting, automatic table nodes and
      // duplicate elements without relying on DOM serialization offsets.
      const tag = element.localName;
      if (!/^[a-z][a-z0-9-]*$/.test(tag)) return { error: "请在源码中编辑此元素" };
      const marker = "data-echo-source-" + token.replace(/[^a-z0-9-]/gi, "");
      const candidates = [];
      const marked = source.replace(startTags(tag), (literal, start) => {
        const index = candidates.push({ start, end: start + literal.length }) - 1;
        return literal.slice(0, tag.length + 1) + ` ${marker}="${index}"` + literal.slice(tag.length + 1);
      });
      const mapped = atPath(parse(marked), selected.path);
      const index = mapped?.getAttribute(marker);
      mapped?.removeAttribute(marker);
      const range = index == null ? undefined : candidates[Number(index)];
      if (!range || mapped.outerHTML !== original.outerHTML) return { error: "无法定位元素的源码，请在源码中编辑" };

      const newStart = startTags(tag).exec(element.outerHTML)?.[0];
      if (!newStart) return { error: "无法保存此元素，请在源码中编辑" };
      let end = range.end;
      let replacement = original.cloneNode(false).outerHTML === element.cloneNode(false).outerHTML
        ? source.slice(range.start, range.end) : newStart;
      if (change.text != null && original.textContent !== element.textContent) {
        const close = explicitEndTag(tag);
        close.lastIndex = range.end;
        const closing = close.exec(source);
        if (!closing || /<[!?/a-z]/i.test(source.slice(range.end, closing.index))) {
          return { error: "文字没有独立的源码范围，请在源码中编辑" };
        }
        if (["pre", "listing"].includes(tag) && element.textContent.startsWith("\n")) replacement += "\n";
        replacement += element.innerHTML;
        end = closing.index;
      }
      const result = source.slice(0, range.start) + replacement + source.slice(end);
      const updated = parse(result);
      if (atPath(updated, selected.path)?.outerHTML !== element.outerHTML
        || updated.documentElement.outerHTML !== doc.documentElement.outerHTML) {
        return { error: "无法单独修改此元素，请在源码中编辑" };
      }
      return result;
    };
  })();
  return handle(message);
}
