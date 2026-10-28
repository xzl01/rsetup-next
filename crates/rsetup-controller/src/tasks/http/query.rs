//! 任务 HTTP 请求参数解析与查询控制。
//!
//! 包含分页常量限制、十进制 u64 严格解析、UUIDv4 幂等键及路径 ID 校验，
//! 以及任务预览过期时间推导。

use axum::http::HeaderName;
use uuid::Uuid;

use crate::error::ControllerError;
use crate::tasks::preview::{PREVIEW_TTL, is_canonical_uuid_v4_key};

/// 默认分页大小（02 §1 `Page`）。
pub const DEFAULT_PAGE_LIMIT: usize = 50;
/// 单页上限（02 §3 批量上限 200）。
pub const MAX_PAGE_LIMIT: usize = 200;
/// 幂等键请求头（仅 `POST /tasks` 校验）。
pub(super) const IDEMPOTENCY_KEY_HEADER: HeaderName = HeaderName::from_static("idempotency-key");

/// 预览有效期毫秒数：与 Task 4 `PREVIEW_TTL` 同源，由路由层补齐
/// （不改 `PreviewOutcome` 形状）。
pub(super) fn preview_expires_in_ms() -> u64 {
    u64::try_from(PREVIEW_TTL.as_millis()).unwrap_or(60_000)
}

/// 严格无符号十进制 u64 解析：拒绝空串、非数字、正负号、下划线、小数点、
/// 前导零（`"0"` 除外）与溢出。
pub fn parse_decimal_u64(value: &str) -> Result<u64, ControllerError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ControllerError::InvalidArgument);
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(ControllerError::InvalidArgument);
    }
    value
        .parse::<u64>()
        .map_err(|_| ControllerError::InvalidArgument)
}

/// 分页 limit：缺省 50，`1..=200` 合法，其余 `InvalidArgument`。
pub fn clamp_page_limit(requested: Option<usize>) -> Result<usize, ControllerError> {
    match requested {
        None => Ok(DEFAULT_PAGE_LIMIT),
        Some(value) if (1..=MAX_PAGE_LIMIT).contains(&value) => Ok(value),
        Some(_) => Err(ControllerError::InvalidArgument),
    }
}

/// 严格 canonical 小写 UUIDv4 幂等键（复用 Task 4 `is_canonical_uuid_v4_key`）；
/// 缺失或非法一律 `InvalidArgument`。
pub fn parse_idempotency_key(value: Option<&str>) -> Result<String, ControllerError> {
    match value.filter(|key| is_canonical_uuid_v4_key(key)) {
        Some(key) => Ok((*key).to_owned()),
        None => Err(ControllerError::InvalidArgument),
    }
}

/// 路径 ID：恰 36 字符 canonical 小写带连字符 UUIDv4（与 `is_canonical_uuid_v4_key`
/// 同一口径），其余一律 `InvalidArgument`（不交给 axum 默认 400 纯文本）。
pub(super) fn parse_path_id(raw: &str) -> Result<Uuid, ControllerError> {
    if !is_canonical_uuid_v4_key(raw) {
        return Err(ControllerError::InvalidArgument);
    }
    Uuid::parse_str(raw).map_err(|_| ControllerError::InvalidArgument)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::IDEMPOTENCY_KEY;
    use super::*;

    #[test]
    fn decimal_u64_rejects_negative_leading_zero_and_overflow() {
        assert_eq!(parse_decimal_u64("0").unwrap(), 0);
        assert_eq!(parse_decimal_u64("18446744073709551615").unwrap(), u64::MAX);
        for bad in [
            "",
            " ",
            "-1",
            "+1",
            "01",
            "007",
            "1_0",
            "1.0",
            "abc",
            "99999999999999999999999",
        ] {
            assert!(
                matches!(
                    parse_decimal_u64(bad),
                    Err(ControllerError::InvalidArgument)
                ),
                "非法十进制字符串必须 InvalidArgument: {bad:?}"
            );
        }
    }

    #[test]
    fn page_limit_defaults_to_50_and_caps_at_200() {
        assert_eq!(clamp_page_limit(None).unwrap(), 50);
        assert_eq!(clamp_page_limit(Some(1)).unwrap(), 1);
        assert_eq!(clamp_page_limit(Some(200)).unwrap(), 200);
        for bad in [0usize, 201, 1_000] {
            assert!(
                matches!(
                    clamp_page_limit(Some(bad)),
                    Err(ControllerError::InvalidArgument)
                ),
                "越界 limit 必须 InvalidArgument: {bad}"
            );
        }
    }

    #[test]
    fn idempotency_key_must_be_canonical_lowercase_uuidv4() {
        assert_eq!(
            parse_idempotency_key(Some(IDEMPOTENCY_KEY)).unwrap(),
            IDEMPOTENCY_KEY
        );
        assert!(matches!(
            parse_idempotency_key(None),
            Err(ControllerError::InvalidArgument)
        ));
        for bad in [
            "",
            "3F2504E0-4F89-41D3-9A0C-0305E82C3301",
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            "3f2504e04f8941d39a0c0305e82c3301",
            "not-a-uuid",
        ] {
            assert!(
                matches!(
                    parse_idempotency_key(Some(bad)),
                    Err(ControllerError::InvalidArgument)
                ),
                "非法幂等键必须 InvalidArgument: {bad:?}"
            );
        }
    }
}
