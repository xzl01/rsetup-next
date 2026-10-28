# 子规格 03：`crates/rsetup-core/src/system_install.rs` 拆分设计

> **状态：待用户审阅，尚未实施。** 本文件是总览规范 [2026-10-26-large-file-split-design.md](../2026-10-26-large-file-split-design.md) §4 登记的 03 号子规格，遵守其 C1–C9 契约与 F1–F8 验证前提（下称「总览」）。本任务只写文档：未改任何源码、未编译、未运行测试、未连接数据库、未提交。

撰写基线：分支 `dev-aghost`，工作树头提交 `c7ff640`；实测 `git status` 当时 [crates/rsetup-core/src/system_install.rs](../../../../crates/rsetup-core/src/system_install.rs) 与 [crates/rsetup-core/src/lib.rs](../../../../crates/rsetup-core/src/lib.rs) 均无未提交改动。行号为撰写会话对 4,720 行文件逐段实测，实施时须按当时工作树重新核对（总览 §3 并行改动注记）。

## 1. 实测现状与锚点

### 1.1 体量、公开面与消费方

- 全文件 4,720 行：实现区 1–2,340，测试区 2,341–4,720（单一 `#[cfg(test)] mod tests`，2,342 行起，无嵌套测试子模块、无 `tests/` 集成测试目录，实测 `crates/rsetup-core/tests/` 不存在）。与总览 §3 快照「2,340 / 2,380」一致。
- 测试函数 **48 个**（对 `#[test]` 属性计数实测；任务初盘所称 48 恰与本次实测一致，本文以逐条自数为准，§6 按目标模块逐一映射后合计仍为 48）。无 `#[ignore]`、无 `#[tokio::test]`、无任何数据库接触。
- 模块声明：lib.rs 第 15 行 `mod system_install;`（crate 私有）；lib.rs 第 56–58 行选择性再导出恰 9 个名字：`BlockDeviceBackend`、`ImageOpener`、`SyntheticBackend`、`SystemBlockBackend`、`SystemImageOpener`、`SystemInstallError`、`SystemInstallManager`、`SystemMount`、`install_needs_privilege`。拆分前后此清单逐字不变（C1）。
- 消费方实测：`rsetup-app` 四个面——CLI [main.rs](../../../../crates/rsetup-app/src/main.rs)（`system_install_backend`/`system_install_manager` 1,525–1,543、`install_needs_privilege` 包装 1,620–1,626）、[tui.rs](../../../../crates/rsetup-app/src/tui.rs)（`system_install_manager` 677–688）、HTTP [server.rs](../../../../crates/rsetup-app/src/server.rs)（`install_manager` 852–863、特权路由判定 952）、特权 helper [helper.rs](../../../../crates/rsetup-app/src/helper.rs)（`SystemInstallApply` 分支自建 manager 181 行、`verify_preview_token` 332 行）；以及 workspace 外独立构建的 [apps/desktop/src-tauri/src/main.rs](../../../../apps/desktop/src-tauri/src/main.rs)（Tauri shell，7–9 行 import、自建 `BlockDeviceBackend` 实现）。全部经 `rsetup_core::` 顶层路径，不触子模块路径。
- **`SyntheticBackend` 是生产可用件，非 test-only**（实测）：CLI `--demo` 或非 Linux 默认合成模式（main.rs 442 行 `synthetic: cli.demo || !cfg!(target_os = "linux")`）、TUI 与 HTTP 的 synthetic 分支均以 `SyntheticBackend::default()` 作生产后端；本模块 `new(None, true)` 的 synthetic 路径（324–337 行）同样服务生产。拆分不得将其降格为 `#[cfg(test)]` 或移入测试树。
- 依赖方向输入：仅 `crate::model` 的 12 个类型（124–128 行 import）与 `crate::transaction::ProcessLock`（517–543 行，`pub(crate)`，[transaction.rs](../../../../crates/rsetup-core/src/transaction.rs) 12 行）。不引用 core 其他私有模块；硬件探测不委托 `hardware`/`mmc`/`nvme` 模块，自行读 sysfs/procfs。

### 1.2 实现区职责锚点（实测行号区间）

