// Debian 维护脚本 systemd 激活审计 —— 严格模板门禁（strict template gate）。
//
// ## 这个脚本是什么
//
// 它对 `dpkg-deb --control` 解出来的 postinst/preinst/prerm/postrm 做**只读**静态检查，
// 判定方式只有一个：**把整份脚本与已登记的 debhelper 自动生成片段逐行完整比对**。
// 比不上一律失败。它不解释 shell：不解析 if/case/&&/|| 的真值，不建模变量、函数、
// 循环或可达性，也不判断任意 shell 程序在运行时会不会执行某条命令。
//
// ## 为什么是模板匹配（架构收缩的理由）
//
// 前三轮（见 docs/testing/tunnel-packaging-fix-report.md §3.3/§8.3/§9）实现过一个自制
// 的 shell tokenizer + 有限 AST，试图从语义上证明「这条 enable 在首装可达」「这个 stop
// 只在 remove 分支」。三轮备审各找到一批新的绕过（短路、分支先后、条件位末命令、
// 参数扩展、赋值值内替换……），说明「用自制解释器证明任意 shell」的漏报面无法穷尽，
// 每轮修复都在扩面。用户已明确批准收缩：**不再解释，只匹配**。因此本脚本不提供任何
// 「语义证明」，只提供一条更强的静态保证：
//
//   脚本内容 = 仓库源维护脚本（去掉 `#DEBHELPER#` 占位行）+ 若干**逐字已知**的
//   debhelper 自动片段，且这些片段的 helper/版本/单元列表/所在脚本种类都在白名单内。
//
// 任何模板外的内容（多一条命令、移动 guard、换单元、注释掉一半、把命令塞进
// `$( )`、`if` 改 `case`、helper 版本没登记过）都会让比对失败。
//
// ## 模板参数（只有这三类，别处一律严格）
//
//   1. 单元列表：`#UNITFILE#`（恰好一个 `'unit'` 单引号词元）/ `#UNITFILES#`
//      （≥1 个 `'unit'`，单空格分隔）/ `#TMPFILES#`（≥1 个裸文件名，单空格分隔）；
//   2. 版本标记：片段起始行 `# Automatically added by <helper>/<version>` 里的
//      `<helper>` 与 `<version>`，必须整串命中登记表（版本不做 >= 放宽）；
//   3. 换行规范化：比对前只丢弃空行、去掉每行行尾空白、把 CRLF 折成 LF。
//
// **不**把「包含某字样」当证明：单元必须出现在被匹配到的模板参数位上，注释里列单元名、
// unmask 行提到单元名都不构成该单元被 enable 的证据。
//
// ## 生命周期口径（结构性保证，不是运行时推断）
//
// 与 debian/rules 的 override_dh_installsystemd 一致（三包 `--no-start
// --no-stop-on-upgrade`；controller/board-agent 另带 `--no-enable`）：
//
//   * 安装/升级不 start/restart/stop：postinst 与 postrm 只允许登记表中**不含
//     `deb-systemd-invoke`** 的模板（enable 记账、purge 记账、tmpfiles、
//     remove 时的 daemon-reload）。登记表中任何会调用 `deb-systemd-invoke` 的模板都
//     被标记为「只允许出现在 prerm」，因此安装/升级路径出现激活调用在结构上就无法通过。
//   * 正常 remove 允许 stop：允许的形状只有 remove|deconfigure 守卫下的 stop，且该模板
//     目前**没有实测证据**（见下），默认不启用，需显式 `--allow-unobserved-template`。
//
// ## 模板表的证据级别（务必如实引用）
//
// 登记表每条形如 `dh_installsystemd/13.24.2`。13.24.2 是 CI 构建镜像 `debian:trixie-slim`
// 里 round1 实测的 debhelper 版本；已观察的三条 systemd 片段同时有产物证据。
// 模板须从 raw 正文核对，不从 HTML→Markdown 转换结果誊写（会误转义 `||`）。
// tmpfiles 已用官方 raw/API 的 264B/SHA256 再核对并纳入测试，详见报告 §10.8：
// https://salsa.debian.org/debian/debhelper/-/raw/debian/13.24.2/autoscripts/postinst-init-tmpfiles
// https://sources.debian.org/api/src/debhelper/13.24.2/autoscripts/postinst-init-tmpfiles/
//
// 唯一的例外是 `remove-stop-synthetic`：它不是实测产物，未在上游 13.25 核实，
// 只是「未来若工具链在 prerm 生成 remove 守卫下的 stop，本仓愿意接受的那个形状」的策略
// 槽位，`observed: false` + `upstreamVerified: false`，默认不参与匹配。关于它必须知道两件事：
//   * debhelper 13.25 确实存在；官方 changelog（2025-08-10）记载
//     “Always stop the service on remove (#1079181)”。归档网页缺条目不等于版本不存在：
//     https://metadata.ftp-master.debian.org/changelogs/main/d/debhelper/unstable_changelog
//   * 13.24.2/13.25 的 `prerm-systemd` 原文都是 `if [ -z "$DPKG_ROOT" ] && [ -d /run/systemd/system ]`
//     ——模板本身不带 $1 守卫；生成脚本是否有外层守卫/是否注入，须核对 helper 生成逻辑。
//     13.24.2 在本仓 flags 下不生成 prerm；不能仅凭模板名推断任意版本的升级行为。
//     未来未登记版本仍应 CI 判红，核实真实产物后再决定登记或修改 rules。
//
// ## 静态边界（勿夸大）
//
// 通过本门禁只证明：产物维护脚本的**文本**由「仓库源片段 + 已登记自动片段」构成，且
// 已登记片段在结构上不含安装/升级期的服务激活调用。它**不**证明：dpkg 真的按这段文本
// 执行、deb-systemd-helper 的状态文件行为、enable 符号链接真的建立。后半部分属于动态
// 证据，由 ci.yml 的 `debian-12-runtime` 作业（Bookworm 真实安装 + policy-rc.d 记录
// + enable 链接存在/不存在断言）提供，两个通道各自如实声明，不互相冒充。
//
// 行为合同与全部正反例见 scripts/tests/systemd-maintainer-audit.test.mjs。
//
// ## CLI
//
//   node scripts/lib/systemd-maintainer-audit.mjs \
//     --package <二进制包名>                 # 只用于报错定位与合同一致性
//     --script-kind <postinst|preinst|prerm|postrm>
//     --mode <default-enable|no-enable>      # 该包的 dh_installsystemd 包型
//     --require-unit <unit>                  # 可重复；本包期望的单元全集（精确多重集）
//     [--source-script <仓库源维护脚本>]      # 该脚本的仓库源（含 #DEBHELPER#）；纯生成脚本省略
//     [--allow-unobserved-template]          # 显式接受 observed:false 的策略槽位模板
//     <待审计脚本文件>                         # 恰好一个
//   node scripts/lib/systemd-maintainer-audit.mjs --list-templates
//
// 退出码：0 通过；1 门禁失败；2 用法错误。

