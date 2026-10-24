# Controller 已验签身份 SQL 准入适配报告（Task：controller-admission）

任务书：`.superpowers/sdd/2026-10-23-production-v3-tunnel/controller-admission-brief.md`
规范：`docs/protocol_spec.md` §4.6（准入状态机）、§4.2/§4.3（descriptor 与 reason 语义）
基线：身份 schema v4（`schema_meta.schema_version = 4`），未新增/修改任何 migration 或表结构。

## 1. 改动面（严格限定）

| 文件 | 状态 |
| --- | --- |
| `crates/rsetup-controller/src/transport/admission.rs` | 由 1 行注释重写为生产实现（含 3 个纯判定单元测试） |
| `crates/rsetup-controller/tests/transport_admission.rs` | 新增集成验收（21 个本任务默认用例 + 2 个 `#[ignore]` 真库用例；另 11 个来自共享 `tests/common`，合计 32 passed / 2 ignored） |
| `docs/testing/tunnel-controller-admission-report.md` | 本报告 |

未触碰：`lib.rs`、`main.rs`、`Cargo.toml`、`migrations/*`、其它 writer 的文件。`transport/mod.rs` 与
`rsetup-tunnel` 依赖由主代理预先登记，本任务只使用。无 commit/push/`cargo clean`，独立
`CARGO_TARGET_DIR=target/tunnel-controller-admission`。

2026-10-24 真库复验轮次的改动面同样严格限定：只有 `crates/rsetup-controller/tests/transport_admission.rs`
真库用例的 2 处解码写法（4.3）与本报告、`docs/testing/v4-real-engine-verification.md`；生产
Rust、`schema/v4.sql`、`Cargo.toml`/`Cargo.lock` 零改动，独立 `CARGO_TARGET_DIR=target/tunnel-admission-real-fix`，
同样无 commit/push/`cargo clean`。

## 2. 交付内容

`SqlxAdmissionPolicy::new(db: DbPool, max_identities: usize)` 实现
`rsetup_tunnel::handshake::AdmissionPolicy`，消费 `VerifiedPeer`。装配层必须只从
`server_handshake` 已验签路径调用本策略；其public字段可构造，“已验签”是caller契约
而非不可伪造的类型证明。本模块不重复验签，对每次 `evaluate` 执行真实 SQL 短事务：

1. **锁序与既有管理 CAS 一致**：`db.begin()` → `integrity::lock_integrity_guard(&mut tx)`（v4
   singleton guard，与 `AdmissionStore::compare_and_set`/管理写同一把 guard）→
   `SELECT ... FROM devices WHERE public_key = ? FOR UPDATE`。既有行分支只读、立即提交释放 guard；
   新身份分支在同一事务内计数后 INSERT。事务不含审批等待、不含网络 IO，并受
   `DEFAULT_DB_TIMEOUT = 5s` 墙钟预算切断（握手初始步骤 10s，`DEFAULT_DB_TIMEOUT` 只允许收缩）。
2. **状态严格映射**（合法域复用 `integrity::validate_device_fields`）：
   `APPROVED+approved` → `Approved`；`PENDING+none` → `Pending`；`PENDING+denied` →
   `Rejected(APPROVAL_DENIED=3)`；`REVOKED+revoked` → `Rejected(REVOKED=2)`。
   归档行、`archived ∉ {0,1}`、合法域之外组合、任何数据库异常、超时、descriptor/时间口径不合法
   一律 `HandshakeError::Policy` 失败关闭（`stops_auto_reconnect() == false`，不落永久决定）。
3. **未知身份登记**：容量预算内 `INSERT ... 'PENDING','none',revision 1,archived FALSE` +
   `descriptor_json` + `first_seen`/`last_seen`。登记语句的状态/决定/revision/archived 全是字面量，
   绑定参数只有 5 个去敏字段，模块内不存在 UPDATE 路径（由源码结构门禁钉住）。
   并发首次握手由 guard 串行 + `public_key` 主键 + 锁定读收敛为一条记录。
4. **容量不是拒绝审批**：`COUNT(*) FROM devices >= max_identities` 时在同一事务内
   `Err(CapacityExhausted)` → 不写任何行、`tx` drop 回滚 → 握手层回**签名**
   `REJECTED(reason=SERVER_ERROR=5)`（板端按 §7 退避重试，绝不停止自动重连）；
   既有已批准/已拒绝/挂起身份在预算为 0 时仍按最新状态复查放行或回绝。
   `max_identities` 构造时收缩到 `MAX_IDENTITY_BUDGET = 8_192` 以内，`0` 表示关闭新身份登记。
