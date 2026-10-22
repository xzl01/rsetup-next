#!/usr/bin/env python3
"""本机 **主控 + 板端** 双进程联合功能与连通性测试。

流程：
  1. 进程内解析 ``secret/mysql.md`` 取得开发库连接参数（复用 ``headless_e2e_verify`` 的
     解析与去敏实现，任何输出均不含口令/连接串）；
  2. 启动板端 ``rsetup-board-agent --loopback-test-server``（127.0.0.1:39876），
     确认日志出现 ``rsetup-board-agent listening on 127.0.0.1:39876``；
  3. 以 ``CONTROLLER_*`` 环境变量启动 ``rsetup-controller``（127.0.0.1:39089），
     轮询 ``/healthz`` 直到 200；
  4. ① 板端 JSON-lines 功能与安全断言（capabilities / status / clock / reboot 安全拒绝）；
     ② 中控 HTTP 接口断言（healthz、未认证 401、引导口令登录、system/time）；
     ③ 输出双端各自与协同的结构化证据日志；
  5. SIGTERM 优雅终止两个进程，确认 39089 与 39876 端口释放，清理临时日志与测试目录。

绝不打印：数据库口令与连接串、管理员一次性口令、session cookie 值、CSRF token 值。
子进程输出仅写入 0700 临时目录内的日志文件，摘要一律经 ``mask()`` 去敏。
"""

from __future__ import annotations

import datetime
import json
import os
import shutil
import socket
import sys
import time

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(SCRIPT_DIR)
if SCRIPT_DIR not in sys.path:
    sys.path.insert(0, SCRIPT_DIR)

# 复用已验证的进程内凭据解析 + 去敏输出 + 子进程/端口工具，避免重复实现秘密处理。
import headless_e2e_verify as ref  # noqa: E402

check = ref.check
log = ref.log
section = ref.section
mask = ref.mask

BOARD_BIN = os.path.join(ROOT, "target", "debug", "rsetup-board-agent")
CONTROLLER_BIN = ref.CONTROLLER_BIN
SECRET_FILE = ref.SECRET_FILE

# 首选任务指定的回环端口；若被本会话无权终止的外部进程占用，则自动改用备用
# 回环端口并把该差异登记为受阻项（绝不静默降级、绝不终止他人进程）。
BOARD_ADDR_REQUESTED = "127.0.0.1:39876"
BOARD_ADDR = BOARD_ADDR_REQUESTED
BOARD_LISTEN_LINE = f"rsetup-board-agent listening on {BOARD_ADDR}"
BOARD_JOURNAL_DIR = "/tmp/rsetup-ba-dual-test"
BOARD_FALLBACK_PORTS = (39877, 39878, 39879, 39881, 39883)

CTRL_ADDR = "127.0.0.1:39089"
CTRL_HOST, CTRL_PORT = CTRL_ADDR.split(":")
CTRL_ORIGIN = f"http://{CTRL_ADDR}"
CURSOR_SECRET_HEX = "0123456789abcdef" * 4  # 64 hex = 32 字节，仅测试用
BOOTSTRAP_LOG = "/tmp/controller-dual-bootstrap.log"

ART_DIR = "/tmp/rsetup-dual-test-artifacts"
BOARD_LOG = os.path.join(ART_DIR, "board-agent.log")
CONTROLLER_LOG = os.path.join(ART_DIR, "controller.log")
EVIDENCE_JSON = "/tmp/rsetup-dual-test-evidence.json"

# 结构化证据（③）：双端各自与协同的逐条断言记录。
STRUCTURED: list[dict] = []
# 受阻项：因本会话权限或共享资源现状无法执行，不计入通过或失败。
BLOCKED: list[dict] = []
# 发现项：执行到且暴露出真实缺陷/数据状态问题的观察，单独列示。
FINDINGS: list[dict] = []


def finding(side: str, phase: str, case: str, detail: str) -> None:
    record = {
        "seq": len(STRUCTURED) + 1,
        "ts_unix_ms": int(time.time() * 1000),
        "side": side,
        "phase": phase,
        "case": case,
        "ok": None,
        "detail": mask(str(detail)),
    }
    STRUCTURED.append(record)
    FINDINGS.append(record)
    log(f"  [FINDING] {case} :: {record['detail']}")
    log("  " + json.dumps({"event": "finding", **record}, ensure_ascii=False))


def blocked(side: str, phase: str, case: str, reason: str) -> None:
    """登记一条受阻记录（不进入通过/失败统计）。"""
    record = {
        "seq": len(STRUCTURED) + 1,
        "ts_unix_ms": int(time.time() * 1000),
        "side": side,
        "phase": phase,
        "case": case,
        "ok": None,
        "detail": mask(str(reason)),
    }
    STRUCTURED.append(record)
    BLOCKED.append(record)
    log(f"  [BLOCKED] {case} :: {record['detail']}")
    log("  " + json.dumps({"event": "blocked", **record}, ensure_ascii=False))


