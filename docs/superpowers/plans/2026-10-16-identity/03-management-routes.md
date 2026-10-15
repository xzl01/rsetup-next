### Task 6: 管理路由骨架、共享映射器与生产网关装配

**Files:**
- Create: `crates/rsetup-controller/src/api/mod.rs`
- Create: 各资源模块骨架 `crates/rsetup-controller/src/api/{users,roles,grants,groups,devices,approvals,audit,system}.rs`
- Modify: `crates/rsetup-controller/src/http_api.rs`（新增 `user_admin_public`）
- Modify: `crates/rsetup-controller/src/lib.rs`（注册 `pub mod api;`）
- Test: `src/api/mod.rs`（纯映射/解析/游标单测）

**Interfaces:**
- Consumes: `HttpAuthState`、`http_gate::*`、`http_api::*`；独立集成owner传入真实网关及probe/sink，不消费或改写旧 `AppState` 本体。
- Produces:
  - 公共解析辅助（全部 `pub(crate)`）：
    - `parse_revision(&str) -> Result<u64, Response>`（必须为严格纯 ASCII 十进制，无 `+` 前缀，溢出 400 `INVALID_ARGUMENT`/`request.revision_invalid`）
    - `parse_device_id_path(&str) -> Result<[u8;32], Response>`（400 `INVALID_ARGUMENT`/`device.invalid_id`）
    - `parse_uuid_path(&str) -> Result<Uuid, Response>`（全计划唯一 UUID 文本 parser，owner `api/mod.rs`：仅接受规范小写连字符 36 字符（`8-4-4-4-12`、hex 全小写），拒空串/大写/大小写混合/花括号/紧凑 32 字符/`urn:` 前缀/首尾空白/尾换行/非 ASCII/畸形；不要求 v4 版本位，不擅扩大已定接受域；错误 400 `INVALID_ARGUMENT`/`request.invalid_id`，真实 body 见 Task 6「parse_uuid_path 唯一 producer」小节）
    - `parse_limit(Option<&str>) -> Result<usize, Response>`（默认 50，1..=200，超限 400 `request.invalid_limit`，**待审默认**）
    - `map_error(ControllerError) -> Response`
    - `page_response<T: serde::Serialize>(items: &[T], next_cursor: Option<String>) -> Response`
  - 8 个资源网关 trait（定义在各子模块中；生产网关在 Task 13 接入真实仓储；各资源测试用受控 fixture）：
    - `UsersGateway`、`RolesGateway`、`GrantsGateway`、`GroupsGateway`、`DevicesGateway`、`ApprovalsGateway`、`AuditGateway`、`SystemGateway`
  - 生产装配签名：`pub fn build_management_router(state: ManagementState) -> axum::Router<()>`（在 Task 13 真正打通真实装配；骨架阶段各子路由返回 0 个 mounted route 的 `Router::new()`，**未实现的路由不注册**）
  - 前端消息键注册表（供 Web 专题 i18n 字典对齐）：
    - `request.invalid_body`, `request.invalid_argument`, `request.invalid_id`, `request.invalid_limit`, `request.revision_invalid`
    - `identity.user.not_found`, `identity.user.username_taken`
    - `identity.role.not_found`, `identity.role.builtin_archived`
    - `identity.grant.not_found`
    - `identity.group.not_found`, `identity.group.members_exceeded`
    - `device.not_found`, `device.invalid_id`
    - `identity.last_admin`, `identity.invalid_state`

#### 统一签名分页 cursor 契约

所有管理分页端点（users/roles/grants/groups/devices/approvals）的 `cursor`/`next_cursor` 一律使用本契约函数对签发与校验，废弃裸内部键直传（与管理适配器子计划 A:247「handler 调用统一签名 cursor 函数」对齐）。

- **函数对**（`api/mod.rs`，`pub(crate)`，各资源模块共用）：
  - `encode_signed_cursor(secret: &[u8; 32], endpoint: EndpointId, user_id: [u8; 16], filter: &CursorFilterCtx, key: CursorKey) -> String`
  - `decode_signed_cursor(secret: &[u8; 32], endpoint: EndpointId, user_id: [u8; 16], filter: &CursorFilterCtx, token: &str) -> Result<CursorKey, ControllerError>`
  - `CursorKey` 两变体：`Uuid([u8; 16])`（users/roles/grants/groups）、`DeviceKey([u8; 32])`（devices/approvals 设备公钥 keyset）；`CursorFilterCtx` 为严格 query DTO 规范化后的筛选/排序参数字节串（固定字段序，每参数 `tag(1B) ‖ len(u16 BE) ‖ bytes`，缺省参数 `tag=0`），`limit` 不参与绑定。
- **上下文绑定与密钥**：HMAC 输入 = `b"rsetup-management-cursor-v1/" ‖ endpoint_id(u16 BE) ‖ user_id(16B) ‖ filter_canon ‖ payload`。`user_id` 取自已认证会话（不接受客户端 actor）；`endpoint_id` 取 Task 1「路由模板 → EndpointId 变体映射表」稳定序号，跨端点/跨用户/换筛选复用一律 MAC 校验失败。`CURSOR_SECRET` 为 32B 密钥，**既不经既有 `ControllerConfig::from_env` 自动载入（该结构只有 `database_url`/`listen_address` 两字段，没有也不会自动读取 cursor 密钥），也不由 Task 13 的 builder 私自加载**：唯一来源是环境变量 `CONTROLLER_MANAGEMENT_CURSOR_SECRET_HEX`，由唯一顶层装配 owner **集成 Task 0** 在注册管理 router 之前读取并安装到 `api/mod.rs` 的 `pub(crate) static MANAGEMENT_CURSOR_SECRET: OnceLock<[u8; 32]>`（与 Task 3 `AUDIT_CURSOR_KEY` 同为 `OnceLock` 静态密钥先例，但来源是部署环境注入而非进程内 CSPRNG）；真实 handler 经 `api/mod.rs` 的 `pub(crate) fn management_cursor_secret() -> Result<&'static [u8; 32], ControllerError>` 取得 `&[u8; 32]`，作为第一调用参数传入 `encode_signed_cursor`/`decode_signed_cursor`（未安装即返回固定去敏 `Config`，经既有 `map_error` 精确 503，失败关闭，绝不回退未签名 cursor）。完整代码定义、Task 0 启动顺序与测试见下「cursor 密钥生产链」小节。`ManagementState` 与各资源 `ApiState` 均不扩密钥字段（账本决议④补充：本账本不因此扩展 state 契约），不得硬编码常量密钥，不得复用 session token 或 `SessionAliasKey` 实例。
- **字节布局**（一律大端；tag = `HMAC-SHA256(CURSOR_SECRET, mac_input)` 前 8B 截断，恒定时间比较）：
  - UUID 键 profile：`payload 24B = endpoint_id(2B) ‖ cursor_kind(1B, =1) ‖ reserved(1B, 恒0) ‖ uuid(16B) ‖ 零填充(4B)`，加 `tag 8B` 共 32B → **64 小写 hex**。
  - 设备公钥 profile：`payload 40B = endpoint_id(2B) ‖ cursor_kind(1B, =2) ‖ reserved(1B, 恒0) ‖ public_key(32B) ‖ 零填充(4B)`，加 `tag 8B` 共 48B → 96 小写 hex（字段账 2+1+1+32+4=40B 守恒；32B 公钥装不进 24B payload，故同一函数对按 `cursor_kind` 使用扩展 profile；4B 零填充编码时恒写全零、解码时校验全零；audit 三元键 40B payload 与该长度兼容）。
- **解码校验顺序（先 MAC 后字段）**：① 长度恰为 64/96 且全小写 hex、hex decode 成功；② 重算 HMAC 并恒定时间比较 8B tag；③ tag 通过后才解析字段：`reserved == 0`、`cursor_kind` 已知、kind=1 要求 UUIDv4 且 4B 填充全零、kind=2 要求完整 32B 公钥且 4B 填充全零，且 `endpoint_id` 必须等于当前端点（第二道防线）。任一步失败一律 `ControllerError::InvalidArgument` → HTTP **400**（经 `map_error` 映射 `INVALID_ARGUMENT`）；不回退未分页全量，不区分「篡改/过期/外来」错误细节。
- **TDD 用例清单**（`api/mod.rs` 单测，全部落在 `signed_cursor_tests` 模块；RED 前置桩：`encode_signed_cursor` 恒返回 `String::new()`、`decode_signed_cursor` 恒返回 `Err(ControllerError::NotFound)`，GREEN 替换为真实实现；每条 selector 恰好运行 1 个测试且因行为断言失败才记 RED；完整测试模块代码见下文「codec 行为测试（5 个核心用例，代码定义）」小节（同一 `mod` 内另含 3 个 codec hardening 负向用例），下面按该 mod 现有 8 个测试函数逐条列出 selector，与下文 module selector 清单一致；真实 handler 往返与 no-key 503 仍属下一独立任务，其 selector 不在本清单）：
  - `api::signed_cursor_tests::signed_cursor_roundtrip_encodes_and_decodes_bound_key` — 正常往返：encode→decode 返回同一 `CursorKey`。
  - `api::signed_cursor_tests::signed_cursor_rejects_tampered_tag_with_invalid_argument` — tag hex 翻转 1 bit → `InvalidArgument`（400）。
  - `api::signed_cursor_tests::signed_cursor_rejects_other_user` — 以另一 `user_id` 解码 → `InvalidArgument`。
  - `api::signed_cursor_tests::signed_cursor_rejects_changed_filter` — 更换筛选/排序参数集解码 → `InvalidArgument`。
  - `api::signed_cursor_tests::signed_cursor_rejects_cross_endpoint` — 更换 `endpoint` 上下文解码 → `InvalidArgument`。
  - `api::signed_cursor_tests::signed_cursor_rejects_invalid_input_shapes` — 输入形状非法（截断、尾部追加、大写 hex、首字符非 hex）解码 → `InvalidArgument`。
  - `api::signed_cursor_tests::signed_cursor_rejects_resigned_malformed_fields` — 用 `resign` helper 以合法密钥对篡改后的 payload 重签，越过 MAC 后 malformed 字段仍被拒 → `InvalidArgument`。
  - `api::signed_cursor_tests::signed_cursor_filter_context_rejects_noncanonical_fields` — filter 非规范构造被拒，规范构造可往返且换 filter 上下文解码 → `InvalidArgument`。
  - RED→GREEN 命令沿用本文既有格式（selector 级）：`CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR="$PWD/.cargo-target" cargo test --offline --locked -p rsetup-controller <上述selector>`。
- **与既有 audit MAC 的关系**：Task 3 的 `encode_audit_cursor`/`decode_audit_cursor`（40B payload + 32B 全量 tag = 144 hex，密钥为进程内 CSPRNG `AUDIT_CURSOR_KEY`，重启旧 cursor 即 400）**保持现状，本次不强制改 audit**；后续统一方向：迁移到本契约函数对与配置注入的稳定密钥（audit 三元键 payload 与设备公钥 profile 的 40B 长度兼容），另行排期。

#### 共享管理 cursor codec（代码定义）

- **owner 与落点**：下列类型与函数由未来实现任务写入 `crates/rsetup-controller/src/api/mod.rs`（与「cursor 密钥生产链」同文件同模块，`pub(crate)` 供各资源模块共用）；本小节只修文档，不改真代码，**零新增 Cargo 依赖**（`sha2`、`hex` 已在本 crate 声明）。MAC 是本模块独立实现的管理域 HMAC-SHA256：不调用 `auth/session.rs` 的私有 `hmac_sha256`/`constant_time_eq_32`，不复用 `SessionAliasKey` 实例，codec 内不读取任何环境变量（密钥只经第一调用参数传入）。行为测试与 handler 接线属下一独立任务，本小节不宣称 Identity blocker 已闭合。
- **`EndpointId` 稳定 cursor 序号**：Task 1「路由模板 → `EndpointId` 变体映射表」（本文 133–152 行）只给变体未给整数，`crate::http_auth::EndpointId` 也未声明 `#[repr]`，故序号用显式 `match` 固定为 1..=16 并按表行序一一对应（`Me`=1 … `DeviceStatusRead`=16）；序号只进入 MAC 输入，不改变读桶键 `(EndpointId, session.digest)` 语义，也不得改用 `as u16` 之类依赖判别值的写法。

```rust
use crate::error::ControllerError;
use crate::http_auth::EndpointId;
use sha2::Digest as _;

const CURSOR_MAC_DOMAIN: &[u8] = b"rsetup-management-cursor-v1/";
const CURSOR_KIND_UUID: u8 = 1;
const CURSOR_KIND_DEVICE: u8 = 2;
const CURSOR_TAG_LEN: usize = 8;
const CURSOR_UUID_HEX: usize = 64;
const CURSOR_DEVICE_HEX: usize = 96;

/// Task 1 映射表的稳定 cursor 序号（1..=16，与表行序一致），仅用于 MAC 输入。
pub(crate) fn endpoint_cursor_code(endpoint: EndpointId) -> u16 {
    match endpoint {
        EndpointId::Me => 1,
        EndpointId::Sessions => 2,
        EndpointId::UsersRead => 3,
        EndpointId::EffectivePermissionsRead => 4,
        EndpointId::RolesRead => 5,
        EndpointId::GrantsRead => 6,
        EndpointId::GroupsRead => 7,
        EndpointId::DevicesRead => 8,
        EndpointId::DeviceDetailRead => 9,
        EndpointId::ApprovalsRead => 10,
        EndpointId::AuditRead => 11,
        EndpointId::SystemStatusRead => 12,
        EndpointId::TaskList => 13,
        EndpointId::TaskDetail => 14,
        EndpointId::TaskChildren => 15,
        EndpointId::DeviceStatusRead => 16,
    }
}

/// cursor 绑定的内部键：UUID 键走 24B payload profile，设备公钥走 40B profile。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CursorKey {
    Uuid([u8; 16]),
    DeviceKey([u8; 32]),
}

/// 严格 query DTO 规范化后的筛选/排序参数集；`limit` 不参与绑定，故不在其中。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CursorFilterCtx {
    fields: Vec<(u8, Option<Vec<u8>>)>,
}

impl CursorFilterCtx {
    /// 无筛选/排序参数（例如只带 `limit`）时的固定上下文。
    pub(crate) fn empty() -> Self {
        Self { fields: Vec::new() }
    }

    /// 由**已按固定 schema 字段序**给出的参数构造：`tag` 取自该 query DTO 的 schema 常量表，
    /// 必须 `> 0`、严格升序且唯一；`None` 表示该参数缺省。长度超过 `u16::MAX`、`tag = 0`、
    /// 重复或乱序一律 `InvalidArgument` 拒绝——绝不截断、绝不静默重排，避免长度/排序歧义。
    pub(crate) fn from_ordered_fields(fields: &[(u8, Option<&[u8]>)]) -> Result<Self, ControllerError> {
        let mut stored: Vec<(u8, Option<Vec<u8>>)> = Vec::with_capacity(fields.len());
        let mut prev_tag: u8 = 0;
        for (tag, value) in fields {
            if *tag == 0 || *tag <= prev_tag {
                return Err(ControllerError::InvalidArgument);
            }
            match value {
                None => stored.push((*tag, None)),
                Some(value) => {
                    if value.len() > u16::MAX as usize {
                        return Err(ControllerError::InvalidArgument);
                    }
                    stored.push((*tag, Some(value.to_vec())));
                }
            }
            prev_tag = *tag;
        }
        Ok(Self { fields: stored })
    }

    /// 规范化字节串：缺省参数写单个 `0`（`tag = 0` 保留为缺省标记），
    /// 其余 `tag(1B) ‖ len(u16 BE) ‖ bytes`；字段顺序即构造顺序，不做二次排序。
    pub(crate) fn canonical(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (tag, value) in &self.fields {
            match value {
                None => out.push(0u8),
                Some(value) => {
                    out.push(*tag);
                    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
                    out.extend_from_slice(value);
                }
            }
        }
        out
    }
}

/// 管理 cursor 域 MAC：`HMAC-SHA256(secret, domain ‖ endpoint(2B BE) ‖ actor(16B) ‖ filter_canon ‖ payload)`。
/// 与 `auth/session.rs` 的私有实现同形（`sha2` 手工 ipad/opad），但域常量与输入账不同，故独立实现。
fn management_cursor_mac(
    secret: &[u8; 32],
    endpoint_code: u16,
    actor: &[u8; 16],
    filter_canon: &[u8],
    payload: &[u8],
) -> [u8; 32] {
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..32 {
        ipad[i] ^= secret[i];
        opad[i] ^= secret[i];
    }
    let mut inner = sha2::Sha256::new();
    inner.update(ipad);
    inner.update(CURSOR_MAC_DOMAIN);
    inner.update(endpoint_code.to_be_bytes());
    inner.update(actor);
    inner.update(filter_canon);
    inner.update(payload);
    let inner_hash = inner.finalize();
    let mut outer = sha2::Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

/// 固定长 xor 累积比较（8B tag）：不短路、不提前返回，不泄露首个差异位置。
fn management_cursor_tag_eq(a: &[u8; CURSOR_TAG_LEN], b: &[u8; CURSOR_TAG_LEN]) -> bool {
    let mut diff = 0u8;
    for i in 0..CURSOR_TAG_LEN {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn management_cursor_all_zero(bytes: &[u8]) -> bool {
    let mut acc = 0u8;
    for b in bytes {
        acc |= b;
    }
    acc == 0
}

/// payload 字节账：UUID `2+1+1+16+4 = 24B`，设备公钥 `2+1+1+32+4 = 40B`；`reserved` 与 4B 填充恒写全零。
fn management_cursor_payload(endpoint_code: u16, key: &CursorKey) -> Vec<u8> {
    let mut payload = Vec::with_capacity(40);
    payload.extend_from_slice(&endpoint_code.to_be_bytes());
    match key {
        CursorKey::Uuid(uuid) => {
            payload.push(CURSOR_KIND_UUID);
            payload.push(0u8);
            payload.extend_from_slice(uuid);
            payload.extend_from_slice(&[0u8; 4]);
        }
        CursorKey::DeviceKey(public_key) => {
            payload.push(CURSOR_KIND_DEVICE);
            payload.push(0u8);
            payload.extend_from_slice(public_key);
            payload.extend_from_slice(&[0u8; 4]);
        }
    }
    payload
}

pub(crate) fn encode_signed_cursor(
    secret: &[u8; 32],
    endpoint: EndpointId,
    user_id: [u8; 16],
    filter: &CursorFilterCtx,
    key: CursorKey,
) -> String {
    let endpoint_code = endpoint_cursor_code(endpoint);
    let payload = management_cursor_payload(endpoint_code, &key);
    let mac = management_cursor_mac(secret, endpoint_code, &user_id, &filter.canonical(), &payload);
    let mut raw = Vec::with_capacity(payload.len() + CURSOR_TAG_LEN);
    raw.extend_from_slice(&payload);
    raw.extend_from_slice(&mac[..CURSOR_TAG_LEN]);
    hex::encode(raw)
}

pub(crate) fn decode_signed_cursor(
    secret: &[u8; 32],
    endpoint: EndpointId,
    user_id: [u8; 16],
    filter: &CursorFilterCtx,
    token: &str,
) -> Result<CursorKey, ControllerError> {
    // ① 严格长度与全小写 hex：长度不符、大写 hex、非 hex 一律先拒，不进入 MAC。
    if token.len() != CURSOR_UUID_HEX && token.len() != CURSOR_DEVICE_HEX {
        return Err(ControllerError::InvalidArgument);
    }
    if !token.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(ControllerError::InvalidArgument);
    }
    let raw = hex::decode(token).map_err(|_| ControllerError::InvalidArgument)?;
    if raw.len() != CURSOR_UUID_HEX / 2 && raw.len() != CURSOR_DEVICE_HEX / 2 {
        return Err(ControllerError::InvalidArgument);
    }
    let (payload, tag_bytes) = raw.split_at(raw.len() - CURSOR_TAG_LEN);
    let mut tag = [0u8; CURSOR_TAG_LEN];
    tag.copy_from_slice(tag_bytes);
    // ② 以当前 endpoint / user / filter 上下文重算 MAC，固定长比较 8B tag。
    let endpoint_code = endpoint_cursor_code(endpoint);
    let mac = management_cursor_mac(secret, endpoint_code, &user_id, &filter.canonical(), payload);
    let mut expected = [0u8; CURSOR_TAG_LEN];
    expected.copy_from_slice(&mac[..CURSOR_TAG_LEN]);
    if !management_cursor_tag_eq(&tag, &expected) {
        return Err(ControllerError::InvalidArgument);
    }
    // ③ tag 通过后才解析字段：MAC 覆盖 payload，字段篡改必已在 ② 失败；以下为第二道防线，
    //    负向 signed-malformed 输入仍逐项显式校验。任一步失败都是同一个去敏 InvalidArgument。
    if payload.len() != 24 && payload.len() != 40 {
        return Err(ControllerError::InvalidArgument);
    }
    let kind = payload[2];
    if u16::from_be_bytes([payload[0], payload[1]]) != endpoint_code || payload[3] != 0u8 {
        return Err(ControllerError::InvalidArgument);
    }
    match kind {
        CURSOR_KIND_UUID => {
            if payload.len() != 24 || !management_cursor_all_zero(&payload[20..24]) {
                return Err(ControllerError::InvalidArgument);
            }
            let mut uuid = [0u8; 16];
            uuid.copy_from_slice(&payload[4..20]);
            // UUIDv4：version nibble = 4，RFC 4122 variant = 10xx。
            if uuid[6] >> 4 != 0x4 || uuid[8] & 0b1100_0000 != 0b1000_0000 {
                return Err(ControllerError::InvalidArgument);
            }
            Ok(CursorKey::Uuid(uuid))
        }
        CURSOR_KIND_DEVICE => {
            if payload.len() != 40 || !management_cursor_all_zero(&payload[36..40]) {
                return Err(ControllerError::InvalidArgument);
            }
            let mut public_key = [0u8; 32];
            public_key.copy_from_slice(&payload[4..36]);
            Ok(CursorKey::DeviceKey(public_key))
        }
        _ => Err(ControllerError::InvalidArgument),
    }
}
```

