// Debian 维护脚本 systemd 审计（scripts/lib/systemd-maintainer-audit.mjs）的行为合同。
//
// ## 被测架构：严格模板门禁（不是 shell 解释器）
//
// 审计只回答一个问题：这份维护脚本是否**逐字**等于「仓库源维护脚本（去掉 `#DEBHELPER#`）
// + 若干已登记的 debhelper 自动片段」，且片段的 helper/版本/单元/所在脚本种类都在白名单内。
// 比对只允许三类参数：单元（tmpfiles）列表、helper/版本标记、换行规范化（丢空行 + 去行尾
// 空白 + CRLF 折 LF）。因此本文件里的断言只有两类：
//
//   * 正向：已观察真实形状在归一后必须通过；
//   * 负向：任何模板外内容、模板变异、未登记来源，一律失败——**不再**逐条解释 shell 语义
//     （前三轮的 `false &&`、`${1:+remove}`、DI 条件位末命令这类反例，在这里只需要
//     「它不是已登记模板」这一条理由就被拒，见 §NEG-*）。
//
// ## 模板原文的来源（测试内独立复写，不从被测模块导入，避免自证）
//
// 模板先核对官方 raw 字节，tmpfiles 另以 Debian API 的 264B/SHA256 防止 HTML 转义误抄：
// https://salsa.debian.org/debian/debhelper/-/raw/debian/13.24.2/autoscripts/postinst-init-tmpfiles
// https://sources.debian.org/api/src/debhelper/13.24.2/autoscripts/postinst-init-tmpfiles/
// `postinst-systemd-enable`、`postinst-systemd-dont-enable`、
// `postrm-systemd` 三条同时在本仓 round1 的 Trixie 最小构建产物里实测出现过；
// `postrm-systemd-reload-only`、`postinst-init-tmpfiles` 只有上游原文证据（未在本仓产物
// 里实测）；唯一的合成形状是 remove 守卫下的 stop，测试明确标注**非实测**。
//
// 不读取仓库产物、不联网、不执行任何真实 helper 或服务。
import { test } from 'node:test';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

import {
  gateMaintainerScript,
  matchTemplate,
  splitScript,
  TEMPLATES,
} from '../lib/systemd-maintainer-audit.mjs';

const AUDIT_CLI = resolve(fileURLToPath(import.meta.url), '../../lib/systemd-maintainer-audit.mjs');
const REPO = resolve(fileURLToPath(import.meta.url), '../../..');

const reasons = (input) => gateMaintainerScript(input).failures.map((f) => f.reason);

// ---------------------------------------------------------------------------
// 模板原文（verbatim）与包装
// ---------------------------------------------------------------------------

const WRAP_END = '# End automatically added section';

const wrap = (marker, lines) =>
  `# Automatically added by ${marker}\n${lines.join('\n')}\n${WRAP_END}\n`;

// 13.24.2: autoscripts/postinst-systemd-enable
const enableLines = (unit) => [
  'if [ "$1" = "configure" ] || [ "$1" = "abort-upgrade" ] || [ "$1" = "abort-deconfigure" ] || [ "$1" = "abort-remove" ] ; then',
  '\t# The following line should be removed in trixie or trixie+1',
  `\tdeb-systemd-helper unmask '${unit}' >/dev/null || true`,
  '\t# was-enabled defaults to true, so new installations run enable.',
  `\tif deb-systemd-helper --quiet was-enabled '${unit}'; then`,
  '\t\t# Enables the unit on first installation, creates new',
  '\t\t# symlinks on upgrades if the unit file has changed.',
  `\t\tdeb-systemd-helper enable '${unit}' >/dev/null || true`,
  '\telse',
  '\t\t# Update the statefile to add new symlinks (if any), which need to be',
  '\t\t# cleaned up on purge. Also remove old symlinks.',
  `\t\tdeb-systemd-helper update-state '${unit}' >/dev/null || true`,
  '\tfi',
  'fi',
];

// 13.24.2: autoscripts/postinst-systemd-dont-enable
const dontEnableLines = (unit) => [
  'if [ "$1" = "configure" ] || [ "$1" = "abort-upgrade" ] || [ "$1" = "abort-deconfigure" ] || [ "$1" = "abort-remove" ] ; then',
  `\tif deb-systemd-helper debian-installed '${unit}'; then`,
  '\t\t# The following line should be removed in trixie or trixie+1',
  `\t\tdeb-systemd-helper unmask '${unit}' >/dev/null || true`,
  `\t\tif deb-systemd-helper --quiet was-enabled '${unit}'; then`,
  '\t\t\t# Create new symlinks, if any.',
  `\t\t\tdeb-systemd-helper enable '${unit}' >/dev/null || true`,
  '\t\tfi',
  '\tfi',
  '\t# Update the statefile to add new symlinks (if any), which need to be cleaned',
  '\t# up on purge. Also remove old symlinks.',
  `\tdeb-systemd-helper update-state '${unit}' >/dev/null || true`,
  'fi',
];

