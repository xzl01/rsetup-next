"""Strict Linux private-file boundary; this API grants no reading authorization."""
import fcntl
import hashlib
import hmac
import json
import os
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
import re
import stat

_LIMIT = 65536


class ObserverError(Exception):
    """A fixed, input-free public failure payload."""

    def __init__(self):
        super().__init__("OBSERVER_REJECTED")


@dataclass(frozen=True, repr=False)
class ObserverAccount:
    user: str
    host: str


@dataclass(frozen=True, repr=False)
class ObserverAuthorization:
    engine: str
    writer_path: Path
    observer_path: Path
    writer_pin: str
    observer_pin: str
    expected_account: ObserverAccount
    run_id: str
    window_start: str
    window_end: str
    transport_policy_ref: str
    topology_ref: str
    operator_confirmed: bool
    scope: str


@dataclass(frozen=True, repr=False)
class OperatorRecord:
    """A separate, externally supplied current-window operator attestation."""

    engine: str
    writer_path: Path
    observer_path: Path
    writer_pin: str
    observer_pin: str
    expected_account: ObserverAccount
    run_id: str
    window_start: str
    window_end: str
    transport_policy_ref: str
    topology_ref: str
    operator_confirmed: bool
    scope: str


_SCOPES = frozenset((
    "observer-capabilities",
    "admission_deactivation_holds_guard_cas_waits_then_denied_on_fresh_v3_mysql_observer",
    "admission_cas_commits_before_deactivation_then_new_cas_denied_on_fresh_v3_mysql_observer",
    "admission_deactivation_holds_guard_cas_waits_then_denied_on_fresh_v3_tidb_observer",
    "admission_cas_commits_before_deactivation_then_new_cas_denied_on_fresh_v3_tidb_observer",
))
_BINDINGS = (
    "engine", "writer_path", "observer_path", "writer_pin", "observer_pin",
    "expected_account", "run_id", "window_start", "window_end",
    "transport_policy_ref", "topology_ref", "operator_confirmed", "scope",
)
_TIMESTAMP = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,6})?(?:Z|\+00:00)\Z")
_SAFE_REF = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}\Z")
_ACCOUNT_ATOM = re.compile(r"[A-Za-z0-9_.%:-]+\Z")


def _utc_timestamp(value: str) -> datetime:
    if type(value) is not str or _TIMESTAMP.fullmatch(value) is None:
        raise ObserverError()
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        raise ObserverError() from None


