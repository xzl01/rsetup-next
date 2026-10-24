# scripts/tests/test_tunnel_two_process.py
"""``scripts/tunnel_two_process.py`` 的**离线**单元测试。

只使用合成输入与假进程：不连数据库、不启动真实 controller/board、不访问网络。覆盖安全
护栏本身：标准 JSON 严格性（注释/尾随逗号/单引号/裸键/重复键/NaN 全拒）、门禁缺失即拒绝且
零 spawn、备份只验真实可验的（hash/CRC/引用/目标名/proof 绑定）且篡改必失败、
``restore_drill=not_performed`` 如实记录不被抬成已验证、审批逐项 CAS ``ok=false`` 不算成功、
符号链接拒绝、秘密不入 console/argv/汇总、任何异常与超时都 SIGTERM→有界 wait→SIGKILL 并
wait、阶段只由真实谓词推进。

临时目录一律由被测脚本的 ``prep_workspace`` 创建在仓库 ``target/tunnel-two-process-prep``
下（不使用 ``$TMPDIR``）。
"""

from __future__ import annotations

import contextlib
import gzip
import hashlib
import io
import json
import os
import stat
import subprocess
import sys
import unittest
import urllib.parse
from types import SimpleNamespace

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, REPO_ROOT)

from scripts import tunnel_two_process  # noqa: E402  (仅用于 patch 假 opener，验证零进程)
from scripts.tunnel_two_process import (  # noqa: E402
    ACK_LITERAL,
    BASE_ENV,
    EXIT_FAILED,
    EXIT_GATE_FAILED,
    EXIT_OK,
    STATUS_CACHE_EMPTY,
    STATUS_MALFORMED,
    STATUS_PRESENT,
    STATUS_TRANSPORT,
    AdminCredentials,
    BackupManifest,
    Console,
    ControllerHttp,
    Credential,
    FakePopen,
    GateError,
    ProcessGroup,
    PrivateLog,
    Redactor,
    TwoProcessRun,
    assert_no_symlink,
    derive_acceptance_manifest,
    ed25519_pkcs8_v1_to_v2,
    fake_clock_step,
    generate_ed25519_identity,
    load_json_document,
    main,
    parse_credential,
    prep_workspace,
    prepare_state_dir,
    private_file,
    read_private_bytes,
    remove_tree,
    require_binary,
    wait_until,
    write_private,
)

DB_NAME = "rsetup_two_proc_synth"
SPKI = bytes.fromhex("302a300506032b6570032100") + bytes(range(32))
# 板端事件行的真实格式：`board-connect event=<Kind> attempt=<n> stop=<..>`（stderr）。
BOARD_READY = 'board-connect event=Ready attempt=0 stop=None\nboard-connect event=Stopped attempt=0 stop=Some("revoked")\n'
BOARD_STUCK = "board-connect event=AttemptStarted attempt=0 stop=None\n"
BOARD_PREFLIGHT = "board-connect event=PreflightFailed attempt=0 stop=None\n"
# 单测里唯一的合成 secret 标记：任何泄漏到 console/stderr/argv 都算失败。
LEAK_TOKEN = "Synth-Leak-Token-91337"


def typed_status_data() -> dict:
    """真实 `GET /api/v1/devices/{id}/status` 的**有样本**投影（http_device_status.rs:324-398）。"""
    return {
        "snapshot": {
            "schema_version": 1,
            "boot_id": "boot-1",
            "agent_epoch": "epoch-1",
            "sample_seq": "9007199254740993",
            "sampled_unix_ms": "1760000000000",
            "sampled_monotonic_ns": "42",
            "clock_epoch": "00000000-0000-0000-0000-000000000001",
            "metrics": [{"key": "cpu.load1", "value": 0.5, "unit": "ratio", "quality": "measured"}],
            "clock_quality": "ntp_valid",
        },
        "received_time": {"system_wall_utc": "2026-10-18T12:00:00Z", "quality": "ntp_valid"},
        "freshness": "fresh",
        "age_ms": "1500",
    }


def empty_cache_status_data() -> dict:
    """空缓存（真实形状）：`snapshot=null`、`freshness="unknown"`、`age_ms=null`。"""
    return {
        "snapshot": None,
        "received_time": {"system_wall_utc": "2026-10-18T12:00:00Z", "quality": "system_fallback"},
        "freshness": "unknown",
        "age_ms": None,
    }


def synthetic_credential() -> dict:
    """已授权目标库 JSON 的形状：**五个平铺字段**，不嵌套 database/admin。"""
    return {
        "host": "127.0.0.1",
        "port": 3306,
        "user": "svc_synth",
        "password": "Synth-Pw/21?#42",
        "database": DB_NAME,
    }


def read_bytes(path: str) -> bytes:
    with open(path, "rb") as handle:
        return handle.read()


def read_text(path: str) -> str:
    with open(path, encoding="utf-8") as handle:
        return handle.read()


def synthetic_admin(**overrides) -> dict:
    node = {"username": "admin", "password": "synthetic-admin-pw", "new_password": "synthetic-new-pw"}
    node.update(overrides)
    return node


class Workspace(unittest.TestCase):
    """每例一个私有工作区（0700），落在仓库 target 下；产物绝不进仓库跟踪目录。"""

    def setUp(self) -> None:
        self.workspace = prep_workspace(self._testMethodName.lower()[:24], REPO_ROOT)
        self.state = os.path.join(self.workspace, "state")
        os.mkdir(self.state, 0o700)
        os.chmod(self.state, 0o700)
        self.stream = io.StringIO()
        self.console = Console(self.stream)

    def tearDown(self) -> None:
        remove_tree(self.workspace)

    def credential_file(self, node: dict | None = None) -> str:
        return write_private(
            os.path.join(self.workspace, "credential.json"),
            json.dumps(node if node is not None else synthetic_credential()),
        )

    def admin_file(self, node: dict | None = None) -> str:
        return write_private(
            os.path.join(self.workspace, "admin.json"),
            json.dumps(node if node is not None else synthetic_admin()),
        )

    def backup(self, body: bytes | None = None, *, database: str = DB_NAME) -> tuple[str, str]:
        """合成 gzip 备份 + 真实 proof，返回 (备份路径, proof 路径)。"""
        raw = b"synthetic backup stream\n" * 4
        body = gzip.compress(raw) if body is None else body
        backup_path = write_private(os.path.join(self.workspace, "backup.sql.gz"), body)
        os.chmod(backup_path, 0o600)
        proof = write_private(os.path.join(self.workspace, "proof.json"), json.dumps({
            "backup_path": backup_path,
            "authoritative": True,
            "consistent_snapshot": True,
            "restore_drill": False,  # 真实：未做恢复演练
            "sha256_gz": hashlib.sha256(body).hexdigest(),
            "sha256_raw": hashlib.sha256(raw).hexdigest(),
            "table_count": 7,
            "rows_total": 42,
            "target_database": database,
            "metadata": {"schema_version": 2},
        }))
        return backup_path, proof

    def manifest_file(self, *, proof: str | None = None, engine: str = "tidb", name: str = "manifest.json") -> str:
        source = proof or self.backup()[1]
        return derive_acceptance_manifest(source, engine=engine, out_path=os.path.join(self.workspace, name))

    def patch_manifest(self, path: str, **changes) -> str:
        node = json.loads(read_text(path))
        node.update(changes)
        write_private(path, json.dumps(node))
        return path


# ------------------------------------------------------------------ 标准 JSON


class JsonDocumentTests(unittest.TestCase):
    def test_accepts_plain_json(self) -> None:
        self.assertEqual(load_json_document('{ "a": [1, 2], "b": null }'), {"a": [1, 2], "b": None})

    def test_rejects_json5_forms(self) -> None:
        """原脚本的「JSON5 子集」被收缩掉：注释/尾随逗号/单引号/裸键**必须**被拒。"""
        forms = (
            '{ // c\n"a": 1}',
            '{"a": 1 /* x */}',
            '{"a": 1,}',
            "{'a': 1}",
            "{a: 1}",
            '"unterminated',
            '{"a": 1} trailing',
        )
        for text in forms:
            with self.subTest(text=text):
                with self.assertRaises(GateError) as caught:
                    load_json_document(text)
                self.assertEqual(caught.exception.code, "INPUT_BAD_JSON")

    def test_rejects_duplicate_keys_and_non_finite(self) -> None:
        for text in ('{"a": 1, "a": 2}', '{"a": NaN}', '[Infinity]', '{"a": -Infinity}'):
            with self.subTest(text=text):
                self.assertRaises(GateError, load_json_document, text)


# ------------------------------------------------------------------ 凭据


