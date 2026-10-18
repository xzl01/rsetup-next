# scripts/tests/check_controller_acceptance_test.py（纯测试；坏样例为单测内 synthetic）
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path

from scripts.check_controller_acceptance import _cells, default_report, validate


def _row(rid: str, status: str = "未验证", rest: tuple = ("", "", "", "")) -> str:
    return f"| {rid} | {status} | {rest[0]} | {rest[1]} | {rest[2]} | {rest[3]} |"


def _drop(text: str, rid: str) -> str:
    return "\n".join(l for l in text.splitlines() if not l.startswith(f"| {rid} |")) + "\n"


def _drop_line(text: str, needle: str) -> str:
    return "\n".join(l for l in text.splitlines() if needle not in l) + "\n"


SCRIPT_PATH = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "check_controller_acceptance.py"))


@contextmanager
def _cli_workspace_dir():
    repo_root = Path(__file__).resolve().parents[2]
    if repo_root != Path.cwd().resolve() or not (repo_root / "AGENTS.md").is_file() or not (repo_root / "scripts").is_dir():
        raise RuntimeError("acceptance test requires design tree root")
    prefix = ".controller-checker-"
    # mkdtemp 无析构/退出时自动删除：清理校验失败后也不能被 finalizer 绕过。
    raw_path = Path(tempfile.mkdtemp(dir=str(repo_root), prefix=prefix))
    owned_path = raw_path.resolve(strict=True)
    created = raw_path.lstat()
    owner_identity = (created.st_dev, created.st_ino)
    try:
        yield owned_path
    finally:
        try:
            current = raw_path.resolve(strict=True)
            info = raw_path.lstat()
            valid = (
                current == owned_path
                and current.parent == repo_root
                and raw_path.name.startswith(prefix)
                and not raw_path.is_symlink()
                and raw_path.is_dir()
                and (info.st_dev, info.st_ino) == owner_identity
            )
        except OSError:
            raise RuntimeError("refusing unverified fixture cleanup") from None
        if not valid:
            raise RuntimeError("refusing unverified fixture cleanup")
        # 只删除上方已重新核验绝对路径/parent/本例inode身份的独占目录。
        shutil.rmtree(raw_path)