- **头部模块文档** 1–108：4 条门禁不变量（§2）、token 语义（handle 非 credential、进程内一次使用/TTL、`verify_preview_token` 为 advisory）、`.xz` 检查边界、无回滚、可测性与注入 root。
- **共享常量** 131–180：`CHUNK_BYTES`/`IMAGE_PREFIX_BYTES`/`PLAN_TTL`/`TOKEN_CLOCK_SKEW`/`INSTALL_ACTION_ID`/`INSTALL_ACTION_TITLE`/`SECTOR_BYTES`/`MIB`/`GIB`/`XZ_MAGIC`/`XZ_FOOTER_MAGIC`/`BLKRRPART`（linux cfg）/`VIRTUAL_DEVICE_PREFIXES`。
- **错误与授权** 182–239：`impl SystemInstallError::code`（187–200，全表面统一错误码词表）、`install_needs_privilege`（209–211）、`enum SystemInstallError`（213–239）。
- **抽象** 241–301：`SystemMount`（243–247）、`trait BlockDeviceBackend`（252–294，7 方法：`targets`/`mounts`/`write_chunk`/`sync_device`/`reread_partitions`/`lock_root`/`is_dry_run`）、`trait ImageOpener`（297–299）、`type Clock`（301）。
- **状态机** 306–916：`SystemInstallManager`（306–320，字段 `backend`/`opener`/`synthetic`/`now`/`issued`/`token_key` 全私有）与 `impl`（322–916）：`new` 324、`with_backend` 339、`with_clock` 354（`#[cfg(test)]`）、`targets` 361、`plan` 373、`apply_live` 386–510、`acquire_target_lock` 517、`build_plan` 547–623、`resolve_image` 626–719、`decompressed_partition_table` 729、`write_image` 754–790、`partial_failure` 794、`verify` 800–841、`consume` 852、`issued_at` 867、`parse_token` 877、`verify_preview_token` 895–915。
- **挂载/设备注解与 sysfs/procfs 探测** 919–1113：`annotate_mounts` 919、`is_running_system_mount` 945、`expand_mounts` 965（运行系统挂载解析失败即 fail-closed 报 `Unsupported`，965–991）、`block_names` 994、`block_index` 1005、`read_mountinfo` 1019、`resolve_mount_names` 1038、`collect_slaves` 1063（LVM/md slave 链归盘）、`resolves_to_target` 1083、`partition_number` 1089/`partition_number_of` 1093（`pN` 与 `N` 命名规则）、`longest_mount_prefix` 1108。
- **token 铸造与校验** 1117–1305：`revision` 1117、`ParsedToken` 1141（`nonce` 字段 `#[allow(dead_code)]`）、`split_plan_token` 1153–1180（固定形状 `install-plan-<binding16>-<issued10>-<nonce16>-<mac64>`）、`mint_nonce` 1184（urandom+原子计数）、`mint_token_key` 1206（进程级 32 字节密钥）、`hmac_sha256` 1229（RFC 2104）、`constant_time_eq` 1248、`token_binding` 1258–1274（绑定 revision+target+image+确认串）、`plan_token` 1280、`stable_hash` 1299（FNV-1a，非安全原语）。
- **写入辅助** 1308–1349：`file_identity` 1314（open handle 的 dev/ino/mtime/ctime）、`chunk_len` 1333（先比后窄，32 位回归）、`same_partition_layout` 1337（按几何比较、忽略内核命名）。
- **live 后端** 1351–1650：`SystemBlockBackend` 1353–1375（`root` 注入 + `handles` 缓存）；`impl BlockDeviceBackend` 1377–1533（`targets` 扫 `sys/class/block` 并按 `VIRTUAL_DEVICE_PREFIXES` 滤虚拟设备、`write_chunk` 以 `O_EXCL` 独占打开 1481–1494、`sync_device` flush 后释放句柄、`reread_partitions` = ioctl 后重扫）；`RawDisk` 1535/`RawPartition` 1543；`classify` 1554–1573（nvme/mmcblk SD-eMMC 判别、ufs 主机路径启发式）；`read_attr` 1575；`parse_mounts` 1582 + `unescape_mount` 1599（八进制转义）；`reread_partition_table` linux 版 1626–1642（`BLKRRPART` ioctl）与非 linux 版 1645–1650（`Unsupported`）。
- **镜像打开与解压** 1652–1801：`SystemImageOpener` 1654–1699（按 open handle 复核 `file_identity`，不符 `StalePlan`；xz 经 `xz -dc` 子进程、stdin 用已验证 handle）；`spawn_decompressor` 1704–1733（stderr 独立线程限 64 KiB）；`Decompressor` 1738–1801（`Read` 见 EOF 才 `finish` 等待子进程、非零退出硬失败、`Drop` 只终止未完成流）。
- **合成后端** 1803–1935：`SyntheticBackend` 1806（计数写入、`lock_root` 取默认 `None`）、`synthetic_target` 1810–1843、`impl BlockDeviceBackend` 1845–1923（4 块标注演示盘、`/`+`/boot` 挂在 mmcblk1）、`BlockDeviceKind::marker` 1925–1935。
- **分区表与 xz 容器解析** 1937–2339：`read_partition_table` 1939–1990（MBR/protective MBR 判别）、`read_gpt` 1992–2054（entry 尺寸 128–4096、最多读 128 项）；`read_xz_size` 2070–2194（自文件尾反向走流、三帧 CRC32 校验、VLI 解码、溢出防护）、`trailing_null_bytes` 2199–2225（padding 上限 4 MiB）、`read_xz_vli` 2228–2246；`hex` 2249、`sha256` 2259–2339（工作区无 hash 依赖、本模块 MUST NOT 新增依赖）。

### 1.3 测试区结构（实测）