class CredentialTests(unittest.TestCase):
    def test_flat_five_fields_accepted_unchanged(self) -> None:
        credential = parse_credential(synthetic_credential())
        self.assertEqual((credential.host, credential.port, credential.user, credential.database),
                         ("127.0.0.1", 3306, "svc_synth", DB_NAME))

    def test_database_name_may_contain_hyphen(self) -> None:
        """合法库名可含连字符：旧 `NAME_RE` 会把真实授权库名错拒成 `CREDENTIAL_FIELD_TYPE`。

        放宽的**只有字符集**：长度上限、首字符规则、以及 URL path 段仍 percent-encode 都不变。
        """
        node = synthetic_credential()
        node["database"] = "rsetup-synth-v4"
        credential = parse_credential(node)
        self.assertEqual(credential.database, "rsetup-synth-v4")
        # 连字符在 URL path 段无需转义，但同段的其它特殊字符仍必须 percent-encode。
        self.assertTrue(credential.dsn().endswith("/rsetup-synth-v4"))
        node["database"] = "rsetup$synth-v4"
        self.assertTrue(parse_credential(node).dsn().endswith("/rsetup%24synth-v4"))

    def test_database_name_rejects_invalid_forms(self) -> None:
        """允许 `-` 不等于放宽输入范围：空白/控制字符/引号/分号/超长/尾随换行仍拒。"""
        for value in ("bad name", "bad;drop", "bad'name", 'bad"name', "bad\x00name",
                      "bad\nname", "bad\tname", "-leading-dash", "ünicode-db", "db" * 33, ""):
            node = synthetic_credential()
            node["database"] = value
            with self.subTest(database=repr(value)), self.assertRaises(GateError) as caught:
                parse_credential(node)
            self.assertEqual(caught.exception.code, "CREDENTIAL_FIELD_TYPE")

    def test_user_name_may_contain_hyphen(self) -> None:
        """真实授权 DB 用户名与库名同值且含合法 `-`（可信 prepare 的语法事实）：旧 `NAME_RE`
        禁 dash 会把合法账号错拒成 `CREDENTIAL_FIELD_TYPE`。user 在此只作 DSN userinfo 字段
        （非 SQL identifier），DSN 段仍以 `quote(safe='')` percent-encode，从不裸拼接。
        """
        node = synthetic_credential()
        node["user"] = "rsetup-synth-user"
        node["database"] = "rsetup-synth-db"
        credential = parse_credential(node)
        self.assertEqual((credential.user, credential.database), ("rsetup-synth-user", "rsetup-synth-db"))
        # 构造 DSN 后反读解析回相同 user（不实际连接）：DSN userinfo 段与库名反解一致。
        parsed = urllib.parse.urlsplit(credential.dsn())
        self.assertEqual(urllib.parse.unquote(parsed.username), "rsetup-synth-user")
        self.assertEqual(urllib.parse.unquote(parsed.path.lstrip("/")), "rsetup-synth-db")
        # 若用户名含真正需要转义的字符，仍走 percent-encode，不裸拼进 DSN。
        node["user"] = "rsetup$synth-user"
        self.assertTrue(parse_credential(node).dsn().startswith("mysql://rsetup%24synth-user:"))

    def test_user_name_rejects_invalid_forms(self) -> None:
        """允许 `-` 不等于放宽 user 输入范围：空白/控制字符/引号/分号/超长/尾随换行仍拒。"""
        for value in ("bad name", "bad;drop", "bad'name", 'bad"name', "bad\x00name",
                      "bad\nname", "bad\tname", "-leading-dash", "ünicode-user", "u" * 65, ""):
            node = synthetic_credential()
            node["user"] = value
            with self.subTest(user=repr(value)), self.assertRaises(GateError) as caught:
                parse_credential(node)
            self.assertEqual(caught.exception.code, "CREDENTIAL_FIELD_TYPE")

    def test_dsn_percent_encodes_reserved_characters(self) -> None:
        dsn = parse_credential(synthetic_credential()).dsn()
        self.assertTrue(dsn.startswith("mysql://svc_synth:"))
        self.assertIn("Synth-Pw%2F21%3F%2342", dsn)
        self.assertNotIn("Synth-Pw/21?#42", dsn)
        self.assertTrue(dsn.endswith(f"/{DB_NAME}"))

    def test_rejects_unknown_and_missing_fields(self) -> None:
        for mutate, code in (
            (lambda n: n.update({"params": {}}), "CREDENTIAL_UNKNOWN_FIELD"),
            (lambda n: n.update({"database": {"name": DB_NAME}}), "CREDENTIAL_FIELD_TYPE"),
            (lambda n: n.pop("user"), "CREDENTIAL_FIELD_MISSING"),
            (lambda n: n.update({"port": "3306"}), "CREDENTIAL_FIELD_TYPE"),
        ):
            node = synthetic_credential()
            mutate(node)
            with self.subTest(code=code), self.assertRaises(GateError) as caught:
                parse_credential(node)
            self.assertEqual(caught.exception.code, code)

    def test_admin_is_separate_and_explicit(self) -> None:
        admin = AdminCredentials(synthetic_admin())
        self.assertEqual(admin.username, "admin")
        self.assertIsNone(AdminCredentials({"username": "admin"}).password)
        with self.assertRaises(GateError) as caught:
            AdminCredentials({"username": "admin", "password": "p", "bootstrap_secret_log": True})
        self.assertEqual(caught.exception.code, "ADMIN_UNKNOWN_FIELD")

    def test_nested_database_object_is_rejected(self) -> None:
        """旧实现自造的 ``{"database": {...}, "admin": {...}}`` 形状必须被拒（不再兼容）。"""
        with self.assertRaises(GateError) as caught:
            Credential({"database": {"host": "127.0.0.1", "name": DB_NAME, "user": "svc"}, "admin": {"username": "admin"}})
        self.assertEqual(caught.exception.code, "CREDENTIAL_UNKNOWN_FIELD")

    def test_credential_file_never_carries_admin(self) -> None:
        node = synthetic_credential()
        node["admin"] = synthetic_admin()
        with self.assertRaises(GateError) as caught:
            parse_credential(node)
        self.assertEqual(caught.exception.code, "CREDENTIAL_UNKNOWN_FIELD")


# ------------------------------------------------------------------ Ed25519 身份 PKCS#8


class IdentityPkcs8Tests(Workspace):
    """openssl genpkey 的 PKCS#8 **v1** → ring `from_pkcs8` 接受的 **v2**（含 public、核一致性）。

    根因（ring 0.17.14 `ed25519/signing.rs:62-90`）：`from_pkcs8` 走 `Version::V2Only` 且要求
    publicKey 与私钥一致；openssl `genpkey` 只产 v1（version=0、无 public），故被拒。本项目**不**
    放宽为 `from_pkcs8_maybe_unchecked`，改为把固定 v1 重排为与 ring `generate_pkcs8` 逐字节相同的 v2。
    """

    def _runner(self, seed: bytes, public_key: bytes):
        def runner(argv, **kwargs):
            argv = list(argv)
            text = " ".join(argv)
            if "genpkey" in text:
                write_private(argv[argv.index("-out") + 1],
                              tunnel_two_process.ED25519_PKCS8_V1_PREFIX + seed)  # openssl v1 固定 48B
                return subprocess.CompletedProcess(argv, 0)
            if "pkey" in text:
                return SimpleNamespace(stdout=tunnel_two_process.ED25519_SPKI_PREFIX + public_key)
            raise AssertionError("unexpected argv")

        return runner

    def test_openssl_v1_reencoded_to_ring_v2(self) -> None:
        seed = bytes(range(32))
        public_key = bytes(255 - i for i in range(32))  # 与 seed 不同：证明 public 段来自 openssl 派生而非 seed
        state = os.path.join(self.workspace, "id")
        os.mkdir(state, 0o700)
        os.chmod(state, 0o700)
        returned = generate_ed25519_identity(state, "ctl", sys.executable, self._runner(seed, public_key))
        self.assertEqual(returned, public_key)
        written = read_private_bytes(os.path.join(state, "ctl", "identity.pk8"))
        # 落盘的 v2 与 ring 模板拼接逐字节一致：prefix ‖ seed ‖ [1]{00} ‖ public（83B、version=1）。
        self.assertEqual(written, tunnel_two_process.ED25519_PKCS8_V2_PREFIX + seed
                                  + tunnel_two_process.ED25519_PKCS8_V2_PUB_MIDDLE + public_key)
        self.assertEqual(len(written), 83)
        self.assertEqual(written[4], 1)        # PKCS#8 v2 version 必须为 1
        self.assertEqual(written[48], 0x81)    # legacy [1] public 包裹（ring accept_legacy 明确接受）
        self.assertEqual(written[51:], public_key)

    def test_converter_ring_shape_and_rejects_bad(self) -> None:
        seed = bytes(range(32))
        public_key = bytes(range(32, 64))
        v2 = ed25519_pkcs8_v1_to_v2(tunnel_two_process.ED25519_PKCS8_V1_PREFIX + seed, public_key)
        self.assertEqual(len(v2), 83)
        self.assertEqual(v2[1], 0x51)          # 外层 SEQUENCE 定长 0x51 = 83-2
        self.assertEqual(v2, tunnel_two_process.ED25519_PKCS8_V2_PREFIX + seed
                            + tunnel_two_process.ED25519_PKCS8_V2_PUB_MIDDLE + public_key)
        for bad_v1, bad_pub in (
            (b"", public_key),
            (b"\x00" * 48, public_key),
            (tunnel_two_process.ED25519_PKCS8_V1_PREFIX + seed + b"\x00", public_key),
            (tunnel_two_process.ED25519_PKCS8_V1_PREFIX + seed, b"\x00" * 31),
        ):
            with self.subTest(v1=len(bad_v1), pub=len(bad_pub)), self.assertRaises(GateError) as caught:
                ed25519_pkcs8_v1_to_v2(bad_v1, bad_pub)
            self.assertEqual(caught.exception.code, "IDENTITY_PKCS8_SHAPE")


# ------------------------------------------------------------------ 备份门禁


