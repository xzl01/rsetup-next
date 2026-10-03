# 中控 V1 任务与运行期 TDD 实施计划（条件性 · 03）

> **致执行代理：** 使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐项执行。每个新增行为和下列边界测试都须先写测试、确认因目标行为缺失而失败，再最少实现、确认通过、全绿重构并提交；使用 `- [ ]` 记录每步。

**Goal:** 为独立中控实现时间质量、错峰查询、最新快照、持久化重启主子任务与受控恢复，不自动重复可能已执行的重启。

**Architecture:** 在 01 的 `crates/rsetup-controller` 中新增时间、任务、轮询和运维模块；经 `BoardClient` 使用 02 的已认证加密 Control 流。只读查询与 prepare 在事务外取得必要证据/票据，随后在 DB 短事务提交 dispatching 发送意图，提交后才在事务外调用 reboot.execute；不是所有板端 RPC 都要先写发送意图。连接和最新快照驻内存，主子任务、设备锁和审计驻 MySQL/TiDB。

**Tech Stack:** Rust 2024/MSRV 1.85，Tokio、Axum、tracing；MySQL/TiDB。SQLx、NTP/时间注入库均为待审阅候选。

**Spec:** `docs/superpowers/specs/2026-09-22-controller-design.md`、`docs/superpowers/specs/2026-09-23-controller-v1-00-index.md` 及同目录 02-data-api、03-device-protocol、04-task-lifecycle、05-runtime-operations、06-web-acceptance 的完整同前缀文件、`docs/protocol_spec.md`。执行者必须阅读规格原文和本计划。

## Global Constraints

- `controller-v1 / draft-1` 未获实施批准。用户仅同意条件性规划；API、状态、票据、NTP/数据库版本与限额/保留期须逐项审阅。05 §1/§2/§4/§6 的 NTP 单次请求上限3s、板端时钟 RTT 上限2s、状态保留112/s/其它48/s（可借空闲）、unknown 后台核实共用查询预算且可低频/不占重启并发槽，以及 05 §1 的私钥0600权限检查、启动进度日志，均为拟议而非获批；05 §10 仅列专项验收，不作为这些参数细节的出处。`reason_code` 签名、验签失败及 AEAD 失败策略的协议修订与双端测试完成前不得生产发布。
- 依据[应用层完整性修订](../specs/2026-10-03-controller-application-integrity-design.md)，identity `0003_identity_application_integrity.sql` 完成且只读结构/数据检查确认 schema_version=3 是未来 Tasks 前置；迁移文件为 `0004_tasks.sql`，完成后 schema_version=4。v3 仅认精确 identity 表集合；v4 同时闭合检查 identity 与 runtime 的表/列/PK/唯一及普通索引、NULL/类型和应用数据完整性。空库、旧版或未知编号一律只读拒绝普通启动，不自动 CREATE/ALTER；未知/已占用 0003、0004 或版本标记与实际结构不符，停止迁移另审，不能猜编号、跳过 identity3 或只加 runtime 表。新 schema 无 FK/CHECK，保留 PK/UNIQUE/NOT NULL/类型长度；事务内校验引用、状态与锁所有权，不以 DB 约束替代。部署/备份编号盘点及两库测试均未由本计划完成；原协议安全门不变。
- 新增依赖须在同一任务内连同根/crate manifest 与 `Cargo.lock` 提交；新依赖须过 MSRV 1.85 与 CI 多 target 检查。
- 每个去重设备恰好一个子任务（包括失败目标）；同设备跨批次持久互斥，后来者 `DEVICE_BUSY` 且不排队。事务内无网络；dispatching 意图一旦提交视为可能已发送，恢复只查询核实。unknown 完成批次但持设备锁。
- TTL、重试、保活、新鲜度使用单调时间，不持久复用上个进程的 monotonic 截止。独立 NTP 启动预算最多30s，fallback后仍 ready 且后台无限有界退避。每台在线设备10s常规采样/1024规模是目标而非已有性能证明；过载跳过不积压，probe 预算不得被业务挤占。
- MySQL/TiDB 分别验证迁移、唯一性、CAS/并发和备份恢复；缺库是未验证，不是 PASS。旧备份恢复必须进入受控模式，不能按普通进程重启自动重放。不得复用板端无认证 `crates/rsetup-app/src/server.rs` 的操作端点；首版不存历史监控，未知值不填0；不记录密码/票据/私钥到审计。
- 所有额外边界用例也须逐一先 RED 后 GREEN；测试工具/fixture/数据库缺失、导入错误不能算有效 RED。每任务单独提交，只有全绿才纯重构。

## 文件结构、职责和接口

