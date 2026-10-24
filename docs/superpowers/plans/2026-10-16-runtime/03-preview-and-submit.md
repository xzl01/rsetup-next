### Task 4: 原子预览/提交、幂等与设备唯一操作锁 (Atomic Preview/Submit & Device Locks)

**Files:**
- Create: `crates/rsetup-controller/src/tasks/preview.rs`
- Create: `crates/rsetup-controller/src/tasks/repository.rs`
- Create: `crates/rsetup-controller/src/tasks/service.rs`
- Modify: `crates/rsetup-controller/src/tasks/mod.rs`
- 前置（本任务不修改，由共享 `error.rs` owner 先行落地）：`crates/rsetup-controller/src/error.rs` 添加 `TaskIdempotencyConflict`/`TaskPreviewInvalidState` 两个单位变体（见 Task 4 Step 4「B2 submit 错误边界」）
- Test: 内联于 `preview.rs`/`service.rs` 的单元测试 + 条件性 `crates/rsetup-controller/tests/task_db.rs`

**Interfaces:**
- Consumes: Task 2 `SubTaskState`/`ReasonCode`/`MainTaskState`；`auth::service::Session`；既有 `crate::parse_device_id`。
- Produces:
  - `pub fn validate_target_count(count: usize) -> Result<(), ControllerError>`（1..=1024，`0` 与 `>1024` 均 `InvalidArgument`）
  - `pub fn canonical_device_order(ids: &mut Vec<[u8; 32]>)`（升序字节序 + 去重）
  - `pub fn device_target_hash(device_ids: &[[u8; 32]], group_ids: &[Uuid]) -> [u8; 32]`（Sha256 over 长度前缀编码，规范顺序）
  - `pub fn request_hash(preview_token: &str, device_ids: &[[u8; 32]], group_ids: &[Uuid]) -> [u8; 32]`
  - `pub fn is_canonical_uuid_v4_key(key: &str) -> bool`（36 字符、小写 hex、`version == 4`、`variant ∈ {8,9,a,b}`）
  - `pub struct PreviewOutcome { pub preview_token: String, pub targets: Vec<PreviewTarget> }`、`pub struct PreviewTarget { pub device_id: [u8; 32], pub eligible: bool, pub reason_code: Option<ReasonCode> }`
  - `pub enum SubmitOutcome { Created { main_task_id: Uuid }, Replayed { main_task_id: Uuid } }`、`pub enum SubmitError { InvalidArgument, ActorInactive, DeviceBusyCount { busy: usize, total: usize }, PreviewExpired, PreviewConsumed, IdempotencyConflict, NotVisible, Backend(ControllerError) }`（`InvalidArgument`/`ActorInactive` 为 submit 请求期与事务内重验的短路变体，对应 Step 4 使用处；`Backend` 为存储/基础设施 `ControllerError` 透传变体，见下文「B2 submit 错误边界」）
  - `pub struct CancelRequest { pub subtask_id: Uuid, pub expected_revision: u64 }`、`pub enum CancelOutcome { Cancelled, AlreadyTerminal { state: SubTaskState }, NotCancellable { state: SubTaskState }, RevisionConflict }`
  - `pub struct TimeReviewRequest { pub main_task_id: Uuid, pub extend: bool, pub evidence_kind: TimeReviewEvidenceKind }`、`pub fn time_review_allows_extend(requested_extension_ms: i64, evidence_kind: TimeReviewEvidenceKind) -> bool`
  - `pub struct ReleaseRequest { pub subtask_id: Uuid, pub expected_revision: u64, pub acknowledge_unknown: bool, pub reason: String }`
  - `pub trait TaskRepository: Send + Sync`（完整异步方法签名，替换原空体；`SqlxTaskRepository` 为唯一生产实现；`MySqlPool` 为仓储内部句柄，`TaskService` 经 `begin()` 取短事务、不直接持有 pool；仓储同名方法与 Step 4 服务内联编排共用同一事务 helper 与 SQL，单一真相源）：

```rust
use chrono::{DateTime, Utc};
use sqlx::{mysql::{MySql, MySqlRow}, FromRow, Transaction};
// 依赖前置：crates/rsetup-controller/Cargo.toml 必须显式声明 sqlx = { workspace = true, features = ["derive"] }
// （workspace 根 sqlx 为 default-features = false 且不含 derive 特性），否则本 crate 的 #[derive(FromRow)] 编译失败。
// 领域行：纯业务结构，**不 derive FromRow**——sqlx-mysql 0.8.6 未对业务枚举
// （MainTaskState/MainTaskOutcome/SubTaskState/ReasonCode）或 [u8; 32] 导出 Type/FromRow，
// 在领域行上盲 derive 必致编译失败；MySQL 原始行由 MainTaskRawRow/SubTaskRawRow 承接，
// 经 checked 转换后才返回领域行（B1 实体行解耦，与 SubLockRow/PreviewSnapshotRow 先例同分层）。
#[derive(Debug, Clone)]
pub struct MainTaskRow { pub id: Uuid, pub actor_user_id: Uuid, pub state: MainTaskState, pub outcome: Option<MainTaskOutcome>, pub revision: u64, pub counts_json: String }
#[derive(Debug, Clone)]
pub struct SubTaskRow { pub id: Uuid, pub device_id: [u8; 32], pub state: SubTaskState, pub reason_code: Option<ReasonCode>, pub revision: u64 }
// MySQL 原始行：BINARY(16)/BINARY(32) 列以 Vec<u8> 承接、枚举列以 String 承接（同 IdempotencyRow/SubLockRow 口径），
// 不 truncate、不 unwrap/panic，损坏一律映射 ControllerError::Database（HTTP 503），绝不返回 400。
#[derive(Debug, Clone, FromRow)]
struct MainTaskRawRow { id: Vec<u8>, actor_user_id: Vec<u8>, state: String, outcome: Option<String>, revision: u64, counts_json: String }
#[derive(Debug, Clone, FromRow)]
struct SubTaskRawRow { id: Vec<u8>, device_id: Vec<u8>, state: String, reason_code: Option<String>, revision: u64 }
// RT-N3 期限边界：SubTaskRawRow 仅承接 list_subtasks_page 列表投影（字段集与 SubTaskRow 对齐，不含期限列）；
// queue_deadline 由恢复/调度路径单独读取（见 classify_recovery 调用方读 sub_tasks.queue_deadline），不进列表行。
// checked 枚举转换：本步为 model 枚举新增 from_str_exact 关联函数（同 crate inherent impl，不改
// model.rs）；字符串映射唯一真相源即 Task 2 serde（snake_case / SCREAMING_SNAKE_CASE），非法值
// 返回 None、由调用方统一映射 Database 503；Step 6 段 parse_subtask_state 函数体等价于
// SubTaskState::from_str_exact + 错误映射，两者同源，不重复定义第二套解析。
impl MainTaskState { pub fn from_str_exact(raw: &str) -> Option<Self> { serde_json::from_value::<Self>(serde_json::Value::String(raw.to_owned())).ok() } }
impl MainTaskOutcome { pub fn from_str_exact(raw: &str) -> Option<Self> { serde_json::from_value::<Self>(serde_json::Value::String(raw.to_owned())).ok() } }
impl SubTaskState { pub fn from_str_exact(raw: &str) -> Option<Self> { serde_json::from_value::<Self>(serde_json::Value::String(raw.to_owned())).ok() } }
impl ReasonCode { pub fn from_str_exact(raw: &str) -> Option<Self> { serde_json::from_value::<Self>(serde_json::Value::String(raw.to_owned())).ok() } }
/// 去敏 Database Decode 错误（503），消息不含实际列值（与 fixed_mysql_bytes 同口径）。
fn task_row_decode_error() -> ControllerError {
    ControllerError::Database(sqlx::Error::Decode(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData, "task row column has unexpected value",
    ))))
}
/// BINARY(16) 列 → Uuid：先经 fixed_mysql_bytes::<16> 校验长度再恢复（长度不符 → DB 损坏 503）。
fn mysql_uuid(raw: &[u8]) -> Result<Uuid, ControllerError> { Ok(Uuid::from_bytes(fixed_mysql_bytes::<16>(raw)?)) }
impl TryFrom<MainTaskRawRow> for MainTaskRow {
    type Error = ControllerError;
    fn try_from(raw: MainTaskRawRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: mysql_uuid(&raw.id)?,
            actor_user_id: mysql_uuid(&raw.actor_user_id)?,
            state: MainTaskState::from_str_exact(&raw.state).ok_or_else(task_row_decode_error)?,
            outcome: match &raw.outcome {
                None => None,
                Some(s) => Some(MainTaskOutcome::from_str_exact(s).ok_or_else(task_row_decode_error)?),
            },
            revision: raw.revision,
            counts_json: raw.counts_json,
        })
    }
}
impl TryFrom<SubTaskRawRow> for SubTaskRow {
    type Error = ControllerError;
    fn try_from(raw: SubTaskRawRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: mysql_uuid(&raw.id)?,
            device_id: fixed_mysql_bytes::<32>(&raw.device_id)?,
            state: SubTaskState::from_str_exact(&raw.state).ok_or_else(task_row_decode_error)?,
            reason_code: match &raw.reason_code {
                None => None,
                Some(s) => Some(ReasonCode::from_str_exact(s).ok_or_else(task_row_decode_error)?),
            },
            revision: raw.revision,
        })
    }
}
pub trait TaskRepository: Send + Sync {
    async fn begin(&self) -> Result<Transaction<'static, MySql>, ControllerError>; // pool 内部于仓储，begin() 为唯一事务入口
    async fn create_preview(&self, token_nonce: [u8; 32], actor_user_id: Uuid, targets: &[PreviewTarget], token_hash: &[u8; 32], expires_at: DateTime<Utc>) -> Result<(), ControllerError>; // RT-N2 方式二：token_nonce 由服务层从 preview token 服务端派生后入参、存储层直接落盘；返回类型保持 `Result<(), ControllerError>`（调用方本就持有 nonce，无需回传，也不存在"落库成功但返回值丢失"的半闭合）
    async fn consume_preview_and_create_tasks(&self, req: &SubmitRequest, ctx: &AuthCtx) -> Result<SubmitOutcome, ControllerError>; // 提交（旧 `consume_preview&create_tasks` 更名）
    async fn find_main_task(&self, main_task_id: Uuid) -> Result<Option<MainTaskRow>, ControllerError>; // 内部 tx_query_one::<MainTaskRawRow> → MainTaskRow::try_from（损坏行 → Database 503）
    async fn list_subtasks_page(&self, main_task_id: Uuid, cursor: Option<String>, limit: usize) -> Result<(Vec<SubTaskRow>, Option<String>), ControllerError>; // 内部 tx_query::<SubTaskRawRow> → 逐行 SubTaskRow::try_from（fail-closed）
    async fn cancel_subtask(&self, req: &CancelRequest, ctx: &AuthCtx) -> Result<CancelOutcome, ControllerError>;
    async fn time_review(&self, main_task_id: Uuid, expected_revision: u64, kind: TimeReviewEvidenceKind, ctx: &AuthCtx) -> Result<TimeReviewOutcome, ControllerError>; // Outcome/Error 定义见 Step 4
    async fn release_lock(&self, device_id: [u8; 32], owner_sub_task_id: Uuid, expected_revision: u64, acknowledge_unknown: bool, reason: &str, ctx: &AuthCtx) -> Result<ReleaseLockOutcome, ControllerError>; // 同上
}
```

  - **`find_main_task` / `list_subtasks_page` 行读取分层（B1 实体行闭合）**：两方法 trait 签名不变，内部一律 `tx_query_one::<MainTaskRawRow>` / `tx_query::<SubTaskRawRow>` 读原始行，再经 `MainTaskRow::try_from` / `SubTaskRow::try_from` checked 转换返回领域行；`find_main_task` 零行返回 `Ok(None)`，`list_subtasks_page` 按序逐行转换（fail-closed：任一行损坏即整页返回 Database 503，不跳行、不截断、不 panic）。领域行绝不进入 sqlx decode 路径，原始行绝不出仓储边界——与 `SubLockRow`/`PreviewSnapshotRow` 先例同分层，B1 实体行部分据此闭合（游标分页语义与 cursor 测试仍为后续待办项，不据此宣称 B1 整体闭合）。
  - **`create_preview` 持久化口径**：写入 `task_previews` 时持久化 `(token_nonce, actor_user_id, target_hash, targets_json, preview_token_hash, preview_generation, created_at, expires_at)`（`consumed_at` 留 NULL）；`target_hash` 是目标清单规范化升序后的 32 字节 SHA256 摘要（即 `device_target_hash` 计算值），供提交时直接读取并写入 `main_tasks.target_hash`。
  - **`SqlxTaskRepository::create_preview` 生产实现（B4 写入侧闭合）**：`SqlxTaskRepository` 为 `TaskRepository` 唯一生产实现（内部持有 `MySqlPool`，`begin()` 为唯一事务入口）；`create_preview` 为单语句 INSERT（存储层自动提交），**INSERT 成功返回（即提交成功）之后**才在内存调用 `register_preview_mono_deadline(token_nonce)` 登记进程单调 60s 期限——注册不得先于 DB 写入（先注册会为未落库行留下单调记录，令泄漏但未落库的 token 在 60s 内通过单调判定）。`targets_json` 与 `decode_preview_targets` 同一口径写出（`hex::encode(device_id)` → 字节序升序去重 → 64 小写 hex 字符串扁平 JSON 数组，`Value: Display` 恒成功、不 unwrap 序列化 Result）；`target_hash = device_target_hash(&device_ids, &[])`（`PreviewTarget` 不携带 group 维度，`group_ids` 恒空数组；提交侧 targets 若含 group，快照比较天然不一致而拒绝，fail-closed）；`preview_generation = preview_generation_bytes()`；`token_nonce` 由**调用方派生后入参**（RT-N2 方式二，本方法不再自行随机，见上文签名与代码块第 ④ 步）；入参 `token_hash` 即 `preview_token_hash` 列值。
**RT-N2 端到端闭环（B4 写入侧真正闭合）**：对外唯一凭据是 64hex `preview_token`，`preview_token_hash` 与 `token_nonce` 都是它的**服务端派生值**，客户端永不携带 hash/nonce（DTO `deny_unknown_fields` 拒外部注入同名字段）。`preview.rs` 新增三个纯函数：`preview_token_bytes(raw: &str) -> Option<[u8; 32]>`（恰 64 小写 hex 严格解码，大写/长度不符/非 hex → `None` → 400 `InvalidArgument`，不 trim、不做大小写折叠）、`preview_token_digest(token: &[u8; 32]) -> [u8; 32]`（`Sha256(token)`，即 `preview_token_hash` 列值）、`preview_token_nonce(token: &[u8; 32]) -> [u8; 32]`（`Sha256("rsetup:preview:nonce:v1" ‖ token)`，域分隔的**非密钥**派生，与 `PREVIEW_GENERATION` 同为非密钥 token，令"外部 token → 内部主键 nonce"映射可恢复、无需服务端会话状态）。**创建侧**：服务层生成 32B CSPRNG token（两段 `Uuid::new_v4`，uuid v4 为本仓库已使用的 CSPRNG 随机源，不新增依赖）
→ 派生 `token_hash`/`token_nonce` → 二者一并传入 `create_preview` 落盘 → INSERT 提交成功后 `register_preview_mono_deadline(token_nonce)` → 响应只回 `preview_token`（HTTP 契约不变）。**提交侧**：服务层对请求 `preview_token` 走同一 `preview_token_bytes` 严格解码与同一对派生函数 → 填入 `SubmitRequest.preview_token_hash`/`preview_token_nonce`（两字段一律服务端派生）→ 仓储按 nonce 主键 `FOR UPDATE` 取行后**必须**以恒定时间比较核验该行 `preview_token_hash` 等于请求派生摘要（见 Step 4 第 5 步）；不匹配即 409 `INVALID_STATE`，与"行不存在/已消费"同投影、不泄露差异。至此"token → 派生 hash/nonce → 落库 → 回传 token → 再派生 → 主键取行 → 摘要核验"闭环成立，写入 `main_tasks.preview_token_hash` 的摘要与消费侧核验的摘要为同一派生值；`created_at` 由 DB 时钟 `NOW(6)` 写入、与消费侧 `NOW(6)` 比较同钟域，`expires_at` 为调用方传入的跨进程上限，`consumed_at` 显式 NULL。任何 SQL/编码错误经 `?` 或 `map_err` 映射 `ControllerError::Database`（503），不吞错、不重试；`register_preview_mono_deadline` 返回 `Config` 错误即放弃该 preview（直接上抛）
——行可能已落库，但单调表无该 nonce、`preview_mono_valid` fail-closed 恒 false，消费必被拒，无需补偿删除。

