import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

test("desktop registers every literal shared UI IPC command", () => {
  const ui = fs.readFileSync(new URL("./app.js", import.meta.url), "utf8");
  const rust = fs.readFileSync(new URL(
    "../apps/desktop/src-tauri/src/main.rs", import.meta.url,
  ), "utf8");
  const blocks = [...rust.matchAll(/generate_handler!\s*\[([\s\S]*?)\]/g)];
  assert.equal(blocks.length, 1, "production and tests must share one registry");
  const registered = new Set(blocks[0][1].match(/\b[a-z_][a-z_0-9]*\b/g));
  const calls = [...ui.matchAll(/tauriInvoke\(\s*["']([^"']+)["']/g)];
  assert.ok(calls.some((match) => match[1] === "storage_status"));
  for (const [, name] of calls) assert.ok(registered.has(name), name);
});
