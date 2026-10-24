# v4 真实引擎核验与 TiDB 表级重建记录

日期：2026-10-24（Asia/Shanghai）。本记录只含去敏结果，不含地址、库名、账号、凭据、私有事务标识或数据内容。

## 1. 授权及执行边界

用户明确要求 DB schema 仅支持 v4，删除旧版本升级与测试路线；已确认在真实一致性备份核验后，清除指定 TiDB 专用开发库内的旧表并直接初始化 v4，旧数据不迁移，已有 v4 的 MySQL 保留。用户授权不包含修改数据库全局配置或 DROP DATABASE，也不扩大到其它目标。

数据库操作由用户信任的 local-qwen 子代理执行，凭据只从本地受限 JSON 在进程内载入，输出私有捕获。真实逻辑备份及回执仅保存在 Git 忽略的私有目录，不属于交付源码。备份做过完整性与一致性核验，但**没有恢复演练**，不称“已验证可恢复”。

## 2. MySQL 现存 v4（4011cf4a）

- 实际引擎：MySQL **8.0**，不是 MySQL 8.4 LTS 的验收证据。
- 当前目标与配置精确匹配；真实备份哈希、CRC、尺寸及目标绑定均通过核验。
- 实际结构：16 张 BASE TABLE、122 列、40 个索引、0 FOREIGN KEY、0 CHECK；singleton=1、schema_version=4。
- 重新编译当前 v4 基线修复后的 CLI 和只读测试，排除旧缓存；CLI `--check` exit 0 只证明版本探测。
- 完整 `v4_schema_readonly_verify` 唯一用例实际执行：**1 passed / 0 failed / 0 filtered out，exit 0**，包含真实 `check_identity_schema` 的结构与数据闭合检查。
- 未执行 `--apply`、DDL、DML、重建、管理员初始化或 TiDB 操作；MySQL 原有数据保留。

此前真实只读核验曾因源码 DDL 行内注释导致 `targets_json` 期望提取失败，不是库里缺列。该源码缺陷已修复，补了全表契约自闭合回归，之后上述真实重验通过。

## 3. TiDB 一致性备份（d4e6c355）

- 实际引擎：TiDB 8.5；重建前 schema v2，11 张 BASE TABLE、2 行数据。
- 同一连接执行一致性事务；服务器快照标识在导出前后稳定，回滚后归零，本连接事务身份核验通过。
- 同事务内两遍全表读取的行数及内容摘要逐表一致，schema 指纹前后一致。
- 导出 11 条 CREATE、2 条逐行 INSERT；gzip 全流 CRC、压缩及原始 SHA256、字节数均通过校验。主代理又独立校验备份文件的哈希、CRC 和计数。
- 未执行 DDL、DML、SET、恢复或清表。旧非一致性 mysqldump 仅保留留档，不作为清表门禁。

## 4. TiDB 表级重建（e892f51c）

执行前重新核对了目标、TiDB 引擎、11 张既有表的精确白名单、2 行现状、真实一致性备份绑定、权限及已审不可变 CLI／只读测试工件。独立对象检查：无 views、triggers、routines、events。先对旧版本调用显式入口，确认在版本门禁拒绝且零 DDL 后，才执行获授权的表级重建。

实际操作：

1. 仅删除核对过的 **11 张旧表**；删除后表对象数为 0。
2. 使用生产 `rsetup-controller-migrate --apply` 直接创建 **16 张 v4 表**，由该入口写入 **1 条 schema_meta**；没有手工执行 SQL 文件或绕过入口写 meta。
3. `--apply` exit 0、`--check` exit 0。
4. 完整只读测试实际执行 **1 passed / 0 failed / 0 filtered out，exit 0**。

最终实际状态：**16 BASE TABLE / 122 列 / 40 索引 / FK 0 / CHECK 0 / schema_version 4 / singleton 1 / initialized=0**，总数据仅 schema_meta 单例。旧数据未迁移，尚未 bootstrap 或创建管理员，也未运行 controller。

执行异常如实记录：私有脚本在 11 条 DROP 已完成后，把合法零值误按负值处理而中止。代理只读确认现场空库后修正零值判断并继续执行显式初始化，**没有重复 DROP、没有恢复旧 schema**，操作明细和回执保留。另一次防重入验证确认现库已是 v4 时拒绝再次删除，零写入。

冻结工件使用的是 v4 主体及注释修复已审版本；随后并行的失败结果诊断／注释收尾不改变 DDL，并已另行独立复审。为核对最终当前源码，后续双引擎测试将重新编译，不能把旧工件等同于所有新代码已在真库执行。

未执行 DROP DATABASE、全局配置修改、旧升级链、MySQL 重建或任何主机重启。

## 5. 双引擎真实准入用例复验（已完成，2026-10-24）

以当前源码在 `target/tunnel-admission-real-fix` 重新构建（`--locked --offline`），两引擎串行执行，逐条 `--exact --ignored --test-threads=1`，私有目录 0700／文件 0600 留存明细。既有 fixture 记录不允许误删：开跑前逐引擎确认四个夹具公钥（首字节 + 31 零字节）在 `devices`/`admission_decisions`/`audit_events` 均无既有记录。