```rust
// service.rs — use 行与既有 `use crate::tasks::preview::{canonical_device_order, decode_preview_targets, preview_generation_bytes, preview_mono_valid};` 去重并入，不重复既有导入；MySqlPool 并入 sqlx 既有导入
use crate::tasks::preview::{device_target_hash, preview_generation_bytes, register_preview_mono_deadline};

pub struct SqlxTaskRepository { pool: MySqlPool }

impl SqlxTaskRepository {
    pub fn new(pool: MySqlPool) -> Self { Self { pool } }
}

impl TaskRepository for SqlxTaskRepository {
    async fn begin(&self) -> Result<Transaction<'static, MySql>, ControllerError> {
        Ok(self.pool.begin().await?) // sqlx::Error 经 ? 转 Database，保留 503 存储语义
    }
    async fn create_preview(&self, token_nonce: [u8; 32], actor_user_id: Uuid, targets: &[PreviewTarget], token_hash: &[u8; 32], expires_at: DateTime<Utc>) -> Result<(), ControllerError> {
        // ① 目标规范化：字节序升序 + 去重（canonical_device_order 同口径）；eligible/reason_code 不持久化
        let mut device_ids: Vec<[u8; 32]> = targets.iter().map(|t| t.device_id).collect();
        device_ids.sort_unstable();
        device_ids.dedup();
        // ② target_hash：规范化升序清单的 SHA256 摘要，提交时直读写入 main_tasks.target_hash
        let target_hash = device_target_hash(&device_ids, &[]);
        // ③ targets_json：64 小写 hex 字符串扁平 JSON 数组，与 decode_preview_targets 逐字节同口径
        let targets_json = serde_json::Value::Array(
            device_ids.iter().map(|id| serde_json::Value::String(hex::encode(id))).collect(),
        ).to_string();
        // ④ 进程启动 epoch；token_nonce 为调用方服务端派生值（RT-N2 方式二），本方法不再生成随机数，
        //    只做持久化——随机源缺失即无 nonce 可落盘，全零 nonce 仍被 preview_mono_valid fail-closed 拒绝
        let preview_generation = preview_generation_bytes();
        // ⑤ INSERT：params! 位置绑定序 = SQL `?` 序，编码错误 map_err 成 Database(Encode)；BINARY 列以 &[u8] 切片绑定
        let args = params![
            &token_nonce[..], actor_user_id, &target_hash[..], &targets_json,
            &token_hash[..], &preview_generation[..], expires_at,
        ];
        let rows = sqlx::query_with::<MySql, _>(
            "INSERT INTO task_previews (token_nonce, actor_user_id, target_hash, targets_json, \
             preview_token_hash, preview_generation, created_at, expires_at, consumed_at) \
             VALUES (?, ?, ?, ?, ?, ?, NOW(6), ?, NULL)",
            args,
        ).execute(&self.pool).await?;
        // MySqlQueryResult 未实现与整数的 PartialEq（sqlx-mysql 0.8.6）：受影响行数只能经 rows_affected() 判断
        if rows.rows_affected() != 1 {
            return Err(ControllerError::Database(sqlx::Error::RowNotFound));
        }
        // ⑥ 单调期限唯一注册点：提交成功之后、返回 preview token 之前；失败即放弃该 preview（fail-closed，不补偿删除）
        register_preview_mono_deadline(token_nonce)?;
        Ok(())
    }
    // 其余 trait 方法与 Step 4 服务内联编排共用同一 impl 块与 SQL（单一真相源），此处不展开
}
```

  - **单调注册单测说明（B4 核销补充，`preview_generation_tests` 扩展，本切片不执行任何命令）**：`register_deadline_in` 对新 nonce 返回 `true` 且 `mono_deadline_allows` 在注入固定 `Instant` 下 `TTL - 1s` 判有效、`TTL + 1s` 判失效；**墙钟回拨不影响判定**——判定链只读 `Instant` 单调跨度，绝不读 `Utc::now`/`DateTime::now`，回拨墙钟不得延长有效期；同一 nonce 重复注册返回 `true` 且 deadline 保留首次值；到期项被 prune。`create_preview` 写入侧集成断言（targets_json 与 `decode_preview_targets` 往返一致、`target_hash` 与 `device_target_hash` 一致、重复 nonce 主键冲突 → `Database`）属后续 DB 集成切片，不据此宣称集成闭合。
  - `pub struct TaskService<R: TaskRepository> { pub repo: R, pub time_gate: crate::time::gate::TimeGateHandle }`（RT-N5：`time_review` 需服务端可信时钟快照，`TimeGateHandle` 由 Task 1 `start_time_gate` 产出并在构造时注入，服务内经 `self.time_gate.current().await` 取当前 `TimeEvidence`，决不接受请求侧自报时钟或自报证据参与分派）
  - RT-R10-3（04 规格 L19/L93）：`TaskService` 增字段 `pub device_cache: Arc<DeviceCache>`（构造时注入）。`DeviceCache` 为 Board 连接层心跳/遥测维护的设备运行态缓存：`#[derive(Debug, Clone, Copy)] pub struct DeviceRuntimeSnapshot { pub online: bool, pub reboot_capable: bool }`；`impl DeviceCache { pub fn snapshot(&self) -> HashMap<[u8; 32], DeviceRuntimeSnapshot> }` 在 `RwLock` 读锁下一次性克隆返回后立即释放。`submit` 于进入事务前读取该快照；缓存未命中的设备一律按离线且无 reboot 能力处理（保守口径），绝不以缓存缺失为由入队等上线。
  - `impl<R: TaskRepository> TaskService<R> { pub fn preview(&self, ...) ; pub fn submit(&self, ...) ; pub fn cancel(...) ; pub fn time_review(...) ; pub fn release_lock(...) }`

- [ ] **Step 1: 先建立可编译桩，再编写行为 RED 测试**

`src/tasks/preview.rs` 桩：

```rust
use uuid::Uuid;
use crate::ControllerError;
use crate::tasks::model::ReasonCode;

/// 可编译 RED 桩：恒 Ok，未拦截 0 与 >1024。
pub fn validate_target_count(_count: usize) -> Result<(), ControllerError> { Ok(()) }

/// 可编译 RED 桩：只按输入顺序去重，不排序。
pub fn canonical_device_order(ids: &mut Vec<[u8; 32]>) { ids.dedup(); }

/// 可编译 RED 桩：恒返回全零摘要，无法区分不同清单。
pub fn device_target_hash(_device_ids: &[[u8; 32]], _group_ids: &[Uuid]) -> [u8; 32] { [0u8; 32] }

/// 可编译 RED 桩：只检查长度，接受非 UUIDv4/大写/非 canonical。
pub fn is_canonical_uuid_v4_key(key: &str) -> bool { key.len() == 36 }
```

`src/tasks/service.rs` 桩：

```rust
use crate::tasks::model::{ReasonCode, SubTaskState};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeReviewEvidenceKind { ReferenceDeadlineProven, ReferenceDeadlineExpired, Inconclusive }

/// 可编译 RED 桩：无条件允许延长，违反“绝不延长”约束。
pub fn time_review_allows_extend(_requested_extension_ms: i64, _kind: TimeReviewEvidenceKind) -> bool {
    true
}

/// 可编译 RED 桩：把 unknown 判成可取消，违反 04 §8。
pub fn cancel_precondition(state: SubTaskState, _is_admin: bool, _has_reboot: bool) -> bool {
    !matches!(state, SubTaskState::Dispatching)
}
```

内联测试（必须 RED）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_rejects_empty_and_excessive_targets() {
        assert!(matches!(validate_target_count(0), Err(crate::ControllerError::InvalidArgument)));
        assert!(validate_target_count(1).is_ok());
        assert!(validate_target_count(1024).is_ok());
        assert!(matches!(validate_target_count(1025), Err(crate::ControllerError::InvalidArgument)));
    }

    #[test]
    fn canonical_order_is_sorted_and_deduplicated() {
        let mut ids = vec![[0x02u8; 32], [0x01u8; 32], [0x02u8; 32], [0x00u8; 32]];
        canonical_device_order(&mut ids);
        assert_eq!(ids, vec![[0x00u8; 32], [0x01u8; 32], [0x02u8; 32]]);
    }

    #[test]
    fn target_hash_is_order_independent_and_content_sensitive() {
        let a = [[0x01u8; 32], [0x02u8; 32]];
        let b = [[0x02u8; 32], [0x01u8; 32]];
        assert_eq!(device_target_hash(&a, &[]), device_target_hash(&b, &[]));
        assert_ne!(device_target_hash(&a, &[]), device_target_hash(&a[..1], &[]));
        let g = [uuid::Uuid::from_u128(7)];
        assert_ne!(device_target_hash(&a, &[]), device_target_hash(&a, &g));
    }

    #[test]
    fn idempotency_key_must_be_canonical_lowercase_uuid_v4() {
        assert!(is_canonical_uuid_v4_key("3f2504e0-4f89-41d3-9a0c-0305e82c3301"));
        assert!(!is_canonical_uuid_v4_key("3F2504E0-4F89-41D3-9A0C-0305E82C3301"));
        assert!(!is_canonical_uuid_v4_key("3f2504e0-4f89-11d3-9a0c-0305e82c3301")); // version 1
        assert!(!is_canonical_uuid_v4_key("3f2504e04f8941d39a0c0305e82c3301"));   // 无连字符
        assert!(!is_canonical_uuid_v4_key("3f2504e0-4f89-41d3-1a0c-0305e82c3301")); // variant 非法
    }

    #[test]
    fn time_review_never_extends_deadline() {
        // 核验只能恢复原期限或判过期，绝不允许延长
        assert!(!time_review_allows_extend(60_000, TimeReviewEvidenceKind::ReferenceDeadlineProven));
        assert!(!time_review_allows_extend(1, TimeReviewEvidenceKind::Inconclusive));
        assert!(!time_review_allows_extend(-1, TimeReviewEvidenceKind::ReferenceDeadlineExpired));
    }

    #[test]
    fn cancel_is_only_for_queued_or_held() {
        use crate::tasks::model::SubTaskState::*;
        assert!(cancel_precondition(Queued, false, true));
        assert!(cancel_precondition(Held, false, true));
        assert!(!cancel_precondition(Dispatching, true, true));
        assert!(!cancel_precondition(Accepted, true, true));
        assert!(!cancel_precondition(Verifying, true, true));
        assert!(!cancel_precondition(Unknown, true, true));
        assert!(!cancel_precondition(Succeeded, true, true));
        assert!(!cancel_precondition(Cancelled, true, true));
        assert!(!cancel_precondition(Expired, true, true));
        assert!(!cancel_precondition(Failed, true, true));
        // 普通用户失去 device.reboot 即不可取消
        assert!(!cancel_precondition(Queued, false, false));
    }
}
```

- [ ] **Step 2: 运行并确认行为 RED（严禁编译错误与 0 用例）**

```bash
cargo test --offline --locked -p rsetup-controller preview_rejects_empty_and_excessive_targets --
cargo test --offline --locked -p rsetup-controller canonical_order_is_sorted_and_deduplicated --
cargo test --offline --locked -p rsetup-controller target_hash_is_order_independent_and_content_sensitive --
cargo test --offline --locked -p rsetup-controller time_review_never_extends_deadline --
cargo test --offline --locked -p rsetup-controller cancel_is_only_for_queued_or_held --
cargo test --offline --locked -p rsetup-controller idempotency_key_must_be_canonical_lowercase_uuid_v4 --
```
预期：各恰好执行 1 个用例并因业务断言失败（`validate_target_count` 桩恒 Ok、`canonical_device_order` 桩只去重不排序、`device_target_hash` 桩恒全零摘要、`time_review_allows_extend` 桩恒 true、`cancel_precondition` 桩把 `Held/Unknown` 放行、`is_canonical_uuid_v4_key` 桩只查长度）；六条命令均在 RED 阶段即运行并失败，`canonical_order_is_sorted_and_deduplicated` 与 `target_hash_is_order_independent_and_content_sensitive` 的首次运行不得推迟到 GREEN。

- [ ] **Step 3: 最小实现（纯逻辑部分）**

```rust
pub fn validate_target_count(count: usize) -> Result<(), ControllerError> {
    if (1..=1024).contains(&count) { Ok(()) } else { Err(ControllerError::InvalidArgument) }
}

pub fn canonical_device_order(ids: &mut Vec<[u8; 32]>) {
    ids.sort_unstable();
    ids.dedup();
}

pub fn device_target_hash(device_ids: &[[u8; 32]], group_ids: &[Uuid]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"rsetup.task.targets.v1");
    hasher.update((device_ids.len() as u64).to_be_bytes());
    let mut ids: Vec<[u8; 32]> = device_ids.to_vec();
    ids.sort_unstable(); // 规范化排序（升序字节序），使 hash 与输入顺序无关
    for id in &ids { hasher.update(id); }
    hasher.update((group_ids.len() as u64).to_be_bytes());
    let mut groups: Vec<Uuid> = group_ids.to_vec();
    groups.sort_unstable();
    for g in groups { hasher.update(g.as_bytes()); }
    hasher.finalize().into()
}

pub fn is_canonical_uuid_v4_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    if bytes.len() != 36 { return false; }
    for (i, b) in bytes.iter().enumerate() {
        let dash = matches!(i, 8 | 13 | 18 | 23);
        if dash { if *b != b'-' { return false; } }
        else if !matches!(b, b'0'..=b'9' | b'a'..=b'f') { return false; }
    }
    bytes[14] == b'4' && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}

pub fn time_review_allows_extend(requested_extension_ms: i64, kind: TimeReviewEvidenceKind) -> bool {
    // 仅当证据为 ReferenceDeadlineProven 且延长量为 0 时允许恢复原期限；绝不延长。
    matches!(kind, TimeReviewEvidenceKind::ReferenceDeadlineProven) && requested_extension_ms == 0
}

pub fn cancel_precondition(state: SubTaskState, is_admin: bool, has_reboot: bool) -> bool {
    matches!(state, SubTaskState::Queued | SubTaskState::Held) && (is_admin || has_reboot)
}
```

- [ ] **Step 4: 实现事务编排（仍为可编译、可审查切片；真实库断言见 Step 6）**

`TaskService::submit` 的固定顺序（**必须逐字实现该顺序**，事务内零网络）：

1. 校验 `is_canonical_uuid_v4_key(idempotency_key)`，否则 `InvalidArgument`；
2. 短事务开始 → `lock_schema_guard_for_version(tx, SUPPORTED_SCHEMA_VERSION)`；
3. 重新读取 actor 行并核验 `active`（普通用户持合法权限即可提交，**不要求** `is_admin`，与代码已去除强制管理员保持一致；`active` 校验保留）；在 guard 内读取当前 `authz_epoch`；
4. **同事务先查持久幂等记录（存在即直接重放，违背先删预览再判幂等的严重缺陷已修正）**：
   `SELECT id, request_hash, target_hash FROM main_tasks WHERE actor_user_id = ? AND idempotency_key = ?`；
   - 若记录存在且 `request_hash` 完全一致：说明该请求此前已成功提交，**即使此时 preview 已过期或被消费，仍直接返回原 `Replayed { main_task_id }`**（事务回滚退出，零副作用，不再次消费预览，不扩大目标）；
   - 若记录存在但 `request_hash` 不一致：说明同一 actor 对相同幂等键提供了不同载荷，回滚事务并返回 `SubmitError::IdempotencyConflict`（HTTP 409 `IDEMPOTENCY_CONFLICT`），绝不新建任务或更新目标；
5. **首次提交才判定有效性、消费 preview 并读出固定目标清单**：幂等未命中时按 a→d 顺序执行：
   a. `SELECT target_hash, targets_json, preview_generation FROM task_previews WHERE token_nonce = ? AND actor_user_id = ? FOR UPDATE`（先锁住该行，杜绝"判定后被并发消费"的窗口）；无行 → 先按 `consumed_at IS NOT NULL` 区分 `PreviewConsumed`/`PreviewExpired`（判别查询须在回滚**之前**执行），再回滚返回；
   b. 有效性三重判定（缺一即回滚返回 `PreviewExpired`，HTTP 409 `INVALID_STATE`；对齐集成账本 T4-04"60s 单调有效"）：① `preview_generation` 等于本进程启动 epoch（`OnceLock` 一次性取值，同时参与 preview token 摘要输入，跨代际 token 无法重新派生匹配，进程重启即令全部存量 preview 失效）；② 进程内**单调** TTL 未到期（`preview_mono_valid(nonce)`：创建时在本进程 `PREVIEW_MONO_DEADLINES` 登记 `Instant + 60s`，消费时判定 `elapsed() <= 60s`，墙钟回拨**不得**延长有效期）；③ `expires_at > NOW(6)` 仅作跨进程上限兜底与 GC 索引，**不得单独**作为有效性依据；
   c. 原子单次消费：`DELETE FROM task_previews WHERE token_nonce = ? AND actor_user_id = ? AND consumed_at IS NULL AND expires_at > NOW(6) AND preview_generation = ?`；`rows_affected == 0` → 回滚并返回 `PreviewExpired` 或 `PreviewConsumed`（这是原子单次使用的唯一实现方式，禁止用 `UPDATE ... consumed_at` 两段式替代）；
   d. **目标真相源**：提交只使用 a 步读出的 `targets_json` 固定清单；`req.target`、`req.target_hash`、`req.counts_json` **一律不作为持久化真相源**（仅 `req.request_hash` 参与幂等比较），杜绝"客户端自报目标"扩大授权面；该三字段在后续切片确认无消费方后应从 `SubmitRequest` 删除（不改 `consume_preview_and_create_tasks` trait 签名）。
6. **锁序（防倒挂，与 Identity 计划 `users → roles → device_groups → devices` 及 `ordered_user_ids` 字节序 first→second 风格一致）**：同一事务内先按 `grants`（`user_id` 定位）取所需 `role_id`/`scope_group_id` 集合，按 `role_id` UUID 字节序升序 `FOR UPDATE` 锁 `roles`，再按 `scope_group_id` UUID 字节序升序锁 `device_groups`，最后按 `public_key` 字节序升序（即 `canonical_device_order`，先 `IN (...)` 去重排序）锁 `devices`，之后才进入 `device_operation_locks` CAS；任何路径不得逆序取锁、不得重复锁同一行。
7. 首次提交写入主任务：`INSERT INTO main_tasks (id, actor_user_id, idempotency_key, request_hash, target_hash, preview_token_hash, state, outcome, revision, counts_json, created_time_json, created_at, updated_at) VALUES (...)`，其中 `target_hash` 取 a 步快照值、`counts_json` 先写 10 键全零占位（`empty_counts_json()`，第 10 步按真实结果回写）、`created_time_json` 为创建时经 `self.time_gate.current().await` 取出的 `TimeEvidence` JSON 快照，与创建同一事务持久化（RT-R8-2，time_review 事务内读取核验）；
8. 目标按第 6 步锁序读出的固定清单以 `canonical_device_order` 顺序逐个处理：**逐目标重验当前授权（RT-N6：可见性与 reboot 权限解耦，判定返回 `TargetAuthz` 三态）**（`device_permitted`：同一 tx 内复用 Identity 真源 `load_snapshot_in_tx` + `snapshot_effective_permissions` 求该 device 当前有效权限集后经 `check_target_authz` 三态判定：含 `Permission::DeviceReboot` → `Permitted`（**只要求 reboot 权限，不要求 `device.task.read`**——读权限不门控创建/提交，详情/结果投影保留独立 `device.task.read`，见 Task 6）；不含 reboot 但含其他有效权限（如 `Permission::DeviceRead` / `Permission::DeviceTaskRead`）→ `VisibleForbidden`；空集 → `NotVisible`；inactive/must-change actor 与 missing/archived 设备由真源先排除得空集，admin 亦依真源事务内重验、不信 ctx 旧布尔，全部依据第 6 步锁定行判定；真源 DB 解码/后端错误以 `SubmitError::Backend` 透传（503 语义），不降为空权限/400）；**仅 `NotVisible`（完全不可见）→ 回滚整个事务并返回 `SubmitError::NotVisible`（HTTP 404 `NOT_FOUND`，与"不存在"同投影，整体不创建、不泄露部分结果）**；**`VisibleForbidden` 不得伪装成不可见**：保留固定目标清单、不申请设备锁，直接 `INSERT INTO sub_tasks ... state='failed', outcome='failed', reason_code='PERMISSION_DENIED'` 并计入 failed 统计，其余 `Permitted` 目标正常执行；`Permitted` 通过后 `INSERT INTO sub_tasks ...`，并通过
```sql
INSERT INTO device_operation_locks (device_id, main_task_id, owner_sub_task_id, generation, acquired_at)
SELECT ?, ?, ?, 1, NOW(6) FROM DUAL
WHERE NOT EXISTS (SELECT 1 FROM device_operation_locks WHERE device_id = ?)
```
申请锁；未插入（`rows_affected == 0`）→ 该子任务 `state = failed`、`reason_code = DEVICE_BUSY`，**不排队、不等待**；
9. 同一事务写 11 字段脱敏审计（`time_evidence` 非空、`process_epoch`/`event_seq` 唯一）；
10. **counts 真实写入**：以第 8 步实际子任务结果聚合分类（`locked`、`device_busy`），`submit_counts_json(locked, device_busy)` 生成 10 键十态频数对象（`queued = locked`、`failed = device_busy`，其余 8 键 `"0"`，与 Task 6 `project_main_task_summary` 十键口径同源），`UPDATE main_tasks SET counts_json = ?, updated_at = NOW(6) WHERE id = ?` 后才 `COMMIT`；**废止**"先写 `req.counts_json` 再丢弃"的假持久化路径；**RT-R8-3 / RT-R9-2 主任务收敛**：回写 counts 的同一 UPDATE 位点无论何种子状态分布，一律以实际子状态分布构造 `sub_states`（`locked` 个 `Queued` + `device_busy` 个 `Failed`）并经 Task 5 唯一聚合器 `calculate_main_task_outcome` 计算 `state`/`outcome`（全 failed → `(Completed, Failed)`；混合 `[Queued, Failed]` → `(Running, None)`，04 规格 L79–83），随 `revision + 1` 同条 UPDATE 落库，与 HTTP 动态投影同源不分裂；
11. `COMMIT`；仅提交成功后才返回 `Created`/`Replayed`。**事务内绝不调用任何 `BoardClient`。**

本步新增的私有 helper 与行结构（同段落声明，**不新增 trait 方法、不改 trait 签名**）：`#[derive(Debug, FromRow)] struct PreviewSnapshotRow { target_hash: Vec<u8>, targets_json: String, preview_generation: Vec<u8>, preview_token_hash: Vec<u8> }`（RT-N2 增列 `preview_token_hash`：消费侧恒定时间核验所用的持久摘要，同样以 `Vec<u8>` 承接后经 `fixed_mysql_bytes::<32>` 校验长度，长度不符 → DB 损坏 503；核验在锁内当场完成，该列**不**进入 `PreviewSnapshotFixed`，下游不重复携带）（sqlx-mysql 0.8.6 无 `[u8; N]` 的 `Type`/`Encode`/`Decode`，`FromRow` 固定宽列一律以 `Vec<u8>` 承接，读出后经 `fixed_mysql_bytes` 显式校验长度恢复 `[u8; 32]`/`[u8; 16]` 再使用；长度不符视为 DB 损坏返回 `ControllerError::Database`（HTTP 503），不 truncate、不 unwrap/panic、不返回 400；校验通过的快照以 `struct PreviewSnapshotFixed { target_hash: [u8; 32], targets_json: String, preview_generation: [u8; 16] }` 在锁内继续交付下游比较与持久化）；`fn fixed_mysql_bytes<const N: usize>(raw: &[u8]) -> Result<[u8; N], ControllerError>`（service.rs 侧私有通用长度校验 helper，完整定义见下方代码块）；`preview.rs` 侧 `pub fn preview_generation_bytes() -> [u8; 16]`（进程启动 epoch）、`pub fn preview_mono_valid(nonce: &[u8; 32]) -> bool`、`pub fn decode_preview_targets(raw: &str) -> Option<Vec<[u8; 32]>>`（快照损坏 → `None`）；`service.rs` 侧 `fn empty_counts_json() -> String`、`fn submit_counts_json(locked: u64, device_busy: u64) -> String`（counts 唯一生产者）、`TaskService::lock_authz_rows_in_order(&self, tx, user_id, targets)`（第 6 步锁序，真实实现见下方代码块，配套 `#[derive(Debug, FromRow)] struct AuthzLockGrantRow { role_id: Option<Vec<u8>>, scope_group_id: Option<Vec<u8>> }` 授权引用集行结构；`std::collections::BTreeSet` 与 `MySqlArguments` 去重并入 `service.rs` 顶层导入，不重复既有导入）、`TaskService::device_permitted(&self, tx, ctx, device_id)`（逐目标重验，真实实现见下方代码块）。

