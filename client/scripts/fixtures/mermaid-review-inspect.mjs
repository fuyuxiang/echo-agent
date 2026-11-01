/** Inspect the XML document behind the production Blob image. Using an HTML
 * fragment would hide namespace, wrapping and SVG image-decoder regressions. */
export async function inspectDiagramImage(image) {
  if (!image) throw new Error("Missing production diagram image");
  const colorCanvas = document.createElement("canvas");
  colorCanvas.width = colorCanvas.height = 1;
  const colorContext = colorCanvas.getContext("2d", { willReadFrequently: true });
  let surface = "rgb(255, 255, 255)";
  for (let element = image; element; element = element.parentElement) {
    const background = getComputedStyle(element).backgroundColor;
    colorContext.clearRect(0, 0, 1, 1);
    colorContext.fillStyle = background; colorContext.fillRect(0, 0, 1, 1);
    const alpha = colorContext.getImageData(0, 0, 1, 1).data[3];
    if (!alpha) continue;
    if (alpha !== 255) throw new Error("Diagram review needs an explicit composited translucent backdrop");
    surface = background; break;
  }
  await image.decode();
  // WKWebView reports the responsive SVG element's current layout size as
  // naturalWidth/naturalHeight. Decode the same Blob independently to measure
  // its intrinsic dimensions without the production img's max-width rule.
  const decoded = new Image();
  decoded.src = image.src;
  await decoded.decode();
  const svg = await (await fetch(image.src)).text();
  const frame = document.createElement("iframe");
  frame.style.cssText = "position:fixed;left:-10000px;top:0;border:0;visibility:hidden;pointer-events:none";
  frame.style.width = `${decoded.naturalWidth}px`;
  frame.style.height = `${decoded.naturalHeight}px`;
  const loaded = new Promise((resolve, reject) => {
    frame.onload = resolve;
    frame.onerror = () => reject(new Error("Could not load standalone SVG document"));
  });
  frame.src = image.src;
  document.body.appendChild(frame);
  try {
    await loaded;
    const doc = frame.contentDocument;
    if (!doc?.documentElement || doc.querySelector("parsererror")) throw new Error("Invalid standalone SVG");
    await doc.fonts?.ready;
    const root = doc.documentElement;
    const viewBox = root.getAttribute("viewBox").split(/[\s,]+/).map(Number);
    const inverse = root.getScreenCTM().inverse();
    const point = (x, y, matrix = inverse) => {
      const value = root.createSVGPoint(); value.x = x; value.y = y;
      const result = value.matrixTransform(matrix);
      return { x: result.x, y: result.y };
    };
    const bounds = (rect) => {
      const corners = [point(rect.left, rect.top), point(rect.right, rect.top),
        point(rect.right, rect.bottom), point(rect.left, rect.bottom)];
      return { left: Math.min(...corners.map(p => p.x)), right: Math.max(...corners.map(p => p.x)),
        top: Math.min(...corners.map(p => p.y)), bottom: Math.max(...corners.map(p => p.y)) };
    };
    const union = (rects) => ({ left: Math.min(...rects.map(r => r.left)), right: Math.max(...rects.map(r => r.right)),
      top: Math.min(...rects.map(r => r.top)), bottom: Math.max(...rects.map(r => r.bottom)) });
    const slack = (inner, outer) => ({ left: inner.left - outer.left, right: outer.right - inner.right,
      top: inner.top - outer.top, bottom: outer.bottom - inner.bottom });
    const intersect = (one, two) => Math.min(one.right, two.right) - Math.max(one.left, two.left) > 0.5
      && Math.min(one.bottom, two.bottom) - Math.max(one.top, two.top) > 0.5;
    const normalize = text => text.replace(/[\s\u00a0]+/gu, " ").trim();
    const labelText = element => {
      if (element.nodeType === Node.TEXT_NODE) return element.textContent;
      if (element.nodeName.toLowerCase() === "br") return " ";
      const text = [...element.childNodes].map(labelText).join("");
      return ["p", "div", "li"].includes(element.nodeName.toLowerCase()) ? ` ${text} ` : text;
    };
    const viewport = { left: viewBox[0], right: viewBox[0] + viewBox[2], top: viewBox[1], bottom: viewBox[1] + viewBox[3] };
    const elements = [...root.querySelectorAll(".nodes > g")];
    const nodes = elements.map(node => {
      const box = node.querySelector(".label > foreignObject");
      const label = box?.firstElementChild ?? node.querySelector(".label");
      const card = node.querySelector(".label-container");
      const cardBounds = bounds((card ?? node).getBoundingClientRect());
      const textRects = [], textColors = new Set();
      if (label) {
        const walker = doc.createTreeWalker(label, NodeFilter.SHOW_TEXT);
        while (walker.nextNode()) {
          if (!walker.currentNode.textContent.trim()) continue;
          textColors.add(frame.contentWindow.getComputedStyle(walker.currentNode.parentElement).color);
          const range = doc.createRange(); range.selectNodeContents(walker.currentNode);
          textRects.push(...[...range.getClientRects()].filter(r => r.width > 0 && r.height > 0).map(bounds));
        }
      }
      const textBounds = textRects.length ? union(textRects) : null;
      const visibleBounds = union([cardBounds, ...(textBounds ? [textBounds] : [])]);
      const style = label ? frame.contentWindow.getComputedStyle(label) : null;
      const shape = card?.tagName.toLowerCase();
      // Bounding boxes alone cannot prove that text fits inside a circle.
      // Check every actual line's corners against its curved outline too.
      let curvedOverflow = null;
      if (["circle", "ellipse"].includes(shape) && textRects.length) {
        const rx = (cardBounds.right - cardBounds.left) / 2, ry = (cardBounds.bottom - cardBounds.top) / 2;
        const cx = (cardBounds.left + cardBounds.right) / 2, cy = (cardBounds.top + cardBounds.bottom) / 2;
        curvedOverflow = Math.max(...textRects.flatMap(r => [[r.left, r.top], [r.right, r.top], [r.right, r.bottom], [r.left, r.bottom]])
          .map(([x, y]) => (Math.hypot((x - cx) / rx, (y - cy) / ry) - 1) * Math.min(rx, ry)));
      }
      return {
        id: node.id, text: normalize(labelText(label ?? node)), html: !!box,
        labelWidth: label ? bounds(label.getBoundingClientRect()).right - bounds(label.getBoundingClientRect()).left : 0,
        boxWidth: box ? Number(box.getAttribute("width")) : null, boxHeight: box ? Number(box.getAttribute("height")) : null,
        cardWidth: cardBounds.right - cardBounds.left, cardHeight: cardBounds.bottom - cardBounds.top,
        slack: box && textBounds ? slack(textBounds, bounds(box.getBoundingClientRect())) : null,
        cardSlack: textBounds ? slack(textBounds, cardBounds) : null, curvedOverflow,
        viewportSlack: slack(visibleBounds, viewport),
        centerOffset: textBounds ? (textBounds.left + textBounds.right - cardBounds.left - cardBounds.right) / 2 : 0,
        shape, lines: new Set(textRects.map(r => Math.round(r.top * 10))).size,
        whiteSpace: style?.whiteSpace, overflowWrap: style?.overflowWrap,
        fill: card ? frame.contentWindow.getComputedStyle(card).fill : null,
        textColors: [...textColors],
        bounds: cardBounds, textBounds, textRects, visibleBounds,
      };
    });
    const overlaps = [], textOverlaps = [];
    for (let a = 0; a < nodes.length; a++) for (let b = a + 1; b < nodes.length; b++) {
      if (intersect(nodes[a].visibleBounds, nodes[b].visibleBounds)) overlaps.push([nodes[a].text, nodes[b].text]);
      if (nodes[a].textRects.some(one => nodes[b].textRects.some(two => intersect(one, two)))) textOverlaps.push([nodes[a].text, nodes[b].text]);
    }
    const pathPoint = (path, length) => {
      const local = path.getPointAtLength(length);
      return point(local.x, local.y, inverse.multiply(path.getScreenCTM()));
    };
    const outlineGap = (card, endpoint, fallback) => {
      if (typeof card?.getTotalLength === "function") {
        const length = card.getTotalLength();
        // Approximate the nearest outline point, then refine the small interval.
        let best = 0, distance = Infinity;
        const at = value => { const p = pathPoint(card, value); return Math.hypot(p.x - endpoint.x, p.y - endpoint.y); };
        for (let i = 0; i <= 128; i++) {
          const value = length * i / 128, candidate = at(value);
          if (candidate < distance) { best = value; distance = candidate; }
        }
        let lo = Math.max(0, best - length / 128), hi = Math.min(length, best + length / 128);
        for (let i = 0; i < 16; i++) {
          const a = lo + (hi - lo) / 3, b = hi - (hi - lo) / 3;
          if (at(a) < at(b)) hi = b; else lo = a;
        }
        return Math.min(distance, at((lo + hi) / 2));
      }
      const dx = Math.max(fallback.left - endpoint.x, 0, endpoint.x - fallback.right);
      const dy = Math.max(fallback.top - endpoint.y, 0, endpoint.y - fallback.bottom);
      return dx || dy ? Math.hypot(dx, dy) : Math.min(endpoint.x - fallback.left, fallback.right - endpoint.x,
        endpoint.y - fallback.top, fallback.bottom - endpoint.y);
    };
    const edgeDetails = [...root.querySelectorAll(".edgePaths path")].map(path => {
      const length = path.getTotalLength();
      const match = path.getAttribute("data-id")?.match(/^edge_(\d+)_(\d+)$/u);
      const fromIndex = match ? elements.findIndex(n => n.id.endsWith(`-node_${match[1]}`)) : -1;
      const toIndex = match ? elements.findIndex(n => n.id.endsWith(`-node_${match[2]}`)) : -1;
      const start = pathPoint(path, 0), end = pathPoint(path, length);
      const paint = frame.contentWindow.getComputedStyle(path);
      return { id: path.getAttribute("data-id"), length, start, end,
        stroke: paint.stroke, strokeOpacity: Number(paint.strokeOpacity) * Number(paint.opacity),
        from: nodes[fromIndex]?.text ?? null, to: nodes[toIndex]?.text ?? null,
        startGap: fromIndex >= 0 ? outlineGap(elements[fromIndex].querySelector(".label-container"), start, nodes[fromIndex].bounds) : null,
        endGap: toIndex >= 0 ? outlineGap(elements[toIndex].querySelector(".label-container"), end, nodes[toIndex].bounds) : null,
        viewportSlack: slack(bounds(path.getBoundingClientRect()), viewport) };
    });
    return { svg, surface, viewBox, nodes, overlaps, textOverlaps, edges: edgeDetails.length, edgeDetails,
      edgeLabels: [...root.querySelectorAll(".edgeLabels .label")].map(label => normalize(label.textContent)).filter(Boolean),
      imageWidth: decoded.naturalWidth, imageHeight: decoded.naturalHeight,
      svgWidth: Number(root.getAttribute("width")), svgHeight: Number(root.getAttribute("height")),
      displayImageWidth: image.naturalWidth, displayImageHeight: image.naturalHeight,
      renderedWidth: image.getBoundingClientRect().width, renderedHeight: image.getBoundingClientRect().height };
  } finally { frame.remove(); }
}
