(() => {
  const channel = window.webkit.messageHandlers.echoAgentFrameRegistration;
  if (!channel || globalThis.__echoFrameRegistration) return;
  const token = [...crypto.getRandomValues(new Uint32Array(4))].map(n => n.toString(16).padStart(8, "0")).join("");
  function path() {
    const result = [];
    try {
      for (let target = window; target !== target.parent; target = target.parent) {
        const parent = target.parent;
        if (result.length >= 16 || parent.length > 256) return null;
        let index = 0;
        while (index < parent.length && parent[index] !== target) index++;
        if (index === parent.length) return null;
        result.unshift(index);
      }
    } catch {
      return null;
    }
    return result;
  }
  globalThis.__echoFrameRegistration = { token, path };
  const register = () => {
    const currentPath = path();
    if (currentPath) channel.postMessage(JSON.stringify({ token, path: currentPath }));
  };
  window.addEventListener("pageshow", event => { if (event.isTrusted) register(); });
  window.addEventListener("pagehide", event => {
    if (event.isTrusted) channel.postMessage(JSON.stringify({ token }));
  });
  register();
})()
