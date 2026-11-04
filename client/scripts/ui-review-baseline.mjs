import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";

/** CSS @import reads files outside Vite's load hooks. A physical HEAD snapshot
 * keeps nested styles and assets consistent. HEAD fixtures retain entry order. */
export function createUiReviewBaseline(root = process.cwd(), { currentFixtures = false } = {}) {
  root = resolve(root);
  const directory = mkdtempSync(join(tmpdir(), "echo-ui-review-head-"));
  const close = () => rmSync(directory, { recursive: true, force: true });
  try {
    const revision = execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).trim();
    const archive = execFileSync("git", ["archive", revision, "src", "scripts/fixtures", "index.html"], {
      cwd: root, maxBuffer: 128 * 1024 * 1024,
    });
    execFileSync("tar", ["-x", "-C", directory], { input: archive });
    const sourcePath = (path) => {
      const name = relative(root, path);
      // New acceptance scenarios can exercise HEAD production components with
      // exactly the same controlled data and interactions as the working tree.
      if (currentFixtures && /^scripts[/\\]fixtures[/\\]/.test(name)) return null;
      return /^(src|scripts[/\\]fixtures)[/\\]/.test(name) ? name : null;
    };
    return {
      directory, revision, close,
      plugin: {
        name: "ui-review-original-source", enforce: "pre",
        transformIndexHtml: {
          order: "pre",
          handler(html, context) {
            const path = context.path.split("?")[0];
            return path === "/" || path === "/index.html" ? readFileSync(join(directory,"index.html"),"utf8") : html;
          },
        },
        resolveId(source, importer) {
          if (!/\.css(?:\?|$)/.test(source)) return null;
          const [file, query] = source.split(/\?(.*)/s);
          const path = file.startsWith("/src/") ? resolve(root, file.slice(1))
            : isAbsolute(file) ? file
            : file.startsWith(".") && importer ? resolve(dirname(importer.split("?")[0]), file) : null;
          const name = path && sourcePath(path);
          if (!name) return null;
          const original = join(directory, name);
          if (!existsSync(original)) throw new Error(`Baseline stylesheet is absent from ${revision}: ${name}`);
          return original + (query === undefined ? "" : `?${query}`);
        },
        load(id) {
          if (/[?&](raw|url|worker)([=&]|$)/.test(id)) return null;
          const path = id.split("?")[0];
          const name = sourcePath(path);
          if (!name || !/\.(tsx?|css)$/.test(path)) return null;
          const original = join(directory, name);
          if (!existsSync(original)) throw new Error(`Baseline source is absent from ${revision}: ${name}`);
          return readFileSync(original, "utf8");
        },
      },
    };
  } catch (error) { close(); throw error; }
}