- **静态自验（本轮不编译、不测试）**：两 profile 布局守恒——UUID `24B payload + 8B tag = 32B → 64 小写 hex`，设备 `40B + 8B = 48B → 96 小写 hex`，与上文「字节布局」字段账逐段一致；`decode` 的 ①②③ 与「解码校验顺序（先 MAC 后字段）」逐条对应，`kind` 与 payload 长度互锁（`kind=1 ↔ 24B`、`kind=2 ↔ 40B`），`endpoint_id`、`reserved`、4B 填充、UUIDv4 version/variant 全部显式校验；所有索引都在长度校验之后，无 `unwrap`/越界/panic 路径；`ControllerError::InvalidArgument` 是无载荷单元变体，错误串不含输入片段，失败即 400，不回退未分页全量。

#### codec 行为测试（5 个核心用例，代码定义）

- **owner 与落点**：下列 `#[cfg(test)] mod signed_cursor_tests` 由未来实现任务写入 `crates/rsetup-controller/src/api/mod.rs`（与上方 codec 同文件），`use super::*;` 直接调用本模块真实函数对，不定义局部同名函数或自定义 decoder；本小节只修文档，不改真代码，不运行测试。覆盖本 mod 现有 8 个用例：5 个核心用例（往返 + 4 个负向上下文绑定）加 3 个 codec hardening 负向用例（`signed_cursor_rejects_invalid_input_shapes` 输入形状、`signed_cursor_rejects_resigned_malformed_fields` 重签 malformed 字段、`signed_cursor_filter_context_rejects_noncanonical_fields` filter 非规范构造）；strict 形状、重签 malformed、filter 构造非法负向已在本组覆盖，仅 handler 往返与 no-key 503 仍属下一独立任务。
- **fixture 与负向断言**：`SECRET = [7u8; 32]`、`ACTOR = [1u8; 16]`、`OTHER_ACTOR = [2u8; 16]` 全为合成值，非真实密钥；UUID fixture `067e6162ea944431836c7d8fa2010b3c` 为合法 v4（`uuid[6] >> 4 == 4`、`uuid[8] & 0xC0 == 0x80`），`hex::decode` + `try_into` 仅在 fixture 内合法，生产代码不动。`ControllerError` 未派生 `PartialEq`，负向一律 `matches!(err, ControllerError::InvalidArgument)`；每个负向先用真实 `encode_signed_cursor` 产出有效 token 并断言同上下文可正常解码，再只改绑定上下文的一个要素——RED 桩下失败发生在「有效 token/正常解码」对照断言，不可仅期待 `InvalidArgument` 自证。
- **RED 前置桩（最小完整同签名桩，仅替换上方两个真实函数体，签名/类型/常量/helper 函数保持真实定义，GREEN 恢复上方真实实现；helper 暂不被调用只产生 dead_code 警告不是错误；RED 是断言失败不是编译错误）**：

```rust
pub(crate) fn encode_signed_cursor(_secret: &[u8; 32], _endpoint: EndpointId, _user_id: [u8; 16], _filter: &CursorFilterCtx, _key: CursorKey) -> String {
    String::new()
}
pub(crate) fn decode_signed_cursor(_secret: &[u8; 32], _endpoint: EndpointId, _user_id: [u8; 16], _filter: &CursorFilterCtx, _token: &str) -> Result<CursorKey, ControllerError> {
    Err(ControllerError::NotFound)
}
```

- **测试模块（GREEN 目标，恰 8 个测试（5 个核心用例 + 3 个 codec hardening 负向用例），每例实际调用上方生产函数对）**：

```rust
#[cfg(test)]
mod signed_cursor_tests {
    use super::*;
    const SECRET: [u8; 32] = [7u8; 32];
    const ACTOR: [u8; 16] = [1u8; 16];
    const OTHER_ACTOR: [u8; 16] = [2u8; 16];
    fn fixture_uuid_v4() -> [u8; 16] {
        let raw = hex::decode("067e6162ea944431836c7d8fa2010b3c").expect("fixture hex");
        <[u8; 16]>::try_from(raw).expect("fixture 16 bytes")
    }
    #[test]
    fn signed_cursor_roundtrip_encodes_and_decodes_bound_key() {
        let endpoint = EndpointId::UsersRead;
        let filter = CursorFilterCtx::empty();
        let uuid = fixture_uuid_v4();
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, CursorKey::Uuid(uuid));
        assert_eq!(token.len(), CURSOR_UUID_HEX);
        assert!(token.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        let raw = hex::decode(&token).expect("lowercase hex");
        assert_eq!(raw[2], CURSOR_KIND_UUID);
        assert_eq!(raw[3], 0u8);
        assert!(management_cursor_all_zero(&raw[20..24]));
        let decoded = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token)
            .expect("uuid profile roundtrip");
        assert_eq!(decoded, CursorKey::Uuid(uuid));
        for public_key in [[0u8; 32], [9u8; 32]] {
            let endpoint = EndpointId::DevicesRead;
            let key = CursorKey::DeviceKey(public_key);
            let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, key);
            assert_eq!(token.len(), CURSOR_DEVICE_HEX);
            let raw = hex::decode(&token).expect("lowercase hex");
            assert_eq!(raw[2], CURSOR_KIND_DEVICE);
            assert_eq!(raw[3], 0u8);
            assert!(management_cursor_all_zero(&raw[36..40]));
            let decoded = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token)
                .expect("device profile roundtrip");
            assert_eq!(decoded, CursorKey::DeviceKey(public_key));
        }
    }
    #[test]
    fn signed_cursor_rejects_tampered_tag_with_invalid_argument() {
        let endpoint = EndpointId::UsersRead;
        let filter = CursorFilterCtx::empty();
        let key = CursorKey::Uuid(fixture_uuid_v4());
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, key);
        decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token).expect("valid token");
        let mut raw = hex::decode(&token).expect("lowercase hex");
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        let tampered = hex::encode(&raw);
        assert_ne!(tampered, token);
        let err = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &tampered)
            .expect_err("tampered tag rejected");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }
    #[test]
    fn signed_cursor_rejects_other_user() {
        let endpoint = EndpointId::UsersRead;
        let filter = CursorFilterCtx::empty();
        let key = CursorKey::Uuid(fixture_uuid_v4());
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, key);
        decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token).expect("same user");
        let err = decode_signed_cursor(&SECRET, endpoint, OTHER_ACTOR, &filter, &token)
            .expect_err("other user rejected");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }
    #[test]
    fn signed_cursor_rejects_changed_filter() {
        let endpoint = EndpointId::RolesRead;
        let filter_asc = CursorFilterCtx::from_ordered_fields(&[(1u8, Some(&b"asc"[..]))])
            .expect("fixture filter");
        let filter_desc = CursorFilterCtx::from_ordered_fields(&[(1u8, Some(&b"desc"[..]))])
            .expect("fixture filter");
        let key = CursorKey::Uuid(fixture_uuid_v4());
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter_asc, key);
        decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter_asc, &token).expect("same filter");
        let err = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter_desc, &token)
            .expect_err("changed filter value rejected");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }
    #[test]
    fn signed_cursor_rejects_cross_endpoint() {
        let endpoint = EndpointId::UsersRead;
        let filter = CursorFilterCtx::empty();
        let key = CursorKey::Uuid(fixture_uuid_v4());
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, key);
        decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token).expect("same endpoint");
        let err = decode_signed_cursor(&SECRET, EndpointId::RolesRead, ACTOR, &filter, &token)
            .expect_err("cross endpoint rejected");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }
    /// 用真实 `management_cursor_mac` 对任意 payload 重新签名：只改字段，不改 MAC 算法与 tag 截断规则。
    fn resign(payload: &[u8], endpoint: EndpointId, filter: &CursorFilterCtx) -> String {
        let mac = management_cursor_mac(&SECRET, endpoint_cursor_code(endpoint), &ACTOR, &filter.canonical(), payload);
        let mut raw = payload.to_vec();
        raw.extend_from_slice(&mac[..CURSOR_TAG_LEN]);
        hex::encode(&raw)
    }
    #[test]
    fn signed_cursor_rejects_invalid_input_shapes() {
        let endpoint = EndpointId::UsersRead;
        let filter = CursorFilterCtx::empty();
        let key = CursorKey::Uuid(fixture_uuid_v4());
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, key.clone());
        let control = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token).expect("control token");
        assert_eq!(control, key);
        let upper = token.to_uppercase();
        assert!(upper.bytes().any(|b| (b'A'..=b'F').contains(&b)), "大写样本须真含 A-F");
        for bad in [token[..CURSOR_UUID_HEX - 1].to_owned(), format!("{token}00"), upper, format!("g{}", &token[1..])] {
            let err = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &bad)
                .expect_err("invalid input shape rejected");
            assert!(matches!(err, ControllerError::InvalidArgument));
        }
    }
    #[test]
    fn signed_cursor_rejects_resigned_malformed_fields() {
        let endpoint = EndpointId::UsersRead;
        let filter = CursorFilterCtx::from_ordered_fields(&[(1u8, Some(&b"asc"[..]))]).expect("fixture filter");
        let key = CursorKey::Uuid(fixture_uuid_v4());
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, key);
        decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token).expect("valid token");
        let raw = hex::decode(&token).expect("lowercase hex");
        let (payload, tag) = raw.split_at(raw.len() - CURSOR_TAG_LEN);
        assert_eq!(payload.len() + tag.len(), CURSOR_UUID_HEX / 2);
        assert!(
            matches!(decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &resign(payload, endpoint, &filter)), Ok(CursorKey::Uuid(_))),
            "resign 未篡改 payload 必须仍可解码，否则 helper 没走真实 MAC"
        );
        // (payload 下标, 篡改值)：reserved / 4B 填充 / 未知 kind / kind=2 却 24B / UUID version / variant。
        for (index, value) in [(3usize, 1u8), (20, 1), (2, 3), (2, CURSOR_KIND_DEVICE), (10, 0x34), (12, 0x03)] {
            let mut mutated = payload.to_vec();
            mutated[index] = value;
            assert_ne!(mutated, payload, "mutation {index} 未改变 payload");
            let err = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &resign(&mutated, endpoint, &filter))
                .expect_err("resigned malformed payload rejected");
            assert!(matches!(err, ControllerError::InvalidArgument));
        }
        // payload[0..2] 端点码：改成另一端点序号后用当前端点的正确 MAC 重签，仍须失败。
        let mut cross = payload.to_vec();
        cross[..2].copy_from_slice(&endpoint_cursor_code(EndpointId::RolesRead).to_be_bytes());
        assert_ne!(cross, payload);
        let err = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &resign(&cross, endpoint, &filter))
            .expect_err("payload endpoint code rejected");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }
    #[test]
    fn signed_cursor_filter_context_rejects_noncanonical_fields() {
        let oversized = vec![0u8; u16::MAX as usize + 1];
        let cases: Vec<Vec<(u8, Option<&[u8]>)>> = vec![
            vec![(0u8, Some(&b"x"[..]))],
            vec![(1u8, Some(&b"x"[..])), (1u8, Some(&b"y"[..]))],
            vec![(2u8, None), (1u8, None)],
            vec![(1u8, Some(oversized.as_slice()))],
        ];
        for fields in cases {
            let err = CursorFilterCtx::from_ordered_fields(&fields).expect_err("non-canonical filter rejected");
            assert!(matches!(err, ControllerError::InvalidArgument));
        }
        // 合法 fixed-tag：None 写单个 0，Some 写 tag ‖ len(u16 BE) ‖ bytes，顺序即构造顺序。
        let filter = CursorFilterCtx::from_ordered_fields(&[(1u8, None), (2u8, Some(&b"asc"[..]))]).expect("canonical filter");
        assert_eq!(filter.canonical(), vec![0u8, 2u8, 0u8, 3u8, b'a', b's', b'c']);
        assert!(CursorFilterCtx::empty().canonical().is_empty());
        let endpoint = EndpointId::RolesRead;
        let key = CursorKey::Uuid(fixture_uuid_v4());
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, key.clone());
        assert_eq!(token.len(), CURSOR_UUID_HEX);
        let control = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token).expect("canonical filter roundtrip");
        assert_eq!(control, key);
        let other = CursorFilterCtx::from_ordered_fields(&[(1u8, None), (2u8, Some(&b"asc"[..])), (3u8, None)]).expect("other filter");
        assert_ne!(other.canonical(), filter.canonical());
        let err = decode_signed_cursor(&SECRET, endpoint, ACTOR, &other, &token).expect_err("changed filter context rejected");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }
}
```
- **hardening 组 RED 说明**：`signed_cursor_filter_context_rejects_noncanonical_fields` 的构造侧断言（`from_ordered_fields` 四类非法 + `canonical` 字节账）不经过 codec 函数对，只替换上方 RED 桩时这部分仍会 GREEN；该用例的 RED 由同一用例内整合的合法 encode/decode 对照与换 filter 上下文负向提供。若要让构造侧断言本身变 RED，必须另把 `from_ordered_fields` 换成故意接受非规范输入的桩——不得宣称「只换 codec 桩就让 8 个用例全部 RED」。
- **selectors（未来命令，本轮不执行；列本 mod 全部 8 个，原 5 个名称不变）**：`CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR="$PWD/.cargo-target" cargo test --offline --locked -p rsetup-controller api::signed_cursor_tests::signed_cursor_roundtrip_encodes_and_decodes_bound_key`，末段依次替换为 `signed_cursor_rejects_tampered_tag_with_invalid_argument`、`signed_cursor_rejects_other_user`、`signed_cursor_rejects_changed_filter`、`signed_cursor_rejects_cross_endpoint`、`api::signed_cursor_tests::signed_cursor_rejects_invalid_input_shapes`、`api::signed_cursor_tests::signed_cursor_rejects_resigned_malformed_fields`、`api::signed_cursor_tests::signed_cursor_filter_context_rejects_noncanonical_fields`，每条 selector 恰好运行 1 个测试。

#### cursor 密钥生产链（代码定义）

- **唯一来源、owner、顺序与语义**：32B 密钥唯一来源是环境变量 `CONTROLLER_MANAGEMENT_CURSOR_SECRET_HEX`（全计划唯一名称；只接受恰 64 个小写 hex 字符并 decode 成 32B，缺失/非法/长度不符一律固定去敏 `ControllerError::Config`，错误串不回显输入片段，任何路径不得打印或记录取值）。既有 `ControllerConfig` 只有 `database_url`/`listen_address`，**不会**自动读取该变量，本计划也不给它加字段；`ManagementState` 与各资源 `ApiState` 不扩密钥字段。读取与安装只发生在**集成 Task 0**（唯一顶层装配 owner，见本文「唯一顶层装配归集成 Task 0」）注册管理 router **之前**：`load_management_cursor_secret_from_env()` → `install_management_cursor_secret()` → `build_management_router()`，任一步 `Err` 即管理 router 不注册、进程失败关闭，绝不以「无 cursor 密钥」降级启动；Task 13 只生产 adapter/state，不得加载或持有该密钥。**Files/owner**：下列定义由未来实现任务写入 `crates/rsetup-controller/src/api/mod.rs`（Task 6 已在 `lib.rs` 注册 `pub mod api;`），集成 Task 0 只改其既有唯一顶层装配文件 `crates/rsetup-controller/src/main.rs` 调用安装；本小节只修文档，不改真代码。安装语义选定为：未安装→写入成功；重复安装同一 key→幂等 `Ok`；重复安装不同 key→`Config` 失败并保留既有 key（`OnceLock::set` 返回的 `Err` 一律显式处理，不吞掉）。生产 key 不硬编码、不写入代码常量。

```rust
// crates/rsetup-controller/src/api/mod.rs
use std::sync::OnceLock;
pub fn parse_management_cursor_secret(raw: &str) -> Result<[u8; 32], ControllerError> {
    if raw.len() != 64 || !raw.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(ControllerError::Config("management cursor secret format".into()));
    }
    let mut secret = [0u8; 32];
    hex::decode_to_slice(raw, &mut secret).map_err(|_| ControllerError::Config("management cursor secret format".into()))?;
    Ok(secret)
}
pub fn load_management_cursor_secret_from_env() -> Result<[u8; 32], ControllerError> {
    let raw = std::env::var("CONTROLLER_MANAGEMENT_CURSOR_SECRET_HEX").map_err(|_| ControllerError::Config("management cursor secret missing".into()))?;
    parse_management_cursor_secret(&raw)
}
pub(crate) static MANAGEMENT_CURSOR_SECRET: OnceLock<[u8; 32]> = OnceLock::new();
fn install_cursor_secret_into(slot: &OnceLock<[u8; 32]>, secret: &[u8; 32]) -> Result<(), ControllerError> {
    // OnceLock::set 的 Err 返回的是被拒绝的本次入参而非已安装值，拿返回值比较恒为真、会误接受不同 key。
    // 保留一次性原子安装语义：失败后经 slot.get() 比较（set 失败即已填充，get 保证可读，无 clear/reset），
    // 仅已安装值与本次入参相等才幂等成功，否则固定去敏 Config，旧 key 保持不变。
    if slot.set(*secret).is_ok() {
        return Ok(());
    }
    match slot.get() {
        Some(installed) if installed == secret => Ok(()),
        _ => Err(ControllerError::Config("management cursor secret already installed".into())),
    }
}
pub fn install_management_cursor_secret(secret: &[u8; 32]) -> Result<(), ControllerError> {
    install_cursor_secret_into(&MANAGEMENT_CURSOR_SECRET, secret)
}
fn cursor_secret_of<'a>(slot: &'a OnceLock<[u8; 32]>) -> Result<&'a [u8; 32], ControllerError> {
    slot.get().ok_or_else(|| ControllerError::Config("management cursor secret not installed".into()))
}
// handler 侧唯一取密钥入口；未安装 → Config → 既有 map_error 精确 503 STORAGE_UNAVAILABLE，绝不回退未签名 cursor。
pub(crate) fn management_cursor_secret() -> Result<&'static [u8; 32], ControllerError> {
    cursor_secret_of(&MANAGEMENT_CURSOR_SECRET)
}
```

