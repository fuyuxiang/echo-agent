import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import ts from "typescript";
import { expect, it } from "vitest";

function sources(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return entry.name === "__tests__" ? [] : sources(path);
    return /\.[jt]sx?$/.test(path) && !/\.(?:test|spec)\./.test(path) ? [path] : [];
  });
}

// This scans the complete production tree, including on slower native runners.
it("every static frontend Tauri command is registered by the production host", { timeout: 30_000 }, () => {
  const host = readFileSync("src-tauri/src/lib.rs", "utf8");
  const handler = host.match(/\.invoke_handler\(tauri::generate_handler!\[([\s\S]*?)^\s*\]\)/m)?.[1];
  expect(handler, "production command handler must be found").toBeDefined();
  const commands = [...handler.matchAll(/^\s*(?:\w+::)*(\w+),\s*$/gm)].map(match => match[1]);
  expect(new Set(commands).size, "duplicate command names shadow IPC endpoints").toBe(commands.length);
  const registered = new Set(commands);
  const missing = [];
  let calls = 0;
  for (const file of sources("src")) {
    const source = ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest, false);
    const invokes = new Set();
    for (const statement of source.statements) {
      if (!ts.isImportDeclaration(statement) || statement.moduleSpecifier.text !== "@tauri-apps/api/core") continue;
      const bindings = statement.importClause?.namedBindings;
      if (bindings && ts.isNamedImports(bindings)) {
        for (const binding of bindings.elements) {
          if ((binding.propertyName ?? binding.name).text === "invoke") invokes.add(binding.name.text);
        }
      }
    }
    if (invokes.size === 0) continue;
    function visit(node) {
      if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && invokes.has(node.expression.text)
          && node.arguments[0] && ts.isStringLiteralLike(node.arguments[0])) {
        calls++;
        const command = node.arguments[0].text;
        if (!registered.has(command)) missing.push(`${file}: ${command}`);
      }
      ts.forEachChild(node, visit);
    }
    visit(source);
  }
  expect(calls, "guard must inspect actual IPC calls").toBeGreaterThan(0);
  expect(missing).toEqual([]);
});
