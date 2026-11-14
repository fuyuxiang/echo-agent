// Enumerate ordinary DOM plus open shadow trees; closed roots remain unavailable.
(selector, root = document) => {
  const output = [];
  const roots = [root];
  let visited = 0;
  while (roots.length && visited < 12000 && output.length < 2000) {
    const current = roots.shift();
    output.push(...current.querySelectorAll(selector));
    for (const element of current.querySelectorAll('*')) {
      if (++visited > 12000) break;
      if (element.shadowRoot) roots.push(element.shadowRoot);
    }
  }
  return output;
}
