//! 板端代理 amd64 本地可执行入口（仅本地测试，不是生产隧道传输）。
//!
//! 用途：
//! - `--once`：通过 `BoardAgent::handle_typed_action` 执行三条只读动作并输出 JSON；
//! - `--watch`：周期性输出最新 `device.status.get` 样本，作为本机长期运行的探针；
//! - `--loopback-test-server`：在回环地址上提供**仅测试用**的逐行 JSON 控制台，
//!   用于在本机驱动器上验证白名单、DTO 校验、journal 查询与错误映射。
//!
//! 本入口**不实现**生产加密隧道（协议 codec 尚未获批），也绝不触发真实主机重启。
//! 回环 JSON 协议是为了本地验收而存在的最小测试接口，不能当作生产 wire 协议。

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rsetup_board_agent::dispatch::{
    BoardAgent, BusinessResponse, BusinessStatus, JournalEpochSource, TaskJournalReader,
};
use rsetup_board_agent::journal::{JournalStore, RealFs};
use rsetup_board_agent::read_only::{PeerContext, ReadOnlySource};
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, Capabilities, ClockSample, DeviceStatus, Metric,
    Query, TaskQuery,
};

const DEFAULT_JOURNAL_DIR: &str = "/tmp/rsetup-board-agent-journal";
const DEFAULT_INTERVAL_SECS: u64 = 10;
const MAX_REQUEST_LINE: usize = 4096;

struct Options {
    once: bool,
    watch: bool,
    loopback: bool,
    listen: String,
    journal_dir: PathBuf,
    interval: Duration,
    controller_key: [u8; 32],
}

fn usage() -> String {
    "rsetup-board-agent <mode>\n\
     \n\
     Modes:\n\
       --once                         run three read-only actions once and print JSON\n\
       --watch                        run device.status.get in a loop\n\
       --loopback-test-server         expose a JSON-lines test console on --listen\n\
     Common options:\n\
       --listen <addr>                loopback address (default 127.0.0.1:39876)\n\
       --journal-dir <dir>            durable journal directory (default /tmp/rsetup-board-agent-journal)\n\
       --interval-secs <n>            watch interval (default 10)\n\
       --controller-key <64-hex>      controller key used for journal queries (default random)\n\
       -h, --help                     print this help\n\
     \n\
     This binary is a local test harness. It does not provide the production encrypted tunnel\n\
     and never performs a real host reboot."
        .to_string()
}

fn parse_args() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut once = false;
    let mut watch = false;
    let mut loopback = false;
    let mut listen = "127.0.0.1:39876".to_string();
    let mut journal_dir = PathBuf::from(DEFAULT_JOURNAL_DIR);
    let mut interval = Duration::from_secs(DEFAULT_INTERVAL_SECS);
    let mut controller_key = [0u8; 32];
    controller_key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    controller_key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--once" => once = true,
            "--watch" => watch = true,
            "--loopback-test-server" => loopback = true,
            "--listen" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing --listen value".to_string())?;
                if value.is_empty() || !value.is_ascii() {
                    return Err("invalid --listen value".to_string());
                }
                listen = value;
            }
            "--journal-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing --journal-dir value".to_string())?;
                if value.is_empty() {
                    return Err("invalid --journal-dir value".to_string());
                }
                journal_dir = PathBuf::from(value);
            }
            "--interval-secs" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing --interval-secs value".to_string())?;
                let secs = value
                    .parse::<u64>()
                    .map_err(|_| "invalid --interval-secs value".to_string())?;
                if secs == 0 || secs > 3600 {
                    return Err("--interval-secs must be 1..=3600".to_string());
                }
                interval = Duration::from_secs(secs);
            }
            "--controller-key" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing --controller-key value".to_string())?;
                if value.len() != 64
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                {
                    return Err("--controller-key must be 64 lowercase hex characters".to_string());
                }
                let bytes =
                    hex::decode(value).map_err(|_| "invalid --controller-key".to_string())?;
                controller_key = bytes
                    .try_into()
                    .map_err(|_| "invalid --controller-key".to_string())?;
            }
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            _ => return Err("unknown argument".to_string()),
        }
    }
    let modes = once as u8 + watch as u8 + loopback as u8;
    if modes != 1 {
        return Err(
            "exactly one of --once, --watch or --loopback-test-server is required".to_string(),
        );
    }
    Ok(Options {
        once,
        watch,
        loopback,
        listen,
        journal_dir,
        interval,
        controller_key,
    })
}