class BackupManifestTests(Workspace):
    def test_derive_copies_claims_and_binds_proof(self) -> None:
        path = self.manifest_file()
        node = json.loads(read_text(path))
        self.assertEqual(node["restore_drill"], "not_performed")
        self.assertEqual(node["authoritative"], True)
        self.assertEqual(node["engine"], "tidb")
        self.assertNotIn("metadata", node)  # 备份自身 schema_version 与门禁无关
        manifest = BackupManifest(node)
        manifest.verify(target_database=DB_NAME)
        facts = manifest.facts()
        self.assertTrue(facts["backup_gz_hash_verified"])
        self.assertTrue(facts["backup_gzip_crc_verified"])  # 真实 gzip CRC + raw sha256
        self.assertTrue(facts["backup_proof_hash_bound"])
        self.assertFalse(facts["restore_drill_performed"])
        self.assertFalse(facts["restore_verified"])  # 未演练绝不称已验证可恢复

    def test_missing_claims_are_not_defaulted_true(self) -> None:
        _, proof = self.backup()
        write_private(proof, json.dumps({
            "backup_path": self.workspace + "/backup.sql.gz",
            "restore_drill": False,
            "sha256_gz": "0" * 64,
            "target_database": DB_NAME,
        }))
        with self.assertRaises(GateError) as caught:
            derive_acceptance_manifest(proof, engine="tidb", out_path=os.path.join(self.workspace, "m.json"))
        self.assertEqual(caught.exception.code, "BACKUP_FIELD_MISSING")

    def test_claim_false_is_rejected(self) -> None:
        path = self.manifest_file()
        for value in (False, "true", 1):
            node = json.loads(read_text(path))
            node["consistent_snapshot"] = value
            write_private(path, json.dumps(node))
            with self.subTest(value=value), self.assertRaises(GateError) as caught:
                BackupManifest(node)
            self.assertEqual(caught.exception.code, "BACKUP_CLAIM_NOT_TRUE")

    def test_restore_drill_true_without_evidence_is_refused(self) -> None:
        _, proof = self.backup()
        node = json.loads(read_text(proof))
        node["restore_drill"] = True  # 光一个 true 不足以确认演练
        write_private(proof, json.dumps(node))
        with self.assertRaises(GateError) as caught:
            derive_acceptance_manifest(proof, engine="tidb", out_path=os.path.join(self.workspace, "m.json"))
        self.assertEqual(caught.exception.code, "BACKUP_RESTORE_DRILL_UNDECLARED")

    def test_tampered_backup_or_manifest_blocks(self) -> None:
        backup_path, proof = self.backup()
        path = self.manifest_file(proof=proof)
        write_private(backup_path, read_bytes(backup_path) + b"tail")
        with self.assertRaises(GateError) as caught:
            BackupManifest(json.loads(read_text(path))).verify(target_database=DB_NAME)
        self.assertEqual(caught.exception.code, "BACKUP_HASH_MISMATCH")

    def test_manifest_hash_edit_fails_proof_binding(self) -> None:
        _, proof = self.backup()
        path = self.manifest_file(proof=proof)
        self.patch_manifest(path, sha256_gz="0" * 64)
        with self.assertRaises(GateError) as caught:
            BackupManifest(json.loads(read_text(path))).verify(target_database=DB_NAME)
        self.assertEqual(caught.exception.code, "BACKUP_PROOF_MISMATCH")

    def test_corrupt_gzip_crc_is_detected(self) -> None:
        raw = b"synthetic backup stream\n" * 4
        body = bytearray(gzip.compress(raw))
        body[-2] ^= 0xFF  # 破 gzip trailer 的 CRC
        backup_path, proof = self.backup(body=bytes(body))
        path = self.manifest_file(proof=proof)
        node = json.loads(read_text(path))
        node["sha256_gz"] = hashlib.sha256(bytes(body)).hexdigest()
        write_private(path, json.dumps(node))
        with self.assertRaises(GateError) as caught:
            BackupManifest(node).verify(target_database=DB_NAME)
        self.assertEqual(caught.exception.code, "BACKUP_GZIP_CORRUPT")
        self.assertTrue(os.path.isabs(backup_path))

    def test_wrong_target_database_blocks(self) -> None:
        node = json.loads(read_text(self.manifest_file()))
        with self.assertRaises(GateError) as caught:
            BackupManifest(node).verify(target_database="some_other_database")
        self.assertEqual(caught.exception.code, "BACKUP_TARGET_MISMATCH")

    # -------------------------------------- M2：proof 与 manifest 两层 claim 必须各自显式为 true

    def _handwritten_manifest(self, proof: str, backup: str, **overrides) -> dict:
        """手写 canonical manifest：claims 全部**手写 true**，proof hash/摘要都用合法值。"""
        node = {
            "manifest_version": "rsetup-two-process-backup/v1",
            "engine": "tidb",
            "target_database": DB_NAME,
            "backup_path": backup,
            "sha256_gz": hashlib.sha256(read_bytes(backup)).hexdigest(),
            "authoritative": True,
            "consistent_snapshot": True,
            "restore_drill": "not_performed",
            "source_proof": {"path": proof, "sha256": hashlib.sha256(read_bytes(proof)).hexdigest()},
        }
        node.update(overrides)
        return node

    def _mutate_proof(self, proof: str, claim: str, value) -> None:
        """改 proof 里的 claim 并**重新绑定** source_proof.sha256：排除 hash 不匹配的干扰，
        只留「claim 方向」这一维被检。"""
        node = json.loads(read_text(proof))
        node[claim] = value
        write_private(proof, json.dumps(node))

    def test_proof_claim_false_laundered_by_handwritten_true_manifest_is_rejected(self) -> None:
        """reviewer 反例：proof `authoritative=false` + 手写 manifest `true` **必须**被拒。"""
        backup, proof = self.backup()
        self._mutate_proof(proof, "authoritative", False)
        manifest = self._handwritten_manifest(proof, backup)
        with self.assertRaises(GateError) as caught:
            BackupManifest(dict(manifest)).verify(target_database=DB_NAME)
        self.assertEqual(caught.exception.code, "BACKUP_PROOF_MISMATCH")

    def test_proof_claim_must_be_true_for_either_claim(self) -> None:
        """两个 claim 逐个检：任一 proof claim 不是显式 true 都拒。"""
        backup, proof = self.backup()
        for claim in ("authoritative", "consistent_snapshot"):
            for value in (False, "true", 1, None):
                backup, proof = self.backup()  # 每轮重新生成：只让一个维度变化
                self._mutate_proof(proof, claim, value)
                manifest = self._handwritten_manifest(proof, backup)  # 重新绑定 proof 当前 hash
                with self.subTest(claim=claim, value=value), self.assertRaises(GateError) as caught:
                    BackupManifest(manifest).verify(target_database=DB_NAME)
                # 非布尔（null/字符串/整数）是形状问题；显式 false 是与 manifest 声明不一致。
                self.assertEqual(
                    caught.exception.code,
                    "BACKUP_PROOF_MISMATCH" if isinstance(value, bool) else "BACKUP_PROOF_SHAPE",
                )

    def test_proof_claim_missing_is_rejected(self) -> None:
        backup, proof = self.backup()
        node = json.loads(read_text(proof))
        node.pop("consistent_snapshot")
        write_private(proof, json.dumps(node))
        manifest = self._handwritten_manifest(proof, backup)
        with self.assertRaises(GateError) as caught:
            BackupManifest(manifest).verify(target_database=DB_NAME)
        self.assertEqual(caught.exception.code, "BACKUP_PROOF_SHAPE")

    def test_proof_hash_tampering_still_blocks(self) -> None:
        """修 M2 不能把 hash 绑定弄松：改了 proof 却不改 hash 必须拒。"""
        backup, proof = self.backup()
        manifest = self._handwritten_manifest(proof, backup)
        self._mutate_proof(proof, "authoritative", False)  # 不重新绑定 sha256
        with self.assertRaises(GateError) as caught:
            BackupManifest(manifest).verify(target_database=DB_NAME)
        self.assertEqual(caught.exception.code, "BACKUP_PROOF_HASH_MISMATCH")

    def test_two_layer_true_path_still_passes(self) -> None:
        """两层都是显式 true 的合法路径必须仍然通过（修复不能反向变成永假）。"""
        backup, proof = self.backup()
        manifest = self._handwritten_manifest(proof, backup)
        node = BackupManifest(manifest)
        node.verify(target_database=DB_NAME)
        facts = node.facts()
        self.assertTrue(facts["backup_proof_hash_bound"])
        self.assertFalse(facts["restore_verified"])  # restore_drill 仍如实 not_performed

    def test_proof_file_must_be_0600(self) -> None:
        """proof 是私有输入：0644 直接拒（脚本不读非私有文件）。"""
        backup, proof = self.backup()
        os.chmod(proof, 0o644)
        manifest = self._handwritten_manifest(proof, backup)
        with self.assertRaises(GateError) as caught:
            BackupManifest(manifest).verify(target_database=DB_NAME)
        self.assertEqual(caught.exception.code, "INPUT_FILE_MODE")

    def test_derived_manifest_file_is_0600(self) -> None:
        _, proof = self.backup()
        path = self.manifest_file(proof=proof)
        self.assertEqual(stat.S_IMODE(os.stat(path).st_mode), 0o600)

    # -------------------------------------- 目标库标识符：合法连字符必须能过

    def test_target_database_with_hyphen_binds_normally(self) -> None:
        """库名带合法连字符时，manifest 校验与真实目标绑定都照过；换库名仍必拒。"""
        dashed = "rsetup-synth-v4"
        backup_path, proof = self.backup(database=dashed)
        node = BackupManifest(self._handwritten_manifest(proof, backup_path, target_database=dashed))
        node.verify(target_database=dashed)
        self.assertTrue(node.facts()["backup_target_bound"])
        with self.assertRaises(GateError) as caught:
            wrong = BackupManifest(self._handwritten_manifest(proof, backup_path, target_database=dashed))
            wrong.verify(target_database="some_other_database")
        self.assertEqual(caught.exception.code, "BACKUP_TARGET_MISMATCH")

    def test_target_database_rejects_invalid_forms(self) -> None:
        """放宽的只是字符集：空白/控制字符/前导连字符/超长在 manifest 侧同样被拒。"""
        backup_path, proof = self.backup()
        for value in ("bad name", "bad;drop", "-leading-dash", "bad\x00name", "bad\nname",
                      "ünicode-db", "db" * 33):
            node = self._handwritten_manifest(proof, backup_path, target_database=value)
            with self.subTest(value=repr(value)), self.assertRaises(GateError) as caught:
                BackupManifest(node)
            self.assertEqual(caught.exception.code, "BACKUP_FIELD_TYPE")