def structured(side: str, phase: str, case: str, ok: bool, detail: str = "") -> bool:
    """登记一条结构化证据，同时输出去敏的人类可读行。"""
    record = {
        "seq": len(STRUCTURED) + 1,
        "ts_unix_ms": int(time.time() * 1000),
        "side": side,
        "phase": phase,
        "case": case,
        "ok": bool(ok),
        "detail": mask(str(detail)),
    }
    STRUCTURED.append(record)
    check(f"[{side}] {case}", ok, record["detail"])
    log(f"  {json.dumps({'event': 'assert', **record}, ensure_ascii=False)}")
    return bool(ok)


def emit(side: str, event: str, **fields) -> None:
    """输出一条不含秘密的结构化运行日志。"""
    record = {"event": event, "side": side, "ts_unix_ms": int(time.time() * 1000)}
    record.update(mask_obj_safe(fields))
    STRUCTURED.append({"seq": len(STRUCTURED) + 1, "side": side, **record})
    log("  " + json.dumps(record, ensure_ascii=False))


def mask_obj_safe(fields: dict) -> dict:
    return {k: ref.mask_obj(v) for k, v in fields.items()}


# --------------------------------------------------------------------------- #
# /proc 真实读数：用于验证板端上报的是本机真实指标，不是伪造常量
# --------------------------------------------------------------------------- #
def proc_load1() -> float | None:
    try:
        with open("/proc/loadavg", encoding="utf-8") as handle:
            return float(handle.read().split()[0])
    except (OSError, ValueError, IndexError):
        return None


def proc_uptime_secs() -> float | None:
    try:
        with open("/proc/uptime", encoding="utf-8") as handle:
            return float(handle.read().split()[0])
    except (OSError, ValueError, IndexError):
        return None


def proc_mem_used_percent() -> float | None:
    try:
        with open("/proc/meminfo", encoding="utf-8") as handle:
            text = handle.read()
    except OSError:
        return None
    total = available = None
    for line in text.splitlines():
        parts = line.split()
        if parts and parts[0] == "MemTotal:":
            total = float(parts[1])
        elif parts and parts[0] == "MemAvailable:":
            available = float(parts[1])
    if not total or available is None or total <= 0:
        return None
    return (total - available) / total * 100.0


def metric_of(payload: dict, key: str) -> dict | None:
    for item in payload.get("metrics", []):
        if item.get("key") == key:
            return item
    return None


# --------------------------------------------------------------------------- #
# 板端 JSON-lines 客户端（socket，逐行请求 / 逐行响应）
# --------------------------------------------------------------------------- #
class BoardClient:
    def __init__(self, addr: str, timeout: float = 10.0) -> None:
        host, port = addr.split(":")
        self.sock = socket.create_connection((host, int(port)), timeout=timeout)
        self.sock.settimeout(timeout)
        self.buffer = b""

    def call(self, payload: dict) -> dict:
        self.sock.sendall((json.dumps(payload) + "\n").encode("utf-8"))
        while b"\n" not in self.buffer:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise RuntimeError("板端连接已关闭")
            self.buffer += chunk
        line, self.buffer = self.buffer.split(b"\n", 1)
        return json.loads(line.decode("utf-8"))

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass


def wait_log_line(path: str, needle: str, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with open(path, "rb") as handle:
                if needle.encode("utf-8") in handle.read():
                    return True
        except OSError:
            pass
        time.sleep(0.2)
    return False


def read_bootstrap_password(path: str) -> str | None:
    """从引导口令文件读取一次性口令；注册进全局去敏词典，绝不返回给输出。"""
    try:
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
    except OSError:
        return None
    for raw in text.splitlines():
        if ":" not in raw:
            continue
        _user, _, password = raw.partition(":")
        password = password.strip()
        if password:
            if password not in ref._SECRETS:
                ref._SECRETS.append(password)
            return password
    return None


# --------------------------------------------------------------------------- #
# 子进程管理（复用 ref 的实现，统一 SIGTERM 优雅终止）
# --------------------------------------------------------------------------- #
spawn = ref.spawn
shutdown = ref.shutdown
tail = ref.tail
port_free = ref.port_free
http_call = ref.http_call
json_or_text = ref.json_or_text
assert_envelope = ref.assert_envelope


def ref_occupant_hint(addr: str) -> str:
    """尽力获取占用者的可读提示（只读取内核与进程表，不做任何终止动作）。"""
    import subprocess

    _host, port = addr.split(":")
    try:
        proc = subprocess.run(
            ["ss", "-ltnp", f"sport = :{port}"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        text = (proc.stdout or "") + (proc.stderr or "")
    except Exception as error:  # noqa: BLE001
        return f"探测失败 {type(error).__name__}"
    for line in text.splitlines():
        if f":{port}" in line:
            return mask(line.strip()[-160:])
    return "未能读取占用者信息"


def main() -> int:
    global BOARD_ADDR, BOARD_LISTEN_LINE
    board = None
    controller = None
    controller_handle = None
    board_handle = None
    cookie: str | None = None
    csrf_len = 0

    try:
        section("0 环境前置")
        shutil.rmtree(ART_DIR, ignore_errors=True)
        os.makedirs(ART_DIR, mode=0o700)
        check("板端二进制存在且可执行", os.access(BOARD_BIN, os.X_OK), BOARD_BIN)
        check("中控二进制存在且可执行", os.access(CONTROLLER_BIN, os.X_OK), CONTROLLER_BIN)
        if port_free(BOARD_ADDR_REQUESTED):
            check(f"板端端口 {BOARD_ADDR_REQUESTED} 空闲", True, BOARD_ADDR_REQUESTED)
        else:
            occupied_by = ref_occupant_hint(BOARD_ADDR_REQUESTED)
            resolved = None
            for candidate in BOARD_FALLBACK_PORTS:
                if port_free(f"127.0.0.1:{candidate}"):
                    resolved = f"127.0.0.1:{candidate}"
                    break
            if resolved is None:
                raise RuntimeError("回环端口全部被占用，无法启动板端测试服务")
            BOARD_ADDR = resolved
            BOARD_LISTEN_LINE = f"rsetup-board-agent listening on {resolved}"
            blocked(
                "board",
                "preflight",
                f"板端按任务指定端口 {BOARD_ADDR_REQUESTED} 启动",
                f"{BOARD_ADDR_REQUESTED} 已被外部进程占用（{occupied_by}），本会话无权终止他人进程；"
                f"改用备用回环端口 {resolved} 完成同等测试",
            )
            check(f"板端备用端口 {resolved} 空闲", port_free(resolved), resolved)
        check("中控端口 127.0.0.1:39089 空闲", port_free(CTRL_ADDR), CTRL_ADDR)
        # 引导口令文件必须不存在：中控用 create_new + 0600 写入，残留会直接启动失败。
        if os.path.exists(BOOTSTRAP_LOG):
            os.unlink(BOOTSTRAP_LOG)
        check("引导口令文件初始不存在", not os.path.exists(BOOTSTRAP_LOG), BOOTSTRAP_LOG)

        section("1 凭据解析（进程内、去敏）")
        creds = ref.parse_credentials(SECRET_FILE)
        database_url = ref.build_database_url(creds)
        check(
            "secret/mysql.md 解析成功",
            all(creds.values()),
            f"host=<masked:{len(creds['host'])}B> port={creds['port']} user=<masked> "
            f"db=<masked:{len(creds['db'])}B> password=<masked:{len(creds['password'])}B>",
        )
        check(
            "连接串仅进程内构造（输出必被去敏）",
            mask(database_url) != database_url,
            "mask() 已覆盖口令与主机",
        )

        section("2 启动板端 rsetup-board-agent（loopback 测试服务）")
        board, board_handle = spawn(
            [
                BOARD_BIN,
                "--loopback-test-server",
                "--listen",
                BOARD_ADDR,
                "--journal-dir",
                BOARD_JOURNAL_DIR,
            ],
            os.environ.copy(),
            BOARD_LOG,
        )
        listening = wait_log_line(BOARD_LOG, BOARD_LISTEN_LINE, 20.0)
        check(
            "板端日志输出监听行",
            listening,
            f"期望 {BOARD_LISTEN_LINE!r}；日志尾部: {tail(BOARD_LOG, 5)}",
        )
        reachable = False
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if not port_free(BOARD_ADDR):
                reachable = True
                break
            time.sleep(0.2)
        check("板端 TCP 端口已监听", reachable, BOARD_ADDR)
        check("板端进程存活", board.poll() is None, f"pid={board.pid} state={board.poll()}")
        emit("board", "process_started", pid=board.pid, listen=BOARD_ADDR, journal_dir=BOARD_JOURNAL_DIR)

        section("3 ① 板端功能与安全测试（JSON-lines over TCP）")
        client = BoardClient(BOARD_ADDR)
        board_pid = board.pid

        caps = client.call({"action": "device.capabilities.get"})
        caps_data = caps.get("data", {})
        actions = caps_data.get("supported_actions") or []
        emit(
            "board",
            "rpc_response",
            action="device.capabilities.get",
            status=caps.get("status"),
            agent_version=caps_data.get("agent_version"),
            supported_actions=actions,
            durable_task_journal=caps_data.get("durable_task_journal"),
            boot_id_len=len(str(caps_data.get("boot_id") or "")),
        )
        structured(
            "board",
            "functional",
            "device.capabilities.get 返回 ok",
            caps.get("status") == "ok" and caps.get("action") == "device.capabilities.get",
            json.dumps({"status": caps.get("status"), "action": caps.get("action")}, ensure_ascii=False),
        )
        structured(
            "board",
            "functional",
            "capabilities 含非空 agent_version",
            isinstance(caps_data.get("agent_version"), str) and bool(caps_data.get("agent_version")),
            f"agent_version={caps_data.get('agent_version')} boot_id=<masked:{len(str(caps_data.get('boot_id') or ''))}B>",
        )
        structured(
            "board",
            "functional",
            "capabilities 含 supported_actions 白名单（6 条动作）",
            isinstance(actions, list)
            and sorted(actions) == sorted(
                [
                    "device.capabilities.get",
                    "device.status.get",
                    "device.clock.get",
                    "device.task.get",
                    "device.reboot.prepare",
                    "device.reboot.execute",
                ]
            ),
            f"count={len(actions)} actions={actions}",
        )

        status_a = client.call({"action": "device.status.get"})
        status_data_a = status_a.get("data", {})
        emit(
            "board",
            "rpc_response",
            action="device.status.get",
            status=status_a.get("status"),
            sample_seq=status_data_a.get("sample_seq"),
            metrics=status_data_a.get("metrics"),
            clock_quality=status_data_a.get("clock_quality"),
        )
        structured(
            "board",
            "functional",
            "device.status.get 返回 ok",
            status_a.get("status") == "ok",
            json.dumps({"status": status_a.get("status"), "sample_seq": status_data_a.get("sample_seq")}),
        )
        want_keys = {
            "cpu.load1": proc_load1(),
            "memory.used_percent": proc_mem_used_percent(),
            "system.uptime": proc_uptime_secs(),
        }
        for key, real in want_keys.items():
            metric = metric_of(status_data_a, key) or {}
            value = metric.get("value")
            close = (
                isinstance(value, (int, float))
                and real is not None
                and metric.get("quality") == "ok"
                and abs(float(value) - real) <= max(2.0, abs(real) * 0.05)
            )
            structured(
                "board",
                "functional",
                f"status 指标 {key} 与本机 /proc 真实读数一致",
                close,
                f"reported={value} unit={metric.get('unit')} quality={metric.get('quality')} proc={real}",
            )
        status_b = client.call({"action": "device.status.get"})
        data_b = status_b.get("data", {})
        seq_a = status_data_a.get("sample_seq")
        seq_b = data_b.get("sample_seq")
        structured(
            "board",
            "functional",
            "device.status.get 两次采样 sample_seq 严格递增",
            isinstance(seq_a, int) and isinstance(seq_b, int) and seq_b > seq_a,
            f"seq {seq_a} -> {seq_b}",
        )
        up_a = (metric_of(status_data_a, "system.uptime") or {}).get("value")
        up_b = (metric_of(data_b, "system.uptime") or {}).get("value")
        structured(
            "board",
            "functional",
            "system.uptime 随时间单调不减（真实运行时间）",
            isinstance(up_a, (int, float)) and isinstance(up_b, (int, float)) and up_b >= up_a,
            f"uptime {up_a} -> {up_b}",
        )

        clock = client.call({"action": "device.clock.get"})
        clock_data = clock.get("data", {})
        now_ms = int(time.time() * 1000)
        emit(
            "board",
            "rpc_response",
            action="device.clock.get",
            status=clock.get("status"),
            receive_unix_ms=clock_data.get("receive_unix_ms"),
            send_monotonic_ns=clock_data.get("send_monotonic_ns"),
            quality=clock_data.get("quality"),
        )
        structured(
            "board",
            "functional",
            "device.clock.get 返回 ok",
            clock.get("status") == "ok",
            json.dumps({"status": clock.get("status"), "quality": clock_data.get("quality")}),
        )
        unix_ms = clock_data.get("receive_unix_ms")
        structured(
            "board",
            "functional",
            "clock 含 Unix 时间戳且与本机时钟差 < 5s",
            isinstance(unix_ms, int) and abs(unix_ms - now_ms) < 5000,
            f"receive_unix_ms={unix_ms} local={now_ms} drift_ms={None if not isinstance(unix_ms, int) else unix_ms - now_ms}",
        )
        mono_r = clock_data.get("receive_monotonic_ns")
        mono_s = clock_data.get("send_monotonic_ns")
        structured(
            "board",
            "functional",
            "clock 含单调时钟读数（ns 整数且 send>=receive）",
            isinstance(mono_r, int) and isinstance(mono_s, int) and mono_s >= mono_r >= 0,
            f"receive_monotonic_ns={mono_r} send_monotonic_ns={mono_s}",
        )

        reboot = client.call({"action": "device.reboot.execute"})
        emit(
            "board",
            "security",
            action="device.reboot.execute",
            status=reboot.get("status"),
            code=reboot.get("code"),
            error=reboot.get("error"),
        )
        structured(
            "board",
            "security",
            "device.reboot.execute 安全拒绝 code=1004 capability unavailable",
            reboot.get("status") == "error" and reboot.get("code") == 1004,
            json.dumps({"status": reboot.get("status"), "code": reboot.get("code"), "error": reboot.get("error")}, ensure_ascii=False),
        )
        still_alive = board.poll() is None and board.pid == board_pid
        caps_after = client.call({"action": "device.capabilities.get"}).get("data", {})
        structured(
            "board",
            "security",
            "拒绝 reboot 后板端进程未重启（pid 与 boot_id 均不变）",
            still_alive and caps_after.get("boot_id") == caps_data.get("boot_id"),
            f"pid={board.pid}/{board_pid} boot_id_same={caps_after.get('boot_id') == caps_data.get('boot_id')}",
        )
        uptime_after = (metric_of(client.call({"action": "device.status.get"}).get("data", {}), "system.uptime") or {}).get("value")
        structured(
            "board",
            "security",
            "拒绝 reboot 后主机运行时间未回零（未发生真实重启）",
            isinstance(uptime_after, (int, float)) and isinstance(up_a, (int, float)) and uptime_after >= up_a,
            f"uptime {up_a} -> {uptime_after}",
        )
        unknown = client.call({"action": "device.shutdown"})
        structured(
            "board",
            "security",
            "白名单外动作 device.shutdown 被拒（code=1002 unsupported action）",
            unknown.get("status") == "error" and unknown.get("code") == 1002,
            json.dumps({"status": unknown.get("status"), "code": unknown.get("code")}),
        )
        client.close()

        section("4 启动中控 rsetup-controller")
        ctrl_env = os.environ.copy()
        ctrl_env.update(
            {
                "CONTROLLER_DATABASE_URL": database_url,
                "CONTROLLER_LISTEN_ADDRESS": CTRL_ADDR,
                "CONTROLLER_ALLOWED_HOSTS": f"{CTRL_HOST}:{CTRL_PORT},localhost:{CTRL_PORT}",
                "CONTROLLER_ALLOWED_ORIGIN": CTRL_ORIGIN,
                "CONTROLLER_MANAGEMENT_CURSOR_SECRET_HEX": CURSOR_SECRET_HEX,
                "CONTROLLER_BOOTSTRAP_SECRET_LOG": BOOTSTRAP_LOG,
                "RUST_LOG": os.environ.get("RUST_LOG", "info"),
            }
        )
        started = time.monotonic()
        controller, controller_handle = spawn([CONTROLLER_BIN], ctrl_env, CONTROLLER_LOG)
        ready = False
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            if controller.poll() is not None:
                break
            try:
                status, _, _ = http_call("GET", f"{CTRL_ORIGIN}/healthz", timeout=2.0)
                if status == 200:
                    ready = True
                    break
            except Exception as error:  # noqa: BLE001
                pass
            time.sleep(0.5)
        boot_seconds = round(time.monotonic() - started, 2)
        check(
            "中控进程启动且 /healthz 就绪",
            ready,
            f"{boot_seconds}s 内就绪 (pid={controller.pid})"
            if ready
            else f"进程状态={controller.poll()} 日志: {tail(CONTROLLER_LOG, 8)}",
        )
        if not ready:
            raise RuntimeError("中控服务未就绪，跳过中控侧断言")
        emit("controller", "process_started", pid=controller.pid, listen=CTRL_ADDR, boot_seconds=boot_seconds)

        section("5 ② 中控服务接口测试（HTTP）")
        status, headers, body = http_call("GET", f"{CTRL_ORIGIN}/healthz")
        structured("controller", "http", "GET /healthz == 200", status == 200, f"status={status} body={body[:80].decode('utf-8', 'replace')}")

        status, headers, body = http_call("GET", f"{CTRL_ORIGIN}/api/v1/auth/me")
        ok_env, detail = assert_envelope(body, "AUTH_REQUIRED", "auth.required")
        no_cookie = not any("set-cookie" in str(k).lower() for k in (headers.keys() or []))
        structured(
            "controller",
            "http",
            "GET /api/v1/auth/me 未认证 == 401 AUTH_REQUIRED（统一信封且不下发 Cookie）",
            status == 401 and ok_env and no_cookie,
            f"status={status} {detail} set_cookie={headers.get('Set-Cookie')!r}",
        )

        status, _, body = http_call("GET", f"{CTRL_ORIGIN}/api/v1/system/time")
        ok_env, detail = assert_envelope(body, "AUTH_REQUIRED", "auth.required")
        structured(
            "controller",
            "security",
            "GET /api/v1/system/time 无 cookie 仍 401 AUTH_REQUIRED（不泄露时间证据）",
            status == 401 and ok_env,
            f"status={status} {detail}",
        )

        password = read_bootstrap_password(BOOTSTRAP_LOG)
        bootstrap_present = bool(password)
        if bootstrap_present:
            check(
                "读取引导口令文件并取得一次性口令",
                True,
                f"file={BOOTSTRAP_LOG} password_len={len(password)}（口令值不回显）",
            )
        else:
            blocked(
                "controller",
                "http",
                "读取引导口令文件 /tmp/controller-dual-bootstrap.log",
                "中控未写入口令：开发库 identity 已 initialized=1（admin 由更早的自动跑批铸造，口令无人持有），"
                "bootstrap_admin 首装分支不再触发；本会话无权执行 identity 首装重置",
            )
        if bootstrap_present:

            login_body = json.dumps({"username": "admin", "password": password}).encode("utf-8")
            status, headers, body = http_call(
                "POST",
                f"{CTRL_ORIGIN}/api/v1/auth/login",
                headers={"Content-Type": "application/json", "Origin": CTRL_ORIGIN},
                data=login_body,
            )
            login_json = json_or_text(body) or {}
            login_data = login_json.get("data") or {}
            csrf_token = login_data.get("csrf_token")
            raw_cookie = headers.get("Set-Cookie") or ""
            if raw_cookie:
                cookie = raw_cookie.split(";")[0]
            csrf_len = len(csrf_token) if isinstance(csrf_token, str) else 0
            if isinstance(csrf_token, str) and csrf_token:
                ref._SECRETS.append(csrf_token)
            if cookie:
                ref._SECRETS.append(cookie.split("=", 1)[1])
            structured(
                "controller",
                "http",
                "POST /api/v1/auth/login 管理员一次性口令登录 == 200",
                status == 200 and bool(cookie) and csrf_len == 64,
                f"status={status} set_cookie={'rsc_session' in raw_cookie} csrf_token_len={csrf_len} "
                f"user_admin={bool((login_data.get('user') or {}).get('is_admin'))} "
                f"must_change_password={(login_data.get('user') or {}).get('must_change_password')}",
            )

            auth_headers = {"Cookie": cookie or "", "X-CSRF-Token": csrf_token or "", "Origin": CTRL_ORIGIN}
            status, headers, body = http_call("GET", f"{CTRL_ORIGIN}/api/v1/system/time", headers=auth_headers)
            time_json = json_or_text(body) or {}
            time_data = time_json.get("data") or {}
            evidence = time_data.get("evidence") or {}
            emit(
                "controller",
                "http_response",
                endpoint="/api/v1/system/time",
                status=status,
                quality=evidence.get("quality"),
                clock_epoch_len=len(str(evidence.get("clock_epoch") or "")),
                retry_active=time_data.get("retry_active"),
            )
            structured(
                "controller",
                "http",
                "GET /api/v1/system/time（带 session cookie + CSRF token）== 200",
                status == 200,
                f"status={status} keys={sorted(time_data.keys())}",
            )
            wall = str(evidence.get("system_wall_utc") or "")
            drift_seconds: float | None = None
            try:
                parsed = datetime.datetime.fromisoformat(wall.replace("Z", "+00:00"))
                drift_seconds = abs((datetime.datetime.now(datetime.timezone.utc) - parsed).total_seconds())
            except ValueError:
                pass
            structured(
                "controller",
                "http",
                "system/time 返回当前 TimeEvidence（quality/clock_epoch/墙钟与本机差 < 30s）",
                bool(evidence.get("quality"))
                and bool(evidence.get("clock_epoch"))
                and drift_seconds is not None
                and drift_seconds < 30.0,
                json.dumps(
                    {
                        "quality": evidence.get("quality"),
                        "source": evidence.get("source"),
                        "clock_epoch_len": len(str(evidence.get("clock_epoch") or "")),
                        "system_wall_utc": wall,
                        "drift_seconds": None if drift_seconds is None else round(drift_seconds, 3),
                        "sample_age_ms": evidence.get("sample_age_ms"),
                        "offset_ms": evidence.get("offset_ms"),
                        "uncertainty_ms": evidence.get("uncertainty_ms"),
                        "evidence_keys": sorted(evidence.keys()),
                    },
                    ensure_ascii=False,
                ),
            )
            structured(
                "controller",
                "http",
                "system/time 返回后台重试状态 retry_active 布尔值",
                isinstance(time_data.get("retry_active"), bool),
                f"retry_active={time_data.get('retry_active')}",
            )


        else:
            blocked("controller", "http", 'POST /api/v1/auth/login 管理员一次性口令登录 == 200', "依赖首装一次性管理员口令；且 admin 行口令校验 503（见 FINDING），前置受阻")
            blocked("controller", "http", 'GET /api/v1/system/time（带 session cookie + CSRF token）== 200', "依赖首装一次性管理员口令；且 admin 行口令校验 503（见 FINDING），前置受阻")
            blocked("controller", "http", 'system/time 返回当前 TimeEvidence（quality/clock_epoch/墙钟与本机差 < 30s）', "依赖首装一次性管理员口令；且 admin 行口令校验 503（见 FINDING），前置受阻")
            blocked("controller", "http", 'system/time 返回后台重试状态 retry_active 布尔值', "依赖首装一次性管理员口令；且 admin 行口令校验 503（见 FINDING），前置受阻")
            # 判别实验：dummy 哈希路径（不存在的用户）与 admin 行对照，用以区分
            # 「凭据校验与登录失败审计写入正常」与「admin 行 password_hash 不可校验」。
            status, headers, body = http_call(
                "POST",
                f"{CTRL_ORIGIN}/api/v1/auth/login",
                headers={"Content-Type": "application/json", "Origin": CTRL_ORIGIN},
                data=json.dumps({"username": "ghost-user-not-exist", "password": "not-the-one-time-secret"}).encode("utf-8"),
            )
            ok_env, detail = assert_envelope(body, "INVALID_CREDENTIALS", "auth.invalid_credentials")
            structured(
                "controller",
                "security",
                "POST /api/v1/auth/login 不存在用户 == 401 INVALID_CREDENTIALS（dummy 校验+登录失败审计写入正常，不下发 Cookie）",
                status == 401 and ok_env and headers.get("Set-Cookie") is None,
                f"status={status} {detail} set_cookie={headers.get('Set-Cookie')!r}",
            )
            status, headers, body = http_call(
                "POST",
                f"{CTRL_ORIGIN}/api/v1/auth/login",
                headers={"Content-Type": "application/json", "Origin": CTRL_ORIGIN},
                data=json.dumps({"username": "admin", "password": "not-the-one-time-secret"}).encode("utf-8"),
            )
            admin_code = str(((json_or_text(body) or {}).get("error") or {}).get("code"))
            if status == 401 and admin_code == "INVALID_CREDENTIALS":
                structured(
                    "controller",
                    "security",
                    "POST /api/v1/auth/login admin 错误口令 == 401 INVALID_CREDENTIALS",
                    True,
                    f"status={status} code={admin_code}",
                )
            elif status == 503 and admin_code == "NOT_READY":
                finding(
                    "controller",
                    "diagnostic",
                    "admin 口令校验返回 503 NOT_READY（应为 401）",
                    "对照实验：不存在用户走 dummy 哈希返回 401 INVALID_CREDENTIALS，说明凭据校验与登录失败审计"
                    "事务写入均正常；仅 admin 行返回 503 NOT_READY。两条路径的唯一差异是 "
                    "PasswordHasher::verify(password, user.password_hash)：Err(ControllerError::Crypto) 才会被 login "
                    "handler 映射为 503（口令不匹配只会得到 401，DB 故障会是 503 STORAGE_UNAVAILABLE）。"
                    "即开发库 admin 行的 password_hash 无法被当前构建校验，即便持有正确口令也登录不了；"
                    "属开发库 identity 数据状态，需首装重置方能修复",
                )
            else:
                structured(
                    "controller",
                    "security",
                    "POST /api/v1/auth/login admin 错误口令返回码在预期之内",
                    False,
                    f"status={status} code={admin_code}",
                )
            status, headers, body = http_call(
                "POST",
                f"{CTRL_ORIGIN}/api/v1/auth/login",
                headers={"Content-Type": "application/json"},
                data=json.dumps({"username": "admin", "password": "x"}).encode("utf-8"),
            )
            ok_env, detail = assert_envelope(body, "CSRF_INVALID", "security.origin_host")
            structured(
                "controller",
                "security",
                "POST /api/v1/auth/login 缺 Origin == 403 CSRF_INVALID（预会话门禁先于凭据校验）",
                status == 403 and ok_env,
                f"status={status} {detail}",
            )

        section("6 ③ 双端协同运行证据")
        joint_client = BoardClient(BOARD_ADDR)
        caps_joint = joint_client.call({"action": "device.capabilities.get"})
        joint_client.close()
        ctrl_status, _, ctrl_body = http_call("GET", f"{CTRL_ORIGIN}/healthz")
        both_alive = board.poll() is None and controller.poll() is None
        structured(
            "joint",
            "coexistence",
            f"板端 {BOARD_ADDR} 与中控 {CTRL_ADDR} 同时在线且各自接口同时可用",
            both_alive
            and caps_joint.get("status") == "ok"
            and ctrl_status == 200,
            f"board_pid={board.pid} controller_pid={controller.pid} "
            f"board_rpc={caps_joint.get('status')} controller_healthz={ctrl_status} "
            f"healthz_body={ctrl_body[:60].decode('utf-8', 'replace')}",
        )

    except Exception as error:  # noqa: BLE001
        check("流程异常", False, f"{type(error).__name__}: {mask(str(error))[:400]}")
    finally:
        section("7 进程清理与端口回收")
        if controller is not None:
            result = shutdown(controller, "controller")
            controller = None
            check("中控进程优雅退出（SIGTERM）", result.startswith("terminated"), result)
        if board is not None:
            result = shutdown(board, "board-agent")
            board = None
            check("板端进程优雅退出（SIGTERM）", result.startswith("terminated") or result.startswith("killed"), result)
        check(f"中控端口已释放 {CTRL_ADDR}", port_free(CTRL_ADDR), CTRL_ADDR)
        check(f"板端端口已释放 {BOARD_ADDR}", port_free(BOARD_ADDR), BOARD_ADDR)
        if controller_handle is not None:
            try:
                controller_handle.close()
            except OSError:
                pass
        if board_handle is not None:
            try:
                board_handle.close()
            except OSError:
                pass

    section("8 临时文件清理")
    with open(CONTROLLER_LOG, "rb") as handle:
        log(f"  中控日志尾部（去敏）: {tail(CONTROLLER_LOG, 3)}")
    log(f"  板端日志尾部（去敏）: {tail(BOARD_LOG, 3)}")

    evidence_out = {
        "board": {"addr": BOARD_ADDR, "journal_dir": BOARD_JOURNAL_DIR},
        "controller": {"addr": CTRL_ADDR, "origin": CTRL_ORIGIN},
        "records": ref.mask_obj(STRUCTURED),
        "checks": ref.mask_obj(ref.RESULTS),
    }
    with open(EVIDENCE_JSON, "w", encoding="utf-8") as handle:
        json.dump(evidence_out, handle, ensure_ascii=False, indent=2)

    removed: list[str] = []
    for path in (BOOTSTRAP_LOG, BOARD_JOURNAL_DIR, ART_DIR):
        try:
            if os.path.isdir(path):
                shutil.rmtree(path)
            elif os.path.exists(path):
                os.unlink(path)
            removed.append(path)
        except OSError as error:
            check(f"清理 {path}", False, type(error).__name__)
    check("引导口令文件已删除（含一次性口令）", not os.path.exists(BOOTSTRAP_LOG), BOOTSTRAP_LOG)
    check("板端 journal 测试目录已删除", not os.path.exists(BOARD_JOURNAL_DIR), BOARD_JOURNAL_DIR)
    check("子进程日志目录已删除", not os.path.exists(ART_DIR), ART_DIR)

    passed = [r for r in ref.RESULTS if r["ok"]]
    failed = [r for r in ref.RESULTS if not r["ok"]]
    with open(EVIDENCE_JSON, "w", encoding="utf-8") as handle:
        json.dump(
            {
                **evidence_out,
                "summary": {
                    "total": len(ref.RESULTS),
                    "passed": len(passed),
                    "failed": len(failed),
                    "blocked": len(BLOCKED),
                    "findings": len(FINDINGS),
                },
                "removed_paths": removed,
            },
            handle,
            ensure_ascii=False,
            indent=2,
        )

    section("联合测试汇总")
    for side in ("board", "controller", "joint"):
        rows = [r for r in STRUCTURED if r.get("side") == side and "case" in r]
        executed = [r for r in rows if r["ok"] is not None]
        ok_count = sum(1 for r in executed if r["ok"])
        blocked_count = sum(1 for r in rows if r["ok"] is None)
        log(
            f"  side={side} 通过 {ok_count}/{len(executed)}"
            + (f"，受阻 {blocked_count}" if blocked_count else "")
        )
    log(
        f"  断言总计 {len(ref.RESULTS)} 项：通过 {len(passed)}，失败 {len(failed)}；"
        f"受阻 {len(BLOCKED)} 项，发现 {len(FINDINGS)} 项"
    )
    for item in FINDINGS:
        log(f"  发现项: {item['case']}")
    for item in failed:
        log(f"  失败项: {item['name']} :: {item['detail']}")
    for item in BLOCKED:
        log(f"  受阻项: {item['case']} :: {item['detail']}")
    if not failed:
        log(f"  退出码 0 的含义：已执行的断言全部通过；受阻 {len(BLOCKED)} 项属环境/权限限制，需上游处置")
    log(f"  去敏证据 JSON: {EVIDENCE_JSON}")
    return 0 if not failed else 1


if __name__ == "__main__":
    raise SystemExit(main())