5. **零缓存**：`evaluate` 每次实查 SQL，`Registered` 恒 `Pending`，批准后的吊销下一次立即生效
   （握手 final recheck 与撤销优先依赖此点，用例 `evaluate_reloads_latest_state_every_time` 断言
   3 次 evaluate 恰 3 次仓储调用）。
6. **去敏与档案口径**：`display_name` = 固定前缀「设备 」+ 公钥 hex 前 12 字符（恒 19 字节，过
   `validate_display_name`，不含任何对端文本）；descriptor 以 4 个 typed 字段原值记录为独立 JSON
   （不作验签材料、不重编码），四段之和超 `MAX_DESCRIPTOR_WIRE_BYTES` 即失败关闭；
   `first_seen`/`last_seen` 复用 `db::system_fallback_time_evidence()` 并反核质量位必须为
   `system_fallback`（默认系统时间不得被标成 `ntp_valid`）。
7. **可观测**：容量拒绝计数 `SqlxAdmissionPolicy::capacity_rejections()`，并以首次 + 每 256 次节流的
   去敏 stderr 记录资源原因。本 crate 尚无结构化日志汇，指标接入归网络装配层（未越界改 `metrics`）。

## 3. 验证证据（本地实测，未连真库）

| 命令 | 结果 |
| --- | --- |
| `CARGO_TARGET_DIR=target/tunnel-controller-admission cargo test -p rsetup-controller --test transport_admission --locked` | `34 tests`：**32 passed / 0 failed / 2 ignored**（2 个 `#[ignore]` 真库用例） |
| `cargo test -p rsetup-controller --lib --locked` | **991 passed / 0 failed / 3 ignored**（含本模块 3 个纯判定用例） |
| `cargo clippy -p rsetup-controller --all-targets --locked --no-deps -- -D warnings` | **clean（Finished）** |
| `cargo fmt --package rsetup-controller -- --check` | **clean（无 diff）** |

真库用例解码修复后（2026-10-24，`CARGO_TARGET_DIR=target/tunnel-admission-real-fix`，`--locked --offline`）复跑：

| 命令 | 结果 |
| --- | --- |
| `cargo test -p rsetup-controller --test transport_admission --locked --offline` | **32 passed / 0 failed / 2 ignored**（默认不连真库） |
| `cargo clippy -p rsetup-controller --all-targets --locked --offline -- -D warnings` | **clean（exit 0；本轮不再需要 `--no-deps` 口径）** |
| `cargo fmt --package rsetup-controller -- --check` | **clean（exit 0）** |
| `cargo test -p rsetup-controller --test transport_admission --test v4_schema_readonly_verify --no-run` | 两个真库工件重新构建；`sha256` 与私有回执记录一致，运行器按哈希拒绝旧工件 |

TDD 轨迹（真实 RED→GREEN）：

1. 先写测试 → RED：`error[E0432] unresolved imports rsetup_controller::transport::admission::*`
   （实现尚不存在，桩/骨架也无法通过 `production_path_takes_guard_before_row_lock_and_wraps_write`）。
2. 实现后再次 RED（测试侧缺陷）：`policy_clamps_max_identities_to_budget` 在 `#[test]`（无 tokio
   上下文）里构造 sqlx lazy 池 → `this functionality requires a Tokio context` → 改 `#[tokio::test]`。
3. clippy 首次报本模块 `manual_is_multiple_of` → 改用 `is_multiple_of` → 复跑 clean。
4. 全绿：32 passed。

覆盖的边界矩阵（离线可证）：

* 状态映射：4 个合法组合 + 归档 3×4 组合 + `archived ∈ {-1,2,i64::MAX}` + 12 组非法/污染组合；
  断言既有状态只可能映射到 reason 2/3，绝不产出 `SIGNATURE_INVALID=1`。
* 容量：`new_identity_allowed` 边界（等号即拒）、预算 clamp（`8193→8192`、`512→512`、`0→0`）、
  容量拒绝映射为 `Rejected(5)` 且 ≠2/3、计数 +1、既有身份不受预算影响、`CapacityExhausted`
  在 INSERT 之前返回。