- Modify `crates/rsetup-controller/{Cargo.toml,src/lib.rs,src/main.rs,src/db.rs,src/model.rs,src/audit.rs,src/api/mod.rs}`：接线与复用 01 认证/ACL，不复制。
- Create `crates/rsetup-controller/migrations/0004_tasks.sql`：task_previews、main_tasks、sub_tasks、device_operation_locks、recovery_checks，显式唯一约束与 revision CAS；已用 0001 不原地改。
- Create `crates/rsetup-controller/src/time/{mod.rs,evidence.rs,ntp.rs,board_clock.rs}`：NTP/时间质量/四时戳；`src/tasks/{mod.rs,model.rs,preview.rs,repository.rs,evidence.rs,scheduler.rs,recovery.rs}`：主子任务状态/执行/恢复；`src/polling/{mod.rs,scheduler.rs,snapshot.rs}`：有界轮询/最新缓存；`src/operations/{mod.rs,backup.rs,mode.rs,retention.rs}`：旧备份受控恢复与待审保留期；`src/api/{tasks.rs,events.rs,system.rs,audit.rs}`：管理 API。
- Create `crates/rsetup-controller/tests/{task_db.rs,controller_recovery.rs,runtime_db.rs}`：真实 DB 集成用例一律 `#[ignore]`，使用 01 计划 `tests/common/mod.rs` 的 `required_test_db()` 和 `CONTROLLER_TEST_DATABASE_URL` 独占空库契约；缺 URL 标未验证。允许另写不调用 `required_test_db()` 的 fake repository/board 纯单测，须标明 fake 边界，不能冒充锁、CAS 或备份的真实 DB 结果。三个集成测试目标均需按引擎用显式 URL 和 `-- --ignored --test-threads=1` 运行，逐目标核对实际执行非零用例及失败数；普通 `cargo test --workspace --locked`/`make test` 不运行 ignored 用例。测试夹具随首个使用它的测试建立，不假装现成。
- Consumes 01 的 `DeviceId/UserId/AuthzRepository/AppState/authz_epoch`；02 的 `rsetup_protocol::wire::{DeviceStatus,ClockSample,TaskRecord,RebootTicket}`。本专题新增 `BoardClient` trait：`capabilities/status/clock/prepare/execute/task_record` 均以认证 DeviceId 作为目标，返回 `Result<wire_type,BoardError>`，production adapter 使用 02 已认证 Control 流，fake 只隔离网络边界。Produces `TimeProvider::evidence()->TimeEvidence`、`TaskService::{preview,submit}`、`TaskScheduler::run_ready()`、`merge_task_record`、`PollScheduler::due(now)`、`SnapshotStore::read(device,now)`。管理路径以 02 `/api/v1/task-previews`、`/tasks`、`/events`、`/system/*` 为准；接口不一致先协调四份计划和测试，不制造第二套设备身份。

---

### Task 1: 时间证据与独立 NTP 启动关卡

**Files:** Create `src/time/{mod.rs,evidence.rs,ntp.rs,board_clock.rs}`, `src/api/system.rs`；Modify `src/{main.rs,model.rs,audit.rs}`, 根 `Cargo.toml`、`crates/rsetup-controller/Cargo.toml`、`Cargo.lock`。

**Interfaces:** `Clock::{wall_utc,monotonic_now}`，`TimeProvider::evidence()->TimeEvidence` 含 system_wall_utc/reference_utc/quality/clock_epoch/source/sample_age_ms/offset_ms/uncertainty_ms；`start_time_gate(clock,client,sources)->TimeGate`，`estimate_board_offset(t1,t2,t3,t4,epochs,boot)->Option<ClockEstimate>`。

- [ ] **Step 1 RED：**
```rust
#[tokio::test(start_paused=true)] async fn ntp_timeout_yields_fallback_and_retry() {
    let gate=tokio::spawn(start_time_gate(FakeClock::new(),NeverRespondsNtp,vec!["slow".into()]));
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    let ready=gate.await.unwrap();
    assert_eq!(ready.quality,TimeQuality::SystemFallback);
    assert!(ready.background_retry_active());
}
```
- [ ] **Step 2 确认失败：** 在可编译空接口、Tokio test-util/可控时钟 fixture 就绪后运行 `cargo test -p rsetup-controller ntp_timeout_yields_fallback_and_retry`；预期30s后仍未 fallback/重试的断言失败，不把缺依赖或导入作为 RED。
- [ ] **Step 3 最少 GREEN：** `tokio::time::timeout(Duration::from_secs(30),query_all_sources())` 将 DNS、多源及每次请求计入总预算；单次请求建议上限3s，有效样本早放行，失败按当时墙钟 fallback，后台无限有界指数退避（建议1s→60s封顶加≤1s抖动，空源按退避重读配置），成功清失败次数、60s复验/180s样本期限为待审默认。NTP 关卡与 DB 初始化可并行，ready 仍需两者完成；30s 仅限 NTP 引入的等待，不含迁移时长；关卡前不开放业务/设备接入或恢复下发，检查私钥0600权限并将启动进度写日志。保留原始墙钟/参考、质量及单调样本年龄；跳钟/来源切换更换 epoch，过期 stale/未知 offset 为 null。NTP 拒 LI=3、stratum非法、来源/请求不匹配/往返劣质，不宣称未认证 NTP 可抗篡改。板端偏差 `((T2-T1)+(T3-T4))/2`，RTT `(T4-T1)-(T3-T2)`，建议上限2s，负RTT/跨 epoch/boot/超时丢弃，大稳定偏差保留；两主机 monotonic 绝对值不相减。`audit.rs` 枚举受控恢复模式进入/退出及 NTP 回退/恢复/过期/跳钟的语言中立审计事件。
- [ ] **Step 4 确认通过：** 目标测试 PASS；空源无忙循环、持续丢包、样本过期/跳钟、板端1970/无RTC、四时戳跨代际各自红绿；另测迁移超过30s仍在 NTP 回退及 DB 初始化完成后 ready，不因整段迁移超时；`cargo test -p rsetup-controller time::` 全绿。
- [ ] **Step 5 重构、回归、提交：** 抽采样检验纯函数，复跑；`git add Cargo.toml Cargo.lock crates/rsetup-controller/Cargo.toml crates/rsetup-controller/src/time crates/rsetup-controller/src/api/system.rs crates/rsetup-controller/src/main.rs crates/rsetup-controller/src/model.rs crates/rsetup-controller/src/audit.rs && git commit -m 'feat(controller): add time evidence'`。