fixture 与工具（迁移映射见 §6）：`scratch`/`Scratch` 2,347–2,370（临时目录固定 `target/system-install-tests/`，Drop 清理，只落普通文件、永不触真实块设备）、`write_file` 2,372/`write_attr` 2,379、`mbr_image` 2,398、`mbr_bytes` 2,453、`mbr_partitions` 2,465、`gpt_image` 2,478、`XZ_FIXTURE`+`XZ_FIXTURE_BYTES` 2,508–2,519、`RereadBehavior` 2,521、`FixtureBackend` 2,532–2,607、`Fixture`+`fixture` 2,609–2,667、`install_request` 2,669、`boot_mounts` 2,677、`xz_available` 2,967（软跳过守卫）、`flip` 2,976、`XZ_MULTI_BLOCK_FIXTURE` 3,947、`DryRunFixture` 4,424。测试体内嵌的一次性 opener 桩随测试走：`FailingOpener` 3,156、`SwappingOpener` 3,458、`TotalOpener` 4,043、`LongOpener` 4,087、`MbrOpener` 4,132、`TextOpener` 4,177、`FixtureOpener` 4,206、`ShortOpener` 4,248。

系统依赖分类（只登记、本轮未运行）：`xz_available()` 守卫 2 个子进程测试（3,026、3,085 处缺 `xz` 即打印跳过并 return，非 `#[ignore]`）；另有多处以 `SystemImageOpener` 对 `.xz` fixture 做 plan 的测试隐含依赖宿主机 xz-utils（如 3,891），无跳过守卫；`BLKRRPART` ioctl 仅在真实设备节点上有内核语义，测试全部经注入 root/普通文件，不触真实设备、不执行挂载、不写块设备、不涉 SQL/DB。

## 2. 头部 4 条安全门禁不变量（文档归属，MUST 逐项原样保留）

头部模块文档 1–108 行随入口文件保留（rustdoc 链接依赖入口 `use`，归属不得散迁）。4 条不变量逐字保留、序次不改：

1. **承载运行 `/` 或 `/boot` 的设备永久拒绝**：`SystemInstallError::RunningSystem` 在构建 plan 时抛出，token、typed acknowledgement、root 权限、重试均不可达写入路径，刻意无 override 开关（判定链：`is_running_system_mount` 945 → `annotate_mounts` 919 → `build_plan` 571–574；解析失败 fail-closed：`expand_mounts` 965–991）。
2. **有已挂载分区的目标被拒绝**：错误列出挂载点；`apply_live` 中重建 plan 复判（575–582），旧 plan 不可重放入后来被挂载的目标。
3. **acknowledgement 必须为手打设备名、布尔不可满足**：空值 `ConfirmationRequired`、不匹配 `AcknowledgementMismatch`（429–439）。
4. **plan token 绑定 target/image/acknowledgement 三重身份，进程内一次使用、`PLAN_TTL` 过期**：apply 从当前设备态重建 plan 比对 binding（407–428），换盘或换镜像文件即 `StalePlan`。

配套顺序不变量（实施时 diff 核对基准，只移动不改）：`apply_live` 硬序为 取目标锁(400)→重建 plan(401)→验签(407)→binding(410)→签发记录与 TTL/skew 双查(413–428)→确认串(429–439)→**消费 token(440)**→开流(443)→分块写(444)→sync(445)→重读分区表验证(446)→记 `ActionRun`。**token 消耗时机**：确认串校验通过之后、打开镜像之前；被拒路径（锁未抢到/binding 不符/过期）一律不消耗（2,418 测试锁定）。

## 3. 目标私有模块树与薄入口（拟建，未创建）

沿用仓内既有 `foo.rs` + `foo/` 目录模块风格（实测 `mmc.rs` + `mmc/sys.rs`、`nvme` 同构），入口文件名不变、lib.rs 第 15 行 `mod system_install;` 零改动：

```text
crates/rsetup-core/src/system_install.rs     入口（薄）：模块级文档(1–108)+子模块声明+9 个
                                             `pub use`+跨子模块共享常量(SECTOR_BYTES/MIB/GIB/PLAN_TTL)
                                             +`BlockDeviceKind::marker` 私有 impl(1,925–1,935，
                                             私有项对全部后代子模块可见，零权限提升)
crates/rsetup-core/src/system_install/
├── error.rs         SystemInstallError + impl code()（约 60 行）
├── crypto.rs        sha256 / hmac_sha256 / constant_time_eq / hex（约 190 行）
├── token.rs         revision / ParsedToken / split_plan_token / mint_nonce / mint_token_key
│                    / token_binding / plan_token / stable_hash（约 560 行，含 6 测试）
├── mounts.rs        annotate_mounts / is_running_system_mount / expand_mounts / block_names
│                    / block_index / read_mountinfo / resolve_mount_names / collect_slaves
│                    / resolves_to_target / partition_number(_of) / longest_mount_prefix
│                    / parse_mounts / unescape_mount / read_attr（约 790 行，含 6 测试）
├── image.rs         read_partition_table / read_gpt / file_identity / same_partition_layout
│                    （约 370 行，含 4 测试）
├── xz.rs            crc32 / XZ_MAGIC / XZ_FOOTER_MAGIC / read_xz_size / trailing_null_bytes
│                    / read_xz_vli（约 430 行，含 5 测试）
├── opener.rs        SystemImageOpener / spawn_decompressor / Decompressor（约 360 行，含 4 测试）
├── backend_sys.rs   SystemBlockBackend + impl + RawDisk/RawPartition + classify + BLKRRPART
│                    两版 reread_partition_table + VIRTUAL_DEVICE_PREFIXES（约 450 行，含 2 测试）
├── backend_synth.rs SyntheticBackend / synthetic_target（`BlockDeviceKind::marker` impl 不上迁、
│                    永久留在入口，见 §5.2）（约 180 行，含 1 测试）
├── manager.rs       SystemInstallManager + Clock + install_needs_privilege + 写循环
│                    （chunk_len/partial_failure/limits 常量）（约 1,100–1,350 行，含 18 测试）
└── test_support.rs  仅 `#[cfg(test)]`：Scratch/fixture()/FixtureBackend/mbr*/gpt*/XZ_FIXTURE*/
                     RereadBehavior/install_request/boot_mounts/xz_available/flip（约 350 行）