/// 本机只读来源：读取 /proc 与 kernel boot_id；任何读数失败都标 quality=unavailable，
/// 绝不把未知回填 0。
struct SystemReadOnlySource {
    boot_id: String,
    agent_epoch: String,
    clock_epoch: String,
    journal_epoch: String,
    sample_seq: AtomicU64,
    started: Instant,
}

impl SystemReadOnlySource {
    fn new(journal_epoch: String) -> Self {
        Self {
            boot_id: read_boot_id(),
            agent_epoch: uuid::Uuid::new_v4().to_string(),
            clock_epoch: uuid::Uuid::new_v4().to_string(),
            journal_epoch,
            sample_seq: AtomicU64::new(0),
            started: Instant::now(),
        }
    }

    fn monotonic_ns(&self) -> u64 {
        self.started
            .elapsed()
            .as_nanos()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    fn metrics(&self) -> Vec<Metric> {
        let mut metrics = Vec::new();
        if let Some(value) = read_loadavg() {
            metrics.push(metric("cpu.load1", value, "1m"));
        }
        if let Some(value) = read_mem_used_percent() {
            metrics.push(metric("memory.used_percent", value, "%"));
        }
        if let Some(value) = read_uptime_secs() {
            metrics.push(metric("system.uptime", value, "s"));
        }
        metrics
    }
}

fn metric(key: &str, value: f64, unit: &str) -> Metric {
    Metric {
        key: key.to_string(),
        value: Some(value),
        unit: unit.to_string(),
        quality: "ok".to_string(),
    }
}

fn read_boot_id() -> String {
    let raw = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| uuid::Uuid::parse_str(s).is_ok());
    raw.unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

fn read_loadavg() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

fn read_uptime_secs() -> Option<f64> {
    std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

fn read_mem_used_percent() -> Option<f64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = None;
    let mut available = None;
    for line in info.lines() {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("MemTotal:") => total = fields.next().and_then(|v| v.parse::<f64>().ok()),
            Some("MemAvailable:") => available = fields.next().and_then(|v| v.parse::<f64>().ok()),
            _ => {}
        }
    }
    let (total, available) = (total?, available?);
    if total <= 0.0 || available < 0.0 || available > total {
        return None;
    }
    let value = (total - available) / total * 100.0;
    value.is_finite().then_some(value)
}

impl ReadOnlySource for SystemReadOnlySource {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: self.boot_id.clone(),
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            supported_actions: rsetup_board_agent::allowed_action_names()
                .into_iter()
                .map(str::to_string)
                .collect(),
            durable_task_journal: true,
            max_status_bytes: 65536,
            journal_epoch: self.journal_epoch.clone(),
        }
    }

    fn status(&self) -> DeviceStatus {
        let seq = self.sample_seq.fetch_add(1, Ordering::SeqCst) + 1;
        DeviceStatus {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: self.boot_id.clone(),
            agent_epoch: self.agent_epoch.clone(),
            sample_seq: seq,
            sampled_unix_ms: unix_ms(),
            sampled_monotonic_ns: self.monotonic_ns(),
            clock_epoch: self.clock_epoch.clone(),
            metrics: self.metrics(),
            clock_quality: "observed".to_string(),
        }
    }

    fn clock(&self) -> ClockSample {
        let now = unix_ms();
        ClockSample {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: self.boot_id.clone(),
            clock_epoch: self.clock_epoch.clone(),
            receive_unix_ms: now,
            send_unix_ms: now,
            receive_monotonic_ns: self.monotonic_ns(),
            send_monotonic_ns: self.monotonic_ns(),
            quality: "observed".to_string(),
        }
    }
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().try_into().unwrap_or(i64::MAX))
        .unwrap_or(0)
}