### Task 2: 预览、幂等提交与持久设备锁

**Files:** Create `migrations/0004_tasks.sql`, `src/tasks/{mod.rs,model.rs,preview.rs,repository.rs}`, `tests/task_db.rs`；Modify `src/{db.rs,audit.rs}`。

**Interfaces:** identity schema_version=3 完成并通过只读结构/数据校验、部署与备份确认未占用 0003/0004 后才可执行 `0004_tasks.sql`；占用、未知版本或身份数据污染则拒绝，不能跳号/猜测表意义。`TaskService::preview(actor,device_ids,group_ids)->Preview`；`TaskService::submit(actor,preview_token,idempotency_key)->TaskId`；actor+key+规范请求去重，按 DeviceId 申请 DB 唯一锁。提交或撤权操作共享 `schema_meta(singleton=1) FOR UPDATE` guard；事务内重新确认 actor 存在且 active、权限与 authz_epoch、组/设备存在且未归档、preview/主任务/子任务及锁的父子引用和 state/owner/generation；按 users(UUID)→roles(UUID)→device_groups(UUID)→devices(公钥)→依赖行取锁，同类型按 ID 排序。不可把事务外 preview/鉴权快照当提交资格；关系与主子任务、设备锁唯一性同时依应用校验及 PK/UNIQUE/CAS，不能仅内存互斥或 FK/CHECK。先验证全部 runtime 表/列/索引及应用完整性再推进 schema_version=4；普通启动仍只读，禁止自动生产 ALTER。

- [ ] **Step 1 RED：**
```rust
#[tokio::test]
#[ignore] // 真实 DB：仅显式 URL + --ignored 运行
async fn later_batch_is_busy_without_network_send() {
    let f=task_fixture().await;
    let first=f.submit_for(device_a(),uuid::Uuid::new_v4()).await.unwrap();
    let later=f.submit_for(device_a(),uuid::Uuid::new_v4()).await.unwrap();
    assert_eq!(f.subtask(first,device_a()).await.state,SubState::Queued);
    assert_eq!(f.subtask(later,device_a()).await.reason,Some(Reason::DeviceBusy));
    assert_eq!(f.board_calls(),0);
}
```
- [ ] **Step 2 确认失败：** 隔离真实 DB fixture（调用 `required_test_db()`）与可编译 stub 就绪后，MySQL：`CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test -p rsetup-controller --test task_db later_batch_is_busy_without_network_send -- --ignored --exact --test-threads=1`；TiDB：`CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test -p rsetup-controller --test task_db later_batch_is_busy_without_network_send -- --ignored --exact --test-threads=1`。仅在各自 URL 存在时运行，逐引擎核对实际执行 1 个用例且因锁竞争/零网络行为断言失败；缺 URL 标未验证，连接错误或零用例不算 RED。
- [ ] **Step 3 最少 GREEN：** 预览显式不可见 ID 整体404、可见组成员固定展开，去重排序/空集400/最多1024；至少128bit随机 token 只存摘要、绑定用户/重启/目标/进程代际、建议单调60s有效。提交与撤权/组成员变更使用共同 guard 和对象锁序，在短事务重读 actor active、authz_epoch、当前授权与设备/组状态；barrier 分别测试“撤权先提交→任务拒绝/失败”及“任务先提交→撤权不伪称已撤回已发任务”，并检查审计失败时任务/锁/epoch 回滚。完全不可见整体404无任务，仍可见但无reboot/离线/不支持每个也建立 failed 子任务；合格目标按 DeviceId 排序后逐项申请 DB 锁，忙即 failed/DEVICE_BUSY 不排队；queued 排队有效期建议60min（待审默认），过期转 expired；审计与一目标一子任务同事务，commit 前零网络。actor+同key同请求在 preview 过期后也返回原 ID，异请求409；请求哈希和 preview_token_hash 持久化。
- [ ] **Step 4 确认通过：** 目标 PASS；固定清单/多组去重、显式不可见、1025、同key并发/异内容、撤权竞争、排队过期、审计失败回滚逐一红绿；真实 DB 用例全部 `#[ignore]`。MySQL：`CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test -p rsetup-controller --test task_db -- --ignored --test-threads=1`；TiDB：`CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test -p rsetup-controller --test task_db -- --ignored --test-threads=1`。两引擎逐一核对实际执行非零、零失败并各记被测版本；缺 URL 标未验证，有 URL 但失败/零用例标失败，普通 `make test` 不替代此验证。
- [ ] **Step 5 重构、回归、提交：** 抽纯预览/事务验证函数并回归；`git add crates/rsetup-controller/migrations/0004_tasks.sql crates/rsetup-controller/src/tasks crates/rsetup-controller/src/db.rs crates/rsetup-controller/src/audit.rs crates/rsetup-controller/tests/task_db.rs && git commit -m 'feat(controller): commit idempotent tasks'`。

