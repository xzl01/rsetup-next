#!/usr/bin/env python3
"""中控真实服务自动化操作验证 + 无头浏览器（Firefox headless / geckodriver W3C）端到端验证。

流程：
  1. 进程内解析 ``secret/mysql.md`` 取得开发库连接参数（任何输出均去敏）；
  2. 以 ``CONTROLLER_*`` 环境变量启动 ``./target/debug/rsetup-controller``，轮询 ``/healthz``；
  3. 纯标准库 W3C WebDriver REST 调用驱动无头 Firefox（内置 SPA 渲染、标题、DOM、登录页截图）；
  4. 直接 HTTP 调用验证探活、资产 404、统一 401/400/403 错误信封；
  5. 优雅终止全部子进程并确认监听端口已释放。

绝不打印：数据库口令、连接串、管理员引导口令。子进程输出仅写入日志文件并按需去敏摘要。
"""

from __future__ import annotations

import base64
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CONTROLLER_BIN = os.path.join(ROOT, "target", "debug", "rsetup-controller")
SECRET_FILE = os.path.join(ROOT, "secret", "mysql.md")
GECKODRIVER = shutil.which("geckodriver") or "/snap/bin/geckodriver"
FIREFOX_LAUNCHER = "/usr/bin/firefox"


def firefox_binary() -> str | None:
    """需要时显式指定 Firefox **ELF 二进制**；默认返回 None 交给 geckodriver 自动定位。

    本机 ``/usr/bin/firefox`` 是 snap 启动器 shell 脚本，geckodriver 会直接判为
    ``binary is not a Firefox executable``；snap 版 geckodriver 与 Firefox 同属一个
    snap，自动定位到的才是可用的真实二进制。可用 ``HEADLESS_E2E_FIREFOX_BIN`` 覆盖。
    """
    override = os.environ.get("HEADLESS_E2E_FIREFOX_BIN")
    if not override:
        return None
    with open(override, "rb") as handle:
        if handle.read(4) != b"\x7fELF":
            raise RuntimeError(f"HEADLESS_E2E_FIREFOX_BIN 不是 ELF 可执行文件: {override}")
    return override

LISTEN_ADDRESS = "127.0.0.1:39088"
HOST, PORT = LISTEN_ADDRESS.split(":")
ORIGIN = f"http://{LISTEN_ADDRESS}"
CURSOR_SECRET_HEX = "0123456789abcdef" * 4  # 64 个十六进制字符 = 32 字节

SCREENSHOT = "/tmp/headless_login.png"
EVIDENCE_JSON = "/tmp/headless_e2e_evidence.json"
ART_DIR = "/tmp/headless_e2e_artifacts"

# 去敏词典：解析凭据后填充，任何写入控制台/证据的输出都先经过 mask()。
_SECRETS: list[str] = []


def mask(text: str) -> str:
    out = text if isinstance(text, str) else str(text)
    for secret in _SECRETS:
        if secret:
            out = out.replace(secret, "***")
    return out


def mask_obj(obj):
    if isinstance(obj, str):
        return mask(obj)
    if isinstance(obj, list):
        return [mask_obj(v) for v in obj]
    if isinstance(obj, dict):
        return {k: mask_obj(v) for k, v in obj.items()}
    return obj


def log(message: str) -> None:
    print(mask(message), flush=True)


# --------------------------------------------------------------------------- #
# 检查结果登记
# --------------------------------------------------------------------------- #
RESULTS: list[dict] = []


def check(name: str, ok: bool, detail: str = "") -> bool:
    RESULTS.append({"name": name, "ok": bool(ok), "detail": mask(str(detail))})
    log(f"  [{'PASS' if ok else 'FAIL'}] {name}" + (f" :: {detail}" if detail else ""))
    return bool(ok)


def section(title: str) -> None:
    log("")
    log(f"=== {title} ===")


