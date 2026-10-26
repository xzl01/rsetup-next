// v4 形状校验与数据扫描共用的用户名纯规则；不提供旧版升级预检入口。
#[allow(dead_code)]
pub(crate) fn valid_username(s: &str) -> bool {
    let bytes = s.as_bytes();
    (3..=64).contains(&bytes.len())
        && bytes
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(*b, b'.' | b'_' | b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_rules_reject_noncanonical_input() {
        let long_ok = "a".repeat(64);
        let long_bad = "a".repeat(65);
        for good in ["abc", "a.b_c-1", long_ok.as_str()] {
            assert!(valid_username(good));
        }
        for bad in [
            "ab",
            long_bad.as_str(),
            "Éric",
            "Alice",
            "alice!",
            "-alice",
            "a b",
        ] {
            assert!(!valid_username(bad), "{bad:?}");
        }
    }
}