### Task 3: 状态机、串行下发与不重发的进程恢复

**Files:** Create `src/tasks/{evidence.rs,scheduler.rs,recovery.rs}`, `tests/controller_recovery.rs`；Modify `src/tasks/{model.rs,repository.rs}`。

**Interfaces:** `merge_task_record(subtask,record)->MergeOutcome`、`TaskScheduler::run_ready()`、`recover_process()`；恢复先从 DB 重建锁/不确定状态，再开放新的变更。

- [ ] **Step 1 RED：** 另先写 `main_task_outcome_corrected_by_late_evidence`：覆盖全成功→success、全取消→cancelled、有unknown→unknown、成功混合其他终态→partial、其余→failed；unknown 后晚到有效证据修正 completed 的 outcome/计数/revision，状态不回 running，非 completed 的 outcome 为 null。以下 `RecoveryFixture` 使用 `required_test_db()` 持久化意图/锁并注入 fake board，两个真实 DB 集成用例均标 `#[ignore]`；若另有 fake repository 纯单测，必须明确不调用 `required_test_db()`、只验证纯状态机而不计真实 DB 恢复覆盖。
```rust
#[tokio::test]
#[ignore] // 持久化恢复：需显式 CONTROLLER_TEST_DATABASE_URL + --ignored
async fn crash_after_intent_never_auto_resends() {
    let f=RecoveryFixture::new().await; f.commit_intent_then_crash().await;
    f.restart_controller().await;
    assert_eq!(f.execute_count(),0);
    assert_eq!(f.subtask().await.state,SubState::Verifying);
    assert!(f.lock_is_held().await);
}
```
- [ ] **Step 2 确认失败：** DB/board 故障 fixture 与可编译 stub 就绪后，分别以 `CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL"` / `CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL"` 前缀运行 `cargo test -p rsetup-controller --test controller_recovery crash_after_intent_never_auto_resends -- --ignored --exact --test-threads=1`；同样逐引擎运行 `cargo test -p rsetup-controller --test controller_recovery main_task_outcome_corrected_by_late_evidence -- --ignored --exact --test-threads=1`（该例也访问真实 DB）。每条命令核对实际执行 1 个用例并确认失败来自错误重发/提前释放锁或汇总修正缺失；缺引擎 URL 标未验证，连接失败/零用例不算 RED。
- [ ] **Step 3 最少 GREEN：** 有限全局槽（建议16），同子任务检查最新账号/`device.reboot`/准入/Control健康/能力/boot/TTL/锁→prepare→再次检查 authz_epoch→短事务提交 dispatching 意图→事务外一个协程 execute。代码边界 `repo.commit_dispatch_intent(id,epoch,pre_boot,ticket_digest).await?; board.execute(device,id,ticket).await?;`，两句之间事务已提交。RPC 5s超时、掉线或 TASK_NOT_FOUND 不证明未执行，只 task.get/事件核实；观察窗口建议5min，未知仍持设备锁但让出并发槽，后台可低频核实并共用查询预算。可信 queued 重新校验后可继续；无法可信计算旧单调 TTL 则 held，明确过期 expired；已 dispatching/accepted/verifying 不再发送，不能重建可靠剩余观察窗口可直接 unknown。状态依 04 合法转移、主任务按 completed/running/held/queued 汇总，unknown 为批次终态而非成功；completed outcome 依 04 §4 计算，全成功 success、全取消 cancelled、有 unknown 则 unknown、无 unknown 且有成功与非成功则 partial、其余 failed；unknown 晚到有效证据可修正 completed 的 outcome/计数/revision，但不转回 running 或 queued。success 要求认证同设备/同子任务/journal_epoch、可靠 attempted、pre_boot 相符、当前认证 boot 与 pre_boot 不同且与 observed_boot_id 一致及 evidence_kind=boot_transition_observed；仅 boot 变、掉线或 Pong 不够。板端低版本/旧代际丢弃，同版本矛盾告警；释放锁必须 device+owner_subtask+generation，旧回调不得删新锁。
- [ ] **Step 4 确认通过：** 目标 PASS；prepare拒绝、票据过期/重用或日志满不执行、未知业务版本/畸形字段核实拒绝、撤权/取消/TTL争用、dispatching 意图 DB commit 失败时 execute 零发送（允许此前事务外的只读查询/prepare）、OS及网络崩溃、日志缺失、通知乱序、unknown保锁/晚到证据和旧锁代际各自红绿；`main_task_outcome_corrected_by_late_evidence` GREEN，确认 completed 不回 running。真实 DB 回归分别运行 `CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test -p rsetup-controller --test controller_recovery -- --ignored --test-threads=1` 与 `CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test -p rsetup-controller --test controller_recovery -- --ignored --test-threads=1`；逐引擎核对非零实际用例、零失败及被测版本，缺 URL 标未验证。fake repository 纯单测可普通运行，但不代替真实 DB。
- [ ] **Step 5 重构、回归、提交：** 仅全绿后抽转移与证据纯函数并回归；`git add crates/rsetup-controller/src/tasks crates/rsetup-controller/tests/controller_recovery.rs && git commit -m 'feat(controller): recover without unsafe replay'`。