```rust
// 集成 Task 0 既有唯一装配文件（src/main.rs）中的固定顺序（示意，非本计划落地代码）
let secret = rsetup_controller::api::load_management_cursor_secret_from_env()?;
rsetup_controller::api::install_management_cursor_secret(&secret)?;
let management = rsetup_controller::api::build_management_router(management_state);
```

```rust
#[cfg(test)]
mod cursor_secret_tests {
    use super::*;
    const SYNTHETIC_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    #[test]
    fn parse_management_cursor_secret_rejects_invalid_input() {
        assert_eq!(parse_management_cursor_secret(SYNTHETIC_HEX).expect("64 位小写 hex 必须通过"), { let mut k = [0u8; 32]; k[31] = 1; k });
        // SYNTHETIC_HEX 仅含数字 0/1，to_uppercase 仍返回同一合法值，不能充当大写拒绝样本；
        // 改用确实 64 字符且含大写 A-F 的 synthetic hex，并断言其形状以免未来退化。
        let uppercase_hex = "A".repeat(64);
        assert_eq!(uppercase_hex.len(), 64, "大写样本必须恰好 64 字符");
        assert!(uppercase_hex.chars().any(|c| matches!(c, 'A'..='F')), "大写样本必须至少含一个大写 hex 字符");
        let bad = [SYNTHETIC_HEX[..63].to_owned(), uppercase_hex, format!("{SYNTHETIC_HEX}0"), format!("{}g", &SYNTHETIC_HEX[..63]), "z".repeat(64), String::new()];
        for case in &bad {
            let err = parse_management_cursor_secret(case).expect_err("非法输入必须拒绝");
            assert!(matches!(&err, ControllerError::Config(m) if m == "management cursor secret format") && (case.len() < 8 || !format!("{err}").contains(&case[..8])), "非法输入须固定去敏拒绝且不回显密钥片段");
        }
    }
    #[test]
    fn cursor_secret_accessor_requires_installed_slot() {
        let err = cursor_secret_of(&OnceLock::<[u8; 32]>::new()).expect_err("未安装必须失败");
        assert!(matches!(&err, ControllerError::Config(m) if m == "management cursor secret not installed"));
        assert_eq!(map_error(err).status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }
    #[test]
    fn install_cursor_secret_is_idempotent_only_for_same_key() {
        let (slot, key) = (OnceLock::<[u8; 32]>::new(), parse_management_cursor_secret(SYNTHETIC_HEX).expect("合法 key"));
        assert!(install_cursor_secret_into(&slot, &key).is_ok());
        assert!(install_cursor_secret_into(&slot, &key).is_ok(), "同 key 重复安装应幂等成功");
        let err = install_cursor_secret_into(&slot, &[7u8; 32]).expect_err("不同 key 必须失败");
        assert!(matches!(&err, ControllerError::Config(m) if m == "management cursor secret already installed"));
        assert_eq!(cursor_secret_of(&slot).expect("已安装"), &key, "不同 key 不得覆盖既有 key");
    }
}
```

- **测试与 RED/GREEN（`api/mod.rs` 文末 `#[cfg(test)] mod cursor_secret_tests`，与既有 `mod tests` 并列；本任务不执行命令）**：安装/读取语义经**局部 `OnceLock` 注入的私有 helper**（`install_cursor_secret_into`/`cursor_secret_of`）验证，生产 wrapper 调用同一 helper，避免全局静态互相污染；不新增 clear/reset 静态、不加弱 secret 回退、不在测试里 `set_var`（loader 只做 `var()` + 已被 parse 测试覆盖的解码）；`SYNTHETIC_HEX` 是仅测试内使用的固定 synthetic key，不引用任何 secret 文件。RED 前置桩为 `parse_management_cursor_secret` 恒返回 `[0u8; 32]`、`cursor_secret_of` 恒 `Ok`、`install_cursor_secret_into` 恒 `Ok(())`，三条测试均因行为断言失败记 RED，不得是编译错误；命令 `CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR="$PWD/.cargo-target" cargo test --offline --locked -p rsetup-controller api::cursor_secret_tests::<name>`，`<name>` 依次取上述三个测试名（每条恰好运行 1 个测试）。

#### HTTP 边界 cursor helper（代码定义）

- **owner 与落点**：下列两个 `pub(crate)` HTTP 边界 helper 及其私有校验 helper 由未来实现任务写入 `crates/rsetup-controller/src/api/mod.rs`（与上方 codec、「cursor 密钥生产链」同文件同模块，供各资源 handler 模块共用）；本小节只修文档，不改真代码，零新增 Cargo 依赖（`hex`、`serde_json` 已在本 crate 声明）。handler 不得直接调用 `encode_signed_cursor`/`decode_signed_cursor` 裸函数对，只经本小节两个 helper；helper 参数一律显式传入 `(secret, endpoint, user_id, filter)`，不引入任何 state 参数、不扩展 `ApiState`；真实 handler 接线与其 HTTP 测试属下一独立任务，本小节不声称当前已有 handler 代码或 no-key 503 测试。

- **内部 endpoint → profile 校验 helper（唯一真源，失败关闭）**：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManagementCursorProfile {
    Uuid,
    Device,
}

pub(crate) fn management_cursor_profile(endpoint: EndpointId) -> Result<ManagementCursorProfile, ControllerError> {
    match endpoint {
        EndpointId::UsersRead | EndpointId::RolesRead | EndpointId::GrantsRead | EndpointId::GroupsRead => Ok(ManagementCursorProfile::Uuid),
        EndpointId::DevicesRead | EndpointId::ApprovalsRead => Ok(ManagementCursorProfile::Device),
        _ => Err(ControllerError::Config("management cursor endpoint profile".to_string())),
    }
}
```

- **封闭白名单**：只有 6 个管理分页读端点有 profile；`AuditRead`（Task 3 独立 MAC profile 的 `encode_audit_cursor`/`decode_audit_cursor`，本单元不覆盖）、`EffectivePermissionsRead`（独立分页 contract，见「真实管理读取补全」区）、`Me`/`Sessions`/`DeviceDetailRead`/`SystemStatusRead`/`TaskList`/`TaskDetail`/`TaskChildren`/`DeviceStatusRead` 一律落 `_ =>` 固定去敏 `Config`（经 `map_error` 精确 503），不得写 `_ => Uuid` 之类默认兜底掩盖装配错误。

- **入口 helper（HTTP `cursor` → 网关 typed key）**：`None` 直接 `Ok(None)`（不触碰 MAC）；`Some` 必须经真实 `decode_signed_cursor`，再校验解码结果落在本 endpoint 预期 profile，profile 不符或 token 非法一律 `ControllerError::InvalidArgument` → 400，无回退、不降级全量。

```rust
pub(crate) fn decode_management_list_cursor(secret: &[u8; 32], endpoint: EndpointId, user_id: [u8; 16], filter: &CursorFilterCtx, token: Option<&str>) -> Result<Option<CursorKey>, ControllerError> {
    let profile = management_cursor_profile(endpoint)?;
    let raw = match token {
        None => return Ok(None),
        Some(raw) => raw,
    };
    let key = decode_signed_cursor(secret, endpoint, user_id, filter, raw)?;
    let bound = matches!(
        (profile, &key),
        (ManagementCursorProfile::Uuid, CursorKey::Uuid(_)) | (ManagementCursorProfile::Device, CursorKey::DeviceKey(_))
    );
    if !bound {
        return Err(ControllerError::InvalidArgument);
    }
    Ok(Some(key))
}
```

- **出口 helper（网关内部 raw key → 外部签名 token）**：只接受 `serde_json::Value::Object`，`items` 必须是 array，`next_cursor` 字段必须存在且为 `null` 或 `String`；内部 raw string 严格小写 hex 且长度恰 32（UUID profile → 16B）或 64（设备 profile → 32B），UUID 另校验 canonical v4 shape；任何后端 malformed/缺字段/错 profile/长度不符一律固定去敏 `ControllerError::Config`（经 `map_error` 精确 503），不得返回 400 掩盖后端损坏，不回显输入片段，不 `unwrap`/截断/panic/日志 echo。

```rust
fn management_page_shape_error() -> ControllerError {
    ControllerError::Config("management cursor page shape".to_string())
}

fn management_page_raw_bytes<const N: usize>(raw: &str) -> Option<[u8; N]> {
    if raw.len() != N * 2 || !raw.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let bytes = hex::decode(raw).ok()?;
    bytes.as_slice().try_into().ok()
}

fn management_page_internal_key(endpoint: EndpointId, raw: &str) -> Result<CursorKey, ControllerError> {
    match management_cursor_profile(endpoint)? {
        ManagementCursorProfile::Uuid => {
            let uuid = management_page_raw_bytes::<16>(raw).ok_or_else(management_page_shape_error)?;
            if uuid[6] >> 4 != 4 || uuid[8] & 0xC0 != 0x80 {
                return Err(management_page_shape_error());
            }
            Ok(CursorKey::Uuid(uuid))
        }
        ManagementCursorProfile::Device => {
            let key = management_page_raw_bytes::<32>(raw).ok_or_else(management_page_shape_error)?;
            Ok(CursorKey::DeviceKey(key))
        }
    }
}

pub(crate) fn sign_management_list_page(secret: &[u8; 32], endpoint: EndpointId, user_id: [u8; 16], filter: &CursorFilterCtx, page: serde_json::Value) -> Result<serde_json::Value, ControllerError> {
    management_cursor_profile(endpoint)?;
    let mut map = match page {
        serde_json::Value::Object(map) => map,
        _ => return Err(management_page_shape_error()),
    };
    if !matches!(map.get("items"), Some(serde_json::Value::Array(_))) {
        return Err(management_page_shape_error());
    }
    let raw_next = match map.get("next_cursor") {
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(raw)) => Some(raw.clone()),
        _ => return Err(management_page_shape_error()),
    };
    let next_cursor = match raw_next {
        None => serde_json::Value::Null,
        Some(raw) => {
            let key = management_page_internal_key(endpoint, &raw)?;
            serde_json::Value::String(encode_signed_cursor(secret, endpoint, user_id, filter, key))
        }
    };
    map.insert("next_cursor".to_string(), next_cursor);
    Ok(serde_json::Value::Object(map))
}
```

- **出口白名单校验（与入口对称）**：`sign_management_list_page` 第一行必须先执行 `management_cursor_profile(endpoint)?`，与入口 `decode_management_list_cursor` 首行的同一道封闭白名单成对；该校验发生在 `page` 形状判断与 `next_cursor` 分支之前，因此 `next_cursor` 为 `null`（`None` 分支）同样被拦截，不得只在 `Some(raw)` 分支经 `management_page_internal_key` 间接校验后放行。白名单为封闭 6 端点：`UsersRead`/`RolesRead`/`GrantsRead`/`GroupsRead` → `Uuid` profile，`DevicesRead`/`ApprovalsRead` → `Device` profile，其余端点落 `_ =>` 固定去敏 `Config` 503，不得写默认兜底。**devices 权限例外**：`DevicesRead` 进入白名单只表示它拥有合法的 `Device` cursor profile，不表示它走本 admin 通用链路——`GET /api/v1/devices` 允许普通用户按可见范围读取（`list_devices_visible`，spec01 rebootOnly 的最小 devices 列表例外由 Task 10 单独处理，Task 6 通用顺序描述不因此修改）；白名单 membership 与权限模型是两件事，不得据此放宽任何端点的白名单，也不得因 devices 有权限例外而省略出口白名单校验。
- **投影守恒**：只替换 `next_cursor` 一个字段（`null` 保持 `null`，`String` 换成签名后的 `String`），`items` 与其余投影字段原样保留；不新增 HTTP 字段、不删除字段、不把裸内部 key 或 `CursorKey` 的 Debug 写进响应。
- **失败分层（fail closed）**：客户端侧（非法/篡改/跨端点/跨用户/换筛选 token、profile 不符）→ `InvalidArgument` 400；后端侧（page 非 object、缺 `items`、`items` 非 array、缺 `next_cursor`、`next_cursor` 非 `null`/`String`、raw 长度或大小写或字节数或 v4 shape 不合法、endpoint 不在白名单）→ 固定去敏 `Config` 503 `STORAGE_UNAVAILABLE`；endpoint 不在白名单一律 503，**含 `next_cursor` 为 `null` 的场景**——出口第一行的 `management_cursor_profile(endpoint)?` 与 `next_cursor` 取值无关，非白名单端点不得因 `None` 分支直接返回未签名的原始 page；两条路径都不得返回未校验的内部 `CursorKey`。
- **转换锚点（两 profile 长度账）**：UUID profile 内部 raw 32 小写 hex（16B）→ 外部 64 小写 hex（24B payload + 8B tag）；设备 profile 内部 raw 64 小写 hex（32B）→ 外部 96 小写 hex（40B payload + 8B tag）；内部长度与 endpoint profile 互锁，写错端点即 503 而非跨 profile 误签。
- **下一 handler 切片的真实调用顺序（必要前置，本小节不声称已有 handler 代码或 no-key HTTP 测试）**：`http_gate::authenticate_read` → `require_admin_session` → 严格 query 规范化出 `CursorFilterCtx`（`limit` 不参与绑定）→ `management_cursor_secret()`（即使 `cursor` 为 `None` 也必须先取密钥，未安装即 `Config` → 精确 503，不得因 `None` 短路跳过）→ `decode_management_list_cursor` → 以 typed `CursorKey` 调 gateway `list`（`Uuid` → `Option<[u8; 16]>`/`Option<Uuid>`，`DeviceKey` → `Option<[u8; 32]>`）→ `sign_management_list_page` → `ok_response`；入口与出口必须复用同一请求内同一规范化 `CursorFilterCtx` 实例，否则下一页 token 解不回。
- **dead_code 说明**：两个 `pub(crate)` helper 与私有 helper 在 handler 同批接线前只产生 `dead_code` 警告，属预期，不新增 `#[allow(dead_code)]` 之类 lint 抑制，GREEN 由 handler 切片真实调用消费。
- **不改动项**：codec 函数对、「cursor 密钥生产链」、`ManagementState` 与各资源 `ApiState` 契约、`EndpointId` 与 gateway trait 签名、Task 3 audit 独立 MAC profile、fixtures 与管理适配器子计划文件全部不变；本单元不写 handler、不写测试、不新增 selector。

`api/mod.rs` 完整状态与装配定义（只组合管理子router，不组合auth/tasks/status/assets或探活，不改旧AppState；顶层归独立集成owner）：

```rust
pub mod users;
pub mod roles;
pub mod grants;
pub mod groups;
pub mod devices;
pub mod approvals;
pub mod audit;
pub mod system;

use std::sync::Arc;
use crate::http_auth::HttpAuthState;
use self::users::{UsersApiState, UsersGateway};
use self::roles::{RolesApiState, RolesGateway};
use self::grants::{GrantsApiState, GrantsGateway};
use self::groups::{GroupsApiState, GroupsGateway};
use self::devices::{DevicesApiState, DevicesGateway, DeviceLiveProbe};
use self::approvals::{ApprovalsApiState, ApprovalsGateway, ManagementNotificationSink};
use self::audit::{AuditApiState, AuditGateway};
use self::system::{SystemApiState, SystemGateway, RecoveryModeProbe};

#[derive(Clone)]
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

pub fn build_management_router(state: ManagementState) -> axum::Router<()> {
    users::users_router(UsersApiState { auth: state.auth.clone(), gateway: state.users })
        .merge(roles::roles_router(RolesApiState { auth: state.auth.clone(), gateway: state.roles }))
        .merge(grants::grants_router(GrantsApiState { auth: state.auth.clone(), gateway: state.grants }))
        .merge(groups::groups_router(GroupsApiState { auth: state.auth.clone(), gateway: state.groups }))
        .merge(devices::devices_router(DevicesApiState {
            auth: state.auth.clone(), gateway: state.devices, live_probe: state.device_probe.clone(),
        }))
        .merge(approvals::approvals_router(ApprovalsApiState {
            auth: state.auth.clone(), gateway: state.approvals, notification_sink: state.notification_sink,
            probe: state.device_probe,
        }))
        .merge(audit::audit_router(AuditApiState { auth: state.auth.clone(), gateway: state.audit }))
        .merge(system::system_router(SystemApiState {
            auth: state.auth, gateway: state.system, recovery_probe: state.recovery_probe,
        }))
}
```

八个资源模块分别声明对应ApiState，复用各文件的Arc/HttpAuthState导入，不通过AppState/FromRef隐式注入业务字段。以下声明分别落入标注文件，不将八个struct复制进同一模块：

```rust
// api/users.rs
#[derive(Clone)]
pub struct UsersApiState { pub auth: Arc<HttpAuthState>, pub gateway: Arc<dyn UsersGateway> }
// api/roles.rs
#[derive(Clone)]
pub struct RolesApiState { pub auth: Arc<HttpAuthState>, pub gateway: Arc<dyn RolesGateway> }
// api/grants.rs
#[derive(Clone)]
pub struct GrantsApiState { pub auth: Arc<HttpAuthState>, pub gateway: Arc<dyn GrantsGateway> }
// api/groups.rs
#[derive(Clone)]
pub struct GroupsApiState { pub auth: Arc<HttpAuthState>, pub gateway: Arc<dyn GroupsGateway> }
// api/audit.rs
#[derive(Clone)]
pub struct AuditApiState { pub auth: Arc<HttpAuthState>, pub gateway: Arc<dyn AuditGateway> }
// api/devices.rs
#[derive(Clone)]
pub struct DevicesApiState {
    pub auth: Arc<HttpAuthState>, pub gateway: Arc<dyn DevicesGateway>,
    pub live_probe: Arc<dyn DeviceLiveProbe>,
}
// api/approvals.rs
#[derive(Clone)]
pub struct ApprovalsApiState {
    pub auth: Arc<HttpAuthState>, pub gateway: Arc<dyn ApprovalsGateway>,
    pub notification_sink: Arc<dyn ManagementNotificationSink>,
    // 审批 connection_state 的唯一消费通道：与子计划A `SqlxManagementGateway.probe` 同名同类型，复用本计划既有 `DeviceLiveProbe`，不新造第二套探针类型。
    // 装配只传 `ManagementState.device_probe` 同一真实探针实例（与 devices 侧同源）；禁止常量 offline 源、Option 缺省或 fixture 默认值，缺真实探针不装配生产 router。
    pub probe: Arc<dyn DeviceLiveProbe>,
}
// api/system.rs
#[derive(Clone)]
pub struct SystemApiState {
    pub auth: Arc<HttpAuthState>, pub gateway: Arc<dyn SystemGateway>,
    pub recovery_probe: Arc<dyn RecoveryModeProbe>,
}
```

完整网关契约如下（每个trait落入标注资源文件，所有方法显式借用actor；`Value`是已校验HTTP响应投影，不是让HTTP传任意SQL/任意操作的入口。设备详情保持单snapshot typed返回，不改为Value）：

