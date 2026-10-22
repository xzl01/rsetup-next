//! 设备状态读组件（集成验收 Task 0b；《04 board-client-and-http》第 1305–1345 行）。
//!
//! 本模块只提供**独立组件子 router** [`build_device_status_router`]：
//! - 不计入 Runtime 的 8 条任务路由，也不混入 Identity 的 26 条管理路由；
//! - 路径集合只有 `GET /api/v1/devices/{id}/status` 一条，不注册任何写路由、
//!   SSE 或 `/system/time`；
//! - 顶层生产装配唯一归属 `crate::production::build_production_router`。
//!
//! 数据来源契约（02 §1）：handler **只读当前进程真实缓存**的设备状态快照，
//! GET 绝不触发执行/采集。缓存无样本时状态源返回 `Ok(None)`，响应投影为
//! `snapshot=null`、`freshness="unknown"`、`age_ms=null`，**不得**以常量伪 PASS；
//! `received_time` 恒为合法 `TimeEvidence` 对象（无样本时为中控当前系统后备
//! 证据 `SystemFallback`，不伪造设备样本或设备接收时间），满足前端
//! `assertDeviceStatusReportShape` 的对象断言。`age_ms` 一律按缓存条目的
//! **单调接收锚点**（`Instant`）计算，宿主墙钟前跳/回拨都不会使年龄倒退或清零。
//! [`EmptyDeviceStatusSnapshotSource`] 是「本进程尚未挂载任何 typed 缓存」这一真实
//! 状态的实现：它不伪造任何快照、时间证据或新鲜度。transport 线 typed 缓存已交付
//! 后，生产装配位改挂 [`crate::polling::SnapshotStore`]（内存最新快照 + 单调 30s
//! 新鲜度 + 代际/序号校验），本模块的 handler 与投影不变。
//!
//! handler 固定顺序与 `src/http_auth.rs`、`src/tasks/http.rs` 完全一致，
//! **不自写** Host/Cookie/CSRF：
//! ①`authenticate_read(EndpointId::DeviceStatusRead)`（Host → 严格单值会话
//! Cookie → 活会话预检 → 权威 authenticate → 读限速 50/s）
//! → ②[`require_business_session`]（临时密码一律 403 `PASSWORD_CHANGE_REQUIRED`，
//! admin 同样，不要求 `is_admin`）→ ③路径提取（以 `Result<Path<String>,
//! PathRejection>` 值接收，任何路径提取失败**绝不**先于两个门禁短路，统一在
//! 门禁后映射 400 `INVALID_ARGUMENT`）与 ID 严格解析 → ④[`DeviceStatusAuthorizer`]
//! **对象级授权判定**（`device.status.read`）
//! → ⑤读真实缓存快照 → ⑥统一错误映射。本模块不新增业务 permission，只消费
//! 01 §4 既有的 `device.status.read`。
//!
//! 授权契约（H1；02 §5「`GET /devices/{id}/status` … `device.status.read`，
//! 只缓存」+ 01 §4 A-04「有 reboot 但无 status/read 不能读取完整状态」
//! \+ 01 A-06「不可见与不存在统一 404」）：路径解析成功后、读缓存**之前**，
//! 必须经 [`DeviceStatusState::authorizer`] 做设备级三态判定（见
//! [`DeviceStatusAccess`]）。授权与存在性求值恒在**同一只读授权快照**上完成
//! （[`SnapshotDeviceStatusAuthorizer`]），handler 不拼 SQL、不自建第二套
//! grant/role/group 语义，也不因缓存 `snapshot` 为空而跳过判定——缓存里有没有
//! 样本，都不改变「谁能看见这台设备的状态」这一事实。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{FromRef, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::get;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::ControllerError;
use crate::api::{map_error, parse_device_id_path};
use crate::auth::service::Session;
use crate::authz::policy::Permission;
use crate::authz::service::snapshot_effective_permissions;
use crate::authz::sqlx_repo::{AuthorizationSnapshot, AuthorizationSnapshotRepository};
use crate::http_api::{err_response, ok_response};
use crate::http_auth::{EndpointId, HttpAuthState};
use crate::http_gate::authenticate_read;
use crate::model::decimal_u64_json;
use crate::tasks::http::require_business_session;
use crate::time::evidence::{TimeEvidence, TimeQuality};

/// 设备状态读路由的显式状态：共享认证上下文 + 真实缓存状态源 + 对象级授权器。
///
/// `auth` 必须与 auth/management/tasks 三线来自**同一个** `Arc<HttpAuthState>`
/// （共享 session/CSRF/限流上下文），由 `build_production_router` 以
/// `Arc::ptr_eq` 强制校验。
///
/// `authorizer` 是 `device.status.read` 的唯一判定入口（H1）：三条线共用认证
/// 上下文，但**不**共用授权判定，缺失该字段即无法编译，装配方无法再次漏掉
/// 设备级鉴权。
#[derive(Clone)]
pub struct DeviceStatusState {
    pub auth: Arc<HttpAuthState>,
    pub snapshot: Arc<dyn DeviceStatusSnapshotSource>,
    pub authorizer: Arc<dyn DeviceStatusAuthorizer>,
}

impl FromRef<DeviceStatusState> for Arc<HttpAuthState> {
    fn from_ref(state: &DeviceStatusState) -> Self {
        Arc::clone(&state.auth)
    }
}

/// dyn-safe 异步返回：与 `ManagementFuture`/`TaskFuture` 同一 BoxFuture 形态，
/// 不用 `async fn` in trait 再转 `dyn`。
pub type DeviceStatusFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ControllerError>> + Send + 'a>>;

/// 状态源：读**当前进程真实缓存**的设备状态快照。
///
/// GET 不得触发执行/采集；缓存无样本返回 `Ok(None)`（响应 `snapshot=null`、
/// `freshness="unknown"`、`age_ms=null`），**不得**常量伪 PASS。读取失败必须
/// `Err` 上抛，由统一映射转 503，绝不降级为 `unknown` 200。
pub trait DeviceStatusSnapshotSource: Send + Sync {
    fn snapshot<'a>(
        &'a self,
        device_id: &'a [u8; 32],
    ) -> DeviceStatusFuture<'a, Option<DeviceStatusCacheEntry>>;
}

/// 缓存条目：真实 typed 快照 + 接收时刻证据 + 单调接收锚点 + 新鲜度判定 + 去敏错误码。
pub struct DeviceStatusCacheEntry {
    pub snapshot: rsetup_protocol::wire::DeviceStatus,
    pub received_time: TimeEvidence,
    /// 本进程接收该样本的**单调锚点**（05 §时间：新鲜度与 `age_ms` 只按单调
    /// 接收年龄）。墙钟证据 [`Self::received_time`] 仅用于展示，不参与年龄计算。
    pub received_at: Instant,
    pub freshness: DeviceStatusFreshness,
    /// 固定去敏稳定码，无原始 driver 日志。
    pub last_error: Option<String>,
}

/// 新鲜度四态（02 §1）：`unknown`/`fresh`/`stale`/`error`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStatusFreshness {
    Unknown,
    Fresh,
    Stale,
    Error,
}

/// 恒无样本状态源：真实反映「本进程缓存中没有任何设备状态样本」。
///
/// 它对任何设备都返回 `Ok(None)`，因此响应恒为 `snapshot=null`、`freshness="unknown"`、
/// `age_ms=null`（`received_time` 为中控系统后备 TimeEvidence 对象）；不返回
/// 常量快照、不返回常量 `fresh`、也不返回错误。生产装配位现由
/// [`crate::polling::SnapshotStore`] 承担真实缓存，本类型保留给「显式不挂载缓存」
/// 的装配与测试。
#[derive(Clone, Copy, Debug, Default)]
pub struct EmptyDeviceStatusSnapshotSource;

impl DeviceStatusSnapshotSource for EmptyDeviceStatusSnapshotSource {
    fn snapshot<'a>(
        &'a self,
        _device_id: &'a [u8; 32],
    ) -> DeviceStatusFuture<'a, Option<DeviceStatusCacheEntry>> {
        Box::pin(async { Ok(None) })
    }
}