`TaskService::device_permitted` 真实实现（与第 6 步锁定行同一 tx、零网络、不开新事务、不造第二套授权体系；`load_snapshot_in_tx` 为 Identity 计划交付的 `pub(crate)` 真源，tasks 模块同 crate 内直接复用。本 helper 只承担**授权判定**职责，**不替代** `lock_authz_rows_in_order` 的锁序职责——后者完整实现已随本代码块一并落地（紧随 `device_permitted` 之后），`schema→users→roles→groups→devices→operation_locks` 总序的锁序就此闭合：`schema_meta` 第 2 步已锁、`users` 环节由 actor 重验读承担，helper 自 `roles` 起按 `roles → device_groups → devices` 主键字节序升序三段 `FOR UPDATE`，与 `load_snapshot_in_tx` 读序完全一致、无倒挂、无第二套锁序）：

```rust
use crate::authz::policy::{AuthorizationSnapshot, Permission};
use crate::authz::service::snapshot_effective_permissions;
use crate::authz::sqlx_repo::load_snapshot_in_tx;
use std::collections::BTreeSet; // `BTreeSet<Vec<u8>>` 自然序 = 字节升序 + 去重，与 Identity `ordered_user_ids` first→second 风格一致

/// 第 6 步授权引用集读出行：`grants.role_id`/`grants.scope_group_id` 均可空（inline 授权无 role_id、
/// 非 group 范围无 scope_group_id），以 `Option<Vec<u8>>` 承接，`None` 不参与锁行；
/// actor 定位谓词与 Identity 真源 `LIST_ACTOR_GRANTS_SQL` 同源。
#[derive(Debug, FromRow)] struct AuthzLockGrantRow { role_id: Option<Vec<u8>>, scope_group_id: Option<Vec<u8>> }

/// 目标授权三态（RT-N6：目标设备**可见性**与 **reboot 权限**解耦，04 任务生命周期规格）：
/// 只有完全不可见才整体 404；可见但缺 `DeviceReboot` **不得**伪装成不可见，必须保留固定
/// 目标清单、把该设备子任务直接落为 `failed`（`PERMISSION_DENIED`），其余有权目标正常执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetAuthz {
    /// 可见且持有 `Permission::DeviceReboot`：按原流程抢设备锁、创建 `queued` 子任务。
    Permitted,
    /// 可见（持有 `Permission::DeviceRead` / `Permission::DeviceTaskRead` 等其他有效权限）
    /// 但缺 `DeviceReboot`：不申请设备锁，直接创建 `failed` 子任务（`state = "failed"`、
    /// `outcome = "failed"`、`reason_code = Some("PERMISSION_DENIED")`）并计入 failed 统计。
    VisibleForbidden,
    /// 完全不可见（该设备无任何有效权限、真源求值为空集且无可见投影）：整体回滚 404。
    NotVisible,
}

/// 纯 consumer seam（service.rs 私有、无 IO）：以 `snapshot_effective_permissions` 对
/// Identity 真源快照求该 device 当前有效权限集，**解耦可见性与 reboot 权限**做三态判定：
/// 含 `Permission::DeviceReboot` → `Permitted`；不含 reboot 但含任何其他有效权限
/// （如 `Permission::DeviceRead` / `Permission::DeviceTaskRead`）→ `VisibleForbidden`；
/// 空集（inactive/must-change actor 与 missing/archived 设备由真源先排除，无权限即无可见
/// 投影）→ `NotVisible`。`device_permitted` 载入快照后调用它；下方纯策略单测经
/// `snapshot_allows_task_creation` 薄包装断言同一实现，不在测试内重写授权规则。
fn check_target_authz(snapshot: &AuthorizationSnapshot, device_id: [u8; 32]) -> TargetAuthz {
    let effective = snapshot_effective_permissions(snapshot, device_id);
    if effective.contains(&Permission::DeviceReboot) {
        return TargetAuthz::Permitted;
    }
    if !effective.is_empty() {
        return TargetAuthz::VisibleForbidden;
    }
    TargetAuthz::NotVisible
}

/// 纯策略单测既有布尔 seam 的薄包装（`TargetAuthz::Permitted` 的布尔投影，口径与 RT-N6 前
/// 一致：只要求 `Permission::DeviceReboot`）；提交路径**不得**消费本包装，必须走
/// `check_target_authz` 三态。
fn snapshot_allows_task_creation(snapshot: &AuthorizationSnapshot, device_id: [u8; 32]) -> bool {
    matches!(check_target_authz(snapshot, device_id), TargetAuthz::Permitted)
}

impl<R: TaskRepository> TaskService<R> {
    /// 逐目标重验当前授权，返回 `TargetAuthz` 三态（RT-N6：可见性与 reboot 权限解耦；
    /// 创建/提交只要求 `Permission::DeviceReboot`，**不要求** `device.task.read`——
    /// 读权限不门控创建/提交；详情/结果投影保留独立 `device.task.read`，见 Task 6）。
    /// 在第 6 步已锁定的同一 tx 内复用 Identity 真源：`load_snapshot_in_tx` 按固定读序
    /// actor 行 → epoch → grants → roles → device groups → 设备行读取（锁定行已由本 tx 持有，
    /// 不开新事务、零网络）；判定委托 `check_target_authz`——其内部先排除
    /// inactive/must-change actor 与 missing/archived 设备（得空集）再求当前有效权限，
    /// admin shortcut 同样由事务内新读 actor 行判定，**不信** `ctx.is_admin` 旧布尔。
    /// 错误保真：真源错误经 `.map_err(SubmitError::Backend)?` 原样包装，而
    /// `From<SubmitError> for ControllerError` 的 `Backend(err) => err` 保留原变体——
    /// actor 行缺失在 Identity 真源是 `PermissionDenied`（HTTP 403），**不重映为 503**；
    /// 仅 DB 解码污染的 `Config` 与 `Database` 落 503 `STORAGE_UNAVAILABLE`；
    /// **不降为空权限/400**。三态口径：`NotVisible` 由调用方（第 8 步）以
    /// `SubmitError::NotVisible`（404 同投影）回滚整个事务；`VisibleForbidden` **不**回滚、
    /// **不**抢设备锁，由调用方直接落 `failed` 子任务（`PERMISSION_DENIED`）；
    /// `Permitted` 走原抢锁排队流程。
    async fn device_permitted(
        &self,
        tx: &mut Transaction<'_, MySql>,
        ctx: &AuthCtx,
        device_id: [u8; 32],
    ) -> Result<TargetAuthz, SubmitError> {
        let snapshot = load_snapshot_in_tx(tx, ctx.user_id.into_bytes(), &[device_id])
            .await
            .map_err(SubmitError::Backend)?;
        Ok(check_target_authz(&snapshot, device_id))
    }

    /// 第 6 步锁序（防倒挂契约的唯一实现，同一短事务、零网络、不开新事务、不造第二套授权体系）。
    /// 总序 `schema_meta → users → roles → device_groups → devices → device_operation_locks`：
    /// `schema_meta` 已由第 2 步 `lock_schema_guard_for_version` 锁定，`users` 环节由第 4 步 actor
    /// 重验读（同 tx `SELECT active, is_admin FROM users WHERE id = ?`）承担，本 helper 从 `roles`
    /// 起锁，三段严格 `roles → device_groups → devices`，不得逆序、不得重复锁同一行、不得跨段交错。
    /// 每段按主键字节序升序 `FOR UPDATE` 行锁（role_id / scope_group_id 为 UUID 字节、devices 为
    /// public_key 字节），`BTreeSet<Vec<u8>>` 自然序即"升序 + 去重"；集合为空安全跳过对应表。
    /// `grants` 引用集以普通 SELECT 读出（与 `load_snapshot_in_tx` 的 grants 读序完全一致、无倒挂，
    /// grants 行本身不是锁目标、不锁）。所有错误（含参数编码失败）一律映射 `SubmitError::Backend`
    /// （503 语义），不降为 400/空权限；`device_operation_locks` CAS 只在本 helper 返回后进入。
    async fn lock_authz_rows_in_order(
        &self,
        tx: &mut Transaction<'_, MySql>,
        user_id: Uuid,
        targets: &[[u8; 32]],
    ) -> Result<(), SubmitError> {
        // ① 读出授权引用集（普通 SELECT，无 FOR UPDATE；空 grants → 两集合为空，后三段全跳过）
        let grant_rows = self
            .tx_query::<AuthzLockGrantRow>(
                tx,
                "SELECT role_id, scope_group_id FROM grants WHERE user_id = ?",
                params![user_id],
            )
            .await
            .map_err(SubmitError::Backend)?;
        let mut role_ids: BTreeSet<Vec<u8>> = BTreeSet::new();
        let mut group_ids: BTreeSet<Vec<u8>> = BTreeSet::new();
        for row in grant_rows {
            if let Some(role_id) = row.role_id {
                role_ids.insert(role_id);
            }
            if let Some(group_id) = row.scope_group_id {
                group_ids.insert(group_id);
            }
        }
        // ② roles：role_id UUID 字节序升序 FOR UPDATE（空集合安全跳过）
        if !role_ids.is_empty() {
            let placeholders = vec!["?"; role_ids.len()].join(", ");
            let mut args = MySqlArguments::default();
            for id in &role_ids {
                sqlx::Arguments::add(&mut args, &id[..])
                    .map_err(|err| SubmitError::Backend(ControllerError::Database(sqlx::Error::Encode(err))))?;
            }
            let sql = format!("SELECT id FROM roles WHERE id IN ({placeholders}) FOR UPDATE");
            self.tx_execute(tx, &sql, args).await.map_err(SubmitError::Backend)?;
        }
        // ③ device_groups：scope_group_id UUID 字节序升序 FOR UPDATE（空集合安全跳过）
        if !group_ids.is_empty() {
            let placeholders = vec!["?"; group_ids.len()].join(", ");
            let mut args = MySqlArguments::default();
            for id in &group_ids {
                sqlx::Arguments::add(&mut args, &id[..])
                    .map_err(|err| SubmitError::Backend(ControllerError::Database(sqlx::Error::Encode(err))))?;
            }
            let sql = format!("SELECT id FROM device_groups WHERE id IN ({placeholders}) FOR UPDATE");
            self.tx_execute(tx, &sql, args).await.map_err(SubmitError::Backend)?;
        }
        // ④ devices：public_key 字节序升序 FOR UPDATE（targets 已由 canonical_device_order 升序去重，
        //    再过 BTreeSet 为纵深防御，锁序不依赖调用方传入顺序）
        let device_ids: BTreeSet<Vec<u8>> = targets.iter().map(|id| id.to_vec()).collect();
        if !device_ids.is_empty() {
            let placeholders = vec!["?"; device_ids.len()].join(", ");
            let mut args = MySqlArguments::default();
            for id in &device_ids {
                sqlx::Arguments::add(&mut args, &id[..])
                    .map_err(|err| SubmitError::Backend(ControllerError::Database(sqlx::Error::Encode(err))))?;
            }
            let sql = format!("SELECT public_key FROM devices WHERE public_key IN ({placeholders}) FOR UPDATE");
            self.tx_execute(tx, &sql, args).await.map_err(SubmitError::Backend)?;
        }
        Ok(())
    }
}
```

借用与类型口径（grep+read 核对 Identity 计划真源，非凭描述猜）：`load_snapshot_in_tx` 定义于 Identity 计划（identity-execution.md L2580-2595）`pub(crate) async fn(tx: &mut sqlx::Transaction<'_, sqlx::MySql>, actor_id: [u8; 16], device_ids: &[[u8; 32]]) -> Result<AuthorizationSnapshot, ControllerError>`，固定读序不得换序、不得拆到别的连接；`snapshot_effective_permissions` 定义于 Identity 计划（L4375-4385）`pub fn(&AuthorizationSnapshot, [u8; 32]) -> HashSet<Permission>`，先排除 `!actor.active || actor.must_change_password` 与 missing/archived 设备再调 `eval_memory_permissions`（admin shortcut 在其内部按事务内新读 actor 行判定）。`ctx.user_id: Uuid` 经 `into_bytes()` 转 `[u8; 16]`，与本计划审计写入既有 `actor_user_id: Some(ctx.user_id.into_bytes())` 同源；`Transaction<'_, MySql>` 沿用上方事务 helper 段落既有导入，三条 `use`（含 `AuthorizationSnapshot`）去重并入 `service.rs` 顶层、不重复既有导入。`check_target_authz` 是 `service.rs` 私有纯函数 seam（`device_permitted` 三态判定的唯一实现，不新增 trait、不新增依赖；`snapshot_allows_task_creation` 仅保留为纯策略单测的布尔薄包装，提交路径不消费）；错误口径以 B2 下方 `From<SubmitError> for ControllerError` 代码块为准：`Backend(err) => err` 原样保真，Identity 真源 `decode_snapshot_actor` 行缺失返回 `PermissionDenied`（403）保持 403，`read_error` 映射的 `Config`（503 `STORAGE_UNAVAILABLE`）与 `Database`（503）保持 503，任何真源错误都不得重映为其他状态码或降级为 `TargetAuthz::NotVisible`。

**局部纯策略测试（4 个策略用例 + 1 个错误保真回归，钉死本 helper 口径）**：测试直接调用上方代码块定义的 `snapshot_allows_task_creation`（经薄包装走 `check_target_authz`，与 `device_permitted` 载入快照后同一判定实现），断言真实 `snapshot_effective_permissions` 的求值结果，不在测试内重写授权规则、不以 `contains` 常量恒真。fixture 以 struct 字面量直接构造 Identity 计划真实结构（字段逐一核对 identity-execution.md：L811 `SnapshotActor`、L813 `AuthorizationSnapshot`、L789 `DeviceAdminView`、L4435-4438 `GrantRecord` 及既有 fixture 形状 L4430-4441），零 DB、零 tx、零新 trait。**混合批次测试说明（RT-N6，B1 受门禁真库切片补齐，本切片不伪称已跑）**：同一提交批次内 `Permitted` 目标正常 `queued` 并持设备锁、`VisibleForbidden` 目标不持锁直接落 `failed`（`outcome='failed'`、`reason_code='PERMISSION_DENIED'`）且计入 counts、含任一 `NotVisible` 目标则整体 404 回滚零残留；纯策略单测继续钉死 `Permitted` 布尔投影（task_read-only → `false` 即 `VisibleForbidden` 非 `Permitted`），三态区分本身由上述真库切片断言：

