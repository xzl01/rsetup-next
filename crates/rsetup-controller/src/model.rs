#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionState {
    Pending,
    Approved,
    Revoked,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewDecision {
    None,
    Approved,
    Denied,
    Revoked,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionSnapshot {
    pub admission_state: AdmissionState,
    pub review_decision: ReviewDecision,
    pub revision: u64,
}

pub fn decimal_u64_json(value: u64) -> serde_json::Value {
    serde_json::Value::String(value.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn revision_is_exact_decimal_json_string() {
        let v = i64::MAX as u64 + 9;
        assert_eq!(super::decimal_u64_json(v), serde_json::json!(v.to_string()));
        assert_eq!(
            super::decimal_u64_json(u64::MAX),
            serde_json::json!(u64::MAX.to_string())
        );
    }
}