/// `GET /api/v1/devices/{id}/status` 的对象级授权三态（01 §4 权限点 + A-06）。
///
/// 三态的**唯一** HTTP 映射写在 [`get_device_status`]：`Allowed` 继续读缓存；
/// `Forbidden` → 403 `PERMISSION_DENIED`；`NotFound` → 404 `NOT_FOUND`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceStatusAccess {
    /// 设备存在且未归档，且有效权限集含 `device.status.read`：允许读状态缓存。
    Allowed,
    /// 设备存在且未归档，有效权限集非空但**不含** `device.status.read`（01 §4
    /// A-04：只有 `device.reboot` 或只有 `device.read` 都不构成读完整状态的授权）。
    Forbidden,
    /// 设备缺失或已归档，**或**有效权限集为空（对该设备完全不可见）：与「不存在」
    /// 不可区分地返回 404，不泄露设备是否存在（01 A-06）。
    NotFound,
}

/// 设备状态读的对象级授权判定。
///
/// 生产实现 [`SnapshotDeviceStatusAuthorizer`]；测试注入同 trait 的 fake，不连
/// 任何数据库。实现必须**只读**授权快照，不得触发采集/执行，也不得因状态缓存
/// 是否有样本而改变结论。
pub trait DeviceStatusAuthorizer: Send + Sync {
    /// 以已认证 `actor` 的身份判定其对 `device_id` 的状态读权限。
    ///
    /// 返回 `Err` 时 handler 一律走 [`map_error`] 统一映射，**绝不**降级放行，也不
    /// 因错误种类改写状态码语义：
    /// - 授权后端不可用（连接/解码/事务失败）→ 由 `map_error` 映射（503）。
    /// - `actor` 用户行缺失时 [`AuthorizationSnapshotRepository::load_snapshot`] 返回
    ///   `ControllerError::PermissionDenied` → `map_error` 映射为 **403**（不降级 200，
    ///   也不折叠成 404）。正常路径下这类 actor 早已在 ①`authenticate` 处被拒（会话
    ///   绑定的用户不存在即认证失败 401），因此该分支实际不可达；一旦出现（例如认证
    ///   与授权之间用户行被删除）它就是 403，本组件不为其另造投影。
    fn authorize<'a>(
        &'a self,
        actor: &'a Session,
        device_id: &'a [u8; 32],
    ) -> DeviceStatusFuture<'a, DeviceStatusAccess>;
}

/// 生产授权器：在**同一只读授权快照**上同时求值设备存在性与有效权限集。
///
/// 复用授权唯一真源 [`AuthorizationSnapshotRepository::load_snapshot`] 与
/// [`snapshot_effective_permissions`]（admin shortcut、archived/missing、
/// actor 生命周期与 grant/role/group 语义全部沿用），本类型不写第二套策略、
/// 不拼 SQL。
pub struct SnapshotDeviceStatusAuthorizer {
    snapshots: Arc<dyn AuthorizationSnapshotRepository>,
}

impl SnapshotDeviceStatusAuthorizer {
    pub fn new(snapshots: Arc<dyn AuthorizationSnapshotRepository>) -> Self {
        Self { snapshots }
    }
}

impl DeviceStatusAuthorizer for SnapshotDeviceStatusAuthorizer {
    fn authorize<'a>(
        &'a self,
        actor: &'a Session,
        device_id: &'a [u8; 32],
    ) -> DeviceStatusFuture<'a, DeviceStatusAccess> {
        Box::pin(async move {
            let snapshot = self
                .snapshots
                .load_snapshot(actor.user.id, std::slice::from_ref(device_id))
                .await?;
            Ok(evaluate_device_status_access(&snapshot, *device_id))
        })
    }
}

/// 三态求值纯函数：设备存在性（missing/archived）与有效权限集在同一个
/// `snapshot` 上求值，无副作用、可脱离 DB 单测。
///
/// 求值顺序固定：先看设备（缺失/已归档一律 `NotFound`，不进入权限求值），再看
/// 有效权限集（空集 = 完全不可见 = `NotFound`；含 `DeviceStatusRead` = `Allowed`；
/// 其余非空 = `Forbidden`）。actor `!active`/`must_change_password` 由
/// [`snapshot_effective_permissions`] 恒给出空集，因此同样落 `NotFound`（fail-closed）。
fn evaluate_device_status_access(
    snapshot: &AuthorizationSnapshot,
    device_id: [u8; 32],
) -> DeviceStatusAccess {
    if !snapshot
        .devices
        .get(&device_id)
        .is_some_and(|device| !device.archived)
    {
        return DeviceStatusAccess::NotFound;
    }
    let effective = snapshot_effective_permissions(snapshot, device_id);
    if effective.contains(&Permission::DeviceStatusRead) {
        DeviceStatusAccess::Allowed
    } else if effective.is_empty() {
        DeviceStatusAccess::NotFound
    } else {
        DeviceStatusAccess::Forbidden
    }
}

/// 设备状态读子路由：恰 1 条端点。
pub fn build_device_status_router(state: DeviceStatusState) -> Router<()> {
    Router::new()
        .route("/api/v1/devices/{id}/status", get(get_device_status))
        .with_state(state)
}

