# 打包审查收尾报告（production-v3-tunnel / packaging-fix）

日期：2026-10-23（Asia/Shanghai）。round1 范围：`.superpowers/sdd/2026-10-23-production-v3-tunnel/packaging-fix-brief.md`
列出的 debian/、`.github/workflows/ci.yml`、`scripts/tests/` 及配套小型脚本；
**§8 是同日的 round2**（`packaging-fix-round2-brief.md`：CI 容器 shell 口径 + 审计漏报 +
逐单元绑定），只碰 `scripts/lib/systemd-maintainer-audit.mjs`、两个 scripts 测试、
`ci.yml` 与本报告，§1～§7 描述 round1，其中被 round2 修正的口径已就地标注；
**§9 是同日的 round3**（`packaging-fix-round3-brief.md`：有限 AST 的证明边界——
短路/分支先后/DI 条件列表/赋值值内替换/参数扩展/两个 Quality 边界/合同绑定），
只碰 lib、两个 scripts 测试与本报告；§8.3/§8.4 中被 round3 修正的口径已就地标注。
**§10 是同日的 round4（`packaging-template-brief.md`）：架构整体收缩为「严格模板门禁」。**
自制 shell 解释器（tokenizer + 有限 AST + 可达性）连同其全部启发式 CLI 已被删除，
§3.3/§3.4/§8.3/§8.4/§9 描述的实现与口径已被替换，作为决策与证据史保留（各节已就地
标注 historical），其结论只在 §10 重新成立的范围内继续有效。**当前生效的唯一口径是 §10。**
未触碰 Rust 源码、任何 Cargo.toml/lib.rs，未 commit/push，未运行 cargo clean，
未做全仓 release 编译。本报告仅含去敏结果。

## 1. 结论速览

| 项 | 状态 |
| --- | --- |
| `Build-Depends: debhelper (>= 13.11.6~)` + 保留 `debhelper-compat (= 13)` | 已加（构建工具下限，非 Rust 锁定） |
| CI 对 rsetup-next postinst 的 **正断言**（`deb-systemd-helper enable` + 三单元逐一覆盖） | 已实现（不要求单元名“仅出现一次”） |
| 生命周期区分（安装/升级不 start/restart/stop；remove 允许 stop） | 已实现（新审计脚本 + 合同测试） |
| 源码合同回归（防删版本下限 / enable 正断言 / Bookworm 链接断言） | 已加，RED→GREEN |
| Bookworm 安装 test-L 三链接 + controller/board 无链接断言 | 原样保留并被合同钉住 |
| 移除无依据的 70 行 .dpkg-bak 推测性迁移章节 + changelog 推测措辞 | 已删/压缩为事实性段落 |
| rules/README.source 精确描述下限；不再以“脚本无激活”充当成功证据 | 已改写 |
| board UMask=0077 | 未新增（brief 条件不成立，不扩展无关 sandbox） |
| Node 合同测试 `node --test scripts/tests/*.test.mjs` | round1 37/37；round2 60/60（§8）；round3 74/74（§9）；round4 初审 67/67；**复审修复后 70/70 通过**（§10.8） |
| **round4 生效架构：严格模板门禁（逐字匹配已登记的 debhelper 13.24.2 上游模板，未知来源/任何变异一律失败）** | **lib + 46 条行为合同 + 24 条 CI 合同 + ci.yml 6 次逐包逐脚本调用；复审 Spec/Quality PASS（§10.9），不含真实产物验收** |
| container 作业内 `<<<` 的 shell 口径（`shell: bash`） | round2 已修 + 合同防删（§8.2，round4 保留） |
| 行级审计漏报（续行 / `fi` 后命令 / 一行多调用 / `else`/`elif` / `case` 主题 / `\|\|` 混入无关分支） | 〔historical〕round2 logical line + 有限 AST（§8.3/§9）；**round4 整体删除解释器，这些形状现在统一因「不是已登记模板」被拒（§10.1/§10.4）** |
| 逐单元 enable 断言与守卫**绑定**（`--require-active-enable-unit` / `--enable-guard-unit`） | 〔historical〕round2/round3 的这两个选项在 round4 删除；绑定改由模板 `#UNITFILE#` 参数位 + `--require-unit` 集合精确比较承担（§10.2/§10.3） |
| round3 证明边界（短路、elif/else/case 先后、DI 只认条件位末命令、精确 `$1`、赋值值内替换、恒假 `||` 支、引号重定向、Unknown 上下文拒审） | 〔historical〕RED-19…RED-30 的语义判定已随 AST 一起删除；这些反例在 round4 全部作为「匹配不上模板」的负例保留（§10.4） |
| Python 验收账本 `scripts/tests/check_controller_acceptance_test.py` | 28/28 通过 |
| `git diff --check` | 本 writer 全部文件干净（整仓 check 仅剩他 writer 文件 `crates/rsetup-protocol/Cargo.toml` 的 EOF 空行噪音，不归本 writer 处理） |
| Docker：Trixie helper 最小构建 + Bookworm policy-rc.d=101 安装验证 | 仅 round1（§6）；**round2 未跑 Docker，见 §8.7** |

## 2. 事实依据（来源精确）