import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

// 起止标记：debhelper 的自动片段包装（Dh_Lib::autoscript 生成），逐字匹配。
const SECTION_START = /^# Automatically added by ([A-Za-z0-9][A-Za-z0-9_-]*)\/([A-Za-z0-9][A-Za-z0-9._+~-]*)$/;
const SECTION_END = '# End automatically added section';

// 模板正文里的参数位。
const PLACEHOLDERS = ['#UNITFILE#', '#UNITFILES#', '#TMPFILES#'];

// 单元词元：debhelper 用 Dh_Lib::quote() 输出单引号包裹的单元名。
const UNIT_TOKEN = '[A-Za-z0-9@._+~-]+';
// tmpfiles 文件名：dh_installtmpfiles 直接把 basename 拼接，不加引号。
const TMPFILE_TOKEN = '[A-Za-z0-9._+-]+';

const T = (name, helper, version, observed, upstreamVerified, scripts, invokesService, lines, note) => ({
  name,
  helper,
  version,
  observed,
  upstreamVerified,
  scripts,
  invokesService,
  lines,
  note,
});

// ---------------------------------------------------------------------------
// 模板登记表（唯一的放行来源）。
//
// helper/version 必须与产物起始行逐字一致；正文逐字来自
// https://sources.debian.org/src/debhelper/13.24.2/autoscripts/<file>。
// `observed` = 本仓在该版本下实测生成过；`upstreamVerified` = 正文与上游原文逐字核对过。
// scripts 列出该模板允许出现的脚本种类；invokesService = 正文含 deb-systemd-invoke。
// ---------------------------------------------------------------------------
export const TEMPLATES = [
  T(
    'postinst-systemd-enable',
    'dh_installsystemd',
    '13.24.2',
    true,
    true,
    ['postinst'],
    false,
    [
      'if [ "$1" = "configure" ] || [ "$1" = "abort-upgrade" ] || [ "$1" = "abort-deconfigure" ] || [ "$1" = "abort-remove" ] ; then',
      '\t# The following line should be removed in trixie or trixie+1',
      '\tdeb-systemd-helper unmask #UNITFILE# >/dev/null || true',
      '\t# was-enabled defaults to true, so new installations run enable.',
      '\tif deb-systemd-helper --quiet was-enabled #UNITFILE#; then',
      '\t\t# Enables the unit on first installation, creates new',
      '\t\t# symlinks on upgrades if the unit file has changed.',
      '\t\tdeb-systemd-helper enable #UNITFILE# >/dev/null || true',
      '\telse',
      '\t\t# Update the statefile to add new symlinks (if any), which need to be',
      '\t\t# cleaned up on purge. Also remove old symlinks.',
      '\t\tdeb-systemd-helper update-state #UNITFILE# >/dev/null || true',
      '\tfi',
      'fi',
    ],
    '默认-enable 包的 enable 记账：首装 was-enabled 默认为 true → 建 enable 符号链接，不调用 deb-systemd-invoke。',
  ),
  T(
    'postinst-systemd-dont-enable',
    'dh_installsystemd',
    '13.24.2',
    true,
    true,
    ['postinst'],
    false,
    [
      'if [ "$1" = "configure" ] || [ "$1" = "abort-upgrade" ] || [ "$1" = "abort-deconfigure" ] || [ "$1" = "abort-remove" ] ; then',
      '\tif deb-systemd-helper debian-installed #UNITFILE#; then',
      '\t\t# The following line should be removed in trixie or trixie+1',
      '\t\tdeb-systemd-helper unmask #UNITFILE# >/dev/null || true',
      '\t\tif deb-systemd-helper --quiet was-enabled #UNITFILE#; then',
      '\t\t\t# Create new symlinks, if any.',
      '\t\t\tdeb-systemd-helper enable #UNITFILE# >/dev/null || true',
      '\t\tfi',
      '\tfi',
      '\t# Update the statefile to add new symlinks (if any), which need to be cleaned',
      '\t# up on purge. Also remove old symlinks.',
      '\tdeb-systemd-helper update-state #UNITFILE# >/dev/null || true',
      'fi',
    ],
    '--no-enable 包的形状：enable 记账整块在 debian-installed 守卫内（首装 false → 不建链接）。',
  ),
  T(
    'postrm-systemd',
    'dh_installsystemd',
    '13.24.2',
    true,
    true,
    ['postrm'],
    false,
    [
      'if [ "$1" = "purge" ]; then',
      '\tif [ -x "/usr/bin/deb-systemd-helper" ]; then',
      '\t\tdeb-systemd-helper purge #UNITFILES# >/dev/null || true',
      '\tfi',
      'fi',
    ],
    'purge 时清 deb-systemd-helper 状态文件；只在 purge 分支，不与服务对话。',
  ),
  T(
    'postrm-systemd-reload-only',
    'dh_installsystemd',
    '13.24.2',
    false,
    true,
    ['postrm'],
    false,
    [
      'if [ "$1" = remove ] && [ -d /run/systemd/system ] ; then',
      '\tsystemctl --system daemon-reload >/dev/null || true',
      'fi',
    ],
    'remove 后的 daemon-reload：不 start/stop 任何服务。是否在 13.24.2 下与本包一同生成未实测（本仓未登记为期望片段，出现时按已登记模板放行但仍要求逐字一致）。',
  ),
  T(
    'postinst-init-tmpfiles',
    'dh_installtmpfiles',
    '13.24.2',
    false,
    true,
    ['postinst'],
    false,
    [
      'if [ "$1" = "configure" ] || [ "$1" = "abort-upgrade" ] || [ "$1" = "abort-deconfigure" ] || [ "$1" = "abort-remove" ] ; then',
      '\tif [ -x "$(command -v systemd-tmpfiles)" ]; then',
      '\t\tsystemd-tmpfiles ${DPKG_ROOT:+--root="$DPKG_ROOT"} --create #TMPFILES# || true',
      '\tfi',
      'fi',
    ],
    'tmpfiles 片段（rsetup-next 的 rsetup-next.conf）。正文里的 $( ) 与 ${ } 是上游模板原文的一部分，不是被解释的 shell。',
  ),
  T(
    'remove-stop-synthetic',
    'dh_installsystemd',
    '13.24.2',
    false,
    false,
    ['prerm'],
    true,
    [
      'if [ "$1" = "remove" ] || [ "$1" = "deconfigure" ] ; then',
      '\tif [ -z "$DPKG_ROOT" ] && [ -d /run/systemd/system ] ; then',
      '\t\tdeb-systemd-invoke stop #UNITFILES# >/dev/null || true',
      '\tfi',
      'fi',
    ],
    '非实测、未在上游 13.25 核实的策略槽位：仅 remove|deconfigure 守卫下的 stop。默认不参与匹配（需 --allow-unobserved-template）；13.24.2 在 --no-start --no-stop-on-upgrade 下不会生成任何 prerm 片段。',
  ),
];