```rust
// service.rs #[cfg(test)] mod policy_tests —— 纯策略 fixture，无 DB、无 tx
use super::{snapshot_allows_task_creation, SubmitError};
use crate::authz::policy::{
    AuthorizationSnapshot, GrantRecord, GrantScope, GrantSource, Permission, SnapshotActor,
};
use crate::devices::repository::DeviceAdminView;
use crate::error::ControllerError;
use crate::model::{AdmissionState, ReviewDecision};
use std::collections::HashMap;
use uuid::Uuid;

const POLICY_DEVICE: [u8; 32] = [3u8; 32];
const ACTIVE_USER: SnapshotActor =
    SnapshotActor { id: [2u8; 16], active: true, is_admin: false, must_change_password: false };

fn policy_device_fixture(id: [u8; 32], archived: bool) -> DeviceAdminView {
    DeviceAdminView {
        public_key: id,
        display_name: "policy-fixture".into(),
        admission_state: AdmissionState::Approved,
        review_decision: ReviewDecision::Approved,
        revision: 1,
        descriptor: None,
        archived,
    }
}

fn policy_snapshot(
    actor: SnapshotActor,
    grants: Vec<GrantRecord>,
    devices: Vec<([u8; 32], bool)>,
) -> AuthorizationSnapshot {
    AuthorizationSnapshot {
        actor,
        authz_epoch: 1,
        grants,
        roles: HashMap::new(),
        device_groups: HashMap::new(),
        devices: devices
            .into_iter()
            .map(|(id, archived)| (id, policy_device_fixture(id, archived)))
            .collect(),
    }
}

fn policy_direct_grant(user_id: [u8; 16], permissions: Vec<Permission>) -> GrantRecord {
    GrantRecord {
        id: Uuid::new_v4(),
        user_id,
        source: GrantSource::Direct(permissions),
        scope: GrantScope::All,
        revision: 1,
    }
}

#[test]
fn snapshot_allows_task_creation_accepts_reboot_only_grant() {
    let snapshot = policy_snapshot(
        ACTIVE_USER,
        vec![policy_direct_grant(ACTIVE_USER.id, vec![Permission::DeviceReboot])],
        vec![(POLICY_DEVICE, false)],
    );
    assert!(snapshot_allows_task_creation(&snapshot, POLICY_DEVICE));
}

#[test]
fn snapshot_allows_task_creation_rejects_task_read_only_grant() {
    let snapshot = policy_snapshot(
        ACTIVE_USER,
        vec![policy_direct_grant(ACTIVE_USER.id, vec![Permission::DeviceTaskRead])],
        vec![(POLICY_DEVICE, false)],
    );
    assert!(!snapshot_allows_task_creation(&snapshot, POLICY_DEVICE));
}

#[test]
fn snapshot_allows_task_creation_rejects_inactive_or_must_change_actor() {
    let grants = vec![policy_direct_grant(ACTIVE_USER.id, vec![Permission::DeviceReboot])];
    for actor in [
        SnapshotActor { active: false, ..ACTIVE_USER },
        SnapshotActor { must_change_password: true, ..ACTIVE_USER },
    ] {
        let snapshot = policy_snapshot(actor, grants.clone(), vec![(POLICY_DEVICE, false)]);
        assert!(!snapshot_allows_task_creation(&snapshot, POLICY_DEVICE));
    }
}

#[test]
fn snapshot_allows_task_creation_rejects_missing_or_archived_device() {
    let grants = vec![policy_direct_grant(ACTIVE_USER.id, vec![Permission::DeviceReboot])];
    let missing = policy_snapshot(ACTIVE_USER, grants.clone(), vec![]);
    let archived = policy_snapshot(ACTIVE_USER, grants, vec![(POLICY_DEVICE, true)]);
    assert!(!snapshot_allows_task_creation(&missing, POLICY_DEVICE));
    assert!(!snapshot_allows_task_creation(&archived, POLICY_DEVICE));
}

#[test]
fn submit_error_backend_maps_through_to_same_controller_error() {
    // 错误保真回归（非新策略）：From<SubmitError> for ControllerError 的 Backend(err) => err 原样透传。
    // actor 行缺失在 Identity 真源是 PermissionDenied（403），不得重映 503 或降为 Ok(false)/400；
    // Config（503 STORAGE_UNAVAILABLE）保持原变体。本测试不覆盖真实异步 DB 路径。
    let denied = ControllerError::from(SubmitError::Backend(ControllerError::PermissionDenied));
    assert!(matches!(denied, ControllerError::PermissionDenied));
    let config = ControllerError::from(SubmitError::Backend(ControllerError::Config(
        "policy-fixture".into(),
    )));
    assert!(matches!(config, ControllerError::Config(_)));
}
```

**selector 与 RED/GREEN 口径**：纯策略 selector 为 `cargo test -p rsetup-controller tasks::service::policy_tests::snapshot_allows_task_creation`（4 用例，模块路径 `tasks::service::policy_tests`）；错误保真 selector 为 `tasks::service::policy_tests::submit_error_backend_maps_through_to_same_controller_error`。未来 RED 采用可编译行为策略：若 `check_target_authz` 被写成额外把 `Permission::DeviceTaskRead` 判为 `TargetAuthz::Permitted` 的错误桩（`|| contains(&Permission::DeviceTaskRead)` 风格），`rejects_task_read_only_grant` 经薄包装因断言失败而 RED（行为 RED，非编译错误）；错误保真用例是对本计划既有 `From` 实现的**回归断言**，不预期此前 RED。两类测试**均不替代** `device_permitted` 的真实 SQL 门禁测试：`load_snapshot_in_tx` 的真库路径（固定读序、锁定行读、actor 行缺失→`PermissionDenied`、解码污染→`Config` 503）fixture 仍缺，留待本任务 B1 受门禁真库切片补齐，本切片不伪称已跑或已通过。`lock_authz_rows_in_order` 的锁序口径以**纯断言辅助说明**钉死而非 mock SQL：锁序由代码结构本身保证——三段固定调用顺序（roles → device_groups → devices）+ `BTreeSet<Vec<u8>>` 自然序（字节升序 + 去重）+ 空集合安全跳过，纯单测只能断言 helper 签名稳定与集合规范化次序（对 `BTreeSet` 迭代序的断言），无法在零 DB 环境模拟 MySQL 行锁；**真库并发死锁/倒挂验证归属 Task 14 集成门禁**，本切片不伪称已跑。

**B2 submit 错误边界（前置依赖，显式内部错误变体，HTTP 契约码不变）：** `TaskFuture` 与 `TaskApi::submit` 保持账本固定签名 `Result<SubmitOutcome, ControllerError>` 不变，服务层 `TaskService::submit` 保持 `Result<SubmitOutcome, SubmitError>` 不变。`IdempotencyConflict`（409 `IDEMPOTENCY_CONFLICT`）与 `PreviewExpired`/`PreviewConsumed`（409 `INVALID_STATE`）在既有 `ControllerError` 无对应变体，映射到既有变体会丢失契约分支。**前置 owner**：共享 `error.rs` owner 先行 Modify `crates/rsetup-controller/src/error.rs`，为 `ControllerError` 添加两个单位变体（本计划本任务不修改 error.rs；前置未落地时相关切片标 BLOCKED 等待前置，不得自行添加）：

```rust
#[error("task idempotency conflict")]
TaskIdempotencyConflict,
#[error("task preview invalid state")]
TaskPreviewInvalidState,
```

`service.rs` 与 `SubmitError` 同段落落地完整双向转换（不得省略分支；`From<ControllerError>` 保证服务内仓储/DB 调用（如 `self.repo.begin().await?`）的 `?` 闭合且不吞掉存储错误；`From<sqlx::Error>` 显式包成 `Backend(ControllerError::Database(err))`，使 rollback/commit 的 `await?` 直接闭合——`?` 不会自动连链两次 `From`——并保留 503 存储语义，既有 typed 边界与 HTTP 409 映射不变）：

```rust
impl From<ControllerError> for SubmitError {
    fn from(err: ControllerError) -> Self { SubmitError::Backend(err) }
}
impl From<sqlx::Error> for SubmitError {
    fn from(err: sqlx::Error) -> Self { SubmitError::Backend(ControllerError::Database(err)) }
}
impl From<SubmitError> for ControllerError {
    fn from(err: SubmitError) -> Self {
        match err {
            SubmitError::InvalidArgument => ControllerError::InvalidArgument,
            SubmitError::ActorInactive => ControllerError::PermissionDenied,
            SubmitError::DeviceBusyCount { .. } => ControllerError::InvalidArgument,
            SubmitError::PreviewExpired | SubmitError::PreviewConsumed => ControllerError::TaskPreviewInvalidState,
            SubmitError::IdempotencyConflict => ControllerError::TaskIdempotencyConflict,
            SubmitError::NotVisible => ControllerError::NotFound,
            SubmitError::Backend(err) => err,
        }
    }
}
```

`DeviceBusyCount` 在固定契约下**不用于整体 busy 失败**——忙目标创建为 `failed`/`DEVICE_BUSY` 子任务（上文第 7 步，"目标 busy 不整体报错"），该映射仅为防御兜底（400），不得映射为 409。`TaskApi` adapter（生产装配 `TaskService<SqlxTaskRepository>`）的 submit 实现须在边界显式转换，不得以 `?` 直接传播 `SubmitError`：

```rust
let outcome = TaskService::submit(self, req, ctx).await.map_err(ControllerError::from)?;
```

`TaskService::time_review`：在短事务内锁定该主任务的**全部** `held` 子任务；若为空返回 `InvalidArgument`（HTTP 400）；RT-N5：分派以**服务端真实时间比对**为准，决不接受客户端自报 `kind` 枚举作为分派依据（`kind` 仅记审计）——取 `self.time_gate.current().await` 当前时钟快照，`quality` 非 `TimeQuality::NtpValid`（Stale/SystemFallback）时无法证明原期限有效性，回滚并保持 `held`、返回 `TimeReviewOutcome::Inconclusive`（由管理员显式 `cancel`（`USER_CANCELLED`）或终止 `failed`（`TIME_UNCERTAIN`）另行处置）；`quality` 为 `NtpValid` 时逐子任务真实比对 `observe_deadline` 与 `reference_utc`（缺失 `observe_deadline` 无法证明未过期，按已过期处置），管理员 `decision` 必须与比对结果一致——`ResumeOriginalDeadline` 要求全部未过期、`Expire` 要求全部已过期，任一矛盾回滚返回 `InvalidArgument`（HTTP 400）拒绝矛盾请求；一致时 Resume → `held → queued`（恢复**原** `observe_deadline`，不写入新期限），Expire → `expired` + `QUEUE_EXPIRED` 并释放设备锁。任何路径都**不修改** `created_at` 与 `observe_deadline` 为更晚值。

`TaskService::release_lock`：校验 `is_admin`、`expected_revision == 当前 revision`、`acknowledge_unknown == true`、`reason.trim()` 非空；在一个事务内把 `device_operation_locks` 该行删除（`WHERE device_id = ? AND owner_sub_task_id = ? AND generation = ?`，匹配失败返回 `RevisionConflict`）并写审计；子任务**保持** `unknown`，`revision` 递增；**绝不**触碰该设备后续任务新 generation 的锁。

上述三段编排对应的 Rust 结构实现（参数绑定顺序须与上文逐条一致；审计为 Step 2 定义的 11 字段脱敏集合）。先给出服务层类型与事务 helper 的完整定义（Task 6 同名消费），并固定以下四条与 sqlx 0.8.6 实际 API 对齐的约束：

- `params!` **不是**任何 sqlx 宏的薄包装：sqlx 0.8.6 未导出 `args!`（`sqlx::args!` 不存在）。宏体在本地以 `MySqlArguments::default()` 起始，对每个实参调用一次 `sqlx::Arguments::add(&mut args, &$e)`，位置顺序与 SQL `?` 一一对应。
- 每个 `add` 返回的 `BoxDynError` 一律 `map_err` 成 `ControllerError::Database(sqlx::Error::Encode(err))` 后用 `?` 上抛：不 `unwrap`、不吞错。宏内错误类型固定为 `ControllerError`，因此在返回 `Result<_, SubmitError>` 的 submit 体内展开时，`?` 由 B2 既有 `From<ControllerError> for SubmitError` 闭合，本任务不新增任何转换。取 `&$e` 而非移动实参，是为了让 `req.idempotency_key: String` 这类非 `Copy` 字段仍可作位置参数（依赖 sqlx 对 `&T` 的 `Encode`/`Type` 实现）。
- 事务 helper 一律显式接收 `tx: &mut Transaction<'_, MySql>`，executor 一律传 `&mut **tx`，使"同一短事务"可静态核验：sqlx 0.8.6 已删除 `Executor for &mut Transaction` 实现，只有 `&mut MySqlConnection` 实现 `Executor`，`Transaction` 需再经一层 `DerefMut<Target = MySqlConnection>`（`&mut *tx` 得到的是 `&mut Transaction`，不满足 bound）。
- `tx_query`/`tx_query_one` 用 `query_as_with::<MySql, T, _>`（`query_with` 只产出 `MySqlRow`，无法映射为 `T`），`T` 保留 `for<'r> FromRow<'r, MySqlRow> + Send + Unpin` 以同时满足 `fetch_all`/`fetch_optional` 的 bound；`tx_exists` 用 `query_scalar_with::<MySql, i64, _>`（标量 `i64` 不实现 `FromRow`，不能走 `tx_query_one::<i64>`）。**仍未闭合**：`[u8; 32]` 二进制入参在 sqlx-mysql 0.8.6 无 `Type`/`Encode` 实现，本任务不引入自定义泛型 binary 转换 trait，二进制入参转换留待后续独立修订，本节不宣称 B1 整体闭合。

