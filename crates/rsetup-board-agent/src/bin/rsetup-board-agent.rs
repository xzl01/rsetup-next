//! 板端代理 amd64 可执行入口。
//!
//! 模式：
//! - `--connect`：**生产模式**。从环境读取 validated 连接配置（显式中控公钥 pin，
//!   无 TOFU），载入一次身份密钥（0600），用真实 `RealFs` journal 与真实 /proc
//!   只读来源装配 `BoardAgent`，然后进入 `rsetup_board_agent::connect` 的连接循环：
//!   TCP → v3 客户端握手 → 加密记录层 → 会话驱动（业务 handler、退避重连、
//!   kick/revoke 分流、优雅排空）。退出码：正常停止/永久停止 0（因此
//!   `Restart=on-failure` 不会绕过停止语义），本端配置或 journal 故障 78。
//! - `--once`：执行三条只读动作并输出 JSON（本机验收）。
//! - `--watch`：周期性输出最新 `device.status.get` 样本（本机探针）。
//! - `--loopback-test-server`：回环地址上的**仅测试用**逐行 JSON 控制台，
//!   用于本机验证白名单、DTO 校验、journal 查询与错误映射。
//!
//! 后三个模式是**本地非生产**通道，不构成与中控互通的证据。本入口在任何模式下
//! 都不触发真实主机重启：`device.reboot.*` 需要由人工接线真实 dispatcher，
//! 未接线时能力申报与执行都返回 1004。

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rsetup_board_agent::connect::{ConnectEvent, SharedAgentFactory};
use rsetup_board_agent::connect::{
    Observer, ReconnectPolicy, RunOutcome, Shutdown, TunnelConnector, UnauthorizedState,
    board_connect_config_from_env, descriptor_from_env, run_board_connection, state_dir_of,
};
use rsetup_board_agent::dispatch::{
    BoardAgent, BusinessResponse, BusinessStatus, JournalEpochSource, TaskJournalReader,
};
use rsetup_board_agent::host_source::SystemReadOnlySource;
use rsetup_board_agent::journal::{JournalStore, RealFs};
use rsetup_board_agent::read_only::PeerContext;
use rsetup_board_agent::session_handler::BusinessDispatch;
use rsetup_protocol::handshake_msg::DeviceDescriptor;
use rsetup_protocol::wire::{BUSINESS_SCHEMA_VERSION, BusinessPayload, Query, TaskQuery};
use rsetup_tunnel::identity::IdentityKey;

const DEFAULT_JOURNAL_DIR: &str = "/tmp/rsetup-board-agent-journal";
const DEFAULT_INTERVAL_SECS: u64 = 10;
const MAX_REQUEST_LINE: usize = 4096;

struct Options {
    connect: bool,
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
       --connect                      production mode: dial the controller from env config\n\
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
     --connect reads RSETUP_CONTROLLER_ADDR, RSETUP_CONTROLLER_PUBLIC_KEYS (explicit pins,\n\
     no TOFU), RSETUP_BOARD_KEY_FILE and RSETUP_BOARD_JOURNAL_DIR, and fails closed when a\n\
     pin is missing or a value is unusable. Every mode never performs a real host reboot."
        .to_string()
}