```

`BlockDeviceBackend`/`ImageOpener`/`SystemMount` 三个抽象连同其安全注释留在入口文件（约 60 行）：它们是对外契约面（9 个导出名中 3 个），且 manager、两个 backend、opener 全部引用，留根部使依赖方向单一。估算行数为「实现+同位测试」合计，全部 ≤900 的目标除 `manager.rs`（§9 例外论证）。

## 4. 符号归属与安全文档归属

| 对外符号（lib.rs 9 名单） | 现定义 | 目标子模块 | 拆分后可见性 |
| --- | --- | --- | --- |
| `SystemInstallError`（含 `code()`） | 213/182 | `error.rs` | `pub`（不变） |
| `install_needs_privilege` | 209 | `manager.rs` | `pub`（不变） |
| `BlockDeviceBackend` / `ImageOpener` / `SystemMount` | 252/297/243 | 入口文件 | `pub`（不变） |
| `SystemInstallManager` | 306 | `manager.rs` | `pub`（不变） |
| `SystemBlockBackend` | 1353 | `backend_sys.rs` | `pub`（不变） |
| `SyntheticBackend` | 1806 | `backend_synth.rs` | `pub`（不变） |
| `SystemImageOpener` | 1654 | `opener.rs` | `pub`（不变） |

安全语义文档归属：头部 1–108（门禁/token/xz 边界/无回滚/可测性）留在入口；`consume`/`verify_preview_token`/`plan_token` 等符号上的 token 语义注释（843–851、886–894、1276–1279）随符号进 `manager.rs`/`token.rs`；`Decompressor`「EOF≠成功」论证注释随符号进 `opener.rs`；`expand_mounts` fail-closed 论证进 `mounts.rs`。均逐字迁移，不改写。非导出符号归属补充：`BlockDeviceKind::marker` impl（1,925–1,935）留在入口私有——其唯一调用点是拟迁 `token.rs` 的 `revision` 内 `target.kind.marker()`（1,124），impl 在入口即被 `token` 等全部后代直接可见，无需任何 `pub(super)` 提升，故不随合成后端进 `backend_synth.rs`（详见 §5.2）。

## 5. 依赖方向与可见性

### 5.1 依赖方向（实测引用推出，无环）

```text
入口 ←（声明并 re-export）← 全部子模块
manager → error, token, mounts, image, xz, opener, crate::transaction::ProcessLock(acquire_target_lock 520/533 与测试 2,425), 入口(抽象/共享常量含 PLAN_TTL)
token   → crypto, error, 入口(共享常量、BlockDeviceKind::marker 私有 impl)
xz      → crypto(crc32 自含), error
image   → error, 入口(SECTOR_BYTES)
opener  → error, image(file_identity)
backend_sys   → mounts, image, error, 入口(SECTOR_BYTES/常量)
backend_synth → 入口(抽象), model
test_support →（仅测试构建）manager 及任意子模块的测试所需项
```

无反向边：`mounts` 不引用 `backend_sys`（`parse_mounts`/`read_attr` 归 `mounts`，被 `backend_sys` 单向引用）；`crypto` 不引用 `token`；`image`/`xz` 互不引用（`XZ_MAGIC` 由 `xz` 提供 `pub(super)`、manager 判定格式时引用）。

### 5.2 精确最窄可见性边界（C1「可见性变更清单」）

- **对外零新增**：仅 §4 表中 9 个原本就 `pub` 的名字保持 `pub` 并由入口 `pub use`；不新增 `pub`，**全程不使用 `pub(crate)`**（当前文件实测 0 处 `pub(super)`/`pub(crate)`，拆分后新增的兄弟间暴露一律 `pub(super)`，作用域恰为 `system_install` 子树，crate 内其他模块不可见）。
- **子模块声明全部私有**（`mod xxx;`），外部经路径 `crate::system_install::xxx::yyy` 不可达。
- 跨兄弟需 `pub(super)` 的恰为以下 **22 项**（21 个函数 + 1 个常量 `XZ_MAGIC`）：`mounts::{annotate_mounts, expand_mounts, resolves_to_target, partition_number_of, longest_mount_prefix, read_attr, parse_mounts}`（7，实测调用点：`expand_mounts`/`partition_number_of`/`read_attr`/`parse_mounts` 被 `backend_sys` 的 mounts()/targets()/classify() 于 1,457/1,425/1,388–1,415/1,456/1,559 调用，`annotate_mounts`/`resolves_to_target`/`longest_mount_prefix` 被 manager 于 365/559、658、657 调用）、`token::{split_plan_token, plan_token, token_binding, mint_nonce, revision}`（5）、`crypto::{sha256, hmac_sha256, constant_time_eq, hex}`（4）、`image::{read_partition_table, file_identity, same_partition_layout}`（3）、`xz::{read_xz_size, XZ_MAGIC}`（2）、`token::mint_token_key`（1，仅 manager 的 `with_backend` 使用）。超出此清单的兄弟间引用即设计错误。
- 跨兄弟 **字段** 的精确边界（逐字段放开，共 16 个字段）：`ParsedToken` 结构体 `pub(super)` 之外，其字段 `binding`/`issued`/`signed`/`mac` 逐字段 `pub(super)`（manager 的 `parse_token`/`apply_live`/`verify_preview_token` 现读本字段，实测 407–428、900–913 行），`nonce` 保持私有（仅 `split_plan_token` 内部解析）。测试构件跨兄弟访问：`Fixture` 的全部 5 个字段 `root`/`device`/`image`/`backend`/`manager`（实测定义 2,609–2,615 行）与 `FixtureBackend` 的全部 7 个字段 `targets`/`mounts`/`device`/`fail_after`/`behavior`/`chunks`/`expected`（实测定义 2,532–2,540 行）均随结构体 `pub(super)`；迁入 `manager.rs` 的测试 `stored_on_target`（3,812）于 3,817–3,829 以完整 7 字段字面量构造 `FixtureBackend`，3,818/3,843 直接读 `backend.targets`、2,617–2,667 的 `fixture()` 返回值以 5 字段访问。`Scratch` 的私有元组字段 `.0` 不提升——跨兄弟仅经现有 `Deref`→`Path` 使用（实测 2,359–2,364 行）。其余结构体（`SystemInstallManager`/`SystemBlockBackend`/`SystemMount` 之外的 `RawDisk`/`RawPartition`/`Decompressor`）字段一律不动。
- 保持 **模块私有** 的代表：`token::{stable_hash, ParsedToken.nonce}`、`xz::{crc32, trailing_null_bytes, read_xz_vli, XZ_FOOTER_MAGIC}`、`mounts::{is_running_system_mount, block_names, block_index, read_mountinfo, resolve_mount_names, collect_slaves, unescape_mount, partition_number}`（`is_running_system_mount` 仅被同列 `annotate_mounts`(929)/`expand_mounts`(972) 调用、无跨兄弟测试直接引用，故不入 22 项）、`image::read_gpt`、`opener::{spawn_decompressor, Decompressor}`（13 号测试直测 `spawn_decompressor`，测试同位该模块即可私有）、`backend_sys::{classify, RawDisk, RawPartition}`、`manager` 全部私有方法与其 6 个私有字段（`with_clock` 除外，见下条）；入口私有的 `BlockDeviceKind::marker` impl（1,925–1,935）：私有项对本模块后代可见，`token::revision` 直接 `target.kind.marker()` 可达、零权限提升，不放入 `backend_synth.rs`（放入则 `token` 作兄弟不可达，见 §5.1）。
- **常量**：`SECTOR_BYTES`/`MIB`/`GIB`/`PLAN_TTL` 定义在入口且保持私有（Rust 私有项对后代模块可见，子模块 `use super::` 即可）；`PLAN_TTL` 是「收进唯一使用者」的例外、**不收进 `manager.rs`**：入口模块文档第 28、42 行的 `` [`PLAN_TTL`] `` intra-doc 链接只能在常量定义所处的入口作用域内解析，迁走后 rustdoc 断链（§9 R6）；`CHUNK_BYTES`/`IMAGE_PREFIX_BYTES`（实施修订：实测双消费者，留入口私有，见 §7 统一追记 B7 段）/`TOKEN_CLOCK_SKEW`/`INSTALL_ACTION_*` 收进唯一使用者 `manager.rs` 私有；`BLKRRPART`/`VIRTUAL_DEVICE_PREFIXES` 收进 `backend_sys.rs` 私有。
- **函数级唯一新增的可见性提升（测试专用，MUST 审查）**：`#[cfg(test)] fn with_clock`（现 354 行，manager 私有）→ `#[cfg(test)] pub(super) fn with_clock`。理由：3 个 token 域测试（2,953/3,248/3,355/3,384 行处调用）迁入 `token.rs` 后按 Rust 作用域规则无法调用兄弟模块私有方法；`pub(super)` 是最窄可行方案，`pub(crate)` 过宽、把 3 个测试留在 `manager.rs` 则违反 C3「测试随被测职责迁移」。非测试构建零影响。字段级放开不在此条，逐字段见上上条「跨兄弟字段的精确边界」。
- `SystemInstallManager`/`SystemBlockBackend` 的字段无跨兄弟访问需求，保持私有不动；本规格新增的字段级 `pub(super)` 仅「跨兄弟字段的精确边界」条所列三组——`ParsedToken` 的 4 个字段，以及 test-only 的 `Fixture` 5 个字段与 `FixtureBackend` 7 个字段（按真实 Rust 兄弟模块可达性要求处理，随 `#[cfg(test)]` 构件不进生产构建）。

