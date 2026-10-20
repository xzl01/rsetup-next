# Controller V1 四线 Task 4 跨线接口与文件所有权收敛清单

状态：**待指定模型复审，不授权实施**（执行计划配套，非正式复审通过，亦非新架构批准）。

## 当前复审事实（仅文档状态，不授权实施）

- 本清单历史已获限定接口静态批准（79360239…）；旧审计/共享gate/readonly等历史切片批准保留，不代表四完整计划或真实验收。
- 当前四线正式状态仍为 0/4 APPROVED，完整 Identity/Runtime/Transport/Web 最近的正式全量审查结论均为 REJECTED（各线正针对新一轮技术阻断进行内聚收敛：Identity ID-R6、Runtime RT-N、Transport TR-R6、Web WEB-R5，未获正式批准、未授权实施、未实现，当前仍处于执行计划收敛阶段）；历史审查记录（代理 acf5ea00…/bd5299c9…/0e917b40…/8aeb551c…）仅作过程记录，不能静默以旧gemini/flash路由当本轮。
- 当前Runtime普通业务helper+两局部tests(20bf52d6…)、Runtime六Board契约(93cc103d…)、Transport六Board消费契约(9d16c3f7…)均仅限定静态APPROVED，无测试/接线证据，四完整未批准；签名固定§3原样。
- EndpointId裁决新增14读变体=10管理+3task+独立DeviceStatusRead，Me/Sessions保留；status已Runtimehandler/Identity桶，不计26/8；Identity全文传播/生产实现仍待，不能说已写代码。
- Identity内部snapshot仍待原任务回调/正式审查；ManagementState/probes/GW0/v4/DB备份/专家+用户+codec/真实浏览器wire门均未完成；集成收敛稿正式CHANGES_REQUIRED3Important1Minor，部分后续修订待复审。
- 本次仅事实标签同步，修改稿待指定模型限定静态复审，不改固定裁决/任何签名/DTO或用户批准字段，无实施/merge/push许可。
日期：2026-10-16。范围：四份执行计划（identity/runtime/transport/web）+ 联合设计 spec `2026-10-07-controller-v1-four-task4-design.md` + 权威规格 00–05。
代码佐证（worktree `controller-v1-four-task4-design`）：`auth/sqlx_repo.rs:105-110`（AUTH_PROCESS_EPOCH/SEQ 及"与 db.rs 完全独立"注释）、`db.rs:2681-2682`（独立 PROCESS_EPOCH/SEQ）、`http_auth.rs:106`（EndpointId 现仅 Me/Sessions；读桶键 (EndpointId,digest) 50/s、写桶键 session digest 20/s）、`http_auth.rs:239-242`（AppState 仅 db/auth 两字段，无法注入 probe）、`error.rs:1-2`（ControllerError 仅派生 Debug/thiserror::Error）、`lib.rs:42`（build_router 仅挂 /healthz、/readyz）、`main.rs`（当前只挂探活 build_router，生产装配未就位）。

## 1. 固定裁决（高于各草案）