```rust
// api/mod.rs；八资源模块使用super::ManagementFuture，并导入Session/Uuid/Permission。
pub type ManagementFuture<'a, T> = crate::authz::sqlx_repo::RepoFuture<'a, T>;

// api/users.rs
pub trait UsersGateway: Send + Sync {
    fn list<'a>(&'a self, actor: &'a Session, cursor: Option<[u8; 16]>, limit: usize) -> ManagementFuture<'a, serde_json::Value>;
    fn create<'a>(&'a self, actor: &'a Session, username: &'a str, display_name: &'a str, is_admin: bool) -> ManagementFuture<'a, serde_json::Value>;
    fn patch<'a>(&'a self, actor: &'a Session, id: [u8; 16], revision: u64, active: Option<bool>, is_admin: Option<bool>, display_name: Option<&'a str>) -> ManagementFuture<'a, serde_json::Value>;
    fn reset_password<'a>(&'a self, actor: &'a Session, id: [u8; 16], revision: u64) -> ManagementFuture<'a, serde_json::Value>;
    fn effective_permissions<'a>(&'a self, actor: &'a Session, target: [u8; 16], device_id: Option<[u8; 32]>, cursor: Option<[u8; 32]>, limit: usize) -> ManagementFuture<'a, serde_json::Value>;
}
// api/roles.rs
pub trait RolesGateway: Send + Sync {
    fn list<'a>(&'a self, actor: &'a Session, cursor: Option<Uuid>, limit: usize) -> ManagementFuture<'a, serde_json::Value>;
    fn create<'a>(&'a self, actor: &'a Session, name: &'a str, permissions: &'a [Permission]) -> ManagementFuture<'a, serde_json::Value>;
    fn patch<'a>(&'a self, actor: &'a Session, id: Uuid, revision: u64, name: Option<&'a str>, permissions: Option<&'a [Permission]>, archived: Option<bool>) -> ManagementFuture<'a, serde_json::Value>;
}
// api/grants.rs；GrantSource/GrantScope来自唯一policy定义，不另造权限点。
pub trait GrantsGateway: Send + Sync {
    fn list<'a>(&'a self, actor: &'a Session, user_id: Option<[u8; 16]>, cursor: Option<Uuid>, limit: usize) -> ManagementFuture<'a, serde_json::Value>;
    fn create<'a>(&'a self, actor: &'a Session, user_id: [u8; 16], source: GrantSource, scope: GrantScope) -> ManagementFuture<'a, serde_json::Value>;
    fn delete<'a>(&'a self, actor: &'a Session, id: Uuid, revision: u64) -> ManagementFuture<'a, ()>;
}
// api/groups.rs
pub trait GroupsGateway: Send + Sync {
    fn list<'a>(&'a self, actor: &'a Session, cursor: Option<Uuid>, limit: usize) -> ManagementFuture<'a, serde_json::Value>;
    fn create<'a>(&'a self, actor: &'a Session, name: &'a str) -> ManagementFuture<'a, serde_json::Value>;
    fn patch<'a>(&'a self, actor: &'a Session, id: Uuid, revision: u64, name: Option<&'a str>, archived: Option<bool>) -> ManagementFuture<'a, serde_json::Value>;
    fn replace_members<'a>(&'a self, actor: &'a Session, id: Uuid, revision: u64, devices: &'a [[u8; 32]]) -> ManagementFuture<'a, serde_json::Value>;
}
// api/devices.rs；DeviceAdminView/HashSet来自repository/std，列表筛选不得静默忽略。
pub trait DevicesGateway: Send + Sync {
    fn list_devices_visible<'a>(&'a self, actor: &'a Session, cursor: Option<[u8; 32]>, limit: usize, group_id: Option<Uuid>, search: Option<&'a str>) -> ManagementFuture<'a, serde_json::Value>;
    fn find_device<'a>(&'a self, actor: &'a Session, id: [u8; 32]) -> ManagementFuture<'a, Option<(DeviceAdminView, HashSet<Permission>)>>;
    fn patch<'a>(&'a self, actor: &'a Session, id: [u8; 32], revision: u64, display_name: &'a str) -> ManagementFuture<'a, serde_json::Value>;
}
// api/approvals.rs
#[derive(Clone, Copy, Debug)]
pub struct ApprovalTarget { pub device_id: [u8; 32], pub expected_revision: u64 }
pub trait ApprovalsGateway: Send + Sync {
    fn list<'a>(&'a self, actor: &'a Session, decision: Option<ReviewDecision>, cursor: Option<[u8; 32]>, limit: usize) -> ManagementFuture<'a, serde_json::Value>;
    fn approve<'a>(&'a self, actor: &'a Session, targets: &'a [ApprovalTarget]) -> ManagementFuture<'a, serde_json::Value>;
    fn reject<'a>(&'a self, actor: &'a Session, targets: &'a [ApprovalTarget], reason: &'a str) -> ManagementFuture<'a, serde_json::Value>;
    fn reopen<'a>(&'a self, actor: &'a Session, id: [u8; 32], revision: u64) -> ManagementFuture<'a, serde_json::Value>;
    fn revoke<'a>(&'a self, actor: &'a Session, id: [u8; 32], revision: u64, reason: &'a str) -> ManagementFuture<'a, serde_json::Value>;
    fn reauthorize<'a>(&'a self, actor: &'a Session, id: [u8; 32], revision: u64) -> ManagementFuture<'a, serde_json::Value>;
}
// api/audit.rs；AuditCursor复合键由Task3审计游标段定义，不能仅event_seq。
pub trait AuditGateway: Send + Sync {
    fn list<'a>(&'a self, actor: &'a Session, filter: AuditFilter, cursor: Option<AuditCursor>, limit: usize) -> ManagementFuture<'a, serde_json::Value>;
}
// api/system.rs；即使只是统计也必须把actor送到仓储当前管理员检查。
pub trait SystemGateway: Send + Sync {
    fn status<'a>(&'a self, actor: &'a Session) -> ManagementFuture<'a, serde_json::Value>;
}
```

响应Value仅由production adapter用固定投影函数构造：User无hash，Role/Group/Grant revision为十进制字符串，Page必有items/next_cursor；资源测试fixture也遵守相同形状。请求由handler严格DTO解析后变成上述参数；禁止客户端直接提交Value作为网关操作。完整adapter实现、签名传播及dyn调用测试在Task13逐项交付，本段本身不宣称接线完成。

子router用`.with_state(state)`归约为Router<()>。全部Gateway/probe/sink须Send+Sync；Gateway与异步probe用显式生命周期BoxFuture、不用async-fn trait再转dyn；ManagementNotificationSink保持与既有同步AdmissionChangeSink兼容的typed Result返回。集成owner显式提供所有真实字段，不提供默认常量源或Option缺省；缺真实源不挂载，整Task4未完成。骨架子router为空是类型前置，不计为API验收。

- [ ] **Step 1: 编写失败测试（解析器边界）**

```rust
// crates/rsetup-controller/src/api/mod.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_parser_fails_closed_on_plus_prefix_and_overflow() {
        assert_eq!(parse_revision("42").unwrap(), 42);
        assert_eq!(parse_revision(&u64::MAX.to_string()).unwrap(), u64::MAX);
        assert!(parse_revision("+42").is_err(), "leading plus is forbidden");
        assert!(parse_revision("").is_err(), "empty revision is forbidden");
        assert!(parse_revision("not-digits").is_err());
        assert!(parse_revision("18446744073709551616").is_err(), "u64 overflow must be 400");
    }

    #[test]
    fn limit_parser_defaults_to_50_and_caps_at_200() {
        assert_eq!(parse_limit(None).unwrap(), 50);
        assert_eq!(parse_limit(Some("100")).unwrap(), 100);
        assert_eq!(parse_limit(Some("200")).unwrap(), 200);
        assert!(parse_limit(Some("0")).is_err(), "0 is invalid");
        assert!(parse_limit(Some("201")).is_err(), ">200 is invalid");
        assert!(parse_limit(Some("abc")).is_err());
    }

    #[test]
    fn user_admin_public_never_exposes_password_hash_and_uses_decimal_revision() {
        let user = UserRecord {
            id: [0x42u8; 16], username: "admin".into(), display_name: "Root".into(),
            active: true, is_admin: true, must_change_password: false, revision: 7,
        };
        let value = crate::http_api::user_admin_public(&user);
        assert_eq!(value["revision"], "7");
        assert_eq!(value["display_name"], "Root");
        assert!(value.get("password_hash").is_none());
        assert_eq!(value["id"], "42424242-4242-4242-4242-424242424242");
    }
}
```

- [ ] **Step 2: 运行并确认行为 RED**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::tests
```
预期：运行 3 个测试；因桩未实现校验（`parse_revision` 桩恒返回 `Ok(0)`）而断言失败。

- [ ] **Step 3: 最小实现**

实现 `parse_revision`、`parse_limit`、`parse_device_id_path`、`parse_uuid_path`、`map_error`；其中 `parse_uuid_path` 直接复制上文「parse_uuid_path 唯一 producer 与 pure tests」小节的真实 body（含 `invalid_uuid_id` 与必要导入），整体替换 RED 宽松桩，本计划唯一真源，consumer 一律 `super::parse_uuid_path` 导入；在 `http_api.rs` 追加 `user_admin_public`；8 个骨架子模块各写出 trait 定义与返回空 `Router::new()` 的 `pub fn router`；`api/mod.rs::build_management_router` 合并这 8 个子路由。

- [ ] **Step 4: 运行并确认 GREEN**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::tests
```
预期：3 个测试全部 PASS。

**parse_uuid_path 唯一 producer 与 pure tests（独立单元，追加于 Step 4 之后，不改变上述 `api::tests` 3 个测试的计数含义）**

- **Owner 与唯一真源**：`pub(crate) fn parse_uuid_path` 仅实现于 `crates/rsetup-controller/src/api/mod.rs`，是全计划 UUID 文本解析的唯一 producer；所有 consumer（Task 8 grants filter 等）一律 `super::parse_uuid_path` 导入，不得新造或从其他路径导入。
- **接受域（严格，不擅扩大）**：仅接受规范小写连字符 36 字符（`8-4-4-4-12`、hex 全小写）。不要求 v4 版本位——已定接受域（Task 8 grants filter 口径）只约束文本形态；cursor `kind=1` 的 UUIDv4 约束属 codec 层字段校验，与本 helper 无关。拒：空串、大写 hex、大小写混合、`{...}` 花括号、紧凑 32 字符、`urn:uuid:` 前缀、首尾空白、尾换行、非 ASCII、任何长度/结构畸形。
- **实现（Step 3 最小实现落地，替换宽松桩；错误复用 `crate::http_api::err_response` 真实 constructor，不新造 error builder）**：

```rust
// crates/rsetup-controller/src/api/mod.rs（顶层既有 import 合并、不重复；
// 需要 `axum::http::StatusCode`、`axum::response::Response`、`uuid::Uuid`，
// 若顶层尚未导入则补 `use crate::http_api::err_response;`）

/// 唯一 UUID 文本 parser：仅接受规范小写连字符 36 字符。
/// `Uuid::try_parse` 同时接受花括号/紧凑/urn 等宽松形态，故解析成功后
/// 必须与 `to_string()` 的规范小写字符串逐字符相等，一次性闭合接受域，
/// 拒大写、花括号、紧凑、空白、尾换行、非 ASCII 与全部畸形。
pub(crate) fn parse_uuid_path(raw: &str) -> Result<Uuid, Response> {
    let parsed = Uuid::try_parse(raw).map_err(|_| invalid_uuid_id())?;
    if parsed.to_string() != raw {
        return Err(invalid_uuid_id());
    }
    Ok(parsed)
}

fn invalid_uuid_id() -> Response {
    err_response(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        "request.invalid_id",
        serde_json::json!({}),
    )
}
```

- **RED 宽松桩（Step 2 之前写入 `api/mod.rs`，可编译但行为错误）**：上述函数体替换为 `Uuid::try_parse(raw).map_err(|_| invalid_uuid_id())`（去掉 canonical 对比），签名与 `invalid_uuid_id` 保持不变——宽松桩会接受大写/花括号/紧凑/urn，负向矩阵因此断言失败。
- **pure tests（`api/mod.rs` 文末 `#[cfg(test)] mod uuid_path_tests`，与 `mod tests`、`cursor_secret_tests` 并列，不并入 `mod tests`，不影响其 3 个测试计数）**：

```rust
// crates/rsetup-controller/src/api/mod.rs（文末）
#[cfg(test)]
mod uuid_path_tests {
    use super::*;

    const CANONICAL: &str = "067e6162-ea94-4431-836c-7d8fa2010b3c";

    #[test]
    fn uuid_path_accepts_canonical_lowercase_only() {
        let parsed = parse_uuid_path(CANONICAL).expect("canonical lowercase must be accepted");
        assert_eq!(parsed.to_string(), CANONICAL);
    }

    #[tokio::test]
    async fn uuid_path_rejects_noncanonical_forms_with_400_invalid_id() {
        for raw in [
            "",
            "067E6162-EA94-4431-836C-7D8FA2010B3C",
            "067e6162-EA94-4431-836C-7D8FA2010b3c",
            "{067e6162-ea94-4431-836c-7d8fa2010b3c}",
            "067e6162ea944431836c7d8fa2010b3c",
            "urn:uuid:067e6162-ea94-4431-836c-7d8fa2010b3c",
            " 067e6162-ea94-4431-836c-7d8fa2010b3c",
            "067e6162-ea94-4431-836c-7d8fa2010b3c ",
            "067e6162-ea94-4431-836c-7d8fa2010b3c\n",
            "067e6162-ea94-4431-836c-7d8fa2010b3",
            "067e6162-ea94-4431-836c-7d8fa2010b3cc",
            "067e6162-ea94-4431-836c-7d8fa2010b3г",
        ] {
            let resp = parse_uuid_path(raw).expect_err("non-canonical form must be rejected");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "case {raw:?}");
            let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
                .await
                .expect("error body");
            let text = String::from_utf8(bytes.to_vec()).expect("utf-8 json");
            assert!(text.contains("INVALID_ARGUMENT"), "case {raw:?}: {text}");
            assert!(text.contains("request.invalid_id"), "case {raw:?}: {text}");
        }
    }
}
```

- **RED/GREEN 顺序与 selector**：Step 2 之前把宽松桩与上述测试模块一并写入 `api/mod.rs`（两处都就绪才运行，RED 不得是找不到函数或 0 测试）。RED：`CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::uuid_path_tests::uuid_path_rejects_noncanonical_forms_with_400_invalid_id` 因宽松桩接受大写/花括号/紧凑/urn 而断言失败；`api::uuid_path_tests::uuid_path_accepts_canonical_lowercase_only` 阳性对照自始 PASS。Step 3 用 canonical 对比真实实现整体替换宽松桩 body，两条各自 selector 下 GREEN。

- [ ] **Step 5: 静态检查与条件性提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/api/ crates/rsetup-controller/src/http_api.rs crates/rsetup-controller/src/lib.rs
git commit -m "feat(controller): API 路由骨架、共享错误映射器与解析辅助函数"
```

---

### 资源测试共享前置层（Task 7–12 复用）

在 `src/api/mod.rs` 文末以 `#[cfg(test)] pub(crate) mod test_fixture` 一次定义（Task 7+ 资源测试专属前置层，与子计划 A 的 `fixture()`/`request()` 名称签名不同、互不复用）；Task 7–12 各资源模块 `#[cfg(test)] mod tests` 以 `use crate::api::test_fixture::*;` 接入（同 crate 子模块可见；全部 test-only，不新增生产 API）。Task 14 的 `admin_actor`/`seed_guarded_admin` 是 DB 集成测试私有 helper，src 单测不可见，此处仅复用其 `Session` 构造形状。本层是 RED 前置：users/请求 helper 部分随 Task 7 落地并编译通过，RED 失败只能是行为断言（如 `404 == 401`），不得是编译错误；devices 部分随 Task 10 落地，避免 Task 7–9 引用未定义类型。Task 8–12 由后续修订接入本层（含把 Task 10–12 的 `authenticated_get(&router, uri)` 用法统一为 helper 返回 `Request`、测试侧 `router.oneshot`）。

**Host allowlist 统一口径**：fixture 配置 `HttpSecurityConfig::from_values(Some("localhost:3080,controller.test"), Some("http://localhost:3080"), None)`——allowlist 同时含 `localhost:3080`（Task 7–12 测试请求 Host）与 `controller.test`（Task 13 生产/子计划 A54 口径），与 Task 1 gate `allowed_hosts` 精确匹配契约（`authenticate_read`/`authenticate_write` Host 步）一致，消除"合法 Host 被 403"疑义；请求 helper 不发 Origin，按门禁契约缺失 Origin 仅豁免 Origin 步、不豁免 CSRF。

```rust
// crates/rsetup-controller/src/api/mod.rs（文末追加；devices 段随 Task 10 落地）
#[cfg(test)]
pub(crate) mod test_fixture {
    use super::*; use crate::api::users::users_router; use crate::auth::service::{IdentityUser, Login, Session, SessionPage};
    use crate::error::ControllerError; use crate::http_auth::{AuthFuture, AuthServiceApi, CsrfBinding}; use crate::http_security::HttpSecurityConfig;
    use axum::body::Body; use axum::http::{header, HeaderName, Request}; use std::time::Instant;
    pub(crate) const FIXTURE_HOST: &str = "localhost:3080"; fn hex64(fill: char) -> String { fill.to_string().repeat(64) }
    // 进程内 actor Session（同 Task 14 admin_actor 形状，is_admin 可控）
    pub(crate) fn fixture_session(is_admin: bool) -> Session { Session { user: IdentityUser { id: [7u8; 16], username: "fixture_admin".into(),
        password_hash: "fixture-placeholder".into(), active: true, is_admin, must_change_password: false, revision: 1 }, digest: [9u8; 32] } }
    // FixtureAuth：Task 1 StubAuth（http_gate.rs tests）的 test_fixture 版克隆，字段 token/session；
    // authenticate 仅当 raw == token 返回 Ok(session.clone())，is_live_session 恒 true，其余方法同 StubAuth 桩返回。
    pub(crate) struct FixtureAuth { token: String, session: Session }
    // 最小装配：合法 Host allowlist + 固定会话 + 绑定 session.digest 的 CSRF
    pub(crate) fn fixture_auth(session: Session) -> Arc<HttpAuthState> {
        let auth = Arc::new(HttpAuthState::new(Arc::new(FixtureAuth { token: hex64('a'), session: session.clone() }),
            HttpSecurityConfig::from_values(Some("localhost:3080,controller.test"), Some("http://localhost:3080"), None).unwrap()));
        auth.csrf.lock().unwrap().insert(session.digest, CsrfBinding { token: hex64('b'), issued_at: Instant::now() }); auth
    }
    // users 资源前置：返回 (Router, 测试状态)；三个入口只差 Session.is_admin / 网关行为
    pub(crate) fn users_state_with(session: Session, gateway: Arc<dyn UsersGateway>) -> (axum::Router<()>, Arc<HttpAuthState>) { let auth = fixture_auth(session); (users_router(UsersApiState { auth: auth.clone(), gateway }), auth) }
    pub(crate) fn fixture_state() -> (axum::Router<()>, Arc<HttpAuthState>) { users_state_with(fixture_session(true), Arc::new(FixtureUsersGateway::Allow)) }
    pub(crate) fn fixture_state_with_non_admin() -> (axum::Router<()>, Arc<HttpAuthState>) { users_state_with(fixture_session(false), Arc::new(FixtureUsersGateway::Allow)) }
    pub(crate) fn fixture_state_with_last_admin_rejection() -> (axum::Router<()>, Arc<HttpAuthState>) { users_state_with(fixture_session(true), Arc::new(FixtureUsersGateway::LastAdmin)) }
    // 纯内存 UsersGateway（Task 7 Step 1 所指）：Allow 返回固定投影；LastAdmin 在 patch 返回 LastAdmin
    pub(crate) enum FixtureUsersGateway { Allow, LastAdmin }
    impl UsersGateway for FixtureUsersGateway {
        fn list<'a>(&'a self, _actor: &'a Session, _c: Option<[u8; 16]>, _l: usize) -> ManagementFuture<'a, serde_json::Value> { Box::pin(async { Ok(serde_json::json!({ "items": [], "next_cursor": null })) }) }
        fn create<'a>(&'a self, _actor: &'a Session, _u: &'a str, _d: &'a str, _a: bool) -> ManagementFuture<'a, serde_json::Value> { Box::pin(async { Ok(serde_json::json!({ "user": null, "temporary_password": "fixture" })) }) }
        fn patch<'a>(&'a self, _actor: &'a Session, _id: [u8; 16], _r: u64, _ac: Option<bool>, _ia: Option<bool>, _dn: Option<&'a str>) -> ManagementFuture<'a, serde_json::Value> { let reject = matches!(self, Self::LastAdmin); Box::pin(async move { if reject { Err(ControllerError::LastAdmin) } else { Ok(serde_json::json!({})) } }) }
        fn reset_password<'a>(&'a self, _actor: &'a Session, _id: [u8; 16], _r: u64) -> ManagementFuture<'a, serde_json::Value> { Box::pin(async { Ok(serde_json::json!({ "user": null, "temporary_password": "fixture" })) }) }
        fn effective_permissions<'a>(&'a self, _actor: &'a Session, _t: [u8; 16], _d: Option<[u8; 32]>, _c: Option<[u8; 32]>, _l: usize) -> ManagementFuture<'a, serde_json::Value> { Box::pin(async { Ok(serde_json::json!({ "items": [] })) }) }
    }
    // 请求 helper：自动携带 Host/Cookie（不发 Origin，缺失仅豁免 Origin 步）；写 helper 另带 CSRF
    pub(crate) fn authenticated_get(uri: &str) -> Request<Body> { Request::builder().method("GET").uri(uri).header(header::HOST, FIXTURE_HOST)
        .header(header::COOKIE, format!("rsc_session={}", hex64('a'))).body(Body::empty()).unwrap() }
    pub(crate) fn authenticated_json_patch(uri: &str, body: serde_json::Value) -> Request<Body> { Request::builder().method("PATCH").uri(uri)
        .header(header::HOST, FIXTURE_HOST).header(header::COOKIE, format!("rsc_session={}", hex64('a')))
        .header(HeaderName::from_static("x-csrf-token"), hex64('b')).header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string())).unwrap() }
    // 响应体 JSON 解析断言 helper
    pub(crate) async fn body_json(res: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(res.into_body(), 1_048_576).await.unwrap(); serde_json::from_slice(&bytes).unwrap() }
    // fixture_devices_state() -> (axum::Router<()>, Arc<HttpAuthState>)（Task 10 devices 路由用，随 Task 10 落地）：装配同 users_state_with(fixture_session(true))，挂 devices_router(DevicesApiState { auth, gateway: Arc<FixtureDevicesGateway>, live_probe: Arc<FixtureLiveProbe> })；
    // FixtureDevicesGateway：list_devices_visible 恒 Ok({"items": [], "next_cursor": null})、find_device 恒 Ok(None)、patch 恒 Ok({})；
    // FixtureLiveProbe 恒 Ok(DeviceLiveView { connection_state: Offline, control_health: Offline, data_health: Offline, capabilities: None })；Task 10 接入时按用例参数化（缺失/不可见/可见三态）。
}
```