### Task 4: 千台错峰轮询和最新快照

**Files:** Create `src/polling/{mod.rs,scheduler.rs,snapshot.rs}`；Modify `src/{lib.rs,main.rs}`。

**Interfaces:** `PollScheduler::due(now)->Vec<DeviceId>`、`SnapshotStore::update(session_epoch,status,receive_evidence)`、`SnapshotStore::read(device,now)->SnapshotView`。周期采集不产生业务主子任务，多浏览器共享同一采集结果。普通查询建议160/s、burst32、在途768；全局未决请求4096是数量上限（含 probe），**status过期阈值30s**按单调接收年龄计算，不能误用为未决请求 TTL；均须审阅压测。

- [ ] **Step 1 RED：** 下述 `phase_fixture(origin: Instant) -> PollScheduler`、`fixture_device_id(i: u16) -> DeviceId`、`finish_status(s: &mut PollScheduler, ids: &[DeviceId])` 均为随首个测试新建的未来纯单测 fixture（不访问数据库或真实网络）：`fixture_device_id` 对 `0..1024` 确定性生成长度为 32B、互异且可用作测试公钥身份的 `DeviceId`，前者据此以 1024 个 `DeviceId`、10s 周期和 `u128` 整数纳秒相位 `i * period_nanos / 1024` 注册设备（`i=0..1024`，相位含 0，不用浮点），设备均在线且无其他查询/在途干扰，预算每 100ms tick 至少 11；后者仅模拟已发 status 响应以清除在途项，不改变下一周期相位。fixture 可按未来 scheduler 实际构造方式实现，但必须维持这里的 `due(now: Instant)` 签名与单调时间轴。`assert_phase_distribution` 也是同一步建立的测试 helper：逐个 100ms tick 记录每 tick 下发数，断言每 tick ≤11、整轮恰好 1024 个互异 ID 且无 `backlog`；避免仅检查总积压而漏掉恢复后的同 tick 洪峰。暂停期间不调用 `due`；过载期间调用 `due` 但禁止发送。
```rust
use std::time::{Duration, Instant};

#[test]
fn overdue_after_pause_keeps_ten_second_phase_distribution() {
    let origin = Instant::now();
    let mut s = phase_fixture(origin);
    let now = origin.checked_add(Duration::from_secs(30)).unwrap()
        .checked_add(Duration::from_nanos(1)).unwrap();
    let first = s.due(now); // 停顿恢复的同 tick 逾期轮次一律跳过（含 0 相位）
    assert!(first.is_empty());
    assert_phase_distribution(&mut s, origin, 30_000); // 后续 (30s,40s] 的 100 个 tick
}

#[test]
fn overload_skips_without_backlog_or_phase_collapse() {
    let origin = Instant::now();
    let mut s = phase_fixture(origin);
    let now = origin.checked_add(Duration::from_secs(30)).unwrap()
        .checked_add(Duration::from_nanos(1)).unwrap();
    s.set_overloaded(true);
    assert!(s.due(now).is_empty()); // 同 tick 逾期，跳过，不积压
    s.set_overloaded(false);
    assert!(s.due(now).is_empty()); // 同一时刻恢复不重放刚跳过的轮次
    assert_phase_distribution(&mut s, origin, 30_000);
}
```

