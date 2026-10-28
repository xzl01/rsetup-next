use super::*;

#[test]
fn constant_time_eq_is_length_and_content_sensitive() {
    assert!(constant_time_eq(b"abc", b"abc"));
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"abc", b"abcd"));
    assert!(constant_time_eq(&[], &[]));
}
