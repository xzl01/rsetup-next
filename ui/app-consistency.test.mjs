// ui/app.js 拆分一致性测试（规格 §8）。两组断言、按批次启用：
//   - 静态守恒组 (b1)–(b4)：批次 1 提交即入库并生效（本文件当前全部内容）。
//     批次 1 作用域 = manifest 中间 18 项（18 片段）；薄 app.js 尚不存在，
//     `init` 及其顶层调用此时仍在旧 ui/app.js（3,928 行，仍是生产文件）内，
//     故 (b3) 直接以旧 app.js 为基准，不留副本；片段声明全集 200 名，
//     并入旧 app.js 的 `init` 后恢复 §2/§5 的 201 名全集（首项 i18n.js 只做
//     存在性/可解析性检查，不参与声明唯一性集合）。
//   - 接线组 (a)(c)(d)：批次 2 cutover 提交内启用，批次 1 不写入本文件。
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { RUNTIME_SCRIPTS, loadRuntimeParts } from "./app-manifest.mjs";

const uiRoot = path.dirname(fileURLToPath(import.meta.url));

// 装载序（§6.2）中 i18n 与薄 app.js 之间的 18 个运行时片段，顺序即拼接/装载顺序。
const FRAGMENTS = RUNTIME_SCRIPTS.slice(1, -1);

// §5 归属表的逐字行区间基线（[file, from, to]，1-based 闭区间，行号为拆分前
// 旧 app.js 实测区间）。同一文件的多段按 §4.2 片段文件内顺序排列（render 为
// renderAll → renderSnapshot → applyStaticTranslations，即 W1/W2 位移后的片段内序）。
// init（3914–3928）批次 1 仍留在旧 app.js 内，不在段表中。
const SEGMENTS = [
  ["modules/app-const.js", 1, 130],
  ["modules/app-state.js", 132, 203],
  ["modules/app-transport.js", 205, 436],
  ["modules/app-hardware.js", 438, 440],
  ["modules/app-format.js", 442, 547],
  ["modules/app-debug.js", 549, 658],
  ["modules/app-refresh.js", 670, 748],
  ["modules/app-render.js", 750, 758],
  ["modules/app-sources.js", 760, 996],
  ["modules/app-render.js", 998, 1092],
  ["modules/app-render.js", 660, 668],
  ["modules/app-hardware.js", 1094, 1227],
  ["modules/app-overlay.js", 1229, 1458],
  ["modules/app-spi.js", 1460, 1635],
  ["modules/app-gpio-video.js", 1637, 1751],
  ["modules/app-thermal.js", 1753, 2168],
  ["modules/app-led.js", 2170, 2408],
  ["modules/app-storage.js", 2410, 2694],
  ["modules/app-storage.js", 2696, 2857],
  ["modules/app-install.js", 2859, 2873],
  ["modules/app-install.js", 2875, 3571],
  ["modules/app-actions.js", 3573, 3693],
  ["modules/app-control.js", 3695, 3912],
];

// 旧 app.js 中作为「单元起点」的行：201 个顶层声明行 + install 审计块起点
// （§5：审计块 2859–2873 整块是 app-install 首单元）。
const UNIT_START = /^const |^let |^var |^function |^async function /;

// 逐行解析顶层声明（行首 const/let/var/function/async function），返回 [{kind, name}]。
// 顶层 `let`/`var` 以 kind="other" 返回，供计数断言排除（§2：实测无顶层 let/var）。
function topLevelDecls(text) {
  const decls = [];
  for (const line of text.split("\n")) {
    let match = /^(?:async )?function ([\w$]+)/.exec(line);
    if (match) {
      decls.push({ kind: "function", name: match[1] });
      continue;
    }
    match = /^const ([\w$]+)\s*=/.exec(line);
    if (match) {
      decls.push({ kind: "const", name: match[1] });
      continue;
    }
    match = /^const \{ ([\w$]+) \}/.exec(line);
    if (match) {
      decls.push({ kind: "const", name: match[1] });
      continue;
    }
    if (/^let |^var /.test(line)) decls.push({ kind: "other", name: line });
  }
  return decls;
}

