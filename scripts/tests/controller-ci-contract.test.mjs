// 中控 CI 配置合同：钉住离线测试目标 `controller-test-offline` 与 CI 中
// 「前端先构建、Rust 编译与资源测试后运行」的硬顺序。
//
// 这是纯配置断言：只读取仓库内的 Makefile 与 .github/workflows/ci.yml 文本，
// 不执行 npm/cargo，也不接触任何数据库。真实双引擎（MySQL/TiDB）用例属操作员
// 逐引擎显式授权的 `--ignored` 轨道，不在本合同范围内。
//
// 为什么顺序是硬约束：`crates/rsetup-controller/src/web.rs` 用 include_str!/
// include_bytes! 在**编译期**嵌入 `apps/controller-web/dist` 的产物（缺 dist 直接
// 编译失败），`tests/web_assets.rs` 再读同一份磁盘字节作为独立预期源。因此任何
// 编译 rsetup-controller 的 CI 作业都必须先构建前端，而不只是 web_assets 那一步。
import { test } from 'node:test';
import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';
const make = readFileSync('Makefile', 'utf8');
const ci = readFileSync('.github/workflows/ci.yml', 'utf8');

// 按 `jobs:` 下两空格缩进的键切分作业块；整行注释不参与顺序判断。
const parseJobs = (text) => {
  const jobs = [];
  let inJobs = false;
  for (const raw of text.split('\n')) {
    if (/^jobs:\s*$/.test(raw)) {
      inJobs = true;
      continue;
    }
    if (inJobs && /^[A-Za-z][^\n]*:\s*$/.test(raw)) inJobs = false;
    if (!inJobs) continue;
    const named = /^  ([A-Za-z0-9_-]+):\s*$/.exec(raw);
    if (named) {
      jobs.push({ name: named[1], lines: [] });
      continue;
    }
    if (raw.trim().startsWith('#')) continue;
    jobs[jobs.length - 1]?.lines.push(raw);
  }
  return jobs.map((job) => ({ name: job.name, lines: job.lines, text: job.lines.join('\n') }));
};

const jobs = parseJobs(ci);
const jobNamed = (needle) => jobs.find((job) => job.text.includes(needle));

// 会编译 rsetup-controller 的步骤：workspace 级 cargo 编译、显式 -p rsetup-controller，
// 以及等价入口（make test / make msrv-check / dpkg-buildpackage 的 dh_auto_*）。
// 经 --manifest-path 指向其它 crate（如 apps/desktop/src-tauri）的命令不算。
const compilesController = (line) => {
  if (line.includes('--manifest-path')) return false;
  if (/(?:^|[^\w])make\s+(?:test|msrv-check)\b/.test(line)) return true;
  if (/\bdpkg-buildpackage\b/.test(line)) return true;
  return /cargo\s+(?:test|check|clippy|build|rustc)\b/.test(line) || /-p\s+rsetup-controller\b/.test(line);
};

const BUILD_STEP = 'apps/controller-web run build';

test('controller offline target and CI build frontend before Rust assets', () => {
  const target = make.split(/^controller-test-offline:/m)[1]?.split(/^\S[^\n]*:/m)[0];
  assert.ok(target, 'missing controller-test-offline target');
  assert.match(target, /apps\/controller-web/);
  for (const step of ['test -- --run', 'run typecheck', 'run build']) assert.ok(target.includes(step));
  assert.ok(target.indexOf('run build') < target.indexOf('--test web_assets'));
  assert.match(ci, /apps\/controller-web/);
  assert.match(ci, /--test web_assets/);
});

test('controller-test-offline is declared as a phony target', () => {
  assert.match(make, /^\.PHONY:.*\bcontroller-test-offline\b.*$/m);
});

test('CI runs the locked web build in the same job before the web_assets test', () => {
  const job = jobNamed('--test web_assets');
  assert.ok(job, 'no CI job runs cargo test --test web_assets');
  const buildAt = job.text.indexOf(BUILD_STEP);
  const assetsAt = job.text.indexOf('--test web_assets');
  const installAt = job.text.search(/npm --prefix apps\/controller-web ci\b/);
  assert.ok(buildAt >= 0, 'the web_assets job must build apps/controller-web first');
  assert.ok(installAt >= 0, 'the web_assets job must install locked dependencies with npm ci');
  assert.ok(installAt < buildAt, 'npm ci must run before the frontend build');
  assert.ok(buildAt < assetsAt, 'the frontend build must run before --test web_assets');
});

test('every CI job that compiles rsetup-controller builds the web dist first', () => {
  const compiling = jobs.filter((job) => job.lines.some(compilesController));
  assert.ok(
    compiling.length >= 3,
    `expected several jobs to compile the controller, got: ${compiling.map((job) => job.name).join(', ')}`,
  );
  for (const job of compiling) {
    const buildLine = job.lines.findIndex((line) => line.includes(BUILD_STEP));
    const compileLine = job.lines.findIndex(compilesController);
    assert.ok(
      buildLine >= 0,
      `job ${job.name} compiles rsetup-controller but never builds apps/controller-web dist`,
    );
    assert.ok(
      buildLine < compileLine,
      `job ${job.name} must build apps/controller-web dist before compiling rsetup-controller`,
    );
  }
});
