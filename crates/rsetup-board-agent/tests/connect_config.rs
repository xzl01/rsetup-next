use rsetup_board_agent::connect_config::{BoardConnectConfig, ConnectConfigError};
use std::path::Path;
fn config(address: &str, keys: &str) -> Result<BoardConnectConfig, ConnectConfigError> {
    BoardConnectConfig::from_values(
        address,
        Path::new("/var/lib/rsetup-board-agent/identity.pk8"),
        Path::new("/var/lib/rsetup-board-agent/journal"),
        keys,
    )
}
#[test]
fn explicit_pins_are_required_and_never_inferred_from_address() {
    assert!(config("controller:8443", "").is_err());
    assert!(config("controller:8443", "   ").is_err());
    let c = config("controller:8443", &"ab".repeat(32)).unwrap();
    assert_eq!(c.trusted_servers, vec![[0xab; 32]]);
}
#[test]
fn rotation_keys_are_bounded_distinct_nonzero_and_exact_width() {
    assert!(config("127.0.0.1:8443", &"00".repeat(32)).is_err());
    assert!(config("127.0.0.1:8443", "ab").is_err());
    let duplicate = format!("{},{}", "ab".repeat(32), "AB".repeat(32));
    assert!(config("127.0.0.1:8443", &duplicate).is_err());
    let many = (1u8..=17)
        .map(|v| hex::encode([v; 32]))
        .collect::<Vec<_>>()
        .join(",");
    assert!(config("127.0.0.1:8443", &many).is_err());
    let two = format!("{}, {}", "ab".repeat(32), "cd".repeat(32));
    assert_eq!(
        config("127.0.0.1:8443", &two)
            .unwrap()
            .trusted_servers
            .len(),
        2
    );
}
#[test]
fn address_is_a_host_port_not_a_url_or_credentials() {
    for bad in [
        "",
        "https://controller:8443",
        "name@controller:8443",
        "controller:0",
        "controller:+8443",
        "controller:65536",
        " controller:8443",
        "controller:8443/path",
        "[::1]",
        "::1:8443",
    ] {
        assert!(config(bad, &"ab".repeat(32)).is_err(), "{bad}");
    }
    for good in [
        "controller:8443",
        "host.example:8443",
        "127.0.0.1:8443",
        "[::1]:8443",
    ] {
        assert!(config(good, &"ab".repeat(32)).is_ok(), "{good}");
    }
}
#[test]
fn state_paths_must_be_absolute_and_without_parent_traversal() {
    for bad in ["relative/identity", "/var/lib/../identity", ""] {
        assert!(
            BoardConnectConfig::from_values(
                "controller:8443",
                Path::new(bad),
                Path::new("/var/lib/rsetup-board-agent/journal"),
                &"ab".repeat(32)
            )
            .is_err()
        );
    }
}
#[test]
fn environment_lookup_requires_pin_and_has_safe_paths() {
    let c = BoardConnectConfig::from_lookup(|key| match key {
        "RSETUP_CONTROLLER_ADDR" => Some("controller:8443".into()),
        "RSETUP_CONTROLLER_PUBLIC_KEYS" => Some("ab".repeat(32)),
        _ => None,
    })
    .unwrap();
    assert_eq!(
        c.identity_file,
        Path::new("/var/lib/rsetup-board-agent/identity.pk8")
    );
    assert_eq!(
        c.journal_dir,
        Path::new("/var/lib/rsetup-board-agent/journal")
    );
    assert!(BoardConnectConfig::from_lookup(|_| None).is_err());
}
#[test]
fn debug_and_errors_never_echo_keys_addresses_or_paths() {
    let key = "ab".repeat(32);
    let c = config("private-host:8443", &key).unwrap();
    let dbg = format!("{c:?}");
    assert!(!dbg.contains(&key));
    assert!(!dbg.contains("private-host"));
    assert!(!dbg.contains("/var/lib"));
    let e = config("http://private-host:8443", &key).unwrap_err();
    assert!(!format!("{e:?} {e}").contains("private-host"));
}

#[test]
fn explicitly_empty_state_path_environment_is_rejected_not_defaulted() {
    for empty_key in ["RSETUP_BOARD_KEY_FILE", "RSETUP_BOARD_JOURNAL_DIR"] {
        let result = BoardConnectConfig::from_lookup(|key| match key {
            "RSETUP_CONTROLLER_ADDR" => Some("controller:8443".into()),
            "RSETUP_CONTROLLER_PUBLIC_KEYS" => Some("ab".repeat(32)),
            value if value == empty_key => Some(String::new()),
            _ => None,
        });
        assert_eq!(result.unwrap_err(), ConnectConfigError::StatePath);
    }
}
#[test]
fn identity_and_journal_must_not_use_the_same_path() {
    let path = Path::new("/var/lib/rsetup-board-agent/shared");
    let result = BoardConnectConfig::from_values("controller:8443", path, path, &"ab".repeat(32));
    assert_eq!(result.unwrap_err(), ConnectConfigError::StatePath);
}