// 纯生成脚本（仓库里没有对应源维护脚本）的标准骨架。
const GENERATED_SKELETON = ['#!/bin/sh', 'set -e'];

// 模板外（源片段与包装行之外）不得出现的敏感调用字样。
// 注意口径：`systemd-tmpfiles` 不在名单里——本仓 debian/postinst 的**已评审源片段**里就
// 合法地调用它（`command -v systemd-tmpfiles` + `--create`），而它不参与 systemd 单元的
// enable/start/stop；自动片段版本仍由模板逐字比对。名单只放「会激活/记账服务」的入口。
const SENSITIVE_IN_RESIDUAL = [
  'deb-systemd-invoke',
  'deb-systemd-helper',
  '_dh_action',
  'systemctl',
];

// ---------------------------------------------------------------------------
// 文本规范化：只做三件事——折行尾、丢空行、去行尾空白（这就是全部「换行规范化」）。
// ---------------------------------------------------------------------------
const normalizeLines = (text) =>
  text
    .replace(/\r\n/g, '\n')
    .split('\n')
    .map((line) => line.replace(/[ \t]+$/, ''))
    .filter((line) => line.trim() !== '');

// 把模板行编译成正则：参数位替换，其余逐字转义。
const templateLineToRegExp = (templateLine) => {
  let source = '';
  let rest = templateLine;
  while (rest.length > 0) {
    const slot = PLACEHOLDERS.find((p) => rest.includes(p));
    if (slot === undefined) {
      source += rest.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      break;
    }
    const index = rest.indexOf(slot);
    source += rest.slice(0, index).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    if (slot === '#UNITFILE#') {
      source += `('${UNIT_TOKEN}')`;
    } else if (slot === '#UNITFILES#') {
      source += `('${UNIT_TOKEN}')(?: '${UNIT_TOKEN}')*`;
    } else {
      source += `(${TMPFILE_TOKEN})(?: ${TMPFILE_TOKEN})*`;
    }
    rest = rest.slice(index + slot.length);
  }
  return new RegExp(`^${source}$`);
};

