// Organization resource pages now use persistent navigation and a compact
// account bar. The illustrated introduction belongs to the overview, so exact
// geometry parity with the project hero is no longer the design contract.
// Keep the existing entry point on the current behavioral and visual checks.
process.argv.push("--extended");
await import("./validate-organization-workspace-ui.mjs");
