//! Bounded observer configuration preparation and explicit Linux inherited-fd consumer.
use sha2::{Digest, Sha256};
use sqlx::{ConnectOptions, mysql::MySqlConnectOptions};

use super::{Engine, ObserverError};

const MAX_BYTES: usize = 65_536;
const INVALID: ObserverError = "config_invalid";

pub(crate) struct PinnedInput<'a> {
    pub(crate) writer_bytes: &'a [u8],
    pub(crate) observer_bytes: &'a [u8],
    pub(crate) writer_pin: &'a str,
    pub(crate) observer_pin: &'a str,
}

// Never derive Debug/Display: options contain the observer password.
pub(crate) struct PreparedObserver {
    options: MySqlConnectOptions,
    schema: String,
    engine: Engine,
    writer_pin: String,
    observer_pin: String,
}

impl PreparedObserver {
    pub(crate) fn options(&self) -> &MySqlConnectOptions {
        &self.options
    }

    pub(crate) fn engine(&self) -> Engine {
        self.engine
    }

    pub(super) fn pins(&self) -> (&str, &str) {
        (&self.writer_pin, &self.observer_pin)
    }

    pub(crate) fn schema(&self) -> &str {
        &self.schema
    }
}

fn verify_pin(bytes: &[u8], pin: &str) -> Result<(), ObserverError> {
    if bytes.is_empty()
        || bytes.len() > MAX_BYTES
        || pin.len() != 64
        || !pin
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || format!("{:x}", Sha256::digest(bytes)) != pin
    {
        return Err("config_changed_since_authorization");
    }
    Ok(())
}

