//! 任务预览的纯逻辑层：目标规范化、摘要计算、令牌派生与进程内单调有效期。
//!
//! 本模块零 IO、零数据库连接，全部行为可离线单测。持久化写入见
//! [`crate::tasks::repository`]，提交编排见 [`crate::tasks::service`]。
//!
//! 令牌口径（RT-N2）：对外唯一凭据是 64 位小写 hex 的 `preview_token`；
//! `preview_token_hash` 与 `token_nonce` 都是它的**服务端派生值**，
//! 客户端永不携带 hash/nonce，因此外部无法注入内部主键。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::ControllerError;
use crate::tasks::model::ReasonCode;

/// 单次提交允许的目标数下限（含）。
pub const PREVIEW_TARGET_MIN: usize = 1;

/// 单次提交允许的目标数上限（含）；`0` 与 `> 1024` 一律 `InvalidArgument`。
pub const PREVIEW_TARGET_MAX: usize = 1024;

/// 预览有效期：进程内单调 60 秒，墙钟回拨不得延长。
pub const PREVIEW_TTL: Duration = Duration::from_secs(60);

/// `device_target_hash` 的域分隔常量；变更即令全部存量预览失效。
const TARGET_HASH_DOMAIN: &[u8] = b"rsetup.task.targets.v1";

/// `request_hash` 的域分隔常量。
const REQUEST_HASH_DOMAIN: &[u8] = b"rsetup.task.request.v1";

/// `token_nonce` 的域分隔常量：非密钥派生，仅令「外部 token → 内部主键」映射可恢复。
const PREVIEW_NONCE_DOMAIN: &[u8] = b"rsetup:preview:nonce:v1";

/// 预览目标：`eligible` 为 false 时 `reason_code` 必须给出原因（不伪造资格）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewTarget {
    pub device_id: [u8; 32],
    pub eligible: bool,
    pub reason_code: Option<ReasonCode>,
}

/// 预览结果：只回一枚 `preview_token`，不回任何服务端派生摘要或主键。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewOutcome {
    pub preview_token: String,
    pub targets: Vec<PreviewTarget>,
}

/// 校验目标数量：仅 `1..=1024` 合法，`0` 与 `> 1024` 均为 `InvalidArgument`。
pub fn validate_target_count(count: usize) -> Result<(), ControllerError> {
    if (PREVIEW_TARGET_MIN..=PREVIEW_TARGET_MAX).contains(&count) {
        Ok(())
    } else {
        Err(ControllerError::InvalidArgument)
    }
}

/// 规范化目标顺序：字节序升序 + 去重（提交侧锁序与快照比较共用同一口径）。
pub fn canonical_device_order(ids: &mut Vec<[u8; 32]>) {
    ids.sort_unstable();
    ids.dedup();
}

/// 目标清单摘要：长度前缀编码 + 规范化升序，SHA256 摘要与输入顺序无关、与内容敏感。
/// 域分隔常量变更即令全部存量摘要失效。
pub fn device_target_hash(device_ids: &[[u8; 32]], group_ids: &[Uuid]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(TARGET_HASH_DOMAIN);
    hasher.update((device_ids.len() as u64).to_be_bytes());
    let mut ids: Vec<[u8; 32]> = device_ids.to_vec();
    ids.sort_unstable();
    for id in &ids {
        hasher.update(id);
    }
    hasher.update((group_ids.len() as u64).to_be_bytes());
    let mut groups: Vec<Uuid> = group_ids.to_vec();
    groups.sort_unstable();
    for group in &groups {
        hasher.update(group.as_bytes());
    }
    hasher.finalize().into()
}

/// 请求摘要：幂等重放判定的唯一比较对象（同一幂等键只允许同一摘要）。
/// 绑定预览令牌与目标清单摘要；令牌以长度前缀编码，避免与相邻字段字节拼接歧义。
pub fn request_hash(preview_token: &str, device_ids: &[[u8; 32]], group_ids: &[Uuid]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(REQUEST_HASH_DOMAIN);
    hasher.update((preview_token.len() as u64).to_be_bytes());
    hasher.update(preview_token.as_bytes());
    hasher.update(device_target_hash(device_ids, group_ids));
    hasher.finalize().into()
}

/// 幂等键必须是 canonical 小写 UUIDv4：36 字符、连字符位固定、小写 hex、
/// `version == 4`、`variant ∈ {8,9,a,b}`。大写与非 canonical 形式一律拒绝。
pub fn is_canonical_uuid_v4_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if *byte != b'-' {
                return false;
            }
        } else if !matches!(byte, b'0'..=b'9' | b'a'..=b'f') {
            return false;
        }
    }
    bytes[14] == b'4' && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}