* 零缓存与草稿：脚本化 fake repository 断言 3 次 evaluate → 3 次读取（Pending→Approved→Revoked）；
  首次登记草稿字段（派生名不含 `SN-0001`/`R2-D2`、descriptor JSON 四段 typed 原值含中文/Unicode、
  `first_seen == last_seen` 且 `quality = system_fallback`）。
* 失败关闭：仓储错误、归档行、非法组合、超预算 descriptor（断言 `loads == 0`，取连接之前即失败）、
  真实不可达 lazy 池（`Err(HandshakeError::Policy)`，<10s 返回、不伪批准）、
  挂起仓储被 25ms 预算切断（`Err(Policy)`，<5s）。
* 源码结构门禁（`include_str!` 读当前实现）：锁序 `guard → LOCK_ADMISSION_ROW_SQL →
  COUNT_IDENTITIES_SQL → CapacityExhausted → REGISTER_NEW_IDENTITY_SQL`；全模块恰 2 处
  `tx.commit()`；批准常量只允许出现在 `classify_row` 的一处映射；无 `todo!`/`unimplemented!`。
* SQL 形状门禁：登记语句恰 5 个绑定、含 `'PENDING'`/`'none'`/`FALSE` 与五个去敏列，且**不含**
  `APPROVED`/`REVOKED`/`TRUE`/`UPDATE`/`ON DUPLICATE`（结构上不可能自动批准或改写决定）；
  锁定读以 `public_key = ?` 结尾 `FOR UPDATE`；计数语句不持行锁。

## 4. 真实数据库：两引擎已实测（2026-10-24 复验）

第 3 节的离线证据本身不连真库。真库验收在后续可信任务中由操作员在两个**隔离、独占、可丢弃**
的 v4 开发库上真实执行，先真实 RED（4.2）、修测试侧解码（4.3）后真实 GREEN（4.4）。

### 4.1 两个显式 `#[ignore]` 用例覆盖什么（`tests/transport_admission.rs::real_db::*`）

* `real_db_registers_dedupes_and_rechecks_latest_decision`：首次登记 → 断言 `PENDING`/`none`/
  `revision = 1`/`archived = 0`/descriptor JSON 四段 typed 原值/`first_seen`+`last_seen` 质量位为
  `system_fallback` → 3 路并发重复登记仍只有 1 行且 `revision` 不变 → 用既有
  `AdmissionStore::compare_and_set`（同 guard、`actor_id = None` 的系统决定）批准 → 下一次
  `evaluate` 立即放行 → 再吊销 → `Rejected(2)` → 另一公钥走 `PENDING`+`denied` → `Rejected(3)`。
* `real_db_capacity_convergence_leaves_existing_identities_alone`：预算 = 当前存量 → 未知公钥只得
  `Rejected(5)` 且**不写行**；同一预算下既有已批准身份仍 `Approved`。

门禁与红线：夹具经 `tests/common::required_test_db()`（`CONTROLLER_TEST_*` + `authorize` 全量确认，
凭据仅进程内从环境载入），本任务不读取 `secret/`、不触发任何破坏性迁移、不伪造备份引用、
不运行未满足隔离门禁的真库、不 `DROP DATABASE`；用例只做自身公钥的表级 `DELETE` 清理
（`devices`/`admission_decisions`/`audit_events`）。夹具库必须是操作员预迁移的当前 v4 基线
（用例内调用 `check_identity_schema` 复核，不做 v1/v2/v3 中间迁移）。

### 4.2 首轮真实 RED：4 次失败与根因（两引擎完全同因）

第一轮真实执行时，MySQL 8.0 与 TiDB 8.5 各 2 条用例失败（共 4 次，`exit 101`，同引擎两侧源行一致）：

| 失败位置 | 现象 | 根因 |
| --- | --- | --- |
| `tests/transport_admission.rs:770`（读 `descriptor_json`/`first_seen`/`last_seen`） | `mismatched types`：SQL 列 `JSON` 与 Rust `String` 声明的 `VARCHAR` 不兼容 | v4 三列真实类型是 `JSON`（`schema/v4.sql`），sqlx 0.8 的 `String` 只声明兼容 VarChar/Blob 系。**测试侧解码写法错**，不是引擎差异，也不是生产 SQL 缺陷 |
| `tests/transport_admission.rs:882`（`SELECT COUNT(*) FROM devices`） | `mismatched types`：SQL 有符号 `BIGINT` 与 Rust `u64` 声明的 `BIGINT UNSIGNED` 不兼容 | 两引擎都以**有符号** `BIGINT` 回报 `COUNT(*)`，而 sqlx 无符号整数的 `compatible` 判定要求列带 UNSIGNED 标志；生产 `COUNT_IDENTITIES_SQL` 本就按 `i64` 读再 checked 转换。**测试侧解码写法错** |