struct JournalReader {
    store: Arc<JournalStore<RealFs>>,
}

impl TaskJournalReader for JournalReader {
    fn query(
        &self,
        controller_key: &[u8; 32],
        query: &TaskQuery,
    ) -> Result<Option<rsetup_protocol::wire::TaskRecord>, BusinessStatus> {
        match self.store.get(controller_key, &query.sub_task_id) {
            Ok(record) => Ok(record.map(|r| r.to_wire())),
            Err(_) => Err(BusinessStatus::InternalError),
        }
    }
}

struct JournalEpoch {
    epoch: String,
}

impl JournalEpochSource for JournalEpoch {
    fn journal_epoch(&self) -> String {
        self.epoch.clone()
    }
}

fn open_agent(
    options: &Options,
) -> Result<BoardAgent<SystemReadOnlySource, JournalReader, JournalEpoch>, String> {
    std::fs::create_dir_all(&options.journal_dir)
        .map_err(|e| format!("cannot create journal directory: {e}"))?;
    let store = JournalStore::open(&options.journal_dir, RealFs)
        .map_err(|e| format!("cannot open journal: {e}"))?;
    let epoch = store.journal_epoch().to_string();
    let store = Arc::new(store);
    Ok(BoardAgent::new(
        SystemReadOnlySource::new(epoch.clone()),
        JournalReader { store },
        JournalEpoch { epoch },
    ))
}

fn business_response_json(response: &BusinessResponse) -> serde_json::Value {
    match response {
        BusinessResponse::Capabilities(caps) => serde_json::json!({
            "schema_version": caps.schema_version,
            "boot_id": caps.boot_id,
            "agent_version": caps.agent_version,
            "supported_actions": caps.supported_actions,
            "durable_task_journal": caps.durable_task_journal,
            "max_status_bytes": caps.max_status_bytes,
            "journal_epoch": caps.journal_epoch,
        }),
        BusinessResponse::DeviceStatus(status) => serde_json::json!({
            "schema_version": status.schema_version,
            "boot_id": status.boot_id,
            "agent_epoch": status.agent_epoch,
            "sample_seq": status.sample_seq,
            "sampled_unix_ms": status.sampled_unix_ms,
            "sampled_monotonic_ns": status.sampled_monotonic_ns,
            "clock_epoch": status.clock_epoch,
            "clock_quality": status.clock_quality,
            "metrics": status.metrics.iter().map(|m| serde_json::json!({
                "key": m.key,
                "value": m.value,
                "unit": m.unit,
                "quality": m.quality,
            })).collect::<Vec<_>>(),
        }),
        BusinessResponse::ClockSample(clock) => serde_json::json!({
            "schema_version": clock.schema_version,
            "boot_id": clock.boot_id,
            "clock_epoch": clock.clock_epoch,
            "receive_unix_ms": clock.receive_unix_ms,
            "send_unix_ms": clock.send_unix_ms,
            "receive_monotonic_ns": clock.receive_monotonic_ns,
            "send_monotonic_ns": clock.send_monotonic_ns,
            "quality": clock.quality,
        }),
        BusinessResponse::TaskRecord(record) => serde_json::json!({
            "schema_version": record.schema_version,
            "sub_task_id": record.sub_task_id,
            "journal_epoch": record.journal_epoch,
            "record_version": record.record_version,
            "state": record.state,
            "pre_boot_id": record.pre_boot_id,
            "observed_boot_id": record.observed_boot_id,
            "reason_code": record.reason_code,
            "updated_unix_ms": record.updated_unix_ms,
            "evidence_kind": record.evidence_kind,
        }),
        BusinessResponse::RebootTicket(ticket) => serde_json::json!({
            "schema_version": ticket.schema_version,
            "sub_task_id": ticket.sub_task_id,
            "boot_id": ticket.boot_id,
            "valid_for_ms": ticket.valid_for_ms,
        }),
        BusinessResponse::Indeterminate => serde_json::json!({"indeterminate": true}),
    }
}

