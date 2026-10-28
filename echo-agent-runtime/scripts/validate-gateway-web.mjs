/** Browser acceptance against a disposable, authenticated Gateway workspace.
 *
 * Install Playwright separately (it is not a server runtime dependency), then:
 * ECHO_UI_BASE_URL=http://127.0.0.1:58123 ECHO_UI_TOKEN=... \
 * PLAYWRIGHT_MODULE=/absolute/path/to/playwright/index.mjs \
 * node scripts/validate-gateway-web.mjs
 *
 * Tests /playground over HTTP and WS and saves/restores memory.enabled through
 * the Dashboard. Use only a test workspace: these actions create session data.
 * The Gateway must have a working provider or a controlled test responder.
 */
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

const base = process.env.ECHO_UI_BASE_URL;
const token = process.env.ECHO_UI_TOKEN;
if (!base || !token) throw new Error("ECHO_UI_BASE_URL and ECHO_UI_TOKEN are required for this test workspace");
const moduleName = process.env.PLAYWRIGHT_MODULE;
const { chromium } = await import(moduleName ? pathToFileURL(moduleName).href : "playwright");
const output = process.env.ECHO_UI_OUTPUT || "/tmp/echo-gateway-ui";
await mkdir(output, { recursive: true });
const browser = await chromium.launch({
  headless: true,
  ...(process.env.ECHO_UI_BROWSER_PATH ? { executablePath: process.env.ECHO_UI_BROWSER_PATH } : {}),
});
const page = await browser.newPage({ locale: "en-US" });
// Real providers can take longer than Playwright's 30s action default. Keep
// controlled local runs fast while allowing an explicit real-model budget.
if (process.env.ECHO_UI_TIMEOUT_MS) page.setDefaultTimeout(Number(process.env.ECHO_UI_TIMEOUT_MS));
const network = await page.context().newCDPSession(page);
await network.send("Network.enable");
network.on("Network.webSocketWillSendHandshakeRequest", ({ request }) => {
  const headers = request.headers;
  console.log(JSON.stringify({ websocket_handshake: {
    origin: headers.Origin, host: headers.Host, fetch_site: headers["Sec-Fetch-Site"],
  } }));
});
const errors = [];
page.on("pageerror", (error) => errors.push(String(error)));
let original;
let apiPrefix;
try {
  const meta = await (await page.request.get(`${base}/meta`)).json();
  apiPrefix = meta.api_prefix;
  assert(meta.auth_required, "this test must exercise authentication");
  assert(meta.interactive_ws_path);
  await page.goto(`${base}/playground`);
  await page.getByLabel("API / Admin token").fill(token);
  const clientSessionKey = await page.getByLabel("Session key", { exact: true }).inputValue();
  assert(clientSessionKey.startsWith("cli:"), "default playground session must satisfy the loopback identity boundary");
  await page.getByLabel("Message", { exact: true }).fill("Reply exactly ECHO_UI_HTTP_OK. Do not use tools.");
  const httpDone = page.waitForResponse((response) => response.url().endsWith(`${apiPrefix}/message`));
  await page.getByRole("button", { name: "Send Message", exact: true }).click();
  const httpResponse = await httpDone;
  const http = await httpResponse.json();
  assert.equal(httpResponse.status(), 200, JSON.stringify(http));
  assert.equal(http.status, "completed");
  assert.equal(http.session_key, clientSessionKey);
  assert(http.reply?.text);
  await page.locator("#log .agent").last().waitFor();
  await page.getByRole("button", { name: "Connect WebSocket" }).click();
  await page.getByText("Connected", { exact: true }).waitFor();
  await page.getByText(`session: ${clientSessionKey}`, { exact: true }).waitFor();
  await page.getByLabel("Message", { exact: true }).fill("Reply exactly ECHO_UI_WS_OK. Do not use tools.");
  await page.getByRole("button", { name: "Send Message", exact: true }).click();
  await page.waitForFunction(() => document.querySelectorAll("#log .agent").length >= 2);
  await page.screenshot({ path: path.join(output, "fallback-auth-custom-paths.png"), fullPage: true });

  const headers = { Authorization: `Bearer ${token}` };
  const current = await (await page.request.get(`${base}${apiPrefix}/config`, { headers })).json();
  original = current.memory.enabled;
  await page.goto(`${base}/login`);
  await page.locator("#echo-token").fill(token);
  await page.getByRole("button", { name: "Log in" }).click();
  await page.waitForURL((url) => url.pathname !== "/login");
  await page.goto(`${base}/config`);
  await page.getByRole("button", { name: "Validate and save", exact: true }).waitFor();
  // Switches retain labels from surrounding HTML label elements.
  const memory = page.getByLabel("Cognitive memory", { exact: true });
  await memory.click();
  await page.getByRole("button", { name: "Validate and save", exact: true }).click();
  await page.getByRole("status").filter({ hasText: "Cognitive memory" }).waitFor();
  await page.reload();
  await page.getByRole("status").filter({ hasText: "Cognitive memory" }).waitFor();
  const saved = await (await page.request.get(`${base}${apiPrefix}/config`, { headers })).json();
  assert.equal(saved.memory.enabled, !original);
  assert.equal(saved._meta.running.memory.enabled, current._meta.running.memory.enabled);
  assert(saved._meta.pending_paths.includes("memory.enabled"));
  await page.screenshot({ path: path.join(output, "config-saved-running-after-refresh.png"), fullPage: true });
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ result: "passed", api_prefix: apiPrefix, interactive_ws_path: meta.interactive_ws_path,
    http_status: http.status, checks: ["authenticated fallback HTTP", "authenticated fallback WS", "config save", "runtime remains unchanged", "refresh retains pending state"], screenshots: output }));
} catch (error) {
  await page.screenshot({ path: path.join(output, "failure.png"), fullPage: true });
  console.error(await page.locator("body").innerText());
  throw error;
} finally {
  if (original !== undefined) {
    await page.request.patch(`${base}${apiPrefix}/config`, {
      headers: { Authorization: `Bearer ${token}` }, data: { changes: { "memory.enabled": original } },
    });
  }
  await browser.close();
}