// 参数位提取：返回该行的 token 列表（单元或文件名），无参数位返回 null。
const placeholdersIn = (templateLine) => {
  const found = PLACEHOLDERS.filter((p) => templateLine.includes(p));
  return found.length > 0 ? found : null;
};

const tokensFrom = (actualLine, placeholders) => {
  // 参数位之后的内容按空白切分：单元是单引号词元，tmpfiles 是裸词元。
  const isUnitSlot = placeholders[0] !== '#TMPFILES#';
  if (isUnitSlot) {
    const tokens = actualLine.match(new RegExp(`'${UNIT_TOKEN}'`, 'g'));
    if (tokens === null) return null;
    return tokens.map((t) => t.slice(1, -1));
  }
  // tmpfiles：取行尾 `|| true` 之前的裸词元序列。
  const match = new RegExp(`--create ((${TMPFILE_TOKEN})(?: ${TMPFILE_TOKEN})*) \\|\\| true$`).exec(
    actualLine,
  );
  if (match === null) return null;
  return match[1].split(' ');
};

// 逐行完整比对一个自动片段。
//
// 参数位的一致性口径（不是「任意一行匹配即可」）：
//   * `#UNITFILE#` 在模板里出现多次（unmask / was-enabled / enable / update-state），
//     这些位置**必须指向同一个单元**——否则就是「守卫查 a 单元、enable 干 b 单元」的
//     变异，整段拒绝；
//   * `#UNITFILES#`/`#TMPFILES#` 出现多次时，各处的词元列表必须完全一致（顺序与个数）；
//   * 因此每个片段的单元结论是**确定的一个单元**或**确定的一个列表**，
//     不靠「字样出现过」这种 presence 判断。
export const matchTemplate = (template, actualLines) => {
  const expected = normalizeLines(template.lines.join('\n'));
  if (expected.length !== actualLines.length) {
    return {
      ok: false,
      reason:
        `模板 ${template.name} 有 ${expected.length} 行有效内容，实际片段有 ${actualLines.length} 行` +
        '（增删行、插入命令或把两段拼一起都会走到这里）',
    };
  }
  const groups = { UNITFILE: [], UNITFILES: [], TMPFILES: [] };
  for (let index = 0; index < expected.length; index += 1) {
    const templateLine = expected[index];
    const actualLine = actualLines[index];
    const placeholders = placeholdersIn(templateLine);
    if (placeholders === null) {
      if (templateLine !== actualLine) {
        return {
          ok: false,
          reason:
            `模板 ${template.name} 第 ${index + 1} 行不一致：期望逐字 ` +
            `${JSON.stringify(templateLine)}，实际 ${JSON.stringify(actualLine)}`,
        };
      }
      continue;
    }
    const regexp = templateLineToRegExp(templateLine);
    if (!regexp.test(actualLine)) {
      return {
        ok: false,
        reason:
          `模板 ${template.name} 第 ${index + 1} 行的参数位（${placeholders.join(', ')}）不匹配：` +
          `实际 ${JSON.stringify(actualLine)}。参数位只接受 debhelper 生成的词元列表` +
          '（`#UNITFILE#` 恰好一个单引号词元，`#UNITFILES#` 为单空格分隔的单引号词元列表）；' +
          '变量、双引号、拼接、行尾追加命令都不算命中',
      };
    }
    const tokens = tokensFrom(actualLine, placeholders);
    if (tokens === null) {
      return {
        ok: false,
        reason: `模板 ${template.name} 第 ${index + 1} 行的参数位无法提取词元列表：${JSON.stringify(actualLine)}`,
      };
    }
    for (const placeholder of placeholders) {
      groups[placeholder.slice(1, -1)].push(tokens);
    }
  }

  const collapse = (kind, label) => {
    const collected = groups[kind];
    if (collected.length === 0) return { ok: true, tokens: [] };
    const first = collected[0].join('\n');
    if (collected.some((tokens) => tokens.join('\n') !== first)) {
      return {
        ok: false,
        reason:
          `模板 ${template.name} 里各 ${label} 参数位指向的${kind === 'UNITFILE' ? '单元' : '列表'}不一致：` +
          collected
            .map((tokens) => JSON.stringify(tokens))
            .join(' vs ') +
          `。同一片段内的${kind === 'UNITFILE' ? '单元必须唯一' : '列表必须一致'}，` +
          '否则就是「守卫检查的对象与实际执行的对象不是同一个」这种变异',
      };
    }
    return { ok: true, tokens: collected[0] };
  };
  const single = collapse('UNITFILE', '#UNITFILE#');
  if (!single.ok) return single;
  const multi = collapse('UNITFILES', '#UNITFILES#');
  if (!multi.ok) return multi;
  const tmp = collapse('TMPFILES', '#TMPFILES#');
  if (!tmp.ok) return tmp;
  if (groups.UNITFILE.length > 0 && groups.UNITFILES.length > 0) {
    return {
      ok: false,
      reason: `模板 ${template.name} 同时出现 #UNITFILE# 与 #UNITFILES# 两种参数位，登记表不支持这种形状`,
    };
  }
  return { ok: true, units: [...single.tokens, ...multi.tokens], tmpfiles: tmp.tokens };
};