/// `GET /api/v1/devices/{id}/status`：见模块头的固定顺序。
///
/// 路径参数以 `Result<Path<String>, PathRejection>` 值接收：Axum 对提取失败
/// 不再自动短路成纯文本 400，`authenticate_read` 与
/// [`require_business_session`] 恒先于任何路径解析执行；门禁通过后的提取失败
/// 统一映射 400 `INVALID_ARGUMENT` 信封。
///
/// 授权判定（H1）恒在路径解析之后、读缓存**之前**：`Forbidden` 403
/// `PERMISSION_DENIED`、`NotFound` 404 `NOT_FOUND`（不可见与不存在统一投影），
/// 授权后端错误走 [`map_error`]（连接/解码/事务失败 503；`load_snapshot` 的
/// actor 行缺失 `PermissionDenied` 为 403，见 [`DeviceStatusAuthorizer::authorize`]），
/// 一律不降级 200。判定只看会话身份与设备，**不因状态缓存
/// 为空而短路**——无样本同样必须通过授权才可能拿到 200。
async fn get_device_status(
    State(state): State<DeviceStatusState>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Response {
    let actor = match authenticate_read(&state.auth, &headers, EndpointId::DeviceStatusRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_business_session(&actor) {
        return response;
    }
    let Path(id) = match path {
        Ok(path) => path,
        Err(_) => return invalid_device_id(),
    };
    let id = match parse_device_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match state.authorizer.authorize(&actor, &id).await {
        Ok(DeviceStatusAccess::Allowed) => {}
        // 设备在册但无 device.status.read：403，绝不读缓存。
        Ok(DeviceStatusAccess::Forbidden) => return map_error(ControllerError::PermissionDenied),
        // 缺失/已归档/完全不可见：与不存在同投影 404。
        Ok(DeviceStatusAccess::NotFound) => return map_error(ControllerError::NotFound),
        // 授权后端失败不降级：统一映射（后端不可用 503 / actor 行缺失 403），绝不返回 200 或 unknown。
        Err(error) => return map_error(error),
    }
    let entry = match state.snapshot.snapshot(&id).await {
        Ok(entry) => entry,
        Err(error) => return map_error(error),
    };
    ok_response(device_status_data(entry.as_ref(), Utc::now()))
}

/// 路径段无法 percent-decode 为 UTF-8 时的统一信封：与
/// [`parse_device_id_path`] 同一 `INVALID_ARGUMENT` 形状。
fn invalid_device_id() -> Response {
    err_response(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        "device.invalid_id",
        json!({}),
    )
}

/// 02 §1 响应投影：`{snapshot, received_time, freshness, age_ms}`，
/// `error` 且保留最后好样本时另带 `last_error`。
///
/// `received_time` 恒为 `TimeEvidence` 对象：无样本时为中控当前系统后备证据
/// （[`fallback_received_time`]），前端 `assertDeviceStatusReportShape` 不接受
/// null。`age_ms` 是无符号 u64 的**十进制字符串**，且只按缓存条目的单调接收
/// 锚点计算（见 [`age_ms`]）；无样本恒 `null`。`now` 由调用方注入，纯函数可测。
fn device_status_data(entry: Option<&DeviceStatusCacheEntry>, now: DateTime<Utc>) -> Value {
    let Some(entry) = entry else {
        return json!({
            "snapshot": Value::Null,
            "received_time": fallback_received_time(now),
            "freshness": DeviceStatusFreshness::Unknown,
            "age_ms": Value::Null,
        });
    };
    let mut data = json!({
        "snapshot": device_status_json(&entry.snapshot),
        "received_time": entry.received_time,
        "freshness": entry.freshness,
        // error 保留最后好样本时同样输出其真实单调 age，绝不因错误态或墙钟跳变清零。
        "age_ms": decimal_u64_json(age_ms(entry)),
    });
    if let Some(last_error) = &entry.last_error {
        data["last_error"] = json!(last_error);
    }
    data
}

/// 无样本时的合法时间证据：中控本进程当前系统墙钟 + `SystemFallback` 质量，
/// 其余证据字段为 `None`。这是「本进程尚未接收任何样本」的真实表示（02 §1
/// 要求 `received_time` 为 TimeEvidence 对象），不伪造设备样本或设备接收时间。
fn fallback_received_time(now: DateTime<Utc>) -> TimeEvidence {
    TimeEvidence {
        system_wall_utc: now,
        reference_utc: now,
        quality: TimeQuality::SystemFallback,
        clock_epoch: Uuid::nil(),
        source: None,
        sample_age_ms: None,
        offset_ms: None,
        uncertainty_ms: None,
    }
}

/// 样本自单调接收锚点起经过的整毫秒数（05 §时间：新鲜度与显示年龄只按单调
/// 接收年龄）。单调时钟永不回拨，宿主墙钟前跳/回拨都不会使年龄倒退或归零；
/// 理论上仅当进程存活超过 u64 毫秒上限时饱和于 `u64::MAX`，不会回绕。
fn age_ms(entry: &DeviceStatusCacheEntry) -> u64 {
    u64::try_from(entry.received_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// `rsetup_protocol::wire::DeviceStatus` 的 02 语义投影。
///
/// wire DTO 本身不含 serde 派生（协议层不预设 JSON 字节格式），因此这里显式
/// 逐字段投影；全部 64 位整数字段（`u64` 与 Protobuf `int64`）一律十进制
/// 字符串，避免超过 2^53 的 `sample_seq` / `sampled_unix_ms` /
/// `sampled_monotonic_ns` 在 JS 侧失真。本函数不新增、不推断任何 wire 字段。
fn device_status_json(status: &rsetup_protocol::wire::DeviceStatus) -> Value {
    json!({
        "schema_version": status.schema_version,
        "boot_id": status.boot_id,
        "agent_epoch": status.agent_epoch,
        "sample_seq": decimal_u64_json(status.sample_seq),
        // Protobuf int64：有符号十进制字符串（02 §1「64 位字段映射十进制字符串」），
        // 负值与超出 JS 2^53 安全整数范围的原始值都必须逐位保留。
        "sampled_unix_ms": status.sampled_unix_ms.to_string(),
        "sampled_monotonic_ns": decimal_u64_json(status.sampled_monotonic_ns),
        "clock_epoch": status.clock_epoch,
        "metrics": status
            .metrics
            .iter()
            .map(|metric| json!({
                "key": metric.key,
                "value": metric.value,
                "unit": metric.unit,
                "quality": metric.quality,
            }))
            .collect::<Vec<Value>>(),
        "clock_quality": status.clock_quality,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::service::{IdentityUser, Login, Session, SessionPage};
    use crate::auth::session::token_digest;
    use crate::authz::policy::{GrantRecord, GrantScope, GrantSource, SnapshotActor};
    use crate::authz::sqlx_repo::{
        AuthorizationSnapshot, AuthorizationSnapshotRepository, RepoFuture,
    };
    use crate::devices::repository::DeviceAdminView;
    use crate::http_auth::{AuthFuture, AuthServiceApi, SessionAuthority};
    use crate::http_security::HttpSecurityConfig;
    use crate::model::{AdmissionState, ReviewDecision};
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use chrono::TimeZone;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tower::ServiceExt;
    use uuid::Uuid;

    const HOST: &str = "controller.test";
    /// 合法设备 ID 形状（64 位小写 hex），只在门禁之后才会被解析。
    const DEVICE_PATH: &str =
        "/api/v1/devices/0000000000000000000000000000000000000000000000000000000000000001/status";
    /// [`DEVICE_PATH`] 解析后的 32B 设备 ID（授权判定必须以此为目标）。
    const DEVICE_ID: [u8; 32] = {
        let mut id = [0u8; 32];
        id[31] = 1;
        id
    };
    /// [`fixture_session`] 的用户 ID：授权快照的 actor 与 grant 归属都用它。
    const ACTOR_ID: [u8; 16] = [7u8; 16];
    /// 第二台在册设备的合法 ID 形状：用于证明 grant 的 scope 只对目标设备成立。
    const OTHER_DEVICE_PATH: &str =
        "/api/v1/devices/0000000000000000000000000000000000000000000000000000000000000002/status";
    /// [`OTHER_DEVICE_PATH`] 解析后的 32B 设备 ID。
    const OTHER_DEVICE_ID: [u8; 32] = {
        let mut id = [0u8; 32];
        id[31] = 2;
        id
    };
    /// percent-decode 后非法 UTF-8 的路径段：历史上会让 Axum 的 `Path<String>`
    /// 提取器先于门禁短路成纯文本 400。
    const MALFORMED_PATH: &str = "/api/v1/devices/%FF/status";
    /// synthetic 会话令牌：仅 64 位小写 hex 形状能通过严格 cookie 门禁。
    const RAW_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 18, 12, 0, 0).unwrap()
    }

    /// 只为把 router 立起来的最小认证桩：`is_live_session` 恒 false，因此
    /// `authenticate_read` 在触达任何仓储或状态源之前严格 401。本模块只断言
    /// 门禁顺序与响应投影，不认证任何真实会话。
    struct NoSessionAuth;

    impl AuthServiceApi for NoSessionAuth {
        fn login<'a>(&'a self, _u: &'a str, _p: &'a str) -> AuthFuture<'a, Login> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn authenticate<'a>(&'a self, _raw: &'a str) -> AuthFuture<'a, Session> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn change_password<'a>(
            &'a self,
            _s: &'a Session,
            _c: &'a str,
            _n: &'a str,
        ) -> AuthFuture<'a, ()> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn logout<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, ()> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn authz_epoch(&self) -> AuthFuture<'_, u64> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn is_live_session(&self, _digest: &[u8; 32]) -> bool {
            false
        }
        fn session_authority<'a>(
            &'a self,
            _digest: &'a [u8; 32],
        ) -> AuthFuture<'a, SessionAuthority> {
            Box::pin(async {
                Ok(SessionAuthority {
                    session_live: false,
                    is_admin: false,
                    authz_epoch: 0,
                })
            })
        }
        fn list_sessions<'a>(
            &'a self,
            _s: &'a Session,
            _c: Option<&'a str>,
            _l: usize,
        ) -> AuthFuture<'a, SessionPage> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn revoke_by_alias<'a>(&'a self, _s: &'a Session, _id: &'a str) -> AuthFuture<'a, bool> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn revoke_others<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, u64> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
    }

    fn fixture_session(must_change_password: bool) -> Session {
        session_with(must_change_password, false)
    }

    /// 同 [`fixture_session`]，但可指定 `is_admin`。授权真相是快照里的 actor 行，
    /// 会话侧的 `is_admin` 只是认证身份；两者的 admin 标记按用例保持一致，避免桩与
    /// 会话互相矛盾。
    fn session_with(must_change_password: bool, is_admin: bool) -> Session {
        Session {
            user: IdentityUser {
                id: ACTOR_ID,
                username: "alice".into(),
                password_hash: "$argon2id$=synthetic-test-only=".into(),
                active: true,
                is_admin,
                must_change_password,
                revision: 1,
            },
            digest: token_digest(RAW_TOKEN.as_bytes()),
        }
    }

    /// 活会话认证桩：`is_live_session` 恒 true、`authenticate` 恒返回指定
    /// synthetic 会话，用于验证**门禁通过后**的路径解析与响应投影；不连接
    /// 任何仓储，也不代表真实认证语义。
    struct LiveSessionAuth {
        session: Session,
    }

    impl AuthServiceApi for LiveSessionAuth {
        fn login<'a>(&'a self, _u: &'a str, _p: &'a str) -> AuthFuture<'a, Login> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn authenticate<'a>(&'a self, _raw: &'a str) -> AuthFuture<'a, Session> {
            Box::pin(async { Ok(self.session.clone()) })
        }
        fn change_password<'a>(
            &'a self,
            _s: &'a Session,
            _c: &'a str,
            _n: &'a str,
        ) -> AuthFuture<'a, ()> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn logout<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, ()> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn authz_epoch(&self) -> AuthFuture<'_, u64> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn is_live_session(&self, _digest: &[u8; 32]) -> bool {
            true
        }
        fn session_authority<'a>(
            &'a self,
            _digest: &'a [u8; 32],
        ) -> AuthFuture<'a, SessionAuthority> {
            let is_admin = self.session.user.is_admin;
            Box::pin(async move {
                Ok(SessionAuthority {
                    session_live: true,
                    is_admin,
                    authz_epoch: 0,
                })
            })
        }
        fn list_sessions<'a>(
            &'a self,
            _s: &'a Session,
            _c: Option<&'a str>,
            _l: usize,
        ) -> AuthFuture<'a, SessionPage> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn revoke_by_alias<'a>(&'a self, _s: &'a Session, _id: &'a str) -> AuthFuture<'a, bool> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn revoke_others<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, u64> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
    }

    /// 记录被调用次数的状态源：用于证明 GET 在门禁通过前绝不读缓存。
    struct CountingSource {
        calls: Arc<AtomicUsize>,
    }

    impl DeviceStatusSnapshotSource for CountingSource {
        fn snapshot<'a>(
            &'a self,
            _device_id: &'a [u8; 32],
        ) -> DeviceStatusFuture<'a, Option<DeviceStatusCacheEntry>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(None) })
        }
    }

    /// 恒有好样本的状态源：验证授权放行后的正常投影。
    struct SampledSource {
        calls: Arc<AtomicUsize>,
    }

    impl DeviceStatusSnapshotSource for SampledSource {
        fn snapshot<'a>(
            &'a self,
            _device_id: &'a [u8; 32],
        ) -> DeviceStatusFuture<'a, Option<DeviceStatusCacheEntry>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(Some(entry(DeviceStatusFreshness::Fresh, None))) })
        }
    }

    fn test_state(snapshot: Arc<dyn DeviceStatusSnapshotSource>) -> Router<()> {
        router_with_state(
            Arc::new(NoSessionAuth),
            snapshot,
            Arc::new(FixedAccessAuthorizer::allowed()),
        )
    }

    /// 活会话版本：门禁通过后才能观察路径解析与响应投影。
    fn live_state(
        snapshot: Arc<dyn DeviceStatusSnapshotSource>,
        must_change_password: bool,
    ) -> Router<()> {
        live_state_with_authorizer(
            snapshot,
            must_change_password,
            Arc::new(FixedAccessAuthorizer::allowed()),
        )
    }

    /// 活会话 + 指定授权桩：授权分支（403/404/后端错误）的可复用入口。
    fn live_state_with_authorizer(
        snapshot: Arc<dyn DeviceStatusSnapshotSource>,
        must_change_password: bool,
        authorizer: Arc<dyn DeviceStatusAuthorizer>,
    ) -> Router<()> {
        router_with_state(
            Arc::new(LiveSessionAuth {
                session: fixture_session(must_change_password),
            }),
            snapshot,
            authorizer,
        )
    }

    /// 活会话 + **生产授权器**（挂在授权快照桩上）：验证真实的设备存在性 +
    /// 有效权限集三态求值，全程不连数据库。
    fn live_state_with_snapshot_authorizer(
        snapshot: Arc<dyn DeviceStatusSnapshotSource>,
        repo: StubSnapshotRepo,
    ) -> Router<()> {
        live_state_with_snapshot_authorizer_as(snapshot, repo, false)
    }

    /// 同 [`live_state_with_snapshot_authorizer`]，但会话侧的 `is_admin` 可指定
    /// （须与快照里的 actor 行一致）。
    fn live_state_with_snapshot_authorizer_as(
        snapshot: Arc<dyn DeviceStatusSnapshotSource>,
        repo: StubSnapshotRepo,
        actor_is_admin: bool,
    ) -> Router<()> {
        router_with_state(
            Arc::new(LiveSessionAuth {
                session: session_with(false, actor_is_admin),
            }),
            snapshot,
            Arc::new(SnapshotDeviceStatusAuthorizer::new(Arc::new(repo))),
        )
    }

    fn router_with_state(
        auth: Arc<dyn AuthServiceApi>,
        snapshot: Arc<dyn DeviceStatusSnapshotSource>,
        authorizer: Arc<dyn DeviceStatusAuthorizer>,
    ) -> Router<()> {
        build_device_status_router(DeviceStatusState {
            auth: Arc::new(HttpAuthState::new(
                auth,
                HttpSecurityConfig {
                    allowed_hosts: vec![HOST.into()],
                    allowed_origin: Some("http://controller.test".into()),
                    trust_tls: false,
                },
            )),
            snapshot,
            authorizer,
        })
    }

    /// 三态授权桩：按预置结论应答，并记录授权判定的入参与次数（证明授权先于
    /// 读缓存、且确实以已认证 actor 与路径设备为目标）。
    #[derive(Clone)]
    struct FixedAccessAuthorizer {
        access: DeviceStatusAccess,
        calls: Arc<AtomicUsize>,
    }

    impl FixedAccessAuthorizer {
        fn allowed() -> Self {
            Self::new(DeviceStatusAccess::Allowed)
        }

        fn new(access: DeviceStatusAccess) -> Self {
            Self {
                access,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl DeviceStatusAuthorizer for FixedAccessAuthorizer {
        fn authorize<'a>(
            &'a self,
            actor: &'a Session,
            device_id: &'a [u8; 32],
        ) -> DeviceStatusFuture<'a, DeviceStatusAccess> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(actor.user.id, ACTOR_ID, "授权必须以已认证会话为 actor");
            assert_eq!(*device_id, DEVICE_ID, "授权目标必须是路径解析出的设备");
            let access = self.access;
            Box::pin(async move { Ok(access) })
        }
    }

    /// 授权后端失败桩：Err 必须走统一映射，绝不降级为 200。
    struct FailingAuthorizer;

    impl DeviceStatusAuthorizer for FailingAuthorizer {
        fn authorize<'a>(
            &'a self,
            _actor: &'a Session,
            _device_id: &'a [u8; 32],
        ) -> DeviceStatusFuture<'a, DeviceStatusAccess> {
            Box::pin(async { Err(ControllerError::Config("device status authz read".into())) })
        }
    }

    /// 授权快照仓储桩：不连 DB，按预置快照（或固定后端错误）应答 `load_snapshot`，
    /// 并记录取快照次数。`device` 是该快照对应的**目标设备**（断言取快照的口径）。
    #[derive(Clone)]
    struct StubSnapshotRepo {
        device: [u8; 32],
        reply: StubReply,
        calls: Arc<AtomicUsize>,
    }

    /// `ControllerError` 非 `Clone`，桩用枚举表达两种应答。
    #[derive(Clone)]
    enum StubReply {
        Snapshot(AuthorizationSnapshot),
        BackendError,
        /// `actor` 用户行缺失：真实 `load_snapshot` 的 fail-closed 应答。
        ActorMissing,
    }

    impl StubSnapshotRepo {
        fn new(snapshot: AuthorizationSnapshot) -> Self {
            Self::for_device(snapshot, DEVICE_ID)
        }

        /// 目标设备不是 [`DEVICE_ID`] 时的构造入口。
        fn for_device(snapshot: AuthorizationSnapshot, device: [u8; 32]) -> Self {
            Self {
                device,
                reply: StubReply::Snapshot(snapshot),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn failing() -> Self {
            Self {
                device: DEVICE_ID,
                reply: StubReply::BackendError,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        /// `actor` 用户行缺失的桩（`load_snapshot` → `PermissionDenied`）。
        fn actor_missing() -> Self {
            Self {
                device: DEVICE_ID,
                reply: StubReply::ActorMissing,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl AuthorizationSnapshotRepository for StubSnapshotRepo {
        fn load_snapshot<'a>(
            &'a self,
            actor_id: [u8; 16],
            device_ids: &'a [[u8; 32]],
        ) -> RepoFuture<'a, AuthorizationSnapshot> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(actor_id, ACTOR_ID);
            assert_eq!(device_ids, [self.device], "只按路径目标设备取一次快照");
            match self.reply.clone() {
                StubReply::Snapshot(snapshot) => Box::pin(async move { Ok(snapshot) }),
                StubReply::BackendError => Box::pin(async {
                    Err(ControllerError::Config(
                        "device status authz snapshot".into(),
                    ))
                }),
                StubReply::ActorMissing => {
                    Box::pin(async { Err(ControllerError::PermissionDenied) })
                }
            }
        }

        fn load_admin_target_snapshot<'a>(
            &'a self,
            _caller: &'a Session,
            _target_user: [u8; 16],
            _device_ids: &'a [[u8; 32]],
        ) -> RepoFuture<'a, AuthorizationSnapshot> {
            Box::pin(async { Err(ControllerError::PermissionDenied) })
        }
    }

    /// 构造授权快照：`perms` 为 direct grant 的权限集（空集 = 完全不可见），
    /// `device` 控制设备行是否存在/已归档。actor 恒 active、非 admin、已改密；
    /// 目标设备 [`DEVICE_ID`]，grant scope 为 `All`。
    fn snapshot_fixture(perms: &[Permission], device: DeviceFixture) -> AuthorizationSnapshot {
        snapshot_fixture_scoped(
            DEVICE_ID,
            FixtureActor::Member,
            GrantScope::All,
            perms,
            device,
        )
    }

    /// 通用授权快照：目标设备、actor 形态、grant scope 各自可指定，其余口径与
    /// [`snapshot_fixture`] 逐字一致（actor 恒 active、已改密；`perms` 空 = 无 grant）。
    fn snapshot_fixture_scoped(
        target: [u8; 32],
        actor: FixtureActor,
        scope: GrantScope,
        perms: &[Permission],
        device: DeviceFixture,
    ) -> AuthorizationSnapshot {
        let mut devices = HashMap::new();
        if let DeviceFixture::Present { archived } = device {
            devices.insert(
                target,
                DeviceAdminView {
                    public_key: target,
                    display_name: "device-1".into(),
                    admission_state: AdmissionState::Approved,
                    review_decision: ReviewDecision::Approved,
                    revision: 1,
                    descriptor: None,
                    archived,
                },
            );
        }
        let grants = if perms.is_empty() {
            Vec::new()
        } else {
            vec![GrantRecord {
                id: Uuid::new_v4(),
                user_id: ACTOR_ID,
                source: GrantSource::Direct(perms.to_vec()),
                scope,
                revision: 1,
            }]
        };
        AuthorizationSnapshot {
            actor: SnapshotActor {
                id: ACTOR_ID,
                active: true,
                is_admin: matches!(actor, FixtureActor::Admin),
                must_change_password: false,
            },
            authz_epoch: 1,
            grants,
            roles: HashMap::new(),
            device_groups: HashMap::new(),
            devices,
        }
    }

    /// 快照里 actor 的两种形态（恒 active、已改密）。
    #[derive(Clone, Copy, Debug)]
    enum FixtureActor {
        Member,
        Admin,
    }

    /// 快照里目标设备的三种形态。
    #[derive(Clone, Copy, Debug)]
    enum DeviceFixture {
        Present { archived: bool },
        Missing,
    }

    fn evidence_ago(millis: i64) -> TimeEvidence {
        let at = now() - chrono::Duration::milliseconds(millis);
        TimeEvidence {
            system_wall_utc: at,
            reference_utc: at,
            quality: TimeQuality::SystemFallback,
            clock_epoch: Uuid::new_v4(),
            source: None,
            sample_age_ms: None,
            offset_ms: None,
            uncertainty_ms: None,
        }
    }

    fn status_sample() -> rsetup_protocol::wire::DeviceStatus {
        rsetup_protocol::wire::DeviceStatus {
            schema_version: 1,
            boot_id: "boot-1".into(),
            agent_epoch: "epoch-1".into(),
            // 2^53 + 1：JSON number 会失真，必须投影为十进制字符串。
            sample_seq: 9_007_199_254_740_993,
            sampled_unix_ms: 1_760_000_000_000,
            sampled_monotonic_ns: 42,
            clock_epoch: "clock-1".into(),
            metrics: vec![rsetup_protocol::wire::Metric {
                key: "cpu.load1".into(),
                value: Some(0.5),
                unit: "ratio".into(),
                quality: "fresh".into(),
            }],
            clock_quality: "ntp_valid".into(),
        }
    }

    /// 单调接收锚点：构造为「已接收 `millis` 毫秒」。
    fn instant_ago(millis: u64) -> Instant {
        Instant::now() - Duration::from_millis(millis)
    }

    fn entry(freshness: DeviceStatusFreshness, last_error: Option<&str>) -> DeviceStatusCacheEntry {
        DeviceStatusCacheEntry {
            snapshot: status_sample(),
            received_time: evidence_ago(60_000),
            received_at: instant_ago(60_000),
            freshness,
            last_error: last_error.map(str::to_string),
        }
    }

    /// `age_ms` 必须是十进制字符串且落在 `[min, min + 5s)`：单调锚点由测试
    /// 构造，只允许测试执行本身的真实耗时作为上界余量。
    fn assert_age_ms_at_least(data: &Value, min: u64) {
        let raw = data["age_ms"]
            .as_str()
            .unwrap_or_else(|| panic!("age_ms 必须是字符串，实得 {:?}", data["age_ms"]));
        let age = raw.parse::<u64>().expect("age_ms 必须是十进制字符串");
        assert!(age >= min, "单调 age 不得小于已接收的 {min}ms，实得 {age}");
        assert!(age < min + 5_000, "单调 age 不得远超预期，实得 {age}");
    }

    async fn get(router: Router<()>, uri: &str) -> (StatusCode, Value) {
        get_with_cookie(router, uri, None).await
    }

    async fn get_with_cookie(
        router: Router<()>,
        uri: &str,
        cookie: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("GET")
            .uri(uri)
            .header("host", HOST);
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", format!("rsc_session={cookie}"));
        }
        let response = router
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 65_536).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    #[tokio::test]
    async fn empty_source_reports_no_sample_without_fabricating_snapshot() {
        let source = EmptyDeviceStatusSnapshotSource;
        assert!(
            source.snapshot(&[7u8; 32]).await.unwrap().is_none(),
            "空状态源必须 Ok(None)，不得伪造样本"
        );
        let data = device_status_data(None, now());
        assert!(data["snapshot"].is_null());
        assert_eq!(data["freshness"], json!("unknown"));
        assert!(data["age_ms"].is_null());
        assert!(data.get("last_error").is_none());
        // F1：received_time 必须是完整 TimeEvidence 对象（前端
        // assertDeviceStatusReportShape 对 null 抛 INVALID_RECEIVED_TIME）。
        let received = &data["received_time"];
        assert!(received.is_object(), "received_time 必须是对象");
        assert_eq!(received["quality"], json!("system_fallback"));
        assert_eq!(received["system_wall_utc"], json!("2026-10-18T12:00:00Z"));
        assert_eq!(received["reference_utc"], json!("2026-10-18T12:00:00Z"));
        assert_eq!(received["clock_epoch"], json!(Uuid::nil().to_string()));
        assert!(received["source"].is_null());
        assert!(received["sample_age_ms"].is_null());
        assert!(received["offset_ms"].is_null());
        assert!(received["uncertainty_ms"].is_null());
    }

    #[test]
    fn sample_projection_keeps_wire_semantics_and_decimal_u64_strings() {
        let mut entry = entry(DeviceStatusFreshness::Fresh, None);
        entry.received_time = evidence_ago(1_500);
        entry.received_at = instant_ago(1_500);
        let data = device_status_data(Some(&entry), now());
        assert_eq!(data["snapshot"]["schema_version"], json!(1));
        assert_eq!(data["snapshot"]["boot_id"], json!("boot-1"));
        assert_eq!(data["snapshot"]["agent_epoch"], json!("epoch-1"));
        assert_eq!(
            data["snapshot"]["sample_seq"],
            json!("9007199254740993"),
            "u64 必须十进制字符串，避免 JS 侧精度失真"
        );
        assert_eq!(data["snapshot"]["sampled_monotonic_ns"], json!("42"));
        assert_eq!(
            data["snapshot"]["sampled_unix_ms"],
            json!("1760000000000"),
            "int64 必须十进制字符串，不得输出 JSON number"
        );
        assert!(data["snapshot"]["sampled_unix_ms"].is_string());
        assert_eq!(data["snapshot"]["clock_quality"], json!("ntp_valid"));
        assert_eq!(data["snapshot"]["metrics"][0]["key"], json!("cpu.load1"));
        assert_eq!(data["snapshot"]["metrics"][0]["value"], json!(0.5));
        assert_eq!(data["freshness"], json!("fresh"));
        assert_age_ms_at_least(&data, 1_500);
        assert!(data.get("last_error").is_none());
    }

    #[test]
    fn sampled_unix_ms_is_signed_decimal_string_beyond_js_safe_integer() {
        // 02 §1「Protobuf 64 位字段映射十进制字符串」：正负超出 JS 2^53 安全
        // 整数与 i64 边界的原始值都必须逐位保留（JSON number 会舍入）。
        for raw in [
            9_007_199_254_740_993i64,
            -9_007_199_254_740_993i64,
            i64::MAX,
            i64::MIN,
            0,
        ] {
            let mut status = status_sample();
            status.sampled_unix_ms = raw;
            let data = device_status_json(&status);
            assert_eq!(
                data.get("sampled_unix_ms").and_then(Value::as_str),
                Some(raw.to_string().as_str()),
                "int64 {raw} 必须原样输出为十进制字符串"
            );
        }
    }

    #[test]
    fn error_freshness_keeps_last_good_sample_age_and_reports_last_error() {
        let data = device_status_data(
            Some(&entry(
                DeviceStatusFreshness::Error,
                Some("TRANSPORT_UNAVAILABLE"),
            )),
            now(),
        );
        assert_eq!(data["freshness"], json!("error"));
        assert_age_ms_at_least(&data, 60_000);
        assert!(
            data["age_ms"].as_str().unwrap().parse::<u64>().unwrap() > 0,
            "错误态不得把好样本 age 清零"
        );
        assert_eq!(data["last_error"], json!("TRANSPORT_UNAVAILABLE"));
        assert!(!data["snapshot"].is_null(), "错误态保留最后好样本");
    }

    #[test]
    fn stale_freshness_is_the_fourth_wire_value() {
        let data = device_status_data(Some(&entry(DeviceStatusFreshness::Stale, None)), now());
        assert_eq!(data["freshness"], json!("stale"));
        assert!(data.get("last_error").is_none());
    }

    #[test]
    fn age_survives_wall_clock_rollback_via_monotonic_anchor() {
        // 宿主墙钟回拨场景：接收证据的墙钟被回拨到 now 之后（墙钟差为负），
        // 但单调锚点显示样本确已接收 60 秒——age 必须仍 ≈60000，绝不清零。
        let mut entry = entry(DeviceStatusFreshness::Error, Some("TRANSPORT_UNAVAILABLE"));
        entry.received_time.system_wall_utc = now() + chrono::Duration::seconds(60);
        let data = device_status_data(Some(&entry), now());
        assert_eq!(data["freshness"], json!("error"));
        assert_age_ms_at_least(&data, 60_000);
    }

    #[test]
    fn age_never_regresses_as_monotonic_clock_advances() {
        // 墙钟前跳不影响单调年龄；单调时钟推进时年龄只增不减。
        let mut entry = entry(DeviceStatusFreshness::Fresh, None);
        entry.received_at = instant_ago(10);
        let first = age_ms(&entry);
        entry.received_at = instant_ago(60_000);
        let later = age_ms(&entry);
        assert!(
            later > first,
            "单调 age 必须随时间推进，不得倒退：{first} -> {later}"
        );
    }

    #[tokio::test]
    async fn route_is_registered_and_read_gate_runs_before_any_snapshot_read() {
        let calls = Arc::new(AtomicUsize::new(0));
        let router = test_state(Arc::new(CountingSource {
            calls: calls.clone(),
        }));
        let (status, body) = get(router, DEVICE_PATH).await;
        // 未认证必须严格 401（路由已注册，否则 404），且状态源一次都没被读。
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], json!("AUTH_REQUIRED"));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "GET 不得在门禁前触达缓存");
    }

    #[tokio::test]
    async fn malformed_path_without_session_still_returns_unified_401() {
        // F2：percent-decode 非法 UTF-8 的路径段绝不允许让提取器先于认证
        // 门禁短路成纯文本 400；缺会话必须严格 401 `AUTH_REQUIRED` 统一信封。
        let calls = Arc::new(AtomicUsize::new(0));
        let router = test_state(Arc::new(CountingSource {
            calls: calls.clone(),
        }));
        let (status, body) = get(router, MALFORMED_PATH).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], json!("AUTH_REQUIRED"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn malformed_path_with_temporary_password_returns_403_before_path_parse() {
        let calls = Arc::new(AtomicUsize::new(0));
        let router = live_state(
            Arc::new(CountingSource {
                calls: calls.clone(),
            }),
            true,
        );
        let (status, body) = get_with_cookie(router, MALFORMED_PATH, Some(RAW_TOKEN)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], json!("PASSWORD_CHANGE_REQUIRED"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn malformed_path_with_live_session_maps_to_invalid_argument_envelope() {
        // 门禁全部通过后，路径提取失败才映射统一 400 `INVALID_ARGUMENT` 信封。
        let calls = Arc::new(AtomicUsize::new(0));
        let router = live_state(
            Arc::new(CountingSource {
                calls: calls.clone(),
            }),
            false,
        );
        let (status, body) = get_with_cookie(router, MALFORMED_PATH, Some(RAW_TOKEN)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], json!("INVALID_ARGUMENT"));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "畸形路径不得触达缓存");
    }

    #[tokio::test]
    async fn live_session_empty_source_returns_unknown_with_time_evidence_object() {
        // 已认证 × 空样本的真实 HTTP 响应：200 + snapshot=null + unknown +
        // age_ms=null，且 received_time 为完整 TimeEvidence 对象（F1 消费兼容）。
        let router = live_state(Arc::new(EmptyDeviceStatusSnapshotSource), false);
        let (status, body) = get_with_cookie(router, DEVICE_PATH, Some(RAW_TOKEN)).await;
        assert_eq!(status, StatusCode::OK);
        let data = &body["data"];
        assert!(data["snapshot"].is_null());
        assert_eq!(data["freshness"], json!("unknown"));
        assert!(data["age_ms"].is_null());
        assert!(
            data["received_time"].is_object(),
            "空样本响应必须携带 TimeEvidence 对象"
        );
        assert_eq!(data["received_time"]["quality"], json!("system_fallback"));
    }

    #[tokio::test]
    async fn component_registers_only_the_status_route() {
        let router = test_state(Arc::new(EmptyDeviceStatusSnapshotSource));
        for path in [
            "/api/v1/devices",
            "/api/v1/devices/0000000000000000000000000000000000000000000000000000000000000001",
            "/api/v1/tasks",
            "/api/v1/devices/0000000000000000000000000000000000000000000000000000000000000001/status/retry",
        ] {
            let (status, _) = get(router.clone(), path).await;
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "path {path} 不应由本组件注册"
            );
        }
    }

    #[test]
    fn snapshot_source_error_maps_to_503_never_unknown_200() {
        // 状态源 Err 走统一映射：503，绝不降级成 freshness=unknown 的 200。
        let response = map_error(ControllerError::Config("device status cache read".into()));
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ---------------- H1：`device.status.read` 对象级授权 ----------------

    #[test]
    fn device_status_access_tristate_is_device_scoped_and_fail_closed() {
        let present = DeviceFixture::Present { archived: false };
        // 含 device.status.read → Allowed
        assert_eq!(
            evaluate_device_status_access(
                &snapshot_fixture(&[Permission::DeviceStatusRead], present),
                DEVICE_ID
            ),
            DeviceStatusAccess::Allowed
        );
        // 有效权限集非空但不含 status.read → Forbidden
        for perms in [
            vec![Permission::DeviceReboot],
            vec![Permission::DeviceRead],
            vec![
                Permission::DeviceRead,
                Permission::DeviceReboot,
                Permission::DeviceTaskRead,
            ],
        ] {
            assert_eq!(
                evaluate_device_status_access(&snapshot_fixture(&perms, present), DEVICE_ID),
                DeviceStatusAccess::Forbidden,
                "权限集 {:?} 不构成读完整状态",
                perms.iter().map(|p| p.as_str()).collect::<Vec<_>>()
            );
        }
        // 完全不可见（空权限集）→ NotFound
        assert_eq!(
            evaluate_device_status_access(&snapshot_fixture(&[], present), DEVICE_ID),
            DeviceStatusAccess::NotFound
        );
        // 设备缺失/已归档 → NotFound，即使带 status.read 授权
        for device in [
            DeviceFixture::Missing,
            DeviceFixture::Present { archived: true },
        ] {
            assert_eq!(
                evaluate_device_status_access(
                    &snapshot_fixture(
                        &[Permission::DeviceRead, Permission::DeviceStatusRead],
                        device
                    ),
                    DEVICE_ID
                ),
                DeviceStatusAccess::NotFound
            );
        }
        // 逐设备求值：另一台设备的结论不得由本设备借用（scope=All 也只对在册设备成立）
        assert_eq!(
            evaluate_device_status_access(
                &snapshot_fixture(&[Permission::DeviceStatusRead], present),
                [9u8; 32]
            ),
            DeviceStatusAccess::NotFound
        );
    }

    #[tokio::test]
    async fn status_read_permission_returns_200_with_projected_sample() {
        // 含 device.status.read 的普通用户 → 200，且响应投影照常输出真实样本。
        let repo = StubSnapshotRepo::new(snapshot_fixture(
            &[Permission::DeviceRead, Permission::DeviceStatusRead],
            DeviceFixture::Present { archived: false },
        ));
        let repo_calls = repo.calls.clone();
        let cache_calls = Arc::new(AtomicUsize::new(0));
        let router = live_state_with_snapshot_authorizer(
            Arc::new(SampledSource {
                calls: cache_calls.clone(),
            }),
            repo,
        );
        let (status, body) = get_with_cookie(router, DEVICE_PATH, Some(RAW_TOKEN)).await;
        assert_eq!(status, StatusCode::OK);
        let data = &body["data"];
        assert_eq!(data["snapshot"]["boot_id"], json!("boot-1"));
        assert_eq!(data["snapshot"]["sample_seq"], json!("9007199254740993"));
        assert_eq!(data["snapshot"]["sampled_unix_ms"], json!("1760000000000"));
        assert_eq!(data["freshness"], json!("fresh"));
        assert_eq!(repo_calls.load(Ordering::SeqCst), 1, "授权只取一次快照");
        assert_eq!(cache_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn missing_status_read_permission_returns_403_without_touching_cache() {
        // 01 §4 A-04：只有 device.read 或只有 device.reboot（甚至两者都有）都
        // 不能读取完整状态 → 403 `PERMISSION_DENIED`，且绝不触达状态缓存。
        for perms in [
            vec![Permission::DeviceRead],
            vec![Permission::DeviceReboot],
            vec![Permission::DeviceRead, Permission::DeviceReboot],
        ] {
            let repo = StubSnapshotRepo::new(snapshot_fixture(
                &perms,
                DeviceFixture::Present { archived: false },
            ));
            let cache_calls = Arc::new(AtomicUsize::new(0));
            let router = live_state_with_snapshot_authorizer(
                Arc::new(SampledSource {
                    calls: cache_calls.clone(),
                }),
                repo,
            );
            let (status, body) = get_with_cookie(router, DEVICE_PATH, Some(RAW_TOKEN)).await;
            let granted = perms
                .iter()
                .map(|permission| permission.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "权限 {granted:?} 不得读取设备状态，实得 {body}"
            );
            assert_eq!(
                body["error"]["code"],
                json!("PERMISSION_DENIED"),
                "{granted:?}"
            );
            assert_eq!(
                body["error"]["message_key"],
                json!("permission.denied"),
                "{granted:?}"
            );
            assert_eq!(
                cache_calls.load(Ordering::SeqCst),
                0,
                "无 status.read 时 GET 不得触达状态缓存（权限 {granted:?}）"
            );
        }
    }

    #[tokio::test]
    async fn forbidden_from_authorizer_never_reads_cache() {
        // 三态 fake 直连：Forbidden → 403 统一信封，缓存零触达。
        let cache_calls = Arc::new(AtomicUsize::new(0));
        let authorizer = FixedAccessAuthorizer::new(DeviceStatusAccess::Forbidden);
        let authorizer_calls = authorizer.calls.clone();
        let router = live_state_with_authorizer(
            Arc::new(SampledSource {
                calls: cache_calls.clone(),
            }),
            false,
            Arc::new(authorizer),
        );
        let (status, body) = get_with_cookie(router, DEVICE_PATH, Some(RAW_TOKEN)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], json!("PERMISSION_DENIED"));
        assert_eq!(authorizer_calls.load(Ordering::SeqCst), 1);
        assert_eq!(cache_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn invisible_device_returns_404_without_touching_cache() {
        // 完全不可见（有效权限集为空）与不存在同投影 404（01 A-06），零缓存触达。
        let repo = StubSnapshotRepo::new(snapshot_fixture(
            &[],
            DeviceFixture::Present { archived: false },
        ));
        let repo_calls = repo.calls.clone();
        let cache_calls = Arc::new(AtomicUsize::new(0));
        let router = live_state_with_snapshot_authorizer(
            Arc::new(SampledSource {
                calls: cache_calls.clone(),
            }),
            repo,
        );
        let (status, body) = get_with_cookie(router, DEVICE_PATH, Some(RAW_TOKEN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "实得 {body}");
        assert_eq!(body["error"]["code"], json!("NOT_FOUND"));
        assert_eq!(body["error"]["message_key"], json!("system.not_found"));
        assert_eq!(repo_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            cache_calls.load(Ordering::SeqCst),
            0,
            "不可见设备不得触达状态缓存，避免以 age/freshness 泄露设备存在性"
        );
    }

    #[tokio::test]
    async fn missing_or_archived_device_returns_404_even_with_status_read() {
        // 设备缺失与已归档：即使快照里有 status.read 授权也一律 404（不可见）。
        for (case, device) in [
            ("missing", DeviceFixture::Missing),
            ("archived", DeviceFixture::Present { archived: true }),
        ] {
            let repo = StubSnapshotRepo::new(snapshot_fixture(
                &[Permission::DeviceRead, Permission::DeviceStatusRead],
                device,
            ));
            let cache_calls = Arc::new(AtomicUsize::new(0));
            let router = live_state_with_snapshot_authorizer(
                Arc::new(SampledSource {
                    calls: cache_calls.clone(),
                }),
                repo,
            );
            let (status, body) = get_with_cookie(router, DEVICE_PATH, Some(RAW_TOKEN)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{case} 设备必须 404");
            assert_eq!(body["error"]["code"], json!("NOT_FOUND"), "{case}");
            assert_eq!(
                body["error"]["message_key"],
                json!("system.not_found"),
                "{case}"
            );
            assert_eq!(cache_calls.load(Ordering::SeqCst), 0, "{case}");
        }
    }

    #[tokio::test]
    async fn not_found_from_authorizer_never_reads_cache() {
        let cache_calls = Arc::new(AtomicUsize::new(0));
        let authorizer = FixedAccessAuthorizer::new(DeviceStatusAccess::NotFound);
        let router = live_state_with_authorizer(
            Arc::new(SampledSource {
                calls: cache_calls.clone(),
            }),
            false,
            Arc::new(authorizer),
        );
        let (status, body) = get_with_cookie(router, DEVICE_PATH, Some(RAW_TOKEN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], json!("NOT_FOUND"));
        assert_eq!(cache_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn authorizer_backend_error_maps_to_503_never_degrades_to_200() {
        // 授权后端 Err → 统一映射 503；绝不因「读不到授权」而放行读缓存。
        for (case, router) in [
            (
                "trait 桩",
                live_state_with_authorizer(
                    Arc::new(SampledSource {
                        calls: Arc::new(AtomicUsize::new(0)),
                    }),
                    false,
                    Arc::new(FailingAuthorizer),
                ),
            ),
            (
                "生产授权器 + 快照仓储失败",
                live_state_with_snapshot_authorizer(
                    Arc::new(SampledSource {
                        calls: Arc::new(AtomicUsize::new(0)),
                    }),
                    StubSnapshotRepo::failing(),
                ),
            ),
        ] {
            let (status, body) = get_with_cookie(router, DEVICE_PATH, Some(RAW_TOKEN)).await;
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "{case} 实得 {body}"
            );
            assert_ne!(status, StatusCode::OK, "{case}：授权失败绝不放行");
        }
    }

    #[tokio::test]
    async fn authorization_runs_after_gates_and_before_any_cache_read() {
        // 门禁顺序：认证 401 与临时密码 403 都不得触达授权与缓存；活会话必须
        // 先授权（1 次）再读缓存（1 次）——缓存无样本也不得短路授权判定。
        let cache_calls = Arc::new(AtomicUsize::new(0));

        let authorizer = FixedAccessAuthorizer::allowed();
        let authorizer_calls = authorizer.calls.clone();
        let anonymous = router_with_state(
            Arc::new(NoSessionAuth),
            Arc::new(CountingSource {
                calls: cache_calls.clone(),
            }),
            Arc::new(authorizer),
        );
        let (status, _) = get(anonymous, DEVICE_PATH).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(authorizer_calls.load(Ordering::SeqCst), 0, "未认证不得授权");
        assert_eq!(cache_calls.load(Ordering::SeqCst), 0);

        let authorizer = FixedAccessAuthorizer::allowed();
        let authorizer_calls = authorizer.calls.clone();
        let temporary = live_state_with_authorizer(
            Arc::new(CountingSource {
                calls: cache_calls.clone(),
            }),
            true,
            Arc::new(authorizer),
        );
        let (status, body) = get_with_cookie(temporary, DEVICE_PATH, Some(RAW_TOKEN)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], json!("PASSWORD_CHANGE_REQUIRED"));
        assert_eq!(
            authorizer_calls.load(Ordering::SeqCst),
            0,
            "临时密码不得进入授权判定"
        );
        assert_eq!(cache_calls.load(Ordering::SeqCst), 0);

        let authorizer = FixedAccessAuthorizer::allowed();
        let authorizer_calls = authorizer.calls.clone();
        let live = live_state_with_authorizer(
            Arc::new(CountingSource {
                calls: cache_calls.clone(),
            }),
            false,
            Arc::new(authorizer),
        );
        let (status, _) = get_with_cookie(live, DEVICE_PATH, Some(RAW_TOKEN)).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "缓存无样本只投影 unknown，不得改变授权判定"
        );
        assert_eq!(
            authorizer_calls.load(Ordering::SeqCst),
            1,
            "授权必须恰好在读缓存前执行一次"
        );
        assert_eq!(cache_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn admin_actor_without_explicit_grant_reads_status_via_admin_shortcut() {
        // admin shortcut（`eval_memory_permissions`：`active && is_admin` → 全量
        // `ALL_DEVICE_PERMISSIONS`，不看 grants）：active 且已改密的 admin，快照里
        // **没有任何显式 grant**，也对在册未归档设备拿到 `device.status.read` → 200。
        // 证的是本组件的对象级授权与仓库既有 admin 语义一致，不自建第二套策略。
        let snapshot = snapshot_fixture_scoped(
            DEVICE_ID,
            FixtureActor::Admin,
            GrantScope::All,
            &[],
            DeviceFixture::Present { archived: false },
        );
        assert_eq!(
            snapshot.grants.len(),
            0,
            "前提：本用例的 admin 必须没有任何显式 grant"
        );
        assert_eq!(
            evaluate_device_status_access(&snapshot, DEVICE_ID),
            DeviceStatusAccess::Allowed,
            "admin shortcut 必须在纯求值上就成立"
        );

        let repo = StubSnapshotRepo::new(snapshot);
        let repo_calls = repo.calls.clone();
        let cache_calls = Arc::new(AtomicUsize::new(0));
        let router = live_state_with_snapshot_authorizer_as(
            Arc::new(SampledSource {
                calls: cache_calls.clone(),
            }),
            repo,
            true,
        );
        let (status, body) = get_with_cookie(router, DEVICE_PATH, Some(RAW_TOKEN)).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "admin 无显式 grant 也必须读到状态，实得 {body}"
        );
        assert_eq!(body["data"]["snapshot"]["boot_id"], json!("boot-1"));
        assert_eq!(body["data"]["freshness"], json!("fresh"));
        assert_eq!(repo_calls.load(Ordering::SeqCst), 1, "授权只取一次快照");
        assert_eq!(cache_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn device_scoped_status_read_allows_only_its_own_device() {
        // scope=Device(target) 的 direct grant 只对目标设备成立（`evaluate_grants`）。
        // ① 目标设备 → 有效权限集含 `device.status.read` → 200；
        // ② 同用户对另一台**在册**设备 → 该设备既不在 grant scope 也无其他 grant →
        //    有效权限集为空 = 完全不可见 → 与「不存在」同投影 404（01 A-06）。本
        //    fixture 下它是「不可见」而非「可见无权限」，故断言 404 而非 403。
        let scoped = |target: [u8; 32]| {
            snapshot_fixture_scoped(
                target,
                FixtureActor::Member,
                GrantScope::Device(DEVICE_ID),
                &[Permission::DeviceStatusRead],
                DeviceFixture::Present { archived: false },
            )
        };

        // ① 目标设备：200 + 真实样本投影。
        let repo = StubSnapshotRepo::for_device(scoped(DEVICE_ID), DEVICE_ID);
        let repo_calls = repo.calls.clone();
        let cache_calls = Arc::new(AtomicUsize::new(0));
        let router = live_state_with_snapshot_authorizer(
            Arc::new(SampledSource {
                calls: cache_calls.clone(),
            }),
            repo,
        );
        let (status, body) = get_with_cookie(router, DEVICE_PATH, Some(RAW_TOKEN)).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "scope 命中的设备必须放行，实得 {body}"
        );
        assert_eq!(body["data"]["snapshot"]["boot_id"], json!("boot-1"));
        assert_eq!(repo_calls.load(Ordering::SeqCst), 1);
        assert_eq!(cache_calls.load(Ordering::SeqCst), 1);

        // ② 另一台在册设备：同一条 grant 不覆盖它 → 不可见 404，零缓存触达。
        let other = scoped(OTHER_DEVICE_ID);
        assert_eq!(
            evaluate_device_status_access(&other, OTHER_DEVICE_ID),
            DeviceStatusAccess::NotFound,
            "前提：另一台设备在纯求值上就应是不可见（非 Forbidden）"
        );
        let repo = StubSnapshotRepo::for_device(other, OTHER_DEVICE_ID);
        let repo_calls = repo.calls.clone();
        let cache_calls = Arc::new(AtomicUsize::new(0));
        let router = live_state_with_snapshot_authorizer(
            Arc::new(SampledSource {
                calls: cache_calls.clone(),
            }),
            repo,
        );
        let (status, body) = get_with_cookie(router, OTHER_DEVICE_PATH, Some(RAW_TOKEN)).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "scope 外的在册设备不得暴露存在性，实得 {body}"
        );
        assert_eq!(body["error"]["code"], json!("NOT_FOUND"));
        assert_eq!(body["error"]["message_key"], json!("system.not_found"));
        assert_eq!(repo_calls.load(Ordering::SeqCst), 1, "按路径目标设备取快照");
        assert_eq!(
            cache_calls.load(Ordering::SeqCst),
            0,
            "scope 外设备不得触达状态缓存"
        );
    }

    #[tokio::test]
    async fn missing_actor_row_from_snapshot_repo_maps_to_403_never_200() {
        // 与 trait `Err` 文档对齐：`load_snapshot` 在 actor 用户行缺失时返回
        // `PermissionDenied`，`map_error` 把它映射为 403（不降级 200、也不折叠成 404）。
        // 正常路径这类 actor 早在 authenticate 就被拒；本用例锁的是「若出现」的投影。
        let cache_calls = Arc::new(AtomicUsize::new(0));
        let router = live_state_with_snapshot_authorizer(
            Arc::new(SampledSource {
                calls: cache_calls.clone(),
            }),
            StubSnapshotRepo::actor_missing(),
        );
        let (status, body) = get_with_cookie(router, DEVICE_PATH, Some(RAW_TOKEN)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "实得 {body}");
        assert_eq!(body["error"]["code"], json!("PERMISSION_DENIED"));
        assert_ne!(status, StatusCode::OK, "actor 行缺失绝不放行");
        assert_eq!(cache_calls.load(Ordering::SeqCst), 0, "授权失败不得读缓存");
    }
}