# ------------------------------------------------------------------ 路径卫生


class PathHygieneTests(Workspace):
    def test_symlinked_state_dir_rejected(self) -> None:
        link = os.path.join(self.workspace, "link-state")
        os.symlink(self.state, link)
        with self.assertRaises(GateError) as caught:
            prepare_state_dir(link, create=False)
        self.assertEqual(caught.exception.code, "SYMLINK_NOT_ALLOWED")

    def test_symlink_ancestor_rejected(self) -> None:
        link = os.path.join(self.workspace, "anc")
        os.symlink("/nonexistent-target", link)
        with self.assertRaises(GateError):
            assert_no_symlink(os.path.join(link, "deeper", "file"))

    def test_state_dir_mode_must_be_exactly_0700(self) -> None:
        loose = os.path.join(self.workspace, "loose")
        os.mkdir(loose, 0o755)
        os.chmod(loose, 0o755)
        with self.assertRaises(GateError) as caught:
            prepare_state_dir(loose, create=False)
        self.assertEqual(caught.exception.code, "STATE_DIR_MODE")

    def test_relative_state_dir_rejected(self) -> None:
        with self.assertRaises(GateError) as caught:
            prepare_state_dir("relative/state", create=True)
        self.assertEqual(caught.exception.code, "STATE_DIR_NOT_ABSOLUTE")

    def test_state_dir_under_repo_target_is_gitignored(self) -> None:
        # 真实 `git check-ignore`：证明脚本临时区确实落在被忽略的 target 下。
        prepare_state_dir(self.state, create=False, repo_root=REPO_ROOT)

    def test_insecure_input_file_mode_rejected(self) -> None:
        path = write_private(os.path.join(self.workspace, "cred.json"), "{}")
        os.chmod(path, 0o644)
        with self.assertRaises(GateError) as caught:
            private_file(path, must_exist=True)
        self.assertEqual(caught.exception.code, "INPUT_FILE_MODE")

    def test_symlinked_input_file_rejected(self) -> None:
        real = write_private(os.path.join(self.workspace, "real.json"), "{}")
        link = os.path.join(self.workspace, "link.json")
        os.symlink(real, link)
        with self.assertRaises(GateError) as caught:
            private_file(link, must_exist=True)
        self.assertEqual(caught.exception.code, "SYMLINK_NOT_ALLOWED")

    def test_state_dir_inside_repo_requires_gitignore(self) -> None:
        calls: list = []

        def runner(root: str, path: str) -> bool:
            calls.append((root, path))
            return False

        with self.assertRaises(GateError) as caught:
            prepare_state_dir(self.state, create=False, repo_root=self.workspace, git_runner=runner)
        self.assertEqual(caught.exception.code, "STATE_DIR_NOT_GITIGNORED")
        self.assertEqual(len(calls), 1)

    def test_state_dir_outside_repo_skips_git_check(self) -> None:
        prepare_state_dir(self.state, create=False, repo_root="/somewhere/else", git_runner=lambda r, p: False)

    def test_workspace_never_uses_tmpdir(self) -> None:
        self.assertTrue(self.workspace.startswith(os.path.join(REPO_ROOT, "target", "tunnel-two-process-prep")))
        self.assertEqual(stat.S_IMODE(os.stat(self.workspace).st_mode), 0o700)

    def test_binary_must_be_absolute_and_executable(self) -> None:
        with self.assertRaises(GateError) as caught:
            require_binary("relative/rsetup-controller")
        self.assertEqual(caught.exception.code, "BINARY_NOT_ABSOLUTE")
        with self.assertRaises(GateError) as caught:
            require_binary(os.path.join(self.workspace, "missing"))
        self.assertEqual(caught.exception.code, "BINARY_MISSING")
        self.assertEqual(require_binary("/usr/bin/false"), "/usr/bin/false")


# ------------------------------------------------------------------ 门禁先于 spawn


class GateBlocksSpawnTests(Workspace):
    """门禁不通过必须在任何 spawn 之前返回：state 目录保持空即证明未启动。"""

    def _argv(self, *, credential=None, backup=None, admin=None, execute=True, ack=ACK_LITERAL) -> list:
        argv = [
            "--credential-file", credential if credential is not None else self.credential_file(),
            "--backup-manifest", backup if backup is not None else self.manifest_file(),
            "--state-dir", self.state,
            "--controller-bin", "/usr/bin/false",
            "--board-bin", "/usr/bin/false",
        ]
        if admin is not False:
            argv += ["--admin-file", admin if admin is not None else self.admin_file()]
        if execute:
            argv += ["--execute", "--ack", ack]
        return argv

    def _assert_never_spawned(self) -> None:
        entries = os.listdir(self.state)
        self.assertEqual([name for name in entries if name.endswith(".stdout.log")], [])
        for name in ("two-process-summary.json", "controller-bootstrap-secret.log", "controller-id"):
            self.assertNotIn(name, entries)

    def test_dry_run_launches_no_acceptance_process(self) -> None:
        """dry-run **零验收进程**：连进程组都不构造（controller/board-agent 无从启动）。

        唯一允许的子进程是仓库内只读 `git check-ignore`（确认 state 目录被 gitignored）；
        它不启动 controller/board-agent，也不连数据库、不访问网络。
        """
        original = tunnel_two_process.ProcessGroup

        class NoProcessGroup:
            def __init__(self, *args, **kwargs):  # noqa: ANN002, ANN003
                raise AssertionError("dry-run 不得构造进程组")

        tunnel_two_process.ProcessGroup = NoProcessGroup
        try:
            code = main(self._argv(execute=False), repo_root=REPO_ROOT, console=self.console)
        finally:
            tunnel_two_process.ProcessGroup = original
        self.assertEqual(code, EXIT_OK)
        self.assertIn("spawned=false", self.stream.getvalue())
        self.assertEqual(os.listdir(self.state), [])

    def test_dry_run_evaluates_gates_without_launching(self) -> None:
        self.assertEqual(main(self._argv(execute=False), repo_root=None, console=self.console), EXIT_OK)
        text = self.stream.getvalue()
        self.assertIn("spawned=false", text)
        self.assertIn("restore_drill_performed=false", text)
        self.assertIn("plan ok=true", text)
        self.assertEqual(os.listdir(self.state), [])

    def test_missing_credential_file_blocks(self) -> None:
        argv = self._argv(credential=os.path.join(self.workspace, "absent.json"))
        self.assertEqual(main(argv, repo_root=None, console=self.console), EXIT_GATE_FAILED)
        self._assert_never_spawned()

    def test_backup_file_tamper_blocks_before_spawn(self) -> None:
        backup_path, proof = self.backup()
        write_private(backup_path, read_bytes(backup_path) + b"x")
        argv = self._argv(backup=self.manifest_file(proof=proof))
        self.assertEqual(main(argv, repo_root=None, console=self.console), EXIT_GATE_FAILED)
        self._assert_never_spawned()

    def test_backup_wrong_target_blocks(self) -> None:
        _, proof = self.backup()
        node = json.loads(read_text(proof))
        node["target_database"] = "some_other_database"
        write_private(proof, json.dumps(node))
        argv = self._argv(backup=self.manifest_file(proof=proof, name="m2.json"))
        self.assertEqual(main(argv, repo_root=None, console=self.console), EXIT_GATE_FAILED)
        self._assert_never_spawned()

    def test_missing_ack_blocks_execution(self) -> None:
        argv = self._argv(ack="ISOLATED_TARGET_AND_BACKUP_VERIFIED_BUT_WRONG")
        self.assertEqual(main(argv, repo_root=None, console=self.console), EXIT_GATE_FAILED)
        self._assert_never_spawned()

    def test_execute_without_admin_source_is_needs_context(self) -> None:
        argv = self._argv(admin=False)
        errors = io.StringIO()
        with contextlib.redirect_stderr(errors):
            code = main(argv, repo_root=None, console=self.console)
        self.assertEqual(code, EXIT_GATE_FAILED)
        # 管理员来源未知：精确 NEEDS_CONTEXT，且从不 SQL 改口令、从不启动。
        self.assertIn("gate_error=NEEDS_CONTEXT", errors.getvalue())
        self._assert_never_spawned()

    def test_missing_inputs_blocked(self) -> None:
        self.assertEqual(main(["--state-dir", self.state], repo_root=None, console=self.console), EXIT_GATE_FAILED)


# ------------------------------------------------------------------ 脱敏