// Do not deserialize into a JSON map: doing so erases duplicate keys (including
// escapes that decode to the same name). Only the string token is decoded by serde_json.
struct Cursor<'a> {
    raw: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn skip_ws(&mut self) {
        while self
            .raw
            .get(self.pos)
            .is_some_and(|b| matches!(*b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.pos += 1;
        }
    }

    fn byte(&mut self, b: u8) -> Result<(), ObserverError> {
        self.skip_ws();
        if self.raw.get(self.pos) != Some(&b) {
            return Err(INVALID);
        }
        self.pos += 1;
        Ok(())
    }

    fn string(&mut self) -> Result<String, ObserverError> {
        self.skip_ws();
        if self.raw.get(self.pos) != Some(&b'"') {
            return Err(INVALID);
        }
        let begin = self.pos;
        self.pos += 1;
        while let Some(&b) = self.raw.get(self.pos) {
            self.pos += 1;
            match b {
                b'\\' => {
                    // Let serde_json validate the escape and UTF-8; avoid treating \" as a close.
                    if self.pos >= self.raw.len() {
                        return Err(INVALID);
                    }
                    self.pos += 1;
                }
                b'"' => {
                    return serde_json::from_slice(&self.raw[begin..self.pos]).map_err(|_| INVALID);
                }
                _ => {}
            }
        }
        Err(INVALID)
    }

    fn value(&mut self) -> Result<&'a [u8], ObserverError> {
        self.skip_ws();
        let start = self.pos;
        match self.raw.get(self.pos) {
            Some(b'"') => {
                self.string()?;
            }
            Some(b'{' | b'[') => {
                let mut stack = Vec::new();
                let mut quoted = false;
                let mut escaped = false;
                while let Some(&b) = self.raw.get(self.pos) {
                    self.pos += 1;
                    if quoted {
                        if escaped {
                            escaped = false;
                        } else if b == b'\\' {
                            escaped = true;
                        } else if b == b'"' {
                            quoted = false;
                        }
                        continue;
                    }
                    match b {
                        b'"' => quoted = true,
                        b'{' => stack.push(b'}'),
                        b'[' => stack.push(b']'),
                        b'}' | b']' => {
                            if stack.pop() != Some(b) {
                                return Err(INVALID);
                            }
                            if stack.is_empty() {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if !stack.is_empty() || quoted {
                    return Err(INVALID);
                }
            }
            Some(_) => {
                while self
                    .raw
                    .get(self.pos)
                    .is_some_and(|b| !matches!(b, b',' | b'}' | b' ' | b'\n' | b'\r' | b'\t'))
                {
                    self.pos += 1;
                }
            }
            None => return Err(INVALID),
        }
        let token = &self.raw[start..self.pos];
        serde_json::from_slice::<serde_json::Value>(token).map_err(|_| INVALID)?;
        Ok(token)
    }
}

pub(super) fn object(raw: &[u8]) -> Result<Vec<(String, &[u8])>, ObserverError> {
    let mut c = Cursor { raw, pos: 0 };
    c.byte(b'{')?;
    let mut fields = Vec::new();
    c.skip_ws();
    if c.raw.get(c.pos) != Some(&b'}') {
        loop {
            let key = c.string()?;
            if fields.iter().any(|(previous, _)| previous == &key) {
                return Err(INVALID);
            }
            c.byte(b':')?;
            let value = c.value()?;
            fields.push((key, value));
            c.skip_ws();
            if c.raw.get(c.pos) != Some(&b',') {
                break;
            }
            c.pos += 1;
        }
    }
    c.byte(b'}')?;
    c.skip_ws();
    if c.pos != raw.len() {
        return Err(INVALID);
    }
    Ok(fields)
}

fn nonempty_string(raw: &[u8]) -> Result<String, ObserverError> {
    serde_json::from_slice::<String>(raw)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(INVALID)
}

fn credentials(raw: &[u8]) -> Result<(String, String), ObserverError> {
    let mut username = None;
    let mut password = None;
    for (key, value) in object(raw)? {
        match key.as_str() {
            "username" => username = Some(nonempty_string(value)?),
            "password" => password = Some(nonempty_string(value)?),
            _ => return Err(INVALID),
        }
    }
    Ok((username.ok_or(INVALID)?, password.ok_or(INVALID)?))
}

struct Writer {
    host: String,
    port: u16,
    database: String,
}

fn writer(raw: &[u8], engine: Engine) -> Result<Writer, ObserverError> {
    let mut version = false;
    let mut matched_engine = false;
    let mut connection = None;
    for (key, value) in object(raw)? {
        match key.as_str() {
            "schema_version" => version = value == b"1",
            "engine" => {
                matched_engine = nonempty_string(value)?
                    == match engine {
                        Engine::Mysql => "mysql",
                        Engine::Tidb => "tidb",
                    };
            }
            "connection" => connection = Some(value),
            // Writer policy is deliberately not interpreted as observer authorization.
            "dev_test_policy" => {
                if !value.starts_with(b"{") {
                    return Err(INVALID);
                }
            }
            _ => return Err(INVALID),
        }
    }
    if !version || !matched_engine {
        return Err(INVALID);
    }
    let mut host = None;
    let mut port = None;
    let mut database = None;
    let mut writer_user = false;
    let mut writer_password = false;
    for (key, value) in object(connection.ok_or(INVALID)?)? {
        match key.as_str() {
            "host" => host = Some(nonempty_string(value)?),
            "port" => {
                port = Some(
                    serde_json::from_slice::<u16>(value)
                        .ok()
                        .filter(|p| *p != 0)
                        .ok_or(INVALID)?,
                );
            }
            "username" => {
                nonempty_string(value)?;
                writer_user = true;
            }
            "password" => {
                nonempty_string(value)?;
                writer_password = true;
            }
            "database" => database = Some(nonempty_string(value)?),
            // Unknown transport options, including TLS, URL and Unix sockets, cannot be
            // silently replaced with SQLx defaults.
            _ => return Err(INVALID),
        }
    }
    if !writer_user || !writer_password {
        return Err(INVALID);
    }
    Ok(Writer {
        host: host.ok_or(INVALID)?,
        port: port.ok_or(INVALID)?,
        database: database.ok_or(INVALID)?,
    })
}

#[cfg(target_os = "linux")]
mod inherited_fd {
    use super::{Engine, MAX_BYTES, ObserverError, PinnedInput, PreparedObserver, prepare};
    use std::{
        fs::{self, File, Metadata},
        io::{Read, Seek, SeekFrom},
        os::{
            fd::{AsRawFd, OwnedFd},
            unix::fs::MetadataExt,
        },
    };

    const INVALID_FD: ObserverError = "config_invalid";
    const MAX_FDINFO: u64 = 4096;
    // Linux fs/fcntl.h: O_ACCMODE=03, O_PATH=010000000. No libc dependency.
    const O_ACCMODE: u32 = 0o3;
    const O_PATH: u32 = 0o10000000;

    fn flags(fd: i32) -> Result<u32, ObserverError> {
        // fdinfo is metadata only; never reopen /proc/self/fd/N for config bytes.
        let mut info = Vec::new();
        File::open(format!("/proc/self/fdinfo/{fd}"))
            .map_err(|_| INVALID_FD)?
            .take(MAX_FDINFO + 1)
            .read_to_end(&mut info)
            .map_err(|_| INVALID_FD)?;
        if info.len() as u64 > MAX_FDINFO {
            return Err(INVALID_FD);
        }
        let info = std::str::from_utf8(&info).map_err(|_| INVALID_FD)?;
        let mut flags = info
            .lines()
            .filter_map(|line| line.strip_prefix("flags:\t"));
        let value = flags.next().ok_or(INVALID_FD)?;
        if flags.next().is_some()
            || value.is_empty()
            || !value.bytes().all(|b| (b'0'..=b'7').contains(&b))
        {
            return Err(INVALID_FD);
        }
        u32::from_str_radix(value, 8).map_err(|_| INVALID_FD)
    }

    fn fd_metadata(fd: i32) -> Result<Metadata, ObserverError> {
        fs::metadata(format!("/proc/self/fd/{fd}")).map_err(|_| INVALID_FD)
    }

    fn same_meta(a: &Metadata, b: &Metadata) -> bool {
        (
            a.dev(),
            a.ino(),
            a.len(),
            a.mode(),
            a.uid(),
            a.gid(),
            a.mtime(),
            a.mtime_nsec(),
            a.ctime(),
            a.ctime_nsec(),
        ) == (
            b.dev(),
            b.ino(),
            b.len(),
            b.mode(),
            b.uid(),
            b.gid(),
            b.mtime(),
            b.mtime_nsec(),
            b.ctime(),
            b.ctime_nsec(),
        )
    }

    fn read_one(file: &mut File) -> Result<Vec<u8>, ObserverError> {
        let fd = file.as_raw_fd();
        let access = flags(fd)?;
        if access & (O_ACCMODE | O_PATH) != 0 {
            return Err(INVALID_FD);
        }
        let before = file.metadata().map_err(|_| INVALID_FD)?;
        if !before.is_file()
            || before.len() == 0
            || before.len() > MAX_BYTES as u64
            || !same_meta(&before, &fd_metadata(fd)?)
        {
            return Err(INVALID_FD);
        }
        file.seek(SeekFrom::Start(0)).map_err(|_| INVALID_FD)?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| INVALID_FD)?;
        let after = file.metadata().map_err(|_| INVALID_FD)?;
        if bytes.len() as u64 != before.len()
            || !same_meta(&before, &after)
            || !same_meta(&after, &fd_metadata(fd)?)
            || flags(fd)? != access
        {
            return Err(INVALID_FD);
        }
        Ok(bytes)
    }

    /// Consume two already-owned fds; no raw descriptor adoption occurs here.
    pub(crate) fn load_inherited(
        writer_fd: OwnedFd,
        observer_fd: OwnedFd,
        writer_pin: &str,
        observer_pin: &str,
        engine: Engine,
    ) -> Result<PreparedObserver, ObserverError> {
        // Both are owned on entry: either file is dropped on every subsequent error.
        let mut writer: File = writer_fd.into();
        let mut observer: File = observer_fd.into();
        let writer_bytes = read_one(&mut writer)?;
        let observer_bytes = read_one(&mut observer)?;
        drop(writer);
        drop(observer);
        prepare(
            PinnedInput {
                writer_bytes: &writer_bytes,
                observer_bytes: &observer_bytes,
                writer_pin,
                observer_pin,
            },
            engine,
        )
    }
}

#[cfg(target_os = "linux")]
pub(crate) use inherited_fd::load_inherited;

#[cfg(not(target_os = "linux"))]
pub(crate) fn load_inherited(
    _writer_fd: std::os::fd::OwnedFd,
    _observer_fd: std::os::fd::OwnedFd,
    _writer_pin: &str,
    _observer_pin: &str,
    _engine: Engine,
) -> Result<PreparedObserver, ObserverError> {
    // Unsupported platform: both owned inputs still close on return.
    Err(INVALID)
}

pub(crate) fn prepare(
    input: PinnedInput<'_>,
    engine: Engine,
) -> Result<PreparedObserver, ObserverError> {
    verify_pin(input.writer_bytes, input.writer_pin)?;
    verify_pin(input.observer_bytes, input.observer_pin)?;
    let writer = writer(input.writer_bytes, engine)?;
    let (username, password) = credentials(input.observer_bytes)?;
    let options = MySqlConnectOptions::new()
        .host(&writer.host)
        .port(writer.port)
        .username(&username)
        .password(&password)
        .disable_statement_logging()
        // SQLx 0.8.6 sends these SETs by default, before the first explicit SELECT.
        .pipes_as_concat(false)
        .no_engine_substitution(false)
        .timezone(None)
        .set_names(false);
    Ok(PreparedObserver {
        options,
        schema: writer.database,
        engine,
        writer_pin: input.writer_pin.to_owned(),
        observer_pin: input.observer_pin.to_owned(),
    })
}