- debhelper 官方 changelog
  <https://metadata.ftp-master.debian.org/changelogs/main/d/debhelper/unstable_changelog>：
  - 13.11.6 条目 “Support units below /usr” —— `dh_installsystemd` 从该版本起解析
    `/usr/lib/systemd/system` 下的单元。本源码包三个包的单元全部装在 /usr 下，低于
    该版本的 helper 会“没看见单元”、生成不含任何激活片段的脚本，把打包伪装成通过。
    因此 `debian/control` 增加 `debhelper (>= 13.11.6~)`：由 `dpkg-checkbuilddeps`
    在**任何构建规则运行前**失败；这是构建工具下限，不是 Rust 版本锁定；用
    `dpkg-buildpackage -d` 绕过依赖检查后再宣称打包通过不在支持口径内。
  - 13.25 条目 “Always stop the service on remove (#1079181)” —— ≥13.25 工具链生成
    的 prerm 在 **remove** 分支总会 stop。因此“任何脚本出现任何 stop”不能作为
    安装/升级违规的判据；本包承诺口径限定为**安装与升级不自动 start/restart/stop**，
    正常 remove 停止运行服务是允许的生命周期行为。
  - 主代理已独立 fetch 上述两个条目；本次实测的 Debian 13 (trixie) 归档候选为
    debhelper **13.24.2**（`apt-cache policy debhelper` 于 debian:trixie-slim 内实测），
    介于两者之间：实测产物不会含 13.25 的 remove-stop 片段，该行为按 changelog 口径
    在断言与文档中前瞻性区分（见 §6 limits）。
  - **round4 复审纠正**：曾把 sources.debian.org 版本列表中缺少条目误写成
    “13.25 不存在”，该结论撤回。主代理与主审均独立取得官方 changelog：13.25 发布于
    2025-08-10，明确包含上述 #1079181 条目，后面还有 13.26～13.32。版本存在与当前
    source 浏览归档是否保留对应条目是两件事。remove-stop 行为变更以该 changelog 为据；
    具体生成片段仍须核对 helper 生成逻辑，不能从裸 `prerm-systemd` 模板推断外层守卫。
    主审核实 13.24.2/13.25 的该裸模板相同且不含 `$1` 守卫；13.24.2 在本仓 flags 下
    不注入它。门禁仍然拒绝未登记 helper/版本，不凭 changelog 自动放行新版本。

## 3. 变更清单

### 3.1 debian/control
- `Build-Depends` 增加 `debhelper (>= 13.11.6~)`，保留 `debhelper-compat (= 13)`；
  注释写明下限来源、失败时机（checkbuilddeps 于构建前）、`-d` 绕过不算支持、
  以及“构建工具下限 ≠ Rust 版本锁定”。

### 3.2 .github/workflows/ci.yml（`Assert Debian systemd maintainer scripts` 步骤重写）
- 源码侧：三包 `dh_installsystemd` 必须显式带 `--no-start`（既有）+ controller/board
  带 `--no-enable`（新）+ rsetup-next **禁止** `--no-enable`（新，热策略/风扇/LED
  必须随包 enable）。
- 产物侧（替换原一刀切 `grep 'deb-systemd-invoke|_dh_action='`）：
  - 对解包出的 postinst/preinst/prerm/postrm 逐一运行
    `scripts/lib/systemd-maintainer-audit.mjs`，区分“动作 × 生命周期分支”；
  - **正断言**：rsetup-next postinst 必须含 `deb-systemd-helper enable`，且用 awk 只收集
    enable 命令本体（含反斜杠续行）后逐一核对三个单元
    `rsetup-next-thermal-policy.service`、`rsetup-next-fan-curve.service`、
    `rsetup-next-led-state.service` 均被处理；单元名允许在 helper 的
    unmask/was-enabled/enable 多段正常引用中出现，不要求“仅出现一次”；并叠加
    `--require-active-enable`（必须存在不在 debian-installed 守卫内的活跃 enable）；
  - 反向：controller/board 的 postinst 用 `--enable-guard` 断言「每个
    `deb-systemd-helper enable` 调用都位于 `debian-installed` 守卫内」——不是
    “没有 enable 字样”（该写法被真实产物证伪，见 §6.2）；
  - 保留 `StateDirectoryMode=0700` 与 `UMask=0077` 单元断言。
- `debian-12-runtime` 作业的 Bookworm 断言原样保留：三个 enable 符号链接 `test -L`
  与 controller/board `test ! -e`，并由新合同测试防止被删。

### 3.3 scripts/lib/systemd-maintainer-audit.mjs（新）〔historical：该文件在 round4 被整体重写，行级/AST 审计已删除，见 §10〕

> **round2 修正（见 §8.3）**：本节与 §3.4 描述的是 round1 的实现。round1 的「行内含字样即
> 判定 + `if`/`fi`/`else` 首物理行正则 + 结构行 `continue`」实现**做不到**本节宣称的
> 「Fail-Closed，任何认不出的形态按违规处理」：认不出的形态不是被「保守判违规」，而是
> 被整行跳过（漏报）；`--enable-guard` 的「每个 enable 调用都在 debian-installed 守卫内」
> 也按行号相邻 + `then`/`fi` 计数推断，跨 `else`/同行 `fi` 会错误继承。两条承诺当时均
> 被夸大，本轮已按真实解析重写并补反例合同。以下条目保留原文以便对照。
纯函数 + CLI 的维护脚本审计。默认（生命周期）模式：
- 唯一允许的 `deb-systemd-invoke` 形状 = 「仅 remove/deconfigure 守卫下的 stop」
  （支持 `if [ "$1" = "remove" ] || [ "$1" = "deconfigure" ]` 与 `case remove|deconfigure)`
  两族形态）；
- 无守卫调用、configure/upgrade/install/abort-* 守卫下的任何调用、非 stop 动作
  （start/restart/reload-or-restart…）、`_dh_action` 赋值或间接使用、`*)` 默认分支、
  `!=` 与不可解析的 $1 比较、else/elif 分支——一律违规（Fail-Closed）；
- `deb-systemd-helper`（enable/disable 记账，不与服务对话）不在生命周期审计范围；
- 不忽略任何 stop：升级守卫下的 stop、无守卫 stop 均判违规。
enable 记账的包型断言（按 §6.2 实测形态设计）另有两个模式：`--enable-guard`
（--no-enable 包的 enable 调用必须全部在 `debian-installed` 守卫内）与
`--require-active-enable`（默认-enable 包必须存在活跃 enable，防空洞通过）。

### 3.4 scripts/tests/systemd-maintainer-audit.test.mjs（新）〔historical：round4 重写为模板门禁合同，旧语义用例已替换，见 §10.4〕
20 条行为合同：enable 记账允许；无条件 start、configure/upgrade 守卫下 start、
`_dh_action` 间接形态、升级 stop、无守卫 stop、remove 分支下非 stop 动作、
case 混合分支（`remove|upgrade)`）与默认分支均违规；if/case 两种 remove-only
守卫下的 stop 与嵌套中性守卫允许；注释行不审计；CLI 退出码与缺文件 Fail-Closed；
`analyzeEnableGuards` 与 `--enable-guard`/`--require-active-enable` 按真实产物
fixture（debhelper 13.24.2 生成物原文形状）正反向验证。

### 3.5 scripts/tests/controller-ci-contract.test.mjs（round1 新增 4 条，round2 再增 4 条）
- `debian/control` 必须含 `debhelper ( >= 13.11.6~ )` 与 `debhelper-compat (= 13)`；
- rules 三包 `dh_installsystemd` 选项逐一钉住（三包 `--no-start --no-stop-on-upgrade`；
  controller/board `--no-enable`；rsetup-next 禁 `--no-enable`）；
- CI 断言步骤必须含 `deb-systemd-helper enable` 正断言、三个单元名逐一出现、引用
  审计脚本，并**禁止**回到 `deb-systemd-invoke|_dh_action=` 一刀切；
- Bookworm 作业的 `test -L` / `test ! -e` 断言与五个单元名不得被删。

### 3.6 debian/rules 与 debian/README.source（注释/文档精确化）
- 删除“helper 只扫 debian/<pkg>/lib/systemd/system，旧版本下调用是 no-op……
  两种情况都算通过”的旧段落——它把“旧 helper 没看见单元导致脚本无激活”当成了
  可接受证据。改为：下限 13.11.6 + checkbuilddeps 构建前失败 + CI 正断言防空洞通过；
  明确 `-d` 绕过使证据失效；记录 13.25 remove-stop 边界与审计口径。
- README.source 新增 “Build-tool floor: debhelper (>= 13.11.6~)” 一节，同口径展开，
  并明确这不是 MSRV（工作区不锁定 Rust 版本，CI 统一 stable）。

### 3.7 文档口径修正（移除无依据内容）
- `debian/rsetup-controller.README.Debian`：整段删除 70 行推测性
  “Upgrading from an older package / .dpkg-bak / .dpkg-old / Case A/B” 迁移章节
  （用户明确尚无部署、不做账户升级；普通升级并不会把 conffile 改名，原文案以
  推测冒充事实）。压缩为事实性段落：doc 示例配置不自动装到 /etc、缺失时
  ConditionPathExists 保持 unit inactive、按 §1 用一条 `install -m0600` 创建；
  生命周期口径限定为安装/升级不 start/restart/stop，并注明 remove 分支可能停止
  运行中的服务（debhelper ≥ 13.25 总是如此，官方 changelog + Debian bug #1079181）。
- `debian/changelog`：删除“.dpkg-bak suffix / Restore or rebuild / daemon-reload”
  推测性迁移 bullet，换成两条事实性 bullet（示例配置移至 doc + 构建工具下限）。
- `debian/rsetup-board-agent.README.Debian`：把“never stops an agent you are
  already running”限定到安装/升级路径，补 remove 边界同口径说明。
- 未改动 `data/*.service`（board 未新增 UMask=0077，因此 brief 第 6 条的
  “同时加 0700 断言”条件不成立；controller 的 UMask/StateDirectoryMode 已有 CI
  断言且与 User/StateDirectory 一致）。

## 4. TDD 过程（RED → GREEN）

1. RED：先写合同（`controller-ci-contract.test.mjs` 新增 4 条 +
   `systemd-maintainer-audit.test.mjs` 17 条）。当时结果：
   `# pass 15 / # fail 3`——debhelper 下限合同、CI enable 正断言合同失败，
   审计模块 ERR_MODULE_NOT_FOUND。
2. GREEN：按 §3 实现（control / ci.yml / 审计脚本 / rules / README.source /
   README.Debian / changelog）。
3. 中途测试自身的两处 fixture 解构笔误（漏 `.violations`）已修正；修正后
   `node --test scripts/tests/*.test.mjs` → 34/34。
4. **Docker 取证轮（第二轮 RED→GREEN）**：真实产物证伪了首版反向断言
   （§6.2：--no-enable 包 postinst 也含 enable 字样）。按 TDD 先补 3 条新合同
   （`analyzeEnableGuards` 形态区分、`--enable-guard`、`--require-active-enable`，
   fixture 用真实产物原文形状），确认旧实现不满足后扩展审计模块并同步改 CI 步骤；
   Node 合同再加 2 条防回归断言（CI 必须用 `--enable-guard`、禁止 enable 字样 grep）。
5. 最终：`node --test scripts/tests/*.test.mjs` → **37 tests, 37 pass, 0 fail**
   （controller 合同 17 + 审计合同 20）。
6. 回归：`python3 -m unittest scripts.tests.check_controller_acceptance_test`
   → 28/28 OK；`git diff --check` → 干净；`.github/workflows/ci.yml` YAML 解析通过。

## 5. 断言语义对照（为什么不是“每个名字只出现一次”或“任何 stop 都报错”）

- debhelper 对每个单元生成的 enable 片段本就包含 unmask / was-enabled / enable 多段
  正常引用（单元名出现 ≥3 次）；合同与 CI 都只要求“出现在 enable 命令的处理范围
  内至少一次”。
- 安装/升级 = configure/upgrade 路径：不得出现任何 `deb-systemd-invoke` 或
  `_dh_action`（含 stop）——`--no-start` + `--no-stop-on-upgrade` 的真实含义。
- remove = 卸载路径：debhelper ≥ 13.25 生成的 stop 允许存在；把它误报成“安装/升级
  自动激活”正是本次修掉的误报口径。

## 6. Docker 实证与 limits

方法：Docker 可用（本地 `debian:trixie-slim`/`debian:bookworm-slim` 镜像，镜像源换为
境内镜像仅为速度）。**不构建完整本仓产物**（需 npm 联网 + cargo 全量 release 编译，
被任务约束禁止），改用最小 harness 源码包 `rsetup-packcheck`：三条
`dh_installsystemd` 调用与本仓逐字同参（`-p…-main --no-start --no-stop-on-upgrade`；
`-p…-noenable --no-enable --no-start --no-stop-on-upgrade`；board 同），staging 安装
本仓**真实 unit 文件**（`data/` 下五个 .service 原文件）。harness 目录与产物已在
验证后删除，不入库。

### 6.1 Trixie（debhelper 13.24.2，`dpkg-query` 容器内实测）最小构建
- `dpkg-buildpackage -us -uc -b` 成功产出 3 个 .deb。
- 解包 control 后对全部 postinst/postrm 跑生命周期审计：6/6 `ok`（无任何
  `deb-systemd-invoke`）。
- rsetup-next 路径包 postinst **正断言实测通过**：含 `deb-systemd-helper enable`；
  awk 收集 enable 命令后三个单元逐一命中；`--require-active-enable` 通过。
- 单元名在主包 postinst 中出现 **4 次**（unmask/was-enabled 注释/enable/if），
  实证“仅出现一次”式断言必然错误。

### 6.2 关键发现：`--no-enable` 包 postinst 也含 enable 字样（反向断言被证伪并修正）
Trixie 真实产物显示 `--no-enable` 包 postinst 包含
`deb-systemd-helper enable 'rsetup-controller.service'`，但整块包在
`if deb-systemd-helper debian-installed 'x'; then`（首装 false，不建链接）里，
而默认-enable 包的 enable 位于 `deb-systemd-helper --quiet was-enabled` 守卫
（生成注释原文 “was-enabled defaults to true, so new installations run enable”）。
因此“controller/board postinst 不含 `deb-systemd-helper enable` 字样”是**假断言**，
会把合法产物判红。已修正为守卫形态断言：审计脚本新增
`--enable-guard`（enable 必须全在 debian-installed 守卫内，用于 --no-enable 包）与
`--require-active-enable`（必须存在活跃 enable，用于默认-enable 包），fixture 直接
取自本次真实产物；CI 与负向对照（包型互换必失败）均实测通过。Node 合同新增防回归
断言（步骤必须用 `--enable-guard`、不得回退到 enable 字样 grep）。

### 6.3 Bookworm（policy-rc.d exit 101 实测在位）安装验证
- 容器内先装 `systemd`（提供 deb-systemd-helper），再 `apt-get install ./out/*.deb`。
- dpkg configure 输出即证据：“Created symlink
  /etc/systemd/system/multi-user.target.wants/rsetup-next-{fan-curve,led-state,
  thermal-policy}.service → /lib/systemd/system/…”（merged-usr 下 /lib 即 /usr/lib）。
- 断言全过：三单元 `test -L` 通过；`rsetup-controller.service`、
  `rsetup-board-agent.service` 均无 enable 链接（`test ! -e`）。
- 未启动任何服务：policy-rc.d=101 且产物 postinst 本就不含
  `deb-systemd-invoke`，`systemctl` 未被调用；slim 镜像无 `ps`，以“grep 进程名
  无输出 + 无 start 片段”作为无服务启动的证据（limit）。

### 6.4 limits（如实）
- harness ≠ 完整本仓包：证明的是 **debhelper 片段生成与 Bookworm 安装行为**，
  完整产物断言（enable 三单元、UMask/StateDirectoryMode、lintian）仍由 CI 执行。
- Bookworm 侧 `deb-systemd-helper`/dpkg 版本为 Bookworm 归档当前版，与本仓
  `0.5.0-1` changelog 的 bookworm 发行口径一致。
- 第一轮 harness 试跑曾因容器 cwd/相对路径与 `|| true` 吞错出现两次假失败
  （units 路径、apt “Unsupported file”），均已定位为验证脚本问题而非打包问题并复测通过。


## 7. 遗留边界（如实声明）

- debhelper 13.25 的 “Always stop the service on remove” 行为以官方 changelog 为据，
  实测环境（Trixie 归档 13.24.2）无法生成该片段做端到端验证；对应行为由
  `systemd-maintainer-audit.test.mjs` 的 synthetic fixture 覆盖（if/case 两族形态）。
  未来工具链升到 ≥13.25 时，CI 审计预期放行「remove-only 守卫下的 stop」、继续拦截
  其它一切——该边界已写入 rules/README.source/README.Debian。
- Docker 验证用最小 harness 包（同三条 `dh_installsystemd` 命令行、同五个真实 unit
  文件）而非完整 `dpkg-buildpackage --build=binary`：完整构建需要 npm 联网安装与
  cargo 全量 release 编译，二者被本次任务约束禁止；harness 证明的是 helper 片段
  生成与 Bookworm 安装链接行为，不替代 CI 的完整产物断言。
- 未运行真实服务：全程 policy-rc.d=101 且产物 postinst 本身不含任何
  `deb-systemd-invoke`，`systemctl` 未被调用、无服务被启动。
- 报告与新增文件均不含凭据或环境敏感值；`secret/` 未被读取。
- round2 追加的边界见 §8.3（受支持语法范围与「未知即拒绝」清单）与 §8.7（本轮未跑
  Docker 与 CI）；`for`/`while`/here-document/`[[ ]]` 一类结构目前是**明确失败**，
  若未来 debhelper 开始生成它们，正确动作是扩展解析器 + 补反例合同，不是放宽判定。

---

## 8. round2（同日第二轮）：CI shell 口径与审计漏报

范围仍只有 5 个文件：`scripts/lib/systemd-maintainer-audit.mjs`、
`scripts/tests/systemd-maintainer-audit.test.mjs`、
`scripts/tests/controller-ci-contract.test.mjs`、`.github/workflows/ci.yml`、本报告。
未改 Rust、`Cargo.toml`、`debian/` 打包规则与 unit 文件，未 commit/push，未 `cargo clean`，
未读取 `secret/`，未做无部署升级迁移。

### 8.1 本轮修的三件事

1. `debian-package` 是 `container: debian:trixie-slim` 作业，而新增的
   `Assert Debian systemd maintainer scripts` 步骤用了 here-string `<<<`；
2. 行级审计不是真 Fail-Closed，存在成体系的**漏报**；
3. 逐单元正断言与守卫形态**脱钩**：一个单元活跃 + 另两个只有 `debian-installed` 守卫时
   整体依然全绿。

### 8.2 container 作业的 shell 口径（阻断项 1）

- 根因：GitHub Actions 对 **container 作业**的默认 shell 是 `sh -e {0}`（不是
  `ubuntu-latest` 上的 bash），`debian:trixie-slim` 的 `/bin/sh` 是 dash；该步骤里的
  `grep -q … <<<"$line"` 是 bash 专有语法，在 `sh` 下直接语法错误。
- 修复（最小方案）：该步骤显式 `shell: bash`（未把整个作业改成 bash，也没把 5 处
  `<<<` 全部改写成 POSIX）。bash 在 trixie-slim 基础镜像里本来就有。
- **判定依据只用 `<<<`**。不使用「本机旧 dash 不认 `set -o pipefail`」作为 Trixie 的
  证据：Debian 的 dash 带 pipefail 补丁，Trixie 的 `sh` 支持 `set -o pipefail`，拿这个
  外推属于错误论证。合同同样只把 `<<<` 当 bashism 触发条件（注释里写明理由，防止后来
  者把 pipefail 加回判据）。
- 防删合同（`controller-ci-contract.test.mjs`）：
  `every container-job step using a bash here-string declares shell: bash`（扫全部
  container 作业的每个步骤）与
  `the systemd assert step declares shell: bash (container default sh + <<< usage)`
  （点名步骤）。两条在改动前均为 RED。

### 8.3 行级审计重写（阻断项 2）〔historical：round3 替换为有限 AST（§9），round4 再整体删除，见 §10〕

round1 的实现按「首物理行正则 + 结构行 `continue`」工作，本轮改为：**先合并 logical
line（词元层面处理反斜杠续行），再用尊重引号/注释/命令分隔符/重定向的 tokenizer 切词，
交给有限递归下降 parser 构出 AST（`if/then/[elif…]/[else]/fi`、`case…in 模式)…;; esac`、
`{ }` 分组、`&&`/`||`/`|`/`;` 组合、赋值与重定向），最后对 AST 里每一个调用点判定守卫**。

审查逐条复现的漏报（全部先 RED 再 GREEN，见 `systemd-maintainer-audit.test.mjs` 的
RED-1…RED-14）：

| 反例 | round1 行为 | round2 |
| --- | --- | --- |
| `if [ "$1" = "remove" ] || \` + 续行 `[ "$1" = "upgrade" ]` + stop | 只看到 remove → 放行 | 违规（取值集合含 upgrade） |
| `fi; deb-systemd-invoke restart …` | 结构行 `continue` → 漏 | 违规（restart，且行号指向真实调用） |
| `stop …; restart …` 同一行 | 只报 1 条 | 报 2 条 |
| `else` 分支体内（分行与单行 `else … ; fi`） | 单行 `else` 漏 | 违规（继承 configure 守卫） |
| `elif [ "$1" = upgrade ]` 分支 | 靠 unknown 兜底 | 用**自身**条件判定，行号正确 |
| `if [ "$1" = remove ] \|\| [ -d / ]` | 当作 remove-only → 放行 | 违规（`\|\|` 有一支不约束 $1 → 整条不约束） |
| `case "$2" in remove)` | 误当 `$1` → 放行 | 违规（主题不是 `$1`） |
| enable 落在 `debian-installed` 守卫的 `else` 分支 | 误判「在守卫内」 | `guard=outside`（else 恰是「非 installed」） |
| `fi; deb-systemd-helper enable …` 同一行 | 误判「在守卫内」 | `guard=outside` |
| `if false; then … enable …; fi` | `--require-active-enable` 当活跃 | 不可达 → 不算活跃 |

守卫集合的合并口径（写在脚本头部注释里）：`&&` 求交、`||` 求并，`||` 里只要有一支与
`$1` 无关整条条件即不约束 `$1`；`elif` 用自身条件；`else` 只继承外层；`case` 分支仅在
主题恰为 `$1` 时取模式集合，`*)` 通配不限定；取交为空即恒假分支 → 违规。
（*round3 修正：`||` 的**恒假支**（`|| false`、`|| [ 1 = 2 ]`）按真值不参与求并，不污名化
remove-only 守卫；`&&`/`||` 的右侧**是否执行**（短路）参与可达性；`elif`/`else`/`case`
分支的先后次序参与可达性（前支恒真/已匹配则后支不可达）；本段「`||` 求并」「`*)` 不限定」
的最终口径以 §9.2 为准。*）

**未知形态明确拒绝，不再默认 neutral**：`for`/`while`/`until`/`select`/函数定义、
here-document、`[[ … ]]`、`test` 内 `-o` 复合条件、`!=`/`-ne` 否定比较、`$1` 与变量
比较、`;|&`/`;;&` 分支终止符、未闭合引号/`if`/`case`、无法推进的 token —— 均报违规并
要求人工扩展解析器。

**如实的口径边界**（勿再当成通用能力）：本脚本是 CI 质量门禁，**不是通用 shell
解释器，也不是恶意脚本的安全沙箱**；已知限制是保留字（`then`/`fi`/`esac`/`in`）作为
命令实参会提前结束该命令，重定向只按词元跳过不做 fd 语义建模。可达性判定对「与 `$1`
无关的运行时条件」（`[ -d /etc/systemd/system ]`、`deb-systemd-helper was-enabled`）按
debhelper 语义视为首装路径可达，对**恒假**守卫（`false`、常量假比较）判不可达，对引用
`$1` 却认不出的条件判「不可证明可达」。
（*round3 修正与收紧：was-enabled/目录两条改为**显式声明的 Maybe 假设**，未知（Unknown）
真值上下文里的敏感调用从「不可证明可达」升级为**明确违规（Unsupported 拒审）**；
`if c1; c2; then` 这类条件位多条命令的真值/守卫只认**最后一条命令**；重定向目标改为
引号感知消费。完整边界见 §9.2。*）

### 8.4 逐单元绑定断言（阻断项 3）〔historical：`--require-active-enable-unit`/`--enable-guard-unit` 两选项在 round4 删除，绑定改由模板参数位承担，见 §10.3〕

新增两个可重复的 CLI 选项（值可用空格或 `=` 传入，未知选项退出码 2）：

- `--require-active-enable-unit <unit>`：该单元必须出现在**某条可证明活跃的**
  `deb-systemd-helper enable` 命令的**字面单元参数**里。「可证明活跃」= 不在
  `debian-installed` 守卫内 **且** 所在分支在 `$1=configure` 首装路径可达。
  （*round3 收紧「可达」与「守卫」的证明口径：可达性为显式三值 + Unknown 拒审；
  DI 守卫只由条件位最后一条命令证明、elif 用自己的条件——见 §9.2/§9.3。*）
- `--enable-guard-unit <unit>`：`--no-enable` 包侧，该单元必须真的有 enable 记账，且其
  全部调用都在 `debian-installed` 守卫内。

单元名只取自该命令的实际词元：注释里列出三个目标单元、`unmask`/`was-enabled` 行提到该
名字、或写成 `"$UNIT"` 变量，都不能充数（`unitsFromVariable` 记录后者）；同一条命令里
多个单元、反斜杠续行的单元列表都正确解析。仍然**不要求**单元名「仅出现一次」。

CI 侧（`shell: bash` 步骤内）接线为逐单元循环，替代原「awk 收集 enable 命令 + grep
单元名」：

```sh
node scripts/lib/systemd-maintainer-audit.mjs --require-active-enable "$next_postinst"
for unit in rsetup-next-thermal-policy.service rsetup-next-fan-curve.service rsetup-next-led-state.service; do
  node scripts/lib/systemd-maintainer-audit.mjs --require-active-enable-unit "$unit" "$next_postinst"