def authorize_and_envelope(
    auth: ObserverAuthorization, record: OperatorRecord, now: datetime, scope: str
) -> bytes:
    """Bind only a caller-provided current-window record; never create one here.

    This function neither opens paths nor treats the record itself as proof of
    operator authenticity. Its caller must verify that external provenance.
    """
    if (type(auth) is not ObserverAuthorization or type(record) is not OperatorRecord
            or auth is record or type(now) is not datetime or now.utcoffset() is None
            or type(scope) is not str):
        raise ObserverError()
    path_type = type(Path("/"))
    for candidate in (auth, record):
        if type(candidate.engine) is not str or candidate.engine not in ("mysql", "tidb"):
            raise ObserverError()
        for path in (candidate.writer_path, candidate.observer_path):
            if (type(path) is not path_type or not path.is_absolute()
                    or ".." in path.parts or len(path.parts) <= 1):
                raise ObserverError()
        if candidate.writer_path == candidate.observer_path:
            raise ObserverError()
        for pin in (candidate.writer_pin, candidate.observer_pin):
            if type(pin) is not str or re.fullmatch(r"[0-9a-f]{64}", pin) is None:
                raise ObserverError()
        if candidate.writer_pin == candidate.observer_pin:
            raise ObserverError()
        account = candidate.expected_account
        if type(account) is not ObserverAccount or any(
            type(atom) is not str or _ACCOUNT_ATOM.fullmatch(atom) is None
            for atom in (account.user, account.host)
        ):
            raise ObserverError()
        for ref in (candidate.run_id, candidate.transport_policy_ref, candidate.topology_ref):
            if type(ref) is not str or _SAFE_REF.fullmatch(ref) is None:
                raise ObserverError()
        if type(candidate.operator_confirmed) is not bool or not candidate.operator_confirmed:
            raise ObserverError()
        if type(candidate.scope) is not str or candidate.scope not in _SCOPES:
            raise ObserverError()
        if candidate.scope != "observer-capabilities" and not candidate.scope.endswith(
                "_" + candidate.engine + "_observer"):
            raise ObserverError()
        start, end = _utc_timestamp(candidate.window_start), _utc_timestamp(candidate.window_end)
        if not start < end or not start <= now < end:
            raise ObserverError()
    if scope != auth.scope or scope != record.scope or any(
        getattr(auth, name) != getattr(record, name) for name in _BINDINGS
    ):
        raise ObserverError()
    wire = json.dumps({
        "engine": auth.engine,
        "writer_pin": auth.writer_pin,
        "observer_pin": auth.observer_pin,
        "expected_account": {"user": auth.expected_account.user,
                             "host": auth.expected_account.host},
        "run_id": auth.run_id,
        "window_start": auth.window_start,
        "window_end": auth.window_end,
        "transport_policy_ref": auth.transport_policy_ref,
        "topology_ref": auth.topology_ref,
        "operator_confirmed": auth.operator_confirmed,
        "scope": scope,
    }, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    if len(wire) > _LIMIT:
        raise ObserverError()
    return wire


def _directory(s, private=False):
    mode = stat.S_IMODE(s.st_mode)
    if not stat.S_ISDIR(s.st_mode):
        raise ObserverError()
    if private:
        if s.st_uid != os.geteuid() or mode & 0o7077 or mode & 0o500 != 0o500:
            raise ObserverError()
    elif s.st_uid not in (0, os.geteuid()) or mode & 0o022:
        raise ObserverError()


def _file(s):
    mode = stat.S_IMODE(s.st_mode)
    if (not stat.S_ISREG(s.st_mode) or s.st_uid != os.geteuid()
            or mode not in (0o400, 0o600) or not 0 <= s.st_size <= _LIMIT):
        raise ObserverError()


def _identity(s):
    return (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns,
            s.st_ctime_ns, s.st_uid, s.st_gid, s.st_mode)


def open_private(path: Path) -> int:
    """Return a caller-owned read-only fd, traversing only anchored components."""
    parent = None
    result = None
    interrupt = None
    try:
        if not isinstance(path, Path) or not path.is_absolute() or path.anchor != "/":
            raise ObserverError()
        parts = path.parts[1:]
        if not parts or ".." in parts:
            raise ObserverError()
        flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
        parent = os.open("/", flags | os.O_DIRECTORY)
        _directory(os.fstat(parent))
        for component in parts[:-1]:
            child = os.open(component, flags | os.O_DIRECTORY, dir_fd=parent)
            previous, parent = parent, child
            os.close(previous)
            _directory(os.fstat(parent))
        _directory(os.fstat(parent), private=True)
        result = os.open(parts[-1], flags, dir_fd=parent)
        if result <= 2:
            raise ObserverError()
        _file(os.fstat(result))
        previous, parent = parent, None
        os.close(previous)
        fd, result = result, None
        return fd
    except (OSError, ValueError, TypeError, OverflowError):
        raise ObserverError() from None
    except (KeyboardInterrupt, SystemExit) as error:
        interrupt = error
        raise
    finally:
        close_failed = False
        for owned in (result, parent):
            if owned is not None:
                try:
                    os.close(owned)
                except OSError:
                    # Linux releases the fd even when close reports an error;
                    # retrying could close a reused descriptor.
                    close_failed = True
                except (KeyboardInterrupt, SystemExit) as error:
                    # Preserve the first interrupt (body, then cleanup order),
                    # but still attempt every remaining owned fd exactly once.
                    if interrupt is None:
                        interrupt = error
        if interrupt is not None:
            raise interrupt from None
        if close_failed:
            raise ObserverError() from None


def read_pinned(fd: int, pin: str) -> bytes:
    """Read and pin raw bytes on the same fd; never close the caller's fd."""
    try:
        if type(fd) is not int or fd <= 2:
            raise ObserverError()
        if not isinstance(pin, str) or re.fullmatch(r"[0-9a-f]{64}", pin) is None:
            raise ObserverError()
        if fcntl.fcntl(fd, fcntl.F_GETFL) & os.O_ACCMODE != os.O_RDONLY:
            raise ObserverError()
        before = os.fstat(fd)
        _file(before)
        os.lseek(fd, 0, os.SEEK_SET)
        chunks = []
        total = 0
        while total <= _LIMIT:
            chunk = os.read(fd, min(8192, _LIMIT + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
        after = os.fstat(fd)
        if total > _LIMIT or total != before.st_size or _identity(before) != _identity(after):
            raise ObserverError()
        data = b"".join(chunks)
        if not hmac.compare_digest(hashlib.sha256(data).hexdigest(), pin):
            raise ObserverError()
        return data
    except (OSError, ValueError, TypeError, OverflowError):
        raise ObserverError() from None