// ---------------------------------------------------------------------------
// 脚本切分：自动片段（含起止标记）+ 模板外残留。
// ---------------------------------------------------------------------------
export const splitScript = (text) => {
  const lines = text.replace(/\r\n/g, '\n').split('\n');
  const sections = [];
  const residual = [];
  let current = null;
  lines.forEach((line, index) => {
    const lineNo = index + 1;
    if (current !== null) {
      if (line === SECTION_END) {
        current.endLine = lineNo;
        sections.push(current);
        current = null;
        return;
      }
      if (SECTION_START.test(line)) {
        const m = SECTION_START.exec(line);
        current.nestedStart = lineNo;
        current.nestedHelper = `${m[1]}/${m[2]}`;
      }
      current.body.push({ line, lineNo });
      return;
    }
    const match = SECTION_START.exec(line);
    if (match) {
      current = {
        helper: match[1],
        version: match[2],
        startLine: lineNo,
        body: [],
      };
      return;
    }
    residual.push({ line, lineNo });
  });
  return { sections, residual, unterminated: current };
};

// 模板外残留的证据要求。
const checkResidual = ({ residual, sourceText, allowEmptySkeleton }) => {
  const failures = [];
  const residualLines = normalizeLines(residual.map((r) => r.line).join('\n'));
  const indexOfSensitive = (line) => SENSITIVE_IN_RESIDUAL.find((token) => line.includes(token));

  residual.forEach(({ line, lineNo }) => {
    const token = indexOfSensitive(line);
    if (token !== undefined) {
      failures.push({
        line: lineNo,
        reason:
          `模板外内容出现敏感调用字样 "${token}"。自动片段之外的任何一行（包括注释、字符串、` +
          '命令替换内部）都不参与模板匹配，因此不能靠"它其实不会执行"放行；' +
          '要么把它放进已登记的自动片段，要么从源维护脚本里去掉',
      });
    }
  });

  if (sourceText !== null) {
    const sourceLines = normalizeLines(sourceText.replace(/\r\n/g, '\n'));
    const debhelperIndex = sourceLines.findIndex((line) => line.includes('#DEBHELPER#'));
    if (debhelperIndex === -1) {
      failures.push({
        line: 0,
        reason: '仓库源维护脚本里没有 #DEBHELPER# 占位行，debhelper 不会把自动片段注入它，源片段与产物无对应关系',
      });
      return failures;
    }
    const expected = sourceLines
      .filter((line) => !line.includes('#DEBHELPER#'))
      .join('\n')
      .split('\n')
      .filter((line) => line.trim() !== '');
    if (expected.join('\n') !== residualLines.join('\n')) {
      const firstDiff = expected.findIndex((line, index) => line !== residualLines[index]);
      const at = firstDiff === -1 ? Math.min(expected.length, residualLines.length) : firstDiff;
      failures.push({
        line: residual[0]?.lineNo ?? 0,
        reason:
          '模板外内容与仓库源维护脚本不一致：产物里非自动片段的部分必须逐字等于 ' +
          `--source-script 指定文件（去掉 #DEBHELPER# 行、忽略空行与行尾空白）${
            expected.length !== residualLines.length ? `；行数 ${expected.length} vs ${residualLines.length}` : ''
          }。第 ${at + 1} 行差异：源 ${JSON.stringify(expected[at] ?? null)} vs 产物 ${JSON.stringify(
            residualLines[at] ?? null,
          )}`,
      });
    }
    return failures;
  }

  // 纯生成脚本：模板外只允许 debhelper 的标准骨架。
  const skeleton = allowEmptySkeleton ? [] : GENERATED_SKELETON;
  if (residualLines.join('\n') !== skeleton.join('\n')) {
    failures.push({
      line: residual[0]?.lineNo ?? 0,
      reason:
        '该脚本没有仓库源（--source-script 未提供），模板外内容只允许 debhelper 生成的骨架 ' +
        `${JSON.stringify(skeleton.join('\n'))}，实际 ${JSON.stringify(residualLines.join('\n'))}`,
    });
  }
  return failures;
};