## 6. 测试与 fixture 切分（48 个，测试随被测职责走）

共享 fixture 集中到 test-only 的 `test_support.rs`（C3 允许的 `#[cfg(test)]` 模块形态）：`Scratch`/`scratch`/`write_file`/`write_attr`/`mbr_image`/`mbr_bytes`/`mbr_partitions`/`gpt_image`/`XZ_FIXTURE`/`XZ_FIXTURE_BYTES`/`XZ_MULTI_BLOCK_FIXTURE`/`RereadBehavior`/`FixtureBackend`/`Fixture`/`fixture`/`install_request`/`boot_mounts`/`xz_available`/`flip`；`flip`（定义 2,976）被 3 个测试跨模块使用——`xz.rs` 的 frame_checksums（2,987，用于 2,997）与 `opener.rs` 的 corrupt_stream（3,025，用于 3,042）、mid_way（3,084，用于 3,091）——故列共享、入 `test_support.rs`；仅单个测试使用的 helper 只剩 `DryRunFixture`（4,424，仅 4,471 的 dry_run 测试用），随该测试留在 `manager.rs`。各测试模块经 `use crate::system_install::test_support::*` 取用，`test_support` 项以 `pub(super)` 放开（test-only）。**不设整搬巨型 `tests.rs`**；私有 helper 的测试留所属模块。逐模块归属（合计 48，实测行号）：

