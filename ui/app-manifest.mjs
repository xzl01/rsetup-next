// ui/app.js 运行时脚本清单的唯一权威来源（规格 §7）。
// 仅测试使用（test-only）：非 *.test.mjs，不被 node --test 的 ui/*.test.mjs glob 拾取。
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

// 路径规范式（§7，唯一定义）：每项为相对 ui/ 的 POSIX 路径、无前导斜杠；
// URL ≡ "/" + manifestPath；HTML src 必须逐字等于该 URL。
// 20 项有序 = HTML 顺序 = 拼接顺序 = 装载顺序（§6.2）：i18n.js 首位、18 个运行时片段居中、薄 app.js 末位。
export const RUNTIME_SCRIPTS = [
  "i18n.js",
  "modules/app-const.js",
  "modules/app-state.js",
  "modules/app-transport.js",
  "modules/app-format.js",
  "modules/app-debug.js",
  "modules/app-refresh.js",
  "modules/app-render.js",
  "modules/app-sources.js",
  "modules/app-hardware.js",
  "modules/app-overlay.js",
  "modules/app-spi.js",
  "modules/app-gpio-video.js",
  "modules/app-thermal.js",
  "modules/app-led.js",
  "modules/app-storage.js",
  "modules/app-install.js",
  "modules/app-actions.js",
  "modules/app-control.js",
  "app.js",
];

const uiRoot = path.dirname(fileURLToPath(import.meta.url));

// i18n 之后的 19 项（18 片段 + 薄 app.js）的 [相对路径, 文本] 有序数组（§8），
// 供 DOM harness 与 IPC 扫描逐文件使用。
export function loadRuntimeParts() {
  return RUNTIME_SCRIPTS.slice(1).map((relativePath) => [
    relativePath,
    fs.readFileSync(path.join(uiRoot, relativePath), "utf8"),
  ]);
}

// 按序读取 i18n 之后的 19 项并以 "\n" 拼接；故意不含 i18n.js，
// 使拼接串始终是「原 app.js 的等价物」（§8：hardening 全量断言作用域逐字不变）。
export function loadRuntimeSource() {
  return loadRuntimeParts().map(([, text]) => text).join("\n");
}

// 既有六处测试 handler 的逐字等价实现（§8），作用于拼接串：
// 自函数声明起，截到下一个顶层 function/async function 之前；其后无函数则取到串尾。
export function handler(source, name) {
  const start = source.search(new RegExp(`(?:async )?function ${name}\\(`));
  assert.ok(start >= 0, `function ${name} not found`);
  const rest = source.slice(start);
  const next = rest.slice(1).search(/\n(?:async )?function /);
  return next < 0 ? rest : rest.slice(0, next + 1);
}
