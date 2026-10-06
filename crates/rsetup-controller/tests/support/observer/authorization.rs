//! Bounded, strict declaration from trusted parent stdin; not proof of operator authority.
use std::{
    io::Read,
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::time::Instant;

use super::{Account, Engine, FlowFailure, FlowStage, ObserverError, PreparedObserver};

const INVALID: ObserverError = "prerequisite_missing";
const MAX_WIRE_BYTES: usize = 65_536;

// No Debug/Display, public fields, or alternate constructor. Parsing a declaration
// never verifies that an operator actually authorized the parent to make it.
pub(crate) struct AuthorizedRun {
    engine: Engine,
    writer_pin: String,
    observer_pin: String,
    user: String,
    host: String,
    run_id: String,
    window_start: i128,
    window_end: i128,
    transport_policy_ref: String,
    topology_ref: String,
    scope: String,
}

impl AuthorizedRun {
    // A declaration parsed from the dedicated stdin, never from inherited environment.
    pub(super) fn pins(&self) -> (&str, &str) {
        (&self.writer_pin, &self.observer_pin)
    }
}

fn string(raw: &[u8]) -> Result<String, ObserverError> {
    serde_json::from_slice::<String>(raw).map_err(|_| INVALID)
}

fn reference(raw: &[u8]) -> Result<String, ObserverError> {
    let value = string(raw)?;
    if value.len() > 128
        || !value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(INVALID);
    }
    Ok(value)
}

fn pin(raw: &[u8]) -> Result<String, ObserverError> {
    let pin = string(raw)?;
    if pin.len() != 64
        || !pin
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(INVALID);
    }
    Ok(pin)
}

fn account_atom(raw: &[u8]) -> Result<String, ObserverError> {
    let atom = string(raw)?;
    if atom.is_empty()
        || !atom
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.%-:".contains(&b))
    {
        return Err(INVALID);
    }
    Ok(atom)
}

fn account(raw: &[u8]) -> Result<(String, String), ObserverError> {
    let mut user = None;
    let mut host = None;
    for (key, value) in super::config::object(raw).map_err(|_| INVALID)? {
        match key.as_str() {
            "user" => user = Some(account_atom(value)?),
            "host" => host = Some(account_atom(value)?),
            _ => return Err(INVALID),
        }
    }
    Ok((user.ok_or(INVALID)?, host.ok_or(INVALID)?))
}

fn decimal(bytes: &[u8]) -> Result<i64, ObserverError> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return Err(INVALID);
    }
    bytes
        .iter()
        .try_fold(0_i64, |n, b| {
            n.checked_mul(10)?.checked_add(i64::from(b - b'0'))
        })
        .ok_or(INVALID)
}

fn now_nanos() -> Result<i128, ObserverError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| INVALID)?;
    Ok(i128::from(elapsed.as_secs()) * 1_000_000_000 + i128::from(elapsed.subsec_nanos()))
}

fn window(raw: &[u8]) -> Result<i128, ObserverError> {
    let text = string(raw)?;
    let b = text.as_bytes();
    if b.len() < 20
        || b.get(4) != Some(&b'-')
        || b.get(7) != Some(&b'-')
        || b.get(10) != Some(&b'T')
        || b.get(13) != Some(&b':')
        || b.get(16) != Some(&b':')
    {
        return Err(INVALID);
    }
    let year = decimal(&b[0..4])?;
    if year == 0 {
        return Err(INVALID);
    }
    let month = decimal(&b[5..7])?;
    let day = decimal(&b[8..10])?;
    let hour = decimal(&b[11..13])?;
    let minute = decimal(&b[14..16])?;
    let second = decimal(&b[17..19])?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return Err(INVALID),
    };
    if day < 1 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return Err(INVALID);
    }
    let mut pos = 19;
    let mut nanos = 0_i64;
    if b.get(pos) == Some(&b'.') {
        pos += 1;
        let fraction_start = pos;
        while b.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        let digits = pos - fraction_start;
        if digits == 0 || digits > 6 {
            return Err(INVALID);
        }
        nanos = decimal(&b[fraction_start..pos])? * 10_i64.pow((9 - digits) as u32);
    }
    if &b[pos..] != b"Z" && &b[pos..] != b"+00:00" {
        return Err(INVALID);
    }
    // Gregorian days since Unix epoch; arithmetic is bounded by the exact four-digit year.
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let year_of_era_day = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + year_of_era_day - 719_468;
    Ok(
        (i128::from(days) * 86_400 + i128::from(hour * 3_600 + minute * 60 + second))
            * 1_000_000_000
            + i128::from(nanos),
    )
}

fn allowed_scope(scope: &str) -> bool {
    matches!(
        scope,
        "observer-capabilities"
            | "admission_deactivation_holds_guard_cas_waits_then_denied_on_fresh_v3_mysql_observer"
            | "admission_cas_commits_before_deactivation_then_new_cas_denied_on_fresh_v3_mysql_observer"
            | "admission_deactivation_holds_guard_cas_waits_then_denied_on_fresh_v3_tidb_observer"
            | "admission_cas_commits_before_deactivation_then_new_cas_denied_on_fresh_v3_tidb_observer"
    )
}

