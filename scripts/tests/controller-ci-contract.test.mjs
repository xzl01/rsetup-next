// 中控 CI 配置合同：钉住离线测试目标 `controller-test-offline`、CI 与 Debian 打包中
// 「前端先构建、Rust 编译与资源测试后运行」的硬顺序，以及 dist 改为**生成物**后的口径：
//
//   * `apps/controller-web/dist` 不入库（既不被 git 跟踪，也被 .gitignore 忽略）；
//   * 每条编译 rsetup-controller 的路径——make 目标、CI 作业、dpkg-buildpackage——
//     都必须先把前端构建出来；
//   * 构建产物的文件名与 hash 由 vite 决定，任何构建配置里都不许写死具体名字。
//
// 这是纯配置断言：只读取仓库内的 Makefile、.gitignore、debian/control、debian/rules、
// 相关文档与 .github/workflows/ci.yml 文本，外加两条**只读**的 git 查询
// （`ls-files` 与 `check-ignore`）确认 dist 的入库状态。不执行 npm/cargo，不联网，
// 也不接触任何数据库；真实双引擎（MySQL/TiDB）用例属操作员逐引擎显式授权的
// `--ignored` 轨道，不在本合同范围内。
//
// 为什么顺序是硬约束：`crates/rsetup-controller/src/web.rs` 用 include_str!/
// include_bytes! 在**编译期**嵌入 `apps/controller-web/dist` 的产物（缺 dist 直接
// 编译失败关闭），`tests/web_assets.rs` 再读同一份磁盘字节作为独立预期源。dist 不再
// 随仓库提供之后，「先构建前端」从 CI 的额外保险变回编译的必要前提，所以每一条编译
// controller 的作业都要为它负责。
import { test } from 'node:test';
import { execFileSync, spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import assert from 'node:assert/strict';

const make = readFileSync('Makefile', 'utf8');
const ci = readFileSync('.github/workflows/ci.yml', 'utf8');
const rules = readFileSync('debian/rules', 'utf8');
const WEB_DIST = 'apps/controller-web/dist';

// 按 `jobs:` 下两空格缩进的键切分作业块；整行注释不参与顺序判断。
// text 是剔除整行注释后的可执行文本，rawText 额外保留注释（供「某字样必须出现在含注释
// 的作业块里」这类断言使用）。
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
const jobCalled = (name) => jobs.find((job) => job.name === name);

// 会编译 rsetup-controller 的步骤：workspace 级 cargo 编译、显式 -p rsetup-controller、
// 等价入口（make test），以及 **dpkg-buildpackage**——本源码包的 debian/rules 会用
// cargo 编译并打包 controller 的三个二进制，所以它同样是「编译 controller」的路径，
// 必须为前端产物负责。经 --manifest-path 指向其它 crate
// （如 apps/desktop/src-tauri）的命令不算。
const compilesController = (line) => {
  if (line.includes('--manifest-path')) return false;
  if (/dpkg-buildpackage\b/.test(line)) return true;
  if (/(?:^|[^\w])make\s+test\b/.test(line)) return true;
  return /cargo\s+(?:test|check|clippy|build|rustc)\b/.test(line) || /-p\s+rsetup-controller\b/.test(line);
};

// dpkg-buildpackage 入口单独留一个谓词：它的构建配方在 debian/rules 里，
// 「前端先构建」只能在那里核对。
const buildsDebianPackage = (line) => /dpkg-buildpackage\b/.test(line);

const BUILD_STEP = 'apps/controller-web run build';
const NPM_CI_STEP = 'npm --prefix apps/controller-web ci';

// 剥掉 recipe 里以 # 开头的行（含 make 配方内 tab 缩进的 shell 注释），只留真命令。
const stripComments = (text) =>
  text
    .split('\n')
    .filter((line) => !line.trim().startsWith('#'))
    .join('\n');

const rulesCode = stripComments(rules);

// debian/control 字段解析：字段名起于行首，续行以空白缩进；返回去掉字段名后的原值。
const controlField = (control, field) => {
  const lines = control.split('\n');
  const start = lines.findIndex((line) => new RegExp(`^${field}:`).test(line));
  assert.ok(start >= 0, `debian/control 缺少 ${field} 字段`);
  const block = [lines[start].replace(new RegExp(`^${field}:`), '')];
  for (const line of lines.slice(start + 1)) {
    if (!/^[ \t]/.test(line)) break;
    block.push(line);
  }
  return block.join(',');
};

// debian/rules 是 Debian 构建真正的配方所在：npm 装依赖与 vite build 必须成对出现，
// 且都排在第一条 cargo 之前，否则 build.rs 读不到 dist 就把整包编译打断。
const assertRulesBuildFrontendBeforeCargo = (source) => {
  const code = stripComments(source);
  const ciAt = code.search(/^[ \t]*npm --prefix apps\/controller-web ci[ \t]*$/m);
  const buildAt = code.search(/^[ \t]*npm --prefix apps\/controller-web run build[ \t]*$/m);
  const cargoAt = code.search(/^[ \t]*cargo\b/m);
  assert.ok(ciAt >= 0, 'debian/rules 必须执行 npm --prefix apps/controller-web ci 安装锁定依赖');
  assert.ok(buildAt >= 0, 'debian/rules 必须执行 npm --prefix apps/controller-web run build 生成 dist');
  assert.ok(cargoAt >= 0, 'debian/rules 必须真的有 cargo 编译命令');
  assert.ok(ciAt < buildAt, 'debian/rules 里 npm ci 必须早于 npm run build');
  assert.ok(
    buildAt < cargoAt,
    'debian/rules 里前端构建必须早于第一条 cargo 编译命令：build.rs 编译期读取 apps/controller-web/dist',
  );
};

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

// 允许 $(MAKE) 变量形式与裸 make（web-dist-check 递归调用自己的兄弟目标）。
const MAKE_CMD = String.raw`(?:\$\$?\(MAKE\)|\bmake\b)`;

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

// 只读的 git 查询：确认 dist 的入库状态。两条命令都不写任何东西。
const gitOutput = (args) => execFileSync('git', args, { encoding: 'utf8' });
const gitIgnores = (path) => {
  try {
    execFileSync('git', ['check-ignore', '-q', path], {
      stdio: ['ignore', 'ignore', 'ignore'],
    });
    return true;
  } catch (err) {
    // 退出码 1 = 明确「未被忽略」，是要断言的正常分支；其余（128 等）是工具出错，
    // 必须原样抛出：本测试在这一点上 Fail-Closed，绝不把 git 故障当成通过。
    if (err && err.status === 1) return false;
    throw err;
  }
};

test('controller offline target and CI build frontend before Rust assets', () => {
  // 意图不变：`--test web_assets` 读的是 apps/controller-web/dist 的磁盘字节，所以它
  // 运行前 dist 必须已经在位。dist 不入库之后，这个「在位」由 web-dist-check 保证：
  // 产物缺失时它自己 `$(MAKE) web-build` 构建（见下一条测试），目标体内则保留前端
  // 自身的两条门禁。
  const offline = extractTarget(/^controller-test-offline:/);
  assert.ok(offline, 'missing controller-test-offline target');
  assert.match(offline.header, /\bweb-dist-check\b/, 'controller-test-offline 必须先决于 web-dist-check');
  assert.doesNotMatch(
    offline.header,
    /\bweb-assets\b/,
    'controller-test-offline 不直接依赖 web-assets：构建走 web-dist-check 的按需分支',
  );
  // 目标体内仍需有前端门禁与 Rust 资源测试。
  for (const step of ['test -- --run', 'run typecheck', '--test web_assets']) {
    assert.ok(offline.recipe.includes(step), `controller-test-offline 配方缺少步骤：${step}`);
  }
  // 构建只在 web-dist-check 里发生一次；目标体内再手写一条 `npm run build` 会变成
  // 「无论 dist 是否新鲜都重建」，正是本次要消除的重复。
  assert.doesNotMatch(
    offline.recipe,
    new RegExp(`${NPM_CMD}\\s+--prefix\\s+\\S+\\s+run build(\\s|$)`),
    '前端构建由 web-dist-check 按需触发，controller-test-offline 不再自行执行 npm run build',
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

// dist 是生成物：编译类目标的先决条件必须能在**干净检出**上把产物造出来，否则
// 「不入库」就等于「干净检出编不过」。web-dist-check 因此是条件式的——缺失即自动
// `$(MAKE) web-build`，在位则一条 npm 都不跑。
test('web-dist-check builds the frontend when dist is missing', () => {
  assert.match(
    make,
    /^\.PHONY:.*\bweb-dist-check\b.*$/m,
    'Makefile 必须把 web-dist-check 声明为 .PHONY 目标',
  );
  assert.match(
    make,
    /^\.PHONY:.*\bweb-build\b.*$/m,
    'Makefile 必须把 web-build 声明为 .PHONY 目标',
  );
  const distCheck = extractTarget(/^web-dist-check:/);
  assert.ok(distCheck, 'missing web-dist-check target');
  const expanded = expandVars(distCheck.recipe);
  assert.match(
    expanded,
    /test -f \S*apps\/controller-web\/dist\/index\.html/,
    'web-dist-check 必须用 test -f 检查 apps/controller-web/dist/index.html',
  );
  // 自动构建：必须真的递归调用 web-build（允许 $(MAKE) 形式），且文案告诉操作员
  // 「将自动构建」，而不是让人再去手工补一条命令。
  assert.match(
    distCheck.recipe,
    new RegExp(`${MAKE_CMD}\\s+web-build(\\s|$|;)`),
    'dist 缺失时 web-dist-check 必须自动执行 make web-build',
  );
  assert.match(
    distCheck.recipe,
    /将自动构建|自动构建/,
    'web-dist-check 的提示必须说明将自动构建前端',
  );
  // 条件式而非无条件：`$(MAKE) web-build` 必须落在 if/else 的「缺失」分支里，
  // 否则每次 make build 都重装依赖 + 重建前端。
  const ifAt = distCheck.recipe.search(/\bif test -f\b/);
  const elseAt = distCheck.recipe.search(/\belse\b/);
  const buildAt = distCheck.recipe.search(new RegExp(`${MAKE_CMD}\\s+web-build`));
  const fiAt = distCheck.recipe.search(/(?:^|[\s;])fi\b/);
  assert.ok(
    ifAt >= 0 && elseAt > ifAt && buildAt > elseAt && fiAt > buildAt,
    '自动构建必须写在「dist 不存在」分支内（if test -f … else … fi），在位时不得重复构建',
  );
  // 构建之后仍要有终局存在性检查：npm 失败但退出码被吞时，也不能让 cargo 裸奔。
  const finalCheck = distCheck.recipe.lastIndexOf('test -f');
  assert.ok(finalCheck > buildAt, '自动构建之后必须再确认一次 dist/index.html 在位');
});

test('controller-test-offline is declared as a phony target', () => {
  assert.match(make, /^\.PHONY:.*\bcontroller-test-offline\b.*$/m);
});

// rust-and-ui：前端四条门禁按「安装 → 测试 → 类型检查 → 构建」执行，构建必须排在
// 读磁盘字节的 `--test web_assets` 之前。dist 入库时代的「与提交产物比对」一步已随
// 之删除——现在构建是编译输入的生产步骤，不是校验步骤。
test('CI runs the locked web build in the same job before the web_assets test', () => {
  const job = jobNamed('--test web_assets');
  assert.ok(job, 'no CI job runs cargo test --test web_assets');
  const buildAt = job.text.indexOf(BUILD_STEP);
  const assetsAt = job.text.indexOf('--test web_assets');
  const installAt = job.text.indexOf(NPM_CI_STEP);
  assert.ok(installAt >= 0, 'the web_assets job must install locked dependencies with npm ci');
  assert.ok(buildAt >= 0, 'the web_assets job must build apps/controller-web first');
  assert.ok(installAt < buildAt, 'npm ci must run before the frontend build');
  assert.ok(buildAt < assetsAt, 'the frontend build must run before --test web_assets');

  // 入库比对已删除：产物不入库之后没有任何「提交产物」可比，留着只会让流水线在
  // 一个不存在的前提上判红。
  assert.doesNotMatch(
    ci,
    /git diff --exit-code -- apps\/controller-web\/dist/,
    'dist 已不入库，CI 不得再与提交产物比对',
  );
  assert.doesNotMatch(
    ci,
    /Verify committed dist/,
    'CI 不得再保留「校验入库 dist」的步骤',
  );
});

// 每条编译 rsetup-controller 的 CI 路径都必须先有 dist。编译入口有三类：
// 显式 cargo 编译（rust-and-ui、rust-multi-arch-check），以及走
// dpkg-buildpackage 的 debian-package——后者的配方在 debian/rules 里，
// 显式的 npm 步骤是双保险。
test('every CI job that compiles rsetup-controller builds the web dist first', () => {
  const compiling = jobs.filter((job) => job.lines.some(compilesController));
  assert.deepEqual(
    compiling.map((job) => job.name).sort(),
    ['debian-package', 'rust-and-ui', 'rust-multi-arch-check'],
    '编译 rsetup-controller 的作业集合发生变化：每条编译作业都必须先构建 apps/controller-web dist',
  );
  assertRulesBuildFrontendBeforeCargo(rules);
  for (const job of compiling) {
    const buildLine = job.lines.findIndex((line) => line.includes(BUILD_STEP));
    const compileLine = job.lines.findIndex(compilesController);
    const compiledByDpkg = job.lines.some(buildsDebianPackage);
    if (buildLine < 0) {
      // 没有显式前端步骤时，唯一合法的解释是「由 debian/rules 构建」：该作业必须是
      // dpkg-buildpackage 入口，且 rules 里的前端构建排在 cargo 之前（上面已断言）。
      assert.ok(
        compiledByDpkg,
        `job ${job.name} compiles rsetup-controller but never builds apps/controller-web dist`,
      );
      continue;
    }
    assert.ok(
      buildLine < compileLine,
      `job ${job.name} must build apps/controller-web dist before compiling rsetup-controller`,
    );
  }
  // dpkg-buildpackage 入口单独点名：漏掉它就是「Debian 构建又不装前端了」的回归。
  const debian = jobCalled('debian-package');
  assert.ok(
    debian && debian.lines.some(buildsDebianPackage),
    'debian-package 作业必须仍在 dpkg-buildpackage 里编译 controller',
  );
});

// 工具链口径：本仓不锁定 Rust 最低版本（没有 rust-version，也没有 msrv-check 目标），
// 所有 Rust 作业统一用 dtolnay/rust-toolchain@stable。这里钉住「不再引回某个具体
// 小版本」：CI 与 Makefile 里出现 1.85 之类的版本作业，或出现非 stable 的工具链
// 安装，都会让本条判红。
test('every Rust job uses the stable toolchain; no MSRV pin left', () => {
  const toolchainLines = ci
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => /uses:[ \t]*dtolnay\/rust-toolchain/.test(line));
  assert.ok(toolchainLines.length >= 3, 'CI 里应有多条 dtolnay/rust-toolchain 安装步骤');
  for (const line of toolchainLines) {
    assert.match(
      line,
      /dtolnay\/rust-toolchain@stable\b/,
      `Rust 工具链必须统一为 stable，实际为：${line}`,
    );
  }
  for (const [name, text] of [
    ['ci.yml', ci],
    ['Makefile', make],
  ]) {
    assert.doesNotMatch(
      text,
      /\bmsrv\b|msrv-check|rust-msrv/,
      `${name} 不应再出现 MSRV/最低版本作业：已统一使用 stable 工具链`,
    );
    assert.doesNotMatch(text, /\b1\.85\b/, `${name} 不应再钉住 Rust 1.85`);
  }
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
  const job = jobCalled('rust-multi-arch-check');
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
  const job = jobCalled('rust-and-ui');
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
    `rust-and-ui 作业应包含多条 cargo 编译命令，实际找到 ${cargoPositions.length} 条`,
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

// Debian 侧的接线：`rsetup-controller` 由本源码包编译交付，而它编译期读取的
// apps/controller-web/dist 不再随源码包提供，所以 debian/rules 必须在 cargo 之前
// 用 npm 生成它，npm 也必须进 Build-Depends。CI 的 debian-package 作业另外显式跑
// 一遍同样的两条命令（双保险），并在 apt 安装清单里带上 npm。
test('debian package builds the frontend before cargo', () => {
  const debian = jobCalled('debian-package');
  assert.ok(debian, 'ci.yml 里找不到 debian-package 作业');

  // 配方层：npm ci → npm run build → 第一条 cargo。
  assertRulesBuildFrontendBeforeCargo(rules);
  // dh_auto_test 仍跑完整 workspace（controller 已随包交付，不得再排除）。
  assert.match(
    rulesCode,
    /^[ \t]*cargo test --workspace[^\n]*--locked[^\n]*--offline[ \t]*$/m,
    'dh_auto_test 必须编译并测试完整 workspace（controller 已随包交付，不得再排除）',
  );

  // 构建依赖层：nodejs 与 npm 都必须是 Build-Depends——前者喂 dh_auto_test 的
  // `node --test ui/*.test.mjs`，后者喂 override_dh_auto_build 的前端构建。
  const control = readFileSync('debian/control', 'utf8');
  const buildDeps = controlField(control, 'Build-Depends')
    .split(',')
    .map((item) => item.trim().replace(/\s*\(.*\)$/, ''))
    .filter(Boolean);
  for (const dep of ['nodejs', 'npm']) {
    assert.ok(
      buildDeps.includes(dep),
      `debian/control 的 Build-Depends 必须包含 ${dep}，实际：${buildDeps.join(', ')}`,
    );
  }

  // 作业层：dpkg-buildpackage 之前既有 npm 的安装清单，也有显式的前端构建步骤。
  const dpkgAt = debian.text.search(/dpkg-buildpackage\b/);
  const installAt = debian.text.indexOf(NPM_CI_STEP);
  const buildAt = debian.text.indexOf(BUILD_STEP);
  assert.ok(dpkgAt >= 0, 'debian-package 作业必须仍用 dpkg-buildpackage 构建');
  assert.ok(installAt >= 0, 'debian-package 作业必须显式安装前端锁定依赖（npm ci）');
  assert.ok(buildAt >= 0, 'debian-package 作业必须显式构建前端（npm run build）');
  assert.ok(installAt < buildAt, '前端依赖安装必须早于前端构建');
  assert.ok(
    buildAt < dpkgAt,
    '前端构建必须早于 dpkg-buildpackage：显式步骤是 rules 里同一条链路的双保险',
  );
  assert.match(
    debian.text,
    /apt-get install[\s\S]*?\bnodejs\b[\s\S]*?\bnpm\b/,
    'debian-package 作业的 apt 安装清单必须同时带上 nodejs 与 npm（容器里没有预装）',
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

  // 编译 workspace 的入口目标都先决于 web-dist-check：dist 不入库之后，该先决条件是
  // 「缺失即自动构建、在位则只确认一次」的那道门（见
  // web-dist-check builds the frontend when dist is missing）。它们不直接依赖
  // web-assets，否则热工作区每次 make build 都会重新走一遍 npm 依赖安装判断。
  for (const name of ['build', 'test', 'lint', 'metrics-gates', 'controller-test-offline']) {
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
      `${name} 不得直接依赖 web-assets：按需构建统一走 web-dist-check`,
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

// dist 的入库状态：`apps/controller-web/dist` 是 vite 的生成物，**既不入库也被忽略**。
// 这一条同时防两个方向的回归：把产物重新提交回仓库（固定哈希产物再次漂移），以及
// 忽略规则被删掉（干净检出后本地构建一次就让 dist 变成一大片 untracked 噪声）。
test('controller web dist is generated, untracked and ignored', () => {
  const webIgnore = readFileSync('apps/controller-web/.gitignore', 'utf8');
  const rootIgnore = readFileSync('.gitignore', 'utf8');
  const ruleLines = webIgnore
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line !== '' && !line.startsWith('#'));

  // 必须有一条整目录的 dist 忽略规则（`dist`、`dist/`、`/dist`、`**/dist/` 之一）。
  const BARE_DIST_RULE = /^\/?(?:\*\*\/)?dist\/?$/;
  assert.ok(
    ruleLines.some((line) => BARE_DIST_RULE.test(line)),
    'apps/controller-web/.gitignore 必须忽略整个 dist 目录（dist/），产物不入库',
  );
  for (const kept of ['node_modules/', 'coverage/', '.npm-cache/']) {
    assert.ok(ruleLines.includes(kept), `apps/controller-web/.gitignore 必须保留 ${kept} 忽略项`);
  }

  // 根 .gitignore 既不得再为 dist 放行（`!…/dist` 重包含），也不得再加一条与
  // 子目录规则相互覆盖的 dist 规则——两处规则同时存在时，谁生效取决于目录顺序。
  const rootRules = rootIgnore
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line !== '' && !line.startsWith('#'));
  const conflicting = rootRules.filter((line) =>
    /^!?\/?apps\/controller-web\/dist(\/.*)?$/.test(line),
  );
  assert.deepEqual(conflicting, [], '根 .gitignore 不得再为 apps/controller-web/dist 写忽略或重包含规则');

  // 磁盘真相：git 没有跟踪 dist 里的任何文件，且这些路径确实被忽略。
  const tracked = gitOutput(['ls-files', '--', '.'])
    .split('\n')
    .filter((path) => path.startsWith(`${WEB_DIST}/`));
  assert.deepEqual(tracked, [], `apps/controller-web/dist 不得有文件被 git 跟踪，实际：${tracked.join(', ')}`);
  for (const probe of [`${WEB_DIST}/index.html`, `${WEB_DIST}/assets/probe.js`]) {
    assert.ok(gitIgnores(probe), `${probe} 必须被 .gitignore 忽略`);
  }

  // Debian 的构建目录同样要忽略：新增 rsetup-board-agent 包后，打包会多出
  // debian/rsetup-board-agent 这棵 staging 树。
  const debianDirs = ['debian/rsetup-next', 'debian/rsetup-controller', 'debian/rsetup-board-agent'];
  for (const dir of debianDirs) {
    assert.ok(
      rootRules.some((line) => line === `/${dir}` || line === `/${dir}/`),
      `根 .gitignore 必须忽略打包 staging 目录 /${dir}`,
    );
    assert.ok(gitIgnores(dir), `${dir} 必须被 .gitignore 忽略`);
  }
});

// vite 给产物加内容 hash，名字随源码变化。任何构建配置里写死 `assets/index-XXXX.js`
// 之类的名字，都会在下次前端改动后变成静默的坏链接或直接 404，所以这里一次扫尽
// 构建与文档侧的全部相关文件（合成名只允许出现在 Rust 测试 fixture 里）。
const ASSET_HASH_PATTERN = /assets\/index-[A-Za-z0-9_-]{4,}\.(?:js|css)/;
test('no build configuration or doc hardcodes a generated asset file name', () => {
  const scanned = [
    'Makefile',
    '.gitignore',
    'apps/controller-web/.gitignore',
    'debian/control',
    'debian/rules',
    'debian/README.source',
    '.github/workflows/ci.yml',
    'README.md',
    'README.zh.md',
    'apps/controller-web/README.md',
    'apps/controller-web/DEV-PROXY.md',
    'docs/testing/controller-v1-known-gaps.md',
  ];
  for (const path of scanned) {
    const text = readFileSync(path, 'utf8');
    const hits = text.split('\n').filter((line) => ASSET_HASH_PATTERN.test(line));
    assert.deepEqual(hits, [], `${path} 不得写死 vite 生成的资产文件名：${hits.join(' | ')}`);
  }
  // 构建脚本按目录扫描资产，不认具体名字（这条同时钉住「运行时按 index.html 引用解析」
  // 的实现方式，改名不会让嵌入逻辑失配）。
  const buildScript = readFileSync('crates/rsetup-controller/build/web_assets.rs', 'utf8');
  assert.doesNotMatch(
    buildScript,
    ASSET_HASH_PATTERN,
    'build 侧资产表不得写死某个 hash 的文件名',
  );
});

// ---------------------------------------------------------------------------
// systemd 打包合同：构建工具下限 + 维护脚本断言的意图守门人
// ---------------------------------------------------------------------------
//
// 事实依据（debhelper 官方 changelog，
// https://metadata.ftp-master.debian.org/changelogs/main/d/debhelper/unstable_changelog ，
// 已由维护者独立核对）：
//   * 13.11.6 条目 "Support units below /usr"：dh_installsystemd 从该版本起才解析
//     /usr/lib/systemd/system 下的单元。本源码包三个包的单元**全部**装在 /usr 下，
//     所以构建工具链必须有下限，否则旧 helper「没看见单元」生成的空脚本会伪装成功。
//   * 13.25 条目 "Always stop the service on remove (#1079181)"：≥13.25 的生成脚本在
//     prerm 的 remove 分支总会 stop。产物侧断言必须区分动作与生命周期分支，不能把
//     任何 stop 都当成安装/升级违规（scripts/tests/systemd-maintainer-audit.test.mjs
//     钉住审计脚本本身的行为）。

// 提取一个作业内某个 step 的正文：从 `- name: <needle>` 行起到下一个同级 step 边界，
// 输入必须是 parseJobs 已剔除注释的 text，注释不得替 run 脚本喂饱断言。
const extractStepBlock = (jobText, needle) => {
  const lines = jobText.split('\n');
  const start = lines.findIndex((line) => /^\s*- name:/.test(line) && line.includes(needle));
  assert.ok(start >= 0, `ci.yml 作业里找不到名为 ${needle} 的步骤`);
  const block = [lines[start]];
  for (const line of lines.slice(start + 1)) {
    if (/^\s*- (name|uses|with|run|env):/.test(line) && !/^\s{8,}\S/.test(line)) break;
    block.push(line);
  }
  return block.join('\n');
};

// 构建工具下限：debhelper (>= 13.11.6~) 与 debhelper-compat (= 13) 并存。
// 这是**构建前**的门禁（dpkg-checkbuilddeps 在任何构建规则运行前失败），不是 Rust
// 版本锁定；用 -d 绕过依赖检查后再宣称打包通过，恰好是该下限要防止的假阳性。
test('debian/control pins the debhelper build-tool floor for /usr unit discovery', () => {
  const control = readFileSync('debian/control', 'utf8');
  const items = controlField(control, 'Build-Depends')
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean);
  const debhelper = items.find((item) => /^debhelper\s*\(/.test(item));
  assert.ok(
    debhelper,
    `Build-Depends 必须有一条显式带版本约束的 debhelper（13.11.6 起才解析 /usr 下单元），实际：${items.join(', ')}`,
  );
  assert.match(
    debhelper,
    /\(\s*>=\s*13\.11\.6~\s*\)/,
    `debhelper 的版本下限必须是 >= 13.11.6~（~ 允许上游补丁号），实际：${debhelper}`,
  );
  assert.ok(
    items.some((item) => /^debhelper-compat\s*\(\s*=\s*13\s*\)$/.test(item)),
    'debhelper-compat (= 13) 必须保留：版本下限是工具门禁，compat 等级另行声明',
  );
});

// rules 侧的生命周期口径（CI 在产物侧另有核对，这里是源码守门人）：
//   * 三包都有 --no-start（安装/升级不 start/restart）与 --no-stop-on-upgrade
//     （升级不 stop）；
//   * controller 与 board-agent 有 --no-enable；
//   * rsetup-next **不得**带 --no-enable：热策略/风扇曲线/LED 单元按默认口径随包
//     enable，这是板卡下次重启后热安全的前提。
test('dh_installsystemd lifecycle options are explicit per package', () => {
  const byPkg = {};
  for (const match of rulesCode.matchAll(/^[ \t]*dh_installsystemd -p(\S+)(.*)$/gm)) {
    byPkg[match[1]] = match[2];
  }
  for (const pkg of ['rsetup-next', 'rsetup-controller', 'rsetup-board-agent']) {
    assert.ok(byPkg[pkg], `debian/rules 必须显式调用 dh_installsystemd -p${pkg}`);
    assert.match(byPkg[pkg], /--no-start/, `${pkg} 必须带 --no-start`);
    assert.match(byPkg[pkg], /--no-stop-on-upgrade/, `${pkg} 必须带 --no-stop-on-upgrade`);
  }
  for (const pkg of ['rsetup-controller', 'rsetup-board-agent']) {
    assert.match(byPkg[pkg], /--no-enable/, `${pkg} 必须带 --no-enable`);
  }
  assert.doesNotMatch(
    byPkg['rsetup-next'],
    /--no-enable/,
    'rsetup-next 不得带 --no-enable：三个硬件策略单元必须随包 enable',
  );
});

// 产物侧断言的**机制**合同：CI 只用严格模板门禁，不再留任何「grep 字样」或旧启发式
// 入口。为什么禁止 grep 字样：真实产物里 `--no-enable` 包的 postinst 同样含
// `deb-systemd-helper enable` 字样（整块在 debian-installed 守卫内），而「有没有字样」
// 也证明不了单元、动作与守卫的关系；模板门禁把这三件事一次性钉住（行为合同见
// scripts/tests/systemd-maintainer-audit.test.mjs）。
test('CI asserts maintainer scripts only through the strict template gate', () => {
  const debian = jobCalled('debian-package');
  assert.ok(debian, 'ci.yml 里找不到 debian-package 作业');
  const step = extractStepBlock(debian.text, 'Assert Debian systemd maintainer scripts');
  assert.match(
    step,
    /scripts\/lib\/systemd-maintainer-audit\.mjs/,
    'CI 步骤必须调用 scripts/lib/systemd-maintainer-audit.mjs 审计解包后的维护脚本',
  );
  // 逐包 × 逐脚本：三包各 postinst/postrm，共 6 条调用。
  assert.equal(
    gateCalls(step).length,
    6,
    '必须恰好 6 条门禁调用（3 包 × postinst/postrm），逐包逐脚本显式传参',
  );
  // 旧口径一律不得回潮：启发式 CLI、字样 grep、以及「任何 deb-systemd-invoke 即失败」
  // 的一刀切（debhelper 若在 prerm 生成 remove 守卫下的 stop，一刀切会把合法产物判红）。
  for (const forbidden of [
    '--enable-guard',
    '--enable-guard-unit',
    '--require-active-enable',
    '--require-active-enable-unit',
    '--allow-unobserved-template',
  ]) {
    assert.ok(
      !step.includes(forbidden),
      `旧或非生产用的选项 ${forbidden} 不得出现在 CI 步骤里`,
    );
  }
  assert.doesNotMatch(
    step,
    /grep[^\n]*(deb-systemd-helper|deb-systemd-invoke|_dh_action)/,
    '维护脚本的 systemd 断言不得再用 grep 字样，必须走模板门禁',
  );
  assert.doesNotMatch(
    step,
    /deb-systemd-invoke\|_dh_action=/,
    '不得保留「任何 deb-systemd-invoke 或 _dh_action 一律失败」的一刀切断言',
  );
  // 「安装/升级不 stop」的结构体现：13.24.2 下三包都**不得生成** preinst/prerm。
  const loops = findLoops(step, 'kind').filter((loop) =>
    loop.body.includes('[ -e "$tmp/$pkg/$kind" ]'));
  assert.equal(
    loops.length,
    1,
    '必须有一条按 kind 检查 preinst/prerm 是否存在的循环（存在即失败）',
  );
  assert.deepEqual([...loops[0].list].sort(), ['preinst', 'prerm']);
});

// Bookworm 安装后的实证断言不得被删：rsetup-next 三个单元的 enable 符号链接必须
// 存在（deb-systemd-helper 直接写链接，不受 policy-rc.d=101 影响），controller 与
// board-agent 必须继续没有链接。
test('Bookworm install step asserts the enable symlinks per unit', () => {
  const runtime = jobCalled('debian-12-runtime');
  assert.ok(runtime, 'ci.yml 里找不到 debian-12-runtime 作业');
  assert.match(runtime.text, /test -L "\$link"/, '必须保留 enable 符号链接的正断言 test -L');
  assert.match(runtime.text, /test ! -e "\$link"/, '必须保留 --no-enable 两包的反向断言');
  for (const unit of ['rsetup-next-thermal-policy', 'rsetup-next-fan-curve', 'rsetup-next-led-state']) {
    assert.ok(runtime.text.includes(unit), `Bookworm 步骤必须覆盖 ${unit} 的 enable 链接断言`);
  }
  for (const unit of ['rsetup-controller', 'rsetup-board-agent']) {
    assert.ok(runtime.text.includes(unit), `Bookworm 步骤必须覆盖 ${unit} 的无链接断言`);
  }
});

// ---------------------------------------------------------------------------
// round2：container 作业的 shell 口径 + 逐单元 enable 断言的绑定强度
// ---------------------------------------------------------------------------
//
// 事实依据（GitHub Actions 文档 “Customize the shell used”）：作业跑在 **container**
// 里时默认 shell 是 `sh -e {0}`（不是 ubuntu-latest 上的 bash），debian:trixie-slim 的
// /bin/sh 是 dash。因此 POSIX sh 不支持的写法必须显式 `shell: bash`。
// 这里的判据**只取 here-string `<<<`**：dash（含 Trixie 的 Debian 补丁版）支持
// `set -o pipefail`，用「本机旧 dash 不认 pipefail」去外推 Trixie 的行为不构成依据，
// 所以本合同也**不**把 pipefail 当 bashism 触发条件。
const parseSteps = (jobText) => {
  const lines = jobText.split('\n');
  const starts = [];
  lines.forEach((line, index) => {
    if (/^\s*- (name|uses|run):/.test(line)) starts.push(index);
  });
  return starts.map((start, index) => ({
    head: lines[start],
    block: lines.slice(start, index + 1 < starts.length ? starts[index + 1] : lines.length).join('\n'),
  }));
};

test('every container-job step using a bash here-string declares shell: bash', () => {
  const containerJobs = jobs.filter((job) => /(?:^|\n)[ \t]*container:/.test(job.text));
  assert.ok(containerJobs.length >= 2, 'CI 里应有 debian-package 与 debian-12-runtime 两个 container 作业');
  for (const job of containerJobs) {
    for (const step of parseSteps(job.text)) {
      if (!step.block.includes('<<<')) continue;
      assert.match(
        step.block,
        /^[ \t]*shell:[ \t]*bash[ \t]*$/m,
        `${job.name} 里使用 <<< 的步骤必须显式 shell: bash（container 作业默认 sh，dash 不认 here-string）：${step.head.trim()}`,
      );
    }
  }
});

test('Bookworm policy recorder logs timestamp phase and every argument before denying actions', () => {
  const job = jobCalled('debian-12-runtime');
  const setup = extractStepBlock(job.text, 'Configure recording policy-rc.d');
  const match = /<<'RSETUP_POLICY_EOF'\n([\s\S]*?)\n[ \t]*RSETUP_POLICY_EOF/.exec(setup);
  assert.ok(match, 'CI 必须在安装之前部署真实记录型 policy-rc.d');
  const script = match[1].split('\n').map((line) => line.replace(/^ {10}/, '')).join('\n') + '\n';
  const temp = mkdtempSync(join(tmpdir(), 'rsetup-policy-test-'));
  try {
    const log = join(temp, 'policy.log');
    for (const args of [
      ['rsetup-controller.service', 'start'],
      ['--quiet', 'rsetup-board-agent.service', 'restart'],
    ]) {
      const result = spawnSync('sh', ['-s', '--', ...args], {
        input: script,
        encoding: 'utf8',
        env: { ...process.env, RSETUP_POLICY_LOG: log, RSETUP_PACKAGE_PHASE: 'install-test' },
      });
      assert.equal(result.status, 101, result.stderr);
    }
    const records = readFileSync(log, 'utf8').trimEnd().split('\n');
    assert.equal(records.length, 2);
    assert.match(records[0], /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ\tinstall-test\trsetup-controller\.service\tstart$/);
    assert.match(records[1], /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ\tinstall-test\t--quiet\trsetup-board-agent\.service\trestart$/);
  } finally {
    // temp 是本测试 mkdtemp 创建的绝对目录，删除前再次验证固定父目录/前缀。
    assert.ok(temp.startsWith(join(tmpdir(), 'rsetup-policy-test-')));
    rmSync(temp, { recursive: true, force: true });
  }
  const install = extractStepBlock(job.text, 'Install on Debian 12');
  assert.ok(job.text.indexOf(setup) < job.text.indexOf(install));
  assert.match(install, /RSETUP_PACKAGE_PHASE:[ \t]*install/);
  assert.match(install, /RSETUP_POLICY_LOG:[ \t]*\/var\/log\/rsetup-package-policy\.log/);
  const output = extractStepBlock(job.text, 'Report package service action log');
  assert.match(output, /if:[ \t]*always\(\)/);
  assert.match(output, /\/var\/log\/rsetup-package-policy\.log/);
});

const NEXT_UNITS = [
  'rsetup-next-thermal-policy.service',
  'rsetup-next-fan-curve.service',
  'rsetup-next-led-state.service',
];

test('the systemd assert step declares shell: bash (container default sh + <<< usage)', () => {
  const debian = jobCalled('debian-package');
  assert.ok(debian, 'ci.yml 里找不到 debian-package 作业');
  const step = extractStepBlock(debian.text, 'Assert Debian systemd maintainer scripts');
  assert.match(
    step,
    /^[ \t]*shell:[ \t]*bash[ \t]*$/m,
    '该步骤用了 <<<，container 作业默认 sh → 必须 shell: bash（防删）',
  );
});

// 逐单元绑定的**结构**合同：名字出现在 CI 文本里不等于断言绑定到该单元。旧写法是
// 「awk 收集 enable 命令 + grep 名字」+ 一条全局 --require-active-enable，因此一个单元
// 活跃、另两个只有 debian-installed 守卫时依然全绿。合同按 shell 结构提取 for 循环
// （循环头 + 到匹配 `done` 的循环体），要求：目标单元写在循环列表里，循环体内对该单元
// 执行逐单元的审计断言；名字 grep、注释里的清单与全局 exists 断言都不能充数。
const findLoops = (stepText, variable) => {
  const lines = stepText.split('\n');
  const loops = [];
  for (let index = 0; index < lines.length; index += 1) {
    const header = new RegExp(`^[ \t]*for ${variable} in ([^\n;]+); do[ \t]*$`).exec(lines[index]);
    if (!header) continue;
    const body = [];
    let closed = false;
    for (const line of lines.slice(index + 1)) {
      if (/^[ \t]*done[ \t]*$/.test(line)) {
        closed = true;
        break;
      }
      body.push(line);
    }
    assert.ok(closed, `for ${variable} in … 的循环体没有闭合的 done`);
    loops.push({ list: header[1].trim().split(/\s+/), body: body.join('\n') });
  }
  return loops;
};

// ---------------------------------------------------------------------------
// 严格模板门禁的 CI 绑定合同（round4）
// ---------------------------------------------------------------------------
//
// 门禁的 CLI 将「审计哪一份脚本」「该脚本属于哪个包」「该包的 dh_installsystemd 包型」
// 「期望哪些单元」「仓库源在哪」全部变成显式参数，因此合同可以按**结构**提取每条调用，
// 并核对绑定关系；名字出现在 CI 文本里不再构成证据。
//
// 为什么必须有这组结构合同：备审三轮的共性问题不是「断言写错」而是「断言打在谁身上」——
// 把审计对象换成 `$tmp/unrelated/postinst`、把单元换成 `unrelated.service`、把文件换成
// `postinst.unrelated`，presence 型合同全都照样绿。下面的断言函数从 CI 文本里解析出
// 每条门禁调用，并用同一函数现场演示这些 mutation 必须被拦下。

// 把一条 `node "$gate" …` 的反斜杠续行折叠成单行，便于按结构核对绑定。
const gateCalls = (stepText) => {
  const calls = [];
  let buffer = null;
  for (const line of stepText.split('\n')) {
    if (buffer === null) {
      if (!/^[ \t]*node "\$gate" /.test(line)) continue;
      buffer = line.trim();
    } else {
      buffer += ' ' + line.trim();
    }
    if (line.trimEnd().endsWith('\\')) continue; // 续行未结束，继续累积
    calls.push(buffer.replace(/\\\s+/g, ' ').replace(/\s+/g, ' ').trim());
    buffer = null;
  }
  assert.ok(buffer === null, '存在未闭合的 node "$gate" 续行');
  return calls;
};

const GATE_EXPECT = [
  {
    pkg: 'rsetup-next',
    kind: 'postinst',
    mode: 'default-enable',
    units: NEXT_UNITS,
    source: 'debian/postinst',
  },
  { pkg: 'rsetup-next', kind: 'postrm', mode: 'default-enable', units: NEXT_UNITS, source: null },
  {
    pkg: 'rsetup-controller',
    kind: 'postinst',
    mode: 'no-enable',
    units: ['rsetup-controller.service'],
    source: 'debian/rsetup-controller.postinst',
  },
  {
    pkg: 'rsetup-controller',
    kind: 'postrm',
    mode: 'no-enable',
    units: ['rsetup-controller.service'],
    source: null,
  },
  {
    pkg: 'rsetup-board-agent',
    kind: 'postinst',
    mode: 'no-enable',
    units: ['rsetup-board-agent.service'],
    source: null,
  },
  {
    pkg: 'rsetup-board-agent',
    kind: 'postrm',
    mode: 'no-enable',
    units: ['rsetup-board-agent.service'],
    source: null,
  },
];

// 单条调用的结构断言：参数齐全、绑定自洽。
const assertGateCall = (call, expected) => {
  const option = (name) => {
    const m = new RegExp(`${name}[ \\t]+("([^"]*)"|\\S+)`).exec(call);
    return m === null ? null : (m[2] ?? m[1]);
  };
  const repeated = (name) => {
    const out = [];
    const re = new RegExp(`${name}[ \\t]+(?:"([^"]*)"|([^\\s"]+))`, 'g');
    for (const m of call.matchAll(re)) out.push(m[1] ?? m[2]);
    return out;
  };
  const flag = (name) => new RegExp(`(^|[ \\t])${name}([ \\t]|$)`).test(call);

  assert.equal(option('--package'), expected.pkg, `${expected.pkg}/${expected.kind}：--package 必须显式`);
  assert.equal(option('--script-kind'), expected.kind, `${expected.pkg}/${expected.kind}：--script-kind 必须显式`);
  assert.equal(
    option('--mode'),
    expected.mode,
    `${expected.pkg}/${expected.kind}：--mode 必须是 ${expected.mode}（对应 debian/rules 的 --no-enable 口径）`,
  );
  // 待审计文件 = 最后一个参数，必须正是 $tmp/<pkg>/<kind>：包目录与脚本种类都要与
  // --package/--script-kind 自洽（换成 $tmp/unrelated/postinst 或 postinst.unrelated
  // 都会在这里断掉）。
  assert.equal(
    (call.match(/\$tmp\//g) ?? []).length,
    1,
    `${expected.pkg}/${expected.kind}：一条调用只能审计一个文件（出现两个 $tmp 路径即审计对象不确定）`,
  );
  const tail = /"\$tmp\/([^/]+)\/([^"]+)"$/.exec(call);
  assert.ok(tail, `${expected.pkg}/${expected.kind}：审计对象必须是 "$tmp/<pkg>/<kind>" 形式，实际结尾：${call.slice(-60)}`);
  assert.equal(tail[1], expected.pkg, `${expected.kind} 的审计对象目录必须等于 --package`);
  assert.equal(tail[2], expected.kind, `审计对象文件名必须等于 --script-kind（postinst.unrelated 之类不认）`);

  const units = repeated('--require-unit');
  assert.ok(units.length > 0, `${expected.pkg}/${expected.kind}：必须显式声明 --require-unit（空集合会让断言空洞通过）`);
  assert.deepEqual(
    [...units].sort(),
    [...expected.units].sort(),
    `${expected.pkg}/${expected.kind}：--require-unit 集合必须恰好是 ${expected.units.join(', ')}`,
  );
  const source = option('--source-script');
  if (expected.source === null) {
    assert.equal(source, null, `${expected.pkg}/${expected.kind}：该脚本没有仓库源，不得凭空指一个`);
    assert.ok(
      !flag('--allow-unobserved-template'),
      '生产 CI 不得接受非实测槽位模板',
    );
  } else {
    assert.equal(
      source,
      expected.source,
      `${expected.pkg}/${expected.kind}：--source-script 必须指向该脚本的仓库源（模板外内容的唯一预期来源）`,
    );
  }
};

const assertStepGateWiring = (stepText) => {
  const calls = gateCalls(stepText);
  assert.equal(calls.length, 6, '必须恰好 6 条门禁调用（3 包 × postinst/postrm）');
  for (const expected of GATE_EXPECT) {
    const matched = calls.filter((call) => {
      const m = /--package[ \t]+"?([A-Za-z0-9._-]+)"?[ \t]+--script-kind[ \t]+"?([A-Za-z0-9._-]+)"?/.exec(call);
      return m !== null && m[1] === expected.pkg && m[2] === expected.kind;
    });
    assert.equal(
      matched.length,
      1,
      `${expected.pkg}/${expected.kind} 必须恰好有一条门禁调用（实际 ${matched.length} 条）`,
    );
    assertGateCall(matched[0], expected);
  }
};

test('CI 的每条门禁调用都把包、脚本种类、包型、单元集合与审计对象绑成一条', () => {
  const debian = jobCalled('debian-package');
  assert.ok(debian, 'ci.yml 里找不到 debian-package 作业');
  assertStepGateWiring(extractStepBlock(debian.text, 'Assert Debian systemd maintainer scripts'));
});

test('审计对象、单元、包型、仓库源任一处被改写都会破坏合同（现场演示 mutation）', () => {
  const debian = jobCalled('debian-package');
  const step = extractStepBlock(debian.text, 'Assert Debian systemd maintainer scripts');
  const mutations = [
    {
      label: '审计对象换成无关包的 postinst',
      text: step.replace('"$tmp/rsetup-controller/postinst"', '"$tmp/unrelated/postinst"'),
    },
    {
      label: '审计对象换成同目录下的后缀文件',
      text: step.replace('"$tmp/rsetup-board-agent/postinst"', '"$tmp/rsetup-board-agent/postinst.unrelated"'),
    },
    {
      label: '单元换成无关单元',
      text: step.replace('--require-unit rsetup-next-led-state.service', '--require-unit unrelated.service'),
    },
    {
      label: 'rsetup-next 的包型改成 no-enable（等于允许 --no-enable 漂移）',
      text: step.replace('--package rsetup-next --script-kind postinst --mode default-enable', '--package rsetup-next --script-kind postinst --mode no-enable'),
    },
    {
      label: '去掉仓库源（模板外内容就失去了唯一预期来源）',
      text: step.replace('\\\n            --source-script debian/postinst \\\n', '\n'),
    },
    {
      label: 'postrm 不再审计',
      text: step.replace('"$tmp/rsetup-next/postrm"', '"$tmp/rsetup-next/postinst"'),
    },
  ];
  for (const mutation of mutations) {
    assert.notEqual(mutation.text, step, `mutation 模板必须能命中当前 CI 文本：${mutation.label}`);
    assert.throws(
      () => assertStepGateWiring(mutation.text),
      undefined,
      `${mutation.label} 必须被结构合同拦下`,
    );
  }
});

// CI 步骤是**文本里的 shell**：引号或续行写错要等真实流水线跑起来才暴露（本仓上一轮
// 就真的写丢过一个收尾单引号）。这里用 `bash -n` 只做**语法检查**（不执行任何命令、
// 不联网、不碰产物），并把 `${{ … }}` 表达式换成常量。
const stepRunScript = (jobText, needle) => {
  const lines = jobText.split('\n');
  const start = lines.findIndex((line) => line.includes(`- name: ${needle}`));
  assert.ok(start >= 0, `ci.yml 里找不到步骤：${needle}`);
  let end = lines.length;
  for (let index = start + 1; index < lines.length; index += 1) {
    if (/^[ \t]*- (name|uses):/.test(lines[index])) {
      end = index;
      break;
    }
  }
  const block = lines.slice(start, end);
  const runAt = block.findIndex((line) => /^[ \t]*run:[ \t]*\|[ \t]*$/.test(line));
  assert.ok(runAt >= 0, `步骤 ${needle} 必须有 run: | 块`);
  const body = block.slice(runAt + 1).filter((line, index) => index >= 0 || line.trim() !== '');
  const first = body.find((line) => line.trim() !== '');
  const indent = /^[ \t]*/.exec(first)[0];
  return body
    .map((line) => (line.startsWith(indent) ? line.slice(indent.length) : line))
    .join('\n')
    .replace(/\$\{\{[^}]*\}\}/g, 'x');
};

test('CI 步骤的 shell 文本必须能通过 bash -n 语法检查', () => {
  const debian = jobCalled('debian-package');
  assert.ok(debian, 'ci.yml 里找不到 debian-package 作业');
  const script = stepRunScript(debian.rawText, 'Assert Debian systemd maintainer scripts');
  assert.match(script, /^set -euo pipefail/, 'run 块必须从 set -euo pipefail 开始（提取本身也要对）');
  const dir = mkdtempSync(join(tmpdir(), 'ci-step-syntax-'));
  try {
    const file = join(dir, 'step.sh');
    writeFileSync(file, `${script}\n`);
    execFileSync('bash', ['-n', file], { stdio: 'pipe' });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('生产 CI 不得打开非实测模板槽位', () => {
  const debian = jobCalled('debian-package');
  const step = extractStepBlock(debian.text, 'Assert Debian systemd maintainer scripts');
  assert.doesNotMatch(
    step,
    /--allow-unobserved-template/,
    '--allow-unobserved-template 只用于本地实验，CI 里出现即等于放行未实测形状',
  );
});

// ---------------------------------------------------------------------------
// Debian 打包作业的 Rust 工具链下限（CI run 40109395224 的根因回归）
// ---------------------------------------------------------------------------
//
// 根因：Debian trixie 的 apt `rustc`/`cargo` 是 1.85.x（本仓日志里安装的是
// 1.85.1+dfsg1-1+deb13u1），而源码用了 1.88.0 才稳定的 let-chains（issue #53667）
// 与 slice_as_chunks（issue #74985）。dpkg-buildpackage 用 apt 的 1.85 编译
// rsetup-core 时刷出 27 条 E0658，arm64 直接失败，amd64 同源。
// 修法与其它 Rust 作业同口径：在 Trixie 容器里**显式**装一支 stable Rust 工具链
// 覆盖 apt 的 1.85，而不是换基础镜像、跳过打包/测试，或把 stable 降级到不兼容版本。
// 此处验证与其余 CI 作业一致的 stable 工具链选择与安装顺序，不宣称完整 workspace
// 的最低版本为 1.88；例如 tunnel 使用的 AtomicU64::try_update 需要更新的 Rust。
test('Debian trixie 作业显式装兼容的 stable Rust 工具链，不把 apt 的 1.85 当唯一编译器', () => {
  const debian = jobCalled('debian-package');
  assert.ok(debian, 'ci.yml 里找不到 debian-package 作业');

  // 保留 Debian trixie：不换基础镜像，兼容工具链在容器内装。
  assert.match(
    debian.rawText,
    /container:[ \t]*debian:trixie-slim/,
    'Debian 打包必须继续在 debian:trixie-slim 容器里进行（保留 Trixie）',
  );

  // 必须显式安装 stable Rust 工具链（与其余 Rust 作业同一 @stable 口径），
  // 让 debian/rules 里裸 `cargo` 解析到它，而不是 apt 的 1.85。
  assert.match(
    debian.text,
    /uses:[ \t]*dtolnay\/rust-toolchain@stable\b/,
    'debian-package 必须显式安装 dtolnay/rust-toolchain@stable，覆盖 apt 的 1.85 编译器',
  );

  // apt 清单仍带上 cargo 与 rustc：Build-Depends 声明了它们，dpkg-checkbuilddeps
  // 在任何构建规则前只核对存在性；这里只是不再让它充当实际构建编译器。
  assert.match(
    debian.text,
    /apt-get install[\s\S]*?\bcargo\b[\s\S]*?\brustc\b/,
    'apt 安装清单必须继续带上 cargo 与 rustc（满足 Build-Depends 存在性检查）',
  );

  // 兼容工具链必须**先于**离线缓存预热与打包编译：cargo fetch 与 dpkg-buildpackage
  // 里的 cargo rustc 必须共用同一支 stable，不能让 fetch/编译退回 apt 的 1.85。
  const toolchainAt = debian.text.search(/uses:[ \t]*dtolnay\/rust-toolchain@stable\b/);
  const fetchAt = debian.text.search(/cargo fetch\b/);
  const dpkgAt = debian.text.search(/dpkg-buildpackage\b/);
  assert.ok(
    debian.text.includes('CARGO_HOME="$GITHUB_WORKSPACE/debian/cargo-home"'),
    '离线 Cargo 缓存边界必须保留：cargo fetch 仍预热到 debian/cargo-home',
  );
  assert.ok(toolchainAt >= 0, '必须存在 stable 工具链安装步骤');
  assert.ok(fetchAt >= 0, 'debian-package 必须继续用 cargo fetch --locked 预热离线缓存');
  assert.ok(toolchainAt < fetchAt, 'stable 工具链安装必须早于 cargo fetch：两者用同一支编译器');
  assert.ok(toolchainAt < dpkgAt, 'stable 工具链安装必须早于 dpkg-buildpackage：打包编译用兼容工具链');
});