1. Transport 是 `rsetup_protocol::wire` 共享业务 DTO 唯一生产者：Capabilities/DeviceStatus/ClockSample/RebootTicket/TaskRecord（含 Metric/Query/RebootPrepare/RebootExecute/TaskQuery）；**DTO ≠ 获批 Protobuf codec 或 512KiB wire**，raw 字节入口失败关闭。
2. Runtime 是 `tasks/board_client.rs` BoardClient 唯一消费者；其余线不得直接构造板端请求。
3. Identity owns `http_gate` 共享边界：单值 Host、严格 Cookie、权威 session、Origin/CSRF、1 MiB（MAX_BODY_BYTES=1_048_576）。EndpointId 按每个 GET 路由模板隔离：10 个管理 GET 模板明确为 UsersRead/RolesRead/GrantsRead/EffectivePermissionsRead/GroupsRead/DevicesRead/DeviceDetailRead/ApprovalsRead/AuditRead/SystemStatusRead，任务读新增 TaskList/TaskDetail/TaskChildren 三变体，另设 1 个独立 DeviceStatusRead 读桶（`GET /devices/{id}/status` 已正式定责 Runtime 路由/handler 读取缓存快照，Identity 负责 EndpointId::DeviceStatusRead）；Me/Sessions 原样保留；写端点一律走既有 session 写桶（按 session digest），不设每端点写变体；DeviceStatusRead 不并入既有模板，不混入 Identity 26 路由计数，也不计入 8 条任务路由。
4. 既有 `auth/sqlx_repo.rs` AUTH_PROCESS_EPOCH/SEQ 与 `db.rs` 独立 PROCESS_EPOCH/SEQ **保留**，不作全 crate 统一重写；新 `audit.rs` 仅服务新管理/任务写者，独立 ADMIN_PROCESS_EPOCH/SEQ，第三独立命名空间（唯一键 (process_epoch,event_seq)，epoch 不同不自动冲突）。
5. Runtime owns v4 guard/版本化；db.rs/integrity.rs/audit.rs/lib.rs 导出等共享改动按 **Identity gate → newadmin audit → Runtime v4** 串行，不并发写。
6. lib/main 顶层生产组装归**唯一独立集成任务所有者**；各线仅提供组件子 router：Identity `build_management_router(state: ManagementState) -> Router<()>`、Runtime `build_tasks_router(state: HttpTasksState) -> Router<()>`；**不用 `runtime_task_router` 命名**（避免 shadow），各线不得自设顶层合并入口。`ManagementState`（待审目标接口，非现有代码；下列字段均为待审目标字段，非已实现）：

   ```rust
   pub struct ManagementState {
       pub auth: Arc<HttpAuthState>,
       pub users: Arc<dyn UsersGateway>,
       pub roles: Arc<dyn RolesGateway>,
       pub grants: Arc<dyn GrantsGateway>,
       pub groups: Arc<dyn GroupsGateway>,
       pub devices: Arc<dyn DevicesGateway>,
       pub approvals: Arc<dyn ApprovalsGateway>,
       pub audit: Arc<dyn AuditGateway>,
       pub system: Arc<dyn SystemGateway>,
       pub device_probe: Arc<dyn DeviceLiveProbe>,
       pub recovery_probe: Arc<dyn RecoveryModeProbe>,
       pub notification_sink: Arc<dyn ManagementNotificationSink>,
   }
   ```

   `build_management_router(state: ManagementState) -> Router<()>` 消费该 state；`notification_sink` 为 typed 管理通知 sink，真实 typed 通知源需集成时提供，不能恒成功。既有封闭 `Arc<AppState>`（http_auth.rs:239-242）只有 db/auth，不能注入 probe，故引入 `ManagementState`，**不改现有 AppState 本体**以免 Auth 受扰；SQLx 实际 production builder 由唯一集成 owner 构造这些，各线待传播、不得声称已实现。
7. `DeviceLiveProbe`（设备详情）源为 Transport typed 真实缓存（真实连接/快照）、`RecoveryModeProbe`（system/status）源为 Runtime typed 真实控制器；**出错不返回常量 offline/空 capability**；缺接线不挂载，且整体 Task 4 未完成（不得宣称完整）。

## 2. 所有权表

| 文件 / 接口 | 所有者 | 角色 | 备注 |
| --- | --- | --- | --- |
| `rsetup-protocol/src/wire.rs` 业务 DTO | Transport | 生产 | 与 03 §2 逐字段对应；Runtime/Identity 不得另定义影子 DTO |
| `tasks/board_client.rs` BoardClient | Runtime | 消费 | 签名按 §3（待审）；仅当前纯组件切片暂用 fake，真实网络不在本切片内；完整目标/本地 merge 仍须获批 codec/crypto 及真实认证双流互通，验收不缩减 |
| `http_gate.rs`、`http_auth.rs` EndpointId | Identity | 生产（共享） | 10 管理 GET + 3 任务读变体 + 1 DeviceStatusRead 由 Identity 一次性加入（原 Me/Sessions 保留） |
| `audit.rs`（新建） | Identity | 生产 | ADMIN_PROCESS_EPOCH/SEQ，11 字段，time_evidence 非空 |
| `auth/sqlx_repo.rs`、`db.rs` 既有 epoch 静态 | Identity / Runtime | 保留 | 不动两个既有命名空间 |
| `integrity.rs` 版本化 guard | Runtime（v4） | 生产 | Identity gate 先行，Runtime v4 随后改写；升级后管理写自动受护 |
| `lib.rs` / `main.rs` 顶层装配 | 集成任务（独立） | 生产 | 合并三子 router；不重复注册 /healthz、/readyz |
| 8 条任务路由 `http_tasks/`（目录形式） | Runtime | 生产 | 不另建单文件 `http_tasks.rs` |
| 26 条管理路由 `api/` | Identity | 生产 | `GET /api/v1/system/status` 单一归属 Identity，Runtime 不重复注册 |
| `GET /api/v1/devices/{id}/status` | Runtime（路由/handler）+ Identity（EndpointId::DeviceStatusRead） | 生产（路由/EndpointId） | 已定责待实施：Runtime 读取缓存快照（GET 不得触发执行/采集）；Identity 负责独立 DeviceStatusRead 读桶；endpoint 已由权威 02 定义，不新增业务 permission，仍 device.status.read；不混入 Identity 26 路由计数，也不计入 8 条任务路由；暂无快照真实源则不常量伪 PASS，目标整验收未完成；未接线禁止宣称完整，不擅改权威 02 |
| Web `task-requests.ts` 8 路径白名单 | Web | 消费 | 形状以 Runtime Task 6 DTO 为准 |