fn call_read_only(
    agent: &BoardAgent<SystemReadOnlySource, JournalReader, JournalEpoch>,
    peer: &PeerContext,
    action: &str,
) -> Result<serde_json::Value, BusinessStatus> {
    let payload = BusinessPayload::Query(Query {
        schema_version: BUSINESS_SCHEMA_VERSION,
    });
    let response = agent.handle_typed_action(peer, action, &payload)?;
    Ok(business_response_json(&response))
}

fn print_once(
    agent: &BoardAgent<SystemReadOnlySource, JournalReader, JournalEpoch>,
    peer: &PeerContext,
) -> Result<(), String> {
    let mut out = serde_json::Map::new();
    for action in [
        "device.capabilities.get",
        "device.status.get",
        "device.clock.get",
    ] {
        let data = call_read_only(agent, peer, action)
            .map_err(|status| format!("{action} failed: {status}"))?;
        out.insert(action.to_string(), data);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::Value::Object(out))
            .map_err(|e| format!("json encode failed: {e}"))?
    );
    Ok(())
}

fn handle_request(
    agent: &BoardAgent<SystemReadOnlySource, JournalReader, JournalEpoch>,
    peer: &PeerContext,
    line: &str,
) -> String {
    let request: serde_json::Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return error_json("INVALID_REQUEST", None),
    };
    let Some(action) = request.get("action").and_then(|v| v.as_str()) else {
        return error_json("INVALID_REQUEST", None);
    };
    let result = match action {
        "device.capabilities.get" | "device.status.get" | "device.clock.get" => {
            call_read_only(agent, peer, action)
        }
        "device.task.get" => {
            let Some(sub_task_id) = request.get("sub_task_id").and_then(|v| v.as_str()) else {
                return error_json("INVALID_REQUEST", Some(action));
            };
            let payload = BusinessPayload::TaskQuery(TaskQuery {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: sub_task_id.to_string(),
            });
            agent
                .handle_typed_action(peer, action, &payload)
                .map(|response| business_response_json(&response))
        }
        "device.reboot.prepare" | "device.reboot.execute" => {
            // 本地测试入口绝不触发真实主机重启；即便未来注入 dispatcher 也不在此打开。
            Err(BusinessStatus::CapabilityUnavailable)
        }
        _ => Err(BusinessStatus::UnsupportedAction),
    };
    match result {
        Ok(data) => serde_json::json!({
            "status": "ok",
            "action": action,
            "data": data,
        })
        .to_string(),
        Err(status) => error_json_with_status(action, status),
    }
}

fn error_json(code: &str, action: Option<&str>) -> String {
    serde_json::json!({
        "status": "error",
        "action": action,
        "error": code,
    })
    .to_string()
}

fn error_json_with_status(action: &str, status: BusinessStatus) -> String {
    serde_json::json!({
        "status": "error",
        "action": action,
        "code": status as i32,
        "error": status.to_string(),
    })
    .to_string()
}

fn serve(
    options: &Options,
    agent: BoardAgent<SystemReadOnlySource, JournalReader, JournalEpoch>,
) -> Result<(), String> {
    let addr: std::net::SocketAddr = options
        .listen
        .parse()
        .map_err(|_| "invalid --listen address".to_string())?;
    if !addr.ip().is_loopback() {
        return Err("the local test console only binds loopback addresses".to_string());
    }
    let listener =
        TcpListener::bind(addr).map_err(|e| format!("cannot bind {}: {e}", options.listen))?;
    println!("rsetup-board-agent listening on {}", options.listen);
    println!("test-only JSON-lines protocol; not the production encrypted tunnel");
    let agent = Arc::new(agent);
    let peer = PeerContext {
        authenticated_controller_key: options.controller_key,
    };
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let agent = Arc::clone(&agent);
        let peer = peer.clone();
        std::thread::spawn(move || {
            let _ = handle_connection(stream, &agent, &peer);
        });
    }
    Ok(())
}