`assert_phase_distribution(s: &mut PollScheduler, origin: Instant, base_ms: u64)` 使用整数 `for tick in 1..=100_u64` 与 `origin.checked_add(Duration::from_millis(base_ms.checked_add(tick.checked_mul(100).unwrap()).unwrap())).unwrap()` 构造各 tick，逐次 `let ids = s.due(at)`，校验 `ids.len() <= 11`，每个 ID 属于 fixture 登记的 1024 个合法设备、`assert!(seen.insert(id))` 不重复，再调用 `finish_status(s, &ids)`；循环结束核对 `seen.len() == 1024`、`s.backlog() == 0`。fixture 中相位为 `i * 10s / 1024`，其中 0 相位在 30s+1ns 首次恢复 tick 或过载时被跳过，下次恰在 40s；其余设备只在严格大于 30s+1ns 的原相位到期。因此两用例都应在后续 100 tick 恰收齐 1024 个 ID，不以睡眠、浮点毫秒或跨进程旧 `Instant` 制造假 RED。
- [ ] **Step 2 确认失败：** 可编译的 `phase_fixture`、`finish_status`、`assert_phase_distribution`、待测 `due(now)`/`backlog`/`set_overloaded` 接口就绪后，分别运行 `cargo test -p rsetup-controller overdue_after_pause_keeps_ten_second_phase_distribution` 和 `cargo test -p rsetup-controller overload_skips_without_backlog_or_phase_collapse`；须因同 tick 逾期设备集中下发、整轮相位分布崩塌或积压行为而 RED，不把 helper 缺失、时钟不匹配或测试配置错误充数。
- [ ] **Step 3 最少 GREEN：** 按设备登记单调 `origin: Instant` 与固定整数 `phase: Duration`（`0 <= phase < period`），`next_due` 始终取 `origin + phase + k*period`；不以过载发生时刻重置相位。停顿恢复或过载/同设备在途致跳轮时，针对旧 `next_due <= now` 计算 `missed = (now.duration_since(next_due).as_nanos() / period.as_nanos()) + 1`（`period > 0`）；对纳秒乘积用 `u128::checked_mul`、对秒数转换用 `u64::try_from`、对 `Instant` 用 `checked_add` 检验范围后置 `next_due = old_next_due + missed*period`，即严格大于 `now` 的第一个同相位周期；溢出显式拒绝/报错，不静默饱和成 `now + period`。若正常到期且未过载/未在途，仅下发这一轮并将下一到期仍按原相位推进；暂停后恢复不得把所有旧到期排成当前 tick 的洪峰，过载跳过也不积压补发。所有期限用同一进程单调 `Instant`，不跨进程持久保存或重用 `origin/next_due`；重启重新均匀构造相位并从当前单调基点安排，不用旧墙钟/旧 `Instant`。同设备常规 status 最多一项在途；任务核实优先但状态保留112/s、其它48/s，可借空闲。1024设备×2流 probe 预留本端2048名额，额外连接先准入限额，不按理论上限为每台预分配1024。当前快照只由当前 session/agent_epoch、严格更高 sample_seq 更新；分存板端原始时间、中控接收 TimeEvidence/单调时刻；冷启动无样本显示等待，新鲜度仅按单调接收年龄，采集失败保留上次好样本并显示 last_error，未知数值不填0。
- [ ] **Step 4 确认通过：** 分别运行上述两个目标测试 PASS，核对 1024 个设备/10s 在暂停和过载同 tick 逾期后恢复仍逐 100ms tick 分布、无积压/重复，且正常到期不因跳轮算法饿死；慢设备、集中重连、双浏览器、普通查询饱和仍可 probe、旧会话结果不覆盖分别红绿；`cargo test -p rsetup-controller polling::` 全绿，记录内存上限和拒绝计数。
- [ ] **Step 5 重构、回归、提交：** 抽查询预算器后重跑；`git add crates/rsetup-controller/src/polling crates/rsetup-controller/src/lib.rs crates/rsetup-controller/src/main.rs && git commit -m 'feat(controller): stagger bounded polling'`。

### Task 5: 授权任务 API、SSE 与系统视图

**Files:** Create `src/api/{tasks.rs,events.rs,audit.rs}`；Modify `src/api/{mod.rs,system.rs}`, `src/{lib.rs,audit.rs}`。

**Interfaces:** 在 01 `build_router(AppState)` 内接入规格 02 的 `/api/v1/task-previews`、`/tasks`、`GET /api/v1/tasks/{id}`（主任务详情，含授权子集投影）、`/tasks/{id}/children`、`/tasks/{id}/cancel`、`/tasks/{id}/time-review`、`/subtasks/{id}/release-lock`、`/events`、`/audit`、`/system/{time,status}`。SSE 事件类型 `device.updated, task.updated, permissions.changed, system.time.changed, reset`，时间事件仅管理员；每连接最多128条待发（待审默认），满时 reset/关闭。