// 13.24.2: autoscripts/postrm-systemd
const purgeLines = (units) => [
  'if [ "$1" = "purge" ]; then',
  '\tif [ -x "/usr/bin/deb-systemd-helper" ]; then',
  `\t\tdeb-systemd-helper purge ${units.map((u) => `'${u}'`).join(' ')} >/dev/null || true`,
  '\tfi',
  'fi',
];

// 13.24.2: autoscripts/postrm-systemd-reload-only
const reloadOnlyLines = () => [
  'if [ "$1" = remove ] && [ -d /run/systemd/system ] ; then',
  '\tsystemctl --system daemon-reload >/dev/null || true',
  'fi',
];

// 13.24.2: autoscripts/postinst-init-tmpfiles
const tmpfilesLines = (files) => [
  'if [ "$1" = "configure" ] || [ "$1" = "abort-upgrade" ] || [ "$1" = "abort-deconfigure" ] || [ "$1" = "abort-remove" ] ; then',
  '\tif [ -x "$(command -v systemd-tmpfiles)" ]; then',
  `\t\tsystemd-tmpfiles \${DPKG_ROOT:+--root="$DPKG_ROOT"} --create ${files.join(' ')} || true`,
  '\tfi',
  'fi',
];

// 合成形状：非上游原文、非实测产物，只是本仓愿意接受的那个策略形状。
const removeStopLines = (units) => [
  'if [ "$1" = "remove" ] || [ "$1" = "deconfigure" ] ; then',
  '\tif [ -z "$DPKG_ROOT" ] && [ -d /run/systemd/system ] ; then',
  `\t\tdeb-systemd-invoke stop ${units.map((u) => `'${u}'`).join(' ')} >/dev/null || true`,
  '\tfi',
  'fi',
];

const SYSTEMD = 'dh_installsystemd/13.24.2';
const TMPFILES_HELPER = 'dh_installtmpfiles/13.24.2';

// debhelper 的 #DEBHELPER# 是**行内**替换（Dh_Lib::_substitution_generator），
// 生成的每个片段文件以换行结尾，替换后原行只剩生成的内容。
const inject = (source, sectionsText) => source.replace('#DEBHELPER#\n', sectionsText);

const NEXT_UNITS = [
  'rsetup-next-thermal-policy.service',
  'rsetup-next-fan-curve.service',
  'rsetup-next-led-state.service',
];

// 真实仓库源（不是 fixture：直接读仓库里的那两份源维护脚本）。
const NEXT_SOURCE = readFileSync(join(REPO, 'debian/postinst'), 'utf8');
const CONTROLLER_SOURCE = readFileSync(join(REPO, 'debian/rsetup-controller.postinst'), 'utf8');

// 13.24.2 下 rsetup-next postinst 应有的全文：源 + tmpfiles 片段 + 三个 enable 片段。
const nextPostinst = ({ units = NEXT_UNITS, tmpfiles = true } = {}) => {
  const auto =
    (tmpfiles ? wrap(TMPFILES_HELPER, tmpfilesLines(['rsetup-next.conf'])) : '') +
    units.map((unit) => wrap(SYSTEMD, enableLines(unit))).join('');
  return inject(NEXT_SOURCE, auto);
};

const controllerPostinst = (unit = 'rsetup-controller.service') =>
  inject(CONTROLLER_SOURCE, wrap(SYSTEMD, dontEnableLines(unit)));

const generatedPostrm = (units, { reloadOnly = false } = {}) =>
  '#!/bin/sh\nset -e\n' +
  wrap(SYSTEMD, purgeLines(units)) +
  (reloadOnly ? wrap(SYSTEMD, reloadOnlyLines()) : '');

const base = {
  pkg: 'rsetup-next',
  scriptKind: 'postinst',
  mode: 'default-enable',
  units: NEXT_UNITS,
  sourceText: NEXT_SOURCE,
};

const fails = (input) => {
  const result = gateMaintainerScript(input);
  assert.equal(result.ok, false, `期望门禁失败，实际通过：${JSON.stringify(result.matched)}`);
  return result.failures.map((f) => f.reason).join(' | ');
};