fn handle_connection(
    stream: TcpStream,
    agent: &BoardAgent<SystemReadOnlySource, JournalReader, JournalEpoch>,
    peer: &PeerContext,
) -> std::io::Result<()> {
    let mut writer = stream.try_clone()?;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => break,
        };
        if line.len() > MAX_REQUEST_LINE {
            let _ = writeln!(writer, "{}", error_json("REQUEST_TOO_LARGE", None));
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = handle_request(agent, peer, trimmed);
        if writeln!(writer, "{response}").is_err() {
            break;
        }
    }
    Ok(())
}

fn watch(
    agent: &BoardAgent<SystemReadOnlySource, JournalReader, JournalEpoch>,
    peer: &PeerContext,
    interval: Duration,
) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "rsetup-board-agent watch mode (interval={}s, test-only read-only source)",
        interval.as_secs()
    )
    .map_err(|_| "stdout closed".to_string())?;
    loop {
        let data = call_read_only(agent, peer, "device.status.get")
            .map_err(|status| format!("device.status.get failed: {status}"))?;
        if writeln!(out, "{}", serde_json::json!({"status": "ok", "data": data})).is_err() {
            // 下游关闭（例如 `| head`）是正常终止，不 panic。
            return Ok(());
        }
        std::thread::sleep(interval);
    }
}

fn main() {
    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    if !options.once && !options.watch && !options.loopback {
        eprintln!("{}", usage());
        std::process::exit(2);
    }
    let agent = match open_agent(&options) {
        Ok(agent) => agent,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };
    let peer = PeerContext {
        authenticated_controller_key: options.controller_key,
    };
    let outcome = if options.once {
        print_once(&agent, &peer)
    } else if options.watch {
        watch(&agent, &peer, options.interval)
    } else {
        serve(&options, agent)
    };
    if let Err(message) = outcome {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsetup_protocol::wire::BusinessMessage;

    #[test]
    fn usage_lists_all_three_modes() {
        let text = usage();
        for mode in ["--once", "--watch", "--loopback-test-server"] {
            assert!(text.contains(mode));
        }
        assert!(text.contains("never performs a real host reboot"));
    }

    #[test]
    fn metric_reading_fails_closed_to_no_value() {
        // 解析失败路径由 Option 表达；不得把未知回填 0。
        assert!(read_loadavg().is_some());
        assert!(read_uptime_secs().is_some());
        assert!(read_mem_used_percent().is_some());
    }

    #[test]
    fn system_source_produces_valid_typed_dtos() {
        let source = SystemReadOnlySource::new(uuid::Uuid::new_v4().to_string());
        let caps = source.capabilities();
        caps.validate().unwrap();
        let status = source.status();
        status.validate().unwrap();
        let clock = source.clock();
        clock.validate().unwrap();
        assert_eq!(status.sample_seq, 1);
        assert_eq!(source.status().sample_seq, 2);
    }

    #[test]
    fn error_json_is_stable_and_contains_no_payload() {
        let error = error_json_with_status("device.task.get", BusinessStatus::TaskNotFound);
        assert!(error.contains("\"status\":\"error\""));
        assert!(error.contains("device.task.get"));
        assert!(!error.contains("controller"));
    }

    #[test]
    fn request_handler_rejects_unknown_and_never_reboots() {
        let options = Options {
            once: true,
            watch: false,
            loopback: false,
            listen: "127.0.0.1:0".to_string(),
            journal_dir: std::env::temp_dir()
                .join(format!("rsetup-board-agent-test-{}", uuid::Uuid::new_v4())),
            interval: Duration::from_secs(1),
            controller_key: [7u8; 32],
        };
        let agent = open_agent(&options).unwrap();
        let peer = PeerContext {
            authenticated_controller_key: options.controller_key,
        };
        let unknown = handle_request(&agent, &peer, r#"{"action":"device.shutdown"}"#);
        assert!(unknown.contains("UNSUPPORTED_ACTION") || unknown.contains("unsupported action"));
        let reboot = handle_request(&agent, &peer, r#"{"action":"device.reboot.execute"}"#);
        assert!(reboot.contains("capability unavailable"));
        let _ = std::fs::remove_dir_all(&options.journal_dir);
    }
}