# --------------------------------------------------------------------------- #
# 凭据解析（进程内，不回显）
# --------------------------------------------------------------------------- #
def parse_credentials(path: str) -> dict:
    aliases = {
        "host": "host",
        "port": "port",
        "user": "user",
        "username": "user",
        "db": "db",
        "database": "db",
        "password": "password",
        "passwd": "password",
    }
    values = {"host": "", "port": "", "user": "", "db": "", "password": ""}
    with open(path, encoding="utf-8") as handle:
        for raw_line in handle:
            line = raw_line.strip().strip("`")
            if not line or line.startswith("#"):
                continue
            parts = re.split(r"\s*[:=]\s*|\s+", line, maxsplit=1)
            if len(parts) != 2:
                continue
            key, value = parts[0].strip().strip("`").lower(), parts[1].strip().strip("`")
            field = aliases.get(key)
            if field and not values[field]:
                values[field] = value
    missing = [k for k, v in values.items() if not v]
    if missing:
        raise RuntimeError(f"credentials incomplete, missing fields: {','.join(missing)}")
    # 注册去敏词（长值优先，避免子串漏遮）。
    _SECRETS.extend(sorted([values["password"], values["user"], values["db"], values["host"]], key=len, reverse=True))
    return values


def build_database_url(creds: dict) -> str:
    user = urllib.parse.quote(creds["user"], safe="")
    password = urllib.parse.quote(creds["password"], safe="")
    database = urllib.parse.quote(creds["db"], safe="")
    return f"mysql://{user}:{password}@{creds['host']}:{creds['port']}/{database}"


# --------------------------------------------------------------------------- #
# 工具函数
# --------------------------------------------------------------------------- #
def port_free(addr: str, timeout: float = 1.0) -> bool:
    host, port = addr.split(":")
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.settimeout(timeout)
        return sock.connect_ex((host, int(port))) != 0


def pick_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def http_call(method: str, url: str, *, headers=None, data=None, timeout: float = 20.0):
    """返回 (status, headers_dict, body_bytes)；HTTPError 也作为正常响应返回。"""
    request = urllib.request.Request(url, data=data, method=method, headers=dict(headers or {}))
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.status, response.headers, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.headers, error.read()


def json_or_text(body: bytes):
    try:
        return json.loads(body.decode("utf-8"))
    except Exception:
        return None


def assert_envelope(body: bytes, code: str, message_key: str | None = None) -> tuple[bool, str]:
    """校验统一错误信封：顶层 {error, request_id}，error 恰为 {code, message_key, params}。"""
    value = json_or_text(body)
    if not isinstance(value, dict):
        return False, "响应不是 JSON"
    top_keys = sorted(value.keys())
    if top_keys != ["error", "request_id"]:
        return False, f"顶层键={top_keys}"
    if not isinstance(value["request_id"], str) or not value["request_id"]:
        return False, "request_id 非字符串"
    error = value["error"]
    if not isinstance(error, dict):
        return False, "error 非对象"
    error_keys = sorted(error.keys())
    if error_keys != ["code", "message_key", "params"]:
        return False, f"error 键={error_keys}"
    if error["code"] != code:
        return False, f"code={error['code']} 期望 {code}"
    if error["params"] != {}:
        return False, f"params={error['params']}"
    if message_key is not None and error["message_key"] != message_key:
        return False, f"message_key={error['message_key']} 期望 {message_key}"
    return True, f"code={error['code']} message_key={error['message_key']} request_id=ok"


# --------------------------------------------------------------------------- #
# W3C WebDriver 客户端（纯标准库）
# --------------------------------------------------------------------------- #
class WebDriver:
    def __init__(self, port: int) -> None:
        self.base = f"http://127.0.0.1:{port}"
        self.session_id: str | None = None

    def request(self, method: str, path: str, payload=None, timeout: float = 90.0):
        url = self.base + path
        data = None if payload is None else json.dumps(payload).encode("utf-8")
        headers = {"Accept": "application/json", "Content-Type": "application/json"}
        status, _, body = http_call(method, url, headers=headers, data=data, timeout=timeout)
        parsed = json_or_text(body)
        if status >= 400:
            detail = ""
            if isinstance(parsed, dict) and isinstance(parsed.get("value"), dict):
                detail = f"{parsed['value'].get('error')}: {parsed['value'].get('message')}"
            raise RuntimeError(f"WebDriver {method} {path} -> HTTP {status} {mask(detail)}")
        return parsed

    def create_session(self, capabilities: dict) -> dict:
        result = self.request("POST", "/session", {"capabilities": {"alwaysMatch": capabilities}})
        value = result["value"]
        self.session_id = value["sessionId"]
        return value["capabilities"]

    def delete_session(self) -> None:
        if self.session_id:
            self.request("DELETE", f"/session/{self.session_id}", timeout=20.0)
            self.session_id = None

    def get(self, url: str) -> None:
        self.request("POST", f"/session/{self.session_id}/url", {"url": url})

    def title(self) -> str:
        return str(self.request("GET", f"/session/{self.session_id}/title")["value"])

    def current_url(self) -> str:
        return str(self.request("GET", f"/session/{self.session_id}/url")["value"])

    def script(self, source: str, args=None):
        return self.request("POST", f"/session/{self.session_id}/execute/sync", {"script": source, "args": args or []})["value"]

    def screenshot_png(self) -> bytes:
        return base64.b64decode(str(self.request("GET", f"/session/{self.session_id}/screenshot")["value"]))

    def status(self) -> dict:
        return self.request("GET", "/status", timeout=10.0)["value"]


