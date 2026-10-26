import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import { loadRuntimeSource } from "./app-manifest.mjs";

test("desktop registers every literal shared UI IPC command", () => {
  // 扫描作用域 = manifest 中 i18n 之后的 19 项运行时脚本拼接串（18 片段 + 薄 app.js），
  // 与原 app.js 全文逐字等价；i18n.js 实测 0 处 tauriInvoke(，不在扫描集内。
  const ui = loadRuntimeSource();
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