test('同一片段内各 #UNITFILE# 位置必须指向同一单元（守卫查 a、enable 干 b 被拒）', () => {
  // 只把 enable 那一行的单元换掉：unmask / was-enabled / update-state 仍指向原单元。
  // 旧口径会因为「字样 enable 出现了」而放过这种「守卫与实际执行对象不同」的变异。
  const diverged = enableLines(NEXT_UNITS[0]).map((line) =>
    line.includes('deb-systemd-helper enable')
      ? line.replace(NEXT_UNITS[0], 'rsetup-next-evil.service')
      : line,
  );
  const text = inject(
    NEXT_SOURCE,
    wrap(SYSTEMD, diverged) +
      wrap(SYSTEMD, enableLines(NEXT_UNITS[1])) +
      wrap(SYSTEMD, enableLines(NEXT_UNITS[2])),
  );
  assert.match(fails({ ...base, text }), /各 #UNITFILE# 参数位指向的单元不一致/);
});

// ---------------------------------------------------------------------------
// 正向：已观察真实形状
// ---------------------------------------------------------------------------

test('真实形状：默认-enable 包 postinst（源片段 + tmpfiles + 三个 enable 片段）通过', () => {
  const result = gateMaintainerScript({ ...base, text: nextPostinst() });
  assert.deepEqual(result.failures, []);
  assert.equal(result.matched.length, 4);
  assert.deepEqual(
    result.matched.filter((m) => m.template === 'postinst-systemd-enable').flatMap((m) => m.units),
    NEXT_UNITS,
  );
});

test('tmpfiles 上游 raw 字节哈希正确且真实 || true 片段必须通过', () => {
  // Debian API 公布的 264B/SHA256；不由被测 TEMPLATES 反推预期。
  const raw = tmpfilesLines(['#TMPFILES#']).join('\n') + '\n';
  assert.equal(Buffer.byteLength(raw), 264);
  assert.equal(
    createHash('sha256').update(raw).digest('hex'),
    'e38a9d798e9fb2f3e8cc19764fbb285f5b195b5d07179f2197d044826f9b4d27',
  );
  assert.deepEqual(gateMaintainerScript({ ...base, text: nextPostinst() }).failures, []);
});

test('tmpfiles 将 || true 错抄为管道 | true 必须拒绝', () => {
  const text = nextPostinst().replace('--create rsetup-next.conf || true', '--create rsetup-next.conf | true');
  assert.notEqual(text, nextPostinst());
  assert.match(fails({ ...base, text }), /不匹配任何已登记模板/);
});

test('真实形状：--no-enable 包 postinst 的 enable 记账整块在 debian-installed 模板内', () => {
  const result = gateMaintainerScript({
    pkg: 'rsetup-controller',
    scriptKind: 'postinst',
    mode: 'no-enable',
    units: ['rsetup-controller.service'],
    sourceText: CONTROLLER_SOURCE,
    text: controllerPostinst(),
  });
  assert.deepEqual(result.failures, []);
  assert.deepEqual(result.matched.map((m) => m.template), ['postinst-systemd-dont-enable']);
});

test('真实形状：纯生成 postrm（骨架 + purge 记账）通过，带 reload-only 也通过', () => {
  const run = (reloadOnly) =>
    gateMaintainerScript({
      pkg: 'rsetup-next',
      scriptKind: 'postrm',
      mode: 'default-enable',
      units: NEXT_UNITS,
      sourceText: null,
      text: generatedPostrm(NEXT_UNITS, { reloadOnly }),
    });
  assert.deepEqual(run(false).failures, []);
  const withReload = run(true);
  assert.deepEqual(withReload.failures, []);
  assert.deepEqual(
    withReload.matched.map((m) => m.template),
    ['postrm-systemd', 'postrm-systemd-reload-only'],
  );
});

test('换行规范化只允许空行/行尾空白/CRLF：这些都不得判红', () => {
  const spaced = nextPostinst()
    .replace(/\n/g, '\n\n')
    .split('\n')
    .map((line) => `${line}   `)
    .join('\n');
  const result = gateMaintainerScript({ ...base, text: `${spaced}\r\n` });
  assert.deepEqual(result.failures, []);
});

test('单元集合与 --require-unit 的顺序无关（多重集比较）', () => {
  const units = [...NEXT_UNITS].reverse();
  const result = gateMaintainerScript({ ...base, units, text: nextPostinst({ units }) });
  assert.deepEqual(result.failures, []);
});

// ---------------------------------------------------------------------------
// 单元绑定：注释/别的参数位/变量都不构成证据
// ---------------------------------------------------------------------------

test('单元缺失：三个声明只 enable 两个必须失败', () => {
  const reason = fails({
    ...base,
    text: nextPostinst({ units: NEXT_UNITS.slice(0, 2) }),
  });
  assert.match(reason, /不是同一多重集/);
});

test('单元多余：额外 enable 一个未声明的单元必须失败', () => {
  const reason = fails({
    ...base,
    text: nextPostinst({ units: [...NEXT_UNITS, 'rsetup-evil.service'] }),
  });
  assert.match(reason, /不是同一多重集/);
});

test('单元重复：同一单元 enable 两次不是「更保险」', () => {
  const reason = fails({
    ...base,
    text: nextPostinst({ units: [NEXT_UNITS[0], NEXT_UNITS[0], NEXT_UNITS[1]] }),
  });
  assert.match(reason, /不是同一多重集/);
});

test('单元只出现在注释或 unmask 行不构成证据（enable 行换成别的单元）', () => {
  const target = NEXT_UNITS[0];
  const swapped = enableLines(target).map((line) =>
    line.includes('deb-systemd-helper enable')
      ? line.replace(target, 'some-other.service')
      : line,
  );
  const text = inject(
    NEXT_SOURCE,
    wrap(SYSTEMD, [...swapped]) +
      wrap(SYSTEMD, enableLines(NEXT_UNITS[1])) +
      wrap(SYSTEMD, enableLines(NEXT_UNITS[2])),
  );
  // 被改过的 enable 行不再命中模板（整段先失败）。
  assert.match(fails({ ...base, text }), /第 8 行（的参数位|不一致）|不是同一多重集/);
  // 反向对照：同样的单元名写在注释里也一样不成立。
  const commented = inject(
    NEXT_SOURCE,
    wrap(SYSTEMD, [
      `# 目标单元：${NEXT_UNITS.join(' ')}`,
      ...enableLines(NEXT_UNITS[1]),
    ]) + wrap(SYSTEMD, enableLines(NEXT_UNITS[2])),
  );
  assert.match(fails({ ...base, text: commented }), /不是同一多重集|未登记/);
});

test('单元写成双引号、变量或拼接都不命中参数位', () => {
  for (const [label, line] of [
    ['双引号', `\t\tdeb-systemd-helper enable "${NEXT_UNITS[0]}" >/dev/null || true`],
    ['变量', `\t\tdeb-systemd-helper enable $UNIT >/dev/null || true`],
    ['拼接', `\t\tdeb-systemd-helper enable '${NEXT_UNITS[0]}' 'extra.service' >/dev/null || true`],
  ]) {
    const lines = enableLines(NEXT_UNITS[0]).map((l) =>
      l.includes('deb-systemd-helper enable') ? line : l,
    );
    const text = inject(
      NEXT_SOURCE,
      wrap(SYSTEMD, lines) +
        wrap(SYSTEMD, enableLines(NEXT_UNITS[1])) +
        wrap(SYSTEMD, enableLines(NEXT_UNITS[2])),
    );
    const reason = fails({ ...base, text });
    assert.match(reason, /参数位|第 .* 行不一致/, `${label} 形态必须被参数位拒绝`);
  }
});

// ---------------------------------------------------------------------------
// 模板变异：round3 的七条语义反例与备审形态，全部因「不是已登记模板」被拒
// （新架构不再逐条解释它们的 shell 语义，也不需要）。
// ---------------------------------------------------------------------------

const mutateFirstEnable = (fn) => {
  const sections = NEXT_UNITS.map((unit) => wrap(SYSTEMD, fn(enableLines(unit), unit)));
  return inject(
    NEXT_SOURCE,
    wrap(TMPFILES_HELPER, tmpfilesLines(['rsetup-next.conf'])) + sections.join(''),
  );
};

test('NEG-1 `false && enable` 这种包装不是已登记模板', () => {
  const text = mutateFirstEnable((lines, unit) => [
    `false && deb-systemd-helper enable '${unit}' >/dev/null || true`,
    ...lines,
  ]);
  assert.match(fails({ ...base, text }), /第 1 行不一致|有效内容/);
});

test('NEG-2 模板行尾追加命令（同 line 尾命令）被拒', () => {
  const withTail = mutateFirstEnable((lines) =>
    lines.map((l) =>
      l.includes('deb-systemd-helper enable')
        ? `${l}; deb-systemd-invoke restart x.service`
        : l,
    ),
  );
  assert.match(fails({ ...base, text: withTail }), /第 8 行的参数位|第 8 行不一致/);
  // 对照：原样模板必须通过（证明失败原因不是 fixture 本身坏了）。
  assert.equal(gateMaintainerScript({ ...base, text: nextPostinst() }).ok, true);
});

test('NEG-3 用 elif/else 重排守卫被拒', () => {
  const text = mutateFirstEnable((lines) => [
    lines[0].replace(' ; then', ' ; then'),
    ...lines.slice(1, -1),
    'elif true ; then',
    '\t:',
    'fi',
  ]);
  assert.match(fails({ ...base, text }), /有效内容|不一致/);
});

test('NEG-4 改成 case 形态被拒（不再是「Unknown 真值」而是「不是模板」）', () => {
  const text = inject(
    NEXT_SOURCE,
    wrap(SYSTEMD, [
      'case "$1" in',
      '\tconfigure)',
      `\t\tdeb-systemd-helper enable '${NEXT_UNITS[0]}' >/dev/null || true`,
      '\t\t;;',
      'esac',
    ]) +
      wrap(SYSTEMD, enableLines(NEXT_UNITS[1])) +
      wrap(SYSTEMD, enableLines(NEXT_UNITS[2])),
  );
  assert.match(fails({ ...base, text }), /不匹配任何已登记模板/);
});

test('NEG-5 条件位末命令被改（debian-installed 守卫名不副实）在 --no-enable 包被拒', () => {
  const tampered = dontEnableLines('rsetup-controller.service').map((l) =>
    l.includes('debian-installed') ? `${l.replace('; then', '')} ; true ; then` : l,
  );
  const text = inject(CONTROLLER_SOURCE, wrap(SYSTEMD, tampered));
  assert.match(
    fails({
      pkg: 'rsetup-controller',
      scriptKind: 'postinst',
      mode: 'no-enable',
      units: ['rsetup-controller.service'],
      sourceText: CONTROLLER_SOURCE,
      text,
    }),
    /不匹配任何已登记模板/,
  );
});

test('NEG-6 赋值值内命令替换（含服务调用）被拒', () => {
  const text = `${nextPostinst()}result="$(deb-systemd-invoke restart x.service)"\n`;
  assert.match(fails({ ...base, text }), /敏感调用字样/);
});

test('NEG-7 `${1:+remove}` 之类的参数扩展守卫出现在 prerm 里也不被承认', () => {
  const text =
    '#!/bin/sh\nset -e\n' +
    wrap(SYSTEMD, [
      'if [ "${1:+remove}" = remove ] ; then',
      `\tdeb-systemd-invoke stop '${NEXT_UNITS[0]}' >/dev/null || true`,
      'fi',
    ]);
  const reason = fails({
    pkg: 'rsetup-next',
    scriptKind: 'prerm',
    mode: 'default-enable',
    units: NEXT_UNITS,
    sourceText: null,
    text,
  });
  assert.match(reason, /不匹配任何已登记模板|未登记/);
});

test('NEG-8 守卫取值被改（configure→upgrade）逐字比对失败', () => {
  const text = inject(
    NEXT_SOURCE,
    wrap(SYSTEMD, enableLines(NEXT_UNITS[0]).map((l) => l.replace('"configure"', '"upgrade"'))) +
      wrap(SYSTEMD, enableLines(NEXT_UNITS[1])) +
      wrap(SYSTEMD, enableLines(NEXT_UNITS[2])),
  );
  assert.match(fails({ ...base, text }), /第 1 行不一致/);
});

test('NEG-9 缩进从制表符改成空格不是「等价改写」', () => {
  const text = nextPostinst().replace(/\tdeb-systemd-helper unmask/, '  deb-systemd-helper unmask');
  assert.match(fails({ ...base, text }), /第 3 行（的参数位|不一致）/);
});

test('NEG-10 安装/升级脚本里出现任何 deb-systemd-invoke 形状都无模板可套', () => {
  const text = nextPostinst() + wrap(SYSTEMD, [
    'if [ "$1" = "configure" ] ; then',
    `\tdeb-systemd-invoke restart '${NEXT_UNITS[0]}' >/dev/null || true`,
    'fi',
  ]);
  const reason = fails({ ...base, text });
  assert.match(reason, /不匹配任何已登记模板|敏感调用字样/);
});

// ---------------------------------------------------------------------------
// helper/版本登记与包装结构
// ---------------------------------------------------------------------------

test('未登记的 debhelper 版本（未来工具链）必须 review-fail，不做 >= 放宽', () => {
  const future = nextPostinst().replaceAll(SYSTEMD, 'dh_installsystemd/13.99.0');
  assert.match(
    fails({ ...base, text: future }),
    /未登记的自动片段来源 dh_installsystemd\/13\.99\.0/,
  );
});

test('未登记的 helper 名字（extrahelper 之类）必须 review-fail', () => {
  const renamed = nextPostinst().replaceAll(SYSTEMD, 'dh_installsystemd_extra/13.24.2');
  assert.match(fails({ ...base, text: renamed }), /未登记的自动片段来源/);
});

test('缺结束标记 / 起始标记嵌套：包装被改过就失败', () => {
  const noEnd = nextPostinst().replaceAll(WRAP_END, '# End automaticaly added section');
  assert.match(fails({ ...base, text: noEnd }), /找不到|结束标记/);
  const nested = nextPostinst().replace(
    `# Automatically added by ${SYSTEMD}\nif [ "$1" = "configure" ]`,
    `# Automatically added by ${SYSTEMD}\n# Automatically added by ${SYSTEMD}\nif [ "$1" = "configure" ]`,
  );
  assert.match(fails({ ...base, text: nested }), /内部又出现一条自动片段起始行/);
});

test('未登记的 14.5 标记和无 $1 守卫 stop 片段必须拒绝（不推断真实 helper 外层守卫）', () => {
  const upstream145 =
    '#!/bin/sh\nset -e\n' +
    wrap('dh_installsystemd/14.5', [
      'if [ -z "$DPKG_ROOT" ] && [ -d /run/systemd/system ]; then',
      `\tdeb-systemd-invoke stop ${NEXT_UNITS.map((u) => `'${u}'`).join(' ')} >/dev/null || true`,
      'fi',
    ]);
  assert.match(
    fails({
      pkg: 'rsetup-next',
      scriptKind: 'prerm',
      mode: 'default-enable',
      units: NEXT_UNITS,
      sourceText: null,
      text: upstream145,
    }),
    /未登记的自动片段来源 dh_installsystemd\/14\.5/,
  );
});

test('合成 remove-stop：默认拒绝，显式 --allow-unobserved-template 才放行且标注非实测', () => {
  const text =
    '#!/bin/sh\nset -e\n' + wrap(SYSTEMD, removeStopLines(NEXT_UNITS));
  const input = {
    pkg: 'rsetup-next',
    scriptKind: 'prerm',
    mode: 'default-enable',
    units: NEXT_UNITS,
    sourceText: null,
    text,
  };
  assert.match(fails(input), /非实测策略槽位/);
  const allowed = gateMaintainerScript({ ...input, allowUnobservedTemplate: true });
  assert.deepEqual(allowed.failures, []);
  assert.equal(allowed.matched[0].observed, false, '放行的模板必须带上 observed:false 标记');
  assert.equal(allowed.matched[0].upstreamVerified, false);
});

test('模板与脚本种类绑定：enable 片段搬到 postrm/prerm、purge 片段搬到 postinst 都被拒', () => {
  const enableInPostrm =
    '#!/bin/sh\nset -e\n' +
    wrap(SYSTEMD, enableLines(NEXT_UNITS[0])) +
    wrap(SYSTEMD, purgeLines(NEXT_UNITS));
  assert.match(
    fails({
      pkg: 'rsetup-next',
      scriptKind: 'postrm',
      mode: 'default-enable',
      units: NEXT_UNITS,
      sourceText: null,
      text: enableInPostrm,
    }),
    /不匹配任何已登记模板|未登记/,
  );
  const purgeInPostinst = inject(
    NEXT_SOURCE,
    wrap(SYSTEMD, purgeLines(NEXT_UNITS)) + wrap(SYSTEMD, enableLines(NEXT_UNITS[0])),
  );
  assert.match(fails({ ...base, text: purgeInPostinst }), /不匹配任何已登记模板/);
});

// ---------------------------------------------------------------------------
// 包型与期望片段
// ---------------------------------------------------------------------------

test('包型互换必须失败：--no-enable 产物按默认-enable 审计，反之亦然', () => {
  assert.match(
    fails({
      pkg: 'rsetup-controller',
      scriptKind: 'postinst',
      mode: 'default-enable',
      units: ['rsetup-controller.service'],
      sourceText: CONTROLLER_SOURCE,
      text: controllerPostinst(),
    }),
    /debian-installed 守卫内的 enable 模板/,
  );
  assert.match(
    fails({
      pkg: 'rsetup-next',
      scriptKind: 'postinst',
      mode: 'no-enable',
      units: NEXT_UNITS,
      sourceText: NEXT_SOURCE,
      text: nextPostinst(),
    }),
    /--no-enable 未生效/,
  );
});

test('postrm 缺 purge 记账必须失败（不能空洞通过）', () => {
  const text = '#!/bin/sh\nset -e\n';
  assert.match(
    fails({
      pkg: 'rsetup-next',
      scriptKind: 'postrm',
      mode: 'default-enable',
      units: NEXT_UNITS,
      sourceText: null,
      text,
    }),
    /没有任何 deb-systemd-helper purge 记账/,
  );
});

test('tmpfiles 片段缺失不影响 systemd 断言（本门禁只管 systemd 片段的期望集合）', () => {
  const result = gateMaintainerScript({ ...base, text: nextPostinst({ tmpfiles: false }) });
  assert.deepEqual(result.failures, []);
});

test('tmpfiles 片段逐字改写（换个文件名之外的任何字节）仍被拒', () => {
  const original = nextPostinst();
  const text = original.replace('--create rsetup-next.conf || true', '--create rsetup-next.conf >/dev/null || true');
  assert.notEqual(text, original, '负例必须实际变异，不能用未命中的 replace 自证');
  assert.match(fails({ ...base, text }), /第 3 行的参数位|第 3 行不一致/);
});

// ---------------------------------------------------------------------------
// 模板外内容（源片段）
// ---------------------------------------------------------------------------

test('模板外注入敏感调用（哪怕在源片段里）必须失败', () => {
  const injected = nextPostinst().replace(
    'exit 0',
    'deb-systemd-invoke start rsetup-controller.service\nexit 0',
  );
  const reason = fails({ ...base, text: injected });
  assert.match(reason, /敏感调用字样/);
  assert.match(reason, /仓库源维护脚本不一致/);
});

test('模板外注入无害命令（仓库源里没有）也必须失败', () => {
  const injected = `${nextPostinst()}\nchmod 0666 /etc/shadow\n`;
  assert.match(fails({ ...base, text: injected }), /仓库源维护脚本不一致/);
});

test('仓库源里没有 #DEBHELPER# 占位行时不得声称源片段与产物对应', () => {
  assert.match(
    fails({
      ...base,
      sourceText: NEXT_SOURCE.replace('#DEBHELPER#\n', ''),
      text: nextPostinst(),
    }),
    /没有 #DEBHELPER# 占位行/,
  );
});

test('源片段注释里出现敏感字样也拒绝（不参与模板匹配的东西不能靠「只是注释」放行）', () => {
  const sourceWithComment = NEXT_SOURCE.replace(
    'exit 0',
    '# 这里不会调 deb-systemd-invoke，只是注释\nexit 0',
  );
  const text = inject(sourceWithComment, wrap(TMPFILES_HELPER, tmpfilesLines(['rsetup-next.conf'])) + NEXT_UNITS.map((u) => wrap(SYSTEMD, enableLines(u))).join(''));
  assert.match(
    fails({ ...base, sourceText: sourceWithComment, text }),
    /敏感调用字样/,
  );
});

test('纯生成脚本的模板外只允许 #!/bin/sh + set -e 骨架', () => {
  const extra = generatedPostrm(NEXT_UNITS) + 'exit 0\n';
  assert.match(
    fails({
      pkg: 'rsetup-next',
      scriptKind: 'postrm',
      mode: 'default-enable',
      units: NEXT_UNITS,
      sourceText: null,
      text: extra,
    }),
    /只允许 debhelper 生成的骨架/,
  );
});

// ---------------------------------------------------------------------------
// 结构性 API 的小合同
// ---------------------------------------------------------------------------

test('splitScript 把起止标记之间的内容与残留分开，标记行本身不算残留', () => {
  const { sections, residual, unterminated } = splitScript(nextPostinst());
  assert.equal(unterminated, null);
  assert.equal(sections.length, 4);
  assert.deepEqual(sections.map((s) => `${s.helper}/${s.version}`), [
    TMPFILES_HELPER,
    SYSTEMD,
    SYSTEMD,
    SYSTEMD,
  ]);
  assert.ok(residual.some((r) => r.line.includes('exit 0')));
  assert.ok(!residual.some((r) => r.line === WRAP_END));
});

test('matchTemplate 只接受参数位差异，别的一律不一致', () => {
  const template = TEMPLATES.find((t) => t.name === 'postrm-systemd');
  assert.equal(matchTemplate(template, normalize(purgeLines(NEXT_UNITS))).ok, true);
  assert.equal(matchTemplate(template, normalize(purgeLines(NEXT_UNITS.slice(0, 1)))).ok, true,
    '单元个数不是模板参数，由集合断言把关');
  assert.equal(
    matchTemplate(template, normalize(purgeLines(NEXT_UNITS)).map((l) => l.replace('purge', 'enable'))).ok,
    false,
  );
});

const normalize = (lines) => lines.map((l) => l.replace(/[ \t]+$/, '')).filter((l) => l.trim() !== '');

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

const runCli = (args) => {
  try {
    const stdout = execFileSync('node', [AUDIT_CLI, ...args], {
      encoding: 'utf8',
      stdio: 'pipe',
    });
    return { status: 0, output: stdout };
  } catch (err) {
    return { status: err.status, output: `${err.stdout ?? ''}${err.stderr ?? ''}` };
  }
};

const withFiles = (t, fn) => {
  const dir = mkdtempSync(join(tmpdir(), 'template-gate-'));
  try {
    fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
};

test('CLI 正向：三包四脚本形态逐个调用都 exit 0', () => {
  withFiles('cli', (dir) => {
    const nextPost = join(dir, 'postinst');
    writeFileSync(nextPost, nextPostinst());
    const nextPostrm = join(dir, 'postrm');
    writeFileSync(nextPostrm, generatedPostrm(NEXT_UNITS));
    const ctrlPost = join(dir, 'postinst-controller');
    writeFileSync(ctrlPost, controllerPostinst());
    const boardPostrm = join(dir, 'postrm-board');
    writeFileSync(boardPostrm, generatedPostrm(['rsetup-board-agent.service']));

    assert.equal(
      runCli([
        '--package', 'rsetup-next', '--script-kind', 'postinst', '--mode', 'default-enable',
        '--source-script', join(REPO, 'debian/postinst'),
        ...NEXT_UNITS.flatMap((u) => ['--require-unit', u]),
        nextPost,
      ]).status,
      0,
    );
    assert.equal(
      runCli([
        '--package', 'rsetup-next', '--script-kind', 'postrm', '--mode', 'default-enable',
        ...NEXT_UNITS.flatMap((u) => ['--require-unit', u]),
        nextPostrm,
      ]).status,
      0,
    );
    assert.equal(
      runCli([
        '--package', 'rsetup-controller', '--script-kind', 'postinst', '--mode', 'no-enable',
        '--source-script', join(REPO, 'debian/rsetup-controller.postinst'),
        '--require-unit', 'rsetup-controller.service',
        ctrlPost,
      ]).status,
      0,
    );
    assert.equal(
      runCli([
        '--package', 'rsetup-board-agent', '--script-kind', 'postrm', '--mode', 'no-enable',
        '--require-unit', 'rsetup-board-agent.service',
        boardPostrm,
      ]).status,
      0,
    );
  });
});

test('CLI 负向：单元写错、包型写错、文件不存在都是 exit 1', () => {
  withFiles('cli-neg', (dir) => {
    const file = join(dir, 'postinst');
    writeFileSync(file, nextPostinst());
    const wrongUnit = runCli([
      '--package', 'rsetup-next', '--script-kind', 'postinst', '--mode', 'default-enable',
      '--source-script', join(REPO, 'debian/postinst'),
      '--require-unit', 'unrelated.service',
      file,
    ]);
    assert.equal(wrongUnit.status, 1);
    assert.match(wrongUnit.output, /不是同一多重集/);
    assert.equal(
      runCli([
        '--package', 'rsetup-next', '--script-kind', 'postinst', '--mode', 'no-enable',
        '--source-script', join(REPO, 'debian/postinst'),
        ...NEXT_UNITS.flatMap((u) => ['--require-unit', u]),
        file,
      ]).status,
      1,
    );
    assert.equal(
      runCli([
        '--package', 'rsetup-next', '--script-kind', 'postinst', '--mode', 'default-enable',
        '--source-script', join(REPO, 'debian/postinst'),
        ...NEXT_UNITS.flatMap((u) => ['--require-unit', u]),
        join(dir, 'definitely-missing'),
      ]).status,
      1,
    );
    // --source-script 指到不存在的文件也必须失败（Fail-Closed）。
    assert.equal(
      runCli([
        '--package', 'rsetup-next', '--script-kind', 'postinst', '--mode', 'default-enable',
        '--source-script', join(dir, 'no-such-source'),
        ...NEXT_UNITS.flatMap((u) => ['--require-unit', u]),
        file,
      ]).status,
      1,
    );
  });
});

test('CLI 用法：缺任一必需参数、多余文件、未知选项都是 exit 2', () => {
  withFiles('cli-usage', (dir) => {
    const file = join(dir, 'postinst');
    writeFileSync(file, inject(NEXT_SOURCE, wrap(SYSTEMD, enableLines(NEXT_UNITS[0]))));
    const good = [
      '--package', 'rsetup-next', '--script-kind', 'postinst', '--mode', 'default-enable',
      '--source-script', join(REPO, 'debian/postinst'),
      '--require-unit', NEXT_UNITS[0],
      file,
    ];
    assert.equal(runCli(good).status, 0);
    for (const drop of ['--package', '--script-kind', '--mode']) {
      const index = good.indexOf(drop);
      const args = [...good.slice(0, index), ...good.slice(index + 2)];
      assert.equal(runCli(args).status, 2, `缺 ${drop} 必须 exit 2`);
    }
    const noUnits = good.filter((a) => a !== '--require-unit' && a !== NEXT_UNITS[0]);
    assert.equal(runCli(noUnits).status, 2, '缺 --require-unit 必须 exit 2');
    assert.equal(runCli([...good, file]).status, 2, '两个文件必须 exit 2（逐脚本调用）');
    assert.equal(runCli([...good, '--enable-guard']).status, 2, '旧启发式选项必须已删除');
    assert.equal(runCli([...good, '--require-active-enable-unit', 'x.service']).status, 2);
    assert.equal(runCli([...good, '--enable-guard-unit', 'x.service']).status, 2);
  });
});

test('CLI 暴露模板登记表：13.24.2 三条实测形状 + 非实测槽位有明确标注', () => {
  const run = runCli(['--list-templates']);
  assert.equal(run.status, 0);
  assert.match(run.output, /dh_installsystemd\/13\.24\.2\s+postinst-systemd-enable/);
  assert.match(run.output, /dh_installsystemd\/13\.24\.2\s+postinst-systemd-dont-enable/);
  assert.match(run.output, /dh_installsystemd\/13\.24\.2\s+postrm-systemd\t/);
  assert.match(run.output, /remove-stop-synthetic.*observed=false.*upstream=false/);
});

test('旧启发式入口不得回潮：auditMaintainerScript / analyzeEnableGuards 已不存在', async () => {
  const mod = await import('../lib/systemd-maintainer-audit.mjs');
  assert.equal(mod.auditMaintainerScript, undefined);
  assert.equal(mod.analyzeEnableGuards, undefined);
});

test('模板登记表自身的一致性：正文含 deb-systemd-invoke 的模板只能出现在 prerm', () => {
  for (const template of TEMPLATES) {
    const body = template.lines.join('\n');
    if (body.includes('deb-systemd-invoke')) {
      assert.deepEqual(template.scripts, ['prerm'], `${template.name} 会调用服务，只能登记在 prerm`);
    }
    assert.ok(['postinst', 'preinst', 'prerm', 'postrm'].includes(template.scripts[0]));
    if (!template.observed || !template.upstreamVerified) {
      assert.ok(template.note.length > 0, `${template.name} 的证据不足必须在 note 里写清`);
    }
  }
});