class ConsoleRedactionTests(Workspace):
    def test_console_rejects_free_text(self) -> None:
        for value in ("mysql://user:pw@host/db", GateError("X"), "admin"):
            with self.subTest(value=value), self.assertRaises(TypeError):
                self.console.emit("plan", "detail", value)

    def test_redactor_scrubs_registered_values(self) -> None:
        redactor = Redactor()
        redactor.add("Synth-Pw/21?#42")
        redactor.add("ab")
        self.assertEqual(redactor.scrub("a=Synth-Pw/21?#42 b"), "a=[redacted] b")
        self.assertIn("ab", redactor.scrub("able"))

    def test_credential_registers_host_user_database(self) -> None:
        """裸 `host`/`user`/`database` 也登记：它们可能随异常文本或私有日志漂出去。"""
        redactor = Redactor()
        credential = parse_credential(synthetic_credential(), redactor)
        for value in (credential.host, credential.user, credential.database, credential.password):
            self.assertNotIn(value, redactor.scrub(f"note {value} tail"))
        self.assertNotIn(credential.dsn(), redactor.scrub(f"url={credential.dsn()}&x=1"))
        # 含连字符的真实库名同样被整段脱敏。
        node = synthetic_credential()
        node["database"] = "rsetup-synth-v4"
        other = Redactor()
        parse_credential(node, other)
        self.assertEqual(other.scrub("db=rsetup-synth-v4;"), "db=[redacted];")

    def test_identifier_scrub_keeps_fixed_tokens(self) -> None:
        """身份字段只做**整段**替换：短值/常见值不得把固定类别码或 JSON 结构打碎。"""
        redactor = Redactor()
        parse_credential(synthetic_credential(), redactor)
        summary = ('{"complete": false, "failure_codes": ["STATUS_RPC_NOT_OBSERVED"], '
                   '"status_probe_category": "transport", "failed_stage": "stage_status_poll"}')
        self.assertEqual(redactor.scrub(summary), summary)
        short = Redactor()
        short.add_identifier("prt")         # 低于 4 字符阈值：不登记
        self.assertEqual(short.scrub("transport prt"), "transport prt")
        short.add_identifier("port")        # 登记后也只在**整段**命中，embedded 的不动
        self.assertEqual(short.scrub("transport port x"), "transport [redacted] x")

    def test_unknown_stage_rejected(self) -> None:
        with self.assertRaises(AssertionError):
            self.console.emit("stage containing secret", "ok", True)


# ------------------------------------------------------------------ 进程所有权


class ProcessOwnershipTests(Workspace):
    def setUp(self) -> None:
        super().setUp()
        FakePopen.instances.clear()
        self.log = PrivateLog(self.workspace)

    def _group(self, opener) -> ProcessGroup:
        sleep, clock = fake_clock_step()
        return ProcessGroup(opener=opener, sleep=sleep, clock=clock)

    def test_polite_child_only_gets_sigterm(self) -> None:
        group = self._group(lambda argv, **kw: FakePopen(argv))
        group.spawn("board", ["/bin/true"], BASE_ENV, "/", self.log)
        group.shutdown_all(1.0, 1.0)
        child = FakePopen.instances[-1]
        self.assertEqual((child.terminates, child.kills), (1, 0))
        self.assertGreaterEqual(child.waits, 1)
        self.assertEqual(group.orphaned, [])

    def test_stubborn_child_is_killed_and_still_waited(self) -> None:
        group = self._group(lambda argv, **kw: FakePopen(argv, ignore_term=True))
        group.spawn("controller", ["/bin/true"], BASE_ENV, "/", self.log)
        group.shutdown_all(1.0, 1.0)
        child = FakePopen.instances[-1]
        self.assertEqual((child.terminates, child.kills), (1, 1))
        self.assertGreaterEqual(child.waits, 1)
        self.assertEqual(group.reaped, 1)
        self.assertEqual(group.orphaned, [])

    def test_context_manager_shuts_down_on_exception(self) -> None:
        group = self._group(lambda argv, **kw: FakePopen(argv, ignore_term=True))
        with self.assertRaises(RuntimeError):
            with group:
                group.spawn("controller", ["/bin/true"], BASE_ENV, "/", self.log)
                group.spawn("board", ["/bin/true"], BASE_ENV, "/", self.log)
                raise RuntimeError("stage blew up")
        self.assertEqual(len(FakePopen.instances), 2)
        for child in FakePopen.instances:
            self.assertEqual((child.terminates, child.kills), (1, 1))
            self.assertGreaterEqual(child.waits, 1)
        self.assertEqual(group.orphaned, [])

    def test_spawn_uses_argv_list_and_no_shell(self) -> None:
        captured: dict = {}

        def opener(argv, **kwargs):
            captured["argv"] = argv
            captured["kwargs"] = kwargs
            return FakePopen(argv)

        group = self._group(opener)
        group.spawn("board", ["/bin/true", "--connect"], dict(BASE_ENV), "/", self.log)
        self.assertIsInstance(captured["argv"], list)
        self.assertFalse(captured["kwargs"].get("shell", False))
        self.assertNotIn("mysql://", " ".join(captured["argv"]))

    def test_timeout_still_reaps_every_child(self) -> None:
        group = self._group(lambda argv, **kw: FakePopen(argv, ignore_term=True))
        group.spawn("controller", ["/bin/true"], BASE_ENV, "/", self.log)
        group.spawn("board", ["/bin/true"], BASE_ENV, "/", self.log)
        group.shutdown_all(0.0, 0.0)
        self.assertEqual(group.kill_requests, 2)
        self.assertEqual(group.reaped, 2)

    def test_popen_failure_closes_both_log_fds(self) -> None:
        """Popen 创建失败：刚打开的 stdout/stderr fd 必须关掉，不能泄漏。"""
        opened: list[int] = []

        def opener(argv, **kwargs):
            opened.extend([kwargs["stdout"], kwargs["stderr"]])
            raise OSError("synthetic fork failure")

        group = self._group(opener)
        with self.assertRaises(GateError) as caught:
            group.spawn("board", ["/bin/true"], BASE_ENV, "/", self.log)
        self.assertEqual(caught.exception.code, "PROCESS_SPAWN_FAILED")
        self.assertEqual(len(opened), 2)
        for fd in opened:
            with self.assertRaises(OSError):  # EBADF：fd 已关闭
                os.fstat(fd)
        self.assertEqual(group.children, [])


class WaitPredicateTests(unittest.TestCase):
    def test_predicate_drives_progress(self) -> None:
        sleep, clock = fake_clock_step(0.5)
        seen: list[int] = []

        def predicate() -> bool:
            seen.append(len(seen))
            return len(seen) >= 3

        self.assertEqual(wait_until("ready", predicate, 10.0, 0.25, sleep, clock), 3)
        self.assertEqual(len(seen), 3)

    def test_timeout_raises_fixed_code(self) -> None:
        sleep, clock = fake_clock_step(0.5)
        with self.assertRaises(GateError) as caught:
            wait_until("ready", lambda: False, 2.0, 0.25, sleep, clock)
        self.assertEqual(caught.exception.code, "WAIT_TIMEOUT_READY")


# ------------------------------------------------------------------ HTTP 合同映射


class FakeOpener:
    """假 opener：按路径回 canned 响应，**绝不**开 socket。"""

    def __init__(self, routes: dict, raises: Exception | None = None) -> None:
        self.routes = routes
        self.calls: list[tuple] = []
        self.raises = raises

    def open(self, request, timeout=None):
        key = f"{request.get_method()} {request.selector}"
        self.calls.append((key, request.get_header("X-csrf-token"), request.data))
        if self.raises is not None:
            raise self.raises
        status, body = self.routes[key]
        return SimpleNamespace(read=lambda: body, getcode=lambda: status)