done
for pkg in rsetup-controller rsetup-board-agent; do
  unit="${pkg}.service"
  ...
  node scripts/lib/systemd-maintainer-audit.mjs --enable-guard --enable-guard-unit "$unit" "$tmp/$pkg/postinst"
done
```

两条新合同按 **shell 结构**提取 for 循环（循环头 + 到匹配 `done` 的循环体）来钉住绑定，
而不是断言「字符串存在于步骤里」：`unit` 循环列表必须恰好是三个目标单元且循环体对
`$unit` 执行 `--require-active-enable-unit`；`pkg` 循环必须对 `${pkg}.service` 执行
`--enable-guard-unit` 并保留全局 `--enable-guard`；同时禁止「awk + enable 字样」式正断言
回潮。四条合同改动前全部 RED。

### 8.5 TDD 过程（RED → GREEN）

1. RED：新增审计合同 19 条 + CI 合同 4 条。旧实现下 `systemd-maintainer-audit.test.mjs`
   `# pass 25 / # fail 14`，`controller-ci-contract.test.mjs` `# pass 17 / # fail 4`。
   （新增 19 条里 5 条按设计旧实现也通过：RED-4/RED-5 属旧实现已部分覆盖的分支、
   RED-8 是「修漏报不得把真实嵌套判红」的反向保护、RED-15 显式记录旧全局断言的洞。）
2. GREEN：重写审计脚本（tokenizer + 有限 AST + 逐单元 CLI）、ci.yml 加 `shell: bash`
   与逐单元接线、合同按结构化方式重写。
3. 中途修的两处自身缺陷：`case` 分支体没有把 `;;` 当终止符（导致合法 remove 分支误报）、
   `$1` 字面判定正则 `\}$` 漏写可选右括号（导致 `case "$1" in` 主题判定恒假）。两处都由
   既有 RED 反例直接暴露，不是靠新增测试掩盖。
4. 最终：`node --test scripts/tests/*.test.mjs` → **60 tests / 60 pass / 0 fail**
   （`controller-ci-contract` 21 + `systemd-maintainer-audit` 39）。round1 的 37 条
   （17 + 20）**全部原样保留且通过**。
5. 回归：`python3 -m unittest scripts.tests.check_controller_acceptance_test` → 28/28 OK；
   `git diff --check`（本 writer 5 个文件）干净；`python3 -c "yaml.safe_load(...)"` 解析
   `.github/workflows/ci.yml` 通过（5 个 job；该步骤 `shell: bash`；run 79 行）；
   `node --check` 两个测试文件与本模块均通过。

### 8.6 语义与产物侧验证（本轮实际跑过的）

- **fixture `sh -n` 语法检查**：debhelper 13.24.2 形态的正常 Trixie postinst（三单元
  was-enabled/enable/unmask）、`--no-enable` postinst（`debian-installed` 守卫形态）、
  postrm（mask/purge）、synthetic 13.25 remove/deconfigure stop 及其变异体，全部
  `dash -n` 与 `sh -n` 通过（临时目录，验证后已删除）。