---

### Task 7: users 资源管理路由（5 条）

**Files:**
- Modify: `crates/rsetup-controller/src/api/users.rs`
- Test: 同文件的 `#[cfg(test)] mod tests`

**契约矩阵：**

| 路径与方法 | 权限 | 输入 | 响应 `data` 形状 | 异常状态 |
| :--- | :--- | :--- | :--- | :--- |
| `GET /api/v1/users` | admin | query `cursor?`, `limit?` | `{ items: [User], next_cursor: String? }` | 401, 403, 400 |
| `POST /api/v1/users` | admin | `{ username, display_name, is_admin }` | `{ user: User, temporary_password: String }`（一次展示） | 400, 409 `INVALID_STATE`(重名) |
| `PATCH /api/v1/users/{id}` | admin | `{ expected_revision, active?, is_admin?, display_name? }` | `User` | 404, 409 `REVISION_CONFLICT` / `LAST_ADMIN` |
| `POST /api/v1/users/{id}/reset-password` | admin | `{ expected_revision }` | `{ user: User, temporary_password: String }` | 404, 409 `REVISION_CONFLICT` |
| `GET /api/v1/users/{id}/effective-permissions` | admin | query `device_id?` | `{ items: [{ permission, source: { kind, id? } }] }` | 404, 400 |

**Interfaces:**
- Produces: `pub trait UsersGateway: Send + Sync`（5 个异步方法）；`pub fn users_router(state: UsersApiState) -> Router<()>`。

- [ ] **Step 1: 编写失败测试（未鉴权 401、非管理员 403、LastAdmin 409、重置密码带临时密码 200）**

测试全部使用「资源测试共享前置层」小节定义的纯内存 `FixtureUsersGateway` 与 helpers（不碰数据库，仅验证 HTTP handler 契约；`fixture_state`/`authenticated_get`/`authenticated_json_patch`/`body_json` 经 `use crate::api::test_fixture::*;` 同文件可见）：

```rust
// crates/rsetup-controller/src/api/users.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_fixture::*;
    use tower::ServiceExt;
    use axum::http::{Request, StatusCode, header};

    // RED: 初始骨架 users_router 未挂载任何路由（0 个 route）
    // 导致请求全部返回 404，测试断言 401/403/200 失败。
    // 前置层 helpers 已定义且编译通过：RED 必须是行为断言失败（404 ≠ 401/403/409），不得是编译错误。
    #[tokio::test]
    async fn unauthenticated_request_is_rejected_with_401() {
        let (router, _state) = fixture_state();
        let req = Request::builder().uri("/api/v1/users").header("host", "localhost:3080").body(axum::body::Body::empty()).unwrap();
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn non_admin_is_rejected_with_403_permission_denied() {
        let (router, _state) = fixture_state_with_non_admin();
        let req = authenticated_get("/api/v1/users");
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn last_admin_demotion_returns_409_last_admin() {
        let (router, _state) = fixture_state_with_last_admin_rejection();
        let body = serde_json::json!({ "expected_revision": "1", "is_admin": false });
        let req = authenticated_json_patch("/api/v1/users/00000000-0000-0000-0000-000000000001", body);
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let val: serde_json::Value = body_json(res).await;
        assert_eq!(val["error"]["code"], "LAST_ADMIN");
    }
}
```

- [ ] **Step 2: 运行并确认行为 RED**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::users::tests
```
预期：运行 3 个测试；全部 **FAIL**（因为骨架未挂载路由返回 404，断言 `404 == 401` / `404 == 403` / `404 == 409` 失败）。

- [ ] **Step 3: 最小实现**

在 `api/users.rs` 实现 5 个 handler，挂载到 `users_router`：

```rust
pub fn users_router(state: UsersApiState) -> Router<()> {
    Router::new()
        .route("/api/v1/users", get(list_users).post(create_user))
        .route("/api/v1/users/{id}", patch(patch_user))
        .route("/api/v1/users/{id}/reset-password", post(reset_password))
        .route("/api/v1/users/{id}/effective-permissions", get(effective_permissions))
        .with_state(state)
}
```

每个 handler 先走 `http_gate::authenticate_read` / `authenticate_write`：`GET /users` 传 `EndpointId::UsersRead`、`GET /users/{id}/effective-permissions` 传 `EndpointId::EffectivePermissionsRead`（读桶键为 `(variant, session.digest)`，同模板不同 id 共享同桶）；4 条写路由（POST/PATCH）只按 `session.digest` 走 `write_limiter`，不进读桶。接着 `require_admin_session`，调用 `state.gateway`，结果经 `user_admin_public` 投影，异常经 `map_error` 处理。`POST /users` 与 `POST /reset-password` 返回 `{ "user": user_admin_public(&u), "temporary_password": raw }`。

#### GET /api/v1/users 真实 handler 代码定义（本修订切片）

- **范围**：本小节只定义 `GET /api/v1/users` 的 query DTO、真实 handler 与唯一注册锚点；其余 4 个 users handler（`create_user`/`patch_user`/`reset_password`/`effective_permissions`）、其余 5 个管理 list handler 与全部 HTTP 测试（含 no-key 503）均属后续任务；codec、「cursor 密钥生产链」、`ApiState`、gateway trait、fixtures 全部不变，本小节单独不构成整体批准。
- **落点与导入**：写入 `crates/rsetup-controller/src/api/users.rs`，文件顶部补齐下列导入（已有的不重复导入）：

```rust
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Uri};
use axum::response::Response;
use serde::Deserialize;
use crate::error::ControllerError;
use crate::http_api::ok_response;
use crate::http_auth::EndpointId;
use crate::http_gate::{authenticate_read, require_admin_session};
use super::{decode_management_list_cursor, management_cursor_secret, map_error, sign_management_list_page, CursorFilterCtx, CursorKey};
```

- **严格 query DTO**：`deny_unknown_fields` 拒绝未知参数；`Query::try_from_uri` 由 handler 在门禁之后手动调用，不让 Axum extractor 在鉴权前短路 400；`limit` 默认 50、合法域 `1..=200`，`0` 或 `>200` 一律 400（spec02 L16）；`limit` 不参与 MAC 绑定，users 无筛选参数故上下文恒为 `CursorFilterCtx::empty()`。

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsersListQuery {
    pub(crate) cursor: Option<String>,
    pub(crate) limit: Option<usize>,
}
```

- **handler（inner + 生产 wrapper）**：inner 供生产与下一任务测试共用；`secret_provider` 真正调用发生在 `authenticate_read` → `require_admin_session` → query/limit 校验之后，且 `cursor` 为 `None` 也必须先取密钥（未安装 → `Config` → `map_error` 精确 503，不得短路跳过）；客户端侧（query 非法、limit 越域、token 篡改/跨端点/跨用户）→ `InvalidArgument` 400，配置/后端侧 → `map_error` 按 Task 6 约定映射（client 400、config 503、backend 503），绝不吞 `Err` 成 generic 500 或返回未签名页面；`DeviceKey` 分支在 `UsersRead` profile 下不可达，仍 fail-closed 400。

```rust
async fn list_users_with_secret_provider<F>(
    state: &UsersApiState,
    headers: &HeaderMap,
    uri: &Uri,
    secret_provider: F,
) -> Response
where
    F: FnOnce() -> Result<[u8; 32], ControllerError>,
{
    let actor = match authenticate_read(&state.auth, headers, EndpointId::UsersRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let query = match Query::<UsersListQuery>::try_from_uri(uri) {
        Ok(query) => query.0,
        Err(_) => return map_error(ControllerError::InvalidArgument),
    };
    let limit = match query.limit {
        None => 50usize,
        Some(value) if (1..=200).contains(&value) => value,
        Some(_) => return map_error(ControllerError::InvalidArgument),
    };
    let filter = CursorFilterCtx::empty();
    let secret = match secret_provider() {
        Ok(secret) => secret,
        Err(error) => return map_error(error),
    };
    let cursor = match decode_management_list_cursor(&secret, EndpointId::UsersRead, actor.user.id, &filter, query.cursor.as_deref()) {
        Ok(None) => None,
        Ok(Some(CursorKey::Uuid(id))) => Some(id),
        Ok(Some(CursorKey::DeviceKey(_))) => return map_error(ControllerError::InvalidArgument),
        Err(error) => return map_error(error),
    };
    let page = match state.gateway.list(&actor, cursor, limit).await {
        Ok(page) => page,
        Err(error) => return map_error(error),
    };
    match sign_management_list_page(&secret, EndpointId::UsersRead, actor.user.id, &filter, page) {
        Ok(body) => ok_response(body),
        Err(error) => map_error(error),
    }
}

pub(crate) async fn list_users(
    State(state): State<UsersApiState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    list_users_with_secret_provider(&state, &headers, &uri, || management_cursor_secret().map(|secret| *secret)).await
}
```

- **唯一注册锚点**：上方既有 `users_router` 定义中的 `.route("/api/v1/users", get(list_users).post(create_user))` 即 `list_users` 的唯一注册点（GET 与 POST 同 path 共存）；本修订不新增第二处 `get(list_users)`、不删除或改写其余 4 条写/读路由、不重写整个 router。
- **NoSecret 测试注入预留（下一任务）**：下一组 no-key HTTP 测试不得触碰全局 `MANAGEMENT_CURSOR_SECRET` OnceLock，也不得给 `UsersApiState` 扩 secret 字段：在同文件 `#[cfg(test)] mod tests` 内定义 test-only axum handler wrapper，调用同一 `list_users_with_secret_provider`，provider 从测试局部 `OnceLock` 读取（或 `move || Ok(local_key)` 闭包），测试 router 只挂该 wrapper；生产 `list_users` 恒只经 `management_cursor_secret()`，不读测试 key，也不得以 test 专属假 handler、自定义 codec 或直接预先计算 secret 绕过 gate/query 顺序。

- [ ] **Step 4: 运行并确认 GREEN**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::users::tests
```
预期：全绿（3 个测试 PASS）。

- [ ] **Step 5: 静态检查与条件性提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/api/users.rs
git commit -m "feat(controller): users 资源管理路由（5 条端点）"
```

#### GET /api/v1/users 签名 cursor 真实 HTTP 行为测试（本修订切片）

- **owner 与落点**：下列 `#[cfg(test)] mod users_signed_cursor_tests` 由未来实现任务写入 `crates/rsetup-controller/src/api/users.rs`，与既有 `#[cfg(test)] mod tests` **并列且互不重复**（既有 3 个用例覆盖 401/403/坏 query，本组只补「无 key 503 先于业务」与「第一页签发 → 第二页解回 typed key」两条真实 HTTP 往返）；本单元只修文档，不改生产 handler/codec/密钥装配/gateway trait/`ApiState`/其他 6 个端点/全局前置层，零新增 Cargo 依赖，本轮不运行任何命令。
- **必须复用生产链路**：测试 router 只挂 test-only wrapper handler，wrapper 内部调用**同一个** `list_users_with_secret_provider`，因此 `authenticate_read` → `require_admin_session` → 严格 query/limit → provider → `decode_management_list_cursor` → `gateway.list` → `sign_management_list_page` → `ok_response` 的顺序与错误分层与生产完全一致；不得自定义 decoder、不得自签 token 代替第一页签发、不得用 test 专属假 handler 绕过 gate/query，也不得在鉴权之前预先计算 provider key。
- **密钥注入边界**：每个测试 router 独占一个 `Arc<OnceLock<[u8; 32]>>` 局部密钥槽，经 `api` 模块私有的 `install_cursor_secret_into`/`cursor_secret_of` 读写（测试模块是 `api` 的后代模块，`crate::api::<私有项>` 路径按 Rust「私有项对本模块后代可见」规则合法，无需提升可见性）；**不触碰**全局 `MANAGEMENT_CURSOR_SECRET`，不给 `UsersApiState` 扩 secret 字段。provider 是 `FnOnce`，闭包内 `map(|secret| *secret)` 只做 32B 数组拷贝；未安装时返回与生产**同一个**去敏 `Config`（经 `map_error` 精确 503 `STORAGE_UNAVAILABLE`）。
- **网关记录边界**：`RecordingUsersGateway` 按真实 trait 签名实现全部 5 个方法，不省略必需 `fn`、不新增全局 fixture variant：`list` 把 `(cursor, limit)` 记进 `Arc<Mutex<Vec<..>>>`，并复用 `FixtureUsersGateway::Allow` 的真实投影后**仅替换** `next_cursor` 一个字段；其余 4 个方法原样委托 `Allow`。委托一律写成 `Box::pin(async move { ... .await })`，避免借临时对象跨 `return` 导致生命周期不闭合；不影响既有 `mod tests`。
- **合成值**：`SYNTHETIC_SECRET = [7u8; 32]` 与内部 raw `067e6162ea944431836c7d8fa2010b3c` 均为测试合成值；后者是 canonical UUIDv4（`uuid[6] >> 4 == 4`、`uuid[8] & 0xC0 == 0x80`），满足出口 helper 对「32 小写 hex + v4 shape」的校验，不得用 `[1u8; 16]` 之类非法 cursor 充当合法页键。