class AcceptanceReportContractTest(unittest.TestCase):
    """行合同：| ID | status | executed_at | layer | command_or_steps | evidence |。"""

    def test_default_all_unverified_report_passes(self):
        self.assertEqual(validate(default_report()), [])

    def test_missing_t4_row_rejected(self):
        self.assertIn("T4-05: missing", validate(_drop(default_report(), "T4-05")))

    def test_duplicate_t4_row_rejected(self):
        self.assertIn("T4-03: duplicate row", validate(default_report() + _row("T4-03") + "\n"))

    def test_t4_pass_without_evidence_rejected(self):
        text = _drop(default_report(), "T4-01") + _row("T4-01", "PASS") + "\n"
        self.assertIn("T4-01: PASS missing executed_at", validate(text))

    def test_missing_title_rejected(self):
        text = _drop_line(default_report(), "# Controller V1 集成验收报告")
        self.assertIn("title missing", validate(text))

    def test_missing_table_header_rejected(self):
        text = _drop_line(default_report(), "| ID | status | executed_at | layer | command_or_steps | evidence |")
        self.assertIn("table header missing", validate(text))

    def test_counter_examples_data_driven(self):
        base = default_report()
        cases = [
            ("malformed id", _drop(base, "T4-01") + _row("T4-1") + "\n"),
            ("malformed id", _drop(base, "T4-01") + _row("T4-001") + "\n"),
            ("malformed id", _drop(base, "T4-01") + _row("T4-xx") + "\n"),
            ("out of range id", _drop(base, "AT-18") + _row("AT-19") + "\n"),
            ("malformed id", _drop(base, "AT-01") + _row("AT-001") + "\n"),
            ("T4-01: illegal status", base.replace("| T4-01 | 未验证 |", "| T4-01 | DONE |")),
            ("T4-01: malformed row", base.replace("| T4-01 | 未验证 | | | | |", "| T4-01 | 未验证 | | |")),
            ("T4-02: duplicate row", base + _row("T4-02") + "\n"),
            ("Spec: 06 §6 reference missing",
             "\n".join(l for l in base.splitlines() if not l.startswith("Spec:")) + "\n"),
            ("T4-01: PASS missing executed_at",
             _drop(base, "T4-01") + _row("T4-01", "PASS", ("", "tower", "cmd", "ref")) + "\n"),
            ("T4-01: PASS missing layer",
             _drop(base, "T4-01") + _row("T4-01", "PASS", ("2026-10-16", "", "cmd", "ref")) + "\n"),
            ("T4-01: PASS missing command_or_steps",
             _drop(base, "T4-01") + _row("T4-01", "PASS", ("2026-10-16", "tower", "", "ref")) + "\n"),
            ("T4-01: PASS missing evidence",
             _drop(base, "T4-01") + _row("T4-01", "PASS", ("2026-10-16", "tower", "cmd", "")) + "\n"),
        ]
        for expected, text in cases:
            with self.subTest(expected=expected):
                self.assertIn(expected, validate(text))

    def test_trailing_extra_empty_column_rejected(self):
        base = default_report()
        row = "| T4-01 | PASS | 2026-10-16 | tower | cmd | ref ||"
        text = _drop(base, "T4-01") + row + "\n"
        self.assertEqual(len(_cells(row)), 7)
        self.assertIn("T4-01: malformed row", validate(text))

    def test_complete_pass_row_pure_success(self):
        # 仅合成测试文本；不把 PASS 写入真实验收报告。
        row = _row("T4-01", "PASS", ("2026-10-16", "tower", "synthetic-command", "synthetic-ref"))
        self.assertEqual(validate(_drop(default_report(), "T4-01") + row + "\n"), [])

    def test_cli_complete_pass_row_success(self):
        text = _drop(default_report(), "T4-01") + _row("T4-01", "PASS", ("2026-10-16", "tower", "synthetic-command", "synthetic-ref")) + "\n"
        with _cli_workspace_dir() as d:
            path = d / "SYNTHETIC_PATH_SENTINEL.md"
            path.write_text(text, encoding="utf-8")
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(path)], capture_output=True, text=True)
            self.assertEqual((res.returncode, res.stdout, res.stderr), (0, "", ""))

    def test_cli_pass_missing_each_required_field(self):
        fields = ("executed_at", "layer", "command_or_steps", "evidence")
        for index, field in enumerate(fields):
            with self.subTest(field=field), _cli_workspace_dir() as d:
                values = ["2026-10-16", "tower", "synthetic-command", "synthetic-ref"]
                values[index] = ""
                text = _drop(default_report(), "T4-01") + _row("T4-01", "PASS", tuple(values)) + "\n"
                expected = f"T4-01: PASS missing {field}"
                self.assertEqual(validate(text), [expected])
                path = d / "SYNTHETIC_PATH_SENTINEL.md"
                path.write_text(text, encoding="utf-8")
                res = subprocess.run([sys.executable, SCRIPT_PATH, str(path)], capture_output=True, text=True)
                self.assertEqual((res.returncode, res.stdout, res.stderr), (1, "", expected + "\n"))

    def test_extra_invalid_id_keeps_all_required_rows(self):
        cases = [("T4-001", "malformed id"), ("T4-13", "out of range id"),
                 ("AT-19", "out of range id"), ("T4-SYNTHETIC_SECRET_SENTINEL", "malformed id")]
        for rid, expected in cases:
            with self.subTest(rid=rid), _cli_workspace_dir() as d:
                # 原30行全保留，只追加恶意/畸形/越界行；不能以缺行副作用假通过。
                text = default_report() + _row(rid) + "\n"
                self.assertEqual(validate(text), [expected])
                path = d / "SYNTHETIC_PATH_SENTINEL.md"
                path.write_text(text, encoding="utf-8")
                res = subprocess.run([sys.executable, SCRIPT_PATH, str(path)], capture_output=True, text=True)
                self.assertEqual((res.returncode, res.stdout, res.stderr), (1, "", expected + "\n"))
                self.assertNotIn("SYNTHETIC_SECRET_SENTINEL", res.stdout + res.stderr)
                self.assertNotIn(str(path), res.stdout + res.stderr)
                self.assertNotIn("Traceback", res.stdout + res.stderr)

    def test_known_id_wrong_column_duplicate_rejected(self):
        text = default_report() + "| T4-03 | 未验证 | |\n"
        expected = ["T4-03: malformed row", "T4-03: duplicate row"]
        self.assertEqual(validate(text), expected)
        with _cli_workspace_dir() as d:
            path = d / "SYNTHETIC_PATH_SENTINEL.md"
            path.write_text(text, encoding="utf-8")
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(path)], capture_output=True, text=True)
            self.assertEqual((res.returncode, res.stdout, res.stderr), (1, "", "\n".join(expected) + "\n"))

    def test_cli_missing_title_rejected(self):
        text = _drop_line(default_report(), "# Controller V1 集成验收报告")
        with _cli_workspace_dir() as d:
            path = d / "SYNTHETIC_PATH_SENTINEL.md"
            path.write_text(text, encoding="utf-8")
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(path)], capture_output=True, text=True)
            self.assertEqual((res.returncode, res.stdout, res.stderr), (1, "", "title missing\n"))

    def test_cli_extra_arguments_exact_exit_two(self):
        with _cli_workspace_dir() as d:
            path = d / "SYNTHETIC_PATH_SENTINEL.md"
            self.assertFalse(path.exists())
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(path), "SYNTHETIC_ARGUMENT_SENTINEL"], capture_output=True, text=True)
            self.assertEqual((res.returncode, res.stdout, res.stderr), (2, "", "invalid arguments\n"))

    def test_cli_default_report_success(self):
        with _cli_workspace_dir() as d:
            report_path = d / "report.md"
            report_path.write_text(default_report(), encoding="utf-8")
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(report_path)], capture_output=True, text=True)
            self.assertEqual(res.returncode, 0)
            self.assertEqual(res.stdout, "")
            self.assertEqual(res.stderr, "")

    def test_cli_invalid_report_rejected(self):
        with _cli_workspace_dir() as d:
            report_path = d / "report.md"
            report_path.write_text(_drop(default_report(), "T4-01"), encoding="utf-8")
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(report_path)], capture_output=True, text=True)
            self.assertEqual(res.returncode, 1)
            self.assertEqual(res.stdout, "")
            self.assertEqual(res.stderr, "T4-01: missing\n")

    def test_cli_missing_file_rejected(self):
        with _cli_workspace_dir() as d:
            missing_path = d / "missing.md"
            self.assertFalse(missing_path.exists())
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(missing_path)],
                                 capture_output=True, text=True)
            self.assertEqual(res.returncode, 1)
            self.assertEqual(res.stdout, "")
            self.assertEqual(res.stderr, "cannot read report\n")

    def test_cli_invalid_arguments_rejected(self):
        res = subprocess.run([sys.executable, SCRIPT_PATH], capture_output=True, text=True)
        self.assertEqual(res.returncode, 2)
        self.assertEqual(res.stdout, "")
        self.assertEqual(res.stderr, "invalid arguments\n")

    def test_cli_utf8_decode_error_rejected(self):
        with _cli_workspace_dir() as d:
            bin_path = d / "invalid_utf8.bin"
            bin_path.write_bytes(b"\xff\xfe\x00\x00")
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(bin_path)], capture_output=True, text=True)
            self.assertEqual(res.returncode, 1)
            self.assertEqual(res.stdout, "")
            self.assertEqual(res.stderr, "cannot read report\n")