class ControllerHttpContractTests(Workspace):
    """脚本对 controller 现行 API 的调用形状：路径、limit 上限、逐项 CAS、CSRF 头。"""

    def _client(self, routes: dict) -> tuple[ControllerHttp, FakeOpener]:
        opener = FakeOpener(routes)
        client = ControllerHttp(18080, PrivateLog(self.state), opener=opener)
        return client, opener

    def _login(self, client: ControllerHttp) -> None:
        client.csrf_token = "ab" * 32

    def test_health_and_login_contract(self) -> None:
        body = json.dumps({"data": {"user": {"must_change_password": True}, "csrf_token": "ab" * 32}}).encode()
        client, opener = self._client({
            "GET /healthz": (200, b""),
            "POST /api/v1/auth/login": (200, body),
        })
        self.assertTrue(client.health())
        self.assertEqual(client.login("admin", "pw"), (True, True))
        self.assertEqual(client.csrf_token, "ab" * 32)
        self.assertIn("POST /api/v1/auth/login", opener.calls[-1][0])

    def test_pending_list_path_respects_limit_cap(self) -> None:
        client, opener = self._client({
            "GET /api/v1/approvals?decision=none&limit=200": (200, json.dumps(
                {"data": {"items": [{"device_id": "aa" * 32, "revision": "7"}]}}).encode()),
        })
        item = client.find_pending_approval("aa" * 32)
        self.assertEqual(item["revision"], "7")
        self.assertEqual(opener.calls[0][0], "GET /api/v1/approvals?decision=none&limit=200")

    def test_approve_requires_item_ok_and_sends_csrf(self) -> None:
        ok = json.dumps({"data": {"results": [{"device_id": "aa" * 32, "ok": True, "revision": "8"}]}}).encode()
        client, opener = self._client({"POST /api/v1/approvals/approve": (200, ok)})
        self._login(client)
        self.assertEqual(client.approve("aa" * 32, "7"), "8")
        self.assertEqual(opener.calls[-1][1], "ab" * 32)  # X-CSRF-Token 必须带上
        conflict = json.dumps({"data": {"results": [{"device_id": "aa" * 32, "ok": False,
                                                       "error": {"code": "REVISION_CONFLICT"}}]}}).encode()
        client2, _ = self._client({"POST /api/v1/approvals/approve": (200, conflict)})
        with self.assertRaises(GateError) as caught:  # HTTP 200 + ok:false 绝不是批准成功
            client2.approve("aa" * 32, "7")
        self.assertEqual(caught.exception.code, "APPROVE_CAS_REJECTED")

    def test_change_password_requires_changed_true(self) -> None:
        client, _ = self._client({"POST /api/v1/auth/password": (200, json.dumps({"data": {"changed": False}}).encode())})
        self._login(client)
        self.assertFalse(client.change_password("a", "b"))
        client2, _ = self._client({"POST /api/v1/auth/password": (200, json.dumps({"data": {"changed": True}}).encode())})
        self._login(client2)
        self.assertTrue(client2.change_password("a", "b"))

    def test_revoke_requires_data_object(self) -> None:
        client, _ = self._client({
            "POST /api/v1/devices/aa/revoke": (200, json.dumps({"data": {"state": "revoked",
                                                                          "requires_device_reset": True}}).encode()),
        })
        self._login(client)
        self.assertTrue(client.revoke("aa", "8"))

    # ---------------------------------------------------- 状态路由：typed sample 与固定类别

    def _probe(self, response) -> str:
        client, _ = self._client({"GET /api/v1/devices/aa/status": response})
        return client.status_probe("aa")

    def test_status_probe_accepts_real_typed_sample(self) -> None:
        body = json.dumps({"data": typed_status_data()}).encode()
        self.assertEqual(self._probe((200, body)), STATUS_PRESENT)

    def test_status_probe_reports_empty_cache_not_sample(self) -> None:
        body = json.dumps({"data": empty_cache_status_data()}).encode()
        self.assertEqual(self._probe((200, body)), STATUS_CACHE_EMPTY)

    def test_status_probe_rejects_http200_without_data(self) -> None:
        """HTTP 200 + `data={}`（或 data 缺失/非对象）**不是**实际样本。"""
        for body in (b"{}", b'{"data": {}}', b'{"data": []}', b"not json", b""):
            with self.subTest(body=body):
                self.assertEqual(self._probe((200, body)), STATUS_MALFORMED)

    def test_status_probe_rejects_typed_sample_shape_violations(self) -> None:
        """按真实 DTO 校形状：freshness=unknown、64 位字段非十进制字符串、metrics 缺键都算 malformed。"""
        for mutate in (
            lambda d: d.update(freshness="unknown"),
            lambda d: d.update(age_ms=None),
            lambda d: d["snapshot"].pop("metrics"),
            lambda d: d["snapshot"].update(sample_seq=7),
            lambda d: d["snapshot"].update(sampled_unix_ms=1760000000000),
            lambda d: d["snapshot"].update(boot_id=""),
        ):
            node = typed_status_data()
            mutate(node)
            with self.subTest(mutated=str(sorted(node["snapshot"]))):
                self.assertEqual(self._probe((200, json.dumps({"data": node}).encode())), STATUS_MALFORMED)

    def test_status_probe_fixed_categories(self) -> None:
        import urllib.error

        cases = (
            ((403, json.dumps({"error": {"code": "PERMISSION_DENIED"}}).encode()), "http_403"),
            ((404, json.dumps({"error": {"code": "NOT_FOUND"}}).encode()), "http_404"),
            ((500, b"boom"), "http_5xx"),
            ((503, b"unavailable"), "http_5xx"),
            ((204, b""), "http_other"),
        )
        for response, expected in cases:
            with self.subTest(response=response[0]):
                self.assertEqual(self._probe(response), expected)
        # transport 失败（连不上）：固定类别，绝不携带底层异常文本。
        client, opener = self._client({})
        opener.raises = urllib.error.URLError(OSError("connection refused"))
        self.assertEqual(client.status_probe("aa"), STATUS_TRANSPORT)

    def test_status_probe_never_returns_body_text(self) -> None:
        secret = "LEAK-BODY-91337"
        client, _ = self._client({"GET /api/v1/devices/aa/status": (500, secret.encode())})
        category = client.status_probe("aa")
        self.assertEqual(category, "http_5xx")
        self.assertNotIn(secret, category)
        self.assertNotIn(secret, read_text(os.path.join(self.state, "http.log")))


# ------------------------------------------------------------------ 阶段流程


class FakeHttp:
    """假 controller HTTP：只实现脚本用到的方法，返回值全为合成常量。"""

    def __init__(self, *, must_change: bool = False, status_category: str = STATUS_PRESENT,
                 registered=None, approve_ok: bool = True) -> None:
        self.csrf_token = "cd" * 32
        self.registered = registered
        self.must_change = must_change
        self.status_category = status_category
        self.approve_ok = approve_ok
        self.revision = "1"
        self.approved = False
        self.revoked = False
        self.login_calls = 0
        self.password_changes = 0
        self.list_paths: list[str] = []
        self.status_calls = 0

    def health(self) -> bool:
        return True

    def login(self, username, password):
        self.login_calls += 1
        return True, self.must_change and self.login_calls == 1

    def change_password(self, current, new_password):
        self.password_changes += 1
        self.must_change = False
        return True

    def find_pending_approval(self, device_id):
        self.list_paths.append("approvals")
        if self.registered is not None and device_id not in self.registered:
            return None
        return {"device_id": device_id, "revision": self.revision}

    def approve(self, device_id, expected_revision):
        if not self.approve_ok:
            raise GateError("APPROVE_CAS_REJECTED")  # 逐项 CAS 失败：HTTP 200 也不算成功
        self.approved = True
        self.revision = str(int(expected_revision) + 1)
        return self.revision

    def revoke(self, device_id, expected_revision):
        self.revoked = True
        return True

    def status_probe(self, device_id):
        self.status_calls += 1
        return self.status_category


class FakeRunner:
    """假 openssl / migrate 调用：按 argv 返回合成结果，绝不真的执行。"""

    def __init__(self, *, schema_output: str = "schema_version=4\n", schema_rc: int = 0) -> None:
        self.schema_output = schema_output
        self.schema_rc = schema_rc
        self.calls: list[list] = []
        self._by_path: dict[str, bytes] = {}

    def __call__(self, argv, **kwargs):
        argv = list(argv)
        self.calls.append(argv)
        text = " ".join(argv)
        if "genpkey" in text:
            target = argv[argv.index("-out") + 1]
            seed = bytes([len(self._by_path) + 1]) * 32
            self._by_path[target] = seed  # pubout 反查同一 seed，保持假公钥与私钥的关联语义
            # openssl genpkey 的真实产物是固定 48B 的 PKCS#8 **v1**（version=0、无 publicKey）。
            write_private(target, tunnel_two_process.ED25519_PKCS8_V1_PREFIX + seed)
            return subprocess.CompletedProcess(argv, 0)
        if "pkey" in text:
            tail = self._by_path.get(argv[argv.index("-in") + 1], bytes([9]) * 32)
            return SimpleNamespace(stdout=SPKI[:12] + tail, returncode=0)
        if "--check" in text:
            os.write(kwargs["stdout"], self.schema_output.encode())
            return subprocess.CompletedProcess(argv, self.schema_rc)
        raise AssertionError("unexpected argv")


class ScriptedPopen(FakePopen):
    """假 controller/board 进程：模拟真实退出语义，不启动任何真实进程。"""

    def __init__(self, argv, *, auto_exit_after: int | None = None, exit_code: int = 0, marker: str | None = None) -> None:
        super().__init__(argv)
        self._auto = auto_exit_after
        self._exit_code = exit_code
        self._marker = marker

    def poll(self):
        if self.returncode is not None:
            return self.returncode
        self.polls += 1
        if self._auto is not None and self.polls > self._auto:
            self._finish()
        return self.returncode

    def terminate(self) -> None:
        super().terminate()
        self._finish()

    def kill(self) -> None:
        super().kill()
        self._finish()

    def _finish(self) -> None:
        if self.returncode is not None:
            return
        self.returncode = self._exit_code
        if self._marker is not None:
            write_private(self._marker, "revoked")


class StageFlowHarness:
    """共用夹具：起一个只含假 HTTP/假进程/假 openssl 的 TwoProcessRun（零真实副作用）。"""

    def _run(self, *, events: dict, http_kwargs=None, schema_runner=None, identity_runner=None,
             admin=None, negative=False, spawn_order=None, migrate_bin=None, state=None):
        state = state or self.state
        gates = {
            "credential": parse_credential(synthetic_credential()),
            "manifest": None,
            "state_dir": state,
            "admin": admin if admin is not None else AdminCredentials(synthetic_admin()),
            "controller_bin": "/usr/bin/false",
            "board_bin": "/usr/bin/false",
            "migrate_bin": migrate_bin,
            "negative_badpin": negative,
        }
        sleep, clock = fake_clock_step(0.1)
        order = list(spawn_order or ["controller", "board", "board-restart", "board-badpin"])
        slot: dict = {"http": None, "next": order}

        def make_http() -> FakeHttp:
            if slot["http"] is None:
                slot["http"] = FakeHttp(**(http_kwargs or {}))
            return slot["http"]

        def opener(argv, **kwargs):
            name = slot["next"].pop(0)
            text = events.get(name)
            if text:
                with open(os.path.join(state, f"{name}.stderr.log"), "a", encoding="utf-8") as handle:
                    handle.write(text)
            marker = os.path.join(state, "board-id", "UNAUTHORIZED") if name == "board" else None
            return ScriptedPopen(
                argv,
                auto_exit_after=1 if name in ("board", "board-restart") else None,
                exit_code=78 if name == "board-restart" else 0,
                marker=marker,
            )

        group = ProcessGroup(opener=opener, sleep=sleep, clock=clock)
        runner = TwoProcessRun(
            gates, self.console, Redactor(), group, sleep=sleep, clock=clock,
            http_factory=lambda port, log: make_http(),
            identity_runner=identity_runner or FakeRunner(),
            schema_runner=schema_runner or FakeRunner(), port_picker=lambda: 18080,
            rng=lambda n: b"\x11" * n, intervals=0.25,
        )
        runner.http = make_http()
        return runner, slot["http"]