```rust
// crates/rsetup-controller/src/api/users.rs（与既有 mod tests 并列，不重复其内容）
#[cfg(test)]
mod users_signed_cursor_tests {
    use super::*;
    use crate::api::test_fixture::*;
    use axum::http::StatusCode;
    use std::sync::{Arc, Mutex, OnceLock};
    use tower::ServiceExt;

    const SYNTHETIC_SECRET: [u8; 32] = [7u8; 32];
    /// 合成 canonical UUIDv4 的 32 小写 hex（version nibble=4、variant=10），仅作网关内部 raw。
    const NEXT_RAW_HEX: &str = "067e6162ea944431836c7d8fa2010b3c";

    fn raw_hex_to_bytes(raw: &str) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&hex::decode(raw).expect("fixture raw cursor 必须是 32 小写 hex"));
        bytes
    }

    /// 每个测试 router 独占的局部密钥槽：不触碰全局 MANAGEMENT_CURSOR_SECRET，也不扩 ApiState。
    #[derive(Clone)]
    struct SignedCursorTestState {
        users: UsersApiState,
        secret_slot: Arc<OnceLock<[u8; 32]>>,
    }

    /// test-only wrapper：与生产 list_users 同构，只把 provider 换成读本测试密钥槽，
    /// 内部仍调用同一个 list_users_with_secret_provider（真实 gate/query/codec/gateway 顺序）。
    async fn list_users_with_test_secret(
        State(state): State<SignedCursorTestState>,
        headers: HeaderMap,
        uri: Uri,
    ) -> Response {
        let slot = Arc::clone(&state.secret_slot);
        // provider 只在 inner 于 authenticate_read → require_admin_session → query/limit 之后被调用一次；
        // 未安装时经 api 私有 cursor_secret_of 返回与生产完全相同的去敏 Config。
        list_users_with_secret_provider(&state.users, &headers, &uri, move || crate::api::cursor_secret_of(&slot).map(|secret| *secret)).await
    }

    #[derive(Default)]
    struct RecordingUsersGateway {
        list_calls: Arc<Mutex<Vec<(Option<[u8; 16]>, usize)>>>,
        next_raw: Arc<Mutex<Option<String>>>,
    }

    #[derive(Clone)]
    struct RecordingHandle {
        list_calls: Arc<Mutex<Vec<(Option<[u8; 16]>, usize)>>>,
        next_raw: Arc<Mutex<Option<String>>>,
    }

    impl RecordingUsersGateway {
        fn handle(&self) -> RecordingHandle { RecordingHandle { list_calls: Arc::clone(&self.list_calls), next_raw: Arc::clone(&self.next_raw) } }
    }

    impl RecordingHandle {
        /// 为下一次 list 武装网关侧内部 raw next_cursor（None → 该页 next_cursor 为 null）。
        fn arm_next_raw(&self, raw: Option<String>) { *self.next_raw.lock().unwrap() = raw; }
        fn calls(&self) -> Vec<(Option<[u8; 16]>, usize)> { self.list_calls.lock().unwrap().clone() }
        fn call_count(&self) -> usize { self.list_calls.lock().unwrap().len() }
    }

    impl UsersGateway for RecordingUsersGateway {
        fn list<'a>(&'a self, actor: &'a Session, cursor: Option<[u8; 16]>, limit: usize) -> ManagementFuture<'a, serde_json::Value> {
            let list_calls = Arc::clone(&self.list_calls);
            let next_raw = Arc::clone(&self.next_raw);
            Box::pin(async move {
                list_calls.lock().unwrap().push((cursor, limit));
                let delegate = FixtureUsersGateway::Allow;
                let mut page = delegate.list(actor, cursor, limit).await?;
                let raw = next_raw.lock().unwrap().take();
                if let Some(map) = page.as_object_mut() {
                    let value = match raw {
                        Some(raw) => serde_json::Value::String(raw),
                        None => serde_json::Value::Null,
                    };
                    map.insert("next_cursor".to_string(), value);
                }
                Ok(page)
            })
        }
        fn create<'a>(&'a self, actor: &'a Session, username: &'a str, display_name: &'a str, is_admin: bool) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async move { FixtureUsersGateway::Allow.create(actor, username, display_name, is_admin).await })
        }
        fn patch<'a>(&'a self, actor: &'a Session, id: [u8; 16], revision: u64, active: Option<bool>, is_admin: Option<bool>, display_name: Option<&'a str>) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async move { FixtureUsersGateway::Allow.patch(actor, id, revision, active, is_admin, display_name).await })
        }
        fn reset_password<'a>(&'a self, actor: &'a Session, id: [u8; 16], revision: u64) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async move { FixtureUsersGateway::Allow.reset_password(actor, id, revision).await })
        }
        fn effective_permissions<'a>(&'a self, actor: &'a Session, target: [u8; 16], device_id: Option<[u8; 32]>, cursor: Option<[u8; 32]>, limit: usize) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async move { FixtureUsersGateway::Allow.effective_permissions(actor, target, device_id, cursor, limit).await })
        }
    }

    fn signed_cursor_router(secret: Option<[u8; 32]>) -> (axum::Router<()>, RecordingHandle) {
        let gateway = Arc::new(RecordingUsersGateway::default());
        let recorder = gateway.handle();
        let gateway: Arc<dyn UsersGateway> = gateway;
        let secret_slot = Arc::new(OnceLock::new());
        if let Some(secret) = secret {
            crate::api::install_cursor_secret_into(&secret_slot, &secret).expect("synthetic key 安装必须成功");
        }
        let state = SignedCursorTestState {
            users: UsersApiState { auth: fixture_auth(fixture_session(true)), gateway },
            secret_slot,
        };
        let router = axum::Router::new()
            .route("/api/v1/users", axum::routing::get(list_users_with_test_secret))
            .with_state(state);
        (router, recorder)
    }

    #[tokio::test]
    async fn list_without_installed_secret_returns_503_before_gateway() {
        let (router, recorder) = signed_cursor_router(None);
        let res = router.oneshot(authenticated_get("/api/v1/users")).await.unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_json(res).await;
        assert_eq!(body["error"]["code"], "STORAGE_UNAVAILABLE");
        assert!(body.get("data").is_none(), "503 失败体不得携带 data");
        assert_eq!(recorder.call_count(), 0, "provider 失败必须先于 gateway.list 返回");
    }

    #[tokio::test]
    async fn list_signs_first_page_and_decodes_second_page_cursor() {
        let (router, recorder) = signed_cursor_router(Some(SYNTHETIC_SECRET));
        recorder.arm_next_raw(Some(NEXT_RAW_HEX.to_string()));
        let first = router.clone().oneshot(authenticated_get("/api/v1/users")).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = body_json(first).await;
        let token = first_body["data"]["next_cursor"].as_str().expect("第一页必须签发 next_cursor").to_string();
        assert_eq!(token.len(), 64, "UUID profile 签名 token 恰 64 小写 hex（24B payload + 8B tag）");
        assert!(token.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "签发 token 必须全小写 hex");
        assert_ne!(token, NEXT_RAW_HEX, "不得把网关内部 raw 32 hex 直接发给客户端");
        assert!(first_body["data"]["items"].is_array(), "items 投影必须原样保留");
        let calls = recorder.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, None, "第一页 cursor 必须是 None");
        assert_eq!(calls[0].1, 50usize, "limit 缺省 50");

        recorder.arm_next_raw(None);
        let second_uri = format!("/api/v1/users?cursor={token}&limit=7");
        let second = router.clone().oneshot(authenticated_get(&second_uri)).await.unwrap();
        assert_eq!(second.status(), StatusCode::OK, "同一 token 换 limit 仍须可解（limit 不参与 MAC 绑定）");
        let second_body = body_json(second).await;
        assert!(second_body["data"]["next_cursor"].is_null(), "末页 next_cursor 必须保持 null");
        let calls = recorder.calls();
        assert_eq!(calls.len(), 2);
        let expected: Option<[u8; 16]> = Some(raw_hex_to_bytes(NEXT_RAW_HEX));
        assert_eq!(calls[1].0, expected, "第二页必须拿到真实 decode 出的 typed UUID 字节");
        assert_eq!(calls[1].1, 7usize, "第二页 limit 必须是 query 里的 7");

        // 篡改 token 属客户端错误：400 且不得触达网关（复用同一路由器，省一个测试函数）
        let mut tampered = hex::decode(&token).expect("签发 token 必须是 hex");
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        let bad_uri = format!("/api/v1/users?cursor={}", hex::encode(&tampered));
        let third = router.oneshot(authenticated_get(&bad_uri)).await.unwrap();
        assert_eq!(third.status(), StatusCode::BAD_REQUEST);
        let third_body = body_json(third).await;
        assert_eq!(third_body["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(recorder.call_count(), 2, "MAC 失败必须在 gateway.list 之前返回");
    }
}
```

- **断言口径**：① 无 key：`cursor` 为 `None`、Host/Cookie/limit 全合法时仍须先取密钥，失败精确 503 `STORAGE_UNAVAILABLE` 且 `gateway.list` 记录数为 0——直接证明 provider 调用点在业务网关之前，不是用 accessor 模拟状态码；② 第一页：`data.next_cursor` 从真实 `body_json` 读取，长度恰 64 且全小写 hex，且不等于网关内部 raw 32 hex（证明经真实 `encode_signed_cursor` 签发，不是裸 raw 直传），`data.items` 原样保留；③ 第二页：原样回传第一页 token 并**换 `limit=7`**，仍须 200 且网关收到 `Some(<typed UUID 16B>)`（证明真实 `decode_signed_cursor` 把 token 解回 typed key、MAC 不绑定 `limit`），末页 `next_cursor` 保持 `null`；④ 篡改：翻转 tag 末字节 1 bit → 400 `INVALID_ARGUMENT` 且 `gateway.list` 调用计数不增长（客户端 400 与后端 503 分层一致）。跨用户/跨端点/换 filter 的上下文绑定负向已由 `api::signed_cursor_tests` 8 个 codec 用例覆盖，本组不重复；bad query 与 401 路径由既有 `mod tests` 覆盖，不削弱。
- **RED / GREEN（本轮不编译、不运行）**：本组是上文 handler 代码定义的行为锁定测试。其 RED 由两个反向桩提供——① 把 provider 调用挪到 `state.gateway.list(...)` 之后（或让 provider 恒 `Ok`）→ 第一条的 503 与 `call_count == 0` 断言失败；② 把出口 `sign_management_list_page` 换成裸 raw 直传 → 第二条的 `token.len() == 64` / `assert_ne!(token, NEXT_RAW_HEX)` 失败，且第二页解码失败变 400。两条都必须先以桩跑出**行为断言**失败再转 GREEN，不得以空断言、`#[ignore]` 或 0 用例冒充。
- **静态自验**：`use super::*` 已带入 `State`/`HeaderMap`/`Uri`/`Response`/`UsersApiState`/`UsersGateway`/`ManagementFuture`/`Session`/`ControllerError`（users.rs 顶层已 import），本模块只额外 import `StatusCode`/`Arc`/`Mutex`/`OnceLock`/`ServiceExt` 与 `test_fixture::*`，无未用 import；`serde_json`/`hex`/`axum` 全限定引用；`Mutex` guard 一律在语句内释放，不跨 `await` 持有；委托一律 `Box::pin(async move { ... .await })`，无借临时对象跨 `return`。
- **selectors（未来命令，本轮不执行）**：`CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR="$PWD/.cargo-target" cargo test --offline --locked -p rsetup-controller api::users::users_signed_cursor_tests::list_without_installed_secret_returns_503_before_gateway`，第二条 selector 末段替换为 `list_signs_first_page_and_decodes_second_page_cursor`；每条恰好运行 1 个测试，整组回归过滤 `api::users::users_signed_cursor_tests`（2 个测试）。
- **范围声明**：本单元只闭合 users 一条 list 端点签名 cursor 的真实 HTTP 往返（B1 的 handler 侧证据，静态未跑）；其余 5 个分页读端点的同型接线与 selector 另后续，**不宣称 Identity 整体 APPROVED**，也不改变 Task 13/14 的真库与生产装配门禁。

---

### Task 8: roles 与 grants 资源管理路由（6 条）

**Files:**
- Modify: `crates/rsetup-controller/src/api/roles.rs`
- Modify: `crates/rsetup-controller/src/api/grants.rs`
- Test: 两文件的 `#[cfg(test)] mod tests`

**契约矩阵：**

| 路径与方法 | 权限 | 输入 | 响应 `data` 形状 | 异常状态 |
| :--- | :--- | :--- | :--- | :--- |
| `GET /api/v1/roles` | admin | `cursor?`, `limit?` | `{ items: [Role], next_cursor: String? }` | 401, 403, 400 |
| `POST /api/v1/roles` | admin | `{ name, permissions: [String] }` | `Role` | 400(非空去重非法校验), 409 |
| `PATCH /api/v1/roles/{id}` | admin | `{ expected_revision, name?, permissions?, archived? }`（从归档恢复必须携带合法非空 `permissions`） | `Role` | 404, 409 `INVALID_STATE`(内置不可归档), 400 `INVALID_ARGUMENT`(恢复无权限/空权限) |
| `GET /api/v1/grants` | admin | `user_id?`, `cursor?`, `limit?` | `{ items: [Grant], next_cursor: String? }` | 401, 403, 400 |
| `POST /api/v1/grants` | admin | `{ user_id, source: {kind, role_id?/permissions?}, scope: {kind, group_id?/device_id?} }` | `Grant` | 400(成对与有效性校验), 404 |
| `DELETE /api/v1/grants/{id}` | admin | `{ expected_revision }`（DELETE 仍需 JSON body/CSRF） | `{ deleted: true }` | 404, 409 `REVISION_CONFLICT` |

**Interfaces:**
- Produces: `RolesGateway`、`GrantsGateway`；`pub fn roles_router(RolesApiState) -> Router<()>`；`pub fn grants_router(GrantsApiState) -> Router<()>`。

- [ ] **Step 1: 编写失败测试（内置角色归档 409、Grant 校验失败 400、DELETE 带 CSRF 成功 200）**

两文件测试均置于各自 `#[cfg(test)] mod tests`，头部 `use super::*; use crate::api::test_fixture::*; use tower::ServiceExt; use axum::body::Body; use axum::http::{header, HeaderName, Request, StatusCode};`。`authenticated_json_patch`/`body_json`/`fixture_auth`/`fixture_session` 一律取自共享前置层（本区不重复定义；Host/Cookie/CSRF 口径沿用共享层，fixture allowlist 含 `localhost:3080` 与 `controller.test`）。共享层未提供 POST 写 helper，grants 区以局部 `auth_json_post` 复用同款头（`FIXTURE_HOST` 经 glob 导入，Cookie 值 `"a".repeat(64)`、CSRF 值 `"b".repeat(64)`，不发 Origin）。`fixture_builtin_role_state()`/`fixture_grants_state()` 在本区 tests 内定义，返回各自 `*ApiState { auth: fixture_auth(fixture_session(true)), gateway: Arc::new(私有桩) }`（roles 桩对归档恒返 `ControllerError::InvalidState`；grants 桩恒 `Ok({})`，400 由 body 校验先于网关产生）。RED 为骨架未挂载路由的 404 行为断言失败，不得是编译错误。

- **GET list cursor 测试的局部密钥注入（roles/grants 强制约束，与 users 区 `SignedCursorTestState`/`list_users_with_test_secret`/`signed_cursor_router` 先例同型）**：生产 `list_roles`/`list_grants` 恒只经 `management_cursor_secret()` 取密钥，单元测试进程内全局 `MANAGEMENT_CURSOR_SECRET` OnceLock 未安装，若测试 router 直接挂生产 `roles_router`/`grants_router`，GET list 任何调用都会先经 `map_error` 返回 503 `STORAGE_UNAVAILABLE`，200/阳性断言必然失败。因此 GET `/api/v1/roles`、`/api/v1/grants` 的 cursor 正负例测试不得挂生产 router：在各自文件 `#[cfg(test)] mod tests` 内定义 test-only state（`SignedRolesCursorTestState { roles: RolesApiState, secret_slot: Arc<OnceLock<[u8; 32]>> }`，grants 同型为 `SignedGrantsCursorTestState { grants: GrantsApiState, secret_slot }`）与 test-only wrapper handler，wrapper 内部调用**同一个** `list_roles_with_secret_provider`/`list_grants_with_secret_provider`（真实 gate/query/codec/gateway 顺序不绕过），provider 经 `move || crate::api::cursor_secret_of(&slot).map(|secret| *secret)` 从本测试局部槽读取；fixture helper `signed_roles_cursor_router(secret: Option<[u8; 32]>)`/`signed_grants_cursor_router(secret: Option<[u8; 32]>)` 在本区定义 `SYNTHETIC_SECRET = [7u8; 32]`（与 users 区同值的测试合成 key），Some 时经 `crate::api::install_cursor_secret_into` 装入局部槽、None 时留空作网关前 503 负例；测试 router 只挂 wrapper 的 GET 路由并 `.with_state(state)` 归约。上方写路径测试（`fixture_builtin_role_state()`/`fixture_grants_state()`）不经 cursor 密钥，维持挂生产 router 不变。**红线**：不得对全局 `MANAGEMENT_CURSOR_SECRET` 调用 `set`，不得给 `RolesApiState`/`GrantsApiState` 扩 secret 字段，不得用 test 专属假 handler 或自签 token 绕过真实链路。

```rust
// crates/rsetup-controller/src/api/roles.rs
#[tokio::test]
async fn builtin_role_archiving_is_rejected_with_409_invalid_state() {
    let router = roles_router(fixture_builtin_role_state());
    let req = authenticated_json_patch("/api/v1/roles/00000000-0000-0000-0000-000000000001",
        serde_json::json!({ "expected_revision": "1", "archived": true }));
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let val: serde_json::Value = body_json(res).await;
    assert_eq!(val["error"]["code"], "INVALID_STATE");
}
```

```rust
// crates/rsetup-controller/src/api/grants.rs
#[tokio::test]
async fn grant_creation_rejects_empty_direct_permissions_with_400() {
    let router = grants_router(fixture_grants_state());
    let req = auth_json_post("/api/v1/grants", serde_json::json!({
        "user_id": "00000000-0000-0000-0000-000000000001",
        "source": { "kind": "direct", "permissions": [] },
        "scope": { "kind": "all" },
    }));
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

// 局部写 helper（共享层仅有 PATCH 变体）：头与 authenticated_json_patch 完全一致，仅方法为 POST
fn auth_json_post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder().method("POST").uri(uri).header(header::HOST, FIXTURE_HOST)
        .header(header::COOKIE, format!("rsc_session={}", "a".repeat(64)))
        .header(HeaderName::from_static("x-csrf-token"), "b".repeat(64))
        .header(header::CONTENT_TYPE, "application/json").body(Body::from(body.to_string())).unwrap()
}
```

- [ ] **Step 2: 运行并确认行为 RED**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::roles::tests
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::grants::tests
```
预期：各 1 个测试；因未挂载路由（404）导致断言失败。

- [ ] **Step 3: 最小实现**

在 `api/roles.rs` 挂载 3 条路由；在 `api/grants.rs` 挂载 3 条路由（`delete(delete_grant)` 读取 bounded JSON body 取得 `expected_revision`）。`DELETE` 端点同样执行 CSRF 与单值 Origin 校验（规格 02 §3 明确约定）。读桶变体：`GET /roles` 传 `EndpointId::RolesRead`、`GET /grants` 传 `EndpointId::GrantsRead`；4 条写路由（POST/PATCH/DELETE）只按 `session.digest` 走 `write_limiter`，不进读桶。

- **GET /api/v1/roles cursor 分页真实接线（本单元唯一范围）**：`list_roles` 为 roles 唯一 GET list handler（命名对齐本计划 Task 3/Task 6 仓储方法 `list_roles` 锚点）。**唯一注册锚点**：`roles_router` 的 `GET /api/v1/roles` 入口中唯一的 `get(list_roles)`（`.route("/api/v1/roles", get(list_roles).post(create_role))`，GET 与 POST 同 path 共存、只注册一次）；本单元不新增第二处 `get(list_roles)`、不删除或改写 POST/PATCH 写路由、不重写整个 router。handler 严格复用 Task 6「下一 handler 切片的真实调用顺序」：`authenticate_read(EndpointId::RolesRead)` → `require_admin_session` → `Query::try_from_uri`（门禁之后手动调用，不让 Axum extractor 在鉴权前短路 400）→ limit 域校验（默认 50、合法域 `1..=200`，`0` 或 `>200` 一律 400，spec02 L16）→ `CursorFilterCtx::empty()`（roles 无筛选参数，`limit` 不参与 MAC 绑定）→ 取密钥（即使 `cursor` 为 `None` 也必须先调 `management_cursor_secret()`，未安装 → `Config` → `map_error` 精确 503，不得因 `None` 短路跳过）→ `decode_management_list_cursor`（`EndpointId::RolesRead`、`actor.user.id`、同一 ctx 实例；篡改/跨端点/跨用户 → `InvalidArgument` 400；`CursorKey::DeviceKey` 在 `RolesRead` profile 下不可达，仍 fail-closed 400）→ `Uuid::from_bytes` 把 raw 16 字节转 `Uuid` → `gateway.list(&actor, cursor, limit)`（同一 actor）→ `sign_management_list_page`（同一 secret/endpoint/actor/同一 ctx 实例，raw 32 字节密钥签出 64 字符 token）→ `ok_response`；配置/后端错误经 Task 6 `map_error` 映射（client 400、config 503、backend 503），绝不吞 `Err` 成 generic 500 或返回未签名页面。
- **导入（与 `api/roles.rs` 顶层既有 import 合并、不重复、不新增未用 `Arc`/`Session`）**：`axum::extract::{Query, State}`、`axum::http::{HeaderMap, Uri}`、`axum::response::Response`、`serde::Deserialize`、`uuid::Uuid`、`crate::error::ControllerError`、`crate::http_api::ok_response`、`crate::http_auth::EndpointId`、`crate::http_gate::{authenticate_read, require_admin_session}`、`super::{decode_management_list_cursor, management_cursor_secret, map_error, sign_management_list_page, CursorFilterCtx, CursorKey}`，全部真实使用。
- **严格 query DTO（本模块局部；`UsersListQuery` 保持独立不动，不在 `api/mod.rs` 新增共用 DTO，无悬空 import）**：

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RolesListQuery {
    pub(crate) cursor: Option<String>,
    pub(crate) limit: Option<usize>,
}
```

- **handler（inner + 生产 wrapper，与 `list_users_with_secret_provider` 同型）**：`secret_provider` 为唯一密钥入口，测试注入规则与 users 相同（test-only wrapper 调同一 inner，provider 从测试局部 `OnceLock` 或 `move || Ok(local_key)` 闭包读取，不触碰全局 `MANAGEMENT_CURSOR_SECRET` OnceLock、不给 `RolesApiState` 扩 secret 字段）；生产 `list_roles` 恒只经 env 安装的 accessor `management_cursor_secret()`。

```rust
async fn list_roles_with_secret_provider<F>(
    state: &RolesApiState,
    headers: &HeaderMap,
    uri: &Uri,
    secret_provider: F,
) -> Response
where
    F: FnOnce() -> Result<[u8; 32], ControllerError>,
{
    let actor = match authenticate_read(&state.auth, headers, EndpointId::RolesRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let query = match Query::<RolesListQuery>::try_from_uri(uri) {
        Ok(query) => query.0,
        Err(_) => return map_error(ControllerError::InvalidArgument),
    };
    let limit = match query.limit {
        None => 50usize,
        Some(value) if (1..=200).contains(&value) => value,
        Some(_) => return map_error(ControllerError::InvalidArgument),
    };
    let filter = CursorFilterCtx::empty();
    let secret = match secret_provider() {
        Ok(secret) => secret,
        Err(error) => return map_error(error),
    };
    let cursor = match decode_management_list_cursor(&secret, EndpointId::RolesRead, actor.user.id, &filter, query.cursor.as_deref()) {
        Ok(None) => None,
        Ok(Some(CursorKey::Uuid(raw))) => Some(Uuid::from_bytes(raw)),
        Ok(Some(CursorKey::DeviceKey(_))) => return map_error(ControllerError::InvalidArgument),
        Err(error) => return map_error(error),
    };
    let page = match state.gateway.list(&actor, cursor, limit).await {
        Ok(page) => page,
        Err(error) => return map_error(error),
    };
    match sign_management_list_page(&secret, EndpointId::RolesRead, actor.user.id, &filter, page) {
        Ok(body) => ok_response(body),
        Err(error) => map_error(error),
    }
}

pub(crate) async fn list_roles(
    State(state): State<RolesApiState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    list_roles_with_secret_provider(&state, &headers, &uri, || management_cursor_secret().map(|secret| *secret)).await
}
```

