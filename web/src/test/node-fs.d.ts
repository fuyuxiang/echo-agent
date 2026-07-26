/**
 * Minimal `node:fs` declaration for tests that read repository files.
 *
 * The dashboard is browser-only, so `@types/node` is deliberately not a
 * dependency — pulling it in would put Node globals in scope for application
 * code too. The cross-stack state machine test needs exactly one function, so
 * declare just that instead.
 */
declare module "node:fs" {
  export function readFileSync(path: URL | string, encoding: "utf8"): string;
  export function existsSync(path: URL | string): boolean;
}

/** Only the one property the state machine test uses to locate the repo root. */
declare const process: { cwd(): string };