## 3. 精确生产者/消费者接口

- **Transport → Runtime（wire DTO，`rsetup_protocol::wire`）**
  - 常量：`BUSINESS_SCHEMA_VERSION=1`；词表 TASK_STATES(5)、TASK_REASON_CODES(12)、EVIDENCE_KINDS、METRIC_QUALITIES、CLOCK_QUALITIES；入站白名单 6 项（device.task.result 是板→中控事件，不入入站列表）。
  - 结构（03 §2 逐字段一一对应、完整；**所有顶层消息 `schema_version: u32` 固定 1**，Metric 非顶层无此字段；UUID 为标准字符串）：
    - Query{schema_version:u32}；Capabilities{schema_version:u32,boot_id:String,agent_version:String,supported_actions:Vec<String>,durable_task_journal:bool,max_status_bytes:u32,journal_epoch:String}；
    - Metric{key:String,value:Option<f64>,unit:String,quality:String}（仅 PartialEq，**不派生 Eq**）；
    - DeviceStatus{schema_version:u32,boot_id:String,agent_epoch:String,sample_seq:u64,sampled_unix_ms:i64,sampled_monotonic_ns:u64,clock_epoch:String,metrics:Vec<Metric>,clock_quality:String}（仅 PartialEq）；
    - ClockSample{schema_version:u32,boot_id:String,clock_epoch:String,receive_unix_ms:i64,send_unix_ms:i64,receive_monotonic_ns:u64,send_monotonic_ns:u64,quality:String}（boot_id/clock_epoch 不可漏）；
    - RebootPrepare{schema_version:u32,sub_task_id:String,expected_boot_id:String}；RebootTicket{schema_version:u32,sub_task_id:String,boot_id:String,execution_token:[u8;32]（32B，03 §4）,valid_for_ms:u32}；
    - RebootExecute{schema_version:u32,sub_task_id:String,expected_boot_id:String,execution_token:[u8;32]}；TaskQuery{schema_version:u32,sub_task_id:String}；
    - TaskRecord{schema_version:u32,sub_task_id:String,journal_epoch:String,record_version:u64,state:String,pre_boot_id:String,observed_boot_id:Option<String>,reason_code:Option<String>,updated_unix_ms:Option<i64>,evidence_kind:String}。
  - 证据完整门禁区分两种“缺失”：**“类型字段缺失”**（Rust 结构缺 03 §2 任一字段 = 影子 DTO，Runtime 标 BLOCKED）与**“消息 optional 值 None”**（proto3 optional 字段缺省，Option=None 合法——合法 accepted/attempted 可 observed_boot_id/reason_code/updated_unix_ms 为 None）；RebootTicket 必需字段 boot_id/valid_for_ms 不得缺；evidence_kind 为 proto3 非 optional string，若映射 Option 必须说明兼容语义（空串 vs None），不擅改 wire 定义。
  - succeeded 判定非“字段存在=成功”，须同时满足 04 §6：同一认证设备+同一 sub_task_id+同一 journal_epoch 持久记录；板端可靠 attempted 且 pre_boot_id 与中控发送前 boot 一致；当前认证连接查询到的 boot_id≠pre_boot_id 且==observed_boot_id；evidence_kind=boot_transition_observed、版本合法、无已确认矛盾失败记录。