| 目标模块 | 测试（行号） | 数量 |
| --- | --- | --- |
| `manager.rs` | chunk_length 2,384；second_write 2,418；consequence_list 2,799；running_system 2,839；mounted 2,881；acknowledgement 2,898；failing_decompressor 3,154；edit_past_prefix 3,526；bounded_chunks 3,563；failed_write 3,605；verification 3,636；failed_reread 3,687；rejects_no_table 3,761；stored_on_target 3,812；more_data 4,083；injected_opener 4,204；ends_early 4,246；dry_run 4,471 | 18 |
| `token.rs` | single_use 2,928；other_process 3,203；editing_field 3,239；forged 3,303；preview_drift 3,346；changed_invalidates 3,428 | 6 |
| `crypto.rs` | sha256_vectors 2,693；hmac_vectors 3,401 | 2 |
| `mounts.rs` | image_on_partition 3,856；aliased_root 4,282；different_disks 4,336；unresolvable 4,404；escapes 4,630；neighbouring 4,672 | 6 |
| `image.rs` | gpt_and_xz 3,891；compressed_table 4,130；not_disk 4,175；layout_ignores_names 4,694 | 4 |
| `xz.rs` | crc32_vectors 3,418；frame_checksums 2,987；every_stream 3,978；padding 4,025；concatenated 4,041 | 5 |
| `opener.rs` | corrupt_stream 3,025；mid_way 3,084；waits_child 3,117；replacement 3,457 | 4 |
| `backend_sys.rs` | injected_root_enumerates 2,709；writes_under_root 4,525 | 2 |
| `backend_synth.rs` | demo_provider 4,584 | 1 |

断言、fixture 语义、软跳过行为逐字随迁；`FailingOpener` 等 8 个内嵌桩随测试走；2 个 xz 工具守卫测试（3,026、3,085）的 `if !xz_available() { return; }` 行为不得改成 `#[ignore]`（保持现状口径，登记见 §8）。

## 7. 分批顺序、未迁代码暂存与回滚

实施顺序 = 依赖方向自底向上，每批一个可独立回滚单元（一个提交，中文提交信息，AGENTS.md）：