// ---------------------------------------------------------------------------
// 门禁主体。
// ---------------------------------------------------------------------------
/**
 * 对一份维护脚本执行严格模板门禁。
 *
 * @param {object} input
 * @param {string} input.text 脚本全文
 * @param {string} input.scriptKind postinst|preinst|prerm|postrm
 * @param {'default-enable'|'no-enable'} input.mode 该包的 dh_installsystemd 包型
 * @param {string[]} input.units --require-unit 传入的期望单元全集（精确多重集）
 * @param {string|null} [input.sourceText] 仓库源维护脚本全文（纯生成脚本传 null）
 * @param {boolean} [input.allowUnobservedTemplate] 允许 observed:false 的策略槽位模板
 * @param {string} [input.pkg] 仅用于报错定位
 * @returns {{ ok: boolean, failures: Array<{line:number,reason:string}>, matched: Array<object> }}
 */
export const gateMaintainerScript = ({
  text,
  scriptKind,
  mode,
  units,
  sourceText = null,
  allowUnobservedTemplate = false,
  pkg = '<package>',
}) => {
  const failures = [];
  const matched = [];
  const SCRIPT_KINDS = ['postinst', 'preinst', 'prerm', 'postrm'];
  if (!SCRIPT_KINDS.includes(scriptKind)) {
    return {
      ok: false,
      failures: [{ line: 0, reason: `未知的 --script-kind：${String(scriptKind)}` }],
      matched,
    };
  }
  if (mode !== 'default-enable' && mode !== 'no-enable') {
    return {
      ok: false,
      failures: [{ line: 0, reason: `未知的 --mode：${String(mode)}（只支持 default-enable / no-enable）` }],
      matched,
    };
  }
  if (!Array.isArray(units) || units.length === 0) {
    return {
      ok: false,
      failures: [{ line: 0, reason: '必须至少有一条 --require-unit：期望单元集合为空会让断言空洞通过' }],
      matched,
    };
  }

  // 「换行规范化」在这里一次性做完（CRLF 折 LF + 去每行行尾空白），使起始/结束标记与
  // 模板正文处于同一口径；**行首缩进仍然严格**（制表符改空格必红）。
  const normalizedText = text
    .replace(/\r\n/g, '\n')
    .split('\n')
    .map((line) => line.replace(/[ \t]+$/, ''))
    .join('\n');
  const { sections, residual, unterminated } = splitScript(normalizedText);
  if (unterminated !== null) {
    failures.push({
      line: unterminated.startLine,
      reason:
        `自动片段起始行（${unterminated.helper}/${unterminated.version}）之后找不到 ` +
        `"${SECTION_END}" 结束标记：脚本被截断或结束标记被改过`,
    });
    return { ok: false, failures, matched };
  }

  const unitsOf = { enable: [], dontEnable: [], purge: [] };
  const seenEnableUnits = [];
  const seenDontEnableUnits = [];
  let invokeTemplate = null;

  for (const section of sections) {
    const marker = `${section.helper}/${section.version}`;
    const candidates = TEMPLATES.filter((template) => {
      if (template.helper !== section.helper || template.version !== section.version) return false;
      if (template.scripts.includes(scriptKind)) return true;
      // 该模板登记在其他脚本种类下：允许出现在这里就必须先把原因写清楚，
      // 因此不直接匹配，统一走「未登记」失败分支。
      return false;
    });
    const bodyLines = normalizeLines(section.body.map((b) => b.line).join('\n'));
    if (section.nestedStart !== undefined) {
      failures.push({
        line: section.nestedStart,
        reason:
          `自动片段 ${marker}（起始 ${section.startLine}）内部又出现一条自动片段起始行 ` +
          `(${section.nestedHelper})：手工拼接 debhelper 片段的痕迹`,
      });
      continue;
    }
    let match = null;
    const attempts = [];
    for (const template of candidates) {
      const result = matchTemplate(template, bodyLines);
      attempts.push({ template, result });
      if (!result.ok) continue;
      if (!template.observed && !template.upstreamVerified && !allowUnobservedTemplate) {
        // 形状对上了但证据不足：与「形状对不上」是两类不同的失败，分开报。
        failures.push({
          line: section.startLine,
          reason:
            `自动片段 ${marker} 命中了非实测策略槽位模板 ${template.name}` +
            '（observed:false 且未核实上游正文）：只有显式 --allow-unobserved-template 才放行，' +
            '通过结果也会带上 observed=false 标记',
        });
        match = { template, result, blocked: true };
        break;
      }
      match = { template, result };
      break;
    }
    if (match?.blocked === true) {
      continue;
    }
    if (match === null) {
      if (candidates.length === 0) {
        failures.push({
          line: section.startLine,
          reason:
            `未登记的自动片段来源 ${marker}（本脚本种类 ${scriptKind} 下没有对应模板）。` +
            '工具链升级、helper 换版本或片段被改过都会走到这里——这是刻意的 review-fail：' +
            '先人工核对上游该版本的原文，再把它加进 TEMPLATES，不承诺「>= 某个下限就自动支持」',
        });
        continue;
      }
      // 多个候选模板都不一致时，报「走得最远」的那一条，信息量最大。
      const distance = (attempt) => {
        const m = /第 (\d+) 行/.exec(attempt.result.reason);
        return m === null ? -1 : Number(m[1]);
      };
      const best = attempts.reduce((a, b) => (distance(b) > distance(a) ? b : a));
      const unobserved = candidates.filter((t) => !t.observed && !t.upstreamVerified);
      const tried = candidates.map((t) => t.name).join(', ');
      failures.push({
        line: section.startLine,
        reason:
          `自动片段 ${marker} 在 ${scriptKind} 下不匹配任何已登记模板（已尝试 ${tried}）。` +
          `${unobserved.length > 0 && !allowUnobservedTemplate ? `其中 ${unobserved.map((t) => t.name).join(', ')} 是非实测策略槽位，需 --allow-unobserved-template。` : ''}` +
          `最接近的一次差异：与 ${best.template.name} — ${best.result.reason}`,
      });
      continue;
    }
    const { template, result } = match;
    matched.push({
      template: template.name,
      marker,
      line: section.startLine,
      units: result.units,
      tmpfiles: result.tmpfiles,
      observed: template.observed,
      upstreamVerified: template.upstreamVerified,
    });

    if (template.invokesService) {
      invokeTemplate = template.name;
    }
    if (template.name === 'postinst-systemd-enable') {
      unitsOf.enable.push(...result.units);
      seenEnableUnits.push(...result.units);
    } else if (template.name === 'postinst-systemd-dont-enable') {
      unitsOf.dontEnable.push(...result.units);
      seenDontEnableUnits.push(...result.units);
    } else if (template.name === 'postrm-systemd') {
      unitsOf.purge.push(...result.units);
    }
  }

  // 安装/升级路径的服务激活：结构性禁止。
  if (invokeTemplate !== null && scriptKind !== 'prerm') {
    failures.push({
      line: 0,
      reason: `会调用 deb-systemd-invoke 的模板 ${invokeTemplate} 出现在 ${scriptKind}：安装/升级路径不得激活服务`,
    });
  }

  // 包型 × 脚本种类 的期望片段。
  const sortedSame = (a, b) =>
    a.length === b.length && [...a].sort().join('\n') === [...b].sort().join('\n');
  const expectSet = (actual, label, where) => {
    if (!sortedSame(actual, units)) {
      failures.push({
        line: 0,
        reason:
          `${where}：${label}覆盖的单元集合 ${JSON.stringify([...actual].sort())} 与 --require-unit 声明的 ` +
          `${JSON.stringify([...units].sort())} 不是同一多重集（缺项、多项、重复都算不一致）`,
      });
    }
  };

  if (scriptKind === 'postinst') {
    if (mode === 'default-enable') {
      if (unitsOf.dontEnable.length > 0) {
        failures.push({
          line: matched.find((m) => m.template === 'postinst-systemd-dont-enable')?.line ?? 0,
          reason:
            '默认-enable 包的 postinst 出现 debian-installed 守卫内的 enable 模板：说明该包按 --no-enable 生成，' +
            '与 debian/rules 的 dh_installsystemd -prsetup-next（无 --no-enable）不符',
        });
      }
      expectSet(unitsOf.enable, 'enable 记账', 'postinst');
    } else {
      if (seenEnableUnits.length > 0) {
        failures.push({
          line: matched.find((m) => m.template === 'postinst-systemd-enable')?.line ?? 0,
          reason:
            '--no-enable 包的 postinst 出现不受 debian-installed 守卫的 enable 模板：--no-enable 未生效，' +
            '该包会随安装建 enable 符号链接',
        });
      }
      expectSet(unitsOf.dontEnable, 'debian-installed 守卫内的 enable 记账', 'postinst');
    }
  } else if (scriptKind === 'postrm') {
    if (unitsOf.purge.length === 0) {
      failures.push({
        line: 0,
        reason:
          'postrm 没有任何 deb-systemd-helper purge 记账片段（dh_installsystemd 的 postrm-systemd）：' +
          'purge 后状态文件会残留，且说明期望的自动片段并未生成',
      });
    } else {
      expectSet(unitsOf.purge, 'purge 记账', 'postrm');
    }
    if (seenEnableUnits.length > 0 || seenDontEnableUnits.length > 0) {
      failures.push({
        line: matched.find((m) => m.template.startsWith('postinst-systemd'))?.line ?? 0,
        reason: 'postrm 里出现 enable 记账模板：enable 记账属于 postinst，出现在卸载脚本里说明片段被搬移过',
      });
    }
  } else if (sections.length > 0 && invokeTemplate === null) {
    failures.push({
      line: sections[0].startLine,
      reason: `${scriptKind} 里出现了已登记但本包不该有的自动片段`,
    });
  }

  failures.push(
    ...checkResidual({
      residual,
      sourceText,
      allowEmptySkeleton: false,
    }),
  );

  return { ok: failures.length === 0, failures: failures.sort((a, b) => a.line - b.line), matched };
};