- [ ] **Step 1 RED：** 除授权子集投影外，先写管理员跨用户取消用例。`cancel_fixture(owner,states)` 是本任务新增的测试 helper：建立具有合法 UUID 的主子任务、逐设备锁和身份；`task_router_fixture()` 返回 `(Router, Uuid)`，其中第二项是已创建共享批次的合法 UUIDv4 task id（不是占位路径）；`alice` 为任务所有者，`bob` 为无该任务读取权限的普通用户，`admin` 为有效且已改密管理员。`cancel_as` 向真实 router 提交当前 revision、`confirm:true`、有效 cookie/CSRF；`states()`、`lock_is_held(index)` 从测试 repository 读取状态和锁，不以 handler 调用次数代替结果。此处若注入 fake repository，明确不调用 `required_test_db()`、只覆盖 router 投影/权限；真实 DB 锁/CAS 由 Task 2/3/6 的 ignored 集成用例单独验证。
```rust
#[tokio::test] async fn admin_cancels_another_users_queued_and_held_only() {
    let f=cancel_fixture("alice",&[SubState::Queued,SubState::Held,SubState::Dispatching]).await;
    assert_eq!(f.cancel_as("bob").await.status(),StatusCode::NOT_FOUND);
    assert_eq!(f.states().await,vec![SubState::Queued,SubState::Held,SubState::Dispatching]);
    assert_eq!(f.cancel_as("admin").await.status(),StatusCode::OK);
    assert_eq!(f.states().await,vec![SubState::Cancelled,SubState::Cancelled,SubState::Dispatching]);
    assert!(!f.lock_is_held(0).await);
    assert!(!f.lock_is_held(1).await);
    assert!(f.lock_is_held(2).await);
}
#[tokio::test] async fn task_count_hides_unreadable_devices() {
    let (app,task_id)=task_router_fixture().await;
    let body=get_json(&app,alice_cookie(),&format!("/api/v1/tasks/{task_id}")).await;
    assert_eq!(body["data"]["view_scope"],"authorized_subset");
    assert_eq!(body["data"]["counts"]["succeeded"],1);
    assert!(body["data"].get("total_targets").is_none());
}
```
- [ ] **Step 2 确认失败：** 分别运行 `cargo test -p rsetup-controller task_count_hides_unreadable_devices` 与 `cargo test -p rsetup-controller admin_cancels_another_users_queued_and_held_only`；先确保 helper/路由可编译，确认断言因隐藏数量泄漏或管理员取消分支缺失而失败。
- [ ] **Step 3 最少 GREEN：** 复用 01 的 session/Host/Origin/JSON/CSRF/强制改密关卡；创建任务 202 仅 task_id/accepted，不绕过 task.read；普通用户只看本人且当前有 task.read 的目标，主子任务按可见子集重算，完全不可见同不存在404。普通用户取消仅作用于本人主任务中当前仍有 `device.reboot` 的 queued/held 子任务；有效管理员可取消任意用户任务的 queued/held 子任务，不受创建者身份或普通 grant 限制。两条分支均重验当前账号/权限，CAS 与 dispatching 竞争，按 owner_subtask+generation 仅释放取消成功子任务自己的锁，不能取消 dispatching 及之后状态；取消响应仍按当前读取权限投影，不泄露无 task.read 的执行数据。管理员 time-review 按主任务粒度对全部 held 子任务原子核验原截止、不延长，无 held 则400；unknown 释放须独立风险确认、不改原结果或重发旧任务。SSE 每次投递前查 session/epoch，撤权关闭旧订阅且丢旧消息，队列满 reset/关闭后让客户端 GET 重取。实现投影时只汇总已授权的 children：`let counts=summarize(children.filter(|c|authz.can_read_task(actor,c.device_id)));`，`can_read_task` 包含有效管理员全局读取分支。
- [ ] **Step 4 确认通过：** 目标 PASS；管理员跨用户取消、普通用户跨用户拒绝/本人撤销 reboot 后不得取消、无读取权限取消响应不泄漏、取消与 dispatching 竞争不误释放锁分别红绿；SSE撤权旧消息/时间事件仅管理员/队列溢出 reset、幂等重放、time-review 无 held 返回400且批量原子、held/unknown 管理分别先见各自 RED 再实现/跑绿；`cargo test -p rsetup-controller api::` 全绿。
- [ ] **Step 5 重构、回归、提交：** 整理投影边界并复跑；`git add crates/rsetup-controller/src/api crates/rsetup-controller/src/lib.rs crates/rsetup-controller/src/audit.rs && git commit -m 'feat(controller): expose authorized tasks'`。

### Task 6: 旧备份受控恢复与两数据库故障演练

**Files:** Create `src/operations/{mod.rs,backup.rs,mode.rs,retention.rs}`, `tests/runtime_db.rs`；Modify `src/{main.rs,api/system.rs,audit.rs}`, `src/tasks/recovery.rs`, `tests/controller_recovery.rs`、`.github/workflows/ci.yml`（扩展已批准的双库 URL 门控）；此前创建的 `tests/task_db.rs` 保持原测试契约。

**Interfaces:** `RecoveryMode::enter(restored_checkpoint)` 撤旧会话并暂停自动准入/变更且审计进入；`RecoveryMode::exit(admin,evidence)` 核对身份密钥、授权/吊销、密码和板端日志并审计退出。辨别旧备份的显式方式须先获批准，不能从墙钟猜测。