```rust
use sqlx::{mysql::{MySql, MySqlArguments, MySqlRow}, FromRow, Transaction};
use chrono::{DateTime, Utc};
use uuid::Uuid;
use crate::ControllerError;
use crate::tasks::preview::{canonical_device_order, decode_preview_targets, preview_generation_bytes, preview_mono_valid};
#[derive(Debug, Clone)]
pub struct SubmitRequest {
    pub idempotency_key: String, pub request_hash: [u8; 32], pub target_hash: [u8; 32],
    pub preview_token_hash: [u8; 32], pub preview_token_nonce: [u8; 32], pub counts_json: String,
    pub observe_deadline: DateTime<Utc>, pub time_evidence: crate::time::evidence::TimeEvidence,
    pub target: TaskTarget,
}
#[derive(Debug, Clone)]
pub struct TaskTarget { pub device_ids: Vec<[u8; 32]>, pub group_ids: Vec<Uuid> } // canonical_device_order() = 升序字节序 + 去重
impl TaskTarget { pub fn canonical_device_order(&self) -> Vec<[u8; 32]> { let mut ids = self.device_ids.clone(); canonical_device_order(&mut ids); ids } }
#[derive(Debug, Clone)]
pub struct AuthCtx { pub user_id: Uuid, pub is_admin: bool } // HTTP 层由 auth::service::Session 产出快照，服务层事务内重验 actor 行
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeReviewDecision { ResumeOriginalDeadline, Expire } // 与 Task 6 同名同形，此处单一定义
#[derive(Debug)]
pub enum TimeReviewOutcome { Completed { affected: usize }, Inconclusive } // Inconclusive：服务端时钟不可信（非 NtpValid），无法证明期限有效性，全部保持 held、零状态变更（HTTP 层仍返回当前 MainTask 视图）
#[derive(Debug)]
pub enum TimeReviewError { InvalidArgument, PermissionDenied, RevisionConflict, Backend(ControllerError) }
#[derive(Debug)]
pub enum ReleaseLockOutcome { Released { revision: u64 } }
#[derive(Debug)]
pub enum ReleaseLockError { InvalidArgument, PermissionDenied, UnknownNotAcknowledged, SubTaskNotFound, RevisionConflict, Backend(ControllerError) }
macro_rules! params {
    ($($e:expr),* $(,)?) => {{
        let mut args = MySqlArguments::default();
        $(
            // 位置绑定顺序 = 宏实参顺序 = SQL `?` 顺序；编码失败立即上抛，不 unwrap、不吞错
            sqlx::Arguments::add(&mut args, &$e)
                .map_err(|err| ControllerError::Database(sqlx::Error::Encode(err)))?;
        )*
        args
    }};
}
#[derive(Debug, FromRow)] pub struct ActorRow { pub active: bool, pub is_admin: bool }
// sqlx-mysql 0.8.6 无 [u8; N] 的 Type/Encode/Decode：本步 submit/time_review/release_lock 相关行结构的 BINARY(32) 列
// 以 Vec<u8> 承接，读出后由 fixed_mysql_bytes 显式校验长度恢复固定数组再使用。
#[derive(Debug, FromRow)] struct IdempotencyRow { id: Uuid, request_hash: Vec<u8>, target_hash: Vec<u8> }
#[derive(Debug, FromRow)] struct HeldSubRow { id: Uuid, device_id: Vec<u8>, lock_generation: u64, queue_deadline: DateTime<Utc>, observe_deadline: Option<DateTime<Utc>> }
#[derive(Debug, FromRow)] struct RevisionRow { revision: u64 }
// state 以 String 承接：SubTaskState 未实现 sqlx::Type/Decode，不得盲 derive 强解数据库枚举；
// release_lock 在 CAS 状态判断之前经 parse_subtask_state 真实解析（非法值 → Database Decode 503）。
#[derive(Debug, FromRow)] struct SubLockRow { revision: u64, state: String, reason_code: Option<String>, lock_generation: u64 }
#[derive(Debug, FromRow)] struct EpochRow { authz_epoch: u64 }
/// MySQL 固定宽列（BINARY(16)/BINARY(32)）读出后的长度校验：长度不符即 DB 损坏，
/// 返回 `ControllerError::Database`（HTTP 503），不 truncate、不 unwrap/panic、不返回 400；
/// 消息去敏，不含任何实际列值。
fn fixed_mysql_bytes<const N: usize>(raw: &[u8]) -> Result<[u8; N], ControllerError> {
    raw.try_into().map_err(|_| {
        ControllerError::Database(sqlx::Error::Decode(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "fixed-width binary column has unexpected length",
        ))))
    })
}
/// 以既有 serde 定义（snake_case，Task 2 唯一真相源）把 sub_tasks.state 的 VARCHAR 串解析为 SubTaskState。
/// 非法值视为 DB 损坏返回 `ControllerError::Database`（HTTP 503），消息去敏、不含原始列值；
/// 绝不把非法值当合法 Unknown 放行，也不返回 400。
fn parse_subtask_state(raw: &str) -> Result<SubTaskState, ControllerError> {
    serde_json::from_value::<SubTaskState>(serde_json::Value::String(raw.to_owned())).map_err(|_| {
        ControllerError::Database(sqlx::Error::Decode(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "sub_tasks.state has an unrecognized value",
        ))))
    })
}
/// `main_tasks.counts_json` 的**唯一生产者**：10 键子任务状态频数对象，键集合与 spec 04 十态、
/// Web `SUBTASK_STATES`、Task 6 `project_main_task_summary` 同源。每个值一律是 u64 的十进制
/// **字符串**，不写 JSON number、不写 null（JSON number 超过 2^53-1 会被 JS `Number` 丢精度）；
/// 不计算各键总和（无 overflow 面），不依赖键序（serde_json 默认 BTreeMap 升序输出）。
/// 入参只接受当次事务内**真实子任务分类计数**（第 7 步 `locked`/`device_busy`），
/// 绝不接受 `req.counts_json` 之类的客户端自报值。
fn submit_counts_json(locked: u64, device_busy: u64) -> String {
    serde_json::json!({
        "queued": locked.to_string(), "held": "0", "dispatching": "0", "accepted": "0",
        "verifying": "0", "succeeded": "0", "failed": device_busy.to_string(), "unknown": "0",
        "cancelled": "0", "expired": "0",
    })
    .to_string() // `Value: Display` 恒成功，不 unwrap 任何序列化 Result
}
/// 主任务 INSERT 阶段的 10 键全零占位（第 9 步按真实结果回写覆盖）；复用同一生产者，
/// 保证占位与终值的键集合恒等。
fn empty_counts_json() -> String { submit_counts_json(0, 0) }
/// 锁内长度校验通过后的固定快照；下游有效性比较与持久化只使用该固定版。
#[derive(Debug)]
struct PreviewSnapshotFixed { target_hash: [u8; 32], targets_json: String, preview_generation: [u8; 16] }
impl<R: TaskRepository> TaskService<R> {
    // 以下 helper 全部复用调用方传入的同一 Transaction：executor 取 `&mut **tx`
    // （`&mut Transaction` 经 `Transaction: DerefMut<Target = MySqlConnection>` 落到连接，
    // 只有 `&mut MySqlConnection` 实现 `Executor`）；任一 `sqlx::Error` 由 `?` 原样转成
    // `ControllerError::Database`，不吞错、不改写、不重试。
    async fn tx_execute(&self, tx: &mut Transaction<'_, MySql>, sql: &str, params: MySqlArguments) -> Result<u64, ControllerError> {
        let result = sqlx::query_with::<MySql, _>(sql, params).execute(&mut **tx).await?;
        Ok(result.rows_affected())
    }
    async fn tx_query<T>(&self, tx: &mut Transaction<'_, MySql>, sql: &str, params: MySqlArguments) -> Result<Vec<T>, ControllerError>
    where T: for<'r> FromRow<'r, MySqlRow> + Send + Unpin {
        Ok(sqlx::query_as_with::<MySql, T, _>(sql, params).fetch_all(&mut **tx).await?)
    }
    async fn tx_query_one<T>(&self, tx: &mut Transaction<'_, MySql>, sql: &str, params: MySqlArguments) -> Result<Option<T>, ControllerError>
    where T: for<'r> FromRow<'r, MySqlRow> + Send + Unpin {
        Ok(sqlx::query_as_with::<MySql, T, _>(sql, params).fetch_optional(&mut **tx).await?)
    }
    async fn tx_exists(&self, tx: &mut Transaction<'_, MySql>, sql: &str, params: MySqlArguments) -> Result<bool, ControllerError> {
        // 存在性探测取标量列：`i64` 不实现 `FromRow`，只能 `query_scalar_with`；仍走同一 tx。
        Ok(sqlx::query_scalar_with::<MySql, i64, _>(sql, params).fetch_optional(&mut **tx).await?.is_some())
    }
    async fn load_actor_row(&self, tx: &mut Transaction<'_, MySql>, user_id: Uuid) -> Result<ActorRow, ControllerError> {
        self.tx_query_one::<ActorRow>(tx, "SELECT active, is_admin FROM users WHERE id = ?", params![user_id])
            .await?
            .ok_or(ControllerError::NotFound)
    }
    async fn load_authz_epoch(&self, tx: &mut Transaction<'_, MySql>) -> Result<u64, ControllerError> {
        self.tx_query_one::<EpochRow>(tx, "SELECT authz_epoch FROM schema_meta WHERE singleton = 1", params![])
            .await?
            .map(|row| row.authz_epoch)
            .ok_or(ControllerError::NotFound)
    }
    // 审计唯一 producer（账本 §审计 producer，audit.rs 归 Identity 拥有）：`crate::audit::insert_audit(&mut Transaction<'_, MySql>, &AuditInsert)`；
    // 本服务不定义 RedactedAudit/insert_redacted_audit，也不定义 process_epoch()/next_event_seq() 方法。
    // process_epoch 生产者 = `audit::admin_process_epoch()`（OnceLock `ADMIN_PROCESS_EPOCH` 进程启动 `Uuid::new_v4` 一次性取值，第三独立命名空间）；
    // event_seq 递增归属 = `audit::next_admin_event_seq()`（`ADMIN_EVENT_SEQ` AtomicU64 checked 递增，溢出返回 RevisionConflict）；
    // 两者与 time_evidence（`system_fallback_time_evidence`，非空）均由 insert_audit 内部取值随 11 字段落列，调用方不传 epoch/seq/time_evidence。
}
```

`time_review`/`release_lock` 服务体内 `self.repo.begin().await?`、`tx_query(...).await?` 与 audit/rollback/commit 的 `sqlx::Error` 均以 `?` 直接上抛，缺 `From` 即无法编译，也不得吞错或降级为泛型字符串丢失 503 存储语义。仿上文 B2 `SubmitError` 方案：两个 enum 各追加 `Backend(ControllerError)` 透传变体（已并入上方定义），并在 `service.rs` 同段落落地完整双向转换（不得省略分支；`From<sqlx::Error>` 显式包成 `Backend(ControllerError::Database(err))`，保证 rollback/commit 直接 `?` 闭合且保留 503 语义）；`TaskApi` adapter 边界以 `ControllerError::from(err)` 显式转换 typed error（不得 `?` 直接传播 typed error），`Backend(err)` 原样透传，使 `Database` → 503 `STORAGE_UNAVAILABLE` 路径保留。三个 typed error（`SubmitError`/`TimeReviewError`/`ReleaseLockError`）直传 `sqlx::Error` 的转换均已在本计划内给出真实实现，不留后续事项：`SubmitError` 见上文「B2 submit 错误边界」的 `From<sqlx::Error> for SubmitError`，另两个见下方代码块；三者一律包成 `Backend(ControllerError::Database(err))`，不从 `Database` 取 string、不吞 503 `STORAGE_UNAVAILABLE`：

```rust
impl From<ControllerError> for TimeReviewError {
    fn from(err: ControllerError) -> Self { TimeReviewError::Backend(err) }
}
impl From<ControllerError> for ReleaseLockError {
    fn from(err: ControllerError) -> Self { ReleaseLockError::Backend(err) }
}
impl From<sqlx::Error> for TimeReviewError {
    fn from(err: sqlx::Error) -> Self { TimeReviewError::Backend(ControllerError::Database(err)) }
}
impl From<sqlx::Error> for ReleaseLockError {
    fn from(err: sqlx::Error) -> Self { ReleaseLockError::Backend(ControllerError::Database(err)) }
}
impl From<TimeReviewError> for ControllerError {
    fn from(err: TimeReviewError) -> Self {
        match err {
            TimeReviewError::InvalidArgument => ControllerError::InvalidArgument,
            TimeReviewError::PermissionDenied => ControllerError::PermissionDenied,
            TimeReviewError::RevisionConflict => ControllerError::RevisionConflict,
            TimeReviewError::Backend(err) => err,
        }
    }
}
impl From<ReleaseLockError> for ControllerError {
    fn from(err: ReleaseLockError) -> Self {
        match err {
            ReleaseLockError::InvalidArgument => ControllerError::InvalidArgument,
            ReleaseLockError::PermissionDenied => ControllerError::PermissionDenied,
            ReleaseLockError::UnknownNotAcknowledged => ControllerError::InvalidArgument,
            ReleaseLockError::SubTaskNotFound => ControllerError::NotFound,
            ReleaseLockError::RevisionConflict => ControllerError::RevisionConflict,
            ReleaseLockError::Backend(err) => err,
        }
    }
}
```

纯行为单测落在 `service.rs` 新增 `#[cfg(test)] mod task_admin_error_tests`（与既有 `mod tests` 不重复；不连 DB、无敏感值；`ControllerError` 未实现 `PartialEq`/`Eq`，一律 `matches!` 断言，不要求 `Eq`）：

```rust
#[cfg(test)]
mod task_admin_error_tests {
    use super::*;

    #[test]
    fn task_admin_error_business_branches_map_to_expected_controller_errors() {
        assert!(matches!(ControllerError::from(TimeReviewError::InvalidArgument), ControllerError::InvalidArgument));
        assert!(matches!(ControllerError::from(TimeReviewError::PermissionDenied), ControllerError::PermissionDenied));
        assert!(matches!(ControllerError::from(TimeReviewError::RevisionConflict), ControllerError::RevisionConflict));
        assert!(matches!(ControllerError::from(ReleaseLockError::InvalidArgument), ControllerError::InvalidArgument));
        assert!(matches!(ControllerError::from(ReleaseLockError::PermissionDenied), ControllerError::PermissionDenied));
        assert!(matches!(ControllerError::from(ReleaseLockError::UnknownNotAcknowledged), ControllerError::InvalidArgument));
        assert!(matches!(ControllerError::from(ReleaseLockError::SubTaskNotFound), ControllerError::NotFound));
        assert!(matches!(ControllerError::from(ReleaseLockError::RevisionConflict), ControllerError::RevisionConflict));
    }

    #[test]
    fn backend_and_sqlx_conversions_preserve_database_error() {
        let err = ControllerError::from(ReleaseLockError::Backend(ControllerError::Database(sqlx::Error::RowNotFound)));
        assert!(matches!(err, ControllerError::Database(sqlx::Error::RowNotFound)));
        fn inner() -> Result<(), TimeReviewError> {
            Err::<(), sqlx::Error>(sqlx::Error::RowNotFound)?;
            Ok(())
        }
        assert!(matches!(inner().unwrap_err(), TimeReviewError::Backend(ControllerError::Database(sqlx::Error::RowNotFound))));
        let submit_err = SubmitError::from(sqlx::Error::RowNotFound);
        assert!(matches!(submit_err, SubmitError::Backend(ControllerError::Database(sqlx::Error::RowNotFound))));
    }
}
```

两条具名 future 命令（仅命令文本，本切片不执行；`--lib` 前缀取自 lib 内模块路径 `tasks::service::task_admin_error_tests`，与 `use rsetup_controller::tasks::service::SubmitOutcome` 的模块路径一致，不空跑）：

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib tasks::service::task_admin_error_tests::task_admin_error_business_branches_map_to_expected_controller_errors -- --exact
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib tasks::service::task_admin_error_tests::backend_and_sqlx_conversions_preserve_database_error -- --exact
```

counts 的纯函数单测落在 `service.rs` 新增 `#[cfg(test)] mod counts_json_tests`（与 `mod tests`、`mod task_admin_error_tests` 互不重复；不连 DB、不读写任何 static/进程状态；对 helper 返回的 `String` 用 `serde_json::from_str` 真实解析后断言，`as_str()` 比较同时覆盖"键存在"与"值是字符串"两条）：

```rust
#[cfg(test)]
mod counts_json_tests {
    use super::*;
    // counts 的唯一生产者 = 本文件的 submit_counts_json / empty_counts_json；
    // 入参只来自当次事务内真实子任务分类（第 7 步 locked / device_busy 计数），不是 req.counts_json。
    const COUNT_KEYS: [&str; 10] = ["queued", "held", "dispatching", "accepted", "verifying",
        "succeeded", "failed", "unknown", "cancelled", "expired"];

    #[test]
    fn submit_counts_json_emits_exact_ten_keys_as_decimal_strings() {
        let empty = serde_json::from_str::<serde_json::Value>(&empty_counts_json())
            .expect("占位必须是合法 JSON").as_object().expect("占位必须是 object").clone();
        assert_eq!(empty.len(), COUNT_KEYS.len(), "键集合必须恰好 10 个，无缺键、无多余键");
        for key in COUNT_KEYS {
            assert_eq!(empty.get(key).and_then(|v| v.as_str()), Some("0"), "{key} 必须是 \"0\"");
        }
        let live = serde_json::from_str::<serde_json::Value>(&submit_counts_json(7, 3))
            .expect("counts 必须是合法 JSON").as_object().expect("counts 必须是 object").clone();
        assert_eq!(live.len(), COUNT_KEYS.len());
        assert_eq!(live["queued"].as_str(), Some("7"), "queued = 当次锁成功子任务数");
        assert_eq!(live["failed"].as_str(), Some("3"), "failed = 当次 DEVICE_BUSY 目标数");
        for key in COUNT_KEYS {
            if key == "queued" || key == "failed" { continue; }
            assert_eq!(live.get(key).and_then(|v| v.as_str()), Some("0"), "{key} 应为 \"0\"");
        }
    }

    #[test]
    fn submit_counts_json_keeps_u64_max_as_decimal_string_not_json_number() {
        let raw = submit_counts_json(u64::MAX, u64::MAX - 1);
        let live = serde_json::from_str::<serde_json::Value>(&raw)
            .expect("counts 必须是合法 JSON").as_object().expect("counts 必须是 object").clone();
        assert_eq!(live.len(), COUNT_KEYS.len());
        assert_eq!(live["queued"].as_str(), Some(u64::MAX.to_string().as_str()));
        assert_eq!(live["failed"].as_str(), Some((u64::MAX - 1).to_string().as_str()));
        assert!(raw.contains(&format!("\"queued\":\"{}\"", u64::MAX)), "上限必须是带引号字符串");
        assert!(!raw.contains(&format!("\"queued\":{}", u64::MAX)), "上限不得写成 JSON number");
    }
}
```