- **范围边界**：本单元只闭合 `GET /roles` 与 `GET /groups`（见 Task 9）的 admin cursor 真实接线；grants filter ctx、devices 普通用户 scope、approvals decision filter 为独立后续，不得声称全部 cursor 端点已闭合；本 admin cursor helper 链不适用于 devices（spec01 rebootOnly 的最小 devices 列表例外由 Task 10 单独处理，Task 6 通用顺序描述不因此修改）。

- **GET /api/v1/grants cursor 分页真实接线与 user_id 筛选绑定（本单元范围）**：`list_grants` 为 grants 唯一 GET list handler（命名对齐本计划 Task 3/Task 8 仓储方法 `list_grants` 锚点）。**唯一注册锚点**：`grants_router` 的 `GET /api/v1/grants` 入口中唯一的 `get(list_grants)`（`.route("/api/v1/grants", get(list_grants).post(create_grant))`，GET 与 POST 同 path 共存、只注册一次），`DELETE /api/v1/grants/{id}` 路由原样保留；本单元不新增第二处 `get(list_grants)`、不删除或改写 POST/DELETE 写路由、不重写整个 router。本小节即上方 roles 单元范围边界中「grants filter ctx 为独立后续」的兑现，roles/groups 单元边界原文保留不改写；devices/approvals 仍为独立后续。handler 严格复用 Task 6「下一 handler 切片的真实调用顺序」：`authenticate_read(EndpointId::GrantsRead)` → `require_admin_session` → `Query::try_from_uri`（门禁之后手动调用，不让 Axum extractor 在鉴权前短路 400）→ limit 域校验（默认 50、合法域 `1..=200`，`0` 或 `>200` 一律 400，spec02 L16）→ 严格 `user_id` 解析与规范化（编码规则见下）→ 以固定 schema 构造 `CursorFilterCtx`（绑定 `user_id`，不绑定 `limit`）→ `management_cursor_secret()`（即使 `cursor` 为 `None` 也必须先取密钥，未安装 → `Config` → `map_error` 精确 503，不得因 `None` 短路跳过）→ `decode_management_list_cursor`（`EndpointId::GrantsRead`、`actor.user.id`、同一 ctx 实例；篡改/跨端点/跨用户/换筛选复用 → `InvalidArgument` 400；`CursorKey::DeviceKey` 在 `GrantsRead` 的 `Uuid` profile 下不可达，仍 fail-closed 400）→ `Uuid::from_bytes` 把 raw 16 字节转 `Uuid` → `gateway.list(&actor, user_id, cursor, limit)`（同一 actor、同一请求内规范化的 `user_id`）→ `sign_management_list_page`（同一 secret/endpoint/actor/同一 ctx 实例，raw 32 字节密钥签出 64 字符 token）→ `ok_response`；客户端侧（query 非法、limit 越域、`user_id` 非法、cursor 篡改/跨用户/筛选不匹配）→ `InvalidArgument` 400，配置/后端侧（密钥未安装、gateway `Err`、gateway 返回页面非合法列表页致 `sign_management_list_page` 固定去敏错误）→ `map_error` 映射精确 503，绝不吞 `Err` 成 generic 500 或返回未签名页面。
- **`user_id` 筛选规范编码规则（唯一口径）**：
  - 解析复用既有 `parse_uuid_path`（计划唯一 UUID 文本解析 helper，producer 在 `api/mod.rs`，经 `super::parse_uuid_path` 导入，400 `INVALID_ARGUMENT`/`request.invalid_id`）：缺省 → `None`；存在但空串、大写、花括号、无连字符或任何非「规范小写 36 字符（无花括号）」形态 → 400。helper 接受域已由 Task 6「parse_uuid_path 唯一 producer 与 pure tests」小节以 canonical 字符串对比闭合为规范小写 36 字符，与本口径一致；模块内不得另造 `Uuid::parse_str` 宽松口径造成双重规范。
  - filter 构造只经 `CursorFilterCtx::from_ordered_fields`，不手写 canonical 字节：grants query schema 唯一筛选字段 `user_id`，schema 常量 `const GRANTS_FILTER_TAG_USER_ID: u8 = 1`；存在 → `[(1, Some(raw16))]`，值字节为 UUID raw 16 字节（与 `CursorKey::Uuid([u8; 16])` 及 gateway `user_id: Option<[u8; 16]>` 同口径，不引入 hex 或连字符文本层）；缺省 → `[(1, None)]`（canonical 为单个 `0` 字节，与存在态可区分；不用 `empty()`——grants schema 恒含该字段）。`limit` 不进 schema、不参与 MAC 绑定。
  - 分页闭环：入口与出口必须复用同一请求内同一 `CursorFilterCtx` 实例；下一页请求必须携带同一 `user_id` 文本（同文本 → 同 raw 16 字节 → 同 canonical → MAC 通过），改写或缺省 `user_id` → filter 不一致 → MAC 校验失败 400。
- **严格 query DTO（本模块局部，与 `UsersListQuery`/`RolesListQuery`/`GroupsListQuery` 同型且相互独立，无悬空 import）**：

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GrantsListQuery {
    pub(crate) cursor: Option<String>,
    pub(crate) user_id: Option<String>,
    pub(crate) limit: Option<usize>,
}
```

- **导入（与 `api/grants.rs` 顶层既有 import 合并、不重复、不新增未用 `Arc`/`Session`）**：与 roles 同一组符号——`axum::extract::{Query, State}`、`axum::http::{HeaderMap, Uri}`、`axum::response::Response`、`serde::Deserialize`、`uuid::Uuid`、`crate::error::ControllerError`、`crate::http_api::ok_response`、`crate::http_auth::EndpointId`、`crate::http_gate::{authenticate_read, require_admin_session}`、`super::{decode_management_list_cursor, management_cursor_secret, map_error, parse_uuid_path, sign_management_list_page, CursorFilterCtx, CursorKey}`，全部真实使用（`parse_uuid_path` 唯一来源为 `super::parse_uuid_path`，即 `api/mod.rs` 中 Task 6「parse_uuid_path 唯一 producer」小节定义的唯一真源，不新造、不从其他模块路径导入）。
- **handler（inner + 生产 wrapper，与 `list_roles_with_secret_provider` 同型）**：`secret_provider` 为唯一密钥入口，测试注入规则与 users/roles 相同（test-only wrapper 调同一 inner，provider 从测试局部 `OnceLock` 或 `move || Ok(local_key)` 闭包读取，不触碰全局 `MANAGEMENT_CURSOR_SECRET` OnceLock、不给 `GrantsApiState` 扩 secret 字段）；生产 `list_grants` 恒只经 env 安装的 accessor `management_cursor_secret()`。

```rust
const GRANTS_FILTER_TAG_USER_ID: u8 = 1;