- [ ] **Step 1 RED：** `RecoveryFixture::from_old_backup()` 使用 `required_test_db()` 在隔离库恢复真实持久状态，board 可 fake；`runtime_db.rs` 另写真实 DB 断连/迁移/锁/CAS/备份恢复及保留期用例，全部 `#[ignore]`。先分别 RED 验证终态任务30天、审计180天、幂等索引至少24h（均为 05 §8 待审建议）：未决/held/unknown 持锁记录不可清理，幂等索引不可早于关联任务清理；可控时间证据与隔离库使失败来自错误清理而非真实等待。若另设无需 DB 的模式转换纯单测，应明确使用 fake fixture 且不调用 `required_test_db()`，不能充当真实恢复/两库验证。
```rust
#[tokio::test]
#[ignore] // 旧备份真实 DB 演练：需显式 URL + --ignored
async fn old_backup_never_replays_queued() {
    let f=RecoveryFixture::from_old_backup().await;
    f.restart_controller().await;
    assert!(f.in_controlled_recovery().await);
    assert_eq!(f.execute_count(),0);
    assert!(!f.can_admit_or_mutate(device_a()).await);
}
```
- [ ] **Step 2 确认失败：** 旧备份 DB fixture/可编译 stub 就绪后，分别以 `CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL"` / `CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL"` 前缀运行 `cargo test -p rsetup-controller --test controller_recovery old_backup_never_replays_queued -- --ignored --exact --test-threads=1`；`runtime_db.rs` 的断连/迁移/锁/CAS/恢复/保留期用例同样逐引擎以显式 URL、`--test runtime_db -- --ignored --test-threads=1` 执行 RED。每引擎逐目标核对实际执行非零并因行为缺失而失败；缺 URL 标未验证，零用例/连接失败不算 RED。
- [ ] **Step 3 最少 GREEN：** 分别安全备份 DB/中控私钥/配置，记录组合恢复点，板端私钥不集中备份。旧备份进入受控模式，撤旧会话、暂停新准入和变更；允许管理员显式授权单设备只读核实，核对吊销/授权、密码和板端任务日志，备份 queued 不自动执行，无法核实则 unknown/held。DB 不可用 ready503，拒绝不能持久的新变更但保持隧道保活；恢复查询补偿。按批准后的 05 §8 保留期做受控清理：终态任务建议30天、审计建议180天、幂等索引至少24h 且不得早于关联任务清理；未决、held、unknown 持锁与待核实记录不得按终态期限删，清理不得使重试变新任务或释放不确定设备锁。门禁示例 `if mode.is_controlled_recovery(){return Err(TaskError::RecoveryReviewRequired);}`。
- [ ] **Step 4 确认通过：** 目标 PASS；DB断连、旧撤权不能复活、恢复后核实、不重发、终态/审计/幂等索引保留期与未决保护分别红绿。真实 DB 回归按引擎执行：MySQL：`CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test -p rsetup-controller --test controller_recovery -- --ignored --test-threads=1` 与 `CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test -p rsetup-controller --test runtime_db -- --ignored --test-threads=1`；TiDB：对应两条命令改用 `CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL"`。另执行 Task 2 的 `task_db` 两引擎命令；逐引擎逐测试目标核对实际执行非零、零失败，分别验证迁移/锁/CAS、备份演练并记录版本/实际恢复点/耗时。最终运行 `cargo test --workspace --locked`、`make test`、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`；前两者默认跳过 `#[ignore]`，不能证明真实 DB 覆盖。缺某引擎 URL 标「未验证」而非 PASS，已配置 URL 却失败/零用例标「失败」。获批实施时在 `.github/workflows/ci.yml` 扩展 01 Task 6 既有按 URL 门控：新增 `task_db`、`controller_recovery`、`runtime_db` 的 MySQL/TiDB ignored 测试与逐目标非零计数；保留 01 的 `mysql_identity`/`tidb_identity`、缺 URL 不阻断普通 CI、URL 存在而失败/零用例使 CI fail、已批准版本/服务约束，不假设现成 DB 服务。
- [ ] **Step 5 重构、回归、提交：** 整理受控恢复状态并复测；`git add crates/rsetup-controller/src/operations crates/rsetup-controller/src/tasks/recovery.rs crates/rsetup-controller/src/main.rs crates/rsetup-controller/src/api/system.rs crates/rsetup-controller/src/audit.rs crates/rsetup-controller/tests .github/workflows/ci.yml && git commit -m 'feat(controller): gate backup recovery'`。

## 覆盖与完成判据

04 TASK-01/02→Task2，TASK-03..07→Tasks2/3/5/6；05 TIME-01..03→Task1，RUN-01→Task4，OPS-01→Task6；06 AT-05..10、AT-12、AT-13、AT-15 与 01/02/04 合约联合验收。AT-12 中控侧：Task 3 票据核实/Task 5 状态投影；01 计划 §验收映射所指 ACL-03（规格 01 §8：隐藏设备/计数与撤权后排队 SSE 不发送）由 01 的可见性/authz_epoch、本文 Task 5 服务端投影/SSE 及 04 前端共同验收，不将本文自引为依赖；AT-18 明确由本文 Task 5 的授权过滤/背压-reset/重连时持久 GET 投影，与 04 Task 4 的订阅缓冲/重取联验，不以 SSE 通知代替持久结果。AT-05..07 与 04 计划联合验收。1024设备/20浏览器验收记录硬件、报文、RTT、吞吐/延迟分位、峰值内存和拒绝计数。本文是待审阅执行路径，未运行未来功能、两数据库或容量测试，不解除协议生产发布阻断。

