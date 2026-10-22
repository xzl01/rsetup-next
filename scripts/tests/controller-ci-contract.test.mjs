// 中控 CI 配置合同：钉住离线测试目标 `controller-test-offline`、CI 中
// 「前端先构建、Rust 编译与资源测试后运行」的硬顺序，以及 dist 入库后的
// 新口径：编译类目标只检查 dist 在位，前端重建走显式 `make web-build`，
// CI 用 `git diff --exit-code -- apps/controller-web/dist` 比对入库产物。
//
// 这是纯配置断言：只读取仓库内的 Makefile、.gitignore 与 .github/workflows/ci.yml
// 文本，不执行 npm/cargo，也不接触任何数据库。真实双引擎（MySQL/TiDB）用例属操作员
// 逐引擎显式授权的 `--ignored` 轨道，不在本合同范围内。
//
// 为什么顺序仍是硬约束：`crates/rsetup-controller/src/web.rs` 用 include_str!/
// include_bytes! 在**编译期**嵌入 `apps/controller-web/dist` 的产物（缺 dist 直接
// 编译失败），`tests/web_assets.rs` 再读同一份磁盘字节作为独立预期源。dist 已随仓库
// 提交，所以 CI 里保留前端构建步骤的意义从「产出编译输入」变成「验证入库产物与源码
// 一致」；两条被编译的 controller 作业仍按原顺序先构建再编译。
import { test } from 'node:test';
import { readFileSync, existsSync } from 'node:fs';
import assert from 'node:assert/strict';
const make = readFileSync('Makefile', 'utf8');
const ci = readFileSync('.github/workflows/ci.yml', 'utf8');

// 按 `jobs:` 下两空格缩进的键切分作业块；整行注释不参与顺序判断。
// text 是剔除整行注释后的可执行文本，rawText 额外保留注释（供「连注释都不许
// 出现某字样」的排他断言使用，例如 Debian 作业不得再以任何形式引回前端构建）。
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
      jobs.push({ name: named[1], lines: [], rawLines: [raw] });
      continue;
    }
    jobs[jobs.length - 1]?.rawLines.push(raw);
    if (raw.trim().startsWith('#')) continue;
    jobs[jobs.length - 1]?.lines.push(raw);
  }
  return jobs.map((job) => ({
    name: job.name,
    lines: job.lines,
    text: job.lines.join('\n'),
    rawText: job.rawLines.join('\n'),
  }));
};

const jobs = parseJobs(ci);
const jobNamed = (needle) => jobs.find((job) => job.text.includes(needle));

// 会编译 rsetup-controller 的步骤：workspace 级 cargo 编译、显式 -p rsetup-controller，
// 以及等价入口（make test）。经 --manifest-path 指向其它 crate
// （如 apps/desktop/src-tauri）的命令不算。
// dpkg-buildpackage **不算**：本包的 debian/rules 用 dh_auto_test 的
// `--exclude rsetup-controller` 把 controller 排除在编译与测试之外，也不安装它的
// 二进制，所以 Debian 作业不需要前端 dist——把它算进来会逼着 CI 为一个不打包的
// crate 去跑 npm，正是本次移除的空转步骤。该排除本身由下面的 Debian 合同钉住。
const compilesController = (line) => {
  if (line.includes('--manifest-path')) return false;
  if (/(?:^|[^\w])make\s+test\b/.test(line)) return true;
  return /cargo\s+(?:test|check|clippy|build|rustc)\b/.test(line) || /-p\s+rsetup-controller\b/.test(line);
};

const BUILD_STEP = 'apps/controller-web run build';