fn scope_matches_engine(scope: &str, engine: Engine) -> bool {
    scope == "observer-capabilities"
        || match engine {
            Engine::Mysql => scope.ends_with("_mysql_observer"),
            Engine::Tidb => scope.ends_with("_tidb_observer"),
        }
}

pub(crate) fn read_authorization(mut input: impl Read) -> Result<AuthorizedRun, ObserverError> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take((MAX_WIRE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| INVALID)?;
    if bytes.is_empty() || bytes.len() > MAX_WIRE_BYTES {
        return Err(INVALID);
    }
    // config::object scans raw keys and decodes each before duplicate detection;
    // serde_json::Value maps alone would silently erase escaped duplicate keys.
    let mut engine = None;
    let mut writer_pin = None;
    let mut observer_pin = None;
    let mut expected_account = None;
    let mut run_id = None;
    let mut window_start = None;
    let mut window_end = None;
    let mut transport_policy_ref = None;
    let mut topology_ref = None;
    let mut operator_confirmed = false;
    let mut scope = None;
    for (key, value) in super::config::object(&bytes).map_err(|_| INVALID)? {
        match key.as_str() {
            "engine" => {
                engine = Some(match string(value)?.as_str() {
                    "mysql" => Engine::Mysql,
                    "tidb" => Engine::Tidb,
                    _ => return Err(INVALID),
                })
            }
            "writer_pin" => writer_pin = Some(pin(value)?),
            "observer_pin" => observer_pin = Some(pin(value)?),
            "expected_account" => expected_account = Some(account(value)?),
            "run_id" => run_id = Some(reference(value)?),
            "window_start" => window_start = Some(window(value)?),
            "window_end" => window_end = Some(window(value)?),
            "transport_policy_ref" => transport_policy_ref = Some(reference(value)?),
            "topology_ref" => topology_ref = Some(reference(value)?),
            "operator_confirmed" if value == b"true" => operator_confirmed = true,
            "scope" => {
                let text = string(value)?;
                if !allowed_scope(&text) {
                    return Err(INVALID);
                }
                scope = Some(text);
            }
            _ => return Err(INVALID),
        }
    }
    let window_start = window_start.ok_or(INVALID)?;
    let window_end = window_end.ok_or(INVALID)?;
    let now = now_nanos()?;
    if !(window_start < window_end && window_start <= now && now < window_end)
        || !operator_confirmed
    {
        return Err(INVALID);
    }
    let (user, host) = expected_account.ok_or(INVALID)?;
    let scope = scope.ok_or(INVALID)?;
    let engine = engine.ok_or(INVALID)?;
    if !scope_matches_engine(&scope, engine) {
        return Err(INVALID);
    }
    let writer_pin = writer_pin.ok_or(INVALID)?;
    let observer_pin = observer_pin.ok_or(INVALID)?;
    if writer_pin == observer_pin {
        return Err(INVALID);
    }
    Ok(AuthorizedRun {
        engine,
        writer_pin,
        observer_pin,
        user,
        host,
        run_id: run_id.ok_or(INVALID)?,
        window_start,
        window_end,
        transport_policy_ref: transport_policy_ref.ok_or(INVALID)?,
        topology_ref: topology_ref.ok_or(INVALID)?,
        scope,
    })
}

fn denied(error_class: ObserverError) -> FlowFailure {
    FlowFailure {
        stage: FlowStage::Prerequisite,
        error_class,
    }
}

pub(crate) fn authorize_open(
    prepared: &PreparedObserver,
    auth: Option<&AuthorizedRun>,
    expected: &Account,
    requested_scope: &str,
    total: Instant,
) -> Result<(), FlowFailure> {
    let auth = auth.ok_or_else(|| denied(INVALID))?;
    let now = now_nanos().map_err(|_| denied(INVALID))?;
    let (writer_pin, observer_pin) = prepared.pins();
    if !allowed_scope(requested_scope)
        || !scope_matches_engine(requested_scope, prepared.engine())
        || auth.scope != requested_scope
        || auth.engine != prepared.engine()
        || auth.writer_pin != writer_pin
        || auth.observer_pin != observer_pin
        || auth.run_id.is_empty()
        || auth.transport_policy_ref.is_empty()
        || auth.topology_ref.is_empty()
        || auth.window_start > now
        || now >= auth.window_end
        || auth.window_start >= auth.window_end
    {
        return Err(denied(INVALID));
    }
    if expected.user.is_empty()
        || expected.host.is_empty()
        || auth.user != expected.user
        || auth.host != expected.host
        || prepared.schema().is_empty()
    {
        return Err(denied("identity_mismatch"));
    }
    if total <= Instant::now() {
        return Err(FlowFailure {
            stage: FlowStage::Connect,
            error_class: "timeout",
        });
    }
    Ok(())
}