- 首轮真实 RED（4 次，两引擎同因）：`tests/transport_admission.rs:770` 把 v4 的 `JSON` 列按 `String` 直读（`VARCHAR` 与 `JSON` 不兼容）、`:882` 把 `SELECT COUNT(*)` 的有符号 `BIGINT` 按 `u64` 读（要求 UNSIGNED 标志）。两处均为**测试侧解码写法**，不是引擎差异也不是生产 SQL 缺陷；用例 1 在首次 INSERT 后读 descriptor 时 panic（未走到去重与 CAS），用例 2 写前 panic，夹具均已精确清理。
- 修复范围只有测试文件两处解码（JSON 列显式 `CAST(... AS CHAR)` 后保留原 descriptor 与两个 `TimeEvidence` serde 强断言；`COUNT(*)` 按 `i64` 读再 `usize::try_from`，负数失败、不 `as` 截断）。生产 Rust、`schema/v4.sql`、`Cargo.toml`/`Cargo.lock` 零改动。
- 真实 GREEN：MySQL 8.0 与 TiDB 8.5 各「登记/去重/复查」「容量收敛」两条用例 **1 passed / 0 failed / exit 0**；两引擎完整 `v4_schema_readonly_verify` 各 **1 passed / 0 failed / 0 filtered out / exit 0**。CAS 批准/吊销/拒绝、`Rejected(2)/Rejected(3)`、预算等于存量时只得 `Rejected(5)` 且零写入、既有批准身份不受容量预算影响，均在真实引擎上通过；未出现需要改生产或改库才能通过的证据。
- 备份门禁逐引擎重核：备份文件存在、gz SHA256、gzip 全流 CRC 与原始 SHA256/字节数、目标库绑定 tag 全部匹配。MySQL 侧为 v4 期权威备份；TiDB 侧是表级重建**前**的一致性快照（`authoritative`/`consistent_snapshot` 真、`writes_performed`/`migration_performed` 假、`restore_drill = not_performed`、当时 11 张表），**未被伪造成 v4 快照**，也仍无恢复演练。
- 闭合：MySQL 侧 `devices` 0、`admission_decisions` 0、`audit_events` 10（既有）、`users` 1、管理员 1、`initialized` 真、`authz_epoch` 0、`admin_guard_revision` 0、`schema_version` 4、16 表前后不变；TiDB 侧对应值 0/0/0/0/0/假/0/0/4/16 不变。CAS 在用例内真实写审计与决定行，随后由用例自带按键 `DELETE` 精确回收，不宣称「零改库」。未创建或重置用户，未 `DROP DATABASE`、未 `--apply`、未迁移、未运行 controller 服务、未改全局配置。
- 默认离线门禁同步复跑：`transport_admission` **32 passed / 0 failed / 2 ignored**，单包 `cargo fmt --check` 与 `cargo clippy -p rsetup-controller --all-targets -- -D warnings` 均 exit 0。

## 6. 尚未完成的验收

真实 controller／board 两个独立进程、审批 API、RPC 状态轮询、撤销停止重连、线上 GOAWAY 时序、故障注入、约 1024 会话容量、完整包安装仍未完成。库内测试、同进程回环及 schema 门禁不能替代这些证据。工作区尚未提交或推送。

## 7. 后续首次真实双进程：失败现场保留（2026-10-24）

双引擎限定 SQL 测试完成后，TiDB 的 fresh v4 又完成一份新的真实一致性快照；旧 v2 备份
保留，仍未做恢复演练。随后以两个冻结且哈希核实的真实生产二进制执行 Task7：dry-run
通过，identity／版本检查／controller health／真实 bootstrap、首次登录、强制改密及重新
登录均成功，但等待审批列表出现 Pending 超时，执行 **exit 4 / complete=false**。
未走到 approve、Ready、状态样本、revoke 或 restart preflight，不能称闭环通过。

现场 TiDB v4 结构不变；数据真实变为 **initialized=true、管理员 1、设备 1、审计 4、总行 9**。
这些是 bootstrap、会话及设备登记的真实副作用，未清理或重置；MySQL 未触及。两个生产
子进程均已回收，无 orphan。§4／§5 的空库或精确夹具清理后计数是此前时点，不再作为
这次失败后“当前空库”的声明。

可信只读诊断（b52ae465）核私有 HTTP／stderr 与库内类型：设备插入前审批列表唯一一次
200 空页，随后 86 次 503（固定 STORAGE_UNAVAILABLE），再转 transport 错误。源码显示
审批 SELECT 的真实 JSON 被 `Option<String>` 读取；这是生产查询错误，区别于 §5 已修的
测试夹具查询。另有独立生命周期缺陷：监听的关停 30s 宽限期在正常启动时就开始计时，
导致 controller 在收到任何 SIGTERM **之前**自行 abort 监听并退出。时间门的
`ready=false` 是合法 SystemFallback，不是本次根因。

两项生产根因已分开派修；DB／协议门禁不放宽，不需新建未授权库或清除既有管理员。
修复和独立审查后，后续执行需新空 state、显式现有管理员凭据及现况备份，不能假称首次
bootstrap。board 的多次 open 失败具体类别因旧日志未导出尚不能确定，不臆测加密失败。