- **Runtime BoardClient（唯一消费者；以下均为待审目标接口，非现有代码）**
  - dyn-safe 形式（待审统一目标契约，非现有代码；不用 `async fn` trait 形式，其与 dyn 对象安全不兼容）：

    ```rust
    use std::future::Future;
    use std::pin::Pin;

    pub type BoardFuture<'a, T> =
        Pin<Box<dyn Future<Output = Result<T, BoardClientError>> + Send + 'a>>;

    pub trait BoardClient: Send + Sync {
        fn capabilities<'a>(&'a self, device: &'a [u8; 32]) -> BoardFuture<'a, Capabilities>;
        fn status<'a>(&'a self, device: &'a [u8; 32]) -> BoardFuture<'a, DeviceStatus>;
        fn clock<'a>(&'a self, device: &'a [u8; 32]) -> BoardFuture<'a, ClockSample>;
        fn prepare<'a>(&'a self, device: &'a [u8; 32], sub_task_id: Uuid, expected_boot_id: &'a str) -> BoardFuture<'a, RebootTicket>;
        fn execute<'a>(&'a self, device: &'a [u8; 32], ticket: &'a RebootTicket, request: &'a RebootExecute) -> BoardFuture<'a, TaskRecord>;
        fn task_record<'a>(&'a self, device: &'a [u8; 32], sub_task_id: Uuid) -> BoardFuture<'a, Option<TaskRecord>>;
    }
    ```

  - 6 方法为待审统一目标契约（即上列签名）：`prepare` 取 `sub_task_id: Uuid` 与 `expected_boot_id: &'a str`；`execute` 取 `ticket: &'a RebootTicket` 与 `request: &'a RebootExecute`（至多一次，须在 dispatching 意图提交后，实现层核对 ticket.sub_task_id==request.sub_task_id）；`task_record` 取 `sub_task_id: Uuid` 并返回 `Option<TaskRecord>`（TASK_NOT_FOUND(1006)→Ok(None)）。
  - `task_query(&TaskQuery)`、`prepare_request()` 为 Transport 扩展建议，标待审，不属 6 方法待审统一目标契约；不让 Transport 提案未经审查压过 Runtime 接口。
  - `BoardClientError` 为 typed 枚举、显式字段（待审目标，非现有代码），取代泛化 ControllerError；保留 typed 稳定状态码（03 §1 status_code 词表）与固定去敏，不得原样携带 driver 日志：

    ```rust
    pub enum BoardClientError {
        DeviceOffline,
        Rejected { status: i32, reason_code: Option<String> },
        ProtocolMismatch,
        CapabilityUnavailable,
        DeviceBusy,
        ResourceExhausted,
        Timeout,
        Transport,
    }
    ```

    `Rejected.reason_code` 严格按 03/04 稳定词表校验（合法值以 03/04 明列/引用即可），不存原 driver 字符串、不自造 `DiagCode` 类型；`ProtocolMismatch`/`CapabilityUnavailable`/`Transport` 均为 unit，`DeviceOffline`/`DeviceBusy`/`ResourceExhausted`/`Timeout` 原 unit 保留，不新增诊断参数，固定非敏感诊断仅错误 variant 承载，无需新依赖。不照抄 Transport 草案 `#[derive(Clone,PartialEq,Eq)]` 派生与 `Controller(#[from] ControllerError)` 变体——实际 `ControllerError`（error.rs）仅派生 Debug/thiserror，照抄派生 Eq 会编译失败，typedError 不混 ControllerError 派生 Eq。
    可见性与导入（适用本文全部代码块）：对 main/外部集成消费的 `ManagementState`/`BoardFuture`/`BoardClient`/`BoardClientError` 公开 `pub`，`ManagementState` 字段 pub；同 crate 使用的 http_gate 辅助仅 `pub(crate)`，不扩大现有 Auth 其它内部接口；代码片段所需 `Arc`/`Uuid`/DTO 从对应模块导入，不声称独立编译或测试。
  - 按阶段错误语义：prepare 失败（尚未 dispatching 意图/execute）可 failed（对应具体 reason_code）并释放自己锁；dispatching 意图已 commit 后 execute Timeout/Transport 不能 failed 或重发，转 verifying；verifying 阶段保持 verifying 至窗口到期才转 unknown，查询（task.get）超时不推进出 verifying；unknown 阶段低频 query 超时保持 unknown，不重占执行槽、不回任何执行中状态；仅合格成功或明确失败证据收敛 unknown；不一律宣称每阶段都转 verifying。
- **Identity → 全线（共享 HTTP 边界，`pub(crate)`）**
  - 共享 gate 精确签名（待审统一目标签名，非现有代码；此文仍待审，不是执行授权）：

    ```rust
    pub(crate) async fn authenticate_read(state: &HttpAuthState, headers: &HeaderMap, endpoint: EndpointId)
        -> Result<Session, Response>;
    pub(crate) async fn authenticate_write(state: &HttpAuthState, headers: &HeaderMap)
        -> Result<Session, Response>;
    pub(crate) async fn read_json_body<T: DeserializeOwned>(headers: &HeaderMap, body: Body)
        -> Result<T, Response>;
    pub(crate) fn require_admin_session(session: &Session) -> Result<(), Response>;
    ```

    固定错误响应（401/403/503/429 含 message_key）；MAX_BODY_BYTES=1 MiB。
  - 限速：读按 (EndpointId, session digest) 50/s 且按 GET 模板隔离；写按 session digest 20/s。
  - `build_management_router(state: ManagementState) -> Router<()>`：26 条管理路由（含 GET /system/status）；DB 错误 503，不返回常量空结果。`ManagementState` 结构、不改 `AppState` 本体与 production builder 归属见固定裁决 #6（Identity 草案现签名仍为 `Arc<AppState>`，各线收敛待传播，不得声称已实现）。
  - `audit::insert_audit(&mut Transaction<MySql>, &AuditInsert)`：ADMIN 命名空间；11 字段、time_evidence 非空、event_seq 单调递增。