// 提取一个 Makefile 目标：返回「头部行 + 其后所有 Tab 缩进的配方行」，遇到第一
// 条非配方、非注释、非空行（即下一条目标/伪目标）停止。配方行只保留 Tab 开头的
// 行，说明性注释不参与断言，避免注释里的字样冒充真正的构建步骤。
const extractTarget = (headerRe) => {
  const lines = make.split('\n');
  const start = lines.findIndex((line) => headerRe.test(line));
  if (start < 0) return undefined;
  const header = lines[start];
  const recipe = [];
  for (const line of lines.slice(start + 1)) {
    if (/^\t/.test(line)) {
      recipe.push(line.trim());
      continue;
    }
    if (line.trim() === '' || /^\s*#/.test(line)) continue;
    break;
  }
  return { header, recipe: recipe.join('\n') };
};

// 取 Makefile 里 `NAME := …` 的赋值块：含续行反斜杠的多行值会一并返回，
// 用于检查某个变量展开后的依赖集合（例如 $(WEB_SRCS) 是否覆盖 src 与 public）。
const assignment = (name) => {
  const lines = make.split('\n');
  const start = lines.findIndex((line) => new RegExp(`^${name}\\s*[:?+]?=`).test(line));
  if (start < 0) return '';
  const block = [lines[start]];
  for (let i = start; i < lines.length; i += 1) {
    if (!/\\\s*$/.test(lines[i])) break;
    block.push(lines[i + 1]);
  }
  return block.join('\n');
};

// 允许 $(NPM) 变量形式与裸 npm。
const NPM_CMD = String.raw`(?:\$\(NPM\)|\bnpm\b)`;

// 剥掉 make 的 $(fn …) 函数调用（含嵌套括号），以空格占位。核对依赖列表时必须先
// 去掉 $(shell find …)：它的参数里也带 src 路径，留着就能把「src 目录本身是不是
// 独立先决条件」这条断言喂饱。
const stripMakeFunction = (text, fn) => {
  const needle = `$(${fn}`;
  let out = '';
  let i = 0;
  while (i < text.length) {
    if (text.startsWith(needle, i) && /\s/.test(text[i + needle.length] ?? '')) {
      let depth = 0;
      for (; i < text.length; i += 1) {
        if (text[i] === '(') depth += 1;
        else if (text[i] === ')') {
          depth -= 1;
          if (depth === 0) {
            i += 1;
            break;
          }
        }
      }
      out += ' ';
      continue;
    }
    out += text[i];
    i += 1;
  }
  return out;
};

// 把 make 文本里的 $(大写变量) 递归展开成字面值（合并续行反斜杠），得到可逐项
// 核对的依赖闭包。函数调用（$(shell …)/$(wildcard …)）原样保留，由调用方决定是否剥离。
const expandVars = (text, depth = 0) => {
  if (depth > 5) return text;
  const expanded = text.replace(/\$\(([A-Z][A-Z0-9_]*)\)/g, (whole, name) => {
    const def = assignment(name);
    if (!def) return whole;
    return def.replace(new RegExp(`^${name}\\s*[:?+]?=`), '').replace(/\\\s*\n/g, ' ').trim();
  });
  return expanded !== text ? expandVars(expanded, depth + 1) : expanded;
};

// 依赖闭包的独立项集合：展开变量 → 剥掉 $(shell …) → 按空白与续行反斜杠切分。
const prereqTokens = (text) =>
  stripMakeFunction(expandVars(text), 'shell')
    .replace(/\\\s*/g, ' ')
    .split(/\s+/)
    .filter(Boolean);

test('controller offline target and CI build frontend before Rust assets', () => {
  // 意图不变：`--test web_assets` 读的是 apps/controller-web/dist 的磁盘字节，
  // 所以它运行前 dist 必须已经在位。dist 入库后这个「在位」不再由 npm 构建保证，
  // 而由 web-dist-check 的存在性检查保证；目标体内仍保留前端两条门禁。
  const offline = extractTarget(/^controller-test-offline:/);
  assert.ok(offline, 'missing controller-test-offline target');
  assert.match(offline.header, /\bweb-dist-check\b/, 'controller-test-offline 必须先决于 web-dist-check');
  assert.doesNotMatch(
    offline.header,
    /\bweb-assets\b/,
    'controller-test-offline 不得再自动触发 npm ci/run build：dist 已入库，重建属显式 make web-build',
  );
  // 目标体内仍需有前端门禁与 Rust 资源测试。
  for (const step of ['test -- --run', 'run typecheck', '--test web_assets']) {
    assert.ok(offline.recipe.includes(step), `controller-test-offline 配方缺少步骤：${step}`);
  }
  assert.doesNotMatch(
    offline.recipe,
    new RegExp(`${NPM_CMD}\\s+--prefix\\s+\\S+\\s+run build(\\s|$)`),
    'dist 入库后 controller-test-offline 不再自行构建前端，构建是 make web-build 的职责',
  );

  // 显式重建入口仍在：`make web-build` → web-assets → $(WEB_STAMP)，stamp 规则执行
  // npm run build 并 touch（允许 $(NPM) 变量形式）。锁定依赖的安装已拆到
  // $(WEB_DEPS_STAMP)，由下面「web stamps split the install from the dist build」
  // 那条测试钉住；这里反过来禁止 stamp 配方再跑 npm ci——那正是每次改组件都重装
  // 依赖的空转来源。
  const webBuild = extractTarget(/^web-build:/);
  assert.ok(webBuild, 'missing web-build target');
  assert.match(webBuild.header, /\bweb-assets\b/, 'web-build 必须依赖 web-assets');
  const webAssets = extractTarget(/^web-assets:/);
  assert.ok(webAssets, 'missing web-assets target');
  assert.match(webAssets.header, /\$\(WEB_STAMP\)/, 'web-assets 必须依赖 $(WEB_STAMP)');
  const stampRule = extractTarget(/^\$\(WEB_STAMP\):/);
  assert.ok(stampRule, 'missing $(WEB_STAMP) rule');
  assert.match(
    stampRule.recipe,
    new RegExp(`${NPM_CMD}\\s+--prefix\\s+\\S+\\s+run build(\\s|$)`),
    '$(WEB_STAMP) 规则必须执行 npm --prefix ... run build 产出 dist',
  );
  assert.match(stampRule.recipe, /\btouch\s+\$@/, '$(WEB_STAMP) 规则必须 touch stamp 文件');
  assert.doesNotMatch(
    stampRule.recipe,
    new RegExp(`${NPM_CMD}\\s+--prefix\\s+\\S+\\s+ci(\\s|$)`),
    '$(WEB_STAMP) 规则不得再执行 npm ci；锁定依赖安装只属于 $(WEB_DEPS_STAMP)',
  );

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
  // 数量口径：rust-msrv 作业用 1.85.0 跑 make msrv-check（默认构建仍是 stable），
  // 它依赖 dist 已入库而不在作业内构建前端；因此 compilesController 只统计
  // rust-and-ui 与 rust-multi-arch-check 两条显式 cargo 编译作业。msrv 作业由
  // 下方专门的合同测试单独钉住。
  assert.ok(
    compiling.length >= 2,
    `expected several jobs to compile the controller, got: ${compiling.map((job) => job.name).join(', ')}`,
  );
  assert.deepEqual(
    compiling.map((job) => job.name).sort(),
    ['rust-and-ui', 'rust-multi-arch-check'],
    '编译 rsetup-controller 的作业集合发生变化：每条编译作业都必须先构建 apps/controller-web dist',
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

// MSRV 作业保留，但只用于 1.85.0 验证；默认工具链仍是 stable，dist 已入库，
// 因此该作业不需要 setup-node/npm，也不在作业内构建前端。
test('rust-msrv job still verifies 1.85.0 with the committed dist', () => {
  const job = jobs.find((entry) => entry.name === 'rust-msrv');
  assert.ok(job, 'ci.yml 必须保留 rust-msrv 作业（MSRV 1.85 验证）');
  assert.match(job.text, /dtolnay\/rust-toolchain@1\.85\.0/, 'MSRV 作业必须显式安装 1.85.0');
  assert.match(job.text, /make msrv-check/, 'MSRV 作业必须运行 make msrv-check');
  assert.doesNotMatch(job.text, /\bnpm\b/, 'dist 已入库，MSRV 作业不得再执行 npm');
  assert.doesNotMatch(job.text, /actions\/setup-node/, 'dist 已入库，MSRV 作业不需要 setup-node');
});

// 架构口径：用户已明确项目只维护 64 位的 arm64 与 amd64。
//
// 为什么钉成「恰好这两个」而不是「至少包含这两个」：32 位目标（armv7、i686）
// 既没有对应的 Debian 发布产物，也没有运行时验证作业，加进矩阵只会让交叉检查
// 变慢并制造无人维护的编译错误，历史上已被反复误加，因此这里按集合精确钉住。
const MULTI_ARCH_TARGETS = ['aarch64-unknown-linux-gnu', 'x86_64-unknown-linux-gnu'];
// 已退出支持范围的 32 位目标关键字，任何一处出现都视为口径被破坏。
const RETIRED_ARCH_PATTERNS = [/armv7/, /armv6/, /arm-unknown-linux/, /arm-linux-gnueab/, /armhf/, /armel/, /i586/, /i686/];

// 取 rust-multi-arch-check 作业里 `target:` 下方的列表项（该作业是唯一按 Rust
// target 三元组展开矩阵的作业；Debian 作业用 include: 列 runner/架构对）。
const parseMatrixTargets = (jobText) => {
  const lines = jobText.split('\n');
  const start = lines.findIndex((line) => /^\s+target:\s*$/.test(line));
  assert.ok(start >= 0, 'rust-multi-arch-check 作业里找不到 matrix 的 target 列表');
  const targets = [];
  for (const line of lines.slice(start + 1)) {
    const item = /^\s*-\s*(\S+)\s*$/.exec(line);
    if (!item) break;
    targets.push(item[1]);
  }
  return targets;
};

test('multi-arch matrix is exactly the 64-bit arm64 and amd64 targets', () => {
  const job = jobs.find((item) => item.name === 'rust-multi-arch-check');
  assert.ok(job, 'ci.yml 里找不到 rust-multi-arch-check 作业');
  const targets = parseMatrixTargets(job.text);
  assert.deepEqual(
    [...targets].sort(),
    [...MULTI_ARCH_TARGETS].sort(),
    '多架构矩阵必须恰好是 aarch64-unknown-linux-gnu 与 x86_64-unknown-linux-gnu',
  );
  for (const target of targets) {
    for (const retired of RETIRED_ARCH_PATTERNS) {
      assert.doesNotMatch(target, retired, `${target} 属于已退出支持范围的 32 位目标`);
    }
  }
  // 整个 workflow 的可执行配置（剔除整行注释后）都不得出现 32 位目标关键字；
  // 注释行不参与判断，否则 ci.yml 里说明口径的注释会把自己判红。
  const ciCode = ci
    .split('\n')
    .filter((line) => !line.trim().startsWith('#'))
    .join('\n');
  for (const retired of RETIRED_ARCH_PATTERNS) {
    assert.doesNotMatch(ciCode, retired, 'ci.yml 的可执行配置不应出现 32 位目标关键字');
  }
});

// L1 修复的接线：Prometheus 指标线规范 §7.8 的五条 grep 静态门禁
// （Makefile 的 `metrics-static-gates` 目标）必须在 CI 里真实执行，否则门禁
// 只存在于文档与本地。该目标只读源码，不编译、不联网、不需要前端 dist，
// 因此它的正确位置是**任何 Rust 编译之前**——失败时立刻定位，不必等 clippy
// 或 cargo test 编完。这里只钉静态目标本身。
//
// 为什么同时禁止完整的 metrics-gates：Makefile 里 `metrics-gates` 依赖
// fmt-check + metrics-static-gates 后再跑 clippy 与两条 cargo test，把它接进
// 本作业只会把同一批编译与测试跑第二遍（并让「静态检查先于编译」失效）。
test('CI runs the metrics static gates before Rust compilation', () => {
  // Makefile 侧：目标必须以 .PHONY 声明，否则 CI 的调用可能撞上同名目录/文件而空转。
  assert.match(
    make,
    /^\.PHONY:.*\bmetrics-static-gates\b.*$/m,
    'Makefile 必须把 metrics-static-gates 声明为 .PHONY 目标',
  );

  // CI 侧：必须真的执行静态门禁目标。
  const job = jobs.find((item) => item.name === 'rust-and-ui');
  assert.ok(job, 'ci.yml 里找不到 rust-and-ui 作业');
  assert.match(
    job.text,
    /^[ \t]*run:[ \t]*make metrics-static-gates[ \t]*$/m,
    'rust-and-ui 作业必须有一步 run: make metrics-static-gates',
  );

  // 顺序：静态只读检查排在 fmt-check 之后、**该作业内任何 cargo 编译命令之前**。
  // 用文本索引比较；parseJobs 已剔除整行注释，注释里的字样不会冒充真正的步骤。
  // 不能只比 clippy：它后面还有 cargo test --workspace、--test web_assets、
  // externals/devicesinfo 与两条 --ignored 用例，任何一条排到静态门禁之前都会让
  // 「先失败先定位」失效，所以这里取全部 `cargo ` 调用的最小下标作比较对象。
  const fmtAt = job.text.indexOf('make fmt-check');
  const gatesAt = job.text.indexOf('make metrics-static-gates');
  const cargoPositions = [...job.text.matchAll(/\bcargo\s/g)].map((match) => match.index);
  assert.ok(fmtAt >= 0, 'rust-and-ui 作业必须保留 make fmt-check 这一步');
  assert.ok(
    cargoPositions.length >= 3,
    `rust-and-ui 作业应包含多条 cargo 命令，实际找到 ${cargoPositions.length} 条`,
  );
  assert.ok(
    fmtAt < gatesAt,
    'make metrics-static-gates 必须排在 make fmt-check 之后，保持静态检查聚拢',
  );
  const firstCargoAt = Math.min(...cargoPositions);
  assert.ok(
    gatesAt < firstCargoAt,
    'make metrics-static-gates 必须排在作业内所有 cargo 编译命令之前——纯只读门禁先于任何 Rust 编译',
  );

  // 排他：完整门禁目标不得进 CI（重复编译与测试）。ci.yml 的说明文字也不写
  // 这个字面命令，因此对整份文本断言即可，注释无需豁免。
  assert.doesNotMatch(
    ci,
    /make metrics-gates\b/,
    'CI 只允许调用 metrics-static-gates，不得调用会重复编译的完整 metrics 门禁目标',
  );

  // Makefile 侧的秘密/配置实体扫描：必须**两遍**都查。CI 是干净检出，暂存区恒为空，
  // 只有 `git diff --cached` 的那半条在流水线里等于空转（这就是它此前形同虚设的原因）；
  // `git ls-files` 扫已跟踪清单才让同一条规则在无 index 变更的 CI 里仍然成立。
  const staticTarget = extractTarget(/^metrics-static-gates:/);
  assert.ok(staticTarget, 'missing metrics-static-gates target');
  // 匹配 `$$(…)` 调用本身，不匹配 Fail-Closed 文案里的字样，否则注释与错误信息
  // 会把断言喂饱，删掉真实命令也照样绿。
  assert.match(
    staticTarget.recipe,
    /\$\$\(git diff --cached --name-only\)/,
    'metrics 门禁必须保留暂存区扫描（拦本地提交）',
  );
  assert.match(
    staticTarget.recipe,
    /\$\$\(git ls-files\)/,
    'metrics 门禁必须扫描已跟踪文件（CI 无 staged 改动，只查暂存区即空转）',
  );
  const trackedScan = staticTarget.recipe.match(/\$\$\(git ls-files\)[\s\S]*/);
  assert.ok(trackedScan, '已跟踪扫描缺少后续过滤');
  for (const needle of [String.raw`controller\.toml`, '^secret/']) {
    assert.ok(
      trackedScan[0].includes(needle),
      `metrics 门禁的已跟踪扫描必须在同一条管道里匹配 ${needle}`,
    );
  }
});

// Debian 作业与 controller 的边界：controller 已由 `rsetup-controller` 包交付，
// dh_auto_test 必须编译并测试完整 workspace，但**不**在 Debian 作业里构建前端——
// dist 已入库，源码包离线构建，既不装 npm 也不跑 vite。
//
// 断言分两层：作业的可执行文本（剔除整行注释）里不得出现任何 npm / setup-node，
// 连注释也不许出现前端目录字样——注释最容易被后来人照抄成真正的步骤。
test('debian package builds controller without a frontend build', () => {
  const debian = jobs.find((job) => job.name === 'debian-package');
  assert.ok(debian, 'ci.yml 里找不到 debian-package 作业');

  const rules = readFileSync('debian/rules', 'utf8');
  // 只认可执行行：debian/rules 的说明注释也可能提到 cargo test，光比对整份文本
  // 会让「注释还在、命令已被改坏」的回归蒙混过关。
  const rulesCode = rules
    .split('\n')
    .filter((line) => !line.trim().startsWith('#'))
    .join('\n');
  assert.match(
    rulesCode,
    /^[ \t]*cargo test --workspace[^\n]*--locked[^\n]*--offline[ \t]*$/m,
    'dh_auto_test 必须编译并测试完整 workspace（controller 已随包交付，不得再排除）',
  );
  assert.doesNotMatch(
    rulesCode,
    /(?:^|[ \t])npm(?![\w-])/,
    'Debian 构建不得依赖 npm：dist 已入库，源码包离线构建',
  );

  assert.doesNotMatch(
    debian.text,
    /\bnpm\b/,
    'debian-package 作业不得调用 npm：dist 已入库，无需前端构建',
  );
  assert.doesNotMatch(
    debian.text,
    /actions\/setup-node/,
    'debian-package 作业不需要 setup-node：没有前端构建步骤',
  );
  assert.doesNotMatch(
    debian.rawText,
    /apps\/controller-web/,
    'debian-package 作业连注释都不应引用前端目录，避免被照抄成新的构建步骤',
  );
  assert.doesNotMatch(
    debian.rawText,
    /npm --prefix/,
    'debian-package 作业不得残留前端依赖安装的命令字样',
  );
});

// 前端产物的两级 stamp：锁定依赖安装（node_modules）与 dist 构建分开打戳。
// 此前单个 stamp 同时吃 package.json 与整个 src 树，任一组件改动都会重新 `npm ci`；
// 拆开后改组件只重跑 `vite build`，改锁文件才重装依赖。dist 侧依赖还要覆盖 src 目录
// 本身（新增/删除文件的 mtime 落在目录上）与 Vite 默认 publicDir（当前不存在，用
// $(wildcard) 展开为空）。测试文件不属于 dist 输入，改 spec 不该触发前端重建。
test('web stamps split the locked install from the dist build', () => {
  assert.match(
    make,
    /^WEB_DEPS_STAMP\s*:=\s*\$\(WEB_DIR\)\/node_modules\/\.rsetup-npm-ci-stamp$/m,
    '依赖 stamp 必须落在 node_modules 下，独立于 dist 的 stamp',
  );
  assert.match(make, /^NPM_CI_FLAGS \?=/m, '必须提供可为空的 NPM_CI_FLAGS，离线时用 --offline');

  const deps = extractTarget(/^\$\(WEB_DEPS_STAMP\):/);
  assert.ok(deps, 'missing $(WEB_DEPS_STAMP) rule');
  assert.match(
    deps.header,
    /\$\(WEB_DIR\)\/package\.json/,
    '$(WEB_DEPS_STAMP) 必须先决于 package.json',
  );
  assert.match(
    deps.header,
    /\$\(WEB_DIR\)\/package-lock\.json/,
    '$(WEB_DEPS_STAMP) 必须先决于 package-lock.json',
  );
  assert.doesNotMatch(
    deps.header,
    /\$\(WEB_SRCS\)|\/src\b/,
    '$(WEB_DEPS_STAMP) 只依赖锁文件，否则改组件又会重装依赖',
  );
  assert.match(
    deps.recipe,
    new RegExp(`${NPM_CMD}\\s+--prefix\\s+\\S+\\s+ci\\s`),
    '$(WEB_DEPS_STAMP) 规则必须执行 npm --prefix ... ci 安装锁定依赖',
  );
  assert.match(
    deps.recipe,
    /\$\(NPM_CI_FLAGS\)/,
    'npm ci 必须透传 $(NPM_CI_FLAGS)，否则离线环境无法传 --offline',
  );
  assert.match(deps.recipe, /\btouch\s+\$@/, '$(WEB_DEPS_STAMP) 规则必须 touch stamp 文件');

  const stamp = extractTarget(/^\$\(WEB_STAMP\):/);
  assert.ok(stamp, 'missing $(WEB_STAMP) rule');
  assert.ok(
    /\$\(WEB_SRCS\)/.test(stamp.header) || /\$\(WEB_DEPS_STAMP\)/.test(stamp.header),
    '$(WEB_STAMP) 必须依赖 $(WEB_SRCS) 或 $(WEB_DEPS_STAMP)',
  );
  // 依赖闭包逐项核对：先展开 $(WEB_SRCS) 等变量，再剥掉 $(shell find …) 并按项切分，
  // 才能区分「src 目录本身是一项」与「src 只是 find 的参数」。public 允许以
  // $(wildcard …) 形式出现——目录不存在时展开为空正是当前状态。
  const webDir = expandVars('$(WEB_DIR)');
  const closureSource = `${stamp.header}\n$(WEB_SRCS)`;
  const closure = expandVars(closureSource);
  const prereqs = prereqTokens(closureSource);
  assert.ok(
    prereqs.includes(`${webDir}/src`),
    'dist 侧依赖必须把 src 目录本身列为独立先决条件（新增/删除文件才会让 stamp 失效）',
  );
  assert.match(
    closure,
    /apps\/controller-web\/public/,
    'dist 侧依赖必须覆盖 Vite 的 publicDir（允许 $(wildcard …) 展开为空）',
  );
  for (const entry of ['index.html', 'vite.config.ts']) {
    assert.ok(
      prereqs.includes(`${webDir}/${entry}`),
      `dist 侧依赖必须包含 ${entry}`,
    );
  }
  assert.match(
    assignment('WEB_SRCS'),
    /!\s*-name\s+'?\*\.test\.ts'?/,
    '测试文件不是 vite build 的输入，改 spec 不得触发前端重建',
  );

  // 编译 workspace 的入口目标只先决于 dist 的**存在性检查**，不再自动触发 npm：
  // dist 已入库，干净检出与离线机器跑 `make build` 不该需要 Node 与网络。
  // 五个入口目标就是 Makefile 里会编译 rsetup-controller 的全部入口（最低版本门禁
  // 已随「不锁定 Rust 版本」的口径一并删除，不在其列）。
  for (const name of ['build', 'test', 'lint', 'metrics-gates', 'msrv-check', 'controller-test-offline']) {
    const target = extractTarget(new RegExp(`^${name}:`));
    assert.ok(target, `missing ${name} target`);
    assert.match(
      target.header,
      /\bweb-dist-check\b/,
      `${name} 会编译 rsetup-controller，必须先决于 web-dist-check`,
    );
    assert.doesNotMatch(
      target.header,
      /\bweb-assets\b/,
      `${name} 不得再依赖 web-assets：前端重建是 make web-build 的显式职责`,
    );
  }
});

test('debian package declares only the amd64 and arm64 architectures', () => {
  const control = readFileSync('debian/control', 'utf8');
  const matches = [...control.matchAll(/^Architecture:[ \t]*(\S[^\n]*)$/gm)].map((m) => m[1].trim());
  assert.ok(matches.length >= 1, 'debian/control 至少应有一个 Architecture 字段');
  for (const match of matches) {
    const declared = match.split(/\s+/).sort();
    assert.deepEqual(
      declared,
      ['amd64', 'arm64'],
      'debian/control 的每个 Architecture 字段都只能声明 amd64/arm64（用户既定口径）',
    );
  }
});

// dist 入库是这一轮的口径变更：`apps/controller-web/dist` 从「本地忽略产物」变成
// 交付物，`crates/rsetup-controller/build.rs` 编译期直接读它，干净检出与离线构建
// 因此不再需要 Node/npm/网络。代价是一条纪律——改前端源码必须重新 `make web-build`
// 并把 dist 一起提交，由 CI 的 dist 比对（下一条测试）兜底。
// 这里只读文本与磁盘存在性，不执行 git/npm/cargo：合同测试自己不能变成需要网络的步骤。
test('controller web dist is committed while only the make stamp stays ignored', () => {
  const webIgnore = readFileSync('apps/controller-web/.gitignore', 'utf8');
  const rootIgnore = readFileSync('.gitignore', 'utf8');
  const ruleLines = webIgnore
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line !== '' && !line.startsWith('#'));

  // 裸 dist 忽略规则一律禁止：`dist`、`dist/`、`/dist`、`**/dist/` 都会把三个交付
  // 文件重新变成本地产物，干净检出立刻回到「不装 Node 就编不出 controller」。
  const BARE_DIST_RULE = /^\/?(?:\*\*\/)?dist\/?$/;
  const bare = ruleLines.filter((line) => BARE_DIST_RULE.test(line));
  assert.deepEqual(bare, [], 'apps/controller-web/.gitignore 不得再忽略整个 dist 目录');

  // 唯一允许被忽略的 dist 内文件是 make 的时间戳：它每次 web-build 都刷新，入库只会
  // 制造无意义 diff。写全相对路径，不接受 `*.rsetup-build-stamp` 这类通配放宽。
  assert.ok(
    ruleLines.includes('dist/.rsetup-build-stamp'),
    'apps/controller-web/.gitignore 必须继续忽略 dist/.rsetup-build-stamp',
  );
  for (const kept of ['node_modules/', 'coverage/', '.npm-cache/']) {
    assert.ok(ruleLines.includes(kept), `apps/controller-web/.gitignore 必须保留 ${kept} 忽略项`);
  }

  // 上层也不能把交付物重新忽略掉：根 .gitignore 的锚定规则优先级足够造成同样的后果。
  const rootRules = rootIgnore
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line !== '' && !line.startsWith('#'));
  assert.equal(
    rootRules.find((line) => /^\/?apps\/controller-web\/dist(\/.*)?$/.test(line) && !line.startsWith('!')),
    undefined,
    '根 .gitignore 不得忽略 apps/controller-web/dist',
  );

  // 交付物确实在工作区里（入库后 CI 与本地都必然满足）：index.html 是 build.rs 的
  // 硬输入，assets 目录是哈希资产的家。
  assert.ok(existsSync('apps/controller-web/dist/index.html'), 'dist/index.html 必须存在');
  assert.ok(existsSync('apps/controller-web/dist/assets'), 'dist/assets 目录必须存在');

  // Makefile 侧：dist 缺失要在 cargo 编译前用可读错误拦下，且该检查绝不碰 npm。
  assert.match(
    make,
    /^\.PHONY:.*\bweb-dist-check\b.*$/m,
    'Makefile 必须把 web-dist-check 声明为 .PHONY 目标',
  );
  const distCheck = extractTarget(/^web-dist-check:/);
  assert.ok(distCheck, 'missing web-dist-check target');
  const expanded = expandVars(distCheck.recipe);
  assert.match(
    expanded,
    /test -f \S*apps\/controller-web\/dist\/index\.html/,
    'web-dist-check 必须用 test -f 检查 apps/controller-web/dist/index.html',
  );
  assert.match(
    distCheck.recipe,
    /make web-build/,
    'web-dist-check 的报错必须直接指向 make web-build',
  );
  assert.doesNotMatch(
    distCheck.recipe,
    new RegExp(NPM_CMD),
    'web-dist-check 只检查产物存在，不得执行 npm',
  );
  // 显式重建入口必须仍然落到装依赖 + 构建的完整链路（配方的其余部分由第一条测试钉住）。
  assert.match(
    make,
    /^\.PHONY:.*\bweb-build\b.*$/m,
    'Makefile 必须把 web-build 声明为 .PHONY 目标',
  );
});

// dist 入库的一致性由 CI 兜底：从源码重新构建一遍，再与入库产物比对。vite 构建是
// 确定性的（重复构建字节一致），所以「源码 + 已提交 dist」不自洽时 diff 必然非空。
test('CI compares the committed dist with a frontend build from source', () => {
  const job = jobs.find((item) => item.name === 'rust-and-ui');
  assert.ok(job, 'ci.yml 里找不到 rust-and-ui 作业');

  // 验证源码的四条前端门禁仍要在位：dist 入库不等于跳过前端测试与类型检查。
  for (const step of [
    'npm --prefix apps/controller-web ci',
    'npm --prefix apps/controller-web test -- --run',
    'npm --prefix apps/controller-web run typecheck',
    BUILD_STEP,
  ]) {
    assert.ok(job.text.includes(step), `rust-and-ui 作业必须保留前端步骤：${step}`);
  }

  // 比对步骤：必须存在、必须排在构建之后，否则比的是入库产物自己。
  const DIFF = 'git diff --exit-code -- apps/controller-web/dist';
  const buildAt = job.text.indexOf(BUILD_STEP);
  const diffAt = job.text.indexOf(DIFF);
  assert.ok(diffAt >= 0, `rust-and-ui 作业必须有 ${DIFF}，否则 dist 与源码脱钩无人发现`);
  assert.ok(buildAt >= 0 && buildAt < diffAt, 'dist 比对必须排在 npm run build 之后');
  assert.match(
    job.text,
    /name: Verify committed dist matches the frontend build/,
    'dist 比对必须是独立可见的步骤名，便于失败定位',
  );
  // 退出码必须能传出去：多行 run 块要 set -eu，且 --exit-code 不能被管道吃掉。
  const diffBlockStart = job.text.lastIndexOf('run: |', diffAt);
  const diffBlock = job.text.slice(diffBlockStart, diffAt);
  assert.match(diffBlock, /set -eu/, 'dist 比对的多行 run 块必须 set -eu，否则非零退出被吞掉');
  assert.doesNotMatch(
    job.text.slice(diffAt, diffAt + DIFF.length),
    /\|/,
    'dist 比对命令不得接管道，否则退出码来自管道末端',
  );
});
