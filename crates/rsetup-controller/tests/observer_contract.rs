const PYTHON_SYNTHETIC_ENVELOPE: &str = r#"
import hashlib
import sys
from dataclasses import replace
from datetime import datetime, timedelta, timezone
from pathlib import Path
try:
    sys.path.insert(0, sys.argv[1])
    from observer_runner import (ObserverAccount, ObserverAuthorization,
                                 OperatorRecord, ObserverError, authorize_and_envelope)
    writer = b'{"schema_version":1,"engine":"mysql","connection":{"host":"fixture.invalid","port":3306,"username":"w","password":"not-real","database":"Exact_DB"}}'
    observer = b'{"username":"o","password":"not-real-either"}'
    now = datetime.now(timezone.utc)
    fields = dict(engine='mysql', writer_path=Path('/synthetic/writer'),
                  observer_path=Path('/synthetic/observer'),
                  writer_pin=hashlib.sha256(writer).hexdigest(),
                  observer_pin=hashlib.sha256(observer).hexdigest(),
                  expected_account=ObserverAccount('o', '%'),
                  run_id='synthetic-offline-run',
                  window_start=(now-timedelta(minutes=5)).isoformat(timespec='microseconds').replace('+00:00','Z'),
                  window_end=(now+timedelta(minutes=5)).isoformat(timespec='microseconds'),
                  transport_policy_ref='synthetic-transport-record',
                  topology_ref='synthetic-topology-record',
                  operator_confirmed=True, scope='observer-capabilities')
    auth = ObserverAuthorization(**fields)
    record = OperatorRecord(**fields)  # independently instantiated synthetic input; not an operator act
    for changes in ({'window_start':'0000-01-01T00:00:00Z'},
                    {'observer_pin':fields['writer_pin']},
                    {'topology_ref':'data:payload'}):
        try:
            authorize_and_envelope(replace(auth, **changes), replace(record, **changes), now,
                                   'observer-capabilities')
        except ObserverError:
            pass
        else:
            raise SystemExit(1)
    wire = authorize_and_envelope(auth, record, now, 'observer-capabilities')
    sys.stdout.buffer.write(wire)
except Exception:
    raise SystemExit(1) from None
"#;

fn python_synthetic_wire() -> Vec<u8> {
    let output = std::process::Command::new("python3")
        .arg("-c")
        .arg(PYTHON_SYNTHETIC_ENVELOPE)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/observer-python"
        ))
        .output()
        .expect("python3 must be available for cross-language contract");
    assert!(
        output.status.success(),
        "synthetic Python envelope generation failed"
    );
    assert!(
        !output.stdout.is_empty(),
        "synthetic wire must not be empty"
    );
    output.stdout
}

#[test]
fn python_generated_exact_wire_parses_and_mutations_reject() {
    let wire = python_synthetic_wire();
    let parsed = observer::read_authorization(wire.as_slice())
        .unwrap_or_else(|_| panic!("Python-generated synthetic bytes must parse"));
    let expected = observer::Account {
        user: "o".into(),
        host: "%".into(),
    };
    assert!(
        observer::authorize_open(
            &prepared(WRITER, OBSERVER, observer::Engine::Mysql).unwrap(),
            Some(&parsed),
            &expected,
            "observer-capabilities",
            tokio::time::Instant::now() + std::time::Duration::from_secs(2)
        )
        .is_ok()
    );
    let mut mutated: serde_json::Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(mutated["writer_pin"], pin(WRITER));
    assert_eq!(mutated["observer_pin"], pin(OBSERVER));
    assert!(mutated["window_start"].as_str().unwrap().ends_with('Z'));
    assert!(mutated["window_end"].as_str().unwrap().ends_with("+00:00"));
    mutated["window_start"] = serde_json::json!("0000-01-01T00:00:00Z");
    assert!(matches!(
        observer::read_authorization(mutated.to_string().as_bytes()),
        Err("prerequisite_missing")
    ));
    mutated = serde_json::from_slice(&wire).unwrap();
    mutated["observer_pin"] = mutated["writer_pin"].clone();
    assert!(matches!(
        observer::read_authorization(mutated.to_string().as_bytes()),
        Err("prerequisite_missing")
    ));
    mutated = serde_json::from_slice(&wire).unwrap();
    mutated["topology_ref"] = serde_json::json!("data:payload");
    assert!(matches!(
        observer::read_authorization(mutated.to_string().as_bytes()),
        Err("prerequisite_missing")
    ));
}