test("(b1) 静态守恒：RUNTIME_SCRIPTS 20 项结构与 §6.2 装载序一致，路径存在且逐文件 node --check 可解析", () => {
  assert.equal(RUNTIME_SCRIPTS[0], "i18n.js");
  assert.equal(RUNTIME_SCRIPTS.at(-1), "app.js");
  assert.deepEqual(FRAGMENTS.map((p) => path.basename(p)), [
    "app-const.js", "app-state.js", "app-transport.js", "app-format.js",
    "app-debug.js", "app-refresh.js", "app-render.js", "app-sources.js",
    "app-hardware.js", "app-overlay.js", "app-spi.js", "app-gpio-video.js",
    "app-thermal.js", "app-led.js", "app-storage.js", "app-install.js",
    "app-actions.js", "app-control.js",
  ]);
  assert.equal(RUNTIME_SCRIPTS.length, 20);
  for (const relativePath of RUNTIME_SCRIPTS) {
    assert.match(relativePath, /^(?:i18n\.js|app\.js|modules\/app-[a-z-]+\.js)$/);
    assert.ok(fs.existsSync(path.join(uiRoot, relativePath)), relativePath);
    execFileSync(process.execPath, ["--check", path.join(uiRoot, relativePath)]);
  }
});

test("(b3) 静态守恒：逐单元字节恒等 + 唯一归属——每个片段逐字节等于 §5 段表在旧 app.js 中的原行区间", () => {
  const oldText = fs.readFileSync(path.join(uiRoot, "app.js"), "utf8");
  const oldLines = oldText.split("\n");
  assert.equal(oldLines.at(-1), "", "app.js 必须以换行结尾");
  assert.equal(oldLines.length - 1, 3928, "批次 1 基准 = 3,928 行的旧 app.js（漂移即失败，需按 §4.2 重新核对）");
  assert.equal(oldLines[3927], "init();");

  // 旧文件中未被段表覆盖的非空行只允许是 init 区间（3914–3928）。
  const owner = new Map();
  for (const [, from, to] of SEGMENTS) {
    for (let line = from; line <= to; line += 1) {
      assert.ok(!owner.has(line), `行 ${line} 被多个段认领（唯一归属违例）`);
      owner.set(line, true);
    }
  }
  for (let line = 1; line <= 3928; line += 1) {
    if (!owner.has(line) && oldLines[line - 1].trim() !== "") {
      assert.ok(line >= 3914 && line <= 3928, `行 ${line} 未被段表覆盖：${oldLines[line - 1].slice(0, 60)}`);
    }
  }
  // 段起点必须落在单元起点上（顶层声明行或审计块 2859），段尾不得是空行——
  // 即没有单元被段边界拆断，也没有单元整体缺失。
  for (const [file, from, to] of SEGMENTS) {
    const isUnitStart = UNIT_START.test(oldLines[from - 1]) || (from === 2859 && oldLines[from - 1].startsWith("// ---"));
    assert.ok(isUnitStart, `${file} 段 ${from}-${to} 起点不是单元起点：${oldLines[from - 1].slice(0, 60)}`);
    assert.notEqual(oldLines[to - 1].trim(), "", `${file} 段 ${from}-${to} 尾行是空行`);
  }

  // 片段字节 = 所承接段的原文按 §4.2 段内顺序拼接、段间隙恰一个空行；
  // 文件头尾零新增、零删除文本（§3.3 规则 i）。
  const parts = new Map(loadRuntimeParts().slice(0, 18));
  const byFile = new Map();
  for (const [file, from, to] of SEGMENTS) {
    if (!byFile.has(file)) byFile.set(file, []);
    byFile.get(file).push(oldLines.slice(from - 1, to).join("\n"));
  }
  assert.deepEqual([...byFile.keys()].sort(), [...FRAGMENTS].sort());
  for (const [file, segTexts] of byFile) {
    const expected = `${segTexts.join("\n\n")}\n`;
    assert.equal(parts.get(file), expected, `${file} 与原行区间字节不恒等`);
  }
});