两条具名 future 命令（仅命令文本，本切片不执行；模块路径同样取自 lib 内 `tasks::service::counts_json_tests`）：

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib tasks::service::counts_json_tests::submit_counts_json_emits_exact_ten_keys_as_decimal_strings -- --exact
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib tasks::service::counts_json_tests::submit_counts_json_keeps_u64_max_as_decimal_string_not_json_number -- --exact
```

```rust
impl<R: TaskRepository> TaskService<R> {
    /// 固定顺序：key 校验 → schema guard → actor 核验 → 持久幂等记录 → 判定并消费 preview
    /// → 读出固定目标清单并逐目标重验当前权限/可见性 → 插入 main_task → 按 canonical_device_order
    /// 逐设备抢锁 → 真实 counts 回写 → 脱敏审计 → COMMIT。
    /// **事务内绝不调用任何 `BoardClient`（零网络）。**
    async fn submit(
        &self,
        req: SubmitRequest,
        ctx: AuthCtx,
    ) -> Result<SubmitOutcome, SubmitError> {
        // 1) 幂等键 canonical UUID v4 校验
        if !is_canonical_uuid_v4_key(&req.idempotency_key) {
            return Err(SubmitError::InvalidArgument);
        }

        // RT-R10-3（04 规格 L19/L93）：进入事务前在事务外只读快照设备在线/能力状态——离线或不支持
        // reboot 的目标在提交期直接落 failed，绝不排队等它上线；缓存读取不进事务、不持行锁。
        let device_snapshot = self.device_cache.snapshot();
        let mut tx = self.repo.begin().await?; // 短事务（pool 为仓储内部句柄，经 begin() 获取）

        // 2) schema 版本 guard（事务内第一条写路径前置）
        lock_schema_guard_for_version(&mut tx, SUPPORTED_SCHEMA_VERSION).await?;

        // 3) 重新读取 actor 行并核验 active（普通用户持合法权限即可提交，不再强制 is_admin）；guard 内读取当前 authz_epoch
        let actor = self.load_actor_row(&mut tx, ctx.user_id).await?;
        if !actor.active {
            tx.rollback().await?;
            return Err(SubmitError::ActorInactive);
        }
        let authz_epoch = self.load_authz_epoch(&mut tx).await?;

        // 4) 同事务先查持久幂等记录（先判幂等、后消费预览，修正“先删预览再判幂等”缺陷）
        match self
            .tx_query_one::<IdempotencyRow>(
                &mut tx,
                "SELECT id, request_hash, target_hash FROM main_tasks \
                 WHERE actor_user_id = ? AND idempotency_key = ?",
                params![ctx.user_id, req.idempotency_key],
            )
            .await?
        {
            Some(row) => {
                // BINARY(32) 列先显式校验长度（DB 损坏 → 503），再进入幂等比较；
                // target_hash 一并校验长度（不留未校验字段），但不参与比较——目标真相源是 preview 快照
                let request_hash = fixed_mysql_bytes::<32>(&row.request_hash)?;
                let _target_hash = fixed_mysql_bytes::<32>(&row.target_hash)?;
                if request_hash == req.request_hash {
                    // 此前已成功提交：回滚退出，零副作用，不再消费预览、不扩大目标
                    tx.rollback().await?;
                    return Ok(SubmitOutcome::Replayed { main_task_id: row.id });
                }
                // 同一 actor、同一幂等键、不同载荷：HTTP 409 IDEMPOTENCY_CONFLICT
                tx.rollback().await?;
                return Err(SubmitError::IdempotencyConflict);
            }
            None => {}
        }

        // 5) 首次提交才判定有效性并消费 preview：先 FOR UPDATE 读出**固定目标快照**（锁住判定窗口），
        //    再以 DELETE 完成原子单次使用（DELETE 仍是唯一消费实现，禁止两段式 UPDATE）
        let snapshot = self
            .tx_query_one::<PreviewSnapshotRow>(
                &mut tx,
                "SELECT target_hash, targets_json, preview_generation, preview_token_hash \
                 FROM task_previews WHERE token_nonce = ? AND actor_user_id = ? FOR UPDATE",
                params![&req.preview_token_nonce[..], ctx.user_id],
            )
            .await?;
        let snapshot = match snapshot {
            // 有效性三重判定（对齐集成账本 T4-04“60s 单调有效”）：进程代际 + 进程内单调 TTL + 墙钟上限兜底。
            // preview_generation_bytes() = 本进程启动 epoch（OnceLock 一次性取值，同时参与 token 摘要输入）；
            // preview_mono_valid() = 创建时登记的 Instant 单调跨度 <= 60s，墙钟回拨不得延长有效期。
            // 固定宽列先经 fixed_mysql_bytes 校验长度恢复 [u8; 32]/[u8; 16]（长度不符 → DB 损坏 503），
            // 再做代际比较；校验通过的快照以 PreviewSnapshotFixed 在锁内交付下游。
            Some(row) => {
                let target_hash = fixed_mysql_bytes::<32>(&row.target_hash)?;
                let preview_generation = fixed_mysql_bytes::<16>(&row.preview_generation)?;
                // RT-N2 绑定核验（B4 端到端闭合）：req.preview_token_hash 是服务层对请求 64hex
                // preview_token 的 SHA256 服务端派生值（绝不采信客户端同名字段），必须与本行
                // preview_token_hash 逐字节比较。比较用 XOR 折叠、无短路、不提前返回、不引入新依赖；
                // 不匹配说明该主键行与所呈 token 非同一 preview（伪造 nonce、跨行串号、派生口径漂移），
                // 与"行不存在/已消费"同投影 409 INVALID_STATE，不泄露差异、不降级为 400。
                let stored_token_hash = fixed_mysql_bytes::<32>(&row.preview_token_hash)?;
                let mut token_hash_diff = 0u8;
                for (a, b) in stored_token_hash.iter().zip(req.preview_token_hash.iter()) {
                    token_hash_diff |= a ^ b;
                }
                if token_hash_diff != 0 {
                    tx.rollback().await?;
                    return Err(SubmitError::PreviewExpired);
                }
                if preview_generation == preview_generation_bytes()
                    && preview_mono_valid(&req.preview_token_nonce)
                {
                    PreviewSnapshotFixed { target_hash, targets_json: row.targets_json, preview_generation }
                } else {
                    tx.rollback().await?;
                    return Err(SubmitError::PreviewExpired); // 跨代际或单调 TTL 到期 → 409 INVALID_STATE
                }
            }
            None => {
                // 先判别再回滚：同 nonce 已落 consumed 行 → Consumed，否则从未存在/已清理 → Expired
                let already = self
                    .tx_exists(
                        &mut tx,
                        "SELECT 1 FROM task_previews \
                         WHERE token_nonce = ? AND actor_user_id = ? \
                           AND consumed_at IS NOT NULL",
                        params![&req.preview_token_nonce[..], ctx.user_id],
                    )
                    .await?;
                tx.rollback().await?;
                return Err(if already {
                    SubmitError::PreviewConsumed
                } else {
                    SubmitError::PreviewExpired
                });
            }
        };
        let consumed = self
            .tx_execute(
                &mut tx,
                "DELETE FROM task_previews \
                 WHERE token_nonce = ? AND actor_user_id = ? \
                   AND consumed_at IS NULL AND expires_at > NOW(6) AND preview_generation = ?",
                params![&req.preview_token_nonce[..], ctx.user_id, &snapshot.preview_generation[..]],
            )
            .await?;
        if consumed == 0 {
            tx.rollback().await?;
            return Err(SubmitError::PreviewConsumed); // 已持该行锁，0 行只可能是并发消费或墙钟上限越界
        }
        // 目标真相源 = 快照固定清单；req.target / req.target_hash / req.counts_json 一律不参与持久化
        let Some(mut targets) = decode_preview_targets(&snapshot.targets_json) else {
            tx.rollback().await?;
            return Err(SubmitError::InvalidArgument); // 快照损坏：整体不创建
        };
        canonical_device_order(&mut targets); // 与 preview 写入同一规范化（升序字节序 + 去重）

        // 6) 写入主任务：target_hash 取快照值；counts_json 先写 10 键全零占位，第 9 步按真实结果回写；
        //    created_time_json 为创建时刻 time_gate 取出的 TimeEvidence 快照，同一事务持久化（RT-R8-2）
        let main_task_id = Uuid::new_v4();
        let created_time = self.time_gate.current().await;
        let created_time_json = serde_json::to_string(&created_time).unwrap_or_else(|_| "{}".to_string());
        self.tx_execute(&mut tx,
            "INSERT INTO main_tasks (id, actor_user_id, idempotency_key, request_hash, \
             target_hash, preview_token_hash, state, outcome, revision, counts_json, \
             created_time_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'queued', NULL, 1, ?, ?, NOW(6), NOW(6))",
            params![
                main_task_id, ctx.user_id, req.idempotency_key, &req.request_hash[..],
                &snapshot.target_hash[..], &req.preview_token_hash[..], empty_counts_json(),
                created_time_json
            ],
        )
        .await?;

        // 7) 目标按 canonical_device_order 顺序逐设备申请锁。取锁顺序固定为
        //    roles(role_id 字节序升序) → device_groups(group_id 字节序升序) → devices(public_key 字节序升序)，
        //    与 Identity 计划锁序及 ordered_user_ids 的字节序 first→second 风格一致，防锁序倒挂；
        //    锁定后逐目标重验当前授权（device_permitted 同 tx 真源快照，返回 TargetAuthz 三态；真源错误透传 Backend）：
        //    NotVisible（完全不可见）→ 整体回滚 404（不创建、不泄露部分结果）；VisibleForbidden（可见但缺 DeviceReboot）→
        //    不申请设备锁、直接创建 failed 子任务（PERMISSION_DENIED）并计入 failed 计数；Permitted → 原流程抢锁排队
        self.lock_authz_rows_in_order(&mut tx, ctx.user_id, &targets).await?;
        let mut device_busy = 0u32; // submit_counts_json 第二参 = 提交期 failed 聚合计数（DEVICE_BUSY、PERMISSION_DENIED、DEVICE_OFFLINE 与 CAPABILITY_UNAVAILABLE 同源计入）（RT-R11-2：词表以 L1194/L1342 获批 ReasonCode 为准）
        let mut locked = 0u32;
        // RT-R10-1：queue_deadline 统一由服务层基于创建时 NTP 参考时基计算（与 time_review 的
        // reference_utc 比对处于 100% 同一可信时基），不再由 SQL DATE_ADD(NOW(6), ...) 依赖 DB 宿主
        // 墙钟生成，消除数据库墙钟漂移导致的排队寿命错配；排队 TTL 与派发后 observe_deadline 仍是独立生命周期。
        // RT-R11-3：reference_utc 是采样时刻而非当前时刻，NtpValid 下须累加创建时样本年龄 sample_age_ms
        // （读 gate 时已按单调锚点动态派生，RT-R8-1），否则样本越旧排队寿命被隐性拉长；样本年龄缺失按 0 处理。
        // RT-R13-1：SystemFallback/Stale 下 reference_utc 已由 current() 实时刷新为当前墙钟 Utc::now()，
        // 直接加 TTL 即可，不重复累加历史 sample_age_ms，否则排队截止被双重放大。
        // 测试说明（RT-R13-1）：断言 NtpValid 创建时 queue_deadline = reference_utc + sample_age_ms + TTL；
        // SystemFallback/Stale 创建时 queue_deadline = reference_utc + TTL，不重复累加历史 age。
        let queue_deadline = match created_time.quality {
            TimeQuality::NtpValid => {
                let created_age = created_time.sample_age_ms
                    .map(|ms| chrono::Duration::milliseconds(ms as i64))
                    .unwrap_or_else(|| chrono::Duration::zero());
                created_time.reference_utc + created_age + chrono::Duration::milliseconds(QUEUE_TTL_MS)
            }
            TimeQuality::SystemFallback | TimeQuality::Stale => {
                created_time.reference_utc + chrono::Duration::milliseconds(QUEUE_TTL_MS)
            }
        };
        for &device_id in &targets {
            match self.device_permitted(&mut tx, &ctx, device_id).await? {
                TargetAuthz::NotVisible => {
                    tx.rollback().await?;
                    return Err(SubmitError::NotVisible); // 404 NOT_FOUND，与“不存在”同投影（仅完全不可见整体回滚）
                }
                TargetAuthz::VisibleForbidden => {
                    // 可见但缺 DeviceReboot：保留固定目标清单，不申请设备锁，直接创建 failed 子任务
                    let denied_sub_task_id = Uuid::new_v4();
                    self.tx_execute(&mut tx,
                        // RT-R7-1：sub_tasks 无 outcome 列（outcome 仅属 main_tasks），失败语义由 state='failed' 承载
                        // RT-R10-1：queue_deadline 直接绑定服务层算好的 NTP 时基值，不再用 DATE_ADD(NOW(6), ...)
                        "INSERT INTO sub_tasks (id, main_task_id, device_id, state, reason_code, \
                         revision, lock_generation, queue_deadline, observe_deadline, created_at, updated_at) \
                         VALUES (?, ?, ?, 'failed', 'PERMISSION_DENIED', 1, 1, ?, ?, NOW(6), NOW(6))",
                        params![denied_sub_task_id, main_task_id, &device_id[..], queue_deadline, req.observe_deadline],
                    )
                    .await?;
                    device_busy += 1;
                    continue;
                }
                // RT-R10-3（04 规格 L19/L93）：权限判定通过后再核验事务外设备状态快照——离线或不支持
                // reboot 能力的目标不申请设备锁、绝不入队等上线，直接落 failed 并计入 device_busy；
                // 仅在线且具备 reboot 能力的 Permitted 目标才进入下方抢锁入队流程。
                TargetAuthz::Permitted => {
                    // RT-R11-1：分支类型统一为 Option<&'static str>（原 &str 与 None 混排不可编译）；
                    // RT-R11-2：无能力原因码用获批词表的 CAPABILITY_UNAVAILABLE（ReasonCode::CapabilityUnavailable）
                    let rejected_reason: Option<&'static str> = match device_snapshot.get(&device_id) {
                        None => Some("DEVICE_OFFLINE"), // 缓存未命中按离线且无能力处理（保守口径）
                        Some(snap) if !snap.online => Some("DEVICE_OFFLINE"),
                        Some(snap) if !snap.reboot_capable => Some("CAPABILITY_UNAVAILABLE"),
                        Some(_) => None,
                    };
                    if let Some(reason_code) = rejected_reason {
                        let rejected_sub_task_id = Uuid::new_v4();
                        self.tx_execute(&mut tx,
                            "INSERT INTO sub_tasks (id, main_task_id, device_id, state, reason_code, \
                             revision, lock_generation, queue_deadline, observe_deadline, created_at, updated_at) \
                             VALUES (?, ?, ?, 'failed', ?, 1, 1, ?, ?, NOW(6), NOW(6))",
                            params![rejected_sub_task_id, main_task_id, &device_id[..], reason_code, queue_deadline, req.observe_deadline],
                        )
                        .await?;
                        device_busy += 1;
                        continue;
                    }
                }
            }
            let sub_task_id = Uuid::new_v4();
            // RT-N3（RT-R10-1 修订）：queue_deadline = 创建时 NTP reference_utc + created_age + QUEUE_TTL_MS，
            // 由服务层统一可信时基计算并作为参数直接绑定，不再取 DB 墙钟（QUEUE_TTL_MS 见调度器段常量）。
            self.tx_execute(&mut tx,
                "INSERT INTO sub_tasks (id, main_task_id, device_id, state, revision, lock_generation, \
                 queue_deadline, observe_deadline, created_at, updated_at) \
                 VALUES (?, ?, ?, 'queued', 1, 1, ?, ?, NOW(6), NOW(6))",
                params![sub_task_id, main_task_id, &device_id[..], queue_deadline, req.observe_deadline],
            )
            .await?;
            let got = self
                .tx_execute(
                    &mut tx,
                    // generation 固定为 1，与子任务 lock_generation 初始值一致
                    "INSERT INTO device_operation_locks \
                     (device_id, main_task_id, owner_sub_task_id, generation, acquired_at) \
                     SELECT ?, ?, ?, 1, NOW(6) FROM DUAL \
                     WHERE NOT EXISTS (\
                        SELECT 1 FROM device_operation_locks WHERE device_id = ?)",
                    params![&device_id[..], main_task_id, sub_task_id, &device_id[..]],
                )
                .await?;
            if got == 0 {
                // 设备忙：该子任务 failed + DEVICE_BUSY，不排队、不等待
                self.tx_execute(&mut tx,
                    "UPDATE sub_tasks \
                     SET state = 'failed', reason_code = 'DEVICE_BUSY', updated_at = NOW(6) \
                     WHERE id = ?",
                    params![sub_task_id],
                )
                .await?;
                device_busy += 1;
            } else {
                locked += 1;
            }
        }

        // 8) 同一事务经唯一 producer 写 11 字段脱敏审计（time_evidence/process_epoch/event_seq 由 insert_audit 内部承载）
        crate::audit::insert_audit(
            &mut tx,
            &crate::audit::AuditInsert {
                actor_kind: "user",
                actor_user_id: Some(ctx.user_id.into_bytes()),
                event_type: "task.submit",
                target_kind: "main_task",
                target_id: Some(main_task_id.to_string()),
                // 脱敏口径：仅存 request_hash 摘要与 authz_epoch；请求 time_evidence 序列化后并入 params_redacted，审计内容不丢失
                params_redacted: &format!(
                    r#"{{"authz_epoch":{},"request_hash":"{}","time_evidence":{}}}"#,
                    authz_epoch,
                    hex::encode(req.request_hash),
                    serde_json::to_string(&req.time_evidence).unwrap_or_else(|_| "null".to_string())
                ),
                outcome: "success",
            },
        )
        .await?;