def wait_for(driver: WebDriver, source: str, timeout: float = 20.0, interval: float = 0.4):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            last = driver.script(source)
        except Exception as error:  # noqa: BLE001
            last = f"error: {error}"
        if last:
            return last
        time.sleep(interval)
    return last


# --------------------------------------------------------------------------- #
# 子进程管理
# --------------------------------------------------------------------------- #
def spawn(cmd: list[str], env: dict | None, logfile: str):
    handle = open(logfile, "wb")
    proc = subprocess.Popen(cmd, stdout=handle, stderr=subprocess.STDOUT, env=env, cwd=ROOT)
    return proc, handle


def shutdown(proc, name: str, timeout: float = 20.0) -> str:
    if proc is None:
        return "not-started"
    if proc.poll() is not None:
        return f"already-exited(code={proc.returncode})"
    proc.send_signal(signal.SIGTERM)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            return f"terminated(code={proc.returncode})"
        time.sleep(0.2)
    proc.kill()
    proc.wait(timeout=10)
    return "killed-after-timeout"


def tail(path: str, lines: int = 15) -> str:
    try:
        with open(path, "rb") as handle:
            data = handle.read().decode("utf-8", "replace").splitlines()
    except OSError:
        return "<无日志>"
    return " / ".join(mask(line).strip() for line in data[-lines:] if line.strip())