fn synthetic_wire(engine: &str, writer_pin: &str, observer_pin: &str) -> serde_json::Value {
    let now = chrono::Utc::now();
    serde_json::json!({
        "engine": engine,
        "writer_pin": writer_pin,
        "observer_pin": observer_pin,
        "expected_account": {"user": "o", "host": "%"},
        "run_id": "synthetic-offline-run",
        "window_start": (now - chrono::Duration::minutes(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "window_end": (now + chrono::Duration::minutes(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "transport_policy_ref": "synthetic-transport-record",
        "topology_ref": "synthetic-topology-record",
        "operator_confirmed": true,
        "scope": "observer-capabilities"
    })
}

fn synthetic_auth(engine: &str, writer: &[u8]) -> observer::AuthorizedRun {
    let wire = synthetic_wire(engine, &pin(writer), &pin(OBSERVER));
    observer::read_authorization(wire.to_string().as_bytes())
        .unwrap_or_else(|_| panic!("synthetic envelope must parse"))
}

#[tokio::test]
async fn real_open_path_reaches_only_injected_no_network_connector() {
    use observer::{Account, Engine, FlowFailure, FlowStage};
    let calls = std::cell::Cell::new(0);
    let auth = observer::read_authorization(python_synthetic_wire().as_slice())
        .unwrap_or_else(|_| panic!("synthetic Python bytes must parse"));
    let result = observer::open_with(
        prepared(WRITER, OBSERVER, Engine::Mysql).unwrap(),
        Some(&auth),
        "observer-capabilities",
        &Account {
            user: "o".into(),
            host: "%".into(),
        },
        "writer@%",
        true,
        tokio::time::Instant::now() + std::time::Duration::from_secs(2),
        |_| async {
            calls.set(calls.get() + 1);
            Err("synthetic-private-connector-error")
        },
    )
    .await;
    assert!(matches!(
        result,
        Err(FlowFailure {
            stage: FlowStage::Connect,
            error_class: "query_error"
        })
    ));
    assert_eq!(calls.get(), 1);
}

#[tokio::test]
async fn shared_open_path_denials_never_invoke_connector() {
    use observer::{Account, Engine, FlowFailure, FlowStage};
    let wire = python_synthetic_wire();
    let valid = observer::read_authorization(wire.as_slice())
        .unwrap_or_else(|_| panic!("synthetic Python wire must parse"));
    let mut wrong_pin_wire: serde_json::Value = serde_json::from_slice(&wire).unwrap();
    wrong_pin_wire["writer_pin"] = serde_json::json!(pin(b"synthetic-other-writer"));
    let wrong_pin = observer::read_authorization(wrong_pin_wire.to_string().as_bytes())
        .unwrap_or_else(|_| panic!("wrong but shaped pin must parse"));
    let mut expired_wire: serde_json::Value = serde_json::from_slice(&wire).unwrap();
    expired_wire["window_end"] = serde_json::json!("2020-01-01T00:00:00Z");
    assert!(matches!(
        observer::read_authorization(expired_wire.to_string().as_bytes()),
        Err("prerequisite_missing")
    ));
    let mut invalid_wire: serde_json::Value = serde_json::from_slice(&wire).unwrap();
    invalid_wire["topology_ref"] = serde_json::json!("data:payload");
    assert!(matches!(
        observer::read_authorization(invalid_wire.to_string().as_bytes()),
        Err("prerequisite_missing")
    ));
    let account = Account {
        user: "o".into(),
        host: "%".into(),
    };
    let wrong_account = Account {
        user: "other".into(),
        host: "%".into(),
    };
    let calls = std::cell::Cell::new(0);
    for (label, auth, scope, expected, deadline) in [
        (
            "missing",
            None,
            "observer-capabilities",
            &account,
            tokio::time::Instant::now() + std::time::Duration::from_secs(2),
        ),
        (
            "invalid wire",
            None,
            "observer-capabilities",
            &account,
            tokio::time::Instant::now() + std::time::Duration::from_secs(2),
        ),
        (
            "expired wire",
            None,
            "observer-capabilities",
            &account,
            tokio::time::Instant::now() + std::time::Duration::from_secs(2),
        ),
        (
            "wrong scope",
            Some(&valid),
            "unapproved-case",
            &account,
            tokio::time::Instant::now() + std::time::Duration::from_secs(2),
        ),
        (
            "wrong pin",
            Some(&wrong_pin),
            "observer-capabilities",
            &account,
            tokio::time::Instant::now() + std::time::Duration::from_secs(2),
        ),
        (
            "wrong account",
            Some(&valid),
            "observer-capabilities",
            &wrong_account,
            tokio::time::Instant::now() + std::time::Duration::from_secs(2),
        ),
        (
            "elapsed deadline",
            Some(&valid),
            "observer-capabilities",
            &account,
            tokio::time::Instant::now() - std::time::Duration::from_secs(1),
        ),
    ] {
        let result = observer::open_with(
            prepared(WRITER, OBSERVER, Engine::Mysql).unwrap(),
            auth,
            scope,
            expected,
            "writer@%",
            true,
            deadline,
            |_| async {
                calls.set(calls.get() + 1);
                Err("query_error")
            },
        )
        .await;
        assert!(result.is_err(), "{label} must refuse");
        assert_eq!(calls.get(), 0, "{label} reached fake connector");
        if label == "elapsed deadline" {
            assert!(matches!(
                result,
                Err(FlowFailure {
                    stage: FlowStage::Connect,
                    error_class: "timeout"
                })
            ));
        }
    }
}

#[test]
fn pure_gate_accepts_synthetic_authorization() {
    use observer::{Account, Engine};
    let prepared = prepared(WRITER, OBSERVER, Engine::Mysql).unwrap();
    let auth = synthetic_auth("mysql", WRITER);
    let account = Account {
        user: "o".into(),
        host: "%".into(),
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    observer::authorize_open(
        &prepared,
        Some(&auth),
        &account,
        "observer-capabilities",
        deadline,
    )
    .unwrap_or_else(|_| panic!("valid synthetic gate denied"));
}

#[test]
fn zero_year_start_rejects_even_with_valid_future_end() {
    let mut wire = synthetic_wire("mysql", &pin(WRITER), &pin(OBSERVER));
    wire["window_start"] = serde_json::json!("0000-01-01T00:00:00Z");
    assert!(matches!(
        observer::read_authorization(wire.to_string().as_bytes()),
        Err("prerequisite_missing")
    ));
}

#[test]
fn equal_lowercase_pins_reject_at_parser() {
    let same_pin = "0".repeat(64);
    let wire = synthetic_wire("mysql", &same_pin, &same_pin);
    assert!(matches!(
        observer::read_authorization(wire.to_string().as_bytes()),
        Err("prerequisite_missing")
    ));
}

#[test]
fn authorization_parser_rejects_unbounded_duplicate_extra_and_invalid_wire() {
    let wire = synthetic_wire("mysql", &pin(WRITER), &pin(OBSERVER));
    let base = wire.to_string();
    let mut extras = wire.clone();
    extras["password"] = serde_json::json!("not-allowed");
    let mut nested = wire.clone();
    nested["expected_account"]["url"] = serde_json::json!("not-allowed");
    let mut null = wire.clone();
    null["scope"] = serde_json::Value::Null;
    let mut false_confirm = wire.clone();
    false_confirm["operator_confirmed"] = serde_json::json!(false);
    let mut bad_pin = wire.clone();
    bad_pin["writer_pin"] = serde_json::json!("A".repeat(64));
    let mut missing = wire.clone();
    missing.as_object_mut().unwrap().remove("topology_ref");
    let mut scheme = wire.clone();
    scheme["transport_policy_ref"] = serde_json::json!("data:payload");
    let mut invalid_day = wire.clone();
    invalid_day["window_start"] = serde_json::json!("2026-02-30T12:00:00Z");
    let mut excess_fraction = wire.clone();
    excess_fraction["window_end"] = serde_json::json!(format!(
        "{}.1234567Z",
        wire["window_end"].as_str().unwrap().trim_end_matches('Z')
    ));
    let duplicate = base.replacen(
        "\"engine\":\"mysql\"",
        "\"engine\":\"mysql\",\"\\u0065ngine\":\"tidb\"",
        1,
    );
    let nested_duplicate =
        base.replacen("\"host\":\"%\"", "\"host\":\"%\",\"\\u0068ost\":\"%\"", 1);
    for invalid in [
        "".to_owned(),
        format!("{base} false"),
        extras.to_string(),
        nested.to_string(),
        null.to_string(),
        false_confirm.to_string(),
        bad_pin.to_string(),
        missing.to_string(),
        scheme.to_string(),
        invalid_day.to_string(),
        excess_fraction.to_string(),
        duplicate,
        nested_duplicate,
        " ".repeat(65537),
    ] {
        assert!(observer::read_authorization(invalid.as_bytes()).is_err());
    }
    struct FailsAfterOne(bool);
    impl std::io::Read for FailsAfterOne {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0 {
                Err(std::io::Error::other("private reader error"))
            } else {
                self.0 = true;
                buf[0] = b'{';
                Ok(1)
            }
        }
    }
    assert!(matches!(
        observer::read_authorization(FailsAfterOne(false)),
        Err("prerequisite_missing")
    ));
}

#[test]
fn pure_preconnect_gate_rejects_missing_stale_mismatch_and_scope() {
    use observer::{Account, Engine};
    let account = Account {
        user: "o".into(),
        host: "%".into(),
    };
    let wrong = Account {
        user: "someone".into(),
        host: "%".into(),
    };
    let prepared = prepared(WRITER, OBSERVER, Engine::Mysql).unwrap();
    let auth = synthetic_auth("mysql", WRITER);
    let mut calls = 0;
    for (a, scope, expected, deadline) in [
        (
            None,
            "observer-capabilities",
            &account,
            tokio::time::Instant::now() + std::time::Duration::from_secs(1),
        ),
        (
            Some(&auth),
            "unapproved-case",
            &account,
            tokio::time::Instant::now() + std::time::Duration::from_secs(1),
        ),
        (
            Some(&auth),
            "observer-capabilities",
            &wrong,
            tokio::time::Instant::now() + std::time::Duration::from_secs(1),
        ),
        (
            Some(&auth),
            "observer-capabilities",
            &account,
            tokio::time::Instant::now() - std::time::Duration::from_secs(1),
        ),
    ] {
        if observer::authorize_open(&prepared, a, expected, scope, deadline).is_ok() {
            calls += 1;
        }
    }
    for wire in [
        synthetic_wire("tidb", &pin(WRITER), &pin(OBSERVER)),
        synthetic_wire("mysql", &pin(OBSERVER), &pin(OBSERVER)),
        {
            let mut w = synthetic_wire("mysql", &pin(WRITER), &pin(OBSERVER));
            w["window_end"] = serde_json::json!("2020-01-01T00:00:00Z");
            w
        },
    ] {
        if let Ok(auth) = observer::read_authorization(wire.to_string().as_bytes()) {
            if observer::authorize_open(
                &prepared,
                Some(&auth),
                &account,
                "observer-capabilities",
                tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .is_ok()
            {
                calls += 1;
            }
        }
    }
    assert_eq!(calls, 0);
}

#[test]
fn matching_synthetic_case_accepts_microseconds_and_explicit_utc_offset() {
    use observer::{Account, Engine};
    let mut wire = synthetic_wire(
        "tidb",
        &pin(&WRITER.replace_bytes(b"mysql", b"tidb")),
        &pin(OBSERVER),
    );
    wire["scope"] = serde_json::json!(
        "admission_deactivation_holds_guard_cas_waits_then_denied_on_fresh_v3_tidb_observer"
    );
    for key in ["window_start", "window_end"] {
        let timestamp = wire[key].as_str().unwrap().trim_end_matches('Z');
        wire[key] = serde_json::json!(format!("{timestamp}.123456+00:00"));
    }
    let auth = observer::read_authorization(wire.to_string().as_bytes())
        .unwrap_or_else(|_| panic!("synthetic UTC case must parse"));
    let writer = WRITER.replace_bytes(b"mysql", b"tidb");
    let prepared = prepared(&writer, OBSERVER, Engine::Tidb).unwrap();
    let expected = Account {
        user: "o".into(),
        host: "%".into(),
    };
    assert!(
        observer::authorize_open(
            &prepared,
            Some(&auth),
            &expected,
            "admission_deactivation_holds_guard_cas_waits_then_denied_on_fresh_v3_tidb_observer",
            tokio::time::Instant::now() + std::time::Duration::from_secs(1)
        )
        .is_ok()
    );
}

#[test]
fn cross_engine_case_scope_never_reaches_fake_connector() {
    use observer::{Account, Engine};
    let mut wire = synthetic_wire("mysql", &pin(WRITER), &pin(OBSERVER));
    wire["scope"] = serde_json::json!(
        "admission_deactivation_holds_guard_cas_waits_then_denied_on_fresh_v3_tidb_observer"
    );
    let auth = observer::read_authorization(wire.to_string().as_bytes());
    let prepared = prepared(WRITER, OBSERVER, Engine::Mysql).unwrap();
    let account = Account {
        user: "o".into(),
        host: "%".into(),
    };
    let mut connections = 0;
    if let Ok(auth) = auth {
        let gate = observer::authorize_open(
            &prepared,
            Some(&auth),
            &account,
            "admission_deactivation_holds_guard_cas_waits_then_denied_on_fresh_v3_tidb_observer",
            tokio::time::Instant::now() + std::time::Duration::from_secs(2),
        );
        if gate.is_ok() {
            connections += 1;
        }
    }
    assert_eq!(connections, 0);
}

#[test]
fn prepared_engine_is_pinned_to_the_verified_writer_shape() {
    use observer::Engine;
    assert_eq!(
        prepared(WRITER, OBSERVER, Engine::Mysql).unwrap().engine(),
        Engine::Mysql
    );
    let tidb_writer = WRITER.replace_bytes(b"mysql", b"tidb");
    assert_eq!(
        prepared(&tidb_writer, OBSERVER, Engine::Tidb)
            .unwrap()
            .engine(),
        Engine::Tidb
    );
    assert!(matches!(
        prepared(WRITER, OBSERVER, Engine::Tidb),
        Err("config_invalid")
    ));
    assert!(matches!(
        prepared(&tidb_writer, OBSERVER, Engine::Mysql),
        Err("config_invalid")
    ));
}

#[test]
fn sqlx_adapter_compiles_and_row_shape_rejects_missing_or_duplicate_rows() {
    fn assert_read_executor<E: observer::ReadExecutor>() {}
    assert_read_executor::<observer::SqlxReadExecutor<'static>>();
    // No MySqlConnection is created; these cardinality checks are used by the real decoder.
    assert!(matches!(
        observer::exactly_one(Vec::<u8>::new()),
        Err(observer::ReadFailure::Decode)
    ));
    assert!(matches!(
        observer::exactly_one(vec![1, 2]),
        Err(observer::ReadFailure::Decode)
    ));
    assert!(matches!(observer::exactly_one(vec![7]), Ok(7)));
    assert!(matches!(observer::zero_or_one::<u8>(vec![]), Ok(None)));
    assert!(matches!(observer::zero_or_one(vec![7]), Ok(Some(7))));
    assert!(matches!(
        observer::zero_or_one(vec![1, 2]),
        Err(observer::ReadFailure::Decode)
    ));
}

#[tokio::test]
async fn sqlx_session_rejects_missing_writer_without_connection_attempt() {
    use observer::{Account, Engine, FlowFailure, FlowStage, ObserverSession};
    let p = prepared(WRITER, OBSERVER, Engine::Mysql).unwrap();
    let auth = synthetic_auth("mysql", WRITER);
    let result = ObserverSession::open(
        p,
        Some(&auth),
        "observer-capabilities",
        &Account {
            user: "o".into(),
            host: "%".into(),
        },
        "",
        true,
        tokio::time::Instant::now() + std::time::Duration::from_secs(1),
    )
    .await;
    assert!(matches!(
        result,
        Err(FlowFailure {
            stage: FlowStage::Prerequisite,
            error_class: "identity_mismatch"
        })
    ));
    let tidb_writer = WRITER.replace_bytes(b"mysql", b"tidb");
    let p = prepared(&tidb_writer, OBSERVER, Engine::Tidb).unwrap();
    let auth_tidb = synthetic_auth("tidb", &tidb_writer);
    let result = ObserverSession::open(
        p,
        Some(&auth_tidb),
        "observer-capabilities",
        &Account {
            user: "o".into(),
            host: "%".into(),
        },
        "writer@%",
        false,
        tokio::time::Instant::now() + std::time::Duration::from_secs(1),
    )
    .await;
    assert!(matches!(
        result,
        Err(FlowFailure {
            stage: FlowStage::Prerequisite,
            error_class: "prerequisite_missing"
        })
    ));
}

#[tokio::test]
async fn sqlx_session_expired_deadline_never_attempts_connect() {
    use observer::{Account, Engine, FlowFailure, FlowStage, ObserverSession};
    let p = prepared(WRITER, OBSERVER, Engine::Mysql).unwrap();
    let auth = synthetic_auth("mysql", WRITER);
    let result = ObserverSession::open(
        p,
        Some(&auth),
        "observer-capabilities",
        &Account {
            user: "o".into(),
            host: "%".into(),
        },
        "writer@%",
        true,
        tokio::time::Instant::now() - std::time::Duration::from_millis(1),
    )
    .await;
    assert!(matches!(
        result,
        Err(FlowFailure {
            stage: FlowStage::Connect,
            error_class: "timeout"
        })
    ));
}

#[test]
fn sqlx_adapter_fixed_stage_is_part_of_compile_only_contract() {
    fn accepts_observer_session(_: &observer::ObserverSession) {}
    let _ = accepts_observer_session;
    let stage = observer::FlowStage::Connect;
    assert!(matches!(stage, observer::FlowStage::Connect));
}

#[path = "support/observer/mod.rs"]
mod observer;

use sha2::{Digest, Sha256};

fn pin(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

const WRITER: &[u8] = br#"{"schema_version":1,"engine":"mysql","connection":{"host":"fixture.invalid","port":3306,"username":"w","password":"not-real","database":"Exact_DB"}}"#;
const OBSERVER: &[u8] = br#"{"username":"o","password":"not-real-either"}"#;

fn prepared(
    writer: &[u8],
    observer_bytes: &[u8],
    engine: observer::Engine,
) -> Result<observer::PreparedObserver, observer::ObserverError> {
    let writer_pin = pin(writer);
    let observer_pin = pin(observer_bytes);
    observer::prepare(
        observer::PinnedInput {
            writer_bytes: writer,
            observer_bytes,
            writer_pin: &writer_pin,
            observer_pin: &observer_pin,
        },
        engine,
    )
}

#[test]
fn observer_derives_endpoint_without_database_or_writer_mutation() {
    use observer::Engine;
    let before = WRITER.to_vec();
    let p = prepared(WRITER, OBSERVER, Engine::Mysql).unwrap();
    assert_eq!(p.options().get_host(), "fixture.invalid");
    assert_eq!(p.options().get_port(), 3306);
    assert_eq!(p.options().get_username(), "o");
    assert_eq!(p.options().get_database(), None);
    assert_eq!(p.options().get_socket(), None);
    assert_eq!(p.schema(), "Exact_DB");
    assert!(matches!(
        p.options().get_ssl_mode(),
        sqlx::mysql::MySqlSslMode::Preferred
    ));
    assert_eq!(WRITER, before);
    // Same options construction for TiDB; never claim this proves actual transport security.
    assert_eq!(
        prepared(
            &WRITER.replace_bytes(b"mysql", b"tidb"),
            OBSERVER,
            Engine::Tidb
        )
        .unwrap()
        .options()
        .get_database(),
        None
    );
}

#[test]
fn observer_rejects_bad_dual_pins_and_bounded_bytes() {
    use observer::{Engine, PinnedInput, prepare};
    let wp = pin(WRITER);
    let op = pin(OBSERVER);
    let uppercase = "A".repeat(64);
    for (bad_wp, bad_op) in [
        (op.as_str(), op.as_str()),
        (wp.as_str(), wp.as_str()),
        ("0", op.as_str()),
        (wp.as_str(), uppercase.as_str()),
    ] {
        assert!(
            prepare(
                PinnedInput {
                    writer_bytes: WRITER,
                    observer_bytes: OBSERVER,
                    writer_pin: bad_wp,
                    observer_pin: bad_op
                },
                Engine::Mysql
            )
            .is_err()
        );
    }
    let mut changed = OBSERVER.to_vec();
    changed.push(b' ');
    assert!(
        prepare(
            PinnedInput {
                writer_bytes: WRITER,
                observer_bytes: &changed,
                writer_pin: &wp,
                observer_pin: &op
            },
            Engine::Mysql
        )
        .is_err()
    );
    for (w, o) in [
        (vec![b' '; 65537], OBSERVER.to_vec()),
        (WRITER.to_vec(), vec![b' '; 65537]),
    ] {
        assert!(prepared(&w, &o, Engine::Mysql).is_err());
    }
}

#[test]
fn observer_json_requires_only_two_distinct_nonempty_string_fields() {
    use observer::Engine;
    for raw in [
        br#"{"username":"o","password":"x","host":"override"}"#.as_slice(),
        br#"{"username":"o","username":"other","password":"x"}"#,
        br#"{"username":"o","\u0075sername":"other","password":"x"}"#,
        br#"{"username":"o","password":"x","pass\u0077ord":"x"}"#,
        br#"{"username":"","password":"x"}"#,
        br#"{"username":"o","password":""}"#,
        br#"{"username":42,"password":"x"}"#,
        br#"{"username":"o","password":null}"#,
        br#"{"username":"o","password":"x"}{}"#,
        br#"{"username":"o","password":"x",}"#,
        br#"["o","x"]"#,
        br#"{"username":"o","password":"x","ssl_mode":"REQUIRED"}"#,
        br#"{"username":"o","password":"x","url":"not-a-url"}"#,
        br#"{"username":"o","password":"x","socket":"/not-real"}"#,
    ] {
        assert!(prepared(WRITER, raw, Engine::Mysql).is_err());
    }
    assert!(
        prepared(
            WRITER,
            br#"{"\u0075sername":"o","password":"x"}"#,
            Engine::Mysql
        )
        .is_ok()
    );
}

#[test]
fn observer_root_rejects_form_feed_with_matching_dual_pins() {
    use observer::Engine;
    let mut after_object = OBSERVER.to_vec();
    after_object.push(0x0c);
    for (index, raw) in [
        OBSERVER.replace_bytes(b"{\"username\"", b"{\x0c\"username\""),
        OBSERVER.replace_bytes(b",\"password\"", b",\x0c\"password\""),
        after_object,
    ]
    .into_iter()
    .enumerate()
    {
        assert!(serde_json::from_slice::<serde_json::Value>(&raw).is_err());
        assert!(
            matches!(prepared(WRITER, &raw, Engine::Mysql), Err("config_invalid")),
            "observer root form-feed at position {index} must be rejected"
        );
    }
}

#[test]
fn writer_root_rejects_form_feed_with_matching_dual_pins() {
    use observer::Engine;
    let mut after_object = WRITER.to_vec();
    after_object.push(0x0c);
    for (index, raw) in [
        WRITER.replace_bytes(b"{\"schema_version\"", b"{\x0c\"schema_version\""),
        WRITER.replace_bytes(b",\"engine\"", b",\x0c\"engine\""),
        after_object,
    ]
    .into_iter()
    .enumerate()
    {
        assert!(serde_json::from_slice::<serde_json::Value>(&raw).is_err());
        assert!(
            matches!(
                prepared(&raw, OBSERVER, Engine::Mysql),
                Err("config_invalid")
            ),
            "writer root form-feed at position {index} must be rejected"
        );
    }
}

#[test]
fn observer_writer_shape_rejects_unrepresentable_connection_fields() {
    use observer::Engine;
    for raw in [
        WRITER.replace_bytes(b"3306", b"0"),
        WRITER.replace_bytes(b"3306", b"65536"),
        WRITER.replace_bytes(b"mysql", b"tidb"),
        WRITER.replace_bytes(b"\"database\"", b"\"socket\""),
        WRITER.replace_bytes(b"\"database\"", b"\"url\""),
        WRITER.replace_bytes(b"\"database\"", b"\"ssl_mode\""),
        WRITER.replace_bytes(b"\"database\"", b"\"tls\""),
        WRITER.replace_bytes(b"\"database\"", b"\"host\""),
        WRITER.replace_bytes(b"\"schema_version\":1", b"\"schema_version\":2"),
        WRITER.replace_bytes(b"\"host\":\"fixture.invalid\"", b"\"host\":\"\""),
    ] {
        assert!(prepared(&raw, OBSERVER, Engine::Mysql).is_err());
    }
    for field in [
        br#", "socket":"/not-real""#.as_slice(),
        br#", "url":"not-a-url""#,
        br#", "tls":"required""#,
        br#", "ssl_mode":"REQUIRED""#,
        br#", "host":"other.invalid""#,
        br#", "\u0068ost":"other.invalid""#,
    ] {
        let mut connection_extra = WRITER.to_vec();
        let closing = connection_extra.len() - 2;
        connection_extra.splice(closing..closing, field.iter().copied());
        assert!(prepared(&connection_extra, OBSERVER, Engine::Mysql).is_err());
    }
    let mut extra = WRITER.to_vec();
    extra.pop();
    extra.extend_from_slice(br#", "connection_url":"not-a-url"}"#);
    assert!(prepared(&extra, OBSERVER, Engine::Mysql).is_err());
}

trait FixtureReplace {
    fn replace_bytes(&self, from: &[u8], to: &[u8]) -> Vec<u8>;
}
impl FixtureReplace for [u8] {
    fn replace_bytes(&self, from: &[u8], to: &[u8]) -> Vec<u8> {
        let at = self.windows(from.len()).position(|w| w == from).unwrap();
        [&self[..at], to, &self[at + from.len()..]].concat()
    }
}

#[cfg(target_os = "linux")]
#[test]
fn inherited_handoff_child_contract() {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};
    let root = std::env::current_dir()
        .unwrap()
        .join(".superpowers/sdd/2026-10-03-controller-task4-isolated-observer")
        .join(format!("o4b1-handoff-fixture-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    for (name, contents) in [
        ("writer", WRITER),
        ("observer", OBSERVER),
        ("artifact", b"synthetic-artifact".as_slice()),
    ] {
        let path = root.join(name);
        fs::write(&path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    for case in [
        "valid",
        "stdio0",
        "stdio1",
        "stdio2",
        "duplicate",
        "artifact_duplicate_writer",
        "artifact_duplicate_observer",
        "closed",
        "wrong_env_writer",
        "wrong_env_observer",
        "readwrite",
        "directory",
        "writer_readwrite",
        "writer_directory",
        "artifact_readwrite",
        "artifact_directory",
    ] {
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("inherited_handoff_child_env")
            .arg("--nocapture")
            .env("O4B1_SYNTHETIC_CASE", case)
            .env("O4B1_SYNTHETIC_ROOT", &root)
            .output()
            .unwrap();
        if case == "valid" {
            assert!(
                child.status.success(),
                "valid synthetic handoff denied: {}",
                String::from_utf8_lossy(&child.stderr)
            );
            assert!(child.stdout.windows(17).any(|b| b == b"SYNTHETIC_SUCCESS"));
        } else {
            assert_eq!(child.status.code(), Some(1), "{case} must fail-stop");
            assert!(
                !child.stdout.windows(17).any(|b| b == b"SYNTHETIC_SUCCESS"),
                "{case} continued"
            );
        }
        assert!(child.stderr.is_empty(), "{case} printed raw child errors");
        assert!(
            !child.stdout.windows(18).any(|b| b == b"synthetic-artifact"),
            "{case} printed raw fixture bytes"
        );
    }
    let expected = std::env::current_dir()
        .unwrap()
        .canonicalize()
        .unwrap()
        .join(".superpowers/sdd/2026-10-03-controller-task4-isolated-observer")
        .join(format!("o4b1-handoff-fixture-{}", std::process::id()));
    assert_eq!(root.canonicalize().unwrap(), expected);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn inherited_handoff_child_env() {
    use std::{
        fs::{self, File, OpenOptions},
        os::fd::{AsRawFd, IntoRawFd},
        path::PathBuf,
    };
    let Ok(case) = std::env::var("O4B1_SYNTHETIC_CASE") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("O4B1_SYNTHETIC_ROOT").unwrap());
    let auth = synthetic_auth("mysql", WRITER);
    let writer = match case.as_str() {
        "writer_readwrite" => OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("writer"))
            .unwrap()
            .into_raw_fd(),
        "writer_directory" => File::open(&root).unwrap().into_raw_fd(),
        _ => File::open(root.join("writer")).unwrap().into_raw_fd(),
    };
    let observer = File::open(root.join("observer")).unwrap().into_raw_fd();
    let artifact = match case.as_str() {
        "artifact_readwrite" => OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("artifact"))
            .unwrap()
            .into_raw_fd(),
        "artifact_directory" => File::open(&root).unwrap().into_raw_fd(),
        _ => File::open(root.join("artifact")).unwrap().into_raw_fd(),
    };
    let changed = match case.as_str() {
        "readwrite" => OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("observer"))
            .unwrap()
            .into_raw_fd(),
        "directory" => File::open(&root).unwrap().into_raw_fd(),
        _ => observer,
    };
    // Closed number originates from a synthetic File; dropping it leaves no owner.
    let closed = if case == "closed" {
        let temporary = File::open(root.join("observer")).unwrap();
        let number = temporary.as_raw_fd();
        drop(temporary);
        number
    } else {
        changed
    };
    let raw = match case.as_str() {
        "stdio0" => [0, changed, artifact],
        "stdio1" => [writer, 1, artifact],
        "stdio2" => [writer, changed, 2],
        "duplicate" => [writer, writer, artifact],
        "artifact_duplicate_writer" => [writer, changed, writer],
        "artifact_duplicate_observer" => [writer, changed, changed],
        "closed" => [writer, changed, closed],
        _ => [writer, changed, artifact],
    };
    let wp = pin(WRITER);
    let op = pin(OBSERVER);
    let stdin_before = fs::metadata("/proc/self/fd/0").unwrap();
    let stdout_before = fs::metadata("/proc/self/fd/1").unwrap();
    let stderr_before = fs::metadata("/proc/self/fd/2").unwrap();
    let pins = match case.as_str() {
        "wrong_env_writer" => ("0".repeat(64), op.clone()),
        "wrong_env_observer" => (wp.clone(), "0".repeat(64)),
        _ => (wp.clone(), op.clone()),
    };
    // SAFETY: This isolated child creates each valid >=3 descriptor from a
    // synthetic File::into_raw_fd, leaving no File/OwnedFd owner. Duplicate
    // numbers still refer to that one owner and are rejected before adoption;
    // stdio 0–2 and the closed number are never transferred. No thread closes,
    // duplicates, or reuses these fds during validation/adoption. On success
    // the returned OwnedFd values are their only owners; never re-adopt `raw`.
    let fds = unsafe { observer::adopt_child_or_exit(raw, &auth, (&pins.0, &pins.1)) };
    // Artifact lifetime ends before the configuration loader runs.
    drop(fds.artifact);
    assert!(fs::metadata(format!("/proc/self/fd/{}", raw[2])).is_err());
    let prepared = observer::load_inherited(
        fds.writer,
        fds.observer,
        &pins.0,
        &pins.1,
        observer::Engine::Mysql,
    )
    .unwrap_or_else(|_| panic!("synthetic config rejected"));
    assert_eq!(prepared.options().get_database(), None);
    assert_eq!(prepared.schema(), "Exact_DB");
    for fd in raw {
        assert!(fs::metadata(format!("/proc/self/fd/{fd}")).is_err());
    }
    use std::os::unix::fs::MetadataExt;
    for (n, before) in [(0, stdin_before), (1, stdout_before), (2, stderr_before)] {
        let after = fs::metadata(format!("/proc/self/fd/{n}")).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
    }
    println!("SYNTHETIC_SUCCESS");
}

#[cfg(target_os = "linux")]
#[test]
fn inherited_owned_fd_contract() {
    use std::process::Command;
    // Raw integers 0/1/2, duplicate and closed descriptors belong to the O4/O6
    // controlled child handoff, never to a safe OwnedFd loader.
    for case in [
        "valid",
        "max_bytes",
        "writable",
        "write_only",
        "directory",
        "oversized",
        "writer_oversized",
        "wrong_pin",
        "writer_wrong_pin",
    ] {
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("inherited_owned_fd_child")
            .arg("--nocapture")
            .env("OBSERVER_FD_FIXTURE_CASE", case)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "synthetic fd case {case} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn inherited_owned_fd_child() {
    use std::{
        fs::{self, File, OpenOptions},
        os::fd::{AsRawFd, OwnedFd},
        os::unix::fs::PermissionsExt,
    };
    let Ok(case) = std::env::var("OBSERVER_FD_FIXTURE_CASE") else {
        return;
    };
    // Child-exclusive synthetic files: never consume the harness parent's fds.
    let root = std::env::current_dir()
        .unwrap()
        .join(".superpowers/sdd/2026-10-03-controller-task4-isolated-observer")
        .join(format!("o2b2-private-fixture-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let writer_path = root.join("writer.json");
    let observer_path = root.join("observer.json");
    fs::write(&writer_path, WRITER).unwrap();
    fs::write(&observer_path, OBSERVER).unwrap();
    fs::set_permissions(&writer_path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(&observer_path, fs::Permissions::from_mode(0o600)).unwrap();
    if matches!(case.as_str(), "oversized" | "max_bytes") {
        let size = if case == "max_bytes" { 65536 } else { 65537 };
        let mut content = OBSERVER.to_vec();
        content.resize(size, b' ');
        fs::write(&observer_path, content).unwrap();
    }
    if case == "writer_oversized" {
        let mut content = WRITER.to_vec();
        content.resize(65537, b' ');
        fs::write(&writer_path, content).unwrap();
    }
    let mut writer = File::open(&writer_path).unwrap();
    if case == "valid" {
        use std::io::Read;
        let mut prefix = [0; 3];
        writer.read_exact(&mut prefix).unwrap();
    }
    let writer_fd: OwnedFd = writer.into();
    let observer_fd: OwnedFd = match case.as_str() {
        "directory" => File::open(&root).unwrap().into(),
        "writable" => OpenOptions::new()
            .read(true)
            .write(true)
            .open(&observer_path)
            .unwrap()
            .into(),
        "write_only" => OpenOptions::new()
            .write(true)
            .open(&observer_path)
            .unwrap()
            .into(),
        _ => File::open(&observer_path).unwrap().into(),
    };
    let (w, o) = (writer_fd.as_raw_fd(), observer_fd.as_raw_fd());
    assert_ne!(w, o);
    let fd_open = |fd: i32| fs::metadata(format!("/proc/self/fd/{fd}")).is_ok();
    let observer_bytes = if case == "max_bytes" {
        fs::read(&observer_path).unwrap()
    } else {
        OBSERVER.to_vec()
    };
    let observer_pin = if case == "wrong_pin" {
        "0".repeat(64)
    } else {
        pin(&observer_bytes)
    };
    let writer_pin = if case == "writer_wrong_pin" {
        "0".repeat(64)
    } else {
        pin(WRITER)
    };
    let loaded = observer::load_inherited(
        writer_fd,
        observer_fd,
        &writer_pin,
        &observer_pin,
        observer::Engine::Mysql,
    );
    if matches!(case.as_str(), "valid" | "max_bytes") {
        let prepared = loaded.expect("valid transferred readonly regular fds must load");
        assert_eq!(prepared.schema(), "Exact_DB");
        assert_eq!(prepared.options().get_database(), None);
    } else {
        let expected = if matches!(case.as_str(), "wrong_pin" | "writer_wrong_pin") {
            "config_changed_since_authorization"
        } else {
            "config_invalid"
        };
        assert!(
            matches!(loaded, Err(error) if error == expected),
            "{case}: incorrect fixed error"
        );
    }
    assert!(!fd_open(w) && !fd_open(o), "{case}: owned fds not closed");
    // Only remove the PID-specific synthetic root inside the approved worktree.
    let expected_root = std::env::current_dir()
        .unwrap()
        .canonicalize()
        .unwrap()
        .join(".superpowers/sdd/2026-10-03-controller-task4-isolated-observer")
        .join(format!("o2b2-private-fixture-{}", std::process::id()));
    assert_eq!(root.canonicalize().unwrap(), expected_root);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn observer_closed_mysql_and_tidb_grammar() {
    use observer::{Account, Engine, validate_grants};
    let a = Account {
        user: "o_fixture".into(),
        host: "%".into(),
    };
    let mysql: Vec<Option<String>> = ["data_lock_waits", "data_locks", "threads"]
        .into_iter()
        .map(|t| {
            Some(format!(
                "GRANT SELECT ON `performance_schema`.`{t}` TO 'o_fixture'@'%'"
            ))
        })
        .collect();
    assert_eq!(validate_grants(Engine::Mysql, &mysql, &a), Ok(()));
    let tidb = vec![Some("GRANT PROCESS ON *.* TO 'o_fixture'@'%'".into())];
    assert_eq!(validate_grants(Engine::Tidb, &tidb, &a), Ok(()));
    assert_eq!(
        validate_grants(Engine::Mysql, &tidb, &a),
        Err("shape_rejected")
    );
    assert_eq!(
        validate_grants(Engine::Tidb, &mysql, &a),
        Err("shape_rejected")
    );
}

#[test]
fn observer_identity_and_roles_are_separate_gates() {
    use observer::{Account, Engine, validate_identity};
    let a = Account {
        user: "o_fixture".into(),
        host: "%".into(),
    };
    assert_eq!(
        validate_identity(
            Engine::Mysql,
            "o_fixture@%",
            "writer@%",
            &a,
            Some("NONE"),
            Some("")
        ),
        Ok(())
    );
    assert_eq!(
        validate_identity(
            Engine::Mysql,
            "o_fixture@%",
            "o_fixture@%",
            &a,
            Some("NONE"),
            Some("")
        ),
        Err("identity_mismatch")
    );
    assert_eq!(
        validate_identity(
            Engine::Mysql,
            "o_fixture@%",
            "writer@%",
            &a,
            Some("role_x"),
            Some("")
        ),
        Err("roles_unverified")
    );
}

fn sessions(engine: observer::Engine) -> [observer::SessionIdentity; 3] {
    let version = match engine {
        observer::Engine::Mysql => "8.0.46",
        observer::Engine::Tidb => "8.0.11-TiDB-v8.5.8",
    };
    let make = |id: u64, database: Option<String>, current_user: &str| observer::SessionIdentity {
        connection_id: id,
        version: version.into(),
        database,
        current_user: current_user.into(),
        server_uuid: Some("synthetic-uuid".into()),
        global_ids: Some(true),
        pessimistic: Some(true),
    };
    [
        make(11, Some("Exact_DB".into()), "writer_fixture@%"),
        make(22, Some("Exact_DB".into()), "writer_fixture@%"),
        make(33, None, "observer_fixture@%"),
    ]
}

fn check_sessions(
    engine: observer::Engine,
    s: &[observer::SessionIdentity; 3],
    topology: bool,
) -> Result<(), observer::ObserverError> {
    observer::validate_sessions(engine, "Exact_DB", &s[0], &s[1], &s[2], topology)
}

#[test]
fn session_schema_cannot_be_empty_even_if_both_writers_report_empty() {
    for engine in [observer::Engine::Mysql, observer::Engine::Tidb] {
        let mut s = sessions(engine);
        s[0].database = Some(String::new());
        s[1].database = Some(String::new());
        assert_eq!(
            observer::validate_sessions(engine, "", &s[0], &s[1], &s[2], true),
            Err("identity_mismatch")
        );
    }
}

#[test]
fn three_physical_sessions_match_each_engine_contract() {
    for engine in [observer::Engine::Mysql, observer::Engine::Tidb] {
        assert_eq!(check_sessions(engine, &sessions(engine), true), Ok(()));
    }
}

#[test]
fn tidb_edge_requires_unique_current_start_ts() {
    use observer::{ActorIds, TrxMapping, validate_edge};
    let ids = ActorIds {
        waiter: 22,
        holder: 11,
    };
    let maps = [
        TrxMapping {
            session: 11,
            start_ts: 101,
        },
        TrxMapping {
            session: 22,
            start_ts: 202,
        },
    ];
    assert_eq!(validate_edge(ids, &maps, Some((202, 101))), Ok(true));
    assert_eq!(validate_edge(ids, &maps, Some((202, 999))), Ok(false));
    assert_eq!(validate_edge(ids, &maps[..1], None), Ok(false));
    assert_eq!(validate_edge(ids, &[], None), Err("identity_mismatch"));
    let dup = [
        TrxMapping {
            session: 11,
            start_ts: 101,
        },
        TrxMapping {
            session: 11,
            start_ts: 303,
        },
    ];
    assert_eq!(validate_edge(ids, &dup, None), Err("identity_mismatch"));
}

#[test]
fn session_identity_mutations_reject_all_shared_fields_both_engines() {
    for engine in [observer::Engine::Mysql, observer::Engine::Tidb] {
        for index in 0..3 {
            let mut s = sessions(engine);
            s[index].version = "8.0.46 ".into();
            assert_eq!(check_sessions(engine, &s, true), Err("identity_mismatch"));
            let mut s = sessions(engine);
            s[index].connection_id = 0;
            assert_eq!(check_sessions(engine, &s, true), Err("identity_mismatch"));
            let mut s = sessions(engine);
            s[index].current_user.clear();
            assert_eq!(check_sessions(engine, &s, true), Err("identity_mismatch"));
        }
        for (index, other) in [(0, 1), (0, 2), (1, 2)] {
            let mut s = sessions(engine);
            s[other].connection_id = s[index].connection_id;
            assert_eq!(check_sessions(engine, &s, true), Err("identity_mismatch"));
        }
        for index in 0..2 {
            for database in [None, Some("exact_db".into()), Some(String::new())] {
                let mut s = sessions(engine);
                s[index].database = database;
                assert_eq!(check_sessions(engine, &s, true), Err("identity_mismatch"));
            }
        }
        for database in [Some("Exact_DB".into()), Some(String::new())] {
            let mut s = sessions(engine);
            s[2].database = database;
            assert_eq!(check_sessions(engine, &s, true), Err("identity_mismatch"));
        }
        let mut s = sessions(engine);
        s[1].current_user = "other_writer@%".into();
        assert_eq!(check_sessions(engine, &s, true), Err("identity_mismatch"));
        let mut s = sessions(engine);
        s[2].current_user = s[0].current_user.clone();
        assert_eq!(check_sessions(engine, &s, true), Err("identity_mismatch"));
        assert_eq!(
            observer::validate_sessions(engine, "", &s[0], &s[1], &s[2], true),
            Err("identity_mismatch")
        );
    }
}

#[test]
fn mysql_session_uuid_requires_three_nonempty_equal_actual_values() {
    use observer::Engine;
    for index in 0..3 {
        for bad_uuid in [
            None,
            Some(String::new()),
            Some("other-synthetic-uuid".into()),
        ] {
            let mut s = sessions(Engine::Mysql);
            s[index].server_uuid = bad_uuid;
            assert_eq!(
                check_sessions(Engine::Mysql, &s, true),
                Err("identity_mismatch")
            );
        }
    }
    let mut s = sessions(Engine::Mysql);
    for identity in &mut s {
        identity.server_uuid = Some("another-synthetic-uuid".into());
    }
    assert_eq!(check_sessions(Engine::Mysql, &s, false), Ok(()));
}

#[test]
fn tidb_session_requires_external_topology_and_actual_global_and_mode_flags() {
    use observer::Engine;
    assert_eq!(
        check_sessions(Engine::Tidb, &sessions(Engine::Tidb), false),
        Err("prerequisite_missing")
    );
    for index in 0..3 {
        for invalid in [None, Some(false)] {
            let mut s = sessions(Engine::Tidb);
            s[index].global_ids = invalid;
            assert_eq!(
                check_sessions(Engine::Tidb, &s, true),
                Err("identity_mismatch")
            );
        }
    }
    for index in 0..2 {
        for invalid in [None, Some(false)] {
            let mut s = sessions(Engine::Tidb);
            s[index].pessimistic = invalid;
            assert_eq!(
                check_sessions(Engine::Tidb, &s, true),
                Err("identity_mismatch")
            );
        }
    }
    // O is read only; a transaction mode on O is not required.
    let mut s = sessions(Engine::Tidb);
    s[2].pessimistic = None;
    assert_eq!(check_sessions(Engine::Tidb, &s, true), Ok(()));
}

#[test]
fn edge_rejects_invalid_actors_mapping_and_start_ts_without_leaking_values() {
    use observer::{ActorIds, TrxMapping, validate_edge};
    let ids = ActorIds {
        waiter: 22,
        holder: 11,
    };
    let valid = || {
        [
            TrxMapping {
                session: 11,
                start_ts: 101,
            },
            TrxMapping {
                session: 22,
                start_ts: 202,
            },
        ]
    };
    assert_eq!(validate_edge(ids, &valid(), None), Ok(false));
    assert_eq!(
        validate_edge(ids, &valid()[..1], Some((202, 101))),
        Ok(false)
    );
    for bad in [
        ActorIds {
            waiter: 0,
            holder: 11,
        },
        ActorIds {
            waiter: 22,
            holder: 0,
        },
        ActorIds {
            waiter: 11,
            holder: 11,
        },
    ] {
        assert_eq!(validate_edge(bad, &valid(), None), Err("identity_mismatch"));
    }
    for index in 0..2 {
        let mut maps = valid();
        maps[index].session = 0;
        assert_eq!(validate_edge(ids, &maps, None), Err("identity_mismatch"));
        let mut maps = valid();
        maps[index].start_ts = 0;
        assert_eq!(validate_edge(ids, &maps, None), Err("identity_mismatch"));
        let mut maps = vec![
            TrxMapping {
                session: 11,
                start_ts: 101,
            },
            TrxMapping {
                session: 22,
                start_ts: 202,
            },
        ];
        maps.push(TrxMapping {
            session: maps[index].session,
            start_ts: 303,
        });
        assert_eq!(validate_edge(ids, &maps, None), Err("identity_mismatch"));
    }
    for bad_edge in [(0, 101), (202, 0)] {
        assert_eq!(
            validate_edge(ids, &valid(), Some(bad_edge)),
            Err("identity_mismatch")
        );
    }
    assert_eq!(validate_edge(ids, &valid(), Some((101, 202))), Ok(false));
    assert_eq!(validate_edge(ids, &valid(), Some((999, 101))), Ok(false));
    assert_eq!(
        validate_edge(
            ids,
            &[TrxMapping {
                session: 22,
                start_ts: 202
            }],
            None
        ),
        Err("identity_mismatch")
    );
    let mut unrelated = vec![
        TrxMapping {
            session: 11,
            start_ts: 101,
        },
        TrxMapping {
            session: 22,
            start_ts: 202,
        },
    ];
    unrelated.extend([
        TrxMapping {
            session: 44,
            start_ts: 404,
        },
        TrxMapping {
            session: 44,
            start_ts: 405,
        },
    ]);
    assert_eq!(
        validate_edge(ids, &unrelated, None),
        Err("identity_mismatch")
    );
    assert_eq!(
        validate_edge(
            ids,
            &[
                TrxMapping {
                    session: 11,
                    start_ts: 101
                },
                TrxMapping {
                    session: 22,
                    start_ts: 101
                }
            ],
            Some((101, 101))
        ),
        Err("identity_mismatch")
    );
}

#[tokio::test]
async fn observer_total_deadline_bounds_pending_query() {
    use std::{future::pending, time::Duration};
    use tokio::time::{Instant, timeout};
    let result = timeout(
        Duration::from_millis(250),
        observer::bounded(
            pending::<Result<(), &'static str>>(),
            Instant::now() + Duration::from_millis(5),
            Duration::from_secs(3),
        ),
    )
    .await;
    assert!(
        matches!(result, Ok(Err("timeout"))),
        "total deadline must cancel pending query"
    );
}

#[tokio::test]
async fn observer_stage_and_expired_deadlines_cancel_pending_without_retry() {
    use std::{
        future::pending,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio::time::{Instant, timeout};
    struct DropMarker(Arc<AtomicUsize>);
    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    for (total, stage) in [
        (
            Instant::now() + Duration::from_secs(3),
            Duration::from_millis(5),
        ),
        (
            Instant::now() - Duration::from_millis(1),
            Duration::from_secs(3),
        ),
        (Instant::now() + Duration::from_secs(3), Duration::ZERO),
    ] {
        let drops = Arc::new(AtomicUsize::new(0));
        let marker = DropMarker(Arc::clone(&drops));
        let result = timeout(
            Duration::from_millis(250),
            observer::bounded(
                async move {
                    let _marker = marker;
                    pending::<Result<(), &'static str>>().await
                },
                total,
                stage,
            ),
        )
        .await;
        assert!(matches!(result, Ok(Err("timeout"))));
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "pending future must be dropped exactly once"
        );
    }
}

#[tokio::test]
async fn observer_bounded_preserves_success_and_fixed_error() {
    use std::time::Duration;
    use tokio::time::Instant;
    let total = Instant::now() + Duration::from_secs(1);
    assert_eq!(
        observer::bounded(
            async { Ok::<_, &'static str>(7) },
            total,
            Duration::from_millis(30)
        )
        .await,
        Ok(7)
    );
    assert_eq!(
        observer::bounded(
            async { Err::<(), _>("decode_error") },
            total,
            Duration::from_millis(30)
        )
        .await,
        Err("decode_error")
    );
}

#[tokio::test]
async fn observer_expired_total_does_not_poll_ready_future() {
    use std::{cell::Cell, time::Duration};
    use tokio::time::Instant;
    let polled = Cell::new(false);
    let result = observer::bounded(
        async {
            polled.set(true);
            Ok::<_, &'static str>(())
        },
        Instant::now() - Duration::from_millis(1),
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(result, Err("timeout"));
    assert!(!polled.get());
}

#[test]
fn identity_and_tidb_mapping_use_safe_exact_result_aliases() {
    use observer::{ActorIds, Engine, ReadStep, query_plan};
    let ids = ActorIds {
        waiter: 22,
        holder: 11,
    };
    for engine in [Engine::Mysql, Engine::Tidb] {
        let sql = query_plan(engine, ReadStep::Identity, "Exact_DB", ids)
            .expect("identity catalog entry")
            .sql();
        assert!(sql.contains("CAST(DATABASE() AS CHAR) AS `observer_database`"));
        assert!(sql.contains("CAST(CURRENT_USER() AS CHAR) AS `observer_current_user`"));
        for unquoted in [" AS database", " AS current_user", " AS session"] {
            assert!(
                !sql.contains(unquoted),
                "identity has an unquoted risky alias"
            );
        }
    }
    let mapping = query_plan(Engine::Tidb, ReadStep::TidbMapping, "Exact_DB", ids)
        .expect("mapping catalog entry")
        .sql();
    assert!(mapping.contains("CAST(SESSION_ID AS UNSIGNED) AS `observer_session`"));
    for unquoted in [" AS database", " AS current_user", " AS session"] {
        assert!(
            !mapping.contains(unquoted),
            "mapping has an unquoted risky alias"
        );
    }
}

#[test]
fn fixed_sql_catalog_has_exact_engine_steps_text_and_bind_order() {
    use observer::{ActorIds, Bind, Engine, ReadStep, query_plan};
    let ids = ActorIds {
        waiter: 22,
        holder: 11,
    };
    let mysql = [
        (
            ReadStep::Identity,
            "SELECT CAST(CONNECTION_ID() AS UNSIGNED) AS connection_id,CAST(VERSION() AS CHAR) AS version,CAST(DATABASE() AS CHAR) AS `observer_database`,CAST(CURRENT_USER() AS CHAR) AS `observer_current_user`,CAST(@@GLOBAL.server_uuid AS CHAR) AS server_uuid",
            vec![],
        ),
        (ReadStep::Grants, "SHOW GRANTS", vec![]),
        (
            ReadStep::Roles,
            "SELECT CAST(CURRENT_ROLE() AS CHAR) AS current_role,CAST(@@GLOBAL.mandatory_roles AS CHAR) AS mandatory_roles",
            vec![],
        ),
        (
            ReadStep::MysqlWaits,
            "SELECT CAST(1 AS SIGNED) AS readable FROM performance_schema.data_lock_waits LIMIT 1",
            vec![],
        ),
        (
            ReadStep::MysqlLocks,
            "SELECT CAST(1 AS SIGNED) AS readable FROM performance_schema.data_locks LIMIT 1",
            vec![],
        ),
        (
            ReadStep::MysqlThreads,
            "SELECT CAST(1 AS SIGNED) AS readable FROM performance_schema.threads LIMIT 1",
            vec![],
        ),
        (
            ReadStep::MysqlWaiterThread,
            "SELECT CAST(COUNT(*) AS SIGNED) AS matches FROM performance_schema.threads WHERE PROCESSLIST_ID=CAST(? AS UNSIGNED)",
            vec![Bind::Unsigned(22)],
        ),
        (
            ReadStep::MysqlHolderThread,
            "SELECT CAST(COUNT(*) AS SIGNED) AS matches FROM performance_schema.threads WHERE PROCESSLIST_ID=CAST(? AS UNSIGNED)",
            vec![Bind::Unsigned(11)],
        ),
        (
            ReadStep::Edge,
            "SELECT CAST(COUNT(*) AS SIGNED) AS matches FROM performance_schema.data_lock_waits AS w JOIN performance_schema.threads AS wt ON wt.THREAD_ID=w.REQUESTING_THREAD_ID JOIN performance_schema.threads AS ht ON ht.THREAD_ID=w.BLOCKING_THREAD_ID JOIN performance_schema.data_locks AS dl ON dl.ENGINE=w.ENGINE AND dl.ENGINE_LOCK_ID=w.REQUESTING_ENGINE_LOCK_ID WHERE w.ENGINE='INNODB' AND wt.PROCESSLIST_ID=CAST(? AS UNSIGNED) AND ht.PROCESSLIST_ID=CAST(? AS UNSIGNED) AND dl.LOCK_STATUS='WAITING' AND CAST(dl.OBJECT_SCHEMA AS BINARY)=CAST(? AS BINARY) AND CAST(dl.OBJECT_NAME AS BINARY)=CAST('schema_meta' AS BINARY)",
            vec![
                Bind::Unsigned(22),
                Bind::Unsigned(11),
                Bind::Text("Exact_DB".into()),
            ],
        ),
    ];
    let tidb = [
        (
            ReadStep::Identity,
            "SELECT CAST(CONNECTION_ID() AS UNSIGNED) AS connection_id,CAST(VERSION() AS CHAR) AS version,CAST(DATABASE() AS CHAR) AS `observer_database`,CAST(CURRENT_USER() AS CHAR) AS `observer_current_user`,JSON_UNQUOTE(JSON_EXTRACT(@@GLOBAL.tidb_config,'$.\"enable-global-kill\"')) AS global_ids,CAST(@@SESSION.tidb_txn_mode AS CHAR) AS txn_mode",
            vec![],
        ),
        (ReadStep::Grants, "SHOW GRANTS", vec![]),
        (
            ReadStep::Roles,
            "SELECT CAST(CURRENT_ROLE() AS CHAR) AS current_role",
            vec![],
        ),
        (
            ReadStep::TidbWaits,
            "SELECT CAST(1 AS SIGNED) AS readable FROM information_schema.DATA_LOCK_WAITS LIMIT 1",
            vec![],
        ),
        (
            ReadStep::TidbTrx,
            "SELECT CAST(1 AS SIGNED) AS readable FROM information_schema.CLUSTER_TIDB_TRX LIMIT 1",
            vec![],
        ),
        (
            ReadStep::TidbMapping,
            "SELECT CAST(SESSION_ID AS UNSIGNED) AS `observer_session`,CAST(ID AS UNSIGNED) AS start_ts FROM information_schema.CLUSTER_TIDB_TRX WHERE SESSION_ID IN (CAST(? AS UNSIGNED),CAST(? AS UNSIGNED)) LIMIT 3",
            vec![Bind::Unsigned(22), Bind::Unsigned(11)],
        ),
        (
            ReadStep::Edge,
            "SELECT CAST(w.TRX_ID AS UNSIGNED) AS waiter_start_ts,CAST(w.CURRENT_HOLDING_TRX_ID AS UNSIGNED) AS holder_start_ts FROM information_schema.DATA_LOCK_WAITS AS w JOIN information_schema.CLUSTER_TIDB_TRX AS wt ON w.TRX_ID=wt.ID JOIN information_schema.CLUSTER_TIDB_TRX AS ht ON w.CURRENT_HOLDING_TRX_ID=ht.ID WHERE wt.SESSION_ID=CAST(? AS UNSIGNED) AND ht.SESSION_ID=CAST(? AS UNSIGNED) AND CAST(JSON_UNQUOTE(JSON_EXTRACT(CASE WHEN JSON_VALID(w.KEY_INFO) THEN w.KEY_INFO ELSE NULL END,'$.db_name')) AS BINARY)=CAST(? AS BINARY) AND CAST(JSON_UNQUOTE(JSON_EXTRACT(CASE WHEN JSON_VALID(w.KEY_INFO) THEN w.KEY_INFO ELSE NULL END,'$.table_name')) AS BINARY)=CAST('schema_meta' AS BINARY) LIMIT 2",
            vec![
                Bind::Unsigned(22),
                Bind::Unsigned(11),
                Bind::Text("Exact_DB".into()),
            ],
        ),
    ];
    for (engine, cases) in [
        (Engine::Mysql, mysql.as_slice()),
        (Engine::Tidb, tidb.as_slice()),
    ] {
        for (step, sql, expected_binds) in cases {
            let plan = query_plan(engine, *step, "Exact_DB", ids).expect("valid catalog entry");
            assert_eq!(plan.sql(), *sql);
            assert!(
                plan.binds() == *expected_binds,
                "incorrect typed bind order"
            );
            assert_eq!(plan.sql().matches('?').count(), plan.binds().len());
            let hostile_schema = "synthetic' OR 1=1 --";
            let altered = query_plan(engine, *step, hostile_schema, ids).unwrap();
            assert_eq!(
                altered.sql(),
                plan.sql(),
                "schema may not interpolate into SQL"
            );
            if *step == ReadStep::Edge {
                assert!(
                    altered.binds()
                        == vec![
                            Bind::Unsigned(22),
                            Bind::Unsigned(11),
                            Bind::Text(hostile_schema.into())
                        ]
                );
            }

            assert!(plan.sql().starts_with("SELECT ") || plan.sql() == "SHOW GRANTS");
            assert!(!plan.sql().contains("LIMIT 0") && !plan.sql().contains("WHERE FALSE"));
            let head = plan
                .sql()
                .split_once(" FROM ")
                .map_or(plan.sql(), |(head, _)| head);
            for sensitive in [
                "KEY_INFO",
                "LOCK_DATA",
                "PROCESSLIST_INFO",
                "SQL_DIGEST",
                "SQL_TEXT",
                "ALL_SQL_DIGESTS",
            ] {
                assert!(!head.contains(sensitive), "sensitive column projected");
            }
            for forbidden in [" USE ", " SET ", " BEGIN ", " FOR UPDATE", " DATABASE()="] {
                assert!(!plan.sql().contains(forbidden));
            }
            if *step != ReadStep::Identity {
                assert!(!plan.sql().contains("DATABASE()"), "O schema must be bound");
            }
        }
    }
}

#[test]
fn plan_validation_accepts_catalog_only_for_its_engine_and_step() {
    use observer::{ActorIds, Engine, ReadStep, query_plan};
    let ids = ActorIds {
        waiter: 22,
        holder: 11,
    };
    for (engine, steps) in [
        (
            Engine::Mysql,
            &[
                ReadStep::Identity,
                ReadStep::Grants,
                ReadStep::Roles,
                ReadStep::MysqlWaits,
                ReadStep::MysqlLocks,
                ReadStep::MysqlThreads,
                ReadStep::MysqlWaiterThread,
                ReadStep::MysqlHolderThread,
                ReadStep::Edge,
            ][..],
        ),
        (
            Engine::Tidb,
            &[
                ReadStep::Identity,
                ReadStep::Grants,
                ReadStep::Roles,
                ReadStep::TidbWaits,
                ReadStep::TidbTrx,
                ReadStep::TidbMapping,
                ReadStep::Edge,
            ][..],
        ),
    ] {
        let other_engine = if engine == Engine::Mysql {
            Engine::Tidb
        } else {
            Engine::Mysql
        };
        for &step in steps {
            let plan = query_plan(engine, step, "Exact_DB", ids).unwrap();
            assert!(plan.validate(engine, step), "canonical entry rejected");
            assert!(
                !plan.validate(other_engine, step),
                "engine mismatch accepted"
            );
            let other_step = if step == ReadStep::Grants {
                ReadStep::Identity
            } else {
                ReadStep::Grants
            };
            assert!(!plan.validate(engine, other_step), "step mismatch accepted");
        }
    }
    let waiter = query_plan(Engine::Mysql, ReadStep::MysqlWaiterThread, "Exact_DB", ids).unwrap();
    assert!(!waiter.validate(Engine::Mysql, ReadStep::MysqlHolderThread));
    let grant = query_plan(Engine::Mysql, ReadStep::Grants, "Exact_DB", ids).unwrap();
    assert!(!grant.validate(Engine::Tidb, ReadStep::Grants));
}

#[test]
fn fixed_sql_catalog_rejects_cross_engine_steps_and_missing_schema_or_actor_ids() {
    use observer::{ActorIds, Engine, ReadStep, query_plan};
    let ids = ActorIds {
        waiter: 22,
        holder: 11,
    };
    for step in [
        ReadStep::MysqlWaits,
        ReadStep::MysqlLocks,
        ReadStep::MysqlThreads,
        ReadStep::MysqlWaiterThread,
        ReadStep::MysqlHolderThread,
    ] {
        assert!(query_plan(Engine::Tidb, step, "Exact_DB", ids).is_err());
    }
    for step in [
        ReadStep::TidbWaits,
        ReadStep::TidbTrx,
        ReadStep::TidbMapping,
    ] {
        assert!(query_plan(Engine::Mysql, step, "Exact_DB", ids).is_err());
    }
    for engine in [Engine::Mysql, Engine::Tidb] {
        for step in [
            ReadStep::Identity,
            ReadStep::Grants,
            ReadStep::Roles,
            ReadStep::Edge,
        ] {
            assert!(query_plan(engine, step, "", ids).is_err());
        }
        for invalid in [
            ActorIds {
                waiter: 0,
                holder: 11,
            },
            ActorIds {
                waiter: 22,
                holder: 0,
            },
            ActorIds {
                waiter: 11,
                holder: 11,
            },
        ] {
            assert!(query_plan(engine, ReadStep::Edge, "Exact_DB", invalid).is_err());
        }
    }
    for (step, invalid) in [
        (
            ReadStep::MysqlWaiterThread,
            ActorIds {
                waiter: 0,
                holder: 11,
            },
        ),
        (
            ReadStep::MysqlHolderThread,
            ActorIds {
                waiter: 22,
                holder: 0,
            },
        ),
        (
            ReadStep::TidbMapping,
            ActorIds {
                waiter: 0,
                holder: 11,
            },
        ),
    ] {
        let engine = if step == ReadStep::TidbMapping {
            Engine::Tidb
        } else {
            Engine::Mysql
        };
        assert!(query_plan(engine, step, "Exact_DB", invalid).is_err());
    }
}

#[cfg(test)]
mod typed_flow_contract {
    use super::observer::{
        self, Account, ActorIds, Engine, QueryPlan, ReadExecutor, ReadFailure, ReadResult, ReadStep,
    };
    use std::{collections::VecDeque, time::Duration};
    use tokio::time::Instant;

    struct Fake {
        engine: Engine,
        remaining: VecDeque<(ReadStep, Result<ReadResult, ReadFailure>)>,
        seen: Vec<ReadStep>,
    }
    impl Fake {
        fn new(engine: Engine, rows: Vec<(ReadStep, ReadResult)>) -> Self {
            Self {
                engine,
                remaining: rows.into_iter().map(|(s, r)| (s, Ok(r))).collect(),
                seen: vec![],
            }
        }
        fn done(&self) {
            assert!(self.remaining.is_empty(), "unconsumed typed steps");
        }
    }
    impl ReadExecutor for Fake {
        async fn read(
            &mut self,
            step: ReadStep,
            plan: QueryPlan,
        ) -> Result<ReadResult, ReadFailure> {
            let expected = observer::query_plan(
                self.engine,
                step,
                "Exact_DB",
                ActorIds {
                    waiter: 22,
                    holder: 11,
                },
            )
            .unwrap();
            assert!(plan.validate(self.engine, step));
            assert_eq!(plan.sql(), expected.sql());
            assert!(plan.binds() == expected.binds());
            self.seen.push(step);
            let (want, value) = self.remaining.pop_front().expect("unexpected step");
            assert!(want == step, "typed step out of order");
            value
        }
    }
    fn account() -> Account {
        Account {
            user: "o_fixture".into(),
            host: "%".into(),
        }
    }
    fn ids() -> ActorIds {
        ActorIds {
            waiter: 22,
            holder: 11,
        }
    }
    async fn preflight_class<E: ReadExecutor>(
        executor: &mut E,
        engine: Engine,
        schema: &str,
        expected: &Account,
        writer: &str,
        topology: bool,
        total: Instant,
    ) -> Result<observer::SessionIdentity, observer::ObserverError> {
        observer::preflight(executor, engine, schema, expected, writer, topology, total)
            .await
            .map_err(|failure| failure.error_class)
    }
    async fn sample_class<E: ReadExecutor>(
        executor: &mut E,
        engine: Engine,
        schema: &str,
        ids: ActorIds,
        total: Instant,
    ) -> Result<bool, observer::ObserverError> {
        observer::sample_edge(executor, engine, schema, ids, total)
            .await
            .map_err(|failure| failure.error_class)
    }
    fn total() -> Instant {
        Instant::now() + Duration::from_secs(1)
    }
    fn good(engine: Engine) -> Vec<(ReadStep, ReadResult)> {
        use ReadStep as S;
        let identity = ReadResult::Identity {
            connection_id: Some(33),
            version: Some(
                match engine {
                    Engine::Mysql => "8.0.46",
                    Engine::Tidb => "8.0.11-TiDB-v8.5.8",
                }
                .into(),
            ),
            database: None,
            current_user: Some("o_fixture@%".into()),
            server_uuid: (engine == Engine::Mysql).then(|| "synthetic-uuid".into()),
            global_ids: (engine == Engine::Tidb).then(|| "true".into()),
            txn_mode: (engine == Engine::Tidb).then(|| "pessimistic".into()),
        };
        let grants = match engine {
            Engine::Mysql => ["data_lock_waits", "data_locks", "threads"]
                .into_iter()
                .map(|table| {
                    Some(format!(
                        "GRANT SELECT ON `performance_schema`.`{table}` TO 'o_fixture'@'%'"
                    ))
                })
                .collect(),
            Engine::Tidb => vec![Some("GRANT PROCESS ON *.* TO 'o_fixture'@'%'".into())],
        };
        let mut rows = vec![
            (S::Identity, identity),
            (S::Grants, ReadResult::Grants(grants)),
            (
                S::Roles,
                ReadResult::Roles {
                    current_role: Some("NONE".into()),
                    mandatory_roles: (engine == Engine::Mysql).then(String::new),
                },
            ),
        ];
        let scans: &[ReadStep] = match engine {
            Engine::Mysql => &[S::MysqlWaits, S::MysqlLocks, S::MysqlThreads],
            Engine::Tidb => &[S::TidbWaits, S::TidbTrx],
        };
        rows.extend(scans.iter().map(|&s| (s, ReadResult::Scan(vec![]))));
        rows
    }
    #[tokio::test]
    async fn typed_preflight_executes_both_full_capability_sequences() {
        for engine in [Engine::Mysql, Engine::Tidb] {
            let mut fake = Fake::new(engine, good(engine));
            let observed = preflight_class(
                &mut fake,
                engine,
                "Exact_DB",
                &account(),
                "writer@%",
                true,
                total(),
            )
            .await
            .unwrap();
            assert_eq!(observed.connection_id, 33);
            assert_eq!(observed.database, None);
            fake.done();
            assert_eq!(fake.seen.len(), if engine == Engine::Mysql { 6 } else { 5 });
        }
    }
    #[tokio::test]
    async fn typed_sample_uses_actor_counts_and_all_mapping_rows() {
        use ReadStep as S;
        let mut mysql = Fake::new(
            Engine::Mysql,
            vec![
                (S::MysqlWaiterThread, ReadResult::Count(Some(1))),
                (S::MysqlHolderThread, ReadResult::Count(Some(1))),
                (S::Edge, ReadResult::Count(Some(2))),
            ],
        );
        assert_eq!(
            sample_class(&mut mysql, Engine::Mysql, "Exact_DB", ids(), total()).await,
            Ok(true)
        );
        mysql.done();
        let mut tidb = Fake::new(
            Engine::Tidb,
            vec![
                (
                    S::TidbMapping,
                    ReadResult::TrxMapping(vec![(Some(11), Some(101)), (Some(22), Some(202))]),
                ),
                (S::Edge, ReadResult::Edges(vec![(Some(202), Some(101))])),
            ],
        );
        assert_eq!(
            sample_class(&mut tidb, Engine::Tidb, "Exact_DB", ids(), total()).await,
            Ok(true)
        );
        tidb.done();
    }
    #[tokio::test]
    async fn preflight_rejects_early_identity_grants_roles_and_scan_failures() {
        use ReadStep as S;
        for engine in [Engine::Mysql, Engine::Tidb] {
            let mut rows = good(engine);
            if let ReadResult::Identity { current_user, .. } = &mut rows[0].1 {
                *current_user = None;
            }
            let mut fake = Fake::new(engine, rows);
            assert!(matches!(
                preflight_class(
                    &mut fake,
                    engine,
                    "Exact_DB",
                    &account(),
                    "writer@%",
                    true,
                    total()
                )
                .await,
                Err("decode_error")
            ));
            assert_eq!(fake.seen.len(), 1);
            let mut rows = good(engine);
            if let ReadResult::Grants(grants) = &mut rows[1].1 {
                grants[0] = None;
            }
            let mut fake = Fake::new(engine, rows);
            assert!(matches!(
                preflight_class(
                    &mut fake,
                    engine,
                    "Exact_DB",
                    &account(),
                    "writer@%",
                    true,
                    total()
                )
                .await,
                Err("decode_error")
            ));
            assert_eq!(fake.seen.len(), 2);
            let mut rows = good(engine);
            if let ReadResult::Roles { current_role, .. } = &mut rows[2].1 {
                *current_role = Some("active_role".into());
            }
            let mut fake = Fake::new(engine, rows);
            assert!(matches!(
                preflight_class(
                    &mut fake,
                    engine,
                    "Exact_DB",
                    &account(),
                    "writer@%",
                    true,
                    total()
                )
                .await,
                Err("roles_unverified")
            ));
            assert_eq!(fake.seen.len(), 3);
            for scan in 3..good(engine).len() {
                for invalid in [
                    ReadResult::Scan(vec![None]),
                    ReadResult::Scan(vec![Some(0)]),
                    ReadResult::Count(Some(0)),
                ] {
                    let mut rows = good(engine);
                    rows[scan].1 = invalid;
                    let mut fake = Fake::new(engine, rows);
                    assert!(matches!(
                        preflight_class(
                            &mut fake,
                            engine,
                            "Exact_DB",
                            &account(),
                            "writer@%",
                            true,
                            total()
                        )
                        .await,
                        Err("decode_error")
                    ));
                    assert_eq!(fake.seen.len(), scan + 1);
                }
            }
            let mut rows = good(engine);
            rows[3].1 = ReadResult::Scan(vec![Some(1)]);
            let mut fake = Fake::new(engine, rows);
            assert!(
                preflight_class(
                    &mut fake,
                    engine,
                    "Exact_DB",
                    &account(),
                    "writer@%",
                    true,
                    total()
                )
                .await
                .is_ok()
            );
            fake.done();
            let mut fake = Fake::new(engine, good(engine));
            assert!(matches!(
                preflight_class(&mut fake, engine, "Exact_DB", &account(), "", true, total()).await,
                Err("identity_mismatch")
            ));
            assert!(fake.seen.is_empty());
            let mut fake = Fake::new(engine, good(engine));
            if engine == Engine::Tidb {
                assert!(matches!(
                    preflight_class(
                        &mut fake,
                        engine,
                        "Exact_DB",
                        &account(),
                        "writer@%",
                        false,
                        total()
                    )
                    .await,
                    Err("prerequisite_missing")
                ));
                assert!(fake.seen.is_empty());
            }
            for (index, failure, expected) in [
                (0, ReadFailure::Query, "query_error"),
                (1, ReadFailure::Query, "grants_query_error"),
                (2, ReadFailure::Decode, "decode_error"),
                (3, ReadFailure::Query, "query_error"),
            ] {
                let mut fake = Fake::new(engine, good(engine));
                fake.remaining[index].1 = Err(failure);
                assert!(
                    matches!(preflight_class(&mut fake,engine,"Exact_DB",&account(),"writer@%",true,total()).await,Err(error) if error==expected)
                );
                assert_eq!(fake.seen.len(), index + 1);
            }
            let _ = S::Grants;
        }
    }
    #[tokio::test]
    async fn tidb_mapping_all_rows_and_edge_cardinality_fail_closed() {
        use ReadStep as S;
        let maps = || vec![(Some(11), Some(101)), (Some(22), Some(202))];
        let run = |m: Vec<(Option<u64>, Option<u64>)>, e: Vec<(Option<u64>, Option<u64>)>| {
            Fake::new(
                Engine::Tidb,
                vec![
                    (S::TidbMapping, ReadResult::TrxMapping(m)),
                    (S::Edge, ReadResult::Edges(e)),
                ],
            )
        };
        for mapping in [
            vec![],
            vec![(Some(22), Some(202))],
            vec![(Some(11), Some(101)), (Some(11), Some(303))],
            vec![
                (Some(11), Some(101)),
                (Some(22), Some(202)),
                (Some(11), Some(303)),
            ],
            vec![
                (Some(11), Some(101)),
                (Some(22), Some(202)),
                (Some(44), Some(404)),
            ],
            vec![(Some(11), Some(101)), (None, Some(202))],
            vec![(Some(11), Some(101)), (Some(22), Some(0))],
        ] {
            let mut fake = run(mapping, vec![(Some(202), Some(101))]);
            assert!(
                sample_class(&mut fake, Engine::Tidb, "Exact_DB", ids(), total())
                    .await
                    .is_err()
            );
            assert_eq!(fake.seen.len(), 1);
        }
        for edges in [
            vec![(Some(202), Some(101)), (Some(202), Some(101))],
            vec![(Some(202), Some(101)), (Some(203), Some(101))],
            vec![(Some(202), Some(101)); 3],
            vec![(None, Some(101))],
            vec![(Some(202), Some(0))],
        ] {
            let mut fake = run(maps(), edges);
            assert!(
                sample_class(&mut fake, Engine::Tidb, "Exact_DB", ids(), total())
                    .await
                    .is_err()
            );
            fake.done();
        }
        for (edges, result) in [(vec![], false), (vec![(Some(999), Some(101))], false)] {
            let mut fake = run(maps(), edges);
            assert_eq!(
                sample_class(&mut fake, Engine::Tidb, "Exact_DB", ids(), total()).await,
                Ok(result)
            );
            fake.done();
        }
        let mut fake = run(vec![(Some(11), Some(101))], vec![]);
        assert_eq!(
            sample_class(&mut fake, Engine::Tidb, "Exact_DB", ids(), total()).await,
            Ok(false)
        );
        fake.done();
    }
    #[tokio::test]
    async fn tidb_unsigned_start_ts_above_signed_limit_reaches_true_edge() {
        // These are synthetic unsigned decoded values, not a signed intermediate.
        let holder_start_ts = i64::MAX as u64 + 1;
        let waiter_start_ts = holder_start_ts + 1;
        let mut fake = Fake::new(
            Engine::Tidb,
            vec![
                (
                    ReadStep::TidbMapping,
                    ReadResult::TrxMapping(vec![
                        (Some(ids().holder), Some(holder_start_ts)),
                        (Some(ids().waiter), Some(waiter_start_ts)),
                    ]),
                ),
                (
                    ReadStep::Edge,
                    ReadResult::Edges(vec![(Some(waiter_start_ts), Some(holder_start_ts))]),
                ),
            ],
        );
        assert_eq!(
            sample_class(&mut fake, Engine::Tidb, "Exact_DB", ids(), total()).await,
            Ok(true)
        );
        fake.done();
    }
    #[tokio::test]
    async fn tidb_mapping_and_edge_reject_null_zero_duplicate_and_unrelated_actor() {
        use observer::{FlowFailure, FlowStage};
        let mapping_cases = [
            (
                vec![(None, Some(101)), (Some(22), Some(202))],
                "decode_error",
            ),
            (
                vec![(Some(11), Some(0)), (Some(22), Some(202))],
                "decode_error",
            ),
            (
                vec![(Some(11), Some(101)), (Some(11), Some(202))],
                "identity_mismatch",
            ),
            (
                vec![(Some(11), Some(101)), (Some(44), Some(202))],
                "decode_error",
            ),
        ];
        for (mapping, expected) in mapping_cases {
            let mut fake = Fake::new(
                Engine::Tidb,
                vec![(ReadStep::TidbMapping, ReadResult::TrxMapping(mapping))],
            );
            assert!(matches!(
                observer::sample_edge(&mut fake, Engine::Tidb, "Exact_DB", ids(), total()).await,
                Err(FlowFailure { stage: FlowStage::Step(ReadStep::TidbMapping), error_class })
                    if error_class == expected
            ));
            fake.done();
        }
        for edge in [(None, Some(101)), (Some(202), Some(0))] {
            let mut fake = Fake::new(
                Engine::Tidb,
                vec![
                    (
                        ReadStep::TidbMapping,
                        ReadResult::TrxMapping(vec![(Some(11), Some(101)), (Some(22), Some(202))]),
                    ),
                    (ReadStep::Edge, ReadResult::Edges(vec![edge])),
                ],
            );
            assert!(matches!(
                observer::sample_edge(&mut fake, Engine::Tidb, "Exact_DB", ids(), total()).await,
                Err(FlowFailure {
                    stage: FlowStage::Step(ReadStep::Edge),
                    error_class: "decode_error"
                })
            ));
            fake.done();
        }
    }
    #[tokio::test]
    async fn tidb_edge_null_decode_takes_priority_over_ambiguous_row_count() {
        use observer::{FlowFailure, FlowStage};
        let maps = || ReadResult::TrxMapping(vec![(Some(11), Some(101)), (Some(22), Some(202))]);
        let mut malformed = Fake::new(
            Engine::Tidb,
            vec![
                (ReadStep::TidbMapping, maps()),
                (
                    ReadStep::Edge,
                    ReadResult::Edges(vec![(None, Some(101)), (Some(202), Some(101))]),
                ),
            ],
        );
        assert!(matches!(
            observer::sample_edge(&mut malformed, Engine::Tidb, "Exact_DB", ids(), total()).await,
            Err(FlowFailure {
                stage: FlowStage::Step(ReadStep::Edge),
                error_class: "decode_error"
            })
        ));
        malformed.done();

        let mut ambiguous = Fake::new(
            Engine::Tidb,
            vec![
                (ReadStep::TidbMapping, maps()),
                (
                    ReadStep::Edge,
                    ReadResult::Edges(vec![(Some(202), Some(101)); 2]),
                ),
            ],
        );
        assert!(matches!(
            observer::sample_edge(&mut ambiguous, Engine::Tidb, "Exact_DB", ids(), total()).await,
            Err(FlowFailure {
                stage: FlowStage::Step(ReadStep::Edge),
                error_class: "identity_mismatch"
            })
        ));
        ambiguous.done();
    }
    #[tokio::test]
    async fn mysql_thread_counts_must_be_exactly_one_and_edge_nonnegative() {
        use ReadStep as S;
        for bad in [None, Some(-1), Some(0), Some(2)] {
            let mut fake = Fake::new(
                Engine::Mysql,
                vec![
                    (S::MysqlWaiterThread, ReadResult::Count(bad)),
                    (S::MysqlHolderThread, ReadResult::Count(Some(1))),
                ],
            );
            assert!(
                sample_class(&mut fake, Engine::Mysql, "Exact_DB", ids(), total())
                    .await
                    .is_err()
            );
            assert_eq!(fake.seen.len(), 1);
        }
        for value in [None, Some(-1)] {
            let mut fake = Fake::new(
                Engine::Mysql,
                vec![
                    (S::MysqlWaiterThread, ReadResult::Count(Some(1))),
                    (S::MysqlHolderThread, ReadResult::Count(Some(1))),
                    (S::Edge, ReadResult::Count(value)),
                ],
            );
            assert!(matches!(
                sample_class(&mut fake, Engine::Mysql, "Exact_DB", ids(), total()).await,
                Err("decode_error")
            ));
        }
        let mut fake = Fake::new(
            Engine::Mysql,
            vec![
                (S::MysqlWaiterThread, ReadResult::Count(Some(1))),
                (S::MysqlHolderThread, ReadResult::Count(Some(1))),
                (S::Edge, ReadResult::Count(Some(0))),
            ],
        );
        assert_eq!(
            sample_class(&mut fake, Engine::Mysql, "Exact_DB", ids(), total()).await,
            Ok(false)
        );
        fake.done();
    }
    #[tokio::test]
    async fn expired_shared_deadline_never_polls_even_ready_executor() {
        let mut fake = Fake::new(Engine::Mysql, good(Engine::Mysql));
        let past = Instant::now() - Duration::from_millis(1);
        assert!(matches!(
            preflight_class(
                &mut fake,
                Engine::Mysql,
                "Exact_DB",
                &account(),
                "writer@%",
                true,
                past
            )
            .await,
            Err("timeout")
        ));
        assert!(fake.seen.is_empty());
        assert!(matches!(
            sample_class(&mut fake, Engine::Mysql, "Exact_DB", ids(), past).await,
            Err("timeout")
        ));
        assert!(fake.seen.is_empty());
    }
    #[tokio::test]
    async fn preflight_identity_and_grants_are_independent_of_successful_scans() {
        for engine in [Engine::Mysql, Engine::Tidb] {
            for mutation in 0..8 {
                let mut rows = good(engine);
                if let ReadResult::Identity {
                    connection_id,
                    version,
                    database,
                    current_user,
                    server_uuid,
                    global_ids,
                    ..
                } = &mut rows[0].1
                {
                    match mutation {
                        0 => *connection_id = Some(0),
                        1 => *version = None,
                        2 => *version = Some("wrong-version".into()),
                        3 => *database = Some(String::new()),
                        4 => *current_user = Some("writer@%".into()),
                        5 => *server_uuid = None,
                        6 => *global_ids = Some("false".into()),
                        _ => *global_ids = None,
                    }
                }
                if (engine == Engine::Mysql && mutation >= 6)
                    || (engine == Engine::Tidb && mutation == 5)
                {
                    continue;
                }
                let mut fake = Fake::new(engine, rows);
                assert!(
                    preflight_class(
                        &mut fake,
                        engine,
                        "Exact_DB",
                        &account(),
                        "writer@%",
                        true,
                        total()
                    )
                    .await
                    .is_err()
                );
                assert_eq!(fake.seen.len(), 1);
            }
            let mut rows = good(engine);
            if let ReadResult::Grants(g) = &mut rows[1].1 {
                g.clear();
            }
            let mut fake = Fake::new(engine, rows);
            assert!(matches!(
                preflight_class(
                    &mut fake,
                    engine,
                    "Exact_DB",
                    &account(),
                    "writer@%",
                    true,
                    total()
                )
                .await,
                Err("shape_rejected")
            ));
            assert_eq!(fake.seen.len(), 2);
            let mut rows = good(engine);
            rows[0].1 = ReadResult::Grants(vec![]);
            let mut fake = Fake::new(engine, rows);
            assert!(matches!(
                preflight_class(
                    &mut fake,
                    engine,
                    "Exact_DB",
                    &account(),
                    "writer@%",
                    true,
                    total()
                )
                .await,
                Err("decode_error")
            ));
            assert_eq!(fake.seen.len(), 1);
            let mut rows = good(engine);
            if let ReadResult::Roles { current_role, .. } = &mut rows[2].1 {
                *current_role = None;
            }
            let mut fake = Fake::new(engine, rows);
            assert!(matches!(
                preflight_class(
                    &mut fake,
                    engine,
                    "Exact_DB",
                    &account(),
                    "writer@%",
                    true,
                    total()
                )
                .await,
                Err("decode_error")
            ));
            assert_eq!(fake.seen.len(), 3);
            if engine == Engine::Mysql {
                let mut rows = good(engine);
                if let ReadResult::Roles {
                    mandatory_roles, ..
                } = &mut rows[2].1
                {
                    *mandatory_roles = Some("injected".into());
                }
                let mut fake = Fake::new(engine, rows);
                assert!(matches!(
                    preflight_class(
                        &mut fake,
                        engine,
                        "Exact_DB",
                        &account(),
                        "writer@%",
                        true,
                        total()
                    )
                    .await,
                    Err("roles_unverified")
                ));
            }
        }
    }
    #[tokio::test]
    async fn sample_query_and_decode_failures_do_not_turn_into_empty_edges() {
        for engine in [Engine::Mysql, Engine::Tidb] {
            let mut fake = Fake::new(engine, vec![]);
            let step = if engine == Engine::Mysql {
                ReadStep::MysqlWaiterThread
            } else {
                ReadStep::TidbMapping
            };
            fake.remaining.push_back((step, Err(ReadFailure::Query)));
            assert!(matches!(
                sample_class(&mut fake, engine, "Exact_DB", ids(), total()).await,
                Err("query_error")
            ));
            fake.done();
            let mut fake = Fake::new(engine, vec![]);
            fake.remaining.push_back((step, Err(ReadFailure::Decode)));
            assert!(matches!(
                sample_class(&mut fake, engine, "Exact_DB", ids(), total()).await,
                Err("decode_error")
            ));
            fake.done();
        }
    }
    #[tokio::test]
    async fn shared_absolute_total_is_not_refreshed_between_steps() {
        struct SlowIdentity {
            inner: Fake,
        }
        impl ReadExecutor for SlowIdentity {
            async fn read(
                &mut self,
                step: ReadStep,
                plan: QueryPlan,
            ) -> Result<ReadResult, ReadFailure> {
                if step == ReadStep::Identity {
                    tokio::time::sleep(Duration::from_millis(15)).await;
                }
                self.inner.read(step, plan).await
            }
        }
        let mut fake = SlowIdentity {
            inner: Fake::new(Engine::Mysql, good(Engine::Mysql)),
        };
        let total = Instant::now() + Duration::from_millis(8);
        assert!(matches!(
            preflight_class(
                &mut fake,
                Engine::Mysql,
                "Exact_DB",
                &account(),
                "writer@%",
                true,
                total
            )
            .await,
            Err("timeout")
        ));
        assert!(
            fake.inner.seen.is_empty(),
            "timed-out query future must be dropped"
        );
        assert!(matches!(
            sample_class(&mut fake, Engine::Mysql, "Exact_DB", ids(), total).await,
            Err("timeout")
        ));
        assert!(fake.inner.seen.is_empty());
    }
    #[tokio::test]
    async fn second_step_cannot_restart_total_budget() {
        struct SlowGrants {
            inner: Fake,
        }
        impl ReadExecutor for SlowGrants {
            async fn read(
                &mut self,
                step: ReadStep,
                plan: QueryPlan,
            ) -> Result<ReadResult, ReadFailure> {
                if step == ReadStep::Grants {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                self.inner.read(step, plan).await
            }
        }
        let mut fake = SlowGrants {
            inner: Fake::new(Engine::Mysql, good(Engine::Mysql)),
        };
        let deadline = Instant::now() + Duration::from_millis(20);
        assert!(matches!(
            preflight_class(
                &mut fake,
                Engine::Mysql,
                "Exact_DB",
                &account(),
                "writer@%",
                true,
                deadline
            )
            .await,
            Err("timeout")
        ));
        assert!(
            fake.inner.seen == [ReadStep::Identity],
            "later query must be cancelled before returning"
        );
    }
    #[tokio::test]
    async fn same_error_class_preserves_explicit_distinct_stages() {
        use observer::{FlowFailure, FlowStage};
        let input = account();
        let mut no_topology = Fake::new(Engine::Tidb, good(Engine::Tidb));
        assert!(matches!(
            observer::preflight(
                &mut no_topology,
                Engine::Tidb,
                "Exact_DB",
                &input,
                "writer@%",
                false,
                total()
            )
            .await,
            Err(FlowFailure {
                stage: FlowStage::Prerequisite,
                error_class: "prerequisite_missing"
            })
        ));
        assert!(no_topology.seen.is_empty());
        for index in [0usize, 1, 2, 3, 4, 5] {
            let mut fake = Fake::new(Engine::Mysql, good(Engine::Mysql));
            let step = fake.remaining[index].0;
            fake.remaining[index].1 = Err(ReadFailure::Decode);
            let result = observer::preflight(
                &mut fake,
                Engine::Mysql,
                "Exact_DB",
                &input,
                "writer@%",
                true,
                total(),
            )
            .await;
            assert!(
                matches!(result,Err(FlowFailure{stage:FlowStage::Step(actual),error_class:"decode_error"}) if actual==step)
            );
            assert_eq!(fake.seen.len(), index + 1);
        }
        for (step, result) in [
            (ReadStep::MysqlWaiterThread, ReadResult::Count(None)),
            (ReadStep::MysqlHolderThread, ReadResult::Count(None)),
            (ReadStep::Edge, ReadResult::Count(None)),
        ] {
            let mut rows = vec![
                (ReadStep::MysqlWaiterThread, ReadResult::Count(Some(1))),
                (ReadStep::MysqlHolderThread, ReadResult::Count(Some(1))),
                (ReadStep::Edge, ReadResult::Count(Some(0))),
            ];
            rows.iter_mut().find(|(s, _)| *s == step).unwrap().1 = result;
            let mut fake = Fake::new(Engine::Mysql, rows);
            assert!(
                matches!(observer::sample_edge(&mut fake,Engine::Mysql,"Exact_DB",ids(),total()).await,
                Err(FlowFailure{stage:FlowStage::Step(actual),error_class:"decode_error"}) if actual==step)
            );
        }
        let mut fake = Fake::new(Engine::Mysql, good(Engine::Mysql));
        let past = Instant::now() - Duration::from_millis(1);
        assert!(matches!(
            observer::preflight(
                &mut fake,
                Engine::Mysql,
                "Exact_DB",
                &input,
                "writer@%",
                true,
                past
            )
            .await,
            Err(FlowFailure {
                stage: FlowStage::Step(ReadStep::Identity),
                error_class: "timeout"
            })
        ));
    }
    #[tokio::test]
    async fn zero_stage_never_polls_ready_future() {
        use std::cell::Cell;
        let polled = Cell::new(false);
        let result = observer::bounded(
            async {
                polled.set(true);
                Ok::<_, &'static str>(())
            },
            Instant::now() + Duration::from_secs(1),
            Duration::ZERO,
        )
        .await;
        assert_eq!(result, Err("timeout"));
        assert!(!polled.get());
    }
}