- **安全改名 stub 的真实语义验证**（`env -i PATH=<仅含 stub 的目录>` 执行，绝不触碰真实
  `deb-systemd-helper`/`deb-systemd-invoke`/`systemctl`/服务；本机确实存在
  `/usr/bin/deb-systemd-helper`，因此桩件目录是 PATH 上唯一目录，且未跑任何带
  `-x /usr/bin/deb-systemd-helper` 分支的脚本）：

  | 执行 | 桩件记录 |
  | --- | --- |
  | 正常 Trixie postinst `configure` | `unmask`×3、`was-enabled`×3、**`enable`×3** |
  | `--no-enable` postinst `configure` | `debian-installed`、`update-state`，**无 enable** |
  | `if false; then … enable …; fi` | **无任何调用** |
  | synthetic 13.25 prerm `remove` / `deconfigure` | `invoke stop` |
  | synthetic 13.25 prerm `configure` / `upgrade` | **无调用** |

  即：`--require-active-enable-unit` 的正断言、`--enable-guard-unit` 的反断言与
  `if false` 不可达判定，都与脚本真实执行语义一致，不是纯词法巧合。
- **审计 CLI 正反例**（同一批 fixture）：4 份正例生命周期审计 `ok`；变异体（续行条件里
  混入 `upgrade` + `restart`）exit 1 且行号指向第 8 行；正常 Trixie fixture 上
  `--require-active-enable` 与三个 `--require-active-enable-unit` 全 `ok`，而
  `--require-active-enable-unit rsetup-controller.service`（该包不该有）exit 1；
  `--no-enable` fixture 上 `--enable-guard --enable-guard-unit rsetup-controller.service`
  `ok`，包型互换（对默认-enable fixture 跑 `--enable-guard`、对 `--no-enable` fixture 跑
  `--require-active-enable-unit rsetup-controller.service`、要求 board-agent 单元）三条
  全部 exit 1。
- **本仓真实维护脚本无过报**：`debian/postinst`、`debian/rsetup-controller.postinst`、
  `debian/rsetup-next/DEBIAN/postinst`、`debian/rsetup-controller/DEBIAN/postinst`、
  `debian/rsetup-next.postinst.debhelper`（本机历史构建产物，含 `case "$1" in
  configure) … ;; esac`、`command -v systemd-tmpfiles`、`$(stat -c %u …)` 命令替换、
  `adduser --system \` 续行、`#DEBHELPER#`）生命周期审计全部 `ok`，没有解析 Fail-Closed。
- **旧工具链产物被逐单元正断言拦下**：本机这份 `dh_installtmpfiles/13.6ubuntu1` 生成的
  postinst 完全没有 enable 记账（正是 13.11.6 之前「没看见 /usr 下单元」的形态），
  `--require-active-enable-unit` 对三个单元分别 exit 1。
- **形态矩阵**（20 条，内存内跑 `auditMaintainerScript`，结果全部符合预期）：7 条真实
  放行形状（`&&` 嵌套 remove-only、remove 守卫内嵌 `command -v` 中性守卫、
  `case remove|deconfigure)` + `*)` 兜底、`command -v` 守卫、命令替换 + `case` 等）0 违规；
  13 条未知/违规形状（续行、`||` 混入无关分支、`case "$2"`、`fi` 后命令、一行两调用、
  单行 `else`、`[[ ]]`、`test -o`、`$1` vs 变量、`!=`、`for`、`while`、未闭合引号、
  here-document、`;;&`）均 ≥1 违规。

### 8.7 本轮**没有**跑的（如实）

- **Docker 未跑**：本机 docker 守护进程与 `debian:trixie-slim`/`debian:bookworm-slim`
  镜像在位，但本会话的工具审查拒绝了 `docker run`，因此本轮**没有**最小 harness 构建，
  也没有重新做 Bookworm 安装/`test -L` 验证；§6 的 Docker 证据全部属 round1，本轮未复核。
  上面的 fixture 与 stub 验证不能替代容器内的 `dh_installsystemd` 生成过程。
- **GitHub Actions CI 未跑**：未 commit/push，`debian-package` 与 `debian-12-runtime`
  作业（含 `shell: bash` 后的该步骤）没有真实流水线运行记录。
- **完整 dpkg-buildpackage / lintian / cargo / npm 未跑**：本轮不改打包配方，产物侧只做
  了上述只读审计与既有真实产物复核。

## 9. round3（同日第三轮）：有限 AST 的证明边界〔historical：AST 架构在 round4 被整体删除；本节保留的价值正是「为什么不能靠解释 shell 证明」的证据，见 §10.1〕

范围仍只有 4 个文件：`scripts/lib/systemd-maintainer-audit.mjs`、
`scripts/tests/systemd-maintainer-audit.test.mjs`、
`scripts/tests/controller-ci-contract.test.mjs`、本报告。`ci.yml` 本轮**未改**（备审
结论「当前 CI 本身正确」，本轮只加合同的推导/绑定断言）。未改 Rust/`Cargo.toml`/
打包规则/unit，未 commit/push，未 `cargo clean`，未读 `secret/`；不重开版本下限、
不扩「已无部署升级迁移」旧议题。本轮定位是 **CI 质量门禁的有限证明范围**，
没有扩成通用 shell 解释器或恶意脚本沙箱：对认不出的条件/替换/参数扩展子语法
**明确拒绝（Unsupported/Fail-Closed）**，而不是实现解释器。

### 9.1 备审阻断项的逐条修复（全部先 RED 精确复现，再 GREEN）

`systemd-maintainer-audit.test.mjs` 新增 **RED-19…RED-30**（12 条行为合同；RED-30 为
反向保护性对照，在旧实现下即通过）。修复前用独立 Node 探测逐条确认旧实现的实际行为
（下表「修复前」全部为实测输出，不是推断）：

| 编号 | 反例（备审原形） | 修复前（实测） | 修复后 |
| --- | --- | --- | --- |
| R1 | `false && deb-systemd-helper enable`、`true \|\| enable` | `active=true`（短路被跳过的 enable 算活跃） | Never → `active=false`；`true && enable`、`false \|\| enable` 仍 `active=true`（RED-19） |
| R1 | `if true; then :; elif true; then enable; fi` | `active=true`（不看前支已恒真） | elif 需前支**能为假**才可达 → `false`（RED-20） |
| R1 | `if false; then :; elif true; then :; else enable; fi` | `active=true`（else 只否定首条件） | else 需**全部**条件能为假 → `false`；两条件皆恒假时 `true`（RED-20） |
| R1 | `case "$1" in configure) : ;; *) enable ;; esac` | `active=true`（default 不看已被首匹配吞掉） | 首次匹配语义：`configure)` 吞掉首装 → `*)` Never；对照 `remove)` 在前不吞 configure、`*)` 在前则其后分支 Never（RED-21） |
| R2 | `if deb-systemd-helper debian-installed x ; true ; then enable ; fi` | `guard=debian-installed`（只查 `cond[0]`）→ `--enable-guard` 误绿 | 分支体取决于条件位**最后一条命令**：`guard=outside`、`active=true`，`--enable-guard` exit 1（RED-22） |
| R2 | `if false; then :; elif DI ; then enable ; fi` | `guard=outside, active=true`（elif 的 DI 守卫全丢） | elif 用自己的条件证明守卫 → `guard=debian-installed`、`active=false`（RED-23） |
| R3 | `result="$(deb-systemd-invoke restart x)"` / 裸 `$(…)` / 反引号 | 0 违规（只扫 `assign.name`） | ≥1 违规：赋值值内替换在赋值时即执行，替换子语法拒审（Fail-Closed）；`$(deb-systemd-helper enable …)` 同样拒且不产生 enable 记录（RED-24） |
| R3 | `printf 'deb-systemd-invoke restart x'`（单引号文字）、`uid=$(stat -c %u "$t")`（无害替换） | 前者误报 1 条、后者误报「函数定义」（词法在 `(` 处割裂） | 均 0 违规：单引号文字不带 variable 标记不冒充调用；`$( … )`/反引号整块词法消费（RED-25） |
| R4 | `[ "${1:+remove}" = remove ]` 下 `invoke stop` | 0 违规（`${1` 子串被当精确 `$1`） | 只精确接受 token `$1`/`${1}`（去引号后）；参数扩展按 Unknown 拒（≥1 违规）；`case "${1:+remove}" in remove)` 同样拒；`"$1"`/`"${1}"` 照常识别（RED-26） |
| Q1 | `[ "$1" = remove ] \|\| false >/dev/null`、`\|\| [ 1 = 2 ]` | 1 违规（恒假支把并集污名化成 FREE）→ **误拒合法 remove-only** | 恒假支不参与取值集求并 → 0 违规；`\|\| true`、`\|\| [ -d / ]` 仍 1 违规（RED-27） |
| Q2 | `deb-systemd-invoke stop 'x.service' >"/dev/null " 2>&1` | 2 违规（重定向目标按非空白run盲割，引号被割坏 →「未闭合双引号」误 Fail-Closed） | 重定向目标引号感知消费 → 0 违规（RED-28） |
| 边界 | `if [ "$1" = "$FOO" ] ; then enable ; fi`、`[ "$1" = "$FOO" ] && enable` | 0 违规（静默按「不可达」放过，正向断言才偶然拦住） | Unknown 真值上下文中的敏感调用**明确 Unsupported 违规**（RED-29）；正向对照（`&&` 右侧继承守卫、`false \|\| invoke stop` 在外层 remove 守卫内合法、恒真无守卫上下文 stop 仍违规）保持不度不过/不漏（RED-30） |

合同侧（`controller-ci-contract.test.mjs` 新增 2 条）：内存 mutation 把
`unit="${pkg}.service"` 改成 `unit="unrelated.service"`、或把 `--enable-guard-unit` 的
审计对象换成 `$tmp/unrelated/postinst`、或把 `next_postinst` 指向无关文件时，旧的
presence 断言全部照绿——本轮把这些推导链钉成**结构断言**并在测试内现场演示两个
mutation 必须被拦截（同时演示旧 presence 断言确实放行，即 RED 复现点）。

### 9.2 「可证明」的最终口径（有限且如实）

- **可达性三值 + 未知拒审**：`Definitely`（可证明首装执行到）/ `Maybe`（仅在**如实
  声明的 debhelper 语义假设**下可达：`deb-systemd-helper --quiet was-enabled` 首装默认
  为真、`[ -d /etc/systemd/system ]` 首装成立——本脚本不执行 helper，不做状态文件实证）/
  `Never`（恒假、短路跳过、被前支吞掉）；条件真值认不出 = `Unknown`，**含敏感调用
  （invoke/enable/`_dh_action`）的 Unknown 上下文直接判违规**，不再静默按不可达或
  neutral 放过。「可证明活跃 enable」= 不在 DI 守卫内 ∧ 可达 ∈ {Definitely, Maybe}，
  因此它的完整含义是「静态可证 + 上面两条声明假设」，勿再引用成无条件实证。
- **短路参与审计上下文**：`&&` 右侧继承左侧证明的 `$1` 守卫（左侧恒真/恒假/未知分别
  决定右侧 Definitely/Never/Unknown），左侧是 DI 正向检查时右侧落在守卫内；`||` 右侧
  仅在左侧为假时执行（左侧恒真 → Never）。
- **分支先后参与可达性**：`elif k` 要求前面所有条件都能为假；`else` 要求全部条件都能
  为假；`case` 按首次匹配——前一支能匹配 `configure` 则后一支 Never，模式含 `$` 认不出
  时按 Unknown 传播。
- **DI 守卫只由条件位最后一条命令证明**；elif 用自己的条件；条件位多条命令且最后一条
  认不出 → Unknown（含敏感调用即拒），不实现通用 shell。
- **`$1` 只精确匹配 token**（`$1`/`${1}`，去引号后）；`${1:+…}`/`${1#…}`/`"$1x"` 一律
  Unknown，不得推导取值集合。
- **赋值值内替换**：含 `deb-systemd-invoke`/`_dh_action`/`deb-systemd-helper … enable`
  → 违规并拒（不解析替换内部、不产生 enable 记录）；无害替换词法整块通过。
  单引号文字（不执行）不算调用、双引号内 `$( )` 算执行——判定依据是词法 `variable`
  标记（未转义且非单引号内的 `$`/`$(`/反引号）。