- **Runtime → Identity/集成（任务 HTTP）**
  - `build_tasks_router(state: HttpTasksState) -> Router<()>`；`HttpTasksState{ api: Arc<dyn TaskApi>, auth: Arc<HttpAuthState> }`。
  - `TaskApi`（preview/submit/list_tasks/task_detail/task_children/cancel/time_review/release_lock），对象安全（Runtime 草案 `TaskFuture<'a,T>` BoxFuture 形式，待审）。
  - DTO 按 02 §1 响应投影与 04（替换历史 Runtime 错误形状；均为待审目标接口）：MainTask 投影（列表项与详情 TaskDetail 同形）：id、operation、state（4 态 queued/running/held/completed）、outcome（5 词 success/cancelled/unknown/partial/failed，非 completed 为 null）、revision（十进制 String）、created_time（已有 `time::evidence::TimeEvidence`，非裸 RFC3339 字符串）、view_scope、counts（`Record<SubTaskState,string>`：10 个固定 key 全部存在，零值序列化为 "0"，值按 02Counter 为 unsigned u64 十进制字符串；拒绝负值/前导零/溢出/JSON number，不擅用 usize 自动序列化为 JSON number；当前授权全集汇总非分页、无 hidden counts；示例 `{"queued":"0","succeeded":"1"}` 仅为局部示意，实际响应为完整十 key）、actions:{can_cancel:boolean}（本次用户批准的新增响应字段，计算规则与批准范围见 §5）。
  - SubTask 投影（ChildItem）：id、device_id、state、reason_code、revision、result_evidence（授权投影；保留证据但不暴露 execution_token 或其 token 摘要）；分页统一 `Page{items,next_cursor}`（02 §1 公共类型），main counts 独立重算，不借分页 children 判断全集。
  - 响应字面量：创建及合法重放仅 `{task_id,accepted:true}`；release-lock 仅 `{released:true,state:"unknown"}`（删除旧 ReleaseOutcome 影子名称）；time-review 响应严格 02 之“当前视图”、cancel 响应严格 02 之“已授权目标的取消请求接收确认及可读取结果”（删除旧 TimeReviewOutcome 影子名称）。
  - 投影：`project_main_task_summary`（主详情，counts 按当前授权全集重算）与 `project_task_children`（分页当前页）各自独立；不得以 children 替代 main counts，不得输出 total_targets/hidden_count。
  - `RecoveryModeProbe`/`DeviceLiveProbe` 适配器：Runtime/Transport 提供 typed 真实源（真实缓存），Identity 挂载入 `ManagementState`；缺接线不挂载。
- **Web → Runtime（消费）**：8 路径白名单（无 GET /task-previews）；仅 POST /tasks 带 Idempotency-Key 头；响应形状按 Runtime DTO；`authorized_subset` 消费 view_scope/counts/items。

## 4. 串行顺序

1. 阶段 A（四线并行，离线基础，不写共享文件）：Identity 仓储接口/LastAdmin 规则；Runtime NTP 门禁+任务模型；Transport 板端 crate+DTO；Web 纯组件+幂等交互。
2. wire DTO 门禁：Transport Task 1 先行落地 wire DTO → 解除 Runtime BoardClient 阻塞（否则 board_client/evidence 保持 BLOCKED）。
3. 共享文件串行（不并发写）：Identity gate（http_gate 抽取、管理路径按 v3）→ newadmin audit（audit.rs，ADMIN 命名空间）→ Runtime v4（integrity.rs 版本化 guard + 0004_tasks.sql；迁移须真实备份/隔离/独占/停写门禁，禁止 DROP DATABASE、仅表级建改删）。
4. 阶段 B/C：Identity SQLx 真实读写与管理网关；Runtime v4 五表、预览/提交/锁、调度与只读恢复；Transport journal 两阶段 fsync+票据/OS 边界。
5. 阶段 D（集成任务）：Web 预览/提交/详情/取消/time-review/release 与 a11y；lib/main 顶层挂载（auth_router ⊕ management_router ⊕ tasks_router，路径集互斥）。
6. 全部 Task 4 验收 + 整分支复审 → 本地 merge `dev-aghost`（**无 push**）。