        // 9) counts 真实写入：按第 7 步实际子任务结果（含 locked/device_busy 分类）回写后才 COMMIT；
        //    废止"先写 req.counts_json 再丢弃"的假持久化路径
        // RT-R8-3 / RT-R9-2：主任务持久汇总随子任务状态原子收敛——无论何种子状态分布，一律以第 7 步
        // 实际子状态分布（locked 个 Queued + device_busy 个 Failed）经 Task 5 唯一聚合器
        // calculate_main_task_outcome 计算主任务 state/outcome 后同事务回写：全 failed →
        // (Completed, Failed)；混合 [Queued, Failed] → (Running, None)（04 规格 L79–83：有子任务
        // 终态即算批次已开始处理）。不得硬编码分支——否则混合批次存储层落 queued 而 HTTP 动态投影
        // （Task 6 project_main_task_summary 同源重算）显示 running，发生分裂
        let counts_json = submit_counts_json(locked as u64, device_busy as u64);
        let mut sub_states = Vec::with_capacity(targets.len());
        for _ in 0..locked { sub_states.push(SubTaskState::Queued); }
        for _ in 0..device_busy { sub_states.push(SubTaskState::Failed); }
        let (main_state, main_outcome) = calculate_main_task_outcome(&sub_states);
        let main_state_str = serde_json::to_value(&main_state).unwrap().as_str().unwrap().to_string();
        let main_outcome_str = main_outcome.map(|o| serde_json::to_value(&o).unwrap().as_str().unwrap().to_string());
        self.tx_execute(
            &mut tx,
            "UPDATE main_tasks SET state = ?, outcome = ?, counts_json = ?, revision = revision + 1, updated_at = NOW(6) WHERE id = ?",
            params![main_state_str, main_outcome_str, counts_json, main_task_id],
        )
        .await?;
        tx.commit().await?;
        Ok(SubmitOutcome::Created { main_task_id })
    }

    /// 短事务内锁定该主任务全部 held 子任务；空 → HTTP 400；
    /// expected_revision 在事务内与主任务当前 revision 做 CAS 重验，不匹配（或主任务缺失）→ 409 REVISION_CONFLICT；
    /// decision 为管理员显式裁决（与 Task 6 `TimeReviewDecision` 同名同形），必须与服务端真实时间比对结果一致；
    /// RT-N5：分派只认 `self.time_gate.current()` 可信时钟与 `observe_deadline` 的比对结果，`kind` 仅记审计、决不参与分派，决不延长截止时间。
    async fn time_review(
        &self,
        main_task_id: Uuid,
        expected_revision: u64,
        decision: TimeReviewDecision,
        kind: TimeReviewEvidenceKind,
        ctx: AuthCtx,
    ) -> Result<TimeReviewOutcome, TimeReviewError> {
        let mut tx = self.repo.begin().await?; // 短事务（pool 为仓储内部句柄）
        lock_schema_guard_for_version(&mut tx, SUPPORTED_SCHEMA_VERSION).await?;

        // 事务内重验有效管理员：权限经 HTTP 层前置，但事务内 actor 若被并发降权/停用须失败关闭
        // （与 submit/cancel 的事务内重验一致；guard 已锁，actor 行读不再依赖 HTTP 层快照）
        let actor = self.load_actor_row(&mut tx, ctx.user_id).await?;
        if !actor.active || !actor.is_admin {
            tx.rollback().await?;
            return Err(TimeReviewError::PermissionDenied); // HTTP 403 PERMISSION_DENIED
        }

        // 一次锁定全部 held 子任务（FOR UPDATE），防止并发分派
        let held = self
            .tx_query::<HeldSubRow>(
                &mut tx,
                "SELECT id, device_id, lock_generation, queue_deadline, observe_deadline FROM sub_tasks \
                 WHERE main_task_id = ? AND state = 'held' \
                 ORDER BY id FOR UPDATE",
                params![main_task_id],
            )
            .await?;
        if held.is_empty() {
            tx.rollback().await?;
            return Err(TimeReviewError::InvalidArgument); // HTTP 400
        }

        // expected_revision CAS：事务内锁定主任务行并重验当前 revision（缺失按不匹配处理）
        let main_rev = self
            .tx_query::<RevisionRow>(
                &mut tx,
                "SELECT revision FROM main_tasks WHERE id = ? FOR UPDATE",
                params![main_task_id],
            )
            .await?
            .into_iter()
            .next();
        if main_rev.map(|r| r.revision) != Some(expected_revision) {
            tx.rollback().await?;
            return Err(TimeReviewError::RevisionConflict); // 新增变体，HTTP 409 REVISION_CONFLICT
        }
        // decision 记审计意图（审计契约要求真实写入审计事件，见下方 params_redacted）；
        // RT-N5：分派不认客户端自报 kind，只认服务端可信时钟与 queue_deadline 的真实时间比对（04 规格 L152）
        let decision_str = match decision {
            TimeReviewDecision::ResumeOriginalDeadline => "resume_original_deadline",
            TimeReviewDecision::Expire => "expire",
        };

        // 服务端真实时间判定（service.rs 增 `use crate::time::evidence::TimeQuality;`）：
        // Stale/SystemFallback 不可信，无法证明原期限有效性 → 回滚保持 held，返回 Inconclusive（零状态变更）
        let now_evidence = self.time_gate.current().await;
        if !matches!(now_evidence.quality, TimeQuality::NtpValid) {
            tx.rollback().await?;
            return Ok(TimeReviewOutcome::Inconclusive);
        }
        // 可信时钟：逐子任务真实比对 queue_deadline 与推进后的当前参考时刻（RT-R7-2）——held 子任务尚未派发，
        // 排队寿命以 queue_deadline（QUEUE_TTL_MS 排队 TTL）判定，不得混用派发后的 observe_deadline 观察窗：
        // RT-R7-3：reference_utc 是静态 NTP 采样时刻（driver 每 60s 复验刷新），直接当作当前时刻与
        // queue_deadline 比较，会把 采样时刻 < queue_deadline < 真实当前时刻 的已过期任务误判为未过期；
        // 必须以样本年龄（单调耗时）推进当前参考时刻后再比对，杜绝已超时任务被错误恢复：
        let sample_age = now_evidence
            .sample_age_ms
            .map(|ms| chrono::Duration::milliseconds(ms as i64))
            .unwrap_or_else(|| chrono::Duration::zero());
        let now_utc = now_evidence.reference_utc + sample_age;
        // RT-R8-2（04 规格 L152）：创建时时钟证据核验——queue_deadline 由创建时 DB 墙钟算出，若创建时墙钟
        // 偏快（如快一天）则实际物理排队寿命已过而 queue_deadline 未到；且跳钟/进程重启换代后
        // now_evidence.clock_epoch != 主任务创建时 clock_epoch，原期限参考系连续性无法证明。故：
        // 创建时时钟纪元与当前纪元不一致，或（Resume 决策且存在 now_utc > s.queue_deadline）时，一律
        // tx.rollback() 并返回 Ok(TimeReviewOutcome::Inconclusive) 保持 held、零状态变更，绝不盲目恢复；
        // 过期处置仍由 Expire 决策按真实比对承接，管理员另按显式 cancel/终止 failed 处置（同非 NtpValid 路径）。
        // 契约：main_tasks 增 created_time_json 列（JSON NOT NULL，submit INSERT 同事务写入创建时
        // TimeEvidence，DDL 与 submit 写入随本契约同批落地），time_review 事务内读取核验。
        // 测试说明：time_review_inconclusive_on_clock_epoch_change——注入创建时纪元与当前纪元不一致
        // （或 Resume 且 now_utc 已越过 queue_deadline），断言回滚后 held 状态与主任务 revision 零变更、
        // 返回 Inconclusive；RED 预期：无核验时任务被错误恢复为 queued。
        // RT-R9-1（04 规格 L152）补充：time_review_inconclusive_on_created_time_quality——注入创建时
        // created_time_json.quality == "system_fallback"（创建当时时钟不可信，纪元虽连续），原排队期限
        // 同样无法证明有效，断言同样 tx.rollback() 保持 held、返回 Inconclusive，绝不盲目恢复。
        #[derive(Debug, FromRow)]
        struct CreatedTimeRow { created_time_json: serde_json::Value }
        let created_row = self
            .tx_query::<CreatedTimeRow>(
                &mut tx,
                "SELECT created_time_json FROM main_tasks WHERE id = ?",
                params![main_task_id],
            )
            .await?
            .into_iter()
            .next();
        let created_epoch = created_row.as_ref().and_then(|r| r.created_time_json.get("clock_epoch")
            .and_then(|v| v.as_str()).map(str::to_owned));
        // RT-R9-1：时间核验必须比较创建时间质量——创建时 quality != "ntp_valid"（如当时处于 Stale/
        // SystemFallback）则原排队期限无法证明有效
        let created_quality = created_row.as_ref().and_then(|r| r.created_time_json.get("quality")
            .and_then(|v| v.as_str()).map(str::to_owned));
        let created_valid = created_quality.as_deref() == Some("ntp_valid");
        let epoch_continuous =
            created_epoch.as_deref() == Some(now_evidence.clock_epoch.to_string().as_str());
        if !created_valid || !epoch_continuous
            || (matches!(decision, TimeReviewDecision::ResumeOriginalDeadline)
                && held.iter().any(|s| now_utc > s.queue_deadline))
        {
            tx.rollback().await?;
            return Ok(TimeReviewOutcome::Inconclusive);
        }
        // now_utc > s.queue_deadline 判定排队已过期，now_utc <= s.queue_deadline 判定排队未过期；
        // decision 必须与比对结果一致（Resume 要求全部排队未过期、Expire 要求全部排队已过期），任一矛盾 → 400 拒绝矛盾请求
        let all_expired = held.iter().all(|s| now_utc > s.queue_deadline);
        let all_live = held.iter().all(|s| now_utc <= s.queue_deadline);
        let expire = match decision {
            TimeReviewDecision::Expire if all_expired => true,
            TimeReviewDecision::ResumeOriginalDeadline if all_live => false,
            _ => {
                tx.rollback().await?;
                return Err(TimeReviewError::InvalidArgument); // HTTP 400：decision 与服务端时间比对矛盾
            }
        };

        // 延长量固定为 0：Resume 只恢复原 observe_deadline；Expire 判过期并释放锁；绝不写入更晚期限
        for sub in &held {
            // checked 校验恢复 domain [u8; 32]（长度不符 → DB 损坏 503，不吞、不截断、不 panic），
            // 下游 SQL 绑定一律用 &device_id[..] slice（sqlx-mysql 无 [u8; N] 的 Encode）
            let device_id = fixed_mysql_bytes::<32>(&sub.device_id)?;
            if !expire {
                // 服务端比对未过期且 decision=Resume：held → queued，恢复**原** observe_deadline（不写入新期限）、清除 reason_code、递增 revision
                self.tx_execute(&mut tx,
                    "UPDATE sub_tasks SET state = 'queued', reason_code = NULL, revision = revision + 1, updated_at = NOW(6) \
                     WHERE id = ?",
                    params![sub.id],
                )
                .await?;
            } else {
                // 服务端比对已过期且 decision=Expire：expired + QUEUE_EXPIRED、递增 revision（kind 自报不参与分派）
                self.tx_execute(&mut tx,
                    "UPDATE sub_tasks \
                     SET state = 'expired', reason_code = 'QUEUE_EXPIRED', revision = revision + 1, updated_at = NOW(6) \
                     WHERE id = ?",
                    params![sub.id],
                )
                .await?;
                // 释放该子任务持有的设备锁（删除其实际 generation 的锁行）
                self.tx_execute(&mut tx,
                    "DELETE FROM device_operation_locks \
                     WHERE device_id = ? AND owner_sub_task_id = ? AND generation = ?",
                    params![&device_id[..], sub.id, sub.lock_generation],
                )
                .await?;
            }
            // 时钟不可信路径已在循环前回滚返回 Inconclusive（保持 held）；
            // 任何路径都不修改 created_at 与 observe_deadline 为更晚值。
        }

        // 随子任务变更推进主任务自身 revision：返回的授权视图与后续 CAS 反映变更后基线
        // （与子任务 revision 递增同事务同提交，事务回滚则一并回滚）
        // RT-R8-3：主任务持久汇总与子任务变更同事务原子更新——Resume/Expire 改写子任务后，
        // 仅递增 revision 会让存储层主行 state/outcome/counts_json 严重陈旧。在同一事务内重查
        // 该主任务全部子任务最新状态，重算十态 counts 与 (state, outcome)，连同 revision 一条
        // UPDATE 回写（同事务同提交，事务回滚一并回滚）。calculate_main_task_outcome 为
        // Task 5 recovery.rs 纯 helper 唯一状态机：本处只消费、不复制第二套转移规则
        // （Task 4 先于 Task 5 执行时须先将其作为前置纯函数交付）
        #[derive(Debug, FromRow)]
        struct MainSubStateRow { state: String }
        let sub_rows = self.tx_query::<MainSubStateRow>(
            &mut tx,
            "SELECT state FROM sub_tasks WHERE main_task_id = ?",
            params![main_task_id],
        )
        .await?;
        let sub_states: Vec<SubTaskState> = sub_rows
            .iter()
            .map(|r| {
                SubTaskState::from_str_exact(&r.state).ok_or_else(|| {
                    TimeReviewError::Backend(ControllerError::Database(sqlx::Error::Decode(Box::new(
                        std::io::Error::new(std::io::ErrorKind::InvalidData, "sub_tasks.state word invalid"),
                    ))))
                })
            })
            .collect::<Result<Vec<_>, TimeReviewError>>()?;
        let (agg_state, agg_outcome) = calculate_main_task_outcome(&sub_states);
        // 十态频数 counts 与 submit_counts_json / Task 6 project_main_task_summary 十键
        // 十进制字符串口径同源；enum → DB 词与 Task 2 serde snake_case 词表同源（unit enum
        // 序列化不可能失败）
        let count = |s: SubTaskState| sub_states.iter().filter(|x| **x == s).count().to_string();
        let counts_json = serde_json::json!({
            "queued": count(SubTaskState::Queued), "held": count(SubTaskState::Held),
            "dispatching": count(SubTaskState::Dispatching), "accepted": count(SubTaskState::Accepted),
            "verifying": count(SubTaskState::Verifying), "succeeded": count(SubTaskState::Succeeded),
            "failed": count(SubTaskState::Failed), "unknown": count(SubTaskState::Unknown),
            "cancelled": count(SubTaskState::Cancelled), "expired": count(SubTaskState::Expired),
        })
        .to_string();
        let state_word = serde_json::to_string(&agg_state)
            .expect("MainTaskState serde is infallible")
            .trim_matches('"')
            .to_owned();
        let outcome_word = agg_outcome.map(|o| {
            serde_json::to_string(&o)
                .expect("MainTaskOutcome serde is infallible")
                .trim_matches('"')
                .to_owned()
        });
        self.tx_execute(&mut tx,
            "UPDATE main_tasks SET state = ?, outcome = ?, counts_json = ?, revision = revision + 1, updated_at = NOW(6) WHERE id = ?",
            params![state_word, outcome_word, counts_json, main_task_id],
        )
        .await?;

        let authz_epoch = self.load_authz_epoch(&mut tx).await?;
        // 兑现 decision 审计契约：decision/evidence_kind 真实写入脱敏参数（列 time_evidence 由 producer 承载，请求证据 kind 语义并入 params_redacted 不丢失）
        let params_redacted = format!(
            r#"{{"authz_epoch":{},"decision":"{}","evidence_kind":"{:?}"}}"#,
            authz_epoch, decision_str, kind
        );
        crate::audit::insert_audit(
            &mut tx,
            &crate::audit::AuditInsert {
                actor_kind: "user",
                actor_user_id: Some(ctx.user_id.into_bytes()),
                event_type: "task.time_review",
                target_kind: "main_task",
                target_id: Some(main_task_id.to_string()),
                params_redacted: &params_redacted,
                outcome: "success",
            },
        )
        .await?;

        tx.commit().await?;
        Ok(TimeReviewOutcome::Completed { affected: held.len() })
    }

    /// 校验 is_admin / expected_revision CAS / acknowledge_unknown / reason 非空（is_admin 在事务内对重读 actor 行再验）；
    /// 删锁行、子任务保持 unknown、revision 递增；绝不触碰后续任务新 generation 的锁。
    async fn release_lock(
        &self,
        // 设备键为 32 字节公钥（与数据库 BINARY(32) 对齐），不是 Uuid；调用方从子任务关联行读取后传入
        device_id: [u8; 32],
        owner_sub_task_id: Uuid,
        expected_revision: u64,
        acknowledge_unknown: bool,
        reason: &str,
        ctx: AuthCtx,
    ) -> Result<ReleaseLockOutcome, ReleaseLockError> {
        // 请求期前置校验（事务外即可短路）
        if !ctx.is_admin {
            return Err(ReleaseLockError::PermissionDenied);
        }
        if !acknowledge_unknown {
            return Err(ReleaseLockError::UnknownNotAcknowledged);
        }
        if reason.trim().is_empty() {
            return Err(ReleaseLockError::InvalidArgument);
        }

        let mut tx = self.repo.begin().await?; // 短事务（pool 为仓储内部句柄）
        lock_schema_guard_for_version(&mut tx, SUPPORTED_SCHEMA_VERSION).await?;

        // 事务内重验 actor 当前状态与有效管理员权限（不信任事务前的 ctx 快照）：
        // 重新读取 actor 行并核验 active 与 is_admin；不满足则回滚并 PermissionDenied
        let actor = self.load_actor_row(&mut tx, ctx.user_id).await?;
        if !actor.active || !actor.is_admin {
            tx.rollback().await?;
            return Err(ReleaseLockError::PermissionDenied);
        }

        // expected_revision CAS：锁定子任务行并核验当前 revision 与状态
        let sub = self
            .tx_query_one::<SubLockRow>(
                &mut tx,
                "SELECT revision, state, reason_code, lock_generation FROM sub_tasks \
                 WHERE id = ? FOR UPDATE",
                params![owner_sub_task_id],
            )
            .await?
            .ok_or(ReleaseLockError::SubTaskNotFound)?;
        // CAS 状态判断之前把 String 承接的 state 真实解析为 SubTaskState：
        // 非法值 → ControllerError::Database(Decode) → 503（不当合法 Unknown、不返回 400）；
        // 保留 state == Unknown 的既有校验语义不变
        let state = parse_subtask_state(&sub.state)?;
        if sub.revision != expected_revision || state != SubTaskState::Unknown {
            tx.rollback().await?;
            return Err(ReleaseLockError::RevisionConflict);
        }

        // 删除与子任务实际 lock_generation 一致的锁行（不写死 generation = 1）；匹配失败同样是 RevisionConflict
        let removed = self
            .tx_execute(
                &mut tx,
                "DELETE FROM device_operation_locks \
                 WHERE device_id = ? AND owner_sub_task_id = ? AND generation = ?",
                params![&device_id[..], owner_sub_task_id, sub.lock_generation],
            )
            .await?;
        if removed == 0 {
            tx.rollback().await?;
            return Err(ReleaseLockError::RevisionConflict);
        }

        // 子任务保持 unknown，revision 递增
        self.tx_execute(&mut tx,
            "UPDATE sub_tasks SET revision = revision + 1, updated_at = NOW(6) \
             WHERE id = ?",
            params![owner_sub_task_id],
        )
        .await?;

        let authz_epoch = self.load_authz_epoch(&mut tx).await?;
        // reason 非空已由入参校验保证；脱敏口径：存 trim 后 reason、authz_epoch，另按 04 规格 L154 T-06 补记
        // 释放当时的实际 lock_generation 与原证据摘要（释放前子任务行 state/reason_code）；
        // RT-R7-5：以 serde_json::json! 安全序列化（format! 手工拼接遇 reason 含引号/特殊字符会生成坏 JSON 致事务失败）
        let params_redacted = serde_json::json!({
            "reason": reason.trim(),
            "authz_epoch": authz_epoch,
            "lock_generation": sub.lock_generation,
            "original_evidence": {
                "state": sub.state,
                "reason_code": sub.reason_code,
            },
        })
        .to_string();
        crate::audit::insert_audit(
            &mut tx,
            &crate::audit::AuditInsert {
                actor_kind: "user",
                actor_user_id: Some(ctx.user_id.into_bytes()),
                event_type: "task.release_lock",
                target_kind: "sub_task",
                target_id: Some(owner_sub_task_id.to_string()),
                params_redacted: &params_redacted,
                outcome: "success",
            },
        )
        .await?;

        tx.commit().await?;
        Ok(ReleaseLockOutcome::Released {
            revision: expected_revision + 1,
        })
    }
}
```

**Step 4 补充 · preview 代际与单调 TTL 的实际定义（追加于 `crates/rsetup-controller/src/tasks/preview.rs` Step 1 桩同文件；复用既有 `use uuid::Uuid;` 与 `use crate::ControllerError;`，不重复导入）：**

- **代际**：`PREVIEW_GENERATION` 由 `Uuid::new_v4()` **一次性**取值（进程重启即换代），是**非密钥** token，仅令跨代际 preview token 无法重新派生匹配；`preview_generation_bytes()` 只返回同一固定 epoch，**不得**每次请求重新随机。
- **单调 TTL 边界**：既有文字 `elapsed() <= 60s` 与实现口径**显式统一**为 `deadline.checked_duration_since(now) <= PREVIEW_MONO_TTL`——`remaining == 0`（恰到期）仍**有效**，`now` 晚于 deadline 即失效，`remaining > TTL`（`now` 早于登记时刻）不可接受一律 `false`；全程只用单调时钟，**不借墙钟延长**，`NOW(6)` 仍只是跨进程上限兜底。
- **注册时机与失败语义（调用点已由 `SqlxTaskRepository::create_preview` 生产实现闭合）**：preview 创建链须在 `tx.commit()` 成功之后、返回 preview token **之前**调用 `register_preview_mono_deadline(nonce)`；唯一生产调用点见上文 `create_preview` 代码块第 ⑥ 步（单语句 INSERT 提交成功之后），本闭合不再改动其余 HTTP/SQL。`checked_add` 溢出或锁不可用返回固定去敏 `ControllerError::Config`，调用方须放弃该 preview。同一 nonce 第二次注册保留**首次** deadline（TTL 绝不刷新）并返回 `Ok(())`；`preview_mono_valid` 对未知 nonce、全零 nonce、毒锁一律 `false`（fail-closed），不 `clear()` 毒锁、不做恢复；注册时顺带 prune 已到期项，map 不永久增长。

```rust
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

const PREVIEW_MONO_TTL: Duration = Duration::from_secs(60);
static PREVIEW_GENERATION: OnceLock<[u8; 16]> = OnceLock::new(); // 进程启动代际，非密钥 token
static PREVIEW_MONO_DEADLINES: OnceLock<Mutex<HashMap<[u8; 32], Instant>>> = OnceLock::new();

/// 毒锁 fail-closed：不 `clear()`、不 `recover`（生产 wrapper 与测试共用同一入口）。
fn lock_deadlines(store: &Mutex<HashMap<[u8; 32], Instant>>) -> Option<MutexGuard<'_, HashMap<[u8; 32], Instant>>> {
    store.lock().ok()
}

pub fn preview_generation_bytes() -> [u8; 16] {
    *PREVIEW_GENERATION.get_or_init(|| *Uuid::new_v4().as_bytes())
}

/// 纯函数：prune 已到期项 + 首次写入；重复 nonce 保留首次 deadline（绝不刷新 TTL），返回 `false` 仅表示 `checked_add` 溢出。
fn register_deadline_in(deadlines: &mut HashMap<[u8; 32], Instant>, now: Instant, nonce: [u8; 32]) -> bool {
    deadlines.retain(|_, deadline| deadline.checked_duration_since(now).is_some());
    if deadlines.contains_key(&nonce) {
        return true;
    }
    match now.checked_add(PREVIEW_MONO_TTL) {
        Some(deadline) => {
            deadlines.insert(nonce, deadline);
            true
        }
        None => false,
    }
}

fn mono_deadline_allows(deadlines: &HashMap<[u8; 32], Instant>, now: Instant, nonce: &[u8; 32]) -> bool {
    match deadlines.get(nonce) {
        Some(deadline) => match deadline.checked_duration_since(now) {
            Some(remaining) => remaining <= PREVIEW_MONO_TTL,
            None => false,
        },
        None => false,
    }
}

pub fn register_preview_mono_deadline(nonce: [u8; 32]) -> Result<(), ControllerError> {
    let store = PREVIEW_MONO_DEADLINES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = lock_deadlines(store).ok_or_else(|| ControllerError::Config("preview mono deadline store unavailable".to_string()))?;
    if register_deadline_in(&mut guard, Instant::now(), nonce) { Ok(()) } else { Err(ControllerError::Config("preview mono deadline store unavailable".to_string())) }
}

pub fn preview_mono_valid(nonce: &[u8; 32]) -> bool {
    if nonce == &[0u8; 32] {
        return false;
    }
    match lock_deadlines(PREVIEW_MONO_DEADLINES.get_or_init(|| Mutex::new(HashMap::new()))) {
        Some(guard) => mono_deadline_allows(&guard, Instant::now(), nonce),
        None => false,
    }
}
```

**纯行为单测（`preview.rs` 新增 `mod preview_generation_tests`，与既有 `mod tests` 并存；局部 `Mutex` + 固定 `Instant` 注入，不触碰全局 `OnceLock`、不 sleep、不倒拨实际时钟、不读环境变量、不连 DB；选择器为 `tasks::preview::preview_generation_tests::<fn>` 的 `--lib` fullpath + `-- --exact`，本切片不执行任何命令）。静态自验：恰到期 `remaining == 0` 判有效（与 `elapsed() <= 60s` 同口径）、`now` 晚于 deadline 判失效、`remaining > TTL` 判不可接受；重复注册不延长 TTL；墙钟回拨不影响单调判定（判定链只读 `Instant` 单调跨度，绝不读墙钟，回拨不得延长有效期）；毒锁在 wrapper 侧 `false`/`Config` 且不 `clear()`。本补充只交付代际与单调 TTL 两项，counts helpers、authz helpers、`PreviewSnapshotRow`/`PreviewSnapshotFixed` 行结构；preview 创建链写入侧（含单调注册调用点）已由 `SqlxTaskRepository::create_preview` 生产实现交付，B4 写入侧据此核销（DB 集成闭合仍以后续集成切片为准）；counts helpers、authz helpers、`PreviewSnapshotRow`/`PreviewSnapshotFixed` 行结构仍待后续步骤/任务，不据此宣称 B1 整体闭合：**

```rust
#[cfg(test)]
mod preview_generation_tests {
    use super::*;