- **恒假 `||` 支按真值不参与求并**（`|| false`、`|| [ 1 = 2 ]`）；恒真支/中性支照旧让
  整条条件不约束 `$1`。
- 保留的 Fail-Closed 清单（`for`/`while`/`until`/`select`/函数定义/here-document/
  `[[ ]]`/`test -o`/否定比较/`;&`/`;;&`/未闭合结构）与已知限制不变；新增限制：重定向
  目标引号感知但无 fd 语义建模；`[…]` glob 按字面、变量模式按 Unknown；`if` 条件位
  真值只看最后一条命令（更前命令只审计副作用）；Unknown 前支使 elif/else 进入 Unknown
  而不是默认放行。

### 9.3 本轮实际跑过的验证

- **RED→GREEN**：14 条新测试先跑 RED（11 条审计/合同失败 + 3 条即 RED-30 与两条
  「当前 CI 已正确」的结构合同，后者以测试内 mutation 演示为 RED 证据），修复后
  `node --test scripts/tests/*.test.mjs` **74/74**（audit 51、contract 23）；
  `node --check` 两测试 + lib 全过；Python 验收账本 28/28；`python3 -c yaml.safe_load`
  解析 ci.yml 正常（5 个作业）。旧 60 条（含 shell: bash 防删、逐单元接线、13.24.2
  真实形态 fixture、13.25 synthetic remove 停止片段）全部原样保留并通过。
- **真实维护脚本回归**：`debian/postinst`、`debian/rsetup-controller.postinst`、
  `DEBIAN/postinst`（next/controller 两个）、`rsetup-next.postinst.debhelper`
  （13.6ubuntu1 旧产物）审计全部 `ok`，无新增 Fail-Closed（R3 的裸 `$( )` 词法修复
  反而消除了对含裸命令替换脚本的误报风险）。
- **正常 Trixie fixtures / synthetic 13.25 保留**：13.24.2 三单元 postinst、
  `--no-enable` DI 形态、postrm、13.25 remove|deconfigure+stop 及含续行+upgrade+restart
  的变异 prerm 共 8 个 fixture `sh -n`/`dash -n` 全过；CLI 正反例矩阵：三单元
  `--require-active-enable-unit` 全 exit 0、controller 单元 exit 1、`--enable-guard`
  对 `--no-enable` fixture exit 0 / 对默认-enable fixture exit 1、变异 prerm exit 1、
  `elif DI`/`case` 遮蔽/短路 fixture 的 `--require-active-enable` 均按新口径 exit 1 而
  `--enable-guard` exit 0。
- **安全改名 stub 的真实语义验证**（`env -i PATH=仅含改名桩` 执行 fixture，绝不触碰
  真实 helper/服务）：trixie postinst `configure` → unmask×3 + was-enabled×3 + enable×3；
  `--no-enable` postinst `configure`（DI 桩模拟首装返回 1）→ 无 enable 调用；
  13.25 prerm `remove` → 仅 `--no-reload stop`、`configure` → 无调用；
  `elif DI`/`case` 遮蔽/`true ||` 短路三个 round3 fixture 在真实 shell 下均无 enable
  执行——与审计的守卫/可达判定一致。
- **形态矩阵**（内存内，§8.6 矩阵的回归 + round3 追加）：8 条真放行形状（含新增的
  裸替换赋值、引号重定向、恒假 `||` 支）0 违规；20 条未知/违规形状（§8.6 原 13 条 +
  `${1:+remove}`、赋值值内 `$()`/反引号、Unknown 条件包 enable、假 DI 条件列表——最后
  这条按守卫记录断言 `guard=outside, active=true`）全部命中。
- **git 范围/卫生**：本 writer 只改上述 4 文件；`git diff --check` 干净；验证用的
  scratch fixture/探针目录（内存脚本外的 shell harness 与 stub）已在验证后删除。

### 9.4 合同加强的内容与边界

`controller-ci-contract.test.mjs` 新增两条结构合同（当前 `ci.yml` 文本原样通过）：

1. `--enable-guard-unit` 循环体必须**逐字包含** `unit="${pkg}.service"` 推导行，且
   审计调用必须把 `"$unit"` 与 `"$tmp/$pkg/postinst"` 绑在同一条命令里；测试内现场
   演示 `unit="unrelated.service"` 与 `$tmp/unrelated/postinst` 两个 mutation 被拦截
   （并演示旧 presence 断言对二者放行，即备审复现点）。
2. rsetup-next 侧：`next_postinst="$tmp/rsetup-next/postinst"` 绑定行必须存在，
   逐单元循环体必须同条调用绑定 `"$unit"` 与 `"$next_postinst"`；指向无关文件的
   mutation 被拦截。

边界如实：这两条是**结构/推导链**断言（CI 是文本步骤，无法在本合同里执行解包），
行为层的 pkg→unit 绑定由审计 CLI 自身的逐单元断言承担（RED-22 等已覆盖）。

### 9.5 本轮**没有**跑的（如实）

- **Docker 未跑**：本会话工具审查拒绝 `docker run`，未做最小 harness 构建，也没有
  复核 §6 的 Bookworm/Trixie 容器证据；本轮 fixture + 改名 stub 验证不能替代容器内
  `dh_installsystemd` 的真实生成过程。
- **GitHub Actions 未跑**：未 commit/push，`debian-package`/`debian-12-runtime` 没有
  真实流水线记录；合同测试只保证 ci.yml 文本结构。
- **完整 deb 未产**：未跑 `dpkg-buildpackage`/`lintian`/`cargo`/`npm`；产物侧只做了
  对既有真实产物（§6.1 生成物与 DEBIAN 暂存目录）的只读审计复核。


---

## 10. round4（同日第四轮）：删除自制 shell 解释器，改为严格模板门禁

范围：只改 `scripts/lib/systemd-maintainer-audit.mjs`、`scripts/tests/systemd-maintainer-audit.test.mjs`、
`scripts/tests/controller-ci-contract.test.mjs`、`.github/workflows/ci.yml` 与本文件。未触碰
Rust/Cargo、`debian/rules`、`debian/control`、unit 文件、升级迁移；未 commit/push/clean。
架构变更（改为严格模板门禁）由用户在 `packaging-template-brief.md` 中明确批准，不是本 writer
自行丢弃既有要求；前三轮的结论与证据全部保留在上文，只是**实现与口径已被替换**。

### 10.1 为什么删掉解释器（而不是继续加第四条特例）

§8.3 与 §9 的做法是「用自制 tokenizer + 有限 AST 证明这段 shell 会不会激活服务」。三轮备审
每轮都能构造出新的绕过（续行、`fi` 后命令、一行多调用、`else`/`elif`/`case` 分支先后、
`false &&`/`true ||` 短路、DI 只认条件位末命令、赋值值内 `$( )`、`${1:+remove}` 参数扩展、
`--require-active-enable` 与逐单元判定口径不一致 …），本 writer 逐条 RED→GREEN 修完，
但**每次修复都在扩「什么叫可证明」的面**：解释器的漏报面 = 「所有可能的 shell 写法」减去
「已建模的语法」，这个差集无法穷尽，也无法用测试证伪。

新口径把问题换成一个**有限且可判定**的问题：

> 这份维护脚本，是不是「仓库里那份源维护脚本（去掉 `#DEBHELPER#`）」+
> 「若干**逐字已知**的 debhelper 自动片段」拼出来的？片段来自哪个 helper 的哪个版本、
> 出现在哪个脚本里、参数位上的单元是哪些，是否都在登记表里？

比不上一律失败。于是不再需要「这条命令是否可达」「这个守卫是否为真」这类判断：
多一条命令、少一条命令、移动 guard、换单元、把命令塞进 `$( )`、把 `if` 改成 `case`、
helper 版本没登记过——**全部只是「不等于模板」**。这也让 §9.2 那句「口径有限且如实」
变成一个真正的硬边界：静态门禁**不再声称**理解任何 shell 语义。

### 10.2 模板登记表：唯一放行来源，逐条标注证据级别

`scripts/lib/systemd-maintainer-audit.mjs` 的 `TEMPLATES` 是唯一放行来源。版本串必须与产物
起始行 `# Automatically added by <helper>/<version>` **逐字相等**（不做 `>=` 放宽、不做前缀匹配）。
证据级别用两个独立布尔位表达，不混为一谈：

| 模板 | helper/version | `observed`（本仓实测生成过） | `upstreamVerified`（与上游原文逐字核对） | 允许出现在 | 含 `deb-systemd-invoke` |
| --- | --- | --- | --- | --- | --- |
| `postinst-systemd-enable` | dh_installsystemd/13.24.2 | 是 | 是 | postinst | 否 |
| `postinst-systemd-dont-enable` | dh_installsystemd/13.24.2 | 是 | 是 | postinst | 否 |
| `postrm-systemd` | dh_installsystemd/13.24.2 | 是 | 是 | postrm | 否 |
| `postrm-systemd-reload-only` | dh_installsystemd/13.24.2 | **否** | 是 | postrm | 否 |
| `postinst-init-tmpfiles` | dh_installtmpfiles/13.24.2 | **否** | 是 | postinst | 否 |
| `remove-stop-synthetic` | dh_installsystemd/13.24.2 | **否** | **否** | prerm | 是 |

来源与如实边界：