class StageFlowTests(StageFlowHarness, Workspace):
    def setUp(self) -> None:
        super().setUp()
        FakePopen.instances.clear()

    def test_happy_path_pending_to_permanent_stop(self) -> None:
        runner, http = self._run(events={"board": BOARD_READY, "board-restart": BOARD_PREFLIGHT},
                                 migrate_bin="/usr/bin/false")
        self.assertEqual(runner.run(), EXIT_OK)
        facts = runner.facts
        self.assertTrue(facts["pending_observed"])
        self.assertTrue(facts["approved"] and facts["revoked"])
        self.assertTrue(facts["ready_observed"] and facts["status_observed"])
        self.assertTrue(facts["board_exited_zero"] and facts["unauthorized_marker"])
        self.assertTrue(facts["preflight_rejected"])
        self.assertEqual(facts["preflight_attempt_count"], 0)
        self.assertEqual(facts["schema_version_probe"], True)
        self.assertEqual(facts["schema_shape_gate"], "controller_startup_healthz_observed")
        self.assertTrue(http.approved and http.revoked)
        self.assertEqual(facts["children_orphaned"], 0)
        self.assertIn("stage=shutdown orphaned=0", self.stream.getvalue())

    def test_approve_cas_false_is_not_success(self) -> None:
        runner, http = self._run(events={"board": BOARD_READY}, http_kwargs={"approve_ok": False})
        self.assertEqual(runner.run(), EXIT_FAILED)
        self.assertEqual(runner.facts.get("failed_stage"), "stage_approve")
        self.assertFalse(http.approved)
        self.assertNotIn("approved", runner.facts)

    def test_secrets_never_reach_console(self) -> None:
        runner, _ = self._run(events={"board": BOARD_READY, "board-restart": BOARD_PREFLIGHT})
        runner.run()
        text = self.stream.getvalue()
        for secret in ("Synth-Pw", "synthetic-admin-pw", "synthetic-new-pw", "svc_synth", "mysql://",
                       DB_NAME, "127.0.0.1", "cd" * 8, "18080"):
            self.assertNotIn(secret, text, text)

    def test_summary_file_is_private_and_scrubbed(self) -> None:
        runner, _ = self._run(events={"board": BOARD_READY, "board-restart": BOARD_PREFLIGHT})
        runner.run()
        path = os.path.join(self.state, "two-process-summary.json")
        self.assertEqual(stat.S_IMODE(os.stat(path).st_mode), 0o600)
        text = read_text(path)
        for secret in ("Synth-Pw", "mysql://", "synthetic-admin-pw", DB_NAME, "127.0.0.1"):
            self.assertNotIn(secret, text)
        self.assertIn('"complete": true', text)

    def test_missing_ready_event_is_not_fake_success(self) -> None:
        runner, _ = self._run(events={"board": BOARD_STUCK})
        self.assertEqual(runner.run(), EXIT_FAILED)
        self.assertFalse(runner.facts.get("ready_observed", False))
        self.assertEqual(runner.facts.get("failed_stage"), "stage_ready")
        for child in FakePopen.instances:
            self.assertGreaterEqual(child.terminates, 1)
            self.assertGreaterEqual(child.waits, 1)
        self.assertEqual(runner.group.orphaned, [])
        self.assertIn("WAIT_TIMEOUT_READY", read_text(os.path.join(self.state, "two-process-summary.json")))

    def test_schema_version_probe_parses_real_output(self) -> None:
        runner, _ = self._run(events={}, schema_runner=FakeRunner(schema_output="schema_version=3\n"),
                              migrate_bin="/usr/bin/false")
        with self.assertRaises(GateError) as caught:
            runner.stage_schema_gate()
        self.assertEqual(caught.exception.code, "SCHEMA_VERSION_NOT_V4")
        self.assertEqual(runner.group.children, [])

    def test_schema_version_missing_is_not_v4(self) -> None:
        runner, _ = self._run(events={}, schema_runner=FakeRunner(schema_output="schema_version=missing\n"),
                              migrate_bin="/usr/bin/false")
        with self.assertRaises(GateError) as caught:
            runner.stage_schema_gate()
        self.assertEqual(caught.exception.code, "SCHEMA_VERSION_MISSING")
        self.assertEqual(runner.group.children, [])

    def test_schema_check_failure_blocks_before_controller(self) -> None:
        runner, _ = self._run(events={}, schema_runner=FakeRunner(schema_rc=1), migrate_bin="/usr/bin/false")
        with self.assertRaises(GateError) as caught:
            runner.stage_schema_gate()
        self.assertEqual(caught.exception.code, "SCHEMA_CHECK_REJECTED")
        self.assertEqual(runner.group.children, [])

    def test_schema_probe_skipped_without_bin_and_shape_claim_stays_honest(self) -> None:
        runner, _ = self._run(events={"board": BOARD_READY, "board-restart": BOARD_PREFLIGHT})
        runner.run()
        # 没传 --migrate-bin 时不做版本探针；形状只由 controller 启动门禁 + health 隐式成立。
        self.assertIs(runner.facts["schema_version_probe"], False)
        self.assertEqual(runner.facts["schema_shape_gate"], "controller_startup_healthz_observed")

    def test_status_poll_absence_is_recorded_not_assumed(self) -> None:
        """缓存为空（真实 `snapshot=null`/`freshness=unknown`）**不是**实际样本：必须失败关闭。"""
        runner, _ = self._run(events={"board": BOARD_READY, "board-restart": BOARD_PREFLIGHT},
                              http_kwargs={"status_category": STATUS_CACHE_EMPTY})
        self.assertEqual(runner.run(), EXIT_FAILED)
        self.assertIs(runner.facts["status_observed"], False)
        self.assertIs(runner.facts["ready_observed"], True)  # Ready 不替代 RPC 证据
        self.assertNotIn("revoked", runner.facts)           # 顺序保持：status 失败即不 revoke
        self.assertEqual(runner.facts.get("failed_stage"), "stage_status_poll")
        summary = read_text(os.path.join(self.state, "two-process-summary.json"))
        self.assertIn("STATUS_RPC_NOT_OBSERVED", summary)
        self.assertIn('"complete": false', summary)

    def test_status_probe_failure_categories_are_all_red(self) -> None:
        """HTTP 403/404/5xx/transport/malformed 与真实空缓存：summary 与 exit 一律不绿。"""
        for category in ("http_403", "http_404", "http_5xx", "http_other",
                         STATUS_TRANSPORT, STATUS_MALFORMED, STATUS_CACHE_EMPTY):
            state = os.path.join(self.workspace, f"state-{category}")
            os.mkdir(state, 0o700)
            os.chmod(state, 0o700)
            runner, _ = self._run(events={"board": BOARD_READY, "board-restart": BOARD_PREFLIGHT},
                                  http_kwargs={"status_category": category}, state=state)
            with self.subTest(category=category):
                errors = io.StringIO()
                with contextlib.redirect_stderr(errors):
                    self.assertEqual(runner.run(), EXIT_FAILED)
                self.assertIs(runner.facts["status_observed"], False)
                self.assertEqual(runner.facts["status_probe_category"], category)
                node = json.loads(read_text(os.path.join(state, "two-process-summary.json")))
                self.assertIn("STATUS_RPC_NOT_OBSERVED", node["failure_codes"])
                self.assertFalse(node["complete"])
                self.assertNotIn("revoked", runner.facts)
                self.assertIn("failure_code=STATUS_RPC_NOT_OBSERVED", errors.getvalue())

    def test_status_probe_present_lets_the_chain_complete(self) -> None:
        """真实 typed sample（`freshness=fresh` + snapshot 对象）才是放行后续阶段的唯一凭据。"""
        runner, http = self._run(events={"board": BOARD_READY, "board-restart": BOARD_PREFLIGHT},
                                 http_kwargs={"status_category": STATUS_PRESENT})
        self.assertEqual(runner.run(), EXIT_OK)
        self.assertTrue(runner.facts["status_observed"])
        self.assertGreaterEqual(http.status_calls, 1)
        self.assertTrue(runner.facts["revoked"])

    def test_must_change_password_forces_real_change(self) -> None:
        runner, http = self._run(events={"board": BOARD_READY, "board-restart": BOARD_PREFLIGHT},
                                 http_kwargs={"must_change": True})
        runner.run()
        self.assertEqual(http.password_changes, 1)
        self.assertEqual(http.login_calls, 2)

    def test_must_change_password_without_new_password_fails_closed(self) -> None:
        runner, http = self._run(events={"board": BOARD_READY},
                                 admin=AdminCredentials({"username": "admin", "password": "synthetic-admin-pw"}),
                                 http_kwargs={"must_change": True})
        self.assertEqual(runner.run(), EXIT_FAILED)
        self.assertEqual(http.password_changes, 0)
        self.assertEqual(runner.facts.get("failed_stage"), "stage_login")

    def test_bootstrap_sink_supply_password_once(self) -> None:
        write_private(os.path.join(self.state, "controller-bootstrap-secret.log"), "admin: boot-sync-pw\n")
        runner, http = self._run(events={"board": BOARD_READY, "board-restart": BOARD_PREFLIGHT},
                                 admin=AdminCredentials({"username": "admin"}))
        runner.run()
        self.assertIs(runner.facts["admin_from_bootstrap_sink"], True)
        self.assertNotIn("boot-sync-pw", self.stream.getvalue())
        self.assertEqual(http.login_calls, 1)

    def test_bootstrap_sink_mode_must_be_private(self) -> None:
        path = write_private(os.path.join(self.state, "controller-bootstrap-secret.log"), "admin: boot-sync-pw\n")
        os.chmod(path, 0o644)
        runner, _ = self._run(events={"board": BOARD_STUCK}, admin=AdminCredentials({"username": "admin"}))
        self.assertEqual(runner.run(), EXIT_FAILED)
        self.assertIn("BOOTSTRAP_SECRET_UNAVAILABLE",
                      read_text(os.path.join(self.state, "two-process-summary.json")))

    def test_no_dsn_in_child_argv(self) -> None:
        runner, _ = self._run(events={"board": BOARD_READY, "board-restart": BOARD_PREFLIGHT})
        runner.run()
        for child in FakePopen.instances:
            joined = " ".join(child.argv)
            self.assertNotIn("mysql://", joined)
            self.assertNotIn("Synth-Pw", joined)

    def _prepare_negative_stage(self, events: dict) -> TwoProcessRun:
        runner, _ = self._run(events=events, negative=True, spawn_order=["board-badpin"],
                              http_kwargs={"registered": set()})
        runner.ports["tunnel"] = 19999
        runner.ports["http"] = 18888
        return runner

    def test_badpin_negative_rejects_ready_board(self) -> None:
        runner = self._prepare_negative_stage({"board-badpin": BOARD_READY})
        with self.assertRaises(GateError) as caught:
            runner.stage_negative_badpin()
        self.assertEqual(caught.exception.code, "BAD_PIN_ACCEPTED")

    def test_badpin_negative_passes_when_never_ready(self) -> None:
        runner = self._prepare_negative_stage({"board-badpin": BOARD_STUCK})
        runner.stage_negative_badpin()
        self.assertIs(runner.facts["badpin_ready"], False)
        self.assertIs(runner.facts["badpin_device_registered"], False)

    def test_badpin_negative_without_attempt_evidence_is_not_a_pass(self) -> None:
        """无任何 AttemptStarted 且进程仍活着：负例**没有取证**，不能当通过。"""
        runner = self._prepare_negative_stage({})  # 假进程既无事件也不退出
        with self.assertRaises(GateError) as caught:
            runner.stage_negative_badpin()
        self.assertEqual(caught.exception.code, "BAD_PIN_NO_ATTEMPT_EVIDENCE")
        self.assertNotIn("badpin_window_observed", runner.facts)