## 5. 数据契约（要点）

- 主任务状态仅 queued/running/held/completed；子任务 10 态（queued/held/dispatching/accepted/verifying/succeeded/failed/unknown/cancelled/expired）。
- `POST /tasks` 202 与合法重放仅响应 `{task_id, accepted:true}`，不泄读投影。
- `POST /subtasks/{id}/release-lock` 仅响应 `{released:true, state:"unknown"}`（无 revision/lock_released）。
- time-review：expected_revision + decision（`resume_original_deadline`|`expire`）+ evidence；对该主任务全部 held 子任务原子生效，无 held 400，绝不延长期限。
- cancel：expected_revision + confirm；仅对合格 queued/held CAS 生效（running 主任务同样可能含这些目标）；dispatching 后不伪称取消。
- main counts 是当前授权全集摘要（普通用户按可见子集重算并标 authorized_subset），**不是分页 children**；counts 类型与 §3 MainTask DTO 一致：`Record<SubTaskState,string>`，10 个固定 key 全部存在（零值 "0"），unsigned u64 十进制字符串（拒绝负值/前导零/溢出/JSON number，不擅用 usize 自动序列化为 JSON number），授权全集汇总非分页、无 hidden counts；created_time 为 TimeEvidence（05 §3）；当前权限与操作呈现分开：`actions` 由"仅待审扩展建议"更新为**本次用户批准**——本会话用户通过 `ask_user_question` 的 `approve-task-cancel-action` 问题明确选"批准该布尔字段（推荐）"，批准新增响应字段 `actions:{can_cancel:boolean}`；批准来源记为"本会话用户对 approve-task-cancel-action 明确批准"（仅为接口批准，非完整计划/实施通过，不伪造外部签字/测试结果）；`can_cancel` 由后端按当前身份、device.reboot 及合格 queued/held 目标计算，只表示可请求取消而非一定成功，不提供隐藏数量、不赋予 task.read；前端只据此显示 cancel，缺字段时关闭入口；POST 仍期望 revision/confirm，事务重验及 CAS，running 主任务也可能有合格 targets；其他 actions 字段（如 can_time_review）不顺批、仍属待审扩展建议；缺 actions 必须关闭危险控件，不借分页或 task.read 判断 reboot。
- 提交幂等：actor+key 唯一；actor/key/request_hash/token_hash 查重；同内容重放先命中持久幂等记录返回原任务（允许 preview 已过期/已消费/已删除）；不同内容 409；首次提交才消费 preview；preview 60s（单调）且随进程代际失效。
- 调度/恢复：DB intent 提交后 execute 至多一次（当前进程单协程）；恢复对 dispatching/accepted/verifying 仅 task.get 只读，不重发；unknown 释放全局槽但保设备锁；迟到有效证据（查询/事件）更新旧任务 unknown 及主 outcome/counts、递增 revision，独立于当前锁 generation；generation 只限制锁释放（device_id+owner_sub_task_id+generation）——锁已释放/新任务持锁仍可收敛旧任务，但不删新锁、不重执行。
- 板端边界：去重命名空间 (controller_key, sub_task_id) 联合键；认证证据；journal_epoch 稳定（仅丢失/损坏更换并报 JOURNAL_LOST）；pre_boot_id 在 accepted 落盘；fsync 两阶段 临时文件→文件 sync_all→rename→父目录 sync_all，rename 前确定失败与 commit-unknown 分开分类；B04 OS 调用边界：accepted/attempted 未可靠提交不调用 OS；OS 已调用且报错后，仅 finish failed/OS_ERROR 可靠持久才以 status=0 回复该记录；finish 为 DefinitelyNotCommitted 或 CommitUnknown 均断流且无业务回包，不伪 failed/unknown、不以非零暗示未执行（DefinitelyNotCommitted 情形 OS fake 计数 1 而宿主真实 reboot 0）；绝不采纳宽泛"所有 OS 明确错误均无回包"（可靠 failed 仍可 status=0）；raw 未获 codec approval 保持失败关闭；测试中 OS 真 reboot 次数恒 0。
- 过程规则：commit 信息中文（type/scope 可英文、描述中文）；TDD RED 必须可编译且实际断言失败（非 0 用例、非 ignored、非依赖失败）。

## 6. 验收门禁（分层证据）