- 五条 `upstreamVerified` 条目的证据最初取自官方源码浏览页；**复审发现 HTML 渲染
  导致 tmpfiles 的 `||` 被误抄为 `|`，实现与 fixture 共同错误，原 67/67 未能检出**。
  现改用 [官方 raw 模板](https://salsa.debian.org/debian/debhelper/-/raw/debian/13.24.2/autoscripts/postinst-init-tmpfiles)
  与 [Debian API 元数据](https://sources.debian.org/api/src/debhelper/13.24.2/autoscripts/postinst-init-tmpfiles/)
  核对：264 字节，SHA256 `e38a9d798e9fb2f3e8cc19764fbb285f5b195b5d07179f2197d044826f9b4d27`。
  新回归测试用此独立哈希约束 fixture，并断言真模板放行、单管道变异拒绝。
  §6.1 实测过的三条 systemd 片段与该 tmpfiles 的仅上游证据不能混为一谈。
- `postrm-systemd-reload-only` 与 `postinst-init-tmpfiles` 只有上游原文证据，**本仓没有
  在该版本下实测生成过**（本仓能本地复现的构建产物来自 debhelper 13.6ubuntu1，见 §10.5），
  故 `observed: false`。它们被登记是「出现时按逐字模板放行」而不是「断言必然出现」：
  门禁对 postrm 只**要求** purge 记账存在，reload-only 可有可无。
- `remove-stop-synthetic` 不是实测产物，也**未在上游 13.25 核实**：它只是「本仓愿意接受
  的那个形状」——`$1` 恰好等于 `remove` 或 `deconfigure` 的守卫下的
  `deb-systemd-invoke stop`。默认**不参与匹配**，必须显式 `--allow-unobserved-template`
  才放行，且放行结果里带 `observed=false` 标记。生产 CI 不带该选项（合同 §10.3 第 5 条钉住）。
  之所以保留这个槽位：它把「正常 remove 允许 stop」这条策略写成了机器可读的形状，同时
  不承诺「未来任意 helper 版本都能被接受」。13.25 版本和 remove-stop 变更由 §2 的官方
  changelog 证实，但不等于本合成槽位就是该版本真实生成物。未来未登记版本仍红灯重审，
  核实实际 helper 生成逻辑和完整脚本后才决定是否登记，不静默放行。
- 登记表自身有一条一致性断言（测试）：正文含 `deb-systemd-invoke` 的模板**只能**登记在 prerm。
  「安装/升级不激活服务」因此是结构性质：postinst/postrm 只可能由不含 invoke 的模板构成。

**模板参数只有三类**，其余一律逐字比对：

1. 单元/文件列表：`#UNITFILE#`（恰好一个 `'unit'` 单引号词元）、`#UNITFILES#`
   （≥1 个单引号词元、单空格分隔）、`#TMPFILES#`（≥1 个裸文件名）。同一片段内所有
   `#UNITFILE#` 位置**必须指向同一个单元**（否则是「守卫查 a、执行 b」的变异，整段拒）；
2. helper/版本标记：起始行整串命中登记表；
3. 换行规范化：仅 CRLF→LF、丢空行、去**行尾**空白。**行首缩进仍然严格**（制表符改空格判红）。

不存在「包含某字样即成立」的通道：单元必须出现在被匹配模板的参数位上，注释里列三个目标
单元、unmask 行提到单元名都不构成该单元被 enable 的证据（负例测试：把 enable 行的单元
换成别的、只在注释里写目标单元，均失败）。

### 10.3 判定规则与 CI 接线

`gateMaintainerScript()`（及同口径 CLI）规则：

1. **切分**：起止标记之间是自动片段，其余是「模板外残留」；起始标记后找不到结束标记
   → 失败；片段内部又出现起始标记（手工拼接痕迹）→ 失败。
2. **片段匹配**：按 helper/version + 脚本种类取候选，逐行完整比对（§10.2 的三类参数）。
   不匹配时报「走得最远的那一条差异」；候选为空 → 「未登记的自动片段来源 …，先人工核对
   上游原文再登记」，**不做 `>=` 放宽**。
3. **模板外残留**：
   - 有仓库源（`--source-script`）→ 残留必须逐字等于该源去掉 `#DEBHELPER#` 行（忽略空行与
     行尾空白）；源里必须有 `#DEBHELPER#`，否则「源与产物无对应关系」直接失败。
   - 无仓库源（debhelper 纯生成脚本）→ 残留只能是 `#!/bin/sh` + `set -e` 骨架
     （debhelper 13 生成脚本无 `exit 0`：
     `Dh_Lib::debhelper_script_subst` 对无源脚本只 `printf '#!/bin/sh\nset -e\n'` 再拼接生成片段，
     本会话已读该实现）。
   - 两种情况都额外禁止模板外出现 `deb-systemd-invoke` / `deb-systemd-helper` /
     `_dh_action` / `systemctl` 字样（连注释都禁止：不参与模板匹配的文本不能靠「其实不会
     执行」放行）。`systemd-tmpfiles` 不在此名单——本仓已评审的 `debian/postinst` 源片段
     合法调用它，而自动片段版本仍由模板逐字核对。
4. **包型 × 脚本种类**：`--mode default-enable` 的 postinst 必须恰好覆盖 `--require-unit`
   全集（缺/多/重复都算不一致，多重集比较），且不得出现 `debian-installed` 守卫模板；
   `--mode no-enable` 反之（出现不受守卫的 enable 模板 = `--no-enable` 未生效）。postrm
   必须有 purge 记账且集合一致；enable 记账出现在 postrm、或 enable 模板出现在非登记脚本种类，
   都失败。

CI（`.github/workflows/ci.yml` 的 `Assert Debian systemd maintainer scripts` 步骤）改为
**逐包 × 逐脚本**的 6 次显式调用，参数把「审计谁、属于哪个包、什么包型、期望哪些单元、
仓库源在哪」全部写明：

```
node "$gate" --package rsetup-next        --script-kind postinst --mode default-enable \
  --source-script debian/postinst \
  --require-unit rsetup-next-thermal-policy.service \
  --require-unit rsetup-next-fan-curve.service \
  --require-unit rsetup-next-led-state.service  "$tmp/rsetup-next/postinst"
node "$gate" --package rsetup-next        --script-kind postrm   --mode default-enable \
  …三个 --require-unit…                      "$tmp/rsetup-next/postrm"
node "$gate" --package rsetup-controller  --script-kind postinst --mode no-enable \
  --source-script debian/rsetup-controller.postinst \
  --require-unit rsetup-controller.service  "$tmp/rsetup-controller/postinst"
node "$gate" --package rsetup-controller  --script-kind postrm   --mode no-enable \
  --require-unit rsetup-controller.service  "$tmp/rsetup-controller/postrm"
node "$gate" --package rsetup-board-agent --script-kind postinst --mode no-enable \
  --require-unit rsetup-board-agent.service "$tmp/rsetup-board-agent/postinst"
node "$gate" --package rsetup-board-agent --script-kind postrm   --mode no-enable \
  --require-unit rsetup-board-agent.service "$tmp/rsetup-board-agent/postrm"
```

（上面是等价简写；CI 里是 6 条完整命令，逐行反斜杠续行。）另外：

- 三包**必须不存在** `preinst`/`prerm`：`--no-start --no-stop-on-upgrade` 下 debhelper
  不生成这两个脚本，「升级不 stop」就体现在它们不存在上；将来工具链若开始生成，CI 变红重审，
  而不是把断言放宽成「存在也行」。
- 保留 `shell: bash`（container 作业默认 `sh -e {0}`，本步骤仍用 `<<<`）、
  `debian/rules` 的 `--no-start`/`--no-enable`/rsetup-next 禁 `--no-enable` 三条源码检查、
  以及 `StateDirectoryMode=0700` / `UMask=0077` 两条 unit 检查。
- 删除的旧接线：四脚本一把梭的 `audit.mjs $scripts`、`--require-active-enable`、
  `--require-active-enable-unit` 逐单元循环、`--enable-guard` / `--enable-guard-unit` 循环、
  `grep -q 'deb-systemd-helper enable'` 正断言。
- 合同测试（`controller-ci-contract.test.mjs`）把上述接线钉成**结构**断言而非 presence：
  从步骤文本折叠出 6 条调用，逐条核对 `--package` 必须等于审计对象路径 `$tmp/<pkg>/<kind>`
  的目录段、`--script-kind` 必须等于文件名段（`postinst.unrelated` 不认）、`--mode` 必须
  等于该包在 `debian/rules` 里的包型、`--require-unit` 集合必须精确、无源脚本不得凭空指
  `--source-script`、一条调用只能有一个 `$tmp` 路径、CI 里不得出现
  `--allow-unobserved-template` 与任何旧选项/`grep` 字样断言。并现场演示 6 个内存 mutation
  （审计对象换成 `$tmp/unrelated/postinst`、换成 `postinst.unrelated`、单元换成
  `unrelated.service`、包型改成 `no-enable`、删掉 `--source-script`、postrm 换成 postinst）
  全部被同一条结构断言拦下。

### 10.4 删除的旧测试与替换理由（不掩盖任何审查失败）

`systemd-maintainer-audit.test.mjs` 里 **round1～round3 的 AST/语义用例整体作废**，因为它们的
断言对象（「这条 enable 可证明活跃」「DI 位于条件位末命令算守卫」「`${1:+remove}` 与 `$1`
比较可判定」「恒假 `||` 支里的 enable 不算活跃」）**在新架构下不再是概念**——新架构不判断
任何 shell 语义。替换原则：每条旧用例改写成「该形状不是已登记模板 → 必须失败」的同一条
判据下的负例，或改成正向模板 fixture。逐条对应如下（旧编号见 §8.3/§8.4/§9.1）：

| 旧用例（语义证明类） | 处置 | 理由 |
| --- | --- | --- |
| `ACTIVE_ENABLE` / `GUARDED_ENABLE` / `MIXED_ENABLE` / `ENABLE_BOOKKEEPING` fixtures 及其真值断言 | 替换为 §10.2 引用的 13.24.2 上游原文 fixture（正向） | 旧 fixture 是手工简化形状，缺 `# The following line should be removed in trixie or trixie+1` 等注释行，不是真实产物文本；新 fixture 逐字取自上游 |
| `analyzeEnableGuards` 的守卫集合并/交、`$1` token 精确性、`--require-active-enable` 可达性判定 | 删除 | 概念不存在；`--mode` + 模板参数位替代其全部对外语义 |
| RED-19…RED-30（`false &&`、`true ||`、`elif`/`else` 先后、`case` 影子、DI 条件位末命令 `; true`、赋值值内 `$( )`、`${1:+remove}`、引号重定向、Unknown 上下文拒审） | 保留为负例，但判据统一改为「不是已登记模板 / 参数位不匹配 / 模板外残留」 | 行为结论不变（一律失败），不再需要解释为什么失败——这正是删除解释器的意义 |
| 「`if false` 里的 enable 不算活跃」「恒假 `||` 支」等「更严格」用例 | 删除 | 新架构下这些形状本来就匹配不上模板；保留它们会重新引入「我们在判断真值」的口径 |
| 合同里 `--require-active-enable-unit` / `--enable-guard-unit` 的绑定与 mutation 用例（§9.4 两条） | 替换为 §10.3 的 6 条调用结构合同 + 6 个 mutation | 绑定对象从「CLI 选项与 `$unit` 变量」变成「显式参数与审计文件路径」，presence 断言一律不再接受 |
| `ci.yml` 里 `deb-systemd-helper enable` 字样正断言的合同 | 删除，改为「维护脚本的 systemd 断言不得再用 grep 字样」 | 真实产物里 `--no-enable` 包也含该字样，presence 既非充分也非必要 |

新增的、旧架构**结构上无法表达**的检查（这才是换架构换来的东西）：未登记 helper/版本必须
review-fail；模板外残留必须逐字等于仓库源；`#UNITFILE#` 各位置指向不一致必须拒；
含 `deb-systemd-invoke` 的模板只能出现在 prerm；合成槽位默认拒绝且放行时带 `observed=false`；
14.5 标记的无守卫 stop 片段必须因未登记版本失败（不据此推断真实 helper 的外层守卫）。

备审阻断项在新架构下的落点（逐条，全部实测失败，见 §10.5 的 CLI 记录与测试文件）：正常模板
（正向）、模板外注入敏感调用、改 guard、`case` 形态、同 line 尾追加命令、命令替换、
换 unit、unit 只在注释里、`#UNITFILE#` 位置不一致、缺/错结束标记、嵌套起始标记、
helper 换名/换版本、14.5 无守卫 stop、合成槽位未授权、包型互换、postrm 缺 purge、
残留注入无害命令、纯生成脚本多 `exit 0`。

### 10.5 本轮实际跑过的验证（静态与本地）

- `node --test scripts/tests/*.test.mjs`：**67/67 通过**（模板门禁行为合同 44 条 +
  CI/打包合同 23 条，其中含本轮新增的 `bash -n` 语法合同）。此前 round3 的 74/74 是旧架构
  的用例数，两者不可直接比较。
- `.github/workflows/ci.yml` 用 `yaml.safe_load` 解析通过；断言该步骤 `shell: bash`、
  含 6 次 `node "$gate"` 调用、仍含 `<<<`（所以 `shell: bash` 不是摆设）。
- 新增一条**语法**合同：把该步骤 `run: |` 块按原缩进抽出、`${{ … }}` 换成常量后跑
  `bash -n`（只解析不执行）。这条合同不是摆设——本 writer 在改这个步骤时真的写丢过一个
  收尾单引号，`bash -n` 立刻报 `unexpected EOF while looking for matching '``，修好后转绿；
  此前的合同只查文本 presence，抓不到这类会让 CI 直接语法失败的错误。
- CLI 正负例（全部在临时目录的**安全 stub** 文本上做，不触碰任何真实 helper/服务/网络）：
  - 正向 6 条（与 CI 同参数，产物按 13.24.2 模板 + 仓库真实源脚本拼装）→ 全部 `exit 0`，
    输出逐段列出命中的模板与参数位词元；
  - 未登记版本 `dh_installsystemd/13.99.0` → `exit 1`，报「未登记的自动片段来源」；
  - 模板外尾部注入 `deb-systemd-invoke restart …` → `exit 1`，同时报「与仓库源不一致
    （行数 38 vs 39）」与「模板外内容出现敏感调用字样」；
  - 单元整体换成 `rsetup-evil.service` → `exit 1`（集合不一致）；
  - 结束标记改成拼错的 `# End automaticaly added section` → `exit 1`（找不到结束标记）；
  - 缺 `--mode` → `exit 2`；旧选项 `--enable-guard` → `exit 2`「未知选项」；
  - 合成 remove-stop 的 prerm：默认 `exit 1`（报「命中了非实测策略槽位模板」），加
    `--allow-unobserved-template` → `exit 0` 且输出带 `[含非实测槽位模板]`；
  - `--list-templates` → `exit 0`，逐行打印 `observed=` / `upstream=` 两个证据位。
- 审计脚本不执行外部程序：模块内无 `child_process`/`spawn`/`execFile`，只做字符串比对，
  因此「跑门禁」本身不可能激活服务或改动系统状态（这也是本轮可以用 stub 文本做全量验证
  而无需容器的原因）。
- 关于「本仓能否本地看到 13.24.2 的真实生成物」：**不能**，如实记录——本地
  `debian/*/DEBIAN/` 里的产物来自 debhelper **13.6ubuntu1**（本机 `dh --version` 实测），
  而 13.6 早于 13.11.6 的 `/usr` 单元发现支持，本地产物里 `deb-systemd-*` 片段数为 0
  （`grep -c "Automatically added" debian/rsetup-controller/DEBIAN/postinst` = 0，
  rsetup-next 只有 1 段 dh_installtmpfiles 片段）。这正是 §2 版本下限存在的原因，
  也意味着 **13.24.2 的完整产物形状只能靠上游原文 + §6.1 的容器实测证据**，本会话无法在
  本机重现。

### 10.6 本轮**没有**跑的（如实）

- **Docker 未跑**：本会话对 Docker 工具调用的审查被拒绝，未做 Trixie 最小 harness 构建，
  也未复核 §6.1/§6.3 的容器证据。按既有约定不绕路（不寻找替代容器驱动、不在宿主机装
  debhelper 跑真实构建）。
- **GitHub Actions 未跑**：未 commit/push，`debian-package` 与 `debian-12-runtime` 没有
  真实流水线记录；本轮合同只保证 ci.yml 的文本结构与调用形状。
- **完整 deb 未产**：未跑 `dpkg-buildpackage`/`dpkg-deb`/`lintian`/`cargo`/`npm`。CI 里
  那 6 次门禁调用面对的是真实 `dpkg-deb --control` 解出的脚本，本会话未在真实产物上执行过；
  若真实产物里出现本登记表之外的 helper/版本或额外片段，CI 会判红——这是设计意图
  （review-fail），但意味着**本轮不能宣称「真实产物一定通过」**。
- **`git diff --check`**：本轮改动文件干净（仅上述 5 个文件；其他 writer 的文件未触碰）。

### 10.7 静态与动态证据的边界（勿夸大）

严格模板门禁只证明**文本构成**：产物维护脚本 = 已评审的仓库源片段 + 逐字已登记的
debhelper 自动片段；已登记片段在结构上不含安装/升级期的服务激活调用（含 invoke 的形状
只登记在 prerm）；`--no-enable` 包的 enable 记账整块在 `debian-installed` 模板内。

它**不**证明：dpkg 真的按这段文本执行；`deb-systemd-helper` 状态文件语义；enable 符号链接
真的建立或不建立。后半属 `debian-12-runtime` 作业的动态职责：Bookworm `apt-get install`
安装三包，随后 `test -L` 断言三个 next 链接存在，`test ! -e` 断言两个 `--no-enable` 包无链接。

复审前 CI **只有镜像自带的 policy-rc.d=101 抑制器，没有动作记录**，旧文所称“保留的动作记录”
不成立。现补安装前部署的记录型 policy-rc.d：UTC 时间、安装 phase 和全部调用参数逐项记录，
始终返回 101；安装开始/结束另写边界，日志步骤 `if: always()` 在安装失败时也输出已有记录。
本地测试实际执行从 CI 提取的 recorder（只用临时日志，不接触真实 policy/service），验证两种
参数形状均记录完整且 exit 101。**未运行新的 Actions/容器安装**，所以现在只有配置与 recorder
行为证据，不能冒称已经取得新的包安装记录；也未增加 upgrade/remove 动态覆盖。
没有调用记录只能说明该 policy 接口未收到请求，不能证明没有绕过它的直接服务调用。
两个通道不互相冒充：静态门禁的红不代表运行时会激活服务，动态绿也不能替代模板来源审查。

### 10.8 round4 主审失败与定点修复（当前验收口径）

主审 96999c05 的结论为 Spec FAIL / Quality FAIL，不按初版 67/67 放行。主代理处理如下：

1. **F1 CRITICAL**：raw/API 独立核实 tmpfiles 原文与哈希后，先修正测试 fixture，新增
   真模板通过/错管道拒绝两用例。旧实现实跑两例均 RED（真拒假收）。修模板后又揭示
   `tokensFrom` 内独立硬编码的单管道提取正则，同步改为精确 `|| true`，未放宽模板。
   既有重定向变异测试的 replace 也须同步更正，并增加 `assert.notEqual` 防止无效变异。
2. **F2 MAJOR**：撤回“13.25 不存在”和“裸模板就证明 helper 在升级 stop”的越界断言。
   版本及 remove-stop 变更由官方 changelog 证实；合成槽位标为“未在上游 13.25 核实”，
   不断言任何上游版本都不存在此正文。未登记 helper/版本和默认禁用合成槽位的策略未变。
3. **F3 MINOR**：补记录型 policy-rc.d 和安装 phase/UTC 边界，始终 exit 101，不执行服务。
   CI 合同实际提取并用 sh 执行 recorder，两次调用的全部参数/phase/时间字段均检查。
   新测试先因 CI 缺记录器 RED，补实现后 GREEN。保留三正两反 enable 链接断言。

实际验证（2026-10-23）：

- `node --test --test-reporter=spec scripts/tests/systemd-maintainer-audit.test.mjs scripts/tests/controller-ci-contract.test.mjs`：
  **70/70**（46 + 24），0 fail；包含 CLI 正负例与 `bash -n` 合同。
- `yaml.safe_load` 解析 CI，并将 Bookworm 各 `run` block 替换 architecture 表达式后逐块
  `sh -n`：全部通过。仅语法验证，未执行 apt 或修改宿主 policy。
- **未运行新 Docker/Actions/完整 dpkg-buildpackage，也没有 upgrade/remove 新动态证据**。
  新增 recorder 的本地执行不等于实际包安装验证，保留 §10.6 的未验证边界。
- 修复待新独立 reviewer 复审；三个生产 session/controller/board writer 仍在途，
  本报告不是整个加密隧道生产交付结论。未 commit/push。

### 10.9 独立复审结论（168810ff，2026-10-23）

**Spec PASS / Quality PASS；F1/F2/F3 全部 ADDRESS，无新阻断。** 复审只读，未暂改或
恢复任何交付源/测试，未派发子代理。复审者独立执行两文件 Node 测试 **70/70（46+24）**、
PyYAML 解析及 shell/stdin 探针；独立 fetch 官方 raw/API 重算 264B/SHA256，核实真实
`dh_installtmpfiles` 的参数替换为 basename，单管道变异拒绝且 basename 提取正确。
官方 changelog 再次证实 13.25 版本与 #1079181。CI recorder 安装先后、环境变量、
101 返回及 always 日志步骤均通过复核。

该 PASS 仅验收严格模板门禁修复与 CI 记录器接线。仍未运行新的 Docker/Actions/完整 deb，
六次门禁尚未在新的真实 13.24.2 产物上执行，upgrade/remove 无新动态证据；未核实的
synthetic 槽位仍默认拒绝。组件审查通过不构成整个生产隧道或真实安装验收通过。

## 11. 本轮（2026-10-24，包装文档纠偏 + 宿主机原生构建核验）

唯一 run 目录：`target/tunnel-deb-verification/20261024T035504Z-task7-debdrybuild/`
（下列日志均在该目录的 `logs/` 下，编号即执行顺序；`work/` 放复制清单与两个执行脚本，
`src/` 是隔离 source 副本。全部产物都在该目录内，宿主未安装任何系统包，未 commit/push。）

### 11.1 交付的文案修正（source contract 层，纯文案，零行为改动）

1. `debian/control` `rsetup-board-agent` 段：删除「The shipped binary is a read-only
   sampling agent … the production encrypted tunnel and the h2c transport are not wired
   up yet … no controller session is possible … runs it as root in the read-only watch
   loop」整段过期描述，改为与 `bin/rsetup-board-agent.rs --connect` 和
   `data/rsetup-board-agent.service` 一致的事实：root 只读采样 + 生产模式主动出站 TCP →
   v3 客户端握手（显式中控公钥 pin，无 TOFU）→ 加密记录层 → HTTP/2 双流会话、不监听任何
   端口、连接参数来自本包**不携带**的必需 EnvironmentFile、状态目录 0700；单元装机即
   disabled，由运维显式 enable，安装/升级不 enable/start/restart/stop；真实重启边界仍未
   接线（两个 reboot 动作回 capability unavailable、能力清单不申报、二进制与单元都不重启主机）。
2. `debian/control` `rsetup-controller` 段：把笼统的「migrate the target database」补成
   v4-only 口径（空库建满 v4 基线 / 已是 v4 只做只读校验 / 其它版本拒绝、无升级路径），
   依据 `crates/rsetup-controller/src/bin/migrate-controller.rs` 与
   `db.rs::production_migration_plan` 的实测语义。
3. `debian/rules` 三处纯注释：包头包清单（第 20 行附近）、operator 决策段（第 33-38 行）、
   以及 `override_dh_installsystemd` 注释里「an always-on root-owned sampler that cannot
   yet be reached over the production tunnel」。**三条 dh_installsystemd 命令行、
   override_dh_installinit、build/test/install 配方一字未改。**
4. `debian/rsetup-controller.README.Debian` 第 106-113 段（主代理另行批准的扩 scope 项）：
   旧文称「an empty database or a v1/v2/v3 database is brought to v3 and then to v4」是
   旧升级描述，已改为 v4-only 三分支（无版本行且零表 → 建满 v4；版本 4 → 只读校验、零 DDL、
   可重复；其它版本一律拒绝，无升级/无 fixture/无就地修复/绝不 DropDB）。**DDL 非事务风险保留
   并写准**：fresh create 是逐条 DDL，中途失败即停并留下已建对象，既不自动回滚也不能在下一次
   `--apply` 续建（会被「零已存在表」入口门拒绝），恢复须由操作员凭第 2 步备份另行授权清表；
   同时保留 `--check` 只是版本探测、不等于完整 shape 校验的限定，五个必需环境变量与四个迁移
   门禁原文未动，并补记「只有该命令跑 DDL，启动服务不做任何 schema 工作」。
5. `debian/README.source` 与 `debian/rsetup-board-agent.README.Debian` 通读后**无**隧道相关
   过期语句（`README.Debian` 已完整描述 `--connect` 生产模式、显式 pin、root、0700、
   退避与停止语义），故未改。`README.source` 第 23-27 行的构建指引
   `dpkg-buildpackage --build=binary --no-sign` 本身不算过期，但它会先跑
   `override_dh_auto_clean: cargo clean`（即清空 `CARGO_TARGET_DIR`，本工作树约 135G），
   本轮不改文档、只在此登记：**在共享工作树做验证性构建必须用 `-nc` 或干脆不进 dpkg-buildpackage。**

### 11.2 宿主机工具链与构建依赖门禁的真实失败（deb 不可交付的确切类别）

`04-env-diagnose.log`：Ubuntu 22.04.5 LTS，dpkg/dpkg-buildpackage/dpkg-deb 1.21.1，
**debhelper 13.6ubuntu1**（`Build-Depends` 要求 `>= 13.11.6~`），node v22.21.1 / npm 11.21.1
（`/usr/local/bin`，非 dpkg 包），cargo/rustc 1.99.1（rustup，非 dpkg 包），
已安装 target 含 `x86_64-unknown-linux-gnu` 与 `aarch64-unknown-linux-gnu`，32 核 / 643G 空闲。

- `05-dpkg-checkbuilddeps.log`：exit=1 ——
  `Unmet build dependencies: cargo debhelper (>= 13.11.6~) nodejs npm rustc`（五项全未满足）。
- `06-dpkg-buildpackage-attempt.log`：`dpkg-buildpackage -nc -b -us -uc` **exit=3**，
  在 `dpkg-checkbuilddeps` 处 abort，未执行任何 `debian/rules` 目标（副本内当时未出现
  `target/`，即无 clean、无 `cargo clean`）。**未使用 `-d`、未装包、未改 Build-Depends。**
- 因此以下三层在本机**不可达**，本轮不存在它们的证据：`.deb` 装配（需要
  `dh_installsystemd` 生成维护脚本片段）、`dpkg-deb -c/--control` 解包审计、
  严格模板门禁在真实产物脚本上的执行。附加理由：debhelper 13.6 的 `dh_installsystemd`
  不解析 `/usr/lib/systemd/system`，即使 `-d` 绕过下限，产物也只会是「脚本里没有激活片段」
  的假阳性，且 13.6 生成的片段必然不匹配登记表里的 `dh_installsystemd/13.24.2` 模板 ——
  手工 `dpkg-deb --build` 出一个无激活脚本的 .deb 同样是被明令禁止的伪 PASS，本轮没有做。

### 11.3 隔离副本的构造与缓存策略（可复现口径）

`01/02/03/10-*.log`：复制清单 = `git ls-files` ∪ `git ls-files --others --exclude-standard`，
剪掉路径段为 `target|node_modules|.git|dist|cargo-home|.debhelper` 的条目与 debian 构建产物，
再只保留工作树**实际存在**的文件（`migrations/0001..0004.sql`、`migrate-identity-test.rs`、
`migration_cli_args.rs`、`migration_v4.rs` 这 7 个已删 tracked 文件因此正确缺席），共 595 个
主工作树文件；submodule `externals/devicesinfo`（HEAD `716ad13`）按自身 `git ls-files` 另复制
574 个文件，`crates/deviceinfo/Cargo.toml` 存在（`check-submodule` 门可满足）。
`debian/cargo-home`（541M / 16752 个 registry 文件）以 `cp -al` **硬链接**进副本，副本内三个
cargo 锁文件（`.package-cache`、`.package-cache-mutate`、`.global-cache`）的硬链接被删除，
主工作区原文件 link count 与内容未受影响。`secret/` 由 gitignore 天然排除，复制清单里
secret/pem/key/credential 关键词命中数为 0；构建全程 `env -i` 白名单环境，
`npm_config_cache`、`TMPDIR`/`TMP`/`TEMP`、`RUSTC_TMPDIR` 全部指向 run 目录内私有路径，
不继承任何数据库/凭据类变量。冷缓存首次失败后（`08-rules-build-install.log`，exit=4：
`error: no matching package named h2 found`，offline 解析）比对 `Cargo.lock` 与两处 registry，
确认只缺 `h2-0.4.20` 与 `tokio-util-0.7.20`，把这两个 crate 及其 sparse-index 缓存条目
从 `~/.cargo` **硬链接**补进副本 CARGO_HOME（`10-registry-supplement.log`）；未改
`debian/cargo-home` 本体、未运行任何 clean、未联网解析依赖。

### 11.4 实际原生构建（`debian/rules` 真实配方，非 debug 复制）

`11-rules-build-install-attempt2.log`（exit=0）在副本内以 `make -f debian/rules` 执行 rules
自身的两个 override 目标，变量口径由 rules 自己 export（日志开头逐项打印）：
`CARGO_HOME=<副本>/debian/cargo-home`、`CARGO_TARGET_DIR=<副本>/target`、
`CARGO_NET_OFFLINE=true`。

- `npm --prefix apps/controller-web ci`（231 包）+ `npm run build`（vite 8.3.2，
  `dist/assets/index-BJ2-TtT0.js` 237.07 kB / `index-zxB4oxKI.css` 22.57 kB）先于首条 cargo，
  满足 `build.rs` 的编译期资产依赖。
- 6 条 `cargo rustc -p <pkg> --release --bin <bin> --locked --offline -- -C target-feature=+crt-static`
  全部成功，**289 行 `Compiling`**（真实冷编译，非复用主工作树 target）。
- `override_dh_auto_install`（含 rules 注明的「装包前重新构建 6 个静态二进制」）与
  文档/单元 install 全部成功；文案最终定稿后又同步重跑一次（`17-rules-reinstall-final.log`，
  exit=0），使副本 install 树里的 `README.Debian` 与仓库最终内容逐字节相同（`19`：SHA256 一致）。
- `12-dpkg-gencontrol-render.log`：`dpkg-gencontrol -p<pkg> -O` 三包均 exit=0，
  `Architecture: amd64`，Depends 渲染正确 —— 证明本轮 control 文案改动不破坏 control 解析。

### 11.5 产物只读断言（无宿主 install/systemctl）

`13-artifact-assert.log`（assert_exit=0）、`14-unit-source-identity.log`、`19-content-boundaries.log`：

- 六个二进制均为 `ELF 64-bit LSB pie executable, x86-64 … static-pie linked … with debug_info`，
  `readelf -lW` 无 `Requesting program interpreter`（`+crt-static` 生效），
  `readelf -hW` Machine = `Advanced Micro Devices X86-64`。SHA256（截断至 16 位以外全值见日志）：
  `rsetup-next` 71388240B `6845b1e9…`、`rsetup-next-helper` 26519696B `d6a4bf4a…`、
  `rsetup-controller` 85792968B `9f274ebc…`、`rsetup-controller-migrate` 70664560B `27f7cc1e…`、
  `rsetup-admin` 68599040B `f8dae35b…`、`rsetup-board-agent` 26927376B `bbbeb8d5…`。
- install 树边界：三包树内**均无 `etc/`**，controller 配置模板只落
  `/usr/share/doc/rsetup-controller/rsetup-controller.default`；5 个 unit 全部装到
  `/usr/lib/systemd/system` 且与 `data/` 源文件 SHA256 逐一相同。
- 配置边界：board-agent 单元 `User=root`/`Group=root`、`StateDirectoryMode=0700`、必需
  `EnvironmentFile=/etc/default/rsetup-board-agent`、`ExecStart=… --connect`、
  `ProtectSystem=strict`、`RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`（不监听）；
  controller 单元 `User=rsetup-controller` 静态账户 + `StateDirectoryMode=0700` +
  `ConditionPathExists=/etc/default/rsetup-controller`；rsetup-next 三个硬件策略单元保留
  `[Install] WantedBy=multi-user.target`（default-enable 语义未被削弱）。源维护脚本内
  无 `deb-systemd-invoke`/`systemctl start|stop|restart|enable` 调用。

### 11.6 默认测试

`09-node-tests.log` 与 `18-node-tests-final.log`：
`node --test scripts/tests/systemd-maintainer-audit.test.mjs scripts/tests/controller-ci-contract.test.mjs`
在文案修正前后各跑一次，均 **tests 70 / pass 70 / fail 0**；测试文件一字未改。
`15-audit-list-templates.log`：`node scripts/lib/systemd-maintainer-audit.mjs --list-templates`
exit=0、7 条登记模板 —— 这是该脚本现成 CLI API 在本轮能真实执行的部分；
`--package/--script-kind/--mode/--source-script` 形态的门禁调用**必须**面对
`dpkg-deb --control` 解出的真实脚本，本机无产物故未执行。

### 11.7 本轮明确没有做（勿与上面各层混读）

- **无 .deb**：无 `dpkg-deb -c/--control/-e` 解包检查、无三包 control 字段/脚本的最终产物级
  校验、无严格模板门禁对真实产物的 6 次调用（原因见 §11.2，属构建依赖门禁不可达，非代码失败）。
- **无宿主 dpkg/dpkg-deb install、无 systemctl 操作**（未 enable/start/restart/stop 任何服务，
  未 reboot，未触碰真实主机状态）；Docker/Debian 13 环境由主代理另做，本轮未安装运行容器。
- **无 arm64 产物**：`rustup target list --installed` 显示 aarch64 target 已装，但本轮只按
  rules 的真实配方做 native amd64，未交叉编译，不宣称 arm64 包通过。
- **无 cargo test/npm 测试**：`override_dh_auto_test` 未执行（它跑 `cargo test --workspace
  --release`，是另一条长耗时路径）；本轮的 Node 侧 70 项合同测试与 Rust 侧 release 编译
  不能互相替代。
- **无双进程端到端**，无真实 DB 连接、无迁移执行，未 commit/push/clean。
- 越界发现（不改，移交主代理）：`crates/rsetup-board-agent/src/tunnel_binding.rs` 的模块注释
  仍写「`network_approved` 默认 false：真实 socket、加密与握手仍未获授权」，而同 crate 的
  `connect.rs`/`bin/…rs --connect` 已是真实 TCP + v3 握手 + h2 双流生产路径（该内存内
  `ApprovalGate` 切片与生产连接路径并存）。Rust 源不在本轮可改范围，但下一个 reviewer 只读
  `tunnel_binding.rs` 会误判隧道未接线，建议按纯注释单独更正。

结论：本轮交付的是**包装文档与真实行为的一致性修正 + 宿主机原生构建与产物只读断言**，
层级为 source contract（已改）/ default tests（70/70）/ actual native build（rules 真实配方
成功，6 个 static amd64 产物）/ artifact inspect（file+readelf+install 树边界）四层为真；
`.deb` 装配与其上的严格模板门禁、真实安装生命周期、arm64、双进程均无证据，不能由以上任何
一层推出。

## 12. 包装文案的独立复核（7c7631b3，2026-10-24）

**无阻断 finding**。reviewer 只读核包描述、当前 dh 行为、v4-only 操作说明与板端内存
夹具注释，独立复跑 **70 项合同测试全部通过**。原生六静态产物构建结果来源于 §11 的
隔离 run 日志，reviewer 核查而非自行再次构建；没有因此宣称已装配 `.deb` 或实际安装。

独立复核无法从老 HEAD 精确分离 writer 本轮纯文案 delta；它确认构建 run 的 rules 与
当前 rules 逐字节一致，当前服务生命周期参数符合合同，不把累计 diff 冒称本轮纯注释。
main 在 writer 交付之后只更正了内存 fixture 的历史授权注释，`ApprovalGate::Default`
及生产逻辑不变。README 的轻微歧义现已收口为服务启动不执行 schema **DDL**，
同时明确首次 bootstrap 会写管理员等应用数据，不能理解成全部零写。

Debian 13 容器构建环境由主代理另行准备，完整包／生命周期结果待后续实际证据；
真实两进程首次运行已经得到失败回执并另行诊断，不归并为本包装任务通过。