async fn list_grants_with_secret_provider<F>(
    state: &GrantsApiState,
    headers: &HeaderMap,
    uri: &Uri,
    secret_provider: F,
) -> Response
where
    F: FnOnce() -> Result<[u8; 32], ControllerError>,
{
    let actor = match authenticate_read(&state.auth, headers, EndpointId::GrantsRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let query = match Query::<GrantsListQuery>::try_from_uri(uri) {
        Ok(query) => query.0,
        Err(_) => return map_error(ControllerError::InvalidArgument),
    };
    let limit = match query.limit {
        None => 50usize,
        Some(value) if (1..=200).contains(&value) => value,
        Some(_) => return map_error(ControllerError::InvalidArgument),
    };
    let user_id = match query.user_id.as_deref() {
        None => None,
        Some(raw) => match parse_uuid_path(raw) {
            Ok(id) => Some(*id.as_bytes()),
            Err(response) => return response,
        },
    };
    let filter = match user_id {
        None => CursorFilterCtx::from_ordered_fields(&[(GRANTS_FILTER_TAG_USER_ID, None)]),
        Some(raw) => {
            CursorFilterCtx::from_ordered_fields(&[(GRANTS_FILTER_TAG_USER_ID, Some(raw.as_slice()))])
        }
    };
    let filter = match filter {
        Ok(filter) => filter,
        Err(error) => return map_error(error),
    };
    let secret = match secret_provider() {
        Ok(secret) => secret,
        Err(error) => return map_error(error),
    };
    let cursor = match decode_management_list_cursor(&secret, EndpointId::GrantsRead, actor.user.id, &filter, query.cursor.as_deref()) {
        Ok(None) => None,
        Ok(Some(CursorKey::Uuid(raw))) => Some(Uuid::from_bytes(raw)),
        Ok(Some(CursorKey::DeviceKey(_))) => return map_error(ControllerError::InvalidArgument),
        Err(error) => return map_error(error),
    };
    let page = match state.gateway.list(&actor, user_id, cursor, limit).await {
        Ok(page) => page,
        Err(error) => return map_error(error),
    };
    match sign_management_list_page(&secret, EndpointId::GrantsRead, actor.user.id, &filter, page) {
        Ok(body) => ok_response(body),
        Err(error) => map_error(error),
    }
}

pub(crate) async fn list_grants(
    State(state): State<GrantsApiState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    list_grants_with_secret_provider(&state, &headers, &uri, || management_cursor_secret().map(|secret| *secret)).await
}
```

- **局部 RED/GREEN 未来说明（本单元不执行命令、不声称全链批准）**：RED 前置 = `grants_router` 骨架未挂载 `GET /api/v1/grants` 时，GET 测试断言 404 行为失败（行为断言，不得是编译错误）；GREEN 局部自验 = 同型 inner + test-only wrapper（测试局部 synthetic key，不引用任何 secret 文件）覆盖三组断言：分页闭环（第一页 `next_cursor` + 同一 `user_id` 取到第二页）、筛选绑定（下一页改写或缺省 `user_id` → 400）、密钥未安装（provider 返回 `Config` → 精确 503，含 `cursor=None` 路径）。未来命令示例（本任务不执行）：`CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::grants::tests`。完整 HTTP 测试矩阵（401/403/CSRF/Origin/limit 域/跨用户与跨筛选复用全矩阵）为独立后续任务，本单元尚未完成，不得据此声称全链批准。提交示例（不执行）：`git add crates/rsetup-controller/src/api/grants.rs && git commit -m "feat(controller): grants GET list 接线签名 cursor 并绑定 user_id 筛选"`。

- [ ] **Step 4: 运行并确认 GREEN**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::roles::tests
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::grants::tests
```
预期：全绿。

- [ ] **Step 5: 静态检查与条件性提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/api/roles.rs crates/rsetup-controller/src/api/grants.rs
git commit -m "feat(controller): roles 与 grants 资源管理路由（6 条端点）"
```

---

### Task 9: groups 资源管理路由（4 条）

**Files:**
- Modify: `crates/rsetup-controller/src/api/groups.rs`
- Test: 同文件的 `#[cfg(test)] mod tests`

**契约矩阵：**

| 路径与方法 | 权限 | 输入 | 响应 `data` 形状 | 异常状态 |
| :--- | :--- | :--- | :--- | :--- |
| `GET /api/v1/groups` | admin | `cursor?`, `limit?` | `{ items: [Group], next_cursor: String? }` | 401, 403, 400 |
| `POST /api/v1/groups` | admin | `{ name }` | `Group` | 400, 409 |
| `PATCH /api/v1/groups/{id}` | admin | `{ expected_revision, name?, archived? }` | `Group` | 404, 409 `REVISION_CONFLICT` |
| `PUT /api/v1/groups/{id}/members` | admin | `{ expected_revision, device_ids: [String] }` | `{ group: Group, member_count: u64 }` | 400(格式或超 1024), 404, 409 |

**Interfaces:**
- Produces: `GroupsGateway`；`pub fn groups_router(GroupsApiState) -> Router<()>`。

- [ ] **Step 1: 编写失败测试（成员超 1024 拒收 400、全量替换返回新 member_count 200）**

测试置于 `#[cfg(test)] mod tests`，头部同 Task 8（含 `use crate::api::test_fixture::*;`）。`fixture_groups_state()` 在本区 tests 内定义，返回 `GroupsApiState { auth: fixture_auth(fixture_session(true)), gateway: Arc::new(私有桩) }`；PUT 写请求用本区局部 `auth_json_put`（形状同 Task 8 grants 的 `auth_json_post`，仅方法为 PUT，Host/Cookie/CSRF 头口径与共享层一致），共享层 helper 不重复定义。RED 为骨架未挂载路由的 404 行为断言失败，不得是编译错误。

```rust
// crates/rsetup-controller/src/api/groups.rs
#[tokio::test]
async fn member_replacement_exceeding_1024_is_rejected_with_400() {
    let router = groups_router(fixture_groups_state());
    let ids: Vec<String> = (0..1025).map(|_| "a".repeat(64)).collect();
    let req = auth_json_put("/api/v1/groups/00000000-0000-0000-0000-000000000001/members",
        serde_json::json!({ "expected_revision": "1", "device_ids": ids }));
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
```

- [ ] **Step 2: 运行并确认行为 RED**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::groups::tests
```
预期：1 个测试 FAIL（未挂载路由返回 404）。

- [ ] **Step 3: 最小实现**

挂载 4 条路由；`GET /groups` 传 `EndpointId::GroupsRead`（读桶），`POST /groups`、`PATCH /groups/{id}`、`PUT /groups/{id}/members` 只按 `session.digest` 走 `write_limiter`，不进读桶；`PUT /groups/{id}/members` 检查 `device_ids.len() <= 1024`（超限 400 `INVALID_ARGUMENT`/`identity.group.members_exceeded`），逐项用 `parse_device_id_path` 校验小写 hex64 格式，去重后传给网关。

- **GET /api/v1/groups cursor 分页真实接线（本单元唯一范围）**：`list_groups` 为 groups 唯一 GET list handler（命名对齐本计划 Task 3/Task 6 仓储方法 `list_groups` 锚点）。**唯一注册锚点**：`groups_router` 的 `GET /api/v1/groups` 入口中唯一的 `get(list_groups)`（`.route("/api/v1/groups", get(list_groups).post(create_group))`，GET 与 POST 同 path 共存、只注册一次）；不新增第二处 `get(list_groups)`、不删除或改写 PATCH/PUT 写路由、不重写整个 router。调用顺序与 `GET /roles` 完全同型，仅替换 `EndpointId::GroupsRead`、`GroupsListQuery` 与 `GroupsApiState`：`authenticate_read` → `require_admin_session` → `Query::try_from_uri`（门禁之后手动调用，extractor 不在鉴权前短路 400）→ limit 默认 50、合法域 `1..=200`、`0` 或 `>200` 一律 400 → `CursorFilterCtx::empty()`（groups 无筛选参数，`limit` 不参与 MAC 绑定）→ `management_cursor_secret()`（即使 `cursor` 为 `None` 也必须先取密钥，未安装 → `Config` → `map_error` 精确 503）→ `decode_management_list_cursor`（篡改/跨端点/跨用户 → 400；`DeviceKey` 在 `GroupsRead` profile 下不可达，仍 fail-closed 400）→ `Uuid::from_bytes` 把 raw 16 字节转 `Uuid` → `gateway.list(&actor, cursor, limit)`（同一 actor）→ `sign_management_list_page`（同一 secret/endpoint/actor/同一 ctx 实例，raw 32 字节密钥签出 64 字符 token）→ `ok_response`；配置/后端错误经 Task 6 `map_error` 映射（client 400、config 503、backend 503），绝不吞 `Err` 或返回未签名页面。
- **导入（与 `api/groups.rs` 顶层既有 import 合并、不重复、不新增未用 `Arc`/`Session`）**：与 roles 同一组符号——`axum::extract::{Query, State}`、`axum::http::{HeaderMap, Uri}`、`axum::response::Response`、`serde::Deserialize`、`uuid::Uuid`、`crate::error::ControllerError`、`crate::http_api::ok_response`、`crate::http_auth::EndpointId`、`crate::http_gate::{authenticate_read, require_admin_session}`、`super::{decode_management_list_cursor, management_cursor_secret, map_error, sign_management_list_page, CursorFilterCtx, CursorKey}`，全部真实使用。
- **严格 query DTO（本模块局部，与 `RolesListQuery`/`UsersListQuery` 同型且相互独立，无悬空 import）**：

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupsListQuery {
    pub(crate) cursor: Option<String>,
    pub(crate) limit: Option<usize>,
}
```

- **handler（inner + 生产 wrapper，测试注入规则同 users/roles：test-only wrapper 调同一 inner、provider 走测试局部 key，不触碰全局 OnceLock、不给 `GroupsApiState` 扩 secret 字段；生产恒只经 `management_cursor_secret()`）**：

```rust
async fn list_groups_with_secret_provider<F>(
    state: &GroupsApiState,
    headers: &HeaderMap,
    uri: &Uri,
    secret_provider: F,
) -> Response
where
    F: FnOnce() -> Result<[u8; 32], ControllerError>,
{
    let actor = match authenticate_read(&state.auth, headers, EndpointId::GroupsRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let query = match Query::<GroupsListQuery>::try_from_uri(uri) {
        Ok(query) => query.0,
        Err(_) => return map_error(ControllerError::InvalidArgument),
    };
    let limit = match query.limit {
        None => 50usize,
        Some(value) if (1..=200).contains(&value) => value,
        Some(_) => return map_error(ControllerError::InvalidArgument),
    };
    let filter = CursorFilterCtx::empty();
    let secret = match secret_provider() {
        Ok(secret) => secret,
        Err(error) => return map_error(error),
    };
    let cursor = match decode_management_list_cursor(&secret, EndpointId::GroupsRead, actor.user.id, &filter, query.cursor.as_deref()) {
        Ok(None) => None,
        Ok(Some(CursorKey::Uuid(raw))) => Some(Uuid::from_bytes(raw)),
        Ok(Some(CursorKey::DeviceKey(_))) => return map_error(ControllerError::InvalidArgument),
        Err(error) => return map_error(error),
    };
    let page = match state.gateway.list(&actor, cursor, limit).await {
        Ok(page) => page,
        Err(error) => return map_error(error),
    };
    match sign_management_list_page(&secret, EndpointId::GroupsRead, actor.user.id, &filter, page) {
        Ok(body) => ok_response(body),
        Err(error) => map_error(error),
    }
}

pub(crate) async fn list_groups(
    State(state): State<GroupsApiState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    list_groups_with_secret_provider(&state, &headers, &uri, || management_cursor_secret().map(|secret| *secret)).await
}
```

- [ ] **Step 4: 运行并确认 GREEN**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::groups::tests
```
预期：PASS。

- [ ] **Step 5: 静态检查与条件性提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/api/groups.rs
git commit -m "feat(controller): groups 资源管理路由（4 条端点）"
```

---

### Task 10: devices 资源管理与最小投影路由（3 条）

**Files:**
- Modify: `crates/rsetup-controller/src/api/devices.rs`
- Test: 同文件的 `#[cfg(test)] mod tests`

**契约矩阵：**

| 路径与方法 | 权限 | 输入 | 响应 `data` 形状 | 异常状态 |
| :--- | :--- | :--- | :--- | :--- |
| `GET /api/v1/devices` | ≥1 权限的已认证用户 | `cursor?`, `limit?`, `group_id?`, `search?` | `{ items: [{ device_id, display_name, effective_permissions }], next_cursor: String? }`（**最低识别投影**；无权限设备完全排除，不泄露总数） | 401, 400 |
| `GET /api/v1/devices/{id}` | **必须持有 `device.read`**（reboot-only 严禁进入详情） | 无 | `{ device_id, display_name, admission_state, review_decision, revision, descriptor, connection_state, capabilities, effective_permissions }` | 401, 403 `PERMISSION_DENIED`（缺 read）/ 404(不可见与不存在同形 404) |
| `PATCH /api/v1/devices/{id}` | admin（仅中控档案） | `{ expected_revision, display_name }` | 更新后档案 | 401, 403, 404, 409 `REVISION_CONFLICT` |

**Interfaces:**
- Produces: `DevicesGateway`（完整dyn-safe定义见Task6）；`DeviceLiveProbe`及其同次缓存视图如下；`pub fn devices_router(DevicesApiState) -> Router<()>`。

```rust
// api/devices.rs；serde/serde_json为既有依赖。仅HTTP视图，不创建第二套wire DTO。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceConnectionState { Online, Offline }
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStreamHealth { Starting, Healthy, Degraded, Offline }
#[derive(Clone, Debug)]
pub struct DeviceLiveView {
    pub connection_state: DeviceConnectionState,
    pub control_health: DeviceStreamHealth,
    pub data_health: DeviceStreamHealth,
    // Transport真实typed缓存适配器按唯一共享DTO做02语义投影；未知样本为None。
    // Value不是原始wire入口，也不是未经验证的任意设备JSON。
    pub capabilities: Option<serde_json::Value>,
}
pub trait DeviceLiveProbe: Send + Sync {
    fn snapshot<'a>(&'a self, id: &'a [u8; 32]) -> super::ManagementFuture<'a, DeviceLiveView>;
}
```

GET详情在单snapshot `gateway.find_device`成功后，只调用一次 `live_probe.snapshot(&id).await`，同次取得连接/双流健康/能力；probe错误映射503，不能改成offline/空能力200。None能力仅表示真实缓存没有样本，不是缺接线默认值。view只用于已获DeviceRead的详情，不返回额外设备成员。

- [ ] **Step 1: 编写失败测试（最低识别投影、不可见与缺失同形 404、reboot-only 访问详情严格拒收 403）**

```rust
// crates/rsetup-controller/src/api/devices.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_fixture::*;
    use tower::ServiceExt;
    use axum::http::StatusCode;
    use std::sync::{Arc, OnceLock};

    // fixture_devices_state()/fixture_reboot_only_state() 及 FixtureDevicesGateway/FixtureLiveProbe
    // 按共享前置层文末声明随 Task 10 落地于 test_fixture 模块内（本区不重复定义，避免两处定义冲突）；
    // FixtureDevicesGateway 按用例参数化：缺失/不可见恒 find_device=Ok(None)、reboot-only 恒
    // Err(PermissionDenied)、list 可见项含 device_id/display_name/effective_permissions。
    // Host/Cookie/CSRF 口径沿用共享层；RED 为骨架未挂载路由的 404 行为断言失败，不得是编译错误。

    // GET list cursor 测试的局部密钥注入（与 users 区先例同型，强制约束）：生产 list_devices
    // 恒只经 management_cursor_secret() 取密钥，单元测试进程内全局 OnceLock 未安装，若直接挂
    // fixture_devices_state() 返回的生产 devices_router，GET /api/v1/devices 恒 503
    // STORAGE_UNAVAILABLE，list_devices_returns_minimum_projection_only 的 200 断言必然失败。
    // 因此该 GET list 阳性用例不得复用 fixture_devices_state() 的 router：本区 tests 内定义
    // SignedDevicesCursorTestState { devices: DevicesApiState, secret_slot: Arc<OnceLock<[u8; 32]>> }
    // 与 test-only wrapper，wrapper 内部调用同一个 list_devices_with_secret_provider（真实
    // gate/query/codec/gateway 顺序不绕过），provider 经
    // move || crate::api::cursor_secret_of(&slot).map(|secret| *secret) 从本测试局部槽读取；
    // fixture helper signed_devices_cursor_router(secret: Option<[u8; 32]>) 在本区定义
    // SYNTHETIC_SECRET = [7u8; 32]（与 users 区同值的测试合成 key），Some 时经
    // crate::api::install_cursor_secret_into 装入局部槽、None 时留空作网关前 503 负例；
    // 测试 router 只挂 wrapper 的 GET 路由并 .with_state(state) 归约。详情/PATCH 等非 cursor
    // 用例（含 reboot-only 403 与同形 404）不经 cursor 密钥，继续用
    // fixture_devices_state()/fixture_reboot_only_state() 挂生产 router 不变。
    // 红线：不得对全局 MANAGEMENT_CURSOR_SECRET 调用 set，不得给 DevicesApiState 扩 secret
    // 字段，不得用 test 专属假 handler 或自签 token 绕过真实链路。

    // 测试局部密钥槽 state（与 users 区 SignedCursorTestState 先例同型）：devices 持同一 DevicesApiState，secret_slot 为本 router 独占的局部 OnceLock，不触碰全局 MANAGEMENT_CURSOR_SECRET。
    #[derive(Clone)]
    struct SignedDevicesCursorTestState {
        devices: DevicesApiState,
        secret_slot: Arc<OnceLock<[u8; 32]>>,
    }

    // test-only wrapper handler：内部调用同一个 list_devices_with_secret_provider，真实 gate/query/codec/gateway 顺序不绕过，provider 经 cursor_secret_of 从本测试局部槽读取。
    async fn list_devices_with_test_secret(
        State(state): State<SignedDevicesCursorTestState>,
        headers: HeaderMap,
        uri: Uri,
    ) -> Response {
        let slot = state.secret_slot.clone();
        list_devices_with_secret_provider(&state.devices, &headers, &uri, move || crate::api::cursor_secret_of(&slot).map(|secret| *secret)).await
    }

    const SYNTHETIC_SECRET: [u8; 32] = [7u8; 32];

    // fixture helper（与 users 区 signed_cursor_router 先例同型）：只挂 wrapper 的 GET list 路由并
    // .with_state(state) 归约；Some 经 install_cursor_secret_into 装入测试合成 key、None 留空作
    // 网关前 503 负例。网关取共享层 visible 三态变体（list_devices_visible 返回含
    // device_id/display_name/effective_permissions 的可见项），三态变体命名以共享层最终实现为准。
    fn signed_devices_cursor_router(secret: Option<[u8; 32]>) -> axum::Router<()> {
        let secret_slot = Arc::new(OnceLock::new());
        if let Some(secret) = secret {
            crate::api::install_cursor_secret_into(&secret_slot, &secret).expect("synthetic key 安装必须成功");
        }
        let gateway: Arc<dyn DevicesGateway> = Arc::new(FixtureDevicesGateway::Visible);
        let live_probe: Arc<dyn DeviceLiveProbe> = Arc::new(FixtureLiveProbe);
        let state = SignedDevicesCursorTestState {
            devices: DevicesApiState { auth: fixture_auth(fixture_session(true)), gateway, live_probe },
            secret_slot,
        };
        axum::Router::new()
            .route("/api/v1/devices", axum::routing::get(list_devices_with_test_secret))
            .with_state(state)
    }

    #[tokio::test]
    async fn invisible_and_missing_device_return_identical_404_envelope() {
        let (router, _state) = fixture_devices_state();
        let router_invisible = router.clone();
        // dev-missing 不在仓储中
        let req_missing = authenticated_get("/api/v1/devices/0000000000000000000000000000000000000000000000000000000000000001");
        let res_missing = router.oneshot(req_missing).await.unwrap();
        // dev-invisible 在仓储中但 actor 无任何权限
        let req_invisible = authenticated_get("/api/v1/devices/0000000000000000000000000000000000000000000000000000000000000002");
        let res_invisible = router_invisible.oneshot(req_invisible).await.unwrap();
        assert_eq!(res_missing.status(), StatusCode::NOT_FOUND);
        assert_eq!(res_invisible.status(), StatusCode::NOT_FOUND);
        let mut val_m: serde_json::Value = body_json(res_missing).await;
        let mut val_i: serde_json::Value = body_json(res_invisible).await;
        val_m.as_object_mut().unwrap().remove("request_id");
        val_i.as_object_mut().unwrap().remove("request_id");
        assert_eq!(val_m, val_i, "identical 404 envelope must not leak device existence");
    }

    #[tokio::test]
    async fn reboot_only_actor_cannot_read_device_detail() {
        let (router, _state) = fixture_reboot_only_state();
        // actor 仅有 device.reboot 权限
        let req = authenticated_get("/api/v1/devices/0000000000000000000000000000000000000000000000000000000000000003");
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let val: serde_json::Value = body_json(res).await;
        assert_eq!(val["error"]["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn list_devices_returns_minimum_projection_only() {
        let router = signed_devices_cursor_router(Some(SYNTHETIC_SECRET));
        let req = authenticated_get("/api/v1/devices");
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let val: serde_json::Value = body_json(res).await;
        let item = &val["data"]["items"][0];
        assert!(item.get("device_id").is_some());
        assert!(item.get("display_name").is_some());
        assert!(item.get("effective_permissions").is_some());
        assert!(item.get("descriptor").is_none(), "descriptor must be hidden in list");
        assert!(item.get("admission_state").is_none(), "admission_state hidden in list");
    }
}
```

- [ ] **Step 2: 运行并确认行为 RED**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::devices::tests
```
预期：3 个测试全部 FAIL（骨架未挂载路由返回 404）。

- [ ] **Step 3: 最小实现**

挂载 3 条路由：
- `GET /api/v1/devices`（cursor 分页真实接线与 group_id/search 规范筛选绑定，本单元范围）：`list_devices` 为 devices 唯一 GET list handler；**唯一注册锚点**为 `devices_router` 的 `GET /api/v1/devices` 入口中唯一的 `get(list_devices)`，不新增第二处注册、不改动详情/PATCH 路由。调用顺序：`authenticate_read(EndpointId::DevicesRead)`（**严禁调用 `require_admin_session`**——spec01 允许 ≥1 权限的已认证普通用户（如 reboot-only）读取最低识别投影；可见范围、最低识别投影（`device_id`/`display_name`/`effective_permissions`，完全排除无权限设备、严禁返回总数）由网关 `list_devices_visible` 单快照保证，handler 不再扩展任何字段）→ `Query::try_from_uri`（门禁之后手动调用，不让 Axum extractor 在鉴权前短路 400）→ limit 域校验（默认 50、合法域 `1..=200`，`0` 或 `>200` 一律 400）→ `group_id` 解析复用 `parse_uuid_path`（缺省 → None，非法 → 400，唯一来源 `super::parse_uuid_path`）→ `search` 规范清洗（去前后空白；trim 后为空 → None；非空须 ≤128 Unicode 字符且 ≤512 UTF-8 字节且无 control 字符，否则 400，不得默默 truncate）→ `CursorFilterCtx::from_ordered_fields`（filter 唯一构造路径：tag 1 = `group_id` UUID raw 16 字节、tag 2 = `search` utf-8 字节切片，schema 恒含两字段、缺省 → `None`；`limit` 不进 schema、不参与 MAC 绑定）→ 取密钥（即使 `cursor` 为 `None` 也必须先调 `management_cursor_secret()`，未安装 → `Config` → `map_error` 精确 503，不得因 `None` 短路跳过）→ `decode_management_list_cursor`（`EndpointId::DevicesRead` → `Device` profile：`CursorKey::DeviceKey(raw32)` 为合法形态；`CursorKey::Uuid` 在本 profile 不可达，仍 fail-closed 400；篡改/跨端点/跨用户/换筛选复用 → `InvalidArgument` 400）→ `state.gateway.list_devices_visible(&actor, cursor, limit, group_id, search)`（`search` 已是借用自 query 的规范化 `&str`，与 `Option<String>` 形态的 `.as_deref()` 等价；同一 actor、同一请求内规范化筛选）→ `sign_management_list_page`（同一 secret/endpoint/actor/同一 ctx 实例；入口与出口必须复用同一请求内同一 `CursorFilterCtx` 实例，否则下一页 token 解不回；下一页请求改写或缺省 `group_id`/`search` → filter 不一致 → MAC 校验失败 400）→ `ok_response`；配置/后端错误经 `map_error` 映射（client 400、config 503、backend 503），绝不吞 `Err` 或返回未签名页面。
- **严格 query DTO（本模块局部，与 `UsersListQuery`/`GrantsListQuery`/`GroupsListQuery` 同型且相互独立，无悬空 import）**：

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DevicesListQuery {
    pub(crate) cursor: Option<String>,
    pub(crate) limit: Option<usize>,
    pub(crate) group_id: Option<String>,
    pub(crate) search: Option<String>,
}
```

- **导入（与 `api/devices.rs` 顶层既有 import 合并、不重复）**：`axum::extract::{Query, State}`、`axum::http::{HeaderMap, Uri}`、`axum::response::Response`、`serde::Deserialize`、`uuid::Uuid`、`crate::error::ControllerError`、`crate::http_api::ok_response`、`crate::http_auth::EndpointId`、`crate::http_gate::{authenticate_read, password_change_required}`（**不导入 `require_admin_session`**——权限例外在 import 层面固化，编译期即保证本端点不会套用 admin 门禁；`password_change_required` 供临时密码前置检查返回 403 `PASSWORD_CHANGE_REQUIRED`）、`super::{decode_management_list_cursor, management_cursor_secret, map_error, parse_uuid_path, sign_management_list_page, CursorFilterCtx, CursorKey}`，全部真实使用。
- **handler（inner + 生产 wrapper，与 `list_grants_with_secret_provider` 同型）**：`secret_provider` 为唯一密钥入口，测试注入规则与 users/roles/grants 相同（test-only wrapper 调同一 inner，provider 从测试局部 `OnceLock` 或 `move || Ok(local_key)` 闭包读取，不触碰全局 `MANAGEMENT_CURSOR_SECRET` OnceLock、不给 `DevicesApiState` 扩 secret 字段）；生产 `list_devices` 恒只经 env 安装的 accessor `management_cursor_secret()`：

```rust
const DEVICES_FILTER_TAG_GROUP_ID: u8 = 1;
const DEVICES_FILTER_TAG_SEARCH: u8 = 2;

async fn list_devices_with_secret_provider<F>(
    state: &DevicesApiState,
    headers: &HeaderMap,
    uri: &Uri,
    secret_provider: F,
) -> Response
where
    F: FnOnce() -> Result<[u8; 32], ControllerError>,
{
    let actor = match authenticate_read(&state.auth, headers, EndpointId::DevicesRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    // spec01 权限例外：此处不得调用 require_admin_session——≥1 权限普通用户可读最低投影。
    // 但临时密码会话（01 身份规格 L18）仅允许 auth/me、password、logout，其余端点一律 403。
    if actor.user.must_change_password {
        return password_change_required();
    }
    let query = match Query::<DevicesListQuery>::try_from_uri(uri) {
        Ok(query) => query.0,
        Err(_) => return map_error(ControllerError::InvalidArgument),
    };
    let limit = match query.limit {
        None => 50usize,
        Some(value) if (1..=200).contains(&value) => value,
        Some(_) => return map_error(ControllerError::InvalidArgument),
    };
    let group_id = match query.group_id.as_deref() {
        None => None,
        Some(raw) => match parse_uuid_path(raw) {
            Ok(id) => Some(id),
            Err(response) => return response,
        },
    };
    let search = match query.search.as_deref() {
        None => None,
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else if trimmed.chars().count() > 128
                || trimmed.len() > 512
                || trimmed.chars().any(|c| c.is_control())
            {
                return map_error(ControllerError::InvalidArgument);
            } else {
                Some(trimmed)
            }
        }
    };
    let filter = match CursorFilterCtx::from_ordered_fields(&[
        (DEVICES_FILTER_TAG_GROUP_ID, group_id.as_ref().map(|id| id.as_bytes().as_slice())),
        (DEVICES_FILTER_TAG_SEARCH, search.map(|value| value.as_bytes())),
    ]) {
        Ok(filter) => filter,
        Err(error) => return map_error(error),
    };
    let secret = match secret_provider() {
        Ok(secret) => secret,
        Err(error) => return map_error(error),
    };
    let cursor = match decode_management_list_cursor(&secret, EndpointId::DevicesRead, actor.user.id, &filter, query.cursor.as_deref()) {
        Ok(None) => None,
        Ok(Some(CursorKey::DeviceKey(raw))) => Some(raw),
        Ok(Some(CursorKey::Uuid(_))) => return map_error(ControllerError::InvalidArgument),
        Err(error) => return map_error(error),
    };
    let page = match state.gateway.list_devices_visible(&actor, cursor, limit, group_id, search).await {
        Ok(page) => page,
        Err(error) => return map_error(error),
    };
    match sign_management_list_page(&secret, EndpointId::DevicesRead, actor.user.id, &filter, page) {
        Ok(body) => ok_response(body),
        Err(error) => map_error(error),
    }
}

pub(crate) async fn list_devices(
    State(state): State<DevicesApiState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    list_devices_with_secret_provider(&state, &headers, &uri, || management_cursor_secret().map(|secret| *secret)).await
}
```

- **局部 RED/GREEN 未来说明（本单元不执行命令、不声称全链批准）**：同型 inner + test-only wrapper（测试局部 synthetic key）覆盖三组断言：分页闭环（第一页 `next_cursor` + 同一 `group_id`/`search` 文本取到第二页）、筛选绑定（下一页改写或缺省 `group_id`/`search` → 400）、密钥未安装（provider 返回 `Config` → 精确 503，含 `cursor=None` 路径）；另加权限例外阳性对照（reboot-only 普通用户 200 + 最低投影字段集）。完整 HTTP 测试矩阵为独立后续任务。
- `GET /api/v1/devices/{id}`：调 `authenticate_read(..., EndpointId::DeviceDetailRead)`；随后独立临时密码前置检查（与 list 同口径，绝不套用 admin 门禁）：`actor.user.must_change_password` 为真即 `return password_change_required()`（403 `PASSWORD_CHANGE_REQUIRED`，01 身份规格 L18）；通过网关/仓储执行一次设备详情读取 `state.gateway.find_device(&actor, id)`（单快照 typed 返回 `Option<(DeviceAdminView, HashSet<Permission>)>`）：
  - 返回 `Ok(None)` → 统一映射 404 `NOT_FOUND`（不可见与不存在同形，无论普通用户还是 admin）；
  - 返回 `Err(ControllerError::PermissionDenied)` → 统一映射 403 `PERMISSION_DENIED`（存在未归档且有其他权限但缺 `DeviceRead`）；
  - 数据库/配置错误（`ControllerError::Database` / `ControllerError::Config`）→ 统一映射 503 `STORAGE_UNAVAILABLE`；
  - 返回 `Ok(Some((view, perms)))` → 一次`live_probe.snapshot(&id).await`取得同次连接/双流健康/能力投影并构造完整200详情；probe错误503，不改成offline/空能力成功。
  - **严禁**在此端点额外调用 `effective_permissions` 或 `require_permission` 造成双快照读；严禁另造跨线 DTO/HTTP 字段；不新增独立 HTTP 端点或授权点、不放开 CRUD、不普通调用 admin helper。适配器消费契约同步：`DevicesGateway::find_device(&self, actor: &Session, id: [u8; 32]) -> BoxFuture<'_, Result<Option<(DeviceAdminView, HashSet<Permission>)>, ControllerError>>` 直通底层的单一 snapshot 仓储方法；完整 gateway/probe 接线属 C2 另待，不假称已完成。
- `PATCH /api/v1/devices/{id}`：`authenticate_write` → `require_admin_session` → `parse_revision` → `validate_display_name` → 调网关。

- [ ] **Step 4: 运行并确认 GREEN**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::devices::tests
```
预期：PASS（3 个测试通过）。

- [ ] **Step 5: 静态检查与条件性提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/api/devices.rs
git commit -m "feat(controller): devices 资源管理与最小投影路由（3 条端点）"
```

---