# --------------------------------------------------------------------------- #
# 主流程
# --------------------------------------------------------------------------- #
def main() -> int:
    os.makedirs(ART_DIR, exist_ok=True)
    controller_log = os.path.join(ART_DIR, "controller.log")
    driver_log = os.path.join(ART_DIR, "geckodriver.log")
    evidence: dict = {"listen": LISTEN_ADDRESS, "artifacts": {}, "checks": []}

    controller = None
    driver_proc = None
    driver: WebDriver | None = None
    driver_port: int | None = None
    secret_dir = None

    try:
        section("0 环境前置")
        check("中控二进制存在且可执行", os.access(CONTROLLER_BIN, os.X_OK), CONTROLLER_BIN)
        check("geckodriver 存在", os.access(GECKODRIVER, os.X_OK), GECKODRIVER)
        check("firefox 入口存在（snap 启动器，真实二进制由 geckodriver 定位）", os.access(FIREFOX_LAUNCHER, os.X_OK), FIREFOX_LAUNCHER)
        check("Python 标准库 urllib/json 就绪", sys.version_info >= (3, 9), sys.version.split()[0])

        section("1 凭据解析（去敏）")
        creds = parse_credentials(SECRET_FILE)
        database_url = build_database_url(creds)
        check(
            "secret/mysql.md 解析成功",
            all(creds.values()),
            f"host=<masked:{len(creds['host'])}B> port={creds['port']} user=<masked> db=<masked:{len(creds['db'])}B> password=<masked:{len(creds['password'])}B>",
        )
        check("连接串仅在进程内构造，输出不含明文口令", "***" not in database_url and mask(database_url) != database_url)

        section("2 启动中控真实服务")
        check("启动前 39088 端口空闲", port_free(LISTEN_ADDRESS), LISTEN_ADDRESS)
        env = os.environ.copy()
        env.update(
            {
                "CONTROLLER_DATABASE_URL": database_url,
                "CONTROLLER_LISTEN_ADDRESS": LISTEN_ADDRESS,
                "CONTROLLER_ALLOWED_HOSTS": f"127.0.0.1:{PORT},localhost:{PORT}",
                "CONTROLLER_ALLOWED_ORIGIN": ORIGIN,
                "CONTROLLER_MANAGEMENT_CURSOR_SECRET_HEX": CURSOR_SECRET_HEX,
                "RUST_LOG": env.get("RUST_LOG", "info"),
            }
        )
        # 管理员引导口令唯一落盘位置：0700 临时目录内的 create_new 文件，验证结束即删除。
        secret_dir = "/tmp/headless_e2e_bootstrap"
        shutil.rmtree(secret_dir, ignore_errors=True)
        os.makedirs(secret_dir, mode=0o700)
        env["CONTROLLER_BOOTSTRAP_SECRET_LOG"] = os.path.join(secret_dir, "bootstrap.log")

        started = time.monotonic()
        controller, controller_handle = spawn([CONTROLLER_BIN], env, controller_log)
        ready = False
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            if controller.poll() is not None:
                break
            try:
                status, _, _ = http_call("GET", f"{ORIGIN}/healthz", timeout=2.0)
                if status == 200:
                    ready = True
                    break
            except Exception:  # noqa: BLE001
                pass
            time.sleep(0.5)
        boot_seconds = round(time.monotonic() - started, 2)
        check(
            "中控进程启动且 /healthz 就绪",
            ready,
            f"{boot_seconds}s 内就绪 (pid={controller.pid})" if ready else f"进程状态={controller.poll()} 日志: {tail(controller_log, 8)}",
        )
        if not ready:
            raise RuntimeError("中控服务未就绪，跳过后续验证")
        evidence["artifacts"]["controller_log"] = controller_log

        section("3 HTTP API 自动化验证")
        status, headers, body = http_call("GET", f"{ORIGIN}/healthz")
        check("GET /healthz == 200", status == 200, f"status={status}")
        status, headers, body = http_call("GET", f"{ORIGIN}/readyz")
        check("GET /readyz == 200（真实库探活）", status == 200, f"status={status}")

        status, headers, body = http_call("GET", f"{ORIGIN}/assets/missing.js")
        ctype = headers.get("Content-Type", "")
        check(
            "GET /assets/missing.js == 404（不回退 HTML）",
            status == 404 and "text/html" not in ctype.lower(),
            f"status={status} content_type={ctype!r} body={len(body)}B",
        )
        check(
            "缺失资产 404 严格性（无 Content-Type 声明、零字节体，绝无 HTML 兜底）",
            ctype == "" and len(body) == 0,
            f"content_type={ctype!r} body={len(body)}B",
        )
        evidence["artifacts"]["missing_asset"] = {"status": status, "content_type": ctype, "bytes": len(body)}

        envelopes: dict[str, str] = {}
        for route in ("/api/v1/users", "/api/v1/tasks"):
            status, headers, body = http_call("GET", f"{ORIGIN}{route}")
            ok, detail = assert_envelope(body, "AUTH_REQUIRED")
            check(f"GET {route} == 401 AUTH_REQUIRED 统一信封", status == 401 and ok, f"status={status} {detail}")
            envelopes[route] = mask(body.decode("utf-8", "replace"))
        check(
            "受保护端点 401 不下发会话 Cookie",
            not any("set-cookie" in k.lower() for k in (headers.keys() or [])),
            f"最后一次的 Set-Cookie={headers.get('Set-Cookie')!r}",
        )

        status, headers, body = http_call(
            "POST",
            f"{ORIGIN}/api/v1/auth/login",
            headers={"Content-Type": "text/plain", "Origin": ORIGIN},
            data=b'{"username":"e2e-probe","password":"***"}',
        )
        ok, detail = assert_envelope(body, "INVALID_ARGUMENT", "auth.login.bad_body")
        no_side_effects = headers.get("Set-Cookie") is None and headers.get("X-CSRF-Token") is None
        check(
            "非法 Content-Type == 400 统一信封（且未下发 Cookie/CSRF）",
            status == 400 and ok and no_side_effects,
            f"status={status} {detail} Set-Cookie={headers.get('Set-Cookie')!r} X-CSRF-Token={headers.get('X-CSRF-Token')!r}",
        )
        envelopes["POST /api/v1/auth/login 非法Content-Type"] = mask(body.decode("utf-8", "replace"))

        # Origin 门禁先于 Content-Type 门禁：两者同时非法时必须得到 403，而不是 400。
        status, headers, body = http_call(
            "POST",
            f"{ORIGIN}/api/v1/auth/login",
            headers={"Content-Type": "text/plain"},
            data=b'{"username":"e2e-probe","password":"***"}',
        )
        ok, detail = assert_envelope(body, "CSRF_INVALID", "security.origin_host")
        check("缺 Origin 且 Content-Type 非法 == 403（Origin 门禁优先）", status == 403 and ok, f"status={status} {detail}")

        status, headers, body = http_call(
            "POST",
            f"{ORIGIN}/api/v1/auth/login",
            headers={"Content-Type": "application/json"},
            data=b'{"username":"e2e-probe","password":"***"}',
        )
        ok, detail = assert_envelope(body, "CSRF_INVALID", "security.origin_host")
        check("缺失 Origin == 403 CSRF_INVALID", status == 403 and ok, f"status={status} {detail}")
        envelopes["POST /api/v1/auth/login 缺Origin"] = mask(body.decode("utf-8", "replace"))

        # 合法 Origin + 合法 Content-Type + 不存在的凭据：必须是凭据层 401，而非安全层 403/400。
        status, headers, body = http_call(
            "POST",
            f"{ORIGIN}/api/v1/auth/login",
            headers={"Content-Type": "application/json", "Origin": ORIGIN},
            data=b'{"username":"e2e-nope","password":"***"}',
        )
        ok, detail = assert_envelope(body, "INVALID_CREDENTIALS")
        check(
            "合法 Origin + 错误凭据 == 401 INVALID_CREDENTIALS（真实库校验发生）",
            status == 401 and ok and headers.get("Set-Cookie") is None,
            f"status={status} {detail} Set-Cookie={headers.get('Set-Cookie')!r}",
        )
        envelopes["POST /api/v1/auth/login 错误凭据"] = mask(body.decode("utf-8", "replace"))
        evidence["artifacts"]["envelopes"] = envelopes

        status, headers, body = http_call("GET", f"{ORIGIN}/")
        html = body.decode("utf-8", "replace")
        check(
            "GET / == 200 且返回 SPA 外壳（含 <div id=\"app\">）",
            status == 200 and '<div id="app">' in html and "/assets/index-" in html,
            f"status={status} bytes={len(body)} content_type={headers.get('Content-Type')}",
        )
        asset = re.search(r'/assets/index-[A-Za-z0-9_-]+\.(?:js|css)', html)
        if asset:
            status, headers, body = http_call("GET", f"{ORIGIN}{asset.group(0)}")
            check(
                "内嵌资产 200 且 immutable 缓存",
                status == 200 and "immutable" in headers.get("Cache-Control", ""),
                f"{asset.group(0)} status={status} cache={headers.get('Cache-Control')} bytes={len(body)}",
            )
        status, headers, body = http_call("GET", f"{ORIGIN}/login")
        check(
            "SPA 深链 /login == 200 HTML 回退",
            status == 200 and "text/html" in headers.get("Content-Type", "").lower(),
            f"status={status} content_type={headers.get('Content-Type')}",
        )
        # 浏览器在无 <link rel="icon"> 时会自行请求 /favicon.ico：该路径既不在资产表也不在
        # SPA 白名单内，必须 404，绝不返回 HTML 外壳或伪造图标。
        status, headers, body = http_call("GET", f"{ORIGIN}/favicon.ico")
        check(
            "GET /favicon.ico == 404（不伪造资产、不回退 HTML）",
            status == 404 and "text/html" not in headers.get("Content-Type", "").lower(),
            f"status={status} content_type={headers.get('Content-Type')!r} body={len(body)}B",
        )
        status, headers, body = http_call("GET", f"{ORIGIN}/api/v1/not-a-route")
        check(
            "未知 /api/v1/* 不回退 SPA（非 200 HTML）",
            "text/html" not in headers.get("Content-Type", "").lower(),
            f"status={status} content_type={headers.get('Content-Type')!r} body={len(body)}B",
        )

        section("4 无头浏览器（geckodriver W3C WebDriver）")
        driver_port = pick_free_port()
        driver_args = [GECKODRIVER, "--host", "127.0.0.1", "--port", str(driver_port), "--log", "warn"]
        firefox_bin = firefox_binary()
        if firefox_bin:
            driver_args[1:1] = ["--binary", firefox_bin]
        driver_proc, driver_handle = spawn(
            driver_args,
            None,
            driver_log,
        )
        driver = WebDriver(driver_port)
        gecko_ready = False
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                gecko_ready = bool(driver.status().get("ready"))
                if gecko_ready:
                    break
            except Exception:  # noqa: BLE001
                pass
            time.sleep(0.3)
        check("geckodriver 启动且 /status ready", gecko_ready, f"port={driver_port}")

        firefox_options: dict = {"args": ["--headless", "--width=1280", "--height=800"]}
        if firefox_bin:
            firefox_options["binary"] = firefox_bin
        caps = {
            "browserName": "firefox",
            "pageLoadStrategy": "normal",
            "moz:firefoxOptions": firefox_options,
        }
        session_caps = driver.create_session(caps)
        check(
            "创建 WebDriver session（headless Firefox）",
            bool(driver.session_id) and session_caps.get("browserName") == "firefox",
            f"browserVersion={session_caps.get('browserVersion')} headless={session_caps.get('moz:headless')} platform={session_caps.get('platformName')}",
        )
        evidence["artifacts"]["session_capabilities"] = {k: str(v) for k, v in session_caps.items()}

        driver.script("return 1;")  # 确认 content process 已就绪
        driver.get(f"{ORIGIN}/")
        title = driver.title()
        check("① 页面标题 == 'Rsetup Controller'", title == "Rsetup Controller", f"title={title!r}")

        rendered = wait_for(driver, "var a=document.getElementById('app');return !!(a && a.childElementCount>0);", timeout=25)
        app_info = driver.script(
            "var a=document.getElementById('app');"
            "return JSON.stringify({exists:!!a, children:a?a.childElementCount:-1,"
            " htmlLen:a?a.innerHTML.length:-1, bodyLen:document.body.innerHTML.length,"
            " head:document.title, scripts:document.scripts.length, styles:document.styleSheets.length});"
        )
        info = json.loads(app_info)
        check("② <div id=\"app\"> 存在且前端已渲染子树", bool(rendered) and info["children"] > 0, json.dumps(info))
        check(
            "③ 内嵌 JS/CSS 由单二进制提供（页面挂上 module script 与样式表）",
            info["scripts"] >= 1 and info["styles"] >= 1 and info["htmlLen"] > 0,
            f"scripts={info['scripts']} styleSheets={info['styles']} appHtml={info['htmlLen']}B",
        )

        resources = driver.script(
            "return JSON.stringify(performance.getEntriesByType('resource').map(function(r){return r.name}));"
        )
        resource_list = json.loads(resources)
        evidence["artifacts"]["browser_resources"] = [mask(u) for u in resource_list]
        check(
            "④ 浏览器实际加载了内嵌资产（/assets/index-*）",
            any("/assets/index-" in u for u in resource_list),
            f"{len(resource_list)} 个资源: " + ", ".join(u.rsplit('/', 1)[-1] for u in resource_list),
        )
        api_calls = [u for u in resource_list if "/api/v1/" in u]
        check(
            "⑤ 前端启动即调用受保护 API（无会话 → 401 → 登录态）",
            len(api_calls) > 0,
            ", ".join(u.replace(ORIGIN, "") for u in api_calls) or "无 /api/v1 请求",
        )

        driver.get(f"{ORIGIN}/#/login")
        login_ready = wait_for(
            driver,
            "return !!document.querySelector('form.auth-form');",
            timeout=20,
        )
        png = driver.screenshot_png()
        with open(SCREENSHOT, "wb") as handle:
            handle.write(png)
        check("⑥ 登录页截图已保存 /tmp/headless_login.png", len(png) > 5_000, f"bytes={len(png)} path={SCREENSHOT}")
        evidence["artifacts"]["screenshot"] = SCREENSHOT

        form_info_raw = driver.script(
            "var f=document.querySelector('form.auth-form');"
            "var u=document.querySelector('#auth-username');var p=document.querySelector('#auth-password');"
            "var b=f?f.querySelector('button[type=\"submit\"],button'):null;"
            "return JSON.stringify({url:location.hash, form:!!f, aria:f?f.getAttribute('aria-label'):null,"
            " h2:f?(f.querySelector('h2')?f.querySelector('h2').textContent.trim():null):null,"
            " username:!!u, password:!!p, pwType:p?p.type:null,"
            " button:b?b.textContent.trim():null, disabled:b?b.disabled:null,"
            " inputs:document.querySelectorAll('input').length,"
            " appChildren:document.getElementById('app').childElementCount});"
        )
        form_info = json.loads(form_info_raw)
        check(
            "⑦ 登录表单渲染（form + 用户名/口令输入 + 提交按钮）",
            bool(login_ready)
            and form_info["form"]
            and form_info["username"]
            and form_info["password"]
            and form_info["pwType"] == "password"
            and bool(form_info["button"]),
            json.dumps(form_info, ensure_ascii=False),
        )
        check(
            "⑧ 路由哈希为 #/login 且表单带无障碍标签",
            form_info["url"] == "#/login" and bool(form_info["aria"]),
            f"hash={form_info['url']} aria-label={form_info['aria']} 按钮文案={form_info['button']}",
        )
        evidence["artifacts"]["login_form"] = form_info
        evidence["artifacts"]["page_title"] = title

        driver.delete_session()
        browser_phase_done = True
        check("⑨ WebDriver session 已销毁", driver.session_id is None)

        section("5 优雅终止与端口回收")
        log(f"  geckodriver: {shutdown(driver_proc, 'geckodriver')}")
        driver_proc = None
        check("geckodriver 端口已释放", port_free(f"127.0.0.1:{driver_port}"), f"port={driver_port}")

        controller_status = shutdown(controller, "controller")
        controller = None
        check("中控进程优雅退出（SIGTERM）", controller_status.startswith("terminated"), controller_status)
        check("监听端口 127.0.0.1:39088 已释放", port_free(LISTEN_ADDRESS), LISTEN_ADDRESS)
        log(f"  中控日志尾部（去敏）: {tail(controller_log, 4)}")

    except Exception as error:  # noqa: BLE001
        check("流程异常", False, f"{type(error).__name__}: {mask(str(error))[:400]}")
    finally:
        section("6 清理")
        if driver is not None and driver.session_id:
            try:
                driver.delete_session()
            except Exception as error:  # noqa: BLE001
                log(f"  销毁 session 失败: {mask(str(error))}")
        if driver_proc is not None:
            log(f"  geckodriver: {shutdown(driver_proc, 'geckodriver')}")
        if controller is not None:
            log(f"  controller: {shutdown(controller, 'controller')}")
        check("清理后 39088 端口空闲", port_free(LISTEN_ADDRESS), LISTEN_ADDRESS)
        if secret_dir:
            shutil.rmtree(secret_dir, ignore_errors=True)
        check("管理员引导口令临时目录已删除", secret_dir is None or not os.path.exists(secret_dir), str(secret_dir))
        leftovers = [d for d in os.listdir(tempfile.gettempdir()) if d.startswith("rust_mozprofile")]
        check("浏览器临时 profile 目录已回收", not leftovers, f"残留={leftovers}")

    hard = list(RESULTS)
    passed = [r for r in hard if r["ok"]]
    failed = [r for r in hard if not r["ok"]]
    evidence["checks"] = mask_obj(RESULTS)
    evidence["summary"] = {"total": len(hard), "passed": len(passed), "failed": len(failed)}
    with open(EVIDENCE_JSON, "w", encoding="utf-8") as handle:
        json.dump(mask_obj(evidence), handle, ensure_ascii=False, indent=2)

    section("验证汇总")
    log(f"总计 {len(hard)} 项：通过 {len(passed)}，失败 {len(failed)}")
    for item in failed:
        log(f"  失败项: {item['name']} :: {item['detail']}")
    log(f"证据 JSON: {EVIDENCE_JSON}")
    log(f"截图: {SCREENSHOT}")
    log(f"子进程日志: {controller_log} / {driver_log}")
    return 0 if not failed else 1


if __name__ == "__main__":
    raise SystemExit(main())