// 登记表的可读输出（CI 调试与报告取证用）。
export const listTemplates = () =>
  TEMPLATES.map((template) => ({
    name: template.name,
    marker: `${template.helper}/${template.version}`,
    scripts: template.scripts.join(','),
    invokesService: template.invokesService,
    observed: template.observed,
    upstreamVerified: template.upstreamVerified,
    note: template.note,
  }));

const USAGE =
  'usage: systemd-maintainer-audit.mjs --package <pkg> --script-kind <postinst|preinst|prerm|postrm>' +
  ' --mode <default-enable|no-enable> --require-unit <unit>[ --require-unit <unit>…]' +
  ' [--source-script <repo-source>] [--allow-unobserved-template] <script-file>\n' +
  '       systemd-maintainer-audit.mjs --list-templates';

const parseCliArgv = (argv) => {
  const options = { units: [], files: [] };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const eq = arg.indexOf('=');
    const name = eq > -1 ? arg.slice(0, eq) : arg;
    const inline = eq > -1 ? arg.slice(eq + 1) : null;
    const takeValue = () => {
      if (inline !== null) return inline === '' ? null : inline;
      index += 1;
      return index < argv.length ? argv[index] : null;
    };
    switch (name) {
      case '--package': {
        const value = takeValue();
        if (!value) return { error: `--package 缺少包名\n${USAGE}` };
        options.pkg = value;
        break;
      }
      case '--script-kind': {
        const value = takeValue();
        if (!value) return { error: `--script-kind 缺少脚本种类\n${USAGE}` };
        options.scriptKind = value;
        break;
      }
      case '--mode': {
        const value = takeValue();
        if (!value) return { error: `--mode 缺少包型\n${USAGE}` };
        options.mode = value;
        break;
      }
      case '--require-unit': {
        const value = takeValue();
        if (!value) return { error: `--require-unit 缺少单元名\n${USAGE}` };
        options.units.push(value);
        break;
      }
      case '--source-script': {
        const value = takeValue();
        if (!value) return { error: `--source-script 缺少路径\n${USAGE}` };
        options.sourceScript = value;
        break;
      }
      case '--allow-unobserved-template':
        options.allowUnobservedTemplate = true;
        break;
      case '--list-templates':
        options.listTemplates = true;
        break;
      case '--help':
      case '-h':
        options.help = true;
        break;
      default:
        if (arg.startsWith('--')) return { error: `未知选项 ${arg}\n${USAGE}` };
        options.files.push(arg);
    }
  }
  return options;
};