| 批 | 内容 | 未迁代码暂存与临时 use 回改 |
| --- | --- | --- |
| B1 | `test_support.rs`：fixture 纯移动 + 入口 `#[cfg(test)] mod test_support;` | 其余全部实现与测试留入口原状；入口 `mod tests` 临时 `use crate::system_install::test_support::*;` 替代原 fixture 定义 |
| B2 | `error.rs` + `crypto.rs`（含 2 个向量测试） | 未迁部分留在入口文件；入口对已迁符号 `use self::error::*; use self::crypto::*;`，已迁测试对未迁 fixture 继续走 `super::*` + `test_support` |
| B3 | `token.rs`（含 6 测试、`with_clock` 提 `pub(super)`；`revision` 调 `target.kind.marker()` 直达入口私有 impl——该 impl 此批不迁不动作，原位置即后代可见，零提升） | 同上双向临时 use |
| B4 | `mounts.rs`（含 6 测试） | 同上 |
| B5 | `image.rs` + `xz.rs`（含 9 测试） | 同上 |
| B6 | `opener.rs` + `backend_sys.rs` + `backend_synth.rs`（含 7 测试） | 同上；`BlockDeviceKind::marker` impl（1,925–1,935）不随合成后端迁入 `backend_synth.rs`，永久留入口私有（§5.2） |
| B7 | `manager.rs`（状态机+写循环+`install_needs_privilege`，含 18 测试）→ 入口收薄为 §3 形态 | 收尾批，无剩余；`PLAN_TTL` 本就不迁（§5.2 常量条），`manager.rs` 以 `use super::PLAN_TTL` 引用 |

> **批次表统一追记（含于 B2 行）**：B2 实施修订（2026-10-26，提交 49e16ee）：入口对 error 采用 `pub use self::error::*`——`SystemInstallError` 属 lib.rs 9 名单，须经入口 `pub use` 保持 `crate::system_install::SystemInstallError` 公开路径（error.rs 仅 1 个 pub 项，对外零新增）；后续批次凡涉及 lib.rs 名单成员的域文件，入口同样 MUST 用 `pub use`（私有 use 将 E0324）。
>
> B6 实施修订（2026-10-26，提交 a937e67）：spawn_decompressor 与 Decompressor 提为 pub(super)——§5.2 原设私有与 §6 将 failing_decompressor 测试分配给 manager.rs（B7 才迁）在 B6 时点自相矛盾：入口测试的 FailingOpener 跨模块调用 spawn_decompressor（私有项对兄弟模块不可达），且 Decompressor 作返回类型私有将触发 private_interfaces（-D warnings 拒绝）。此为最窄裁决；B7 迁走 failing_decompressor 后，这两项 SHOULD 收窄回私有（届时在 B7 提交中完成并在修订记录登记）。
>
> B7 实施修订（2026-10-26，提交 6aaddfa）：spawn_decompressor/Decompressor 维持 pub(super) 不收窄——failing_decompressor 测试随迁 manager.rs 后，其内嵌 FailingOpener 桩仍跨文件调用 spawn_decompressor（manager.rs:890），收窄即 E0603；§7 B6 追记的『B7 迁走后 SHOULD 收窄回私有』分支前提不成立，按实测裁决保持。
>
> IMAGE_PREFIX_BYTES 实测为 manager 与 opener（replacement 测试）双消费者，§6 分属两模块；收进 manager 私有将使 opener 测试不可达、提 pub(super) 超出 22 项清单，故按共享常量模式留入口私有（与 SECTOR_BYTES/MIB/GIB/PLAN_TTL 同类），兄弟模块既有 import 路径零改动（提交 6aaddfa）。

规则：未迁代码**只留在入口 `system_install.rs` 原位置**，不另立临时文件、不注释掉、不双份共存；每批删除入口内被迁走的定义并补一行 `use`，同批迁移该职责测试。单批回滚 = revert 该提交（改动面恒为「入口 + 该批新文件」），不依赖后续批。各批完成不等于全量拆分完成，期间门禁以实施会话逐批 `cargo` 结果为准，本规格不声称任何编译/测试通过。

## 8. 验证命令（登记，本轮一律未执行）

前提实测：包名 `rsetup-core`（Cargo.toml `name`）；core 编译不含 `rsetup-controller` 的 build.rs，**不需要 web dist**（总览 F2 仅约束 controller），`make test`/`make lint` 因走 workspace 才带 `web-dist-check` 先决。`--locked` 一律放 cargo 参数侧（`cargo test -p rsetup-core --locked`），不得置于 `--` 之后。

- `make fmt-check`（总览 F1；`rsetup-core` 在 `FMT_PACKAGES` 第 1 位，实测 Makefile 73 行）
- `cargo clippy -p rsetup-core --all-targets -- -D warnings`
- `cargo doc -p rsetup-core`（intra-doc 链接解析检查：入口模块文档 `` [`PLAN_TTL`] `` 两处（28、42 行）依赖该常量定义留在入口作用域（§5.2 常量条、§9 R6），门禁链接 `[SystemInstallError::RunningSystem]` 等同批覆盖；本轮仅登记、未执行）
- `cargo test -p rsetup-core --locked`（48 个测试全在 lib 目标内）
- 全量回归出口：`make controller-test-offline`（F2/F3 前提）
- 有副作用项只登记不执行：本文件无 `#[ignore]` 轨道；2 个子进程测试依 `xz_available()` 软跳过（缺 xz-utils 时静默通过）、数处 xz-plan 测试隐含依赖 xz-utils；`BLKRRPART` ioctl、`O_EXCL` 独占打开的真实设备语义在注入 root 普通文件上不覆盖，需真机操作员验证（与 09 号 `hardware` 的真机轨道同类，`2026-10-26-large-file-split/09-hardware.md`，未创建）；本模块与 MySQL/TiDB `--ignored` 真库轨道（总览 F6）完全无关，测试不产生块设备写入、挂载或数据库副作用（写文件仅限 `target/system-install-tests/`）。