class StageExceptionContainmentTests(StageFlowHarness, Workspace):
    """M3：原生异常（非 GateError）也必须被阶段层包住 → 固定类别码 + 非零 exit + summary false。"""

    def setUp(self) -> None:
        super().setUp()
        FakePopen.instances.clear()

    def _stderr_of(self, callable_):
        errors = io.StringIO()
        with contextlib.redirect_stderr(errors):
            code = callable_()
        return code, errors.getvalue()

    def test_called_process_error_in_identity_is_contained(self) -> None:
        class Boom:
            def __call__(self, argv, **kwargs):
                raise subprocess.CalledProcessError(1, ["/usr/bin/false", LEAK_TOKEN])

        runner, _ = self._run(events={}, identity_runner=Boom())
        code, errors = self._stderr_of(runner.run)
        self.assertEqual(code, EXIT_FAILED)
        node = json.loads(read_text(os.path.join(self.state, "two-process-summary.json")))
        self.assertIn("UNEXPECTED_STAGE_ERROR", node["failure_codes"])
        self.assertFalse(node["complete"])
        self.assertNotIn(LEAK_TOKEN, errors)
        self.assertNotIn(LEAK_TOKEN, read_text(os.path.join(self.state, "two-process-summary.json")))

    def test_timeout_expired_in_schema_gate_is_contained(self) -> None:
        class TimedOut:
            def __call__(self, argv, **kwargs):
                raise subprocess.TimeoutExpired([LEAK_TOKEN], 1)

        runner, _ = self._run(events={}, schema_runner=TimedOut(), migrate_bin="/usr/bin/false")
        code, errors = self._stderr_of(runner.run)
        self.assertEqual(code, EXIT_FAILED)
        self.assertIn("UNEXPECTED_STAGE_ERROR",
                      read_text(os.path.join(self.state, "two-process-summary.json")))
        self.assertNotIn(LEAK_TOKEN, errors)
        self.assertEqual(runner.group.children, [])  # schema 阶段不 spawn

    def test_file_exists_error_in_negative_journal_is_contained(self) -> None:
        os.mkdir(os.path.join(self.state, "board-badpin-journal"), 0o700)
        runner, _ = self._run(events={"board": BOARD_READY}, negative=True,
                              spawn_order=["controller", "board", "board-badpin"])
        code, errors = self._stderr_of(runner.run)
        self.assertEqual(code, EXIT_FAILED)
        self.assertIn("UNEXPECTED_STAGE_ERROR",
                      read_text(os.path.join(self.state, "two-process-summary.json")))
        self.assertNotIn(LEAK_TOKEN, errors)
        self.assertEqual(runner.facts.get("failed_stage"), "stage_negative_badpin")

    def test_keyboard_interrupt_is_not_swallowed(self) -> None:
        """Ctrl-C 仍要向外传播（不假继续），但收尾与 summary 必须先做完。"""
        class Interrupter:
            def __call__(self, argv, **kwargs):
                raise KeyboardInterrupt

        runner, _ = self._run(events={}, identity_runner=Interrupter())
        with self.assertRaises(KeyboardInterrupt):
            runner.run()
        node = json.loads(read_text(os.path.join(self.state, "two-process-summary.json")))
        self.assertIn("RUN_INTERRUPTED", node["failure_codes"])
        self.assertFalse(node["complete"])
        self.assertEqual(runner.group.orphaned, [])

    def test_shutdown_exception_is_recorded(self) -> None:
        class ExplodingGroup(ProcessGroup):
            def shutdown_all(self, term_timeout=20.0, kill_timeout=10.0):
                raise RuntimeError("synthetic shutdown boom")

        sleep, clock = fake_clock_step(0.1)
        gates = {
            "credential": parse_credential(synthetic_credential()), "manifest": None,
            "state_dir": self.state, "admin": AdminCredentials(synthetic_admin()),
            "controller_bin": "/usr/bin/false", "board_bin": "/usr/bin/false",
            "migrate_bin": None, "negative_badpin": False,
        }
        group = ExplodingGroup(opener=lambda argv, **kw: FakePopen(argv), sleep=sleep, clock=clock)
        runner = TwoProcessRun(gates, self.console, Redactor(), group, sleep=sleep, clock=clock,
                               http_factory=lambda port, log: FakeHttp(), identity_runner=FakeRunner(),
                               schema_runner=FakeRunner(), port_picker=lambda: 18080,
                               rng=lambda n: b"\x11" * n, intervals=0.25)
        errors = io.StringIO()
        with contextlib.redirect_stderr(errors):
            code = runner.run()
        self.assertEqual(code, EXIT_FAILED)
        self.assertIn("UNEXPECTED_STAGE_ERROR",
                      read_text(os.path.join(self.state, "two-process-summary.json")))

    def test_summary_write_failure_is_not_green(self) -> None:
        """summary 写不进去（例如同名目录占位）绝不能返回 exit 0 或留下旧的 complete=true。"""
        os.mkdir(os.path.join(self.state, "two-process-summary.json"), 0o700)
        runner, _ = self._run(events={"board": BOARD_READY, "board-restart": BOARD_PREFLIGHT})
        code, errors = self._stderr_of(runner.run)
        self.assertEqual(code, EXIT_FAILED)
        self.assertIn("SUMMARY_WRITE_FAILED", errors)

    def test_main_does_not_echo_unrecognized_argument(self) -> None:
        errors = io.StringIO()
        with contextlib.redirect_stderr(errors):
            code = main(["--credential-file", self.credential_file(), f"--oops={LEAK_TOKEN}"],
                        repo_root=None, console=self.console)
        self.assertEqual(code, EXIT_GATE_FAILED)
        self.assertNotIn(LEAK_TOKEN, errors.getvalue())
        self.assertIn("gate_error=ARGUMENTS_INVALID", errors.getvalue())

    def test_main_derive_io_error_is_fixed_code(self) -> None:
        _, proof = self.backup()
        errors = io.StringIO()
        argv = ["--derive-manifest-from-proof", proof, "--engine", "tidb",
                "--manifest-out", os.path.join(self.workspace, "missing-dir", "m.json")]
        with contextlib.redirect_stderr(errors):
            code = main(argv, repo_root=None, console=self.console)
        self.assertEqual(code, EXIT_GATE_FAILED)
        self.assertIn("gate_error=MANIFEST_OUTPUT_UNWRITABLE", errors.getvalue())
        self.assertNotIn(self.workspace, errors.getvalue())


class SelftestTests(unittest.TestCase):
    def test_selftest_passes_offline(self) -> None:
        stream = io.StringIO()
        self.assertEqual(main(["--selftest"], console=Console(stream)), EXIT_OK)
        self.assertIn("selftest ok=true", stream.getvalue())


if __name__ == "__main__":
    unittest.main()
