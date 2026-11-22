/**
 * 浏览器预览 —— 对齐 EchoAgent `context-viewer-components/browser-preview`
 * (嵌入式网页预览框)。
 *
 * 纯函数:URL 安全校验 + iframe sandbox 策略。EchoAgent 是本地桌面应用,在 WebView 内
 * 再嵌 iframe 预览外部页面,需限制可加载的协议 + sandbox 属性。便于单测。
 */

/** 判定 URL 是否可安全嵌入预览。 */
export function isPreviewableUrl(url: string): boolean {
  try {
    if (!url || url.length > 4096 || /[\u0000-\u001f\u007f]/.test(url)) return false;
    const u = new URL(url);
    // 仅允许 http/https(禁 file/data/javascript/blob 等,避免本地文件/脚本注入)。
    if (u.protocol !== "http:" && u.protocol !== "https:") return false;
    // 拒绝明显的本地/内网回环(localhost/127/0.0.0.0/::1/内网 IP 段)以防 SSRF。
    if (u.username || u.password) return false;
    const host = u.hostname.toLowerCase().replace(/^\[|\]$/g, "").replace(/\.+$/, "");
    if (!host) return false;
    // Block local aliases as well as Tauri's ipc.localhost/asset.localhost
    // special hosts. A suffix check is essential: checking only "localhost"
    // still allowed attacker-controlled frames to address privileged subhosts.
    if (
      host === "localhost"
      || host.endsWith(".localhost")
      || host.endsWith(".local")
      || host.endsWith(".localdomain")
      || host.endsWith(".internal")
      || host.endsWith(".lan")
      || host === "home.arpa"
      || host.endsWith(".home.arpa")
    ) return false;

    const ipv4 = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
    if (ipv4) {
      const [, aRaw, bRaw, cRaw] = ipv4;
      const a = Number(aRaw);
      const b = Number(bRaw);
      const c = Number(cRaw);
      if (
        a === 0
        || a === 10
        || a === 127
        || a >= 224
        || (a === 100 && b >= 64 && b <= 127)
        || (a === 169 && b === 254)
        || (a === 172 && b >= 16 && b <= 31)
        || (a === 192 && b === 168)
        || (a === 192 && b === 0 && (c === 0 || c === 2))
        || (a === 198 && (b === 18 || b === 19 || (b === 51 && c === 100)))
        || (a === 203 && b === 0 && c === 113)
      ) return false;
    }

    if (host.includes(":")) {
      if (host === "::" || host === "::1" || host.startsWith("::ffff:")) return false;
      const first = Number.parseInt(host.split(":").find(Boolean) ?? "0", 16);
      // fc00::/7 unique-local, fe80::/10 link-local, ff00::/8 multicast.
      if ((first & 0xfe00) === 0xfc00 || (first & 0xffc0) === 0xfe80 || (first & 0xff00) === 0xff00) {
        return false;
      }
    }
    return true;
  } catch {
    return false;
  }
}

const TRUSTED_REMOTE_IMAGE_HOSTS = new Set<string>();

/** Remote catalog images are disabled unless a trusted asset host is wired in.
 * Local catalog images are loaded through bounded native commands; keeping the
 * remote allow-list empty avoids DNS rebinding against local/private services. */
export function safeRemoteImageUrl(value?: string): string | undefined {
  if (!value || !/^https:\/\//i.test(value)) return undefined;
  if (!isPreviewableUrl(value)) return undefined;
  try {
    const url = new URL(value);
    const host = url.hostname.toLowerCase().replace(/^\[|\]$/g, "").replace(/\.+$/, "");
    if (!TRUSTED_REMOTE_IMAGE_HOSTS.has(host)) return undefined;
    return url.toString();
  } catch {
    return undefined;
  }
}

/** Parse an address for the managed browser. Network policy is enforced by its native controller. */
export function normalizePreviewUrl(input: string): string | null {
  const address = input.trim();
  if (!address || address.length > 8192 || /[\u0000-\u001f\u007f]/.test(address)) return null;
  const hasScheme = /^[a-z][a-z0-9+.-]*:/i.test(address);
  const hostWithPort = /^(?:[\w.-]+|\[[\da-f:]+\]):\d+(?:[/?#]|$)/i.test(address);
  if (hasScheme && !hostWithPort && !/^https?:\/\//i.test(address)) return null;
  const local = /^(?:localhost|127\.0\.0\.1|\[::1\])(?=[:/?#]|$)/i.test(address);
  try {
    const url = new URL(/^https?:\/\//i.test(address) ? address : `${local ? "http" : "https"}://${address}`);
    return url.hostname && !url.username && !url.password ? url.toString() : null;
  } catch {
    return null;
  }
}

/** The panel can request a task-scoped exception for an exact loopback port. */
export function developmentEndpoint(url: string): string | null {
  const normalized = normalizePreviewUrl(url);
  if (!normalized) return null;
  const parsed = new URL(normalized);
  return ["localhost", "127.0.0.1", "[::1]"].includes(parsed.hostname)
    ? `${parsed.hostname}:${parsed.port || (parsed.protocol === "https:" ? "443" : "80")}`
    : null;
}