两次 panic 都落在用例自身的断言查询上：用例 1 在首次 INSERT 成功、读 descriptor 时中断（未走到
去重与 CAS）；用例 2 在任何写入前中断。夹具由用例自带的按键 `DELETE` 精确回收，首轮残留
（用例 1 一行 `devices`）已按同一公钥清干净。

### 4.3 修复：只改测试侧解码写法（2 处），不迁就测试改生产

* JSON 三列改为 `CAST(... AS CHAR)` 显式取 JSON 文本，再走**原有** serde 强断言。选它而不是
  `sqlx::types::Json<serde_json::Value>`：workspace 的 `sqlx` 未启用 `json` feature（改用它要动
  `Cargo.toml`，超出本次范围），而 `CAST(... AS CHAR)` 正是本仓既有 JSON 读取口径
  （`src/tasks/repository.rs`、`src/authz/sqlx_repo.rs`）。descriptor 四段 typed 原值与
  `first_seen`/`last_seen` 两个 `TimeEvidence` 质量位断言全部保留，未放宽为「非空即通过」。
* `COUNT(*)` 改按 `i64` 读，再 `usize::try_from` 转预算：负数直接失败，不用 `as` 截断。
* 同文件其余真实查询逐一核对：`revision`（`BIGINT UNSIGNED`）用 `u64`、
  `CAST(archived AS SIGNED)` 与按键 `COUNT(*)` 用 `i64`，均与真实列类型相符，保持不变。
* 生产 Rust、`schema/v4.sql`、`Cargo.toml`/`Cargo.lock` 零改动（`v4.sql` SHA256 与上一轮记录一致）；
  容量 0 夹具的合法语义（预算 = 当前存量）保留，未改成「永远有数据」的假绿。

### 4.4 真实 GREEN：两引擎串行各 2 条 exact ignored 用例 + 完整只读门禁

执行前置（逐引擎，只读）：备份文件存在 + gz SHA256 + gzip 全流 CRC 与原始 SHA256/字节数 +
目标库绑定 tag 匹配；TiDB 另核 `authoritative`/`consistent_snapshot` 为真、`writes_performed`/
`migration_performed` 为假、`restore_drill = not_performed`（该备份是表级重建**前**的一致性快照，
未被伪造成 v4 快照）；现库形状 16 表 / 122 列 / 40 索引 / FK 0 / CHECK 0 / `schema_version` 4 /
singleton 1；四个夹具公钥（首字节 + 31 个零字节）在 `devices`/`admission_decisions`/`audit_events`
均无既有记录，任一存在即拒绝开跑。凭据只从受限 JSON 进程内载入，不入 argv、不入 console。

| 引擎 | 执行 | 结果 |
| --- | --- | --- |
| MySQL 8.0 | `--exact --ignored --test-threads=1` 登记/去重/复查 | **1 passed / 0 failed / exit 0** |
| MySQL 8.0 | 同上，容量收敛 | **1 passed / 0 failed / exit 0** |
| MySQL 8.0 | `v4_schema_readonly_verify` | **1 passed / 0 failed / 0 filtered out / exit 0** |
| TiDB 8.5 | `--exact --ignored --test-threads=1` 登记/去重/复查 | **1 passed / 0 failed / exit 0** |
| TiDB 8.5 | 同上，容量收敛 | **1 passed / 0 failed / exit 0** |
| TiDB 8.5 | `v4_schema_readonly_verify` | **1 passed / 0 failed / 0 filtered out / exit 0** |

