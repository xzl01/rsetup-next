#!/usr/bin/env python3
# scripts/tunnel_two_process.py
"""真实 controller + board-agent 两进程隧道验收**准备**脚本（口径见准备报告）。

* 只启动**真实**预编译产物 ``rsetup-controller`` 与 ``rsetup-board-agent --connect``；不用
  测试 server、假 SQL 准入策略或伪 boot。默认不连接不启动：真正运行需 ``--execute`` + 逐字
  ``--ack`` + 本地门禁全过。
* 输入一律**标准 JSON**（``json.loads``）：注释、尾随逗号、单引号、裸键、重复键、``NaN``/
  ``Infinity`` 全拒；UTF-8 严格解码。目标库 JSON 用已授权的五个平铺字段，不需任何变换。
* 管理员凭据是**独立显式私有文件**（``--admin-file``，0600），或用 controller 首启自己写出的
  bootstrap sink 私有文件；缺来源精确 ``NEEDS_CONTEXT`` 拒绝执行。从不 SQL 改口令或重置 admin。
* 备份门禁只**真实验证**引用/权限/sha256/（声明时）gzip CRC 与 raw sha256/目标库名/proof hash
  绑定；``authoritative``、``consistent_snapshot`` 只从私有 proof 显式复制校验、绝不默认；
  ``restore_drill=not_performed`` 如实登记，不会被抬成「已验证可恢复」。
* 秘密只在本进程内载入：DSN 只进子进程 env（无 shell、无 source、无 tracing），子进程输出与
  HTTP 响应体只进私有 state 目录（0700/0600、``O_NOFOLLOW``）；console 只打印固定阶段名、
  布尔、计数与退出码。子进程由本进程拥有：所有退出路径 SIGTERM → 有界 wait → SIGKILL → wait。
* 离线自检与单测临时目录固定落在仓库 ``target/tunnel-two-process-prep``（见 ``prep_workspace``）。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import secrets
import socket
import stat
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import zlib

ACK_LITERAL = "ISOLATED_TARGET_AND_BACKUP_VERIFIED"
EXIT_OK = 0
EXIT_GATE_FAILED = 3
EXIT_FAILED = 4
OPENSSL_PATH_DEFAULT = "/usr/bin/openssl"
ED25519_SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")
# Ed25519 PKCS#8 定长 shape（用于把 openssl 产出的 v1 就地重排为 ring 接受的 v2）。
# 来源：ring 0.17.14 `pkcs8.rs::wrap_key_`（`prefix || private_key || middle || public_key`）与其
# `ed25519_pkcs8_v2_template.der`（`3051 0201 01 3005 06032b6570 0422 0420 | 812100`，
# private_key_index=0x10）。openssl `genpkey -algorithm ed25519` 产 PKCS#8 **v1**（version=0、无
# publicKey），而生产 `IdentityKey::from_pkcs8`→ring `Ed25519KeyPair::from_pkcs8` 只收 **v2**
# （`Version::V2Only`，强制含 publicKey 并核公私一致性，见 ed25519/signing.rs:62-90）。本项目**不**
# 放宽为 `from_pkcs8_maybe_unchecked`，故在此把固定 v1 重排成与 ring `generate_pkcs8` 逐字节相同的 v2。
ED25519_PKCS8_V1_PREFIX = bytes.fromhex("302e020100300506032b657004220420")  # v1 固定头（version=0）
ED25519_PKCS8_V2_PREFIX = bytes.fromhex("3051020101300506032b657004220420")  # v2 模板 prefix[:0x10]
ED25519_PKCS8_V2_PUB_MIDDLE = bytes.fromhex("812100")  # v2 模板 [1]{unusedBits=0} 落在 seed 与 pub 之间
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
USERNAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
# 标识符一律用 `fullmatch` 精确锚定：`^…$` + `match` 会放过尾随换行（`$` 允许在末尾 `\n`
# 之前收尾），那等于把控制字符放进 DSN。字符集本身已排除空白、NUL 与其它控制字符；
# 长度上限 1+63=64（MySQL/TiDB 标识符上限）。
NAME_RE = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_$]{0,63}")
# 数据库名额外允许**合法连字符**（合成示例 `rsetup-synth-v4`）：MySQL/TiDB 里含 `-` 的库名需要反引号
# 引用，而本脚本从不构造 SQL，只把它作为 URL path 段 percent-encode 后进 DSN/env。
# 首字符仍限字母/数字/下划线（不以 `-` 开头，避免被当成命令行选项）；`-` 只允许出现在后续位。
DATABASE_NAME_RE = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_$-]{0,63}")
# 授权 DB 用户名与库名同值且含合法 `-`（可信 prepare 已确认的**语法事实**，无需读值/改账户）。
# user 在此**只作 DSN userinfo 字段**（`quote(safe='')` percent-encode 后入 DSN/env），
# 从不作 SQL identifier，也不触发任何账号/服务器全局操作。字符范围与 `DATABASE_NAME_RE`
# 相同、首字符规则/长度上限一致，但作为**独立 pattern** 保留政策边界：放宽的只有字符集。
USER_NAME_RE = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_$-]{0,63}")
HOST_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9.-]{0,252}$")

# 已授权目标库 JSON 的五个平铺字段（不嵌套、不改名、不额外要求 params/socket 等）。
CREDENTIAL_FIELDS = {"host", "port", "user", "password", "database"}
ADMIN_FIELDS = {"username", "password", "new_password"}

# canonical 验收 manifest：键名与实际备份 proof 对齐，另加 engine 与 proof hash 绑定。
MANIFEST_VERSION = "rsetup-two-process-backup/v1"
MANIFEST_REQUIRED = (
    "manifest_version", "engine", "target_database", "backup_path", "sha256_gz",
    "authoritative", "consistent_snapshot", "restore_drill", "source_proof",
)
MANIFEST_OPTIONAL = ("sha256_raw", "table_count", "rows_total")
RESTORE_DRILL_NOT_PERFORMED = "not_performed"
BOOTSTRAP_SECRET_FILENAME = "controller-bootstrap-secret.log"

# controller 启动期 check_identity_schema 要求的版本；migrate --check 只输出版本号。
REQUIRED_SCHEMA_VERSION = 4
SCHEMA_VERSION_RE = re.compile(r"^schema_version=([0-9]{1,3}|missing)$", re.MULTILINE)
GZIP_WBITS = zlib.MAX_WBITS | 16
PREP_ROOT_SUBPATH = ("target", "tunnel-two-process-prep")

# 阶段名：稳定合同，报告与 console 共用。
STAGES = (
    "plan", "identity", "schema_gate", "controller_up", "login", "board_pending", "approve",
    "ready", "status_poll", "revoke", "board_stop", "preflight", "shutdown", "negative_badpin",
)

# 超时预算（秒）：controller 启动预算与 NTP 30s 回退并行，health 留 90s 余量。
TIMEOUTS = {
    "health": 90.0, "pending": 60.0, "ready": 60.0, "status": 30.0, "stop": 30.0,
    "preflight": 20.0, "bootstrap_secret": 90.0, "term": 20.0, "kill": 10.0,
    "badpin_attempt": 20.0,
}

# badpin 负例在「已观察到尝试」之后还要观察多久的窗口（秒）。
BAD_PIN_WINDOW_SECONDS = 5.0

# 状态路由的**固定观测类别**：只作为 facts/summary 里的固定 token，绝不携带响应体。
# 依据真实 DTO（crates/rsetup-controller/src/http_device_status.rs:317-398）：
# 有样本 = ``snapshot`` 对象 + ``freshness`` ∈ {fresh,stale,error} + ``age_ms`` 十进制字符串；
# 空缓存 = ``snapshot=null`` + ``freshness="unknown"`` + ``age_ms=null``（真实但**不是样本**）。
STATUS_PRESENT = "present"
STATUS_CACHE_EMPTY = "empty_cache"
STATUS_MALFORMED = "malformed"
STATUS_TRANSPORT = "transport"
STATUS_HTTP_403 = "http_403"
STATUS_HTTP_404 = "http_404"
STATUS_HTTP_5XX = "http_5xx"
STATUS_HTTP_OTHER = "http_other"
STATUS_SAMPLE_FRESHNESS = ("fresh", "stale", "error")
STATUS_SNAPSHOT_STRING_FIELDS = ("boot_id", "agent_epoch", "clock_epoch", "clock_quality")
STATUS_SNAPSHOT_DECIMAL_FIELDS = ("sample_seq", "sampled_monotonic_ns")
DECIMAL_INT_RE = re.compile(r"^-?[0-9]{1,20}$")
UNSIGNED_INT_RE = re.compile(r"^[0-9]{1,20}$")

# 只接受形如固定大写常量的错误码；其它一律不信任（绝不回显自由文本）。
FIXED_CODE_RE = re.compile(r"[A-Z][A-Z0-9_]{0,39}")

# 阶段与运行期的固定失败类别（非 GateError 的原生异常也归到这里）。
UNEXPECTED_STAGE_ERROR = "UNEXPECTED_STAGE_ERROR"
RUN_INTERRUPTED = "RUN_INTERRUPTED"
PROCESS_SPAWN_FAILED = "PROCESS_SPAWN_FAILED"
SUMMARY_WRITE_FAILED = "SUMMARY_WRITE_FAILED"


def sanitize_code(code) -> str:
    """GateError.code 只有形如固定大写常量才可信；否则折成固定类别，绝不回显文本。"""
    if isinstance(code, str) and FIXED_CODE_RE.fullmatch(code):
        return code
    return UNEXPECTED_STAGE_ERROR


class GateError(Exception):
    """门禁/阶段失败：只携带**固定类别码**，绝不携带值（DSN、口令、pin、路径内容）。"""

    def __init__(self, code: str) -> None:
        super().__init__(code)
        self.code = code


class Console:
    """只允许固定 token 与布尔/整数的控制台：自由文本（含 ``str(exc)``）一律进不了 stdout。"""

    def __init__(self, stream=None) -> None:
        self._stream = stream if stream is not None else sys.stdout
        self._allowed = set(STAGES) | {
            "ok", "fail", "skip", "planned", "selftest", "gate", "error", "run",
        }

    def emit(self, stage: str, key: str, value) -> None:
        assert stage in self._allowed, "stage must be a fixed token"
        assert re.fullmatch(r"[a-z][a-z0-9_]{0,31}", key), "key must be a fixed token"
        if isinstance(value, bool):
            rendered = "true" if value else "false"
        elif isinstance(value, int):
            rendered = str(value)
        elif value is None:
            rendered = "-"
        elif isinstance(value, str) and value in self._allowed:
            rendered = value
        else:
            raise TypeError("console value must be bool/int/None/fixed token")
        print(f"stage={stage} {key}={rendered}", file=self._stream, flush=True)


def _reject_constant(raw: str):
    raise ValueError("non-finite constant")


def load_json_document(text: str):
    """标准 JSON：``json.loads`` + 重复键拒绝 + ``NaN``/``Infinity`` 拒绝。

    刻意**不**支持注释、尾随逗号、单引号、裸键——标准 JSON 没有它们，脚本不再自带解释器。
    """

    def _object(pairs):
        out: dict = {}
        for key, value in pairs:
            if key in out:
                raise ValueError("duplicate key")
            out[key] = value
        return out

    try:
        return json.loads(text, object_pairs_hook=_object, parse_constant=_reject_constant)
    except (ValueError, RecursionError):
        raise GateError("INPUT_BAD_JSON") from None


class Redactor:
    """登记秘密与身份字段值，供必须落盘的文本做兜底替换（console 本就不打印自由文本）。

    两类值两种强度，取舍都写清楚：

    * **秘密**（口令、DSN、bootstrap sink 取到的口令）：无条件整串替换，即使紧邻其它字符也不
      留残留片段；长度下限 ``MIN_REDACT_LEN`` 以下的不登记（太短的值会把一切文本打碎，这类短
      秘密不该出现在本合同里，DSN 仍会整体命中）。
    * **身份字段**（host/user/database）：只在**整段**出现时替换（前后不是 ``[A-Za-z0-9_]``）。
      否则库名恰为 ``port`` 这类值会把 summary 里的固定类别 ``transport`` 撕成 ``t[redacted]``，
      把可核对的证据文本打碎——身份字段本不是秘密，这层只是防裸值漂进日志/异常文本。
    """

    MIN_REDACT_LEN = 4

    def __init__(self) -> None:
        self._values: list[str] = []
        self._identifiers: list[str] = []

    def add(self, value) -> None:
        if isinstance(value, str) and len(value) >= self.MIN_REDACT_LEN and value not in self._values:
            self._values.append(value)

    def add_identifier(self, value) -> None:
        if (isinstance(value, str) and len(value) >= self.MIN_REDACT_LEN
                and value not in self._identifiers and value not in self._values):
            self._identifiers.append(value)

    def scrub(self, text: str) -> str:
        # 长值先替换：否则短值先命中会把长值（如整条 DSN）撕成半截，留下可拼回的残留片段。
        for value in sorted(self._values, key=len, reverse=True):
            text = text.replace(value, "[redacted]")
        for value in sorted(self._identifiers, key=len, reverse=True):
            text = re.sub(rf"(?<![A-Za-z0-9_]){re.escape(value)}(?![A-Za-z0-9_])", "[redacted]", text)
        return text


def _lstat(path: str):
    try:
        return os.lstat(path)
    except FileNotFoundError:
        return None
    except OSError:
        raise GateError("STATE_DIR_UNUSABLE") from None


def assert_no_symlink(path: str) -> None:
    """路径本身与**每一个已存在的祖先**都不得是符号链接。"""
    cursor = os.path.abspath(path)
    while True:
        meta = _lstat(cursor)
        if meta is not None and stat.S_ISLNK(meta.st_mode):
            raise GateError("SYMLINK_NOT_ALLOWED")
        parent = os.path.dirname(cursor)
        if parent == cursor:
            return
        cursor = parent


def private_file(path: str, *, must_exist: bool) -> None:
    """常规文件、非链接、属主 euid、权限恰 0600。"""
    assert_no_symlink(path)
    meta = _lstat(path)
    if meta is None:
        if must_exist:
            raise GateError("INPUT_FILE_MISSING")
        return
    if not stat.S_ISREG(meta.st_mode):
        raise GateError("INPUT_FILE_NOT_REGULAR")
    if meta.st_uid != os.geteuid():
        raise GateError("INPUT_FILE_OWNER")
    if stat.S_IMODE(meta.st_mode) != 0o600:
        raise GateError("INPUT_FILE_MODE")


def write_private(path: str, payload) -> str:
    """0600 独占写入（合成 fixture 与派生产物共用）。"""
    data = payload if isinstance(payload, bytes) else str(payload).encode("utf-8")
    fd = os.open(path, os.O_CREAT | os.O_WRONLY | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
    try:
        os.write(fd, data)
    finally:
        os.close(fd)
    return path


def read_private_bytes(path: str, limit: int = 1 << 20) -> bytes:
    private_file(path, must_exist=True)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        data = b""
        while True:
            chunk = os.read(fd, 65_536)
            if not chunk:
                return data
            data += chunk
            if len(data) > limit:
                raise GateError("INPUT_FILE_TOO_LARGE")
    finally:
        os.close(fd)


def load_private_json(path: str, *, code: str):
    """0600 私有输入 → 标准 JSON。UTF-8 严格解码，绝不 ``replace`` 吞坏字节。"""
    try:
        text = read_private_bytes(path).decode("utf-8")
    except UnicodeDecodeError:
        raise GateError(code) from None
    return load_json_document(text)


def prepare_state_dir(path: str, *, create: bool, repo_root: str | None = None, git_runner=None) -> str:
    """state 目录：绝对路径、非链接、已存在则恰 0700 且属主 euid；仓库内必须 gitignored。"""
    if not os.path.isabs(path):
        raise GateError("STATE_DIR_NOT_ABSOLUTE")
    normalized = os.path.normpath(path)
    if normalized in {os.sep, os.path.expanduser("~"), os.path.abspath(os.sep + "tmp")}:
        raise GateError("STATE_DIR_NOT_PRIVATE")
    assert_no_symlink(normalized)
    meta = _lstat(normalized)
    if meta is not None:
        if not stat.S_ISDIR(meta.st_mode):
            raise GateError("STATE_DIR_NOT_DIRECTORY")
        if meta.st_uid != os.geteuid():
            raise GateError("STATE_DIR_OWNER")
        if stat.S_IMODE(meta.st_mode) != 0o700:
            raise GateError("STATE_DIR_MODE")
    elif create:
        if _lstat(os.path.dirname(normalized)) is None:
            raise GateError("STATE_DIR_PARENT_MISSING")
        try:
            os.mkdir(normalized, 0o700)
            os.chmod(normalized, 0o700)
        except OSError:
            raise GateError("STATE_DIR_UNUSABLE") from None
    else:
        raise GateError("STATE_DIR_MISSING")
    if repo_root:
        ensure_not_tracked(normalized, repo_root, git_runner)
    if create and _lstat(os.path.join(normalized, ".state-dir-marker")) is None:
        os.close(os.open(
            os.path.join(normalized, ".state-dir-marker"),
            os.O_CREAT | os.O_WRONLY | os.O_NOFOLLOW | os.O_EXCL, 0o600,
        ))
    return normalized


def ensure_not_tracked(path: str, repo_root: str, git_runner) -> None:
    """在仓库工作树内落秘密前必须已被 gitignore；仓库外直接通过。"""
    root = os.path.realpath(repo_root)
    if not os.path.realpath(path).startswith(root + os.sep):
        return
    runner = git_runner if git_runner is not None else _git_check_ignore
    if not runner(root, path):
        raise GateError("STATE_DIR_NOT_GITIGNORED")


def _git_check_ignore(repo_root: str, path: str) -> bool:
    try:
        result = subprocess.run(  # noqa: S603 - 固定 argv，无 shell
            ["/usr/bin/git", "check-ignore", "-q", path], cwd=repo_root,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20, check=False,
        )
    except OSError:
        raise GateError("GIT_CHECK_UNAVAILABLE") from None
    return result.returncode == 0


class PrivateLog:
    """把子进程输出与 HTTP 响应体收进 state 目录内的 0600 文件。"""

    def __init__(self, state_dir: str) -> None:
        self._dir = state_dir

    def path(self, name: str) -> str:
        return os.path.join(self._dir, name)

    def open_write(self, name: str) -> int:
        """每次运行的日志文件必须不存在（O_EXCL）：不复用、不覆盖既有证据。"""
        try:
            fd = os.open(self.path(name), os.O_CREAT | os.O_WRONLY | os.O_TRUNC | os.O_NOFOLLOW | os.O_EXCL, 0o600)
        except FileExistsError:
            raise GateError("STATE_LOG_EXISTS") from None
        os.fchmod(fd, 0o600)
        return fd

    def append(self, name: str, text: str) -> None:
        fd = os.open(self.path(name), os.O_CREAT | os.O_WRONLY | os.O_APPEND | os.O_NOFOLLOW, 0o600)
        try:
            os.write(fd, text.encode("utf-8", "replace"))
        finally:
            os.close(fd)

    def read_tail(self, name: str, limit: int = 262_144) -> str:
        """只读自己创建的私有文件（O_NOFOLLOW），用于真实谓词轮询。"""
        try:
            fd = os.open(self.path(name), os.O_RDONLY | os.O_NOFOLLOW)
        except FileNotFoundError:
            return ""
        except OSError:
            raise GateError("STATE_LOG_UNUSABLE") from None
        try:
            data = b""
            while True:
                chunk = os.read(fd, 65_536)
                if not chunk:
                    break
                data = (data + chunk)[-limit:]
        finally:
            os.close(fd)
        return data.decode("utf-8", "replace")


def prep_workspace(name: str, root: str) -> str:
    """在 ``<repo>/target/tunnel-two-process-prep`` 下独占创建 0700 工作区。

    自检与单测共用：临时文件**显式**落在仓库内 gitignored 的 target 下，不读 ``$TMPDIR``。
    """
    base = os.path.abspath(os.path.join(root, *PREP_ROOT_SUBPATH))
    if _lstat(base) is None:
        if _lstat(os.path.dirname(base)) is None:
            raise GateError("WORKSPACE_PARENT_MISSING")
        try:
            os.mkdir(base, 0o700)
        except OSError:
            raise GateError("WORKSPACE_UNUSABLE") from None
    os.chmod(base, 0o700)
    path = os.path.join(base, f"{name}-{os.getpid()}-{secrets.token_hex(4)}")
    assert_no_symlink(path)
    try:
        os.mkdir(path, 0o700)
    except OSError:
        raise GateError("WORKSPACE_UNUSABLE") from None
    os.chmod(path, 0o700)
    if os.path.realpath(os.path.dirname(path)) != os.path.realpath(base):
        raise GateError("WORKSPACE_NOT_PRIVATE")
    return path


def remove_tree(path: str) -> None:
    """只删自己创建的目录树（自底向上，无 shell）。"""
    if not os.path.isdir(path) or os.path.islink(path):
        return
    for entry in os.scandir(path):
        remove_tree(entry.path) if entry.is_dir(follow_symlinks=False) else _try_unlink(entry.path)
    _try_unlink(path, rmdir=True)


def _try_unlink(path: str, *, rmdir: bool = False) -> None:
    try:
        os.rmdir(path) if rmdir else os.unlink(path)
    except OSError:
        pass


def _require_str(obj: dict, key: str, code_missing: str, code_type: str, *, allow_empty: bool = False) -> str:
    if key not in obj:
        raise GateError(code_missing)
    value = obj[key]
    if not isinstance(value, str) or (not allow_empty and not value):
        raise GateError(code_type)
    return value


def _reject_unknown(obj: dict, allowed: set, code: str) -> None:
    if not isinstance(obj, dict) or set(obj) - allowed:
        raise GateError(code)


class Credential:
    """已授权目标库 JSON：**五个平铺字段**，原文件不需任何变换。"""

    def __init__(self, node: dict) -> None:
        _reject_unknown(node, CREDENTIAL_FIELDS, "CREDENTIAL_UNKNOWN_FIELD")
        self.host = _require_str(node, "host", "CREDENTIAL_FIELD_MISSING", "CREDENTIAL_FIELD_TYPE")
        if not HOST_RE.match(self.host):
            raise GateError("CREDENTIAL_FIELD_TYPE")
        self.database = _require_str(node, "database", "CREDENTIAL_FIELD_MISSING", "CREDENTIAL_FIELD_TYPE")
        if not DATABASE_NAME_RE.fullmatch(self.database):
            raise GateError("CREDENTIAL_FIELD_TYPE")
        self.user = _require_str(node, "user", "CREDENTIAL_FIELD_MISSING", "CREDENTIAL_FIELD_TYPE")
        if not USER_NAME_RE.fullmatch(self.user):
            raise GateError("CREDENTIAL_FIELD_TYPE")
        port = node.get("port", 3306)
        if not isinstance(port, int) or isinstance(port, bool) or not 1 <= port <= 65535:
            raise GateError("CREDENTIAL_FIELD_TYPE")
        self.port = port
        password = node.get("password", "")
        if not isinstance(password, str):
            raise GateError("CREDENTIAL_FIELD_TYPE")
        self.password = password

    def dsn(self) -> str:
        """percent-encode 后只进子进程 env；**绝不**进 argv 或 shell 字符串。"""
        return "mysql://{}:{}@{}:{}/{}".format(
            urllib.parse.quote(self.user, safe=""), urllib.parse.quote(self.password, safe=""),
            self.host, self.port, urllib.parse.quote(self.database, safe=""),
        )


class AdminCredentials:
    """管理员账号：独立显式私有文件（0600）。脚本不内置口令、不 SQL 改口令、不重置 admin。

    ``password`` 缺失即改用 controller 自己写出的 bootstrap sink；``new_password`` 缺失且真实
    登录回 ``must_change_password`` 时直接失败关闭。
    """

    def __init__(self, node: dict) -> None:
        _reject_unknown(node, ADMIN_FIELDS, "ADMIN_UNKNOWN_FIELD")
        self.username = _require_str(node, "username", "ADMIN_FIELD_MISSING", "ADMIN_FIELD_TYPE")
        if not USERNAME_RE.match(self.username):
            raise GateError("ADMIN_FIELD_TYPE")
        self.password = _require_str(node, "password", "ADMIN_FIELD_MISSING", "ADMIN_FIELD_TYPE") if "password" in node else None
        self.new_password = _require_str(node, "new_password", "ADMIN_FIELD_MISSING", "ADMIN_FIELD_TYPE") if "new_password" in node else None


def parse_credential(node: dict, redactor: Redactor | None = None) -> Credential:
    """从**已解析**的标准 JSON 对象构造凭据（不做 I/O）。"""
    credential = Credential(node)
    sink = redactor if redactor is not None else Redactor()
    sink.add(credential.password)
    sink.add(credential.dsn())
    # host/user/database 本身不是秘密，但**裸值**可能随异常文本或私有日志漂出去：整段脱敏。
    sink.add_identifier(credential.host)
    sink.add_identifier(credential.user)
    sink.add_identifier(credential.database)
    return credential


def load_credential(path: str, redactor: Redactor) -> Credential:
    return parse_credential(load_private_json(path, code="CREDENTIAL_BAD_JSON"), redactor)


def load_admin(path: str, redactor: Redactor) -> AdminCredentials:
    admin = AdminCredentials(load_private_json(path, code="ADMIN_BAD_JSON"))
    redactor.add(admin.password)
    redactor.add(admin.new_password)
    return admin


def _sha256_field(value) -> str:
    if not isinstance(value, str) or not SHA256_RE.match(value):
        raise GateError("BACKUP_FIELD_TYPE")
    return value


class BackupManifest:
    """canonical 验收 manifest：只如实登记 + 真实可验证的绑定，绝不默认任何 claim。

    ``authoritative``/``consistent_snapshot`` 必须**显式为 JSON true**（缺失/false/非布尔即拒绝，
    不从「有备份文件」推断一致性）；``restore_drill`` 只如实登记，``not_performed`` 不会被抬成
    已验证可恢复；备份 ``metadata.schema_version`` 与本门禁无关（绑定的是库名相同，不是版本相同）。
    """

    def __init__(self, node: dict) -> None:
        _reject_unknown(node, set(MANIFEST_REQUIRED) | set(MANIFEST_OPTIONAL), "BACKUP_UNKNOWN_FIELD")
        for field in MANIFEST_REQUIRED:
            if field not in node:
                raise GateError("BACKUP_FIELD_MISSING")
        if node["manifest_version"] != MANIFEST_VERSION or node["engine"] not in {"tidb", "mysql"}:
            raise GateError("BACKUP_VERSION_UNSUPPORTED")
        self.engine = node["engine"]
        if not isinstance(node["target_database"], str) or not DATABASE_NAME_RE.fullmatch(node["target_database"]):
            raise GateError("BACKUP_FIELD_TYPE")
        self.target_database = node["target_database"]
        if not isinstance(node["backup_path"], str) or not os.path.isabs(node["backup_path"]):
            raise GateError("BACKUP_FILE_INVALID")
        self.backup_path = os.path.normpath(node["backup_path"])
        self.sha256_gz = _sha256_field(node["sha256_gz"])
        self.sha256_raw = _sha256_field(node["sha256_raw"]) if "sha256_raw" in node else None
        for claim in ("authoritative", "consistent_snapshot"):
            if node[claim] is not True:  # 缺失已在上面拦；显式 false/非布尔同样拒绝
                raise GateError("BACKUP_CLAIM_NOT_TRUE")
        self.authoritative = True
        self.consistent_snapshot = True
        drill = node["restore_drill"]
        if not isinstance(drill, str) or not drill:
            raise GateError("BACKUP_FIELD_TYPE")
        self.restore_drill = drill
        self.restore_drill_performed = drill != RESTORE_DRILL_NOT_PERFORMED
        proof = node["source_proof"]
        if not isinstance(proof, dict) or set(proof) != {"path", "sha256"}:
            raise GateError("BACKUP_PROOF_BINDING_INVALID")
        if not isinstance(proof["path"], str) or not os.path.isabs(proof["path"]):
            raise GateError("BACKUP_PROOF_BINDING_INVALID")
        self.proof_path = os.path.normpath(proof["path"])
        self.proof_sha256 = _sha256_field(proof["sha256"])
        for field in MANIFEST_OPTIONAL:
            if field in node and field != "sha256_raw":
                if not isinstance(node[field], int) or isinstance(node[field], bool):
                    raise GateError("BACKUP_FIELD_TYPE")
        self._verified = {"gz": False, "crc": False, "target": False, "proof": False}

    def facts(self) -> dict:
        """去敏事实：只有布尔与固定语义，不含路径、digest 或库名。"""
        return {
            "backup_gz_hash_verified": self._verified["gz"],
            "backup_gzip_crc_verified": self._verified["crc"],
            "backup_target_bound": self._verified["target"],
            "backup_proof_hash_bound": self._verified["proof"],
            "restore_drill_performed": self.restore_drill_performed,
            "restore_verified": False,  # 演练未做即恒 false：绝不伪称已验证可恢复。
        }

    def verify(self, *, target_database: str) -> None:
        """执行时**真实**验证：proof hash 绑定 → 引用/权限 → 目标名 → sha256 →（声明时）CRC。"""
        self._verify_proof_binding()
        assert_no_symlink(self.backup_path)
        meta = _lstat(self.backup_path)
        if meta is None or not stat.S_ISREG(meta.st_mode):
            raise GateError("BACKUP_FILE_MISSING")
        if stat.S_IMODE(meta.st_mode) & 0o077:
            raise GateError("BACKUP_FILE_MODE")
        self._verified["target"] = self.target_database == target_database
        if not self._verified["target"]:
            raise GateError("BACKUP_TARGET_MISMATCH")
        self._verified["gz"], self._verified["crc"] = self._stream_and_hash()
        if not self._verified["gz"]:
            raise GateError("BACKUP_HASH_MISMATCH")

    def _verify_proof_binding(self) -> None:
        """重算 proof 文件 sha256 并复核其中 claim/digest：改旧 manifest 蒙不过去。

        **两层都要检**：manifest 的 claim 由 :class:`BackupManifest` 构造期强制显式 true；
        这里再要求 proof 里同一 claim 也是**显式布尔 true**，两者不一致即拒——手写 manifest
        把 ``authoritative=false`` 的真实 proof 洗成 true 必须失败（不能只信派生通道）。
        """
        node = load_private_json(self.proof_path, code="BACKUP_PROOF_UNREADABLE")
        if not isinstance(node, dict):
            raise GateError("BACKUP_PROOF_SHAPE")
        if _sha256_of(self.proof_path) != self.proof_sha256:
            raise GateError("BACKUP_PROOF_HASH_MISMATCH")
        for claim in ("authoritative", "consistent_snapshot"):
            value = node.get(claim)
            if not isinstance(value, bool):  # 缺失/非布尔都不是可信声明
                raise GateError("BACKUP_PROOF_SHAPE")
            if value is not True or getattr(self, claim, None) is not True:
                raise GateError("BACKUP_PROOF_MISMATCH")
        if node.get("sha256_gz") != self.sha256_gz or node.get("target_database") != self.target_database:
            raise GateError("BACKUP_PROOF_MISMATCH")
        self._verified["proof"] = True

    def _stream_and_hash(self) -> tuple[bool, bool]:
        """一遍流式：gz 文件 sha256 +（声明 raw 时）gzip 解压 CRC 校验与 raw sha256。"""
        gz = hashlib.sha256()
        raw = hashlib.sha256() if self.sha256_raw else None
        decompressor = zlib.decompressobj(GZIP_WBITS) if raw is not None else None
        try:
            fd = os.open(self.backup_path, os.O_RDONLY | os.O_NOFOLLOW)
        except OSError:
            raise GateError("BACKUP_FILE_MISSING") from None
        try:
            while True:
                chunk = os.read(fd, 1 << 20)
                if not chunk:
                    break
                gz.update(chunk)
                if raw is not None:
                    try:
                        raw.update(decompressor.decompress(chunk))
                    except zlib.error:
                        raise GateError("BACKUP_GZIP_CORRUPT") from None
        finally:
            os.close(fd)
        if raw is None:
            return gz.hexdigest() == self.sha256_gz, False
        try:
            raw.update(decompressor.flush())
        except zlib.error:
            raise GateError("BACKUP_GZIP_CORRUPT") from None
        if decompressor.unconsumed_tail or not decompressor.eof:
            raise GateError("BACKUP_GZIP_CORRUPT")
        if raw.hexdigest() != self.sha256_raw:
            raise GateError("BACKUP_RAW_HASH_MISMATCH")
        return gz.hexdigest() == self.sha256_gz, True


def _sha256_of(path: str) -> str:
    digest = hashlib.sha256()
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        while chunk := os.read(fd, 1 << 20):
            digest.update(chunk)
    finally:
        os.close(fd)
    return digest.hexdigest()


def derive_acceptance_manifest(proof_path: str, *, engine: str, out_path: str) -> str:
    """可信执行者**进程内**从真实 proof 派生 canonical manifest（0600，绝不读 secret/）。

    只复制 proof 里显式存在的键；``restore_drill=false`` → ``"not_performed"``，写 ``true`` 或缺失
    一律拒绝（不伪确认）。备份自身 ``metadata.schema_version`` 不进 manifest、不参与门禁。
    """
    node = load_private_json(proof_path, code="BACKUP_PROOF_UNREADABLE")
    if not isinstance(node, dict):
        raise GateError("BACKUP_PROOF_SHAPE")
    for key in ("backup_path", "sha256_gz", "authoritative", "consistent_snapshot", "target_database"):
        if key not in node:
            raise GateError("BACKUP_FIELD_MISSING")
    drill = node.get("restore_drill")
    if drill is False:
        drill_text = RESTORE_DRILL_NOT_PERFORMED
    elif isinstance(drill, str) and drill:
        drill_text = drill
    else:
        raise GateError("BACKUP_RESTORE_DRILL_UNDECLARED")
    absolute = os.path.normpath(os.path.abspath(proof_path))
    private_file(absolute, must_exist=True)
    manifest = {
        "manifest_version": MANIFEST_VERSION,
        "engine": engine,
        "target_database": node["target_database"],
        "backup_path": node["backup_path"],
        "sha256_gz": node["sha256_gz"],
        "authoritative": node["authoritative"],
        "consistent_snapshot": node["consistent_snapshot"],
        "restore_drill": drill_text,
        "source_proof": {"path": absolute, "sha256": _sha256_of(absolute)},
    }
    for key in MANIFEST_OPTIONAL:
        if key in node:
            manifest[key] = node[key]
    BackupManifest(manifest)  # 派生结果必须当场通过同一套字段门禁
    try:
        fd = os.open(out_path, os.O_CREAT | os.O_WRONLY | os.O_TRUNC | os.O_NOFOLLOW | os.O_EXCL, 0o600)
    except OSError:  # 固定类别：不回显输出路径（可能是调用方私有目录）
        raise GateError("MANIFEST_OUTPUT_UNWRITABLE") from None
    try:
        os.write(fd, (json.dumps(manifest, sort_keys=True) + "\n").encode("utf-8"))
    finally:
        os.close(fd)
    os.chmod(out_path, 0o600)
    return out_path


def load_backup_manifest(path: str, credential: Credential) -> BackupManifest:
    manifest = BackupManifest(load_private_json(path, code="BACKUP_BAD_JSON"))
    manifest.verify(target_database=credential.database)
    return manifest


def require_binary(path: str) -> str:
    if not os.path.isabs(path):
        raise GateError("BINARY_NOT_ABSOLUTE")
    normalized = os.path.normpath(path)
    meta = _lstat(normalized)
    if meta is None:
        raise GateError("BINARY_MISSING")
    if not stat.S_ISREG(meta.st_mode):
        raise GateError("BINARY_NOT_REGULAR")
    if not os.access(normalized, os.X_OK):
        raise GateError("BINARY_NOT_EXECUTABLE")
    return normalized


def pick_loopback_port(family=socket.AF_INET) -> int:
    """bind(0) 取端口后立刻 close：存在 TOCTOU 窗口（见报告），**失败不静默换端口/target**。"""
    probe = socket.socket(family, socket.SOCK_STREAM)
    try:
        probe.bind(("127.0.0.1", 0) if family == socket.AF_INET else ("::1", 0))
        return int(probe.getsockname()[1])
    except OSError:
        raise GateError("PORT_UNAVAILABLE") from None
    finally:
        probe.close()


def ed25519_pkcs8_v1_to_v2(private_der: bytes, public_key: bytes) -> bytes:
    """把 openssl `genpkey` 的固定 PKCS#8 **v1** DER 重排为 ring `from_pkcs8` 接受的 **v2**。

    只做**固定 Ed25519 shape** 的定长切拼（v1=48B、v2=83B），绝不写通用 ASN.1 解析器，也不放宽
    任何校验：v1 必须是 48 字节且以 `ED25519_PKCS8_V1_PREFIX` 开头、public_key 必须 32 字节，否则
    fail-closed 抛 `IDENTITY_PKCS8_SHAPE`（宁可失败关闭，也不写坏私钥文件）。输出与 ring
    `Ed25519KeyPair::generate_pkcs8`（`wrap_key_` 用同一模板）对相同 (seed, public) **逐字节一致**。
    """
    if len(public_key) != 32 or len(private_der) != 48 or not private_der.startswith(ED25519_PKCS8_V1_PREFIX):
        raise GateError("IDENTITY_PKCS8_SHAPE")
    seed = private_der[len(ED25519_PKCS8_V1_PREFIX):]
    return ED25519_PKCS8_V2_PREFIX + seed + ED25519_PKCS8_V2_PUB_MIDDLE + public_key


def generate_ed25519_identity(state_dir: str, name: str, openssl_path: str, runner=subprocess.run) -> bytes:
    """openssl 生成 Ed25519 身份，并重排为 ring `from_pkcs8` 接受的 PKCS#8 **v2** 私钥文件。

    openssl `genpkey` 只产 PKCS#8 **v1**（version=0、无 publicKey），生产 `IdentityKey::from_pkcs8`
    仅收 **v2**（强制 publicKey 一致性）；故把 seed 从 0600 私钥文件读入本进程、与 openssl 派生的
    public 一起重排为 v2 后写回**同一 0600 文件**。seed/public **从不**进 argv、console、普通日志或
    报告，只在内存内瞬存用于重排（重排不落 argv/shell，纯字节拼接）。
    """
    if not os.path.exists(openssl_path):
        raise GateError("OPENSSL_MISSING")
    directory = os.path.join(state_dir, name)
    try:
        os.mkdir(directory, 0o700)
    except FileExistsError:
        raise GateError("IDENTITY_DIR_EXISTS") from None
    os.chmod(directory, 0o700)
    key_path = os.path.join(directory, "identity.pk8")
    os.close(os.open(key_path, os.O_CREAT | os.O_WRONLY | os.O_EXCL | os.O_NOFOLLOW, 0o600))
    runner(  # noqa: S603 - 固定 argv，无 shell，openssl 直接把 v1 私钥落 0600 文件
        [openssl_path, "genpkey", "-algorithm", "ed25519", "-outform", "DER", "-out", key_path],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True,
    )
    os.chmod(key_path, 0o600)
    result = runner(  # noqa: S603
        [openssl_path, "pkey", "-inform", "DER", "-in", key_path, "-pubout", "-outform", "DER"],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=True,
    )
    der = result.stdout or b""
    if len(der) != 44 or not der.startswith(ED25519_SPKI_PREFIX):
        raise GateError("IDENTITY_PUBKEY_SHAPE")
    public_key = der[len(ED25519_SPKI_PREFIX):]
    # v1→v2：读同一 0600 文件的 v1，重排为 ring 接受的 v2 后原地写回（同 inode/0600/非链接不变）。
    v2 = ed25519_pkcs8_v1_to_v2(read_private_bytes(key_path), public_key)
    write_private(key_path, v2)
    return public_key


def bad_pin_hex(rng) -> str:
    while True:
        value = rng(32).hex()
        if value != "00" * 32:
            return value


class ProcessGroup:
    """本进程拥有全部子进程：SIGTERM → 有界 wait → SIGKILL 兜底 → 再 wait（零遗留）。"""

    def __init__(self, opener=None, sleep=time.sleep, clock=time.monotonic) -> None:
        self._opener = opener if opener is not None else subprocess.Popen
        self._sleep = sleep
        self._clock = clock
        self._wait_grace = 1.0
        self.children: list = []
        self.term_requests = 0
        self.kill_requests = 0
        self.reaped = 0
        self.orphaned: list[str] = []

    def spawn(self, name: str, argv: list, env: dict, cwd: str, log: PrivateLog) -> object:
        out_fd = log.open_write(f"{name}.stdout.log")
        try:
            err_fd = log.open_write(f"{name}.stderr.log")
        except BaseException:
            os.close(out_fd)  # 第二个日志都没开成：先关刚开的 fd
            raise
        try:
            popen = self._opener(  # noqa: S603 - 固定 argv，绝不 shell=True
                argv, env=env, cwd=cwd, stdin=subprocess.DEVNULL,
                stdout=out_fd, stderr=err_fd, close_fds=True,
            )
        except BaseException:  # Popen 创建失败：两个日志 fd 必须关掉，绝不泄漏、绝不遗留半启动
            os.close(out_fd)
            os.close(err_fd)
            raise GateError(PROCESS_SPAWN_FAILED) from None
        os.close(out_fd)
        os.close(err_fd)
        entry = {"name": name, "popen": popen, "killed": False}
        self.children.append(entry)
        return entry

    @staticmethod
    def _alive(entry) -> bool:
        return entry["popen"].poll() is None

    def _reap(self, entry, deadline: float, interval: float) -> bool:
        popen = entry["popen"]
        while self._clock() < deadline and self._alive(entry):
            self._sleep(interval)
        if self._alive(entry):
            return False
        try:
            popen.wait(timeout=1)
        except Exception:  # noqa: BLE001 - 只关心「是否已收尸」，不外泄异常文本
            return not self._alive(entry)
        return True

    def shutdown_all(self, term_timeout: float = TIMEOUTS["term"], kill_timeout: float = TIMEOUTS["kill"]) -> None:
        """SIGTERM 全部 → 有界 wait → 仍存活者 SIGKILL → **每个子进程都再 wait() 收尸**。"""
        interval = 0.05
        deadline = self._clock() + term_timeout
        for entry in self.children:
            if self._alive(entry):
                self.term_requests += 1
                entry["popen"].terminate()
        for entry in self.children:
            if not self._alive(entry) or self._reap(entry, deadline, interval):
                continue
            self.kill_requests += 1
            entry["killed"] = True
            entry["popen"].kill()
            if not self._reap(entry, self._clock() + kill_timeout, interval):
                self.orphaned.append(entry["name"])
        for entry in self.children:
            # 兜底：无论上面走的是哪条分支，这里都必须对每个子进程再 wait 一次。
            if self._reap(entry, self._clock() + self._wait_grace, interval):
                self.reaped += 1
            elif entry["name"] not in self.orphaned:
                self.orphaned.append(entry["name"])

    def __enter__(self) -> "ProcessGroup":
        return self

    def __exit__(self, exc_type, exc, tb) -> bool:
        self.shutdown_all()
        return False


def wait_until(label: str, predicate, timeout: float, interval: float = 0.25, sleep=time.sleep, clock=time.monotonic) -> int:
    """阶段推进只由**真实谓词**驱动（不靠固定 sleep 猜 Ready）。"""
    deadline = clock() + timeout
    attempts = 0
    while True:
        attempts += 1
        if predicate():
            return attempts
        if clock() >= deadline:
            raise GateError(f"WAIT_TIMEOUT_{label}".upper())
        sleep(interval)


def _revision_of(item: dict) -> str:
    """``revision`` 是十进制**字符串**（controller 出口即 ``to_string()``）。"""
    value = item.get("revision") if isinstance(item, dict) else None
    if not isinstance(value, str) or not re.fullmatch(r"[0-9]{1,20}", value):
        raise GateError("APPROVE_REVISION_UNKNOWN")
    return value


def classify_status_payload(payload) -> str:
    """按**真实** `http_device_status` DTO 判分类；返回固定 token（绝不返回体内容）。

    真实投影（``crates/rsetup-controller/src/http_device_status.rs:317-398``）：

    * 有样本：``data.snapshot`` 是对象，``data.freshness`` ∈ ``fresh|stale|error``，
      ``data.age_ms`` 是 u64 十进制字符串；``snapshot`` 内 64 位字段一律十进制字符串，
      ``metrics`` 是 ``{key,value,unit,quality}`` 数组。
    * 无样本（空缓存，真实结果）：``snapshot=null``、``freshness="unknown"``、``age_ms=null``。
      这是「本进程还没收到任何设备状态样本」的如实表示，**不是**验收需要的实际样本。
    """
    if not isinstance(payload, dict) or not isinstance(payload.get("data"), dict):
        return STATUS_MALFORMED
    data = payload["data"]
    snapshot = data.get("snapshot")
    freshness = data.get("freshness")
    if snapshot is None:
        return STATUS_CACHE_EMPTY if freshness == "unknown" else STATUS_MALFORMED
    if not isinstance(snapshot, dict):
        return STATUS_MALFORMED
    if not isinstance(freshness, str) or freshness not in STATUS_SAMPLE_FRESHNESS:
        return STATUS_MALFORMED
    age_ms = data.get("age_ms")
    if not isinstance(age_ms, str) or not UNSIGNED_INT_RE.match(age_ms):
        return STATUS_MALFORMED
    version = snapshot.get("schema_version")
    if not isinstance(version, int) or isinstance(version, bool):
        return STATUS_MALFORMED
    for field in STATUS_SNAPSHOT_STRING_FIELDS:
        value = snapshot.get(field)
        if not isinstance(value, str) or not value:
            return STATUS_MALFORMED
    for field in STATUS_SNAPSHOT_DECIMAL_FIELDS:
        value = snapshot.get(field)
        if not isinstance(value, str) or not UNSIGNED_INT_RE.match(value):
            return STATUS_MALFORMED
    sampled_unix_ms = snapshot.get("sampled_unix_ms")  # Protobuf int64：有符号十进制字符串
    if not isinstance(sampled_unix_ms, str) or not DECIMAL_INT_RE.match(sampled_unix_ms):
        return STATUS_MALFORMED
    metrics = snapshot.get("metrics")
    if not isinstance(metrics, list) or not metrics:
        return STATUS_MALFORMED
    for metric in metrics:
        if not isinstance(metric, dict) or not isinstance(metric.get("key"), str) or not metric["key"]:
            return STATUS_MALFORMED
    return STATUS_PRESENT


class ControllerHttp:
    """loopback HTTP：cookie 交给标准 CookieProcessor，CSRF 用登录响应的 ``csrf_token``。

    路径严格按现行 API：``GET /api/v1/approvals?decision=none&limit=200``（limit 上限 200，
    超限 400）、``POST /api/v1/approvals/approve``（逐项 CAS：HTTP 恒 200，须看逐项 ``ok``）、
    ``POST /api/v1/devices/{id}/revoke``、``GET /api/v1/devices/{id}/status``。响应体只进私有日志；
    状态路由**不记响应体**，只记固定观测类别（见 :meth:`ControllerHttp.status_probe`）。
    """

    def __init__(self, port: int, log: PrivateLog, opener=None, timeout: float = 5.0) -> None:
        self._base = f"http://127.0.0.1:{port}"
        self._host = f"127.0.0.1:{port}"
        self._log = log
        self._opener = opener if opener is not None else urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor()
        )
        self._timeout = timeout
        self.csrf_token: str | None = None

    def request(self, method: str, path: str, body=None, with_csrf: bool = False,
                record_body: bool = True):
        data = None
        headers = {"Host": self._host, "Accept": "application/json"}
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
            headers["Origin"] = self._base
        if with_csrf and self.csrf_token is not None:
            headers["X-CSRF-Token"] = self.csrf_token
        request = urllib.request.Request(f"{self._base}{path}", data=data, headers=headers, method=method)
        try:
            response = self._opener.open(request, timeout=self._timeout)
            raw = response.read()
            status = int(response.getcode())
        except urllib.error.HTTPError as exc:  # 4xx/5xx 也带 body
            raw = exc.read()
            status = int(exc.code)
        except (urllib.error.URLError, OSError):
            self._log.append("http.log", f"{method} {path} -> {STATUS_TRANSPORT}\n")
            return None, None
        self._log.append("http.log", f"{method} {path} -> {status}\n")
        if raw and record_body:
            self._log.append("http.log", raw.decode("utf-8", "replace") + "\n")
        if not raw:
            return status, None
        try:
            return status, json.loads(raw.decode("utf-8", "replace"))
        except ValueError:
            return status, None

    def health(self) -> bool:
        return self.request("GET", "/healthz")[0] == 200

    def login(self, username: str, password: str) -> tuple[bool, bool]:
        status, payload = self.request("POST", "/api/v1/auth/login", {"username": username, "password": password})
        if status != 200 or not isinstance(payload, dict):
            return False, False
        data = payload.get("data") or {}
        token = data.get("csrf_token")
        if not isinstance(token, str):
            return True, False
        self.csrf_token = token
        return True, bool((data.get("user") or {}).get("must_change_password"))

    def change_password(self, current: str, new_password: str) -> bool:
        status, payload = self.request(
            "POST", "/api/v1/auth/password",
            {"current_password": current, "new_password": new_password}, with_csrf=True,
        )
        # 真实成功契约：200 + `data.changed == true`（不是「任何 200 都算改密成功」）。
        return status == 200 and isinstance(payload, dict) and (payload.get("data") or {}).get("changed") is True

    def find_pending_approval(self, device_id: str):
        status, payload = self.request("GET", "/api/v1/approvals?decision=none&limit=200")
        if status != 200 or not isinstance(payload, dict):
            return None
        for item in (payload.get("data") or {}).get("items") or []:
            if isinstance(item, dict) and item.get("device_id") == device_id:
                return item
        return None

    def approve(self, device_id: str, expected_revision: str) -> str:
        """真实 CAS：200 只说明请求被受理，逐项 ``ok`` 为 true 才算批准成功。"""
        status, payload = self.request(
            "POST", "/api/v1/approvals/approve",
            {"targets": [{"device_id": device_id, "expected_revision": expected_revision}], "confirm": True},
            with_csrf=True,
        )
        if status != 200 or not isinstance(payload, dict):
            raise GateError("APPROVE_REJECTED")
        for item in (payload.get("data") or {}).get("results") or []:
            if isinstance(item, dict) and item.get("device_id") == device_id:
                if item.get("ok") is not True:
                    raise GateError("APPROVE_CAS_REJECTED")
                return _revision_of(item)
        raise GateError("APPROVE_RESULT_MISSING")

    def revoke(self, device_id: str, expected_revision: str) -> bool:
        status, payload = self.request(
            "POST", f"/api/v1/devices/{device_id}/revoke",
            {"expected_revision": expected_revision, "reason": "two-process-acceptance", "confirm": True},
            with_csrf=True,
        )
        return status == 200 and isinstance(payload, dict) and isinstance(payload.get("data"), dict)

    def status_probe(self, device_id: str) -> str:
        """状态路由的**固定观测类别**（响应体既不返回也不落日志）。

        只有按真实 DTO 校验通过的 typed sample 才返回 ``present``；``empty_cache`` 是真实的
        空缓存结果但**不是样本**，``http_403``/``http_404``/``http_5xx``/``transport``/
        ``malformed`` 是各类失败——全部是固定 token，绝不携带状态码以外的任何内容。
        """
        status, payload = self.request("GET", f"/api/v1/devices/{device_id}/status", record_body=False)
        if status is None:
            return STATUS_TRANSPORT
        if status in (401, 403):
            return STATUS_HTTP_403
        if status == 404:
            return STATUS_HTTP_404
        if status >= 500:
            return STATUS_HTTP_5XX
        if status != 200:
            return STATUS_HTTP_OTHER
        category = classify_status_payload(payload)
        self._log.append("http.log", f"status_probe device={device_id} category={category}\n")
        return category


BASE_ENV = {"PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "C", "LC_ALL": "C"}


def controller_env(dsn: str, http_port: int, tunnel_port: int, state_dir: str, cursor_secret: str) -> dict:
    env = dict(BASE_ENV)
    env.update({
        "CONTROLLER_DATABASE_URL": dsn,
        "CONTROLLER_LISTEN_ADDRESS": f"127.0.0.1:{http_port}",
        "CONTROLLER_MANAGEMENT_CURSOR_SECRET_HEX": cursor_secret,
        "CONTROLLER_CONFIG": os.path.join(state_dir, "controller-config.toml"),
        "CONTROLLER_BOOTSTRAP_SECRET_LOG": os.path.join(state_dir, BOOTSTRAP_SECRET_FILENAME),
        "CONTROLLER_ALLOWED_HOSTS": f"127.0.0.1:{http_port}",
        "CONTROLLER_ALLOWED_ORIGIN": f"http://127.0.0.1:{http_port}",
        "RSETUP_CONTROLLER_KEY_FILE": os.path.join(state_dir, "controller-id", "identity.pk8"),
        "RSETUP_TUNNEL_LISTEN": f"127.0.0.1:{tunnel_port}",
        "RSETUP_TUNNEL_POLL_PERIOD_MS": "2000",
    })
    # 刻意不写 NTP_SERVERS：servers 为空 → 启动采样在 30s 预算内回退 SystemFallback。
    return env


def board_env(tunnel_port: int, pin_hex: str, state_dir: str, serial: str) -> dict:
    env = dict(BASE_ENV)
    env.update({
        "RSETUP_CONTROLLER_ADDR": f"127.0.0.1:{tunnel_port}",
        "RSETUP_CONTROLLER_PUBLIC_KEYS": pin_hex,
        "RSETUP_BOARD_KEY_FILE": os.path.join(state_dir, "board-id", "identity.pk8"),
        "RSETUP_BOARD_JOURNAL_DIR": os.path.join(state_dir, "board-journal"),
        # 合成描述符：不取真实机器标识。
        "RSETUP_BOARD_DEVICE_SN": serial,
        "RSETUP_BOARD_DEVICE_MODEL": "SYNTH-MODEL",
        "RSETUP_BOARD_FW_VERSION": "0.0.0-synthetic",
        "RSETUP_BOARD_EXTRA_INFO": "two-process-acceptance",
    })
    return env


def write_metrics_disabled_config(state_dir: str) -> None:
    """metrics 默认要求 basic auth；本验收只走 loopback，显式关掉避免无关启动失败。"""
    fd = os.open(
        os.path.join(state_dir, "controller-config.toml"),
        os.O_CREAT | os.O_WRONLY | os.O_TRUNC | os.O_NOFOLLOW | os.O_EXCL, 0o600,
    )
    try:
        os.write(fd, b"[metrics]\nenabled = false\n")
    finally:
        os.close(fd)


def prepare_board_journal(state_dir: str) -> None:
    directory = os.path.join(state_dir, "board-journal")
    try:
        os.mkdir(directory, 0o700)
    except FileExistsError:
        raise GateError("JOURNAL_DIR_EXISTS") from None
    os.chmod(directory, 0o700)


def discover_repo_root(start: str) -> str | None:
    cursor = os.path.abspath(start)
    while True:
        if os.path.exists(os.path.join(cursor, ".git")):
            return cursor
        parent = os.path.dirname(cursor)
        if parent == cursor:
            return None
        cursor = parent


def evaluate_gates(opts, redactor: Redactor, repo_root: str | None, git_runner=None) -> dict:
    """所有门禁都在 spawn/连库**之前**完成；任一失败即抛 GateError，绝不启动。"""
    credential = load_credential(opts.credential_file, redactor)
    manifest = load_backup_manifest(opts.backup_manifest, credential)
    admin = load_admin(opts.admin_file, redactor) if opts.admin_file else None
    gates = {
        "credential": credential,
        "manifest": manifest,
        "state_dir": prepare_state_dir(opts.state_dir, create=bool(opts.execute), repo_root=repo_root, git_runner=git_runner),
        "admin": admin,
        "controller_bin": require_binary(opts.controller_bin) if opts.controller_bin else None,
        "board_bin": require_binary(opts.board_bin) if opts.board_bin else None,
        # 可选的只读版本探针（migrate --check）；不传就完全跳过，不做任何隐式迁移。
        "migrate_bin": require_binary(opts.migrate_bin) if opts.migrate_bin else None,
        "admin_missing": admin is None,
    }
    if opts.execute and admin is None:
        # 目标库已有管理员而口令未知：只能由人工提供，绝不 SQL 改口令。
        raise GateError("NEEDS_CONTEXT")
    return gates


class TwoProcessRun:
    """真实两进程主链路：Pending → 批准（真实 CAS）→ Ready → 只读状态 → Revoke → 永久停连。"""

    def __init__(self, gates: dict, console: Console, redactor: Redactor, group: ProcessGroup, *,
                 sleep=time.sleep, clock=time.monotonic, http_factory=ControllerHttp,
                 openssl_path: str = OPENSSL_PATH_DEFAULT, identity_runner=subprocess.run,
                 schema_runner=subprocess.run, port_picker=pick_loopback_port,
                 rng=secrets.token_bytes, intervals: float = 0.25) -> None:
        self.gates = gates
        self.console = console
        self.redactor = redactor
        self.group = group
        self._sleep = sleep
        self._clock = clock
        self._http_factory = http_factory
        self._openssl = openssl_path
        self._identity_runner = identity_runner
        self._schema_runner = schema_runner
        self._port_picker = port_picker
        self._rng = rng
        self._interval = intervals
        self.negative_enabled = bool(gates.get("negative_badpin"))
        self.log = PrivateLog(gates["state_dir"])
        self.http: ControllerHttp | None = None
        self.facts: dict = {}
        self.entry_codes: dict = {}
        self.ports: dict = {}
        self.pins: dict = {}
        self.device_id: str | None = None
        self.badpin_device_id: str | None = None
        self._revision: str | None = None

    def _emit(self, stage: str, **pairs) -> None:
        for key, value in pairs.items():
            self.console.emit(stage, key, value)

    def _wait(self, label: str, predicate, timeout: float) -> int:
        return wait_until(label, predicate, timeout, self._interval, self._sleep, self._clock)

    def _event_seen(self, name: str, event: str) -> bool:
        return f"board-connect event={event} " in self.log.read_tail(f"{name}.stderr.log")

    def _event_count(self, name: str, event: str) -> int:
        return self.log.read_tail(f"{name}.stderr.log").count(f"board-connect event={event} ")

    def _exited(self, name: str):
        entry = next((c for c in self.group.children if c["name"] == name), None)
        if entry is None or entry["popen"].poll() is None:
            return None
        return entry["popen"].returncode

    def stage_identity(self) -> None:
        state = self.gates["state_dir"]
        controller_pub = generate_ed25519_identity(state, "controller-id", self._openssl, self._identity_runner)
        board_pub = generate_ed25519_identity(state, "board-id", self._openssl, self._identity_runner)
        self.pins["controller"] = controller_pub.hex()
        self.pins["board"] = board_pub.hex()
        self.device_id = board_pub.hex()  # 设备 id 就是板端公钥的 hex64
        write_metrics_disabled_config(state)
        prepare_board_journal(state)
        self.facts["identity_ok"] = True
        self._emit("identity", ok=True)

    def stage_schema_gate(self) -> None:
        """版本探针只是**补充**证据且只证明版本号；完整 v4 形状由 controller 启动期
        ``check_identity_schema`` 硬门禁保证：不通过就不绑端口、进程退出，health 谓词必失败。"""
        migrate_bin = self.gates["migrate_bin"]
        self.facts["schema_shape_gate"] = "controller_startup"
        if migrate_bin is None:
            self.facts["schema_version_probe"] = False
            self._emit("schema_gate", version_probe=False)
            return
        env = dict(BASE_ENV)
        env["CONTROLLER_DATABASE_URL"] = self.gates["credential"].dsn()
        out_fd = self.log.open_write("migrate-check.stdout.log")
        err_fd = self.log.open_write("migrate-check.stderr.log")
        try:
            completed = self._schema_runner(  # noqa: S603 - 固定 argv（仅 --check），无 shell
                [migrate_bin, "--check"], env=env, stdout=out_fd, stderr=err_fd,
                stdin=subprocess.DEVNULL, timeout=120, check=False,
            )
        finally:
            os.close(out_fd)
            os.close(err_fd)
        if completed.returncode != 0:
            raise GateError("SCHEMA_CHECK_REJECTED")
        # 真实输出只有 `schema_version=<n>` 或 `schema_version=missing`（不含形状信息）。
        match = SCHEMA_VERSION_RE.search(self.log.read_tail("migrate-check.stdout.log"))
        if match is None:
            raise GateError("SCHEMA_VERSION_UNREADABLE")
        if match.group(1) == "missing":
            raise GateError("SCHEMA_VERSION_MISSING")
        version = int(match.group(1))
        self.facts["schema_version_probe"] = version == REQUIRED_SCHEMA_VERSION
        self._emit("schema_gate", version_probe=version == REQUIRED_SCHEMA_VERSION)
        if version != REQUIRED_SCHEMA_VERSION:
            raise GateError("SCHEMA_VERSION_NOT_V4")

    def stage_controller_up(self) -> None:
        state = self.gates["state_dir"]
        self.ports["http"] = self._port_picker()
        self.ports["tunnel"] = self._port_picker()
        cursor_secret = self._rng(32).hex()
        self.redactor.add(cursor_secret)
        env = controller_env(self.gates["credential"].dsn(), self.ports["http"], self.ports["tunnel"], state, cursor_secret)
        # 端口/目标一旦选定就不回退：绑定失败即失败，绝不静默换端口、换 target 或换协议。
        self.group.spawn("controller", [self.gates["controller_bin"]], env, state, self.log)
        self.http = self._http_factory(self.ports["http"], self.log)
        try:
            self._wait("health", self.http.health, TIMEOUTS["health"])
        except GateError:
            if self._exited("controller") is not None:
                raise GateError("CONTROLLER_EXITED_BEFORE_READY") from None
            raise
        # /healthz 只在启动门禁（含 check_identity_schema）之后才开始服务：health 成立才把
        # 形状门禁记成隐式成立；--check 的版本号本身不构成形状 PASS。
        self.facts["controller_healthy"] = True
        self.facts["schema_shape_gate"] = "controller_startup_healthz_observed"
        self._emit("controller_up", ok=True)

    def stage_login(self) -> None:
        admin = self.gates["admin"]
        if admin is None:
            raise GateError("NEEDS_CONTEXT")
        password = admin.password or self._read_bootstrap_secret(admin.username)
        ok, must_change = self.http.login(admin.username, password)
        if not ok:
            raise GateError("LOGIN_FAILED")
        if must_change:
            if admin.new_password is None:
                raise GateError("ADMIN_PASSWORD_CHANGE_REQUIRED")
            if not self.http.change_password(password, admin.new_password):
                raise GateError("PASSWORD_CHANGE_FAILED")
            self.redactor.add(admin.new_password)
            if not self.http.login(admin.username, admin.new_password)[0]:
                raise GateError("LOGIN_FAILED")
        if self.http.csrf_token is None:
            raise GateError("CSRF_TOKEN_MISSING")
        self.facts["login_ok"] = True
        self._emit("login", ok=True)

    def _read_bootstrap_secret(self, username: str) -> str:
        """只读**私有 state 目录内**由真实 controller 首启写出的 sink（create_new + 0600）。

        只取用一次：不改任何口令、不删文件、不落 console。
        """
        path = os.path.join(self.gates["state_dir"], BOOTSTRAP_SECRET_FILENAME)
        try:
            self._wait("bootstrap_secret", lambda: os.path.exists(path), TIMEOUTS["bootstrap_secret"])
            private_file(path, must_exist=True)
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
            try:
                raw = os.read(fd, 8192).decode("utf-8", "replace")
            finally:
                os.close(fd)
        except GateError:
            raise GateError("BOOTSTRAP_SECRET_UNAVAILABLE") from None
        prefix = f"{username}: "
        for line in raw.splitlines():
            if line.startswith(prefix) and line[len(prefix):].strip():
                password = line[len(prefix):].strip()
                self.redactor.add(password)
                self.facts["admin_from_bootstrap_sink"] = True
                return password
        raise GateError("BOOTSTRAP_SECRET_UNAVAILABLE")

    def stage_board_pending(self) -> None:
        state = self.gates["state_dir"]
        env = board_env(self.ports["tunnel"], self.pins["controller"], state, self._device_serial())
        self.group.spawn("board", [self.gates["board_bin"], "--connect"], env, state, self.log)
        self._wait("pending", lambda: self.http.find_pending_approval(self.device_id) is not None, TIMEOUTS["pending"])
        self.facts["pending_observed"] = True
        self._emit("board_pending", ok=True)

    def _device_serial(self) -> str:
        self.facts.setdefault("device_serial_synthetic", True)
        return f"SYNTH-{self._rng(6).hex()}"

    def stage_approve(self) -> None:
        item = self.http.find_pending_approval(self.device_id)
        if item is None:
            raise GateError("APPROVAL_ITEM_MISSING")
        self._revision = self.http.approve(self.device_id, _revision_of(item))
        self.facts["approved"] = True
        self._emit("approve", ok=True)

    def stage_ready(self) -> None:
        """Ready 只认**板端私有事件日志**里双流首探成功的真实事件，不靠固定 sleep。"""
        self._wait("ready", lambda: self._event_seen("board", "Ready"), TIMEOUTS["ready"])
        self.facts["ready_observed"] = True
        self._emit("ready", ok=True)

    def stage_status_poll(self) -> None:
        """状态阶段**只认 typed sample**：拿不到真实样本就是失败，不做「无证据也算过」。

        ``empty_cache``（真实 200 + ``snapshot=null``/``freshness=unknown``）、403/404/5xx、
        transport 失败、形状不符都不是样本；``status_observed=false`` 时固定抛
        ``STATUS_RPC_NOT_OBSERVED``（非零 exit、summary ``complete=false``）。Ready 不能替代
        这条 RPC 证据，阶段顺序也保持 Pending→approve→Ready→status→revoke 不变。
        """
        last = STATUS_TRANSPORT

        def probe() -> bool:
            nonlocal last
            last = self.http.status_probe(self.device_id)
            return last == STATUS_PRESENT

        try:
            self._wait("status", probe, TIMEOUTS["status"])
        except GateError:
            self.facts["status_observed"] = False
            self.facts["status_probe_category"] = last  # 固定 token，不含响应体
            self._emit("status_poll", ok=False)
            raise GateError("STATUS_RPC_NOT_OBSERVED") from None
        self.facts["status_observed"] = True
        self.facts["status_probe_category"] = STATUS_PRESENT
        self._emit("status_poll", ok=True)

    def stage_revoke(self) -> None:
        if not self.http.revoke(self.device_id, self._revision):
            raise GateError("REVOKE_FAILED")
        self.facts["revoked"] = True
        self._emit("revoke", ok=True)

    def stage_board_stop(self) -> None:
        """签名 revoke → 板端永久停止：落 UNAUTHORIZED 标记 + exit 0（不是被信号杀死）。"""
        self._wait("stop", lambda: self._exited("board") is not None, TIMEOUTS["stop"])
        code = self._exited("board")
        self.entry_codes["board"] = code if code is not None else -1
        stopped = self._event_seen("board", "Stopped")
        marker = os.path.exists(os.path.join(self.gates["state_dir"], "board-id", "UNAUTHORIZED"))
        self.facts["board_exited_zero"] = code == 0
        self.facts["board_stopped_event"] = stopped
        self.facts["unauthorized_marker"] = marker
        self._emit("board_stop", exited_zero=code == 0, stopped_event=stopped, marker=marker)
        if code != 0 or not stopped or not marker:
            raise GateError("BOARD_DID_NOT_STOP_PERMANENTLY")

    def stage_preflight(self) -> None:
        """重启后 preflight 必须拒绝且**零重连**：exit 78 + PreflightFailed + AttemptStarted 计数 0。"""
        state = self.gates["state_dir"]
        env = board_env(self.ports["tunnel"], self.pins["controller"], state, self._device_serial())
        self.group.spawn("board-restart", [self.gates["board_bin"], "--connect"], env, state, self.log)
        self._wait("preflight", lambda: self._exited("board-restart") is not None, TIMEOUTS["preflight"])
        code = self._exited("board-restart")
        self.entry_codes["board_restart"] = code if code is not None else -1
        rejected = self._event_seen("board-restart", "PreflightFailed")
        attempts = self._event_count("board-restart", "AttemptStarted")
        self.facts["preflight_rejected"] = code == 78 and rejected and attempts == 0
        self.facts["preflight_attempt_count"] = attempts
        self._emit("preflight", rejected=code == 78 and rejected, attempt_count=attempts)
        if not self.facts["preflight_rejected"]:
            raise GateError("PREFLIGHT_DID_NOT_REJECT")

    def stage_negative_badpin(self) -> None:
        """可选负例：错误 pin 的板端不得 Ready，也不得在 controller 落审批项。

        **先要取证再判定**：必须先观察到本轮真的发起过连接尝试（``AttemptStarted`` 事件，或
        进程已带事件退出）。只 sleep 固定时间后「没看到 Ready」不是有效负例——没有尝试就
        没有证据，固定抛 ``BAD_PIN_NO_ATTEMPT_EVIDENCE`` 而不是通过。

        **不**声称 GOAWAY/wire 级证据（无证据即不测），只按可观测事实判定，结论只到
        「窗口内未观察到接受」为止。
        """
        if not self.negative_enabled:
            self.facts["negative_badpin_skipped"] = True
            return
        state = self.gates["state_dir"]
        bad_pub = generate_ed25519_identity(state, "board-badpin-id", self._openssl, self._identity_runner)
        directory = os.path.join(state, "board-badpin-journal")
        os.mkdir(directory, 0o700)
        os.chmod(directory, 0o700)
        env = board_env(self.ports["tunnel"], bad_pub.hex(), state, self._device_serial())
        env["RSETUP_BOARD_KEY_FILE"] = os.path.join(state, "board-badpin-id", "identity.pk8")
        env["RSETUP_BOARD_JOURNAL_DIR"] = directory
        env["RSETUP_CONTROLLER_PUBLIC_KEYS"] = bad_pin_hex(self._rng)
        self.group.spawn("board-badpin", [self.gates["board_bin"], "--connect"], env, state, self.log)
        try:
            self._wait("badpin_attempt", lambda: self._event_count("board-badpin", "AttemptStarted") > 0
                       or self._event_seen("board-badpin", "Ready")
                       or self._exited("board-badpin") is not None, TIMEOUTS["badpin_attempt"])
        except GateError:
            self.facts["badpin_attempt_observed"] = False
            raise GateError("BAD_PIN_NO_ATTEMPT_EVIDENCE") from None
        self.facts["badpin_attempt_observed"] = True
        self._sleep(BAD_PIN_WINDOW_SECONDS)  # 已尝试之后再开观察窗口，窗口内继续找「被接受」的证据
        ready = self._event_seen("board-badpin", "Ready")
        registered = self.http.find_pending_approval(bad_pub.hex()) is not None
        self.badpin_device_id = bad_pub.hex()
        self.facts["badpin_window_observed"] = True
        self.facts["badpin_ready"] = ready
        self.facts["badpin_device_registered"] = registered
        self._emit("negative_badpin", ready=ready, registered=registered)
        if ready or registered:
            raise GateError("BAD_PIN_ACCEPTED")

    def stage_shutdown(self) -> None:
        self.group.shutdown_all(TIMEOUTS["term"], TIMEOUTS["kill"])
        for entry in self.group.children:
            self.entry_codes.setdefault(entry["name"], int(entry["popen"].returncode or 0))
        self.facts["children_terminated"] = self.group.term_requests
        self.facts["children_killed"] = self.group.kill_requests
        self.facts["children_reaped"] = self.group.reaped
        self.facts["children_orphaned"] = len(self.group.orphaned)
        self._emit(
            "shutdown", terminated=self.group.term_requests, killed=self.group.kill_requests,
            reaped=self.group.reaped, orphaned=len(self.group.orphaned),
        )
        if self.group.orphaned:
            raise GateError("CHILD_PROCESS_LEFT_RUNNING")

    def run(self) -> int:
        self.negative_enabled = bool(self.gates.get("negative_badpin"))
        order = [
            self.stage_identity, self.stage_schema_gate, self.stage_controller_up, self.stage_login,
            self.stage_board_pending, self.stage_approve, self.stage_ready, self.stage_status_poll,
            self.stage_revoke, self.stage_board_stop, self.stage_preflight,
        ]
        if self.negative_enabled:
            order.insert(8, self.stage_negative_badpin)
        else:
            self.facts["negative_badpin_skipped"] = True
        manifest = self.gates.get("manifest")
        if manifest is not None:
            self.facts.update(manifest.facts())
        required = [stage.__name__ for stage in order]
        # 先清掉上一轮 summary：写失败时宁可「没有 summary」，也不留下旧运行的 complete=true。
        try:
            os.unlink(os.path.join(self.gates["state_dir"], "two-process-summary.json"))
        except OSError:
            pass
        failed: list[str] = []
        completed: list[str] = []
        interrupted: BaseException | None = None
        try:
            for stage in order:
                try:
                    stage()
                except GateError as error:
                    self._record_failure(failed, stage.__name__, sanitize_code(error.code))
                    break
                except (KeyboardInterrupt, SystemExit) as error:  # 不吞真实程序退出
                    self._record_failure(failed, stage.__name__, RUN_INTERRUPTED)
                    interrupted = error
                    break
                except Exception:  # noqa: BLE001 - 原生异常只入固定类别，绝不外泄文本
                    self._record_failure(failed, stage.__name__, UNEXPECTED_STAGE_ERROR)
                    break
                completed.append(stage.__name__)
        finally:
            # owned children 必须被终止 + wait：shutdown 自身的异常也只入固定类别。
            try:
                self.stage_shutdown()
            except GateError as error:
                self._record_failure(failed, "stage_shutdown", sanitize_code(error.code))
            except Exception:  # noqa: BLE001 - 固定类别，不外泄 shutdown 的异常文本
                self._record_failure(failed, "stage_shutdown", UNEXPECTED_STAGE_ERROR)
            except BaseException as error:  # KeyboardInterrupt/SystemExit：先写 summary 再如实退出
                self._record_failure(failed, "stage_shutdown", RUN_INTERRUPTED)
                interrupted = interrupted or error
            summary_ok = self._write_summary(
                failed,
                complete=(not failed and completed == required
                          and self.facts.get("status_observed") is True
                          and not self.group.orphaned),
            )
        if interrupted is not None:
            raise interrupted  # 真实 Ctrl-C / 程序退出继续向外传播，不假装「已继续」
        if not summary_ok:
            return EXIT_FAILED
        return EXIT_OK if not failed else EXIT_FAILED

    def _record_failure(self, failed: list[str], stage: str, code: str) -> None:
        """失败只以**固定类别码 + 固定阶段名**外显：无 ``str(exc)``、无 traceback、无路径。"""
        failed.append(code)
        self.facts.setdefault("failed_stage", stage)
        self.console.emit("run", "fail", "fail")
        print(f"failure_code={code} failed_stage={stage}", file=sys.stderr, flush=True)

    def _write_summary(self, failed: list[str], complete: bool = False) -> bool:
        """原子写私有 summary；写失败必须让调用方知道（绝不能返回 exit 0 或留下旧 complete=true）。"""
        payload = dict(self.facts)
        payload["exit_codes"] = {k: int(v) for k, v in self.entry_codes.items()}
        payload["failure_codes"] = sorted(set(failed))
        payload["complete"] = bool(complete)
        # 汇总只含布尔/计数/固定类别码；不含 DSN、口令、pin、私钥、cookie、CSRF、备份内容。
        text = json.dumps(payload, sort_keys=True, default=str)
        directory = self.gates["state_dir"]
        path = os.path.join(directory, "two-process-summary.json")
        part = path + ".part"
        try:
            fd = os.open(part, os.O_CREAT | os.O_WRONLY | os.O_TRUNC | os.O_NOFOLLOW | os.O_EXCL, 0o600)
            try:
                os.write(fd, (self.redactor.scrub(text) + "\n").encode("utf-8"))
            finally:
                os.close(fd)
            os.replace(part, path)  # 原子替换：不会出现半写，也不会留下上一轮的 complete=true
        except OSError:
            try:
                os.unlink(part)
            except OSError:
                pass
            self.console.emit("run", "fail", "fail")
            print(f"failure_code={SUMMARY_WRITE_FAILED}", file=sys.stderr, flush=True)
            return False
        return True


def build_parser() -> argparse.ArgumentParser:
    parser = _FixedErrorParser(
        prog="tunnel_two_process.py",
        description="真实 controller/board 两进程隧道验收（默认 dry-run：不连接、不启动；输入为标准 JSON）",
    )
    parser.add_argument("--credential-file", help="已授权目标库 JSON（0600，五个平铺字段）")
    parser.add_argument("--backup-manifest", help="canonical 备份验收 manifest（0600，标准 JSON）")
    parser.add_argument("--admin-file", help="管理员私有 JSON（0600；缺 password 则用 controller bootstrap sink）")
    parser.add_argument("--state-dir", help="私有状态目录（0700，仓库内必须 gitignored）")
    parser.add_argument("--controller-bin", default="", help="预编译 rsetup-controller 绝对路径")
    parser.add_argument("--board-bin", default="", help="预编译 rsetup-board-agent 绝对路径")
    parser.add_argument("--migrate-bin", default="", help="可选 rsetup-controller-migrate 绝对路径（仅 --check 版本探针）")
    parser.add_argument("--execute", action="store_true", help="真正启动两个进程（还需 --ack）")
    parser.add_argument("--ack", default="", help=f"逐字确认：{ACK_LITERAL}")
    parser.add_argument("--with-badpin-negative", action="store_true", help="附加 bad-pin 可选负例")
    parser.add_argument("--selftest", action="store_true", help="离线自检（合成输入，不连库、不启动真实进程）")
    parser.add_argument("--derive-manifest-from-proof", metavar="PROOF", help="从真实备份 proof 派生 canonical manifest")
    parser.add_argument("--engine", choices=("tidb", "mysql"), help="派生 manifest 时的引擎（显式，不猜）")
    parser.add_argument("--manifest-out", help="派生 manifest 的输出路径（0600，必须不存在）")
    return parser


class _FixedErrorParser(argparse.ArgumentParser):
    """argparse 的错误路径**只出固定码**：默认实现会把用户误传的参数原样回显。"""

    def error(self, message: str):  # noqa: ANN401 - argparse 合同：这里刻意丢弃 message
        raise GateError("ARGUMENTS_INVALID")


def _gate_failed(console: Console, error: GateError) -> int:
    console.emit("gate", "fail", "fail")
    print(f"gate_error={sanitize_code(error.code)}", file=sys.stderr, flush=True)
    return EXIT_GATE_FAILED


def run_derive(opts, console: Console) -> int:
    """可信执行者在**本进程内**把真实 proof 转成 canonical manifest（不打印任何值）。"""
    if not opts.engine or not opts.manifest_out:
        console.emit("gate", "inputs", False)
        print("gate_error=INPUTS_MISSING", file=sys.stderr, flush=True)
        return EXIT_GATE_FAILED
    try:
        derive_acceptance_manifest(opts.derive_manifest_from_proof, engine=opts.engine, out_path=opts.manifest_out)
    except GateError as error:
        return _gate_failed(console, error)
    console.emit("plan", "manifest_derived", True)
    return EXIT_OK


def run_plan(opts, console: Console, repo_root: str | None) -> int:
    """dry-run：只评估本地门禁（含备份真实绑定），绝不 spawn、绝不连库。"""
    try:
        gates = evaluate_gates(opts, Redactor(), repo_root)
    except GateError as error:
        return _gate_failed(console, error)
    console.emit("plan", "credential", True)
    console.emit("plan", "backup_bound", True)
    console.emit("plan", "restore_drill_performed", gates["manifest"].restore_drill_performed)
    console.emit("plan", "state_dir_private", True)
    console.emit("plan", "admin_source", not gates["admin_missing"])
    console.emit("plan", "binaries", bool(gates["controller_bin"] and gates["board_bin"]))
    console.emit("plan", "schema_version_probe", bool(gates["migrate_bin"]))
    console.emit("plan", "spawned", False)
    console.emit("plan", "ok", True)
    return EXIT_OK


def run_execute(opts, console: Console, repo_root: str | None) -> int:
    if opts.ack != ACK_LITERAL:
        console.emit("gate", "ack", False)
        print("gate_error=ACK_MISMATCH", file=sys.stderr, flush=True)
        return EXIT_GATE_FAILED
    if not opts.controller_bin or not opts.board_bin:
        console.emit("gate", "binaries", False)
        print("gate_error=BINARY_MISSING", file=sys.stderr, flush=True)
        return EXIT_GATE_FAILED
    redactor = Redactor()
    try:
        gates = evaluate_gates(opts, redactor, repo_root)
    except GateError as error:
        return _gate_failed(console, error)
    gates["negative_badpin"] = bool(opts.with_badpin_negative)
    with ProcessGroup() as group:
        return TwoProcessRun(gates, console, redactor, group).run()


def main(argv=None, repo_root: str | None = None, console: Console | None = None) -> int:
    console = console if console is not None else Console()
    try:
        opts = build_parser().parse_args(argv)  # 解析错误 → GateError(ARGUMENTS_INVALID)，不回显参数
        if opts.selftest:
            return run_selftest(console)
        if opts.derive_manifest_from_proof:
            return run_derive(opts, console)
        missing = [name for name in ("credential_file", "backup_manifest", "state_dir") if not getattr(opts, name)]
        if missing:
            console.emit("gate", "inputs", False)
            print("gate_error=INPUTS_MISSING", file=sys.stderr, flush=True)
            return EXIT_GATE_FAILED
        root = repo_root or discover_repo_root(os.getcwd())
        return run_execute(opts, console, root) if opts.execute else run_plan(opts, console, root)
    except GateError as error:
        return _gate_failed(console, error)
    except Exception:  # noqa: BLE001 - 顶层同样只出固定码，不打 traceback
        console.emit("error", "fail", "fail")
        print(f"run_error={UNEXPECTED_STAGE_ERROR}", file=sys.stderr, flush=True)
        return EXIT_FAILED


class FakePopen:
    """离线自检/单测用假进程：只记录信号与 wait 顺序，**绝不**启动真实进程。

    * ``auto_exit_after``：第 N 次 :meth:`poll` 之后自然退出；``None`` 表示只能被杀。
    * ``ignore_term``：SIGTERM 无效，用来验证 SIGKILL 兜底与「必定 wait」。
    """

    instances: list["FakePopen"] = []

    def __init__(self, argv, *, ignore_term: bool = False, auto_exit_after: int | None = None) -> None:
        self.argv = list(argv)
        self.returncode: int | None = None
        self.terminates = 0
        self.kills = 0
        self.waits = 0
        self.polls = 0
        self._ignore_term = ignore_term
        self._auto_exit_after = auto_exit_after
        FakePopen.instances.append(self)

    def poll(self):
        if self.returncode is not None:
            return self.returncode
        self.polls += 1
        if self._auto_exit_after is not None and self.polls > self._auto_exit_after:
            self.returncode = 0
        return self.returncode

    def terminate(self) -> None:
        self.terminates += 1
        if not self._ignore_term:
            self.returncode = 0

    def kill(self) -> None:
        self.kills += 1
        self.returncode = -9

    def wait(self, timeout=None):
        self.waits += 1
        return self.returncode


def fake_clock_step(step: float = 0.1):
    """自检/单测用的假时钟：返回 ``(sleep, clock)``，让有界 wait 不消耗真实时间。"""
    state = {"now": 0.0}

    def sleep(_seconds: float) -> None:
        state["now"] += step

    def clock() -> float:
        return state["now"]

    return sleep, clock


def run_selftest(console: Console) -> int:
    """离线自检：标准 JSON 严格性、备份绑定与篡改拒绝、符号链接拒绝、进程所有权。

    只用合成输入；临时目录在 ``target/tunnel-two-process-prep`` 下；不连库、不起真实进程。
    """
    problems: list[str] = []
    root = discover_repo_root(os.getcwd())
    if root is None:
        console.emit("selftest", "ok", False)
        return EXIT_FAILED
    workspace = prep_workspace("selftest", root)

    def check(label: str, ok: bool) -> None:
        console.emit("selftest", label, ok)
        if not ok:
            problems.append(label)

    def gate(manifest_path: str, database: str, *, expect_ok: bool) -> bool:
        try:
            load_backup_manifest(manifest_path, Credential({"host": "127.0.0.1", "user": "svc", "database": database}))
            return expect_ok
        except GateError:
            return not expect_ok

    def gate_code(manifest_node: dict, database: str) -> str:
        """直接按 manifest 节点跑门禁，返回固定结果码（``OK`` 或 GateError 码）。"""
        try:
            BackupManifest(manifest_node).verify(target_database=database)
            return "OK"
        except GateError as error:
            return error.code

    try:
        # 1) 标准 JSON：注释/尾随逗号/单引号/裸键/重复键/NaN 全部拒绝。
        check("json_plain", load_json_document('{"a": 1, "b": [1, 2]}') == {"a": 1, "b": [1, 2]})
        rejected = 0
        for text in ('{ // c\n"a": 1}', '{"a": 1,}', "{'a': 1}", "{a: 1}", '{"a": 1, "a": 2}', '{"a": NaN}'):
            try:
                load_json_document(text)
            except GateError:
                rejected += 1
        check("json_rejects_non_standard", rejected == 6)

        # 2) 脱敏：秘密值不残留在落盘文本里。
        redactor = Redactor()
        redactor.add("synthetic-secret-value")
        check("redaction", "[redacted]" in redactor.scrub("prefix synthetic-secret-value suffix"))

        # 3) 备份绑定与篡改检测（全程零 spawn）。
        body = b"synthetic backup body"
        backup_path = write_private(os.path.join(workspace, "backup.bin"), body)
        os.chmod(backup_path, 0o600)
        proof_path = write_private(os.path.join(workspace, "proof.json"), json.dumps({
            "backup_path": backup_path, "authoritative": True, "consistent_snapshot": True,
            "restore_drill": False, "sha256_gz": hashlib.sha256(body).hexdigest(),
            "target_database": "rsetup_synth", "metadata": {"schema_version": 2},
        }))
        manifest_path = derive_acceptance_manifest(proof_path, engine="tidb", out_path=os.path.join(workspace, "manifest.json"))
        check("backup_derive_bound", gate(manifest_path, "rsetup_synth", expect_ok=True))
        check("backup_wrong_target_blocks", gate(manifest_path, "rsetup_other", expect_ok=False))
        tampered = json.loads(read_private_bytes(manifest_path).decode("utf-8"))
        tampered["sha256_gz"] = "0" * 64
        write_private(manifest_path, json.dumps(tampered))
        check("backup_manifest_tamper_blocks", gate(manifest_path, "rsetup_synth", expect_ok=False))
        derive_acceptance_manifest(proof_path, engine="tidb", out_path=os.path.join(workspace, "m2.json"))
        write_private(backup_path, body + b" more")
        check("backup_file_tamper_blocks", gate(os.path.join(workspace, "m2.json"), "rsetup_synth", expect_ok=False))

        # 4) 符号链接 state 目录拒绝。
        real_state = os.path.join(workspace, "state-real")
        os.mkdir(real_state, 0o700)
        os.chmod(real_state, 0o700)
        link = os.path.join(workspace, "state-link")
        os.symlink(real_state, link)
        try:
            prepare_state_dir(link, create=False)
            check("symlink_state_rejected", False)
        except GateError:
            check("symlink_state_rejected", True)

        # 5) 状态路由：只有按真实 DTO 校验过的 typed sample 才算样本。
        typed = {"data": {"snapshot": {
            "schema_version": 1, "boot_id": "boot-1", "agent_epoch": "epoch-1", "sample_seq": "9",
            "sampled_unix_ms": "1760000000000", "sampled_monotonic_ns": "42", "clock_epoch": "e-1",
            "metrics": [{"key": "cpu.load1", "value": 0.5, "unit": "ratio", "quality": "measured"}],
            "clock_quality": "ntp_valid"}, "received_time": {"quality": "ntp_valid"},
            "freshness": "fresh", "age_ms": "1500"}}
        empty_cache = {"data": {"snapshot": None, "received_time": {"quality": "system_fallback"},
                                "freshness": "unknown", "age_ms": None}}
        check("status_needs_typed_sample",
              classify_status_payload(typed) == STATUS_PRESENT
              and classify_status_payload(empty_cache) == STATUS_CACHE_EMPTY
              and classify_status_payload({}) == STATUS_MALFORMED
              and classify_status_payload({"data": {}}) == STATUS_MALFORMED)

        # 6) 两层 claim：手写 manifest 把 proof 的 false 洗成 true 必须被拒（且是 claim 原因）。
        launder_backup = write_private(os.path.join(workspace, "backup-l.bin"), b"launder body")
        os.chmod(launder_backup, 0o600)
        launder_hash = hashlib.sha256(b"launder body").hexdigest()

        def proof_node(claim: bool) -> str:
            return write_private(os.path.join(workspace, f"proof-{claim}.json"), json.dumps({
                "backup_path": launder_backup, "authoritative": claim, "consistent_snapshot": True,
                "restore_drill": False, "sha256_gz": launder_hash, "target_database": "rsetup_synth",
            }))

        true_proof = proof_node(True)
        honest_manifest = derive_acceptance_manifest(true_proof, engine="tidb",
                                                      out_path=os.path.join(workspace, "m3.json"))
        false_proof = proof_node(False)
        absolute_false = os.path.normpath(os.path.abspath(false_proof))
        laundered = json.loads(read_private_bytes(honest_manifest).decode("utf-8"))
        laundered["source_proof"] = {"path": absolute_false, "sha256": _sha256_of(absolute_false)}
        check("proof_two_layer_true_passes",
              gate_code(json.loads(read_private_bytes(honest_manifest).decode("utf-8")), "rsetup_synth") == "OK")
        check("proof_claim_laundering_blocks",
              gate_code(laundered, "rsetup_synth") == "BACKUP_PROOF_MISMATCH")

        # 7) 非固定类别的错误码不回显（console/stderr 只出固定码）。
        check("unsafe_error_code_folded",
              sanitize_code("STATUS_RPC_NOT_OBSERVED") == "STATUS_RPC_NOT_OBSERVED"
              and sanitize_code("failed for host=127.0.0.1 pw=s3cret") == UNEXPECTED_STAGE_ERROR)

        # 5) 进程所有权：SIGTERM 无效 → SIGKILL 兜底并 wait，零遗留；正常子进程只 SIGTERM。
        for ignore_term, label in ((True, "kill_fallback_and_wait"), (False, "term_is_enough")):
            sleep, clock = fake_clock_step()
            FakePopen.instances.clear()
            group = ProcessGroup(opener=lambda argv, **kw: FakePopen(argv, ignore_term=ignore_term), sleep=sleep, clock=clock)
            log_dir = os.path.join(workspace, label)  # 每轮独立目录：日志 O_EXCL 不复用
            os.mkdir(log_dir, 0o700)
            group.spawn("fake", ["/bin/true"], BASE_ENV, workspace, PrivateLog(log_dir))
            group.shutdown_all(0.5, 0.5)
            child = FakePopen.instances[0]
            kills_ok = (child.kills == 1) if ignore_term else (child.kills == 0 and group.kill_requests == 0)
            check(label, child.terminates == 1 and child.waits >= 1 and group.reaped == 1
                  and not group.orphaned and kills_ok)

        # 8) Popen 创建失败：刚打开的日志 fd 必须关掉（不泄漏 fd，也不留半启动子进程）。
        def boom_opener(argv, **kwargs):
            raise OSError("synthetic fork failure")

        fds_before = sorted(os.listdir("/proc/self/fd"))
        spawn_fail_dir = os.path.join(workspace, "spawn-fail")
        os.mkdir(spawn_fail_dir, 0o700)
        nap, tick = fake_clock_step()
        fd_group = ProcessGroup(opener=boom_opener, sleep=nap, clock=tick)
        try:
            fd_group.spawn("fake", ["/bin/true"], BASE_ENV, workspace, PrivateLog(spawn_fail_dir))
            check("spawn_failure_closes_fds", False)
        except GateError as error:
            check("spawn_failure_closes_fds",
                  error.code == PROCESS_SPAWN_FAILED and sorted(os.listdir("/proc/self/fd")) == fds_before)
    finally:
        remove_tree(workspace)

    ok = not problems
    console.emit("selftest", "ok", ok)
    return EXIT_OK if ok else EXIT_FAILED


if __name__ == "__main__":
    sys.exit(main())
