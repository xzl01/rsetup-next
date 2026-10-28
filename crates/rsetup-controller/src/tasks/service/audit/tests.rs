use super::*;

#[test]
fn audit_reason_is_never_echoed_verbatim() {
    let redacted = redact_reason("operator forced release after 3 retries");
    assert!(!redacted.contains("operator"));
    assert!(!redacted.contains("retries"));
    assert_eq!(redacted, "redacted:6");
}