因此在真实引擎上得到验收的事项：`PENDING`/`none`/`revision = 1`/`archived = 0` 真实入库与
JSON 列写入、first/last_seen 反读口径；同一公钥 3 路并发登记收敛为 1 行且 `revision` 不推进；
`compare_and_set` 批准后下一次 `evaluate` 立即 `Approved`（无缓存）；吊销优先 → `Rejected(2)`；
`PENDING` + `denied` → `Rejected(3)`；预算 = 存量（当时为 0）时未知公钥只得 `Rejected(5)` 且零写入、
`capacity_rejections == 1`，同预算下既有批准身份仍 `Approved`。CAS 的 guard、审计写入均走通，
**没有出现需要修改生产代码或改库才能通过的证据**。

副作用与闭合（如实记录，不称「零改库」）：CAS 在用例内真实写入 `admission_decisions` 与
`audit_events`，随后由用例自带的按键 `DELETE` 精确回收。MySQL 侧运行前后 `devices` 0、
`admission_decisions` 0、`audit_events` 10（既有值不变）、`users` 1、管理员 1、`initialized` 真、
`authz_epoch` 0、`admin_guard_revision` 0、`schema_version` 4 全部不变；TiDB 侧对应
0/0/0/0/0/假/0/0/4 不变。未 `DROP DATABASE`、未 `--apply`、未执行任何迁移、未创建用户或管理员、
未运行 controller 服务、未改库全局设置。私有明细（备份核验、逐用例子进程输出、前后计数、
源码与二进制 SHA256、脱敏回执）保留在 Git 忽略的 `secret/admission-real-fix-20261023T194837Z/`，
目录 0700、文件 0600，不含地址、账号、凭据或行内容。

仍**未**验收：guard 串行化在真实握手并发下的吞吐与锁等待、生产装配接入、两个独立进程的端到端
部署与约 1024 会话容量（清单见 `v4-real-engine-verification.md`）。库内用例不能替代这些证据。

### 4.5 运行方式（仅在操作员逐项确认后）

两条用例须逐条 exact 串行，且必须用当前源码重新构建的工件（旧二进制不可复用）：

```bash
CARGO_TARGET_DIR=target/tunnel-admission-real-fix \
cargo test -p rsetup-controller --test transport_admission --no-run --locked --offline
# 再对每个已授权目标库，以进程内环境变量提供 CONTROLLER_TEST_DATABASE_URL（URL 编码）、
# CONTROLLER_TEST_EXPECTED_DATABASE、CONTROLLER_TEST_ALLOW_DESTRUCTIVE=1、
# CONTROLLER_TEST_BACKUP_REF=<真实备份文件相对路径>、
# CONTROLLER_TEST_MIGRATION_ACK=isolated-exclusive-backed-up-disposable，
# 直接调用 deps 下的测试可执行文件：
#   <exe> --exact --ignored --test-threads=1 real_db::real_db_registers_dedupes_and_rechecks_latest_decision
#   <exe> --exact --ignored --test-threads=1 real_db::real_db_capacity_convergence_leaves_existing_identities_alone
```

凭据五值不入 argv、不 `source`、不开 tracing；子进程输出私有捕获后先做凭据扫描再打印摘要。

## 5. 设计决定与与任务书的偏差

1. **descriptor 预算取 `MAX_DESCRIPTOR_WIRE_BYTES = 16_247`**（握手层既有唯一真相，
   `rsetup_protocol::sig`），而非任务书表述的 16 384：后者是握手帧 payload 级预算，descriptor 段的
   实际上限由该常量强制。本模块用它做防御性复核（正常路径握手层已保证）。
2. **容量计数为整表 `SELECT COUNT(*) FROM devices`（含 `archived` 行）**：归档行仍占存储且不能由
   握手复活，把它排除在预算外会让表无界增长；资源不足永远是可重试的 5，不是永久拒绝。
3. **既有身份路径不回写 `last_seen`**：`evaluate` 会被 PENDING 期间的每个 probe 周期与 final
   recheck 反复调用，回写会把只读复核变成写放大与 guard 竞争；`revision` 语义属管理 CAS，
   不允许被握手路径推进。任务书只要求首次登记的 first/last_seen 证据，故未扩展。
4. **`AdmissionRepository` 泛型 seam（`SqlxAdmissionPolicy<R = SqlxAdmissionRepository>`）**：任务书
   要求「可拆 pure decision + repository trait」；因该 trait 采用 RPITIT（非对象安全，与本 crate
   既有 `AdmissionStore` 同形），策略用泛型而非 `dyn`。公开构造签名 `new(db, max_identities)` 不变。
