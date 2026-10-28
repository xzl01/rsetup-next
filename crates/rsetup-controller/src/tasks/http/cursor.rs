//! 任务游标编解码与端点隔离。
//!
//! 提供基于 HMAC-SHA256 的任务分页游标签发与校验，支持 `Tasks` 与 `TaskChildren`
//! 两种端点上下文隔离。

use uuid::Uuid;

use crate::error::ControllerError;

/// 游标 HMAC 域分隔常量：与 `SessionAliasKey` 同一风格，变更即令全部游标失效。
const TASK_CURSOR_DOMAIN: &[u8] = b"rsetup-task-cursor-v1/";
/// 游标字节布局：24B payload（`created_at_ms` 大端 8B ‖ `tie_break_id` 16B）+ 8B tag。
const TASK_CURSOR_BYTES: usize = 32;
/// HMAC tag 截断长度（与账本决议④补充 UUID 键 profile 一致）。
const TASK_CURSOR_TAG_BYTES: usize = 8;

/// 游标端点上下文：HMAC 输入含 scope 字节，跨端点复用必在解码时失败。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskCursorScope {
    Tasks,
    TaskChildren,
}

impl TaskCursorScope {
    fn scope_byte(self) -> u8 {
        match self {
            TaskCursorScope::Tasks => 0x01,
            TaskCursorScope::TaskChildren => 0x02,
        }
    }
}

/// HMAC-SHA256（`sha2` 手写 ipad/opad，与 `auth::session::hmac_sha256` 同一构造；
/// 零新增依赖）。
fn hmac_sha256(key: &[u8; 32], domain: &[u8], scope_byte: u8, payload: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..32 {
        ipad[i] ^= key[i];
        opad[i] ^= key[i];
    }
    let mut inner = sha2::Sha256::new();
    inner.update(ipad);
    inner.update(domain);
    inner.update([scope_byte]);
    inner.update(payload);
    let inner_hash = inner.finalize();

    let mut outer = sha2::Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

/// 恒定时间比较（长度已由调用方固定，不提前短路内容差异）。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

/// 签发游标：`hex(payload ‖ HMAC-SHA256(secret, domain ‖ scope ‖ payload)[..8])`
/// = 64 小写 hex。密钥由调用方（TaskApi 生产实现）显式传入，HTTP 层不经手。
pub fn encode_task_cursor(
    endpoint: TaskCursorScope,
    created_at_ms: i64,
    tie_break_id: Uuid,
    secret: &[u8; 32],
) -> String {
    let mut payload = [0u8; 24];
    payload[..8].copy_from_slice(&created_at_ms.to_be_bytes());
    payload[8..].copy_from_slice(tie_break_id.as_bytes());
    let mac = hmac_sha256(secret, TASK_CURSOR_DOMAIN, endpoint.scope_byte(), &payload);

    let mut raw = [0u8; TASK_CURSOR_BYTES];
    raw[..24].copy_from_slice(&payload);
    raw[24..].copy_from_slice(&mac[..TASK_CURSOR_TAG_BYTES]);
    hex::encode(raw)
}

/// 解码游标，校验序**先 MAC 后字段**：① 恰 64 小写 hex → ② 以当前 scope 重算
/// HMAC 并恒定时间比较 8B tag → ③ tag 通过后才解析字段。任一步失败一律
/// `InvalidArgument`（400），不区分篡改/过期/外来细节，也不得回退到未分页全量。
pub fn decode_task_cursor(
    endpoint: TaskCursorScope,
    cursor: &str,
    secret: &[u8; 32],
) -> Result<(i64, Uuid), ControllerError> {
    if cursor.len() != 64
        || !cursor
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(ControllerError::InvalidArgument);
    }
    let mut raw = [0u8; TASK_CURSOR_BYTES];
    hex::decode_to_slice(cursor, &mut raw).map_err(|_| ControllerError::InvalidArgument)?;

    let mac = hmac_sha256(
        secret,
        TASK_CURSOR_DOMAIN,
        endpoint.scope_byte(),
        &raw[..24],
    );
    if !constant_time_eq(&raw[24..], &mac[..TASK_CURSOR_TAG_BYTES]) {
        return Err(ControllerError::InvalidArgument);
    }

    let created_at_ms = i64::from_be_bytes(
        raw[..8]
            .try_into()
            .map_err(|_| ControllerError::InvalidArgument)?,
    );
    let tie_break_id = Uuid::from_bytes(
        raw[8..24]
            .try_into()
            .map_err(|_| ControllerError::InvalidArgument)?,
    );
    Ok((created_at_ms, tie_break_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREATED_AT_MS: i64 = 1_725_000_000_123;
    const SECRET: &[u8; 32] = &[0x5A; 32];

    fn tie_break_id() -> Uuid {
        Uuid::parse_str("7c9e6679-7425-40de-944b-e07fc1f90ae7").unwrap()
    }

    #[test]
    fn task_cursor_round_trip_success() {
        let token = encode_task_cursor(
            TaskCursorScope::Tasks,
            CREATED_AT_MS,
            tie_break_id(),
            SECRET,
        );
        assert_eq!(token.len(), 64);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        let decoded = decode_task_cursor(TaskCursorScope::Tasks, &token, SECRET)
            .expect("同 secret 同 scope 必须精确解回");
        assert_eq!(decoded, (CREATED_AT_MS, tie_break_id()));
    }

    #[test]
    fn task_cursor_rejects_tampered_tag_with_invalid_argument() {
        let token = encode_task_cursor(
            TaskCursorScope::Tasks,
            CREATED_AT_MS,
            tie_break_id(),
            SECRET,
        );
        let mut tampered: Vec<char> = token.chars().collect();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == '0' { '1' } else { '0' };
        let tampered: String = tampered.into_iter().collect();
        let err = decode_task_cursor(TaskCursorScope::Tasks, &tampered, SECRET)
            .expect_err("篡改 tag 必须失败");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }

    #[test]
    fn task_cursor_rejects_cross_endpoint_with_invalid_argument() {
        let token = encode_task_cursor(
            TaskCursorScope::Tasks,
            CREATED_AT_MS,
            tie_break_id(),
            SECRET,
        );
        let err = decode_task_cursor(TaskCursorScope::TaskChildren, &token, SECRET)
            .expect_err("游标不得跨端点复用");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }

    #[test]
    fn task_cursor_rejects_malformed_length_or_hex() {
        let token = encode_task_cursor(
            TaskCursorScope::Tasks,
            CREATED_AT_MS,
            tie_break_id(),
            SECRET,
        );
        let mut too_short = token.clone();
        too_short.pop();
        let mut too_long = token.clone();
        too_long.push('0');
        let mut non_hex = token.clone();
        non_hex.replace_range(0..1, "z");
        let cases = [too_short, too_long, non_hex, token.to_uppercase()];
        for case in cases {
            let err = decode_task_cursor(TaskCursorScope::Tasks, &case, SECRET)
                .expect_err("格式非法必须失败");
            assert!(matches!(err, ControllerError::InvalidArgument));
        }
    }
}