## 9. 模块专属风险、阈值例外与 DoD

- **风险 R1 顺序与消耗时机**：§2 的 `apply_live` 硬序与「确认串后才消费 token」是行为协议（测试 2,418 锁定），B7 移动时以语句序 diff 核对；锁先于重建 plan 取得（398–400 注释）不得重排。
- **风险 R2 设备写入语义**：`O_EXCL` 独占打开、句柄缓存按 `target.id` 键控、`sync_all` 后置空句柄、ioctl 在 sync 之后（`reread_partitions` 1516–1532），任一前移都会改变故障形态；只移动。
- **风险 R3 挂载探测 fail-closed**：`expand_mounts` 对不可解析的运行系统挂载报 `Unsupported` 拒绝一切目标（962–977），迁移中不得「顺手」改成降级。
- **风险 R4 算法逐字性**：sha256/HMAC/FNV/crc32/GPT/xz 反向走流与既有 fixture 向量强耦合，禁止借拆分换实现或加依赖（模块注释明示工作区无 hash 依赖）；`hmac` 仅 RFC 4231 向量测试。
- **风险 R5 合成后端**：`SyntheticBackend` 生产用途（§1.1）不得因「demo」字样被归入 test-only；其 `lock_root` 继承 trait 默认 `None`（跳过进程间锁）与 `is_dry_run` 继承默认 `false` 是行为事实，保持。
- **风险 R6 门禁文档链接**：入口 rustdoc 的 `[SystemInstallError::RunningSystem]` 等 intra-doc 链接依赖入口的 `pub use`，B2 起保持入口 re-export 完整即不破坏；`` [`PLAN_TTL`] `` 链接两处（28、42 行）还依赖该常量的定义处于入口作用域，故 `PLAN_TTL` 定义留在入口私有、不收进 `manager.rs`（manager 用 `use super::PLAN_TTL`）；`cargo doc` 链接解析仅登记未执行（§8），实施批内若移动常量为先，链接断言会在该批文档构建暴露。
- **C4 阈值例外申请（唯一）**：`manager.rs` 估算 1,100–1,350 行（含 18 测试）。论证：plan/apply 状态机含锁、门禁复判、token 校验、消费、写循环、验证与 `ActionRun` 记账是一条不可切薄的顺序协议（§2），18 个同位测试与被测语句互证；再拆（如写循环独立文件）会把「错误文本含已写字节数」这类跨文件断言语义拆散。本轮不再拆，若审阅认为必要，切分点只能在 `write_image`/`partial_failure`/`chunk_len` 三符号处另立决议。
- **DoD**：① C1–C7 逐条自检记录；② lib.rs 9 名单逐项比对 + 4 个 rsetup-app 消费面与 desktop shell 的 import 零修改；③ §2 四条不变量与 §6 48 测试计数迁移前后逐字核对（48 = 48，软跳过行为保持）；④ §5.2 可见性清单核对：新增 `pub(super)` 恰为 22 项函数/常量符号 + `ParsedToken` 4 字段 + test-only `Fixture` 5 字段、`FixtureBackend` 7 字段 + 1 个 `#[cfg(test)] with_clock`；`BlockDeviceKind::marker` impl 与 `PLAN_TTL` 均入口私有、零提升；新增 `pub(crate)` 0、新增 `pub` 0；⑤ §8 命令按前提执行并如实报告（含未执行项）；⑥ 每批 revert 可用；⑦ 不新增 `Cargo.toml` 依赖、不改 CI/Makefile；⑧ 真机 `BLKRRPART`/`O_EXCL` 结论沿用原注释「pending hardware access」表述，不新增未经批准的验证声称。

修订记录：B2偏差追记（已修订，2026-10-26，提交 49e16ee；见 §7 批次表统一追记）。
B6偏差追记（已修订，2026-10-26，提交 a937e67/419f9c7；见 §7 统一追记）。
B7裁决追记（已修订，2026-10-26，提交 6aaddfa；见 §7 统一追记）。

实施登记（补录，2026-10-28；批次实施期间未逐批追记，现据 git 历史补录；B1–B7 全批次状态：B2/B6/B7 见上，其余补录如下）：
- `52ca7b1` refactor(core): 拆分 system_install 测试夹具至 system_install/test_support（批次 B1，审查 PASS）
- `d6d7e51` refactor(core): 拆分 system_install token 域至 system_install/token.rs（批次 B3，审查 PASS）
- `222b900` refactor(core): 拆分 system_install mounts 域至 system_install/mounts（批次 B4，审查 PASS）
- `cb865dc` refactor(core): 拆分 system_install image 与 xz 域至 system_install/image、xz（批次 B5，审查 PASS）