5. **临界区墙钟预算 `DEFAULT_DB_TIMEOUT = 5s`（只可收缩）**：任务书要求「不持锁等审批/网络」，
   但未定数值；取握手初始步骤 10s 的一半，超时失败关闭并可重试。
6. **容量原因的记录方式**：本 crate 无 `tracing`/`log` 依赖，故用进程内计数 + 节流 stderr；
   未擅自引入日志框架，也未越界改 `metrics` 模块。装配层需要接指标时可直接读
   `capacity_rejections()`。
7. **`clippy` 全依赖口径当前不可用**：`cargo clippy -p rsetup-controller --all-targets -- -D warnings`
   会在他人未提交的在途文件 `crates/rsetup-tunnel/src/h2_keepalive.rs` 报 2 处
   `clippy::question_mark`（Task 4a/4c 范围）。按任务书「并行 I/O 变化可暂编译不稳定，不得越界改」，
   本任务改用 `--no-deps` 证明**本包 clean**，并在此如实记录该口径限制。

## 6. 遗留与后续（供主代理排期）

* **未装配**：`transport::admission` 目前没有生产引用方（全部条目 `pub`，无 dead-code 告警）；网络装配（Task 5/6）
  需注入 `SqlxAdmissionPolicy::new(db, max)` 到 `ServerHandshake` 的 `AdmissionPolicy` 参数，并按部署
  规模设定 `max_identities`，同时把 `capacity_rejections()` 接入指标与告警。
* **guard 串行化的吞吐**：每次 `evaluate`（含每个 probe 周期的复查）都会取 v4 singleton guard，
  与全部管理写互斥。短事务 + 5s 预算 + 容量预算限制了放大，但真实引擎上的握手并发表现未测；
  若成为瓶颈，需要专家评审「既有行只读复核是否允许走无 guard 的一致性读」，不能自行放宽。
* **`APPROVAL_DENIED` 的持久记录细化**（§4.6 第 4 条要求应用层细化）与管理员重新开放申请流程
  仍属后续任务；本模块只保证不把它与资源拒绝混用。
* **真库验收**：两个 `#[ignore]` 用例已在 MySQL 8.0 与 TiDB 8.5 上真实通过（见 4.4）；剩余未验收项
  是并发吞吐、生产装配与端到端部署，不再包含「准入判定未在真实引擎跑过」。


## 独立主审与文档收尾

GLM3cb929ac独立Spec/Quality PASS，复跑32passed/2ignored与3纯判定目标、no-deps clippy/fmt；真实引擎仍未验收。共享夹具数量更正为11，已验签改成caller前提而非类型不可伪造。既有身份last_seen的后续更新职责交controller连接管理/可信快照轮询装配：通过成功认证且当前代际的活动证据更新，不能由未经验证peer或单纯TCP在线推进；目前该装配未完成，不能称活动时间已闭环。

2026-10-24 真库复验收尾：首轮两引擎 4 次真实失败均为测试侧 sqlx 解码写法（JSON 列按 `String` 直读、`COUNT(*)` 按 `u64` 读），按 4.3 修最小两处后，MySQL 8.0 与 TiDB 8.5 各 2 条 exact ignored 用例与完整 v4 只读门禁全部真实通过（4.4）。本行之上的「真实引擎仍未验收」表述已被 4.2–4.4 取代；生产 Rust/SQL/Cargo 未改，容量 0 夹具语义与全部业务断言保留。

### 查询修复的独立复审（53742bc5）

**Spec PASS / Quality PASS，两处 ADDRESS，无新回归。** reviewer 独立核查 JSON 的显式
CAST 后仍有原 serde 断言、COUNT 有符号读取及 checked 转换、容量 0 夹具和按键清理均未
放宽；独立实跑默认用例 **32 passed / 0 failed / 2 ignored**，包级格式检查通过。
reviewer 未读取凭据、未连真实库；§4 的双引擎 GREEN 来源为可信执行任务 eb655639 的实际
运行，不冒称 reviewer 又独立执行了真库测试。真实生产两进程、1024 会话等仍未验。

历史口径澄清：§5.7 的 clippy 依赖阻断与§6/原主审段的“未装配”描述是首次组件交付时点，
后来 controller listener/manager 已实际接入该策略并通过组件审查；线上互通证据仍待真实
两进程测试，不能反向把历史文字当作现版本完全无生产引用，也不能把编译接线当部署验收。