/// 严格解码 64 位小写 hex；大写、长度不符、非 hex 一律 `None`（不 trim、不折叠大小写）。
fn hex64_to_bytes(raw: &str) -> Option<[u8; 32]> {
    let bytes = raw.as_bytes();
    if bytes.len() != 64
        || !bytes
            .iter()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return None;
    }
    hex::decode(raw).ok()?.try_into().ok()
}

/// 严格解码 64 位小写 hex 预览令牌；大写、长度不符、非 hex 一律 `None`（不 trim、不折叠大小写）。
pub fn preview_token_bytes(raw: &str) -> Option<[u8; 32]> {
    hex64_to_bytes(raw)
}

/// `preview_token_hash` 列值：`Sha256(token)`。
pub fn preview_token_digest(token: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(token);
    hasher.finalize().into()
}

/// `token_nonce` 主键值：域分隔的 `Sha256(domain ‖ token)`，非密钥派生。
pub fn preview_token_nonce(token: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(PREVIEW_NONCE_DOMAIN);
    hasher.update(token);
    hasher.finalize().into()
}

/// 生成新的预览令牌：两段 `Uuid::new_v4` 的 simple hex 拼接（64 位小写 hex）。
/// uuid v4 是本仓库既有的 CSPRNG 随机源，不新增依赖。
pub fn new_preview_token() -> String {
    let head = Uuid::new_v4().simple().to_string();
    let tail = Uuid::new_v4().simple().to_string();
    let mut token = head;
    token.push_str(&tail);
    token
}

/// 进程启动 epoch（16 字节）：跨代际预览一律失效，同时参与令牌派生链。
pub fn preview_generation_bytes() -> [u8; 16] {
    static PREVIEW_GENERATION: OnceLock<[u8; 16]> = OnceLock::new();
    *PREVIEW_GENERATION.get_or_init(|| Uuid::new_v4().into_bytes())
}

/// 进程内单调期限登记表：`nonce -> 单调截止时刻`。判定链只读 `Instant`，
/// 绝不读墙钟，因此墙钟回拨不得延长有效期；表缺失/毒化一律 fail-closed。
static PREVIEW_MONO_DEADLINES: Mutex<Option<HashMap<[u8; 32], Instant>>> = Mutex::new(None);

fn deadline_registry_error() -> ControllerError {
    ControllerError::Config("preview deadline registry unavailable".into())
}

/// 以注入的单调时刻登记 60 秒期限：新 nonce 插入、重复登记保留首次 deadline，
/// 并顺带剪除已到期项。返回该 nonce 登记后是否处于登记表内。
fn register_deadline_at(nonce: [u8; 32], now: Instant) -> Result<bool, ControllerError> {
    let mut guard = PREVIEW_MONO_DEADLINES
        .lock()
        .map_err(|_| deadline_registry_error())?;
    let map = guard.get_or_insert_with(HashMap::new);
    map.retain(|_, deadline| *deadline > now);
    map.entry(nonce).or_insert(now + PREVIEW_TTL);
    Ok(map.contains_key(&nonce))
}

/// 以注入的单调时刻判定期限是否仍未到期。登记表缺失即 fail-closed。
fn mono_deadline_allows_at(nonce: &[u8; 32], now: Instant) -> bool {
    let Ok(guard) = PREVIEW_MONO_DEADLINES.lock() else {
        return false;
    };
    let Some(map) = guard.as_ref() else {
        return false;
    };
    map.get(nonce).is_some_and(|deadline| *deadline >= now)
}

/// 读取某 nonce 当前登记的截止时刻（仅测试用于断言「重复注册保留首次值」）。
#[cfg(test)]
fn mono_deadline_at(nonce: &[u8; 32]) -> Option<Instant> {
    let Ok(guard) = PREVIEW_MONO_DEADLINES.lock() else {
        return None;
    };
    guard.as_ref()?.get(nonce).copied()
}

/// 清空登记表（仅测试用于隔离用例）。
#[cfg(test)]
fn clear_preview_mono_deadlines() {
    if let Ok(mut guard) = PREVIEW_MONO_DEADLINES.lock()
        && let Some(map) = guard.as_mut()
    {
        map.clear();
    }
}