- ① 纯逻辑/组件 RED→GREEN（可编译行为）；② 进程内 HTTP 权限矩阵、文件持久化故障注入、模拟网络/时钟；③ 双引擎真实 SQLx、并发/CAS、跨进程崩溃恢复（缺真实备份/隔离时 ignored 测试保持未运行，不以 return 冒充通过）；④ codec 专家书面决议 + 用户复审 + codec 获批后，验证 512KiB Protobuf 信封、真实认证/双流网络互通与受控浏览器端到端。
- 当前未通过：真实 DB 隔离备份停写（禁止 DROP DATABASE，仅表级建改删）/ 应用账号锁事务 / 浏览器 / wire 集成。**备份相关历史 artifacts 存在但本轮未读未验**，不伪备份 ACK——不可写"备份不存在"，也不可写"已验证"。codec 专家书面 decision 未找到、用户审批未补，不伪专家报告 → raw 字节入口必须失败关闭。测试中 OS 真实宿主 reboot 次数恒 0（不得以模拟计数冒充）。
- 全部门禁（含 fmt/test/clippy、Web 全套、凭据与提交范围检查）满足后才本地 merge dev-aghost，无 push。

## 7. 历史矛盾与待裁决列表（原快照保留；当前状态以上方为准）

1. 审计 epoch：历史草案冲突（Identity 草案 Task 4 曾要求"统一为单进程 epoch + 全局单序列"，与固定裁决冲突）已在 Identity 写入计划切片修订（只读 grep 核实：该切片现用 `crate::audit::admin_process_epoch()` 独立 `ADMIN_PROCESS_EPOCH`/`ADMIN_EVENT_SEQ` 命名空间，既有两个写者 epoch 原样保留，与固定裁决一致）；该切片已获最新指定 gemini（fc9bbd5a…）的**限定 APPROVED**（仅审计 namespace：6 处文本静态符合、无测试；旧 CHANGES_REQUIRED 仅作历史记录，碰撞措辞/精确断言由另代理修）；完整 Identity 仍 NOT APPROVED（Host 业务 spy/锁序/生产/audit_page u64/Task 14 空测试），不得声称整计划/真实审计已通过；`audit_page` 单 `u64` 游标分页（cursor 现仅 `event_seq`）尚待修。→ 固定裁决（保留既有两命名空间 + 新独立 ADMIN 第三命名空间）仍为准，切片偏差须登记。
2. EndpointId：8 个任务变体（TaskPreview/TaskCreate/TaskList/TaskDetail/TaskChildren/TaskCancel/TaskReview/TaskRelease）源出 Runtime 执行计划（1880/1961 行）；Identity 草案仅加单 ManagementRead 变体 vs 裁决（10 管理 GET 模板 + 3 任务读 TaskList/TaskDetail/TaskChildren，写走 session 桶，Me/Sessions 保留）→ 以本清单为准。
3. router 命名：Identity Task 13 草案要求 `runtime_task_router(RuntimeApiState)` vs 裁决 `build_tasks_router(HttpTasksState)->Router<()>` 且不用 runtime_task_router → 以本清单为准。
4. BoardClient 签名：Runtime 草案（Result<_,ControllerError>、execute 携带 subtask_id）vs Transport 提案（typed BoardClientError、execute(ticket,request)、task_record→Option）→ 裁决取 §3 之 6 方法待审统一目标契约与 dyn-safe 签名（待审）；task_query/prepare_request 标待审扩展，Transport 提案不单方压过 Runtime 接口；Runtime 草案需修订。
5. TaskRecord 双份定义：wire DTO state 为 String，Transport 草案板端 crate 另定义 typed TaskState → wire 层唯一权威，板端为映射层，边界须标注。
6. `GET /api/v1/devices/{id}/status` **已定责待实施**（原未分配：Identity 26 路由不含、Runtime Task 6 未列、Web 详情依赖）→ 正式定责 Runtime（设备状态快照消费者）负责路由/handler 读取缓存快照（GET 不得触发执行/采集）；Identity 负责 EndpointId::DeviceStatusRead 独立读桶；endpoint 已由权威 02 定义，不新增业务 permission，仍 device.status.read；不混入 Identity 26 路由计数，该额外读取路由不计入 8 条任务路由；暂无快照真实源则不常量伪 PASS，目标整验收未完成；未接线禁止宣称完整，不擅改权威 02。
7. 评审状态：formal 三线与 Web 本会话均 **CHANGES_REQUIRED**；codex-proxy/gemini-3.8-flash-high 复审结论亦 CHANGES_REQUIRED（本清单已按该反馈修订，仍非批准）→ 均不能称批准。
8. 已知 Runtime 错误：Dispatching→Cancelled（04 合法转移不含）；响应 `main_task_id` 命名（02 创建响应为 task_id）；release 响应多出 `lock_released` 字段。
9. 已知 Transport 错误：浮点 Eq（含 f64 结构不得派生 Eq）；finishCommitUnknown 路径 OS 调用次数断言。
10. 已知 Web 错误：主 10 态呈现、race、a11y（dialog/焦点/Esc）待修。
11. 其余待裁决：`tokio test-util` 特性（workspace 层 vs controller dev-dependencies，Runtime 建议后者）；reason_code 以 Runtime Task 2 为 Rust 侧唯一权威、transport 板端白名单须同步；task.read 简写统一映射 device.task.read，严禁新增独立权限点；NTP fake 应答不得推广为真实网络结论。