fn parse_args() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut connect = false;
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
            "--connect" => connect = true,
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
    let modes = connect as u8 + once as u8 + watch as u8 + loopback as u8;
    if modes != 1 {
        return Err("exactly one of --connect, --once, --watch or --loopback-test-server                     is required"
            .to_string());
    }
    Ok(Options {
        connect,
        once,
        watch,
        loopback,
        listen,
        journal_dir,
        interval,
        controller_key,
    })
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
    // boot_id 只来自真实内核读数；读不到就是失败，绝不伪造。
    let source = SystemReadOnlySource::from_system(epoch.clone(), false, true)
        .map_err(|e| format!("cannot read host identity: {e}"))?;
    Ok(BoardAgent::new(
        source,
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
    if options.connect {
        // 生产模式：配置、身份、journal 全部 fail-closed，不进入本地 JSON 通道。
        match run_connect() {
            Ok(code) => std::process::exit(code),
            Err((code, message)) => {
                eprintln!("{message}");
                std::process::exit(code);
            }
        }
    }
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

/// 生产模式装配与运行。返回进程退出码（0 = 正常停止/永久停止，78 = 本端故障）。
///
/// 任何一步失败都只打印**去敏**原因：不打印地址、公钥、路径与环境值内容。
fn run_connect() -> Result<i32, (i32, String)> {
    let config = board_connect_config_from_env()
        .map_err(|_| (78, "connection configuration is invalid".to_string()))?;
    let descriptor: DeviceDescriptor =
        descriptor_from_env().map_err(|_| (78, "device descriptor is invalid".to_string()))?;
    // 身份密钥：**启动期载入一次**（0600/owner/symlink 由 IdentityKey 校验），
    // 每条 connection 的 random 与 X25519 临时密钥由握手库每次重新生成。
    let identity = IdentityKey::load_or_generate(&config.identity_file)
        .map_err(|_| (78, "board identity key is unavailable".to_string()))?;
    // journal 必须是真实目录（不接受符号链接），持久失败绝不降级为内存假 durable。
    let metadata = std::fs::symlink_metadata(&config.journal_dir)
        .map_err(|_| (78, "journal directory is unavailable".to_string()))?;
    if !metadata.is_dir() {
        return Err((78, "journal directory is not a real directory".to_string()));
    }
    let store = JournalStore::open(&config.journal_dir, RealFs)
        .map_err(|_| (78, "journal cannot be opened or recovered".to_string()))?;
    let epoch = store.journal_epoch().to_string();
    let store = Arc::new(store);
    let source = SystemReadOnlySource::from_system(epoch.clone(), false, true)
        .map_err(|_| (78, "host identity is unavailable".to_string()))?;
    let agent = BoardAgent::new(
        source,
        JournalReader {
            store: Arc::clone(&store),
        },
        JournalEpoch { epoch },
    );
    let dispatch: Arc<dyn BusinessDispatch> = Arc::new(agent);
    let connector = TunnelConnector::new(
        config.clone(),
        Arc::new(identity),
        descriptor,
        Arc::new(SharedAgentFactory::new(dispatch)),
    )
    .with_unauthorized_state(UnauthorizedState::in_state_dir(state_dir_of(
        &config.identity_file,
    )));

    let observer: Observer = Arc::new(|event: ConnectEvent| {
        // 只打类别与尝试次数：无地址、无公钥、无路径。
        eprintln!(
            "board-connect event={:?} attempt={} stop={:?}",
            event.kind,
            event.attempt,
            event.stop.map(|stop| stop.label())
        );
    });
    let policy = ReconnectPolicy::system();
    let shutdown = Shutdown::new();
    {
        let shutdown = shutdown.clone();
        // SIGTERM（systemd stop）与 SIGINT（前台调试）走同一条优雅排空路径。
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_) => return,
            };
            runtime.block_on(async {
                use tokio::signal::unix::{SignalKind, signal};
                let Ok(mut term) = signal(SignalKind::terminate()) else {
                    return;
                };
                let Ok(mut int) = signal(SignalKind::interrupt()) else {
                    return;
                };
                tokio::select! {
                    _ = term.recv() => {}
                    _ = int.recv() => {}
                }
                shutdown.signal();
            });
        });
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .map_err(|_| (78, "cannot start runtime".to_string()))?;
    let outcome = runtime.block_on(run_board_connection(
        Arc::new(connector),
        policy,
        shutdown,
        observer,
    ));
    match outcome {
        RunOutcome::Shutdown => {
            eprintln!("board-connect stopped by shutdown signal");
        }
        RunOutcome::Stopped {
            stop,
            state_written,
        } => {
            // 合法吊销/拒绝：不再自动重连；未授权状态未落盘必须显式喊人。
            eprintln!(
                "board-connect permanently stopped ({}), unauthorized state written={}",
                stop.label(),
                state_written
            );
        }
        RunOutcome::Fault(reason) => {
            eprintln!("board-connect fault: {reason:?}");
        }
    }
    Ok(outcome.exit_code())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsetup_board_agent::read_only::ReadOnlySource;
    use rsetup_protocol::wire::validate_dto;

    #[test]
    fn usage_lists_every_mode_including_production_connect() {
        let text = usage();
        for mode in ["--connect", "--once", "--watch", "--loopback-test-server"] {
            assert!(text.contains(mode), "usage 缺少 {mode}");
        }
        assert!(text.contains("never performs a real host reboot"));
        // 生产模式必须写明显式 pin 且缺 pin 直接失败关闭。
        assert!(text.contains("no TOFU"));
    }

    #[test]
    fn missing_mode_is_rejected_and_names_the_production_mode() {
        // 测试进程的 argv 不带模式标志：必须报「恰好一个模式」并列出 --connect。
        let message = match parse_args() {
            Ok(_) => panic!("无模式必须失败"),
            Err(message) => message,
        };
        assert!(message.contains("exactly one of --connect"), "{message}");
    }

    #[test]
    fn metric_readings_come_from_the_real_system_or_are_absent() {
        // 读数失败时该指标**缺席**，绝不回填 0（host_source 的失败路径）。
        let metrics = rsetup_board_agent::host_source::read_metrics();
        for metric in &metrics {
            assert!(metric.value.is_some() || metric.quality == "unavailable");
        }
    }

    #[test]
    fn system_source_produces_valid_typed_dtos() {
        // 生产入口的只读来源：boot_id 必须真实可读，DTO 必须通过 wire 自检。
        let source =
            SystemReadOnlySource::from_system(uuid::Uuid::new_v4().to_string(), false, true)
                .expect("本机内核 boot_id 必须真实可读");
        let caps = source.capabilities();
        validate_dto(&caps).unwrap();
        // 未接线 reboot dispatcher：能力清单不得虚报可执行重启。
        assert!(
            !caps
                .supported_actions
                .iter()
                .any(|action| action.starts_with("device.reboot."))
        );
        let status = source.status();
        validate_dto(&status).unwrap();
        validate_dto(&source.clock()).unwrap();
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
            connect: false,
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