/// 预览落库成功后唯一的单调期限注册点。**必须**在 INSERT 提交成功之后调用：
/// 先注册会为未落库的行留下单调记录。登记表毒化返回 `Config` 错误，调用方
/// 据此放弃该预览（fail-closed，不做补偿删除）。
pub fn register_preview_mono_deadline(nonce: [u8; 32]) -> Result<(), ControllerError> {
    register_deadline_at(nonce, Instant::now())?;
    Ok(())
}

/// 消费侧单调有效性判定：仅当本进程登记过该 nonce 且单调期限未到才为 true。
/// 全零 nonce、跨进程 nonce、墙钟回拨后的 nonce 均判 false。
pub fn preview_mono_valid(nonce: &[u8; 32]) -> bool {
    mono_deadline_allows_at(nonce, Instant::now())
}

/// 目标快照 JSON 的唯一写出格式：64 位小写 hex 字符串的扁平数组。
/// `Value: Display` 恒成功，因此不 unwrap 任何序列化 `Result`。
pub fn encode_preview_targets(ids: &[[u8; 32]]) -> String {
    serde_json::Value::Array(
        ids.iter()
            .map(|id| serde_json::Value::String(hex::encode(id)))
            .collect(),
    )
    .to_string()
}

/// 解析目标快照：扁平 JSON 数组 + 每项 64 位小写 hex + 必须已是 canonical 升序去重清单。
/// 任何偏离（非数组、大写 hex、长度不符、乱序、重复）都视为快照损坏，返回 `None`。
pub fn decode_preview_targets(raw: &str) -> Option<Vec<[u8; 32]>> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let items = value.as_array()?;
    let mut ids = Vec::with_capacity(items.len());
    for item in items {
        ids.push(hex64_to_bytes(item.as_str()?)?);
    }
    let mut canonical = ids.clone();
    canonical_device_order(&mut canonical);
    if canonical != ids {
        return None;
    }
    Some(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(seed: u8, len: usize) -> Vec<[u8; 32]> {
        (0..len as u8).map(|i| [seed + i; 32]).collect()
    }

    #[test]
    fn preview_rejects_empty_and_excessive_targets() {
        assert!(matches!(
            validate_target_count(0),
            Err(crate::ControllerError::InvalidArgument)
        ));
        assert!(validate_target_count(1).is_ok());
        assert!(validate_target_count(1024).is_ok());
        assert!(matches!(
            validate_target_count(1025),
            Err(crate::ControllerError::InvalidArgument)
        ));
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
        assert_ne!(
            device_target_hash(&a, &[]),
            device_target_hash(&a[..1], &[])
        );
        let g = [uuid::Uuid::from_u128(7)];
        assert_ne!(device_target_hash(&a, &[]), device_target_hash(&a, &g));
    }

    #[test]
    fn idempotency_key_must_be_canonical_lowercase_uuid_v4() {
        assert!(is_canonical_uuid_v4_key(
            "3f2504e0-4f89-41d3-9a0c-0305e82c3301"
        ));
        assert!(!is_canonical_uuid_v4_key(
            "3F2504E0-4F89-41D3-9A0C-0305E82C3301"
        ));
        assert!(!is_canonical_uuid_v4_key(
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301"
        )); // version 1
        assert!(!is_canonical_uuid_v4_key(
            "3f2504e04f8941d39a0c0305e82c3301"
        )); // 无连字符
        assert!(!is_canonical_uuid_v4_key(
            "3f2504e0-4f89-41d3-1a0c-0305e82c3301"
        )); // variant 非法
        assert!(!is_canonical_uuid_v4_key(""));
        assert!(!is_canonical_uuid_v4_key(
            &"3f2504e0-4f89-41d3-9a0c-0305e82c330".repeat(2)
        ));
    }

    #[test]
    fn request_hash_binds_token_and_targets() {
        let a = [[0x01u8; 32], [0x02u8; 32]];
        let b = [[0x02u8; 32], [0x01u8; 32]];
        let base = request_hash("tok-a", &a, &[]);
        assert_eq!(base, request_hash("tok-a", &b, &[]), "目标顺序不得影响摘要");
        assert_ne!(base, request_hash("tok-b", &a, &[]), "令牌必须参与摘要");
        assert_ne!(
            base,
            request_hash("tok-a", &a[..1], &[]),
            "目标内容必须参与摘要"
        );
        let g = [uuid::Uuid::from_u128(9)];
        assert_ne!(base, request_hash("tok-a", &a, &g), "分组必须参与摘要");
    }

    #[test]
    fn preview_token_bytes_is_strict_lowercase_hex_only() {
        let token = new_preview_token();
        assert_eq!(token.len(), 64);
        assert!(preview_token_bytes(&token).is_some());
        assert_eq!(preview_token_bytes(&token.to_uppercase()), None);
        assert!(preview_token_bytes(&token[..63]).is_none());
        assert!(preview_token_bytes(&format!("{token}0")).is_none());
        assert!(preview_token_bytes(&format!(" {token}")).is_none());
        assert!(preview_token_bytes("").is_none());
        // 派生值必须确定且彼此域分隔
        let raw = preview_token_bytes(&token).expect("canonical token");
        assert_eq!(preview_token_digest(&raw), preview_token_digest(&raw));
        assert_ne!(
            preview_token_digest(&raw),
            preview_token_nonce(&raw),
            "hash 与 nonce 必须域分隔，不得同值"
        );
        let other = new_preview_token();
        let other_raw = preview_token_bytes(&other).expect("canonical token");
        assert_ne!(preview_token_nonce(&raw), preview_token_nonce(&other_raw));
    }

    #[test]
    fn preview_generation_is_stable_within_process() {
        assert_eq!(preview_generation_bytes(), preview_generation_bytes());
    }

    #[test]
    fn mono_deadline_enforces_ttl_keeps_first_writer_and_prunes_expired() {
        // 登记表是进程级共享状态，本用例内顺序执行三段断言，避免并行用例互相剪除
        clear_preview_mono_deadlines();
        let nonce = preview_token_nonce(&preview_token_bytes(&new_preview_token()).expect("token"));
        let zero = [0u8; 32];
        assert!(
            !preview_mono_valid(&nonce),
            "未登记的 nonce 必须 fail-closed"
        );
        assert!(!preview_mono_valid(&zero), "全零 nonce 必须 fail-closed");

        // ① 判定入参只有单调时刻：判定链绝不读 Utc::now/DateTime::now，墙钟回拨无从延长有效期
        let base = Instant::now();
        assert!(register_deadline_at(nonce, base).expect("registry"));
        assert!(preview_mono_valid(&nonce), "登记后本进程内立即有效");
        assert!(mono_deadline_allows_at(
            &nonce,
            base + PREVIEW_TTL - Duration::from_secs(1)
        ));
        assert!(mono_deadline_allows_at(&nonce, base + PREVIEW_TTL));
        assert!(!mono_deadline_allows_at(
            &nonce,
            base + PREVIEW_TTL + Duration::from_secs(1)
        ));

        // ② 同一 nonce 重复注册保留首次 deadline
        let kept = mono_deadline_at(&nonce).expect("deadline registered");
        assert_eq!(kept, base + PREVIEW_TTL);
        assert!(
            register_deadline_at(nonce, base + Duration::from_secs(5)).expect("registry"),
            "重复注册必须幂等"
        );
        assert_eq!(mono_deadline_at(&nonce), Some(kept));

        // ③ 到期项在下次注册时被剪除，登记表不随请求线性增长
        let other = preview_token_nonce(&preview_token_bytes(&new_preview_token()).expect("token"));
        assert!(
            register_deadline_at(other, base + PREVIEW_TTL + Duration::from_secs(10))
                .expect("registry")
        );
        assert_eq!(mono_deadline_at(&nonce), None);
        assert!(mono_deadline_at(&other).is_some());
    }

    #[test]
    fn preview_targets_json_round_trips_and_rejects_corruption() {
        let mut targets = ids(0x10, 3);
        targets.reverse();
        let mut canonical = targets.clone();
        canonical_device_order(&mut canonical);
        let json = encode_preview_targets(&canonical);
        assert_eq!(decode_preview_targets(&json), Some(canonical.clone()));
        // 乱序快照视为损坏
        assert_eq!(
            decode_preview_targets(&encode_preview_targets(&targets)),
            None
        );
        // 重复项视为损坏
        let mut duplicated = canonical.clone();
        duplicated.push(canonical[0]);
        assert_eq!(
            decode_preview_targets(&encode_preview_targets(&duplicated)),
            None
        );
        // 非数组、大写 hex、空串一律视为损坏
        assert_eq!(decode_preview_targets("{}"), None);
        assert_eq!(decode_preview_targets("[\"AB\"]"), None);
        assert_eq!(decode_preview_targets("not json"), None);
        assert_eq!(
            decode_preview_targets(&encode_preview_targets(&[])),
            Some(vec![])
        );
    }
}