## 8. 历史自查与回执（原轮次记录，不代表本次结果）

- 自查：本文件 ≤220 行、简体中文、状态"待指定模型复审，不授权实施"；本轮为唯一极小文档修订（仅 2 项：`actions:{can_cancel:boolean}` 用户批准落地并同步 MainTask DTO 投影、`BoardClientError` 去掉未定义 DiagCode/ReasonCode 占位并收敛为无诊断参数的最小 typed enum），仅修订本文件一份，未改四草案/代码/其它文件，未新建报告，未跑测试/联网/DB/secret，未提交/合并/push；固定裁决、四线精确接口、串行顺序、数据契约、门禁、矛盾均有对应条目，待审目标字段均已显式标注，无隐性占位符；上一轮跨线收尾修订（ManagementState 显式字段 code block、BoardClient 6 方法 dyn-safe 完整签名、共享 gate 精确签名、矛盾 #1 审计 epoch 状态更新及"官方契约"措辞清理）已体现于本文件。
- 回执：实际产出即本清单（无另报告文件）；本轮为唯一极小文档修订，非正式批准。未决门禁：指定模型复审、codec 专家书面 decision、双引擎真实 DB 备份/隔离/停写门禁、真实浏览器 a11y、wire 真实互通、整分支复审、矛盾 #6 已定责待实施（真接线未通过）、Identity 写入计划切片已获限定 APPROVED（最新指定 gemini fc9bbd5a…，仅审计 namespace，6 处文本静态符合、无测试；旧 CHANGES_REQUIRED 仅作历史记录），完整 Identity 仍 NOT APPROVED（Host 业务 spy/锁序/生产/audit_page u64/Task 14 空测试，不得声称整计划/真实审计已通过）及其碰撞措辞/精确断言、`audit_page` 单 `u64` 游标分页。本轮结束，不接新任务。

## 第 5 轮修复仲裁记录（2026-10-14）

1. **TaskRecord wire 形状唯一权威**：以本账本既有定义（§3 L65–74：state/pre_boot_id/observed_boot_id?/reason_code?/updated_unix_ms?/evidence_kind）为唯一权威 wire 形状；任何线不得新增 accepted/attempted/OS 结果等 wire 字段；Runtime 侧 wire DTO 门禁改为"按本账本逐字段核验形状 + 以 state/evidence_kind/reason_code 语义核验 attempted 与 OS 结果可靠性"，缺字段不再判影子 DTO。
2. **evidence_kind 兼容映射唯一化**：维持 wire proto3 非 optional string（L72）；Rust 侧如需 Option<String>，唯一兼容映射为：空串 ↔ None 双向一致、写侧缺省落空串、读侧空串归一为 None、词表校验在归一后进行；Transport 计划按此补唯一转换规则与契约测试。
3. **preview reason HTTP 词表**：以 Runtime Task 6 DTO 为准（04 ReasonCode，SCREAMING_SNAKE_CASE，或 null）；Web 侧消费词表、类型与 i18n 与其同步，废弃单线小写词表，禁止单线私造映射。
4. **签名分页 cursor 契约原则**：绑定用户/筛选/排序上下文、HMAC 签名防篡改（篡改返回 400）、防跨端点复用；Identity 管理分页与 Runtime children 分页各自按此原则落地，密钥注入方与字节布局由各线计划自行定义并写明，本账本不扩展 state 契约。
- 决议④补充（2026-10-14）：cursor 字节布局按各线 `cursor_kind` 双 profile 落地——UUID 键 profile（users/roles/grants/groups）24B payload + 8B tag = 32B → 64 小写 hex；设备公钥键 profile（devices/approvals）40B payload + 8B tag = 48B → 96 小写 hex（32B 公钥无法无损装入 24B，信息论约束）；两 profile 的 MAC/校验序/错误语义完全一致，密钥注入方仍为管理路由装配层；本账本不因此扩展 state 契约。