const isMain =
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  const options = parseCliArgv(process.argv.slice(2));
  if (options.error) {
    console.error(options.error);
    process.exit(2);
  }
  if (options.listTemplates) {
    for (const template of listTemplates()) {
      console.log(
        `${template.marker}\t${template.name}\t[${template.scripts}]` +
          `\tservice-invoking=${template.invokesService}` +
          `\tobserved=${template.observed}\tupstream=${template.upstreamVerified}`,
      );
    }
    process.exit(0);
  }
  if (options.help) {
    console.error(USAGE);
    process.exit(0);
  }
  const missing = [];
  if (!options.pkg) missing.push('--package');
  if (!options.scriptKind) missing.push('--script-kind');
  if (!options.mode) missing.push('--mode');
  if (options.units.length === 0) missing.push('--require-unit');
  if (options.files.length !== 1) missing.push('恰好一个脚本文件');
  if (missing.length > 0) {
    console.error(`缺少必需参数：${missing.join('、')}\n${USAGE}`);
    process.exit(2);
  }

  const file = options.files[0];
  let text;
  try {
    text = readFileSync(file, 'utf8');
  } catch (err) {
    console.error(`${file}: 读不到待审计脚本：${err.message}`);
    process.exit(1);
  }
  let sourceText = null;
  if (options.sourceScript !== undefined) {
    try {
      sourceText = readFileSync(options.sourceScript, 'utf8');
    } catch (err) {
      console.error(`${options.sourceScript}: 读不到 --source-script 指定的仓库源：${err.message}`);
      process.exit(1);
    }
  }

  const result = gateMaintainerScript({
    text,
    pkg: options.pkg,
    scriptKind: options.scriptKind,
    mode: options.mode,
    units: options.units,
    sourceText,
    allowUnobservedTemplate: options.allowUnobservedTemplate === true,
  });
  if (result.ok) {
    const names = result.matched.map((m) => `${m.template}(${m.units.concat(m.tmpfiles).join(' ') || '-'})`);
    console.log(
      `ok: ${options.pkg}/${options.scriptKind} mode=${options.mode} ` +
        `模板 ${names.length === 0 ? '（无自动片段）' : names.join(' + ')}` +
        (options.allowUnobservedTemplate ? ' [含非实测槽位模板]' : ''),
    );
    process.exit(0);
  }
  for (const failure of result.failures) {
    console.error(`${options.pkg}/${options.scriptKind}:${failure.line}: ${failure.reason}`);
  }
  process.exit(1);
}