test("(b2) 静态守恒：顶层绑定跨文件零重复；片段 200 名（19 const + 181 function）+ 旧 app.js 的 init = 201 名全集", () => {
  const parts = loadRuntimeParts().slice(0, 18);
  const seen = new Map();
  let constCount = 0;
  let functionCount = 0;
  for (const [file, text] of parts) {
    for (const { kind, name } of topLevelDecls(text)) {
      assert.notEqual(kind, "other", `发现计划外的顶层 let/var 声明：${file} ${name}`);
      if (kind === "const") constCount += 1;
      else functionCount += 1;
      const key = `${kind}:${name}`;
      assert.ok(!seen.has(key), `顶层绑定 ${name} 重复声明：${seen.get(key) ?? ""} 与 ${file}`);
      seen.set(key, file);
    }
  }
  assert.equal(constCount, 19, "§2/§5：const 全集 19 名（12 常量 + i18n + t + state + $ + $$ + tauriInvoke + transport）全部在片段内");
  assert.equal(functionCount, 200 - 19, "片段内顶层 function 应为 181 个（182 − init）");
  assert.equal(seen.size, 200, "批次 1 片段声明全集应为 200 名");

  // 旧 app.js（仍是生产文件）贡献且仅贡献 `init`；并入后恢复 201 名全集。
  const oldText = fs.readFileSync(path.join(uiRoot, "app.js"), "utf8");
  const oldDecls = topLevelDecls(oldText);
  assert.equal(oldDecls.length, 201, "§2：旧 app.js 顶层声明共 201 个");
  const fragmentNames = new Set([...seen.keys()]);
  const oldOnly = oldDecls.filter(({ kind, name }) => !fragmentNames.has(`${kind}:${name}`));
  assert.deepEqual(oldOnly, [{ kind: "function", name: "init" }], "旧 app.js 此时只应额外声明 init");
  const oldConsts = oldDecls.filter(({ kind }) => kind === "const").length;
  assert.equal(oldConsts, 19);
  assert.equal(oldDecls.length - oldConsts, 182);
});

test("(b4) 静态守恒：18 片段拼接串的单元序列 = 原行号序 + 仅 W1–W3 三处位移", () => {
  const oldText = fs.readFileSync(path.join(uiRoot, "app.js"), "utf8");
  const oldSeq = topLevelDecls(oldText).map(({ name }) => name);
  assert.equal(oldSeq.length, 201);

  const moveBefore = (seq, name, before) => {
    const rest = seq.filter((n) => n !== name);
    assert.equal(rest.length, seq.length - 1, `位移单元 ${name} 应恰出现一次`);
    const index = rest.indexOf(before);
    assert.ok(index >= 0, `位移锚点 ${before} 不存在`);
    rest.splice(index, 0, name);
    return rest;
  };
  // 白名单恰三处（§4.2）：W1 applyStaticTranslations→app-render（renderSnapshot 后、
  // app-sources 首单元 providerName 前）；W2 renderSnapshot→app-render（renderAll 后）；
  // W3 hardwareReason→app-hardware 首单元（bindHardwareButtons 前）。
  let expected = moveBefore(oldSeq, "applyStaticTranslations", "providerName");
  expected = moveBefore(expected, "renderSnapshot", "applyStaticTranslations");
  expected = moveBefore(expected, "hardwareReason", "bindHardwareButtons");
  expected = expected.filter((n) => n !== "init"); // init 留在旧 app.js，批次 2 才入拼接串

  const concatenated = loadRuntimeParts().slice(0, 18).map(([, text]) => text).join("\n");
  const actual = topLevelDecls(concatenated).map(({ name }) => name);
  assert.deepEqual(actual, expected, "拼接串单元序列 ≠ 原行号序 + W1–W3（顺序/归属/白名单被锁定）");
});