    fn seeded(nonce: [u8; 32], now: Instant) -> HashMap<[u8; 32], Instant> {
        let mut map = HashMap::new();
        assert!(register_deadline_in(&mut map, now, nonce));
        map
    }

    #[test]
    fn generation_is_stable_within_process_and_uuid_v4_shaped() {
        let first = preview_generation_bytes();
        assert_eq!(first, preview_generation_bytes());
        assert_eq!(first[6] >> 4, 4);
        assert_eq!(first[8] & 0xC0, 0x80);
    }

    #[test]
    fn mono_deadline_window_is_inclusive_and_rejects_expiry_unknown_or_early() {
        let (nonce, now) = ([0x21u8; 32], Instant::now());
        let map = seeded(nonce, now);
        assert!(mono_deadline_allows(&map, now, &nonce));
        assert!(mono_deadline_allows(&map, now + PREVIEW_MONO_TTL, &nonce));
        assert!(!mono_deadline_allows(&map, now + Duration::from_secs(61), &nonce));
        assert!(!mono_deadline_allows(&map, now, &[0x77u8; 32]));
        assert!(!mono_deadline_allows(&map, now, &[0u8; 32]));
        let earlier = now.checked_sub(Duration::from_secs(1)).expect("monotonic instant has room");
        assert!(!mono_deadline_allows(&map, earlier, &nonce));
    }

    #[test]
    fn duplicate_registration_keeps_first_deadline_and_prunes_expired() {
        let (nonce, now) = ([0x23u8; 32], Instant::now());
        let mut map = seeded(nonce, now);
        assert!(register_deadline_in(&mut map, now + Duration::from_secs(30), nonce));
        assert_eq!(map.get(&nonce), Some(&(now + PREVIEW_MONO_TTL)));
        assert!(!mono_deadline_allows(&map, now + Duration::from_secs(61), &nonce));
        assert!(register_deadline_in(&mut map, now + Duration::from_secs(120), [0x24u8; 32]));
        assert!(!map.contains_key(&nonce));
    }

    #[test]
    fn poisoned_lock_fails_closed_without_clearing() {
        let store: Mutex<HashMap<[u8; 32], Instant>> = Mutex::new(HashMap::new());
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = store.lock().unwrap();
            panic!("poison preview mono deadline store");
        }));
        assert!(poisoned.is_err());
        assert!(lock_deadlines(&store).is_none());
    }
}
```

**Step 4 补充 · 一次性固定 `targets_json` 解码单元（追加于 `crates/rsetup-controller/src/tasks/preview.rs` 同一文件；`serde_json::from_str` 与 `hex::decode_to_slice` 均按 `auth/session.rs` 同风格以 crate 全路径直接调用，不新增依赖、不新增 `use` 行、不改全局 hex 解析器）：**

- **唯一持久表示**（与 `task_previews.targets_json` 的 schema 注释同一口径）：canonical `device_id` 的 **64 字符 lowercase hex 字符串 JSON 数组**（扁平 `["…", "…"]`）；**不是** `PreviewTarget` 对象数组，**不保存**显示名、`eligible` 或任何客户端额外字段。下一段 preview 创建链必须写出与该 decoder 完全一致的表示（`hex::encode(device_id)` → 字节序升序去重 → `serde_json::to_string`）；本任务只交付读取侧，不改 SQL/schema/写入链。
- **顺序与重复属于数据完整性**：规范化由创建生产端负责，decoder **不静默重排、不去重、不 truncate**——同一 snapshot 是提交的唯一目标真相源，乱序或重复说明持久内容与生产端契约不一致，必须 `None` 让整笔提交回滚（既有调用点返回 `SubmitError::InvalidArgument`），不得"修好"后继续。调用点其后的 `canonical_device_order(&mut targets)` 在合法输入上是幂等空操作（纵深防御），不是 decoder 的兜底。
- **校验口径（一律 fail-closed 返回 `None`）**：`serde_json::from_str::<Vec<String>>(raw).ok()?` 先固定形状（外层非数组、对象/数字/`null` 元素都解码失败）；数量必须落在 `1..=1024`（与 `validate_target_count` 同一边界）；逐项必须恰好 64 个 ASCII **小写** hex（大写、长度不符、非 hex 均拒绝；`hex::decode_to_slice` 只在小写检查通过后作二次校验，不依赖它对大写的宽容）；逐项必须严格大于前一项（升序 + 无重复）。全程无 `unwrap`/`expect`/`panic`/截断/宽松回退。
- **consumer import 同步**：`service.rs` 代码块的 value import 由 `use crate::tasks::preview::canonical_device_order;` 改为 `use crate::tasks::preview::{canonical_device_order, decode_preview_targets, preview_generation_bytes, preview_mono_valid};`——四个名字在本任务代码块内都有真实调用点（`decode_preview_targets` 在第 5 步目标真相源读出，`preview_generation_bytes`/`preview_mono_valid` 在第 5 步有效性判定①②），**不**加入尚无调用点的 `register_preview_mono_deadline`（issuer 接线属后续任务）——该句由 B4 写入侧闭合更新：`SqlxTaskRepository::create_preview` 交付真实调用点后，`register_preview_mono_deadline` 与 `device_target_hash` 去重并入同一 import 列表（token issuer 接线仍属后续任务）。同 scope 类型签名不变。

```rust
/// 一次性固定快照解码：只接受 canonical 表示（1..=1024 项、逐项 64 位 ASCII 小写 hex、严格升序且无重复）。
pub fn decode_preview_targets(raw: &str) -> Option<Vec<[u8; 32]>> {
    let items: Vec<String> = serde_json::from_str(raw).ok()?;
    if !(1..=1024).contains(&items.len()) {
        return None;
    }
    let mut targets: Vec<[u8; 32]> = Vec::with_capacity(items.len());
    for item in &items {
        let bytes = item.as_bytes();
        if bytes.len() != 64 || !bytes.iter().all(|&b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let mut device_id = [0u8; 32];
        if hex::decode_to_slice(bytes, &mut device_id).is_err() {
            return None;
        }
        if let Some(&previous) = targets.last() {
            if previous >= device_id {
                return None;
            }
        }
        targets.push(device_id);
    }
    Some(targets)
}
```

**纯行为单测（`preview.rs` 新增 `mod preview_snapshot_tests`，与既有 `mod tests`、`preview_generation_tests` 并存；纯字符串输入，不连 DB、不读环境变量、不触碰任何全局状态、不 sleep；合法输入只能由本 mod 内 `snapshot_json` helper 产出，选择器为 `tasks::preview::preview_snapshot_tests::<fn>` 的 `--lib` fullpath + `-- --exact`，本切片不执行任何命令）。静态自验：两个唯一升序 id 往返得到原字节数组；`malformed_json`/`count_zero`/`count_1025`/`device_too_short`/`device_uppercase`/`element_numeric`/`element_object`/`element_null`/`duplicate_device`/`unsorted_devices` 逐项 `None`（表驱动，用例名即失败线索）。本补充只交付 decoder 与 import 同步（`target_hash` 列已在 `migrations/0004_tasks.sql` 的 `task_previews` DDL 补齐，schema 缺失处理不再待办），`PreviewSnapshotRow`/`PreviewSnapshotFixed` 的 sqlx 行读取、authz helpers、preview 创建链与 token issuer 接线仍待后续步骤/任务，不据此宣称 B1 闭合：**

```rust
#[cfg(test)]
mod preview_snapshot_tests {
    use super::*;

    const DEVICE_A: [u8; 32] = [0x11u8; 32];
    const DEVICE_B: [u8; 32] = [0x22u8; 32];

    /// 合法 JSON 的唯一来源：与创建链同一持久表示（canonical 小写 hex 字符串数组）。
    fn snapshot_json(ids: &[[u8; 32]]) -> String {
        let items: Vec<String> = ids.iter().map(|id| hex::encode(id)).collect();
        serde_json::to_string(&items).expect("hex strings always serialize")
    }

    #[test]
    fn canonical_snapshot_round_trips_two_ordered_targets() {
        let raw = snapshot_json(&[DEVICE_A, DEVICE_B]);
        assert_eq!(decode_preview_targets(&raw), Some(vec![DEVICE_A, DEVICE_B]));
    }

    #[test]
    fn rejects_every_non_canonical_snapshot_shape() {
        let (a, b) = (hex::encode(DEVICE_A), hex::encode(DEVICE_B));
        // DEVICE_A 的 hex 全为数字 '1'，to_uppercase 后不变、仍是合法 canonical 输入；
        // uppercase 负例改用确实含 A–F 大写字母的合成串，仅作用于本 case，不改 DEVICE_A/B fixture。
        let uppercase = "ab".repeat(32).to_uppercase();
        assert_eq!(uppercase.len(), 64);
        assert!(uppercase.chars().any(|c| matches!(c, 'A'..='F')));
        let too_many: Vec<String> = (0..1025usize).map(|i| format!("\"{:064x}\"", i)).collect();
        let cases: Vec<(&str, String)> = vec![
            ("malformed_json", "[".to_string()),
            ("count_zero", "[]".to_string()),
            ("count_1025", format!("[{}]", too_many.join(","))),
            ("device_too_short", format!("[\"{}\"]", &a[..62])),
            ("device_uppercase", format!("[\"{}\"]", uppercase)),
            ("element_numeric", "[123]".to_string()),
            ("element_object", format!("[{{\"device_id\":\"{}\"}}]", a)),
            ("element_null", "[null]".to_string()),
            ("duplicate_device", format!("[\"{}\",\"{}\"]", a, a)),
            ("unsorted_devices", format!("[\"{}\",\"{}\"]", b, a)),
        ];
        for (name, raw) in cases {
            assert_eq!(decode_preview_targets(&raw), None, "{}", name);
        }
    }
}
```

- [ ] **Step 5: 运行并确认 GREEN**

```bash
cargo test --offline --locked -p rsetup-controller preview_rejects_empty_and_excessive_targets --
cargo test --offline --locked -p rsetup-controller canonical_order_is_sorted_and_deduplicated --
cargo test --offline --locked -p rsetup-controller target_hash_is_order_independent_and_content_sensitive --
cargo test --offline --locked -p rsetup-controller idempotency_key_must_be_canonical_lowercase_uuid_v4 --
cargo test --offline --locked -p rsetup-controller time_review_never_extends_deadline --
cargo test --offline --locked -p rsetup-controller cancel_is_only_for_queued_or_held --
```
预期：各执行 1 个用例，全部 PASS（6 用例 PASS，0 失败）。

- [ ] **Step 6（严格条件门禁）：隔离双库设备锁/幂等/原子消费验证**

**前置与 Task 3 Step 5 完全相同**（授权隔离目标、真实备份、只读核验、独占停写、显式迁移门禁）。满足后才创建 `tests/task_db.rs` 并运行：

```bash
CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test --offline --locked -p rsetup-controller --test task_db -- --ignored --test-threads=1
CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test --offline --locked -p rsetup-controller --test task_db -- --ignored --test-threads=1
```
必须覆盖的可观察行为（每条都断言 DB 真实行，**不得**用内存 fake 代替）：
- `later_batch_is_busy_without_network_send`：同设备两批不同幂等键 → 前者 `queued`、后者 `failed/DEVICE_BUSY`；`device_operation_locks` 只有一行且 owner 仍是首个子任务；受控 `BoardClient` 替身的 `execute` 计数为 0；
- `same_key_same_content_replays_original_task`：同 actor + 同幂等键 + 同清单 → 返回**原** `main_task_id`，`sub_tasks` 数量不增加；
- `same_key_different_content_conflicts`：同键不同清单 → `IdempotencyConflict`，不新建任务；
- `preview_token_is_single_use`：同一 token 两次提交（并发或串行）→ 第二次 `PreviewConsumed`/`PreviewExpired`，且未产生第二批子任务；
- `expired_preview_replay_returns_original_task_without_reconsuming`：首次提交成功后，显式将 `task_previews` 行置为过期（`expires_at = NOW(6) - INTERVAL 10 SECOND`）或已消费，重放相同 actor + 幂等键 + 载荷的请求，断言仍成功返回**原** `main_task_id`（直接幂等重放，不因预览过期拒绝）；
- `submit_all_failed_batch_persists_main_completed_failed`（RT-R8-3）：同批次全部目标均 `DEVICE_BUSY`/`PERMISSION_DENIED`（全部子任务落 `failed`、无一入队）→ 主任务持久行必须为 `state='completed'`、`outcome='failed'`、`counts_json.failed` = 目标数、`revision = 2`（INSERT 1 + 收敛 UPDATE +1），不得残留 `queued`/`NULL`；混合批次（至少一个入队且至少一个 failed，即 `[Queued, Failed]`，RT-R9-2）主行必须落 `state='running'`、`outcome=NULL`、`revision = 2`，与 HTTP 动态投影完全自洽（聚合器对混合批次返回 `(Running, None)`，04 规格 L79–83：有子任务终态即算批次已开始处理）；RED 预期：硬编码分支实现下混合批次主行落 `queued`/`NULL` 与动态投影 `running` 分裂，断言失败；GREEN 判据：真库断言主行 state/outcome/counts/revision 四字段；RT-R10-3 补充：同一测试族须覆盖排队槽位满时提交离线设备——离线/能力检查先于排队槽位占用与设备锁申请，目标必须直接落 `failed`（`DEVICE_OFFLINE`），绝不入队等它上线；RT-R11-1/RT-R11-2 补充：同一测试族还须覆盖在线但不具备 reboot 能力（`reboot_capable = false`）的目标——不落 `DEVICE_OFFLINE`、不入队，真库断言该行 `state='failed'` 且 `reason_code='CAPABILITY_UNAVAILABLE'`（获批词表，对应 `ReasonCode::CapabilityUnavailable`），且读取路径与 HTTP 投影把该行正确映射为同一字符串 `CAPABILITY_UNAVAILABLE`，不得投影出 `CAPABILITY_UNSUPPORTED`、空串或缺失 reason_code；
- `time_review_rejects_after_admin_revoked`：管理员被并发降权/停用后事务内重验 → `PermissionDenied`（403）、回滚，held 子任务状态与主任务 revision 零变更；RED 预期：事务内权限重验未实现（只信 HTTP 层快照）时用例因 `PermissionDenied` 断言失败；GREEN 判据：真库断言错误变体与零行变更；
- `time_review_rejects_revision_cas_conflict`：`expected_revision != main_tasks.revision`（或主任务缺失）→ `RevisionConflict`（409），任何子任务状态不变；RED 预期：CAS 重验缺失时用例因 `RevisionConflict` 断言失败；GREEN 判据：真库断言 revision 与子任务状态未动；
- `time_review_expired_releases_locks_and_bumps_revision`：`ReferenceDeadlineExpired` 分支 → 主任务全部 held 子任务 `expired` + `QUEUE_EXPIRED`、其 `device_operation_locks` 行删除、`main_tasks.revision` 恰好 +1、`created_at`/`observe_deadline` 不变；RED 预期：expired 分支未实现时用例因 `expired` 状态/锁行删除/revision 递增断言失败；GREEN 判据：真库断言上述行与字段；
- `time_review_updates_main_task_summary_row`（RT-R8-3）：Resume 与 Expire 分支提交后，主任务持久行必须与子任务变更同事务更新——事务内重查全部子任务最新状态重算十态 `counts_json`，经 `calculate_main_task_outcome` 重算 `state`/`outcome`（全部 held 恢复 `queued` 且其余皆 `queued` → 主行 `queued`/`NULL`；全部转 `expired` 且其余皆终态 → 主行 `completed` 且 outcome 按聚合规则收敛），与 `revision + 1` 同一条 UPDATE 落库；回滚路径主行零变更；RED 预期：仅递增 revision 的旧实现因主行 `state`/`outcome`/`counts_json` 陈旧断言失败；GREEN 判据：真库断言主行四字段与子任务状态一致；
- `time_review_writes_redacted_decision_audit`：每次 time_review 落一条 `task.time_review` 审计，`params_redacted` 仅含 decision/kind/authz_epoch 摘要，无原始 evidence、token 或 reason 明文；RED 预期：审计写入缺失时用例因审计行数与脱敏形状断言失败；GREEN 判据：真库断言审计行存在且脱敏字段合规；
- `release_lock_rejects_stale_generation_or_revision`：`expected_revision` 不匹配，或锁行 `generation` 与子任务当前 `lock_generation` 不一致（旧代际）→ `RevisionConflict`，锁行不删除，且该设备后续任务新 generation 的锁不受触碰；RED 预期：`WHERE … AND generation = ?` 代际条件缺失时用例因"锁行仍存在/新代际锁未动"断言失败；GREEN 判据：真库断言锁行数与 generation 未变；
- `release_lock_releases_matching_generation`：管理员 + `acknowledge_unknown=true` + 非空 reason + `device_id/owner_sub_task_id/lock_generation` 全匹配 → 锁行删除、子任务保持 `unknown`、其 `revision` +1、审计落 `task.release_lock`（脱敏）；审计载荷必须断言 `params_redacted` 含 `lock_generation`（释放当时锁行的实际 generation）与 `original_evidence`（含释放前子任务行原 `state`/`reason_code`，与真库释放前值一致），且审计写入失败时整个释放事务必须回滚（锁行不删除、子任务 `revision` 不递增）；RED 预期：实现缺失时用例因锁行删除/revision 递增/审计载荷断言失败；GREEN 判据：真库断言上述行、字段与审计载荷形状。

预期：各引擎执行用例数 **> 0** 且全部 PASS，记录被测引擎版本。缺 URL、连接失败、0 用例、备份不可证 **一律** `BLOCKED`；**禁止** `eprintln!("BLOCKED")` + `return` 让 ignored 用例报告成功，**禁止**伪造 accepted 证据。

- [ ] **Step 7: 静态检查与提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/tasks/preview.rs crates/rsetup-controller/src/tasks/repository.rs \
  crates/rsetup-controller/src/tasks/service.rs crates/rsetup-controller/src/tasks/mod.rs
# tests/task_db.rs 仅在 Step 6 门禁通过后才纳入提交
git commit -m "feat(controller): atomic task preview, submit, and persistent device locks"
```

---
