# scripts/tests/check_controller_acceptance_test.py（纯测试；坏样例为单测内 synthetic）
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path

from scripts.check_controller_acceptance import ALL_IDS, HEADER, _cells, default_report, validate

SEPARATOR = "| --- | --- | --- | --- | --- | --- |"


def _row(rid: str, status: str = "未验证", rest: tuple = ("", "", "", "")) -> str:
    return f"| {rid} | {status} | {rest[0]} | {rest[1]} | {rest[2]} | {rest[3]} |"


def _ledger_doc(header_line: str | None = HEADER) -> str:
    """合成文档：正文保留完整行合同引用，真实账本表头可整行删除或改列名。"""
    lines = ["# Controller V1 集成验收报告", "Spec: 06 §6", "",
             "> 本报告按行合同 `" + HEADER + "` 登记事实", ""]
    if header_line is not None:
        lines.append(header_line)
    lines.append(SEPARATOR)
    lines.extend(_row(rid) for rid in ALL_IDS)
    return "\n".join(lines) + "\n"


def _insert_after(text: str, anchor_rid: str, row: str) -> str:
    """把 row 插入到账本内部（anchor 行之后），模拟真实模板中间插入非法行。"""
    lines = text.splitlines()
    for index, line in enumerate(lines):
        if line.startswith(f"| {anchor_rid} |"):
            lines.insert(index + 1, row)
            return "\n".join(lines) + "\n"
    raise AssertionError(f"anchor row {anchor_rid} not found")


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

    # --- F1：真实账本表头必须紧邻分隔线，正文引用行合同不得代替 ---

    def test_ledger_doc_with_intact_header_passes(self):
        self.assertEqual(validate(_ledger_doc()), [])

    def test_real_header_removed_but_quoted_contract_kept_rejected(self):
        text = _ledger_doc(None)
        self.assertIn(HEADER, text)  # 正文引用仍在：旧实现的全文子串判断会误判通过
        self.assertEqual(validate(text), ["table header missing"])
        with _cli_workspace_dir() as d:
            path = d / "SYNTHETIC_PATH_SENTINEL.md"
            path.write_text(text, encoding="utf-8")
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(path)], capture_output=True, text=True)
            self.assertEqual((res.returncode, res.stdout, res.stderr), (1, "", "table header missing\n"))

    def test_real_header_renamed_column_rejected(self):
        renamed = "| ID | status | execution_time | layer | command_or_steps | evidence |"
        text = _ledger_doc(renamed)
        self.assertIn(HEADER, text)  # 顶部合同引用保留，仅真实表头改列名
        self.assertEqual(validate(text), ["table header missing"])
        with _cli_workspace_dir() as d:
            path = d / "SYNTHETIC_PATH_SENTINEL.md"
            path.write_text(text, encoding="utf-8")
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(path)], capture_output=True, text=True)
            self.assertEqual((res.returncode, res.stdout, res.stderr), (1, "", "table header missing\n"))
            self.assertNotIn("execution_time", res.stdout + res.stderr)
            self.assertNotIn(str(path), res.stdout + res.stderr)

    def test_header_not_adjacent_to_separator_rejected(self):
        text = _ledger_doc().replace(HEADER + "\n" + SEPARATOR,
                                     HEADER + "\n>\n" + SEPARATOR)
        self.assertIn(HEADER, text)
        self.assertEqual(validate(text), ["table header missing"])

    # --- F2：账本数据区域内不得混入未定义 ID，且错误不回显原值 ---

    def test_unknown_id_row_inside_ledger_rejected(self):
        cases = [("ZZ-01", "malformed id"), ("t4-01", "malformed id"),
                 ("at-01", "malformed id"), ("T4-13", "out of range id")]
        for rid, expected in cases:
            with self.subTest(rid=rid), _cli_workspace_dir() as d:
                text = _insert_after(default_report(), "T4-01", _row(rid))
                self.assertEqual(validate(text), [expected])
                path = d / "SYNTHETIC_PATH_SENTINEL.md"
                path.write_text(text, encoding="utf-8")
                res = subprocess.run([sys.executable, SCRIPT_PATH, str(path)], capture_output=True, text=True)
                self.assertEqual((res.returncode, res.stdout, res.stderr), (1, "", expected + "\n"))
                self.assertNotIn(rid, res.stdout + res.stderr)
                self.assertNotIn(str(path), res.stdout + res.stderr)
                self.assertNotIn("Traceback", res.stdout + res.stderr)

    def test_unknown_id_row_appended_after_ledger_rows_rejected(self):
        cases = [("ZZ-01", "malformed id"), ("t4-01", "malformed id"), ("AT-99", "out of range id")]
        for rid, expected in cases:
            with self.subTest(rid=rid):
                text = default_report() + _row(rid) + "\n"
                self.assertEqual(validate(text), [expected])

    def test_unknown_id_row_in_ledger_doc_rejected(self):
        text = _ledger_doc() + _row("ZZ-01", "DONE") + "\n"
        self.assertEqual(validate(text), ["malformed id"])

    def test_other_tables_outside_ledger_ignored(self):
        # 账本之外的说明表格不参与行合同校验，但账本本身仍须完整。
        text = default_report() + "\n## 其他说明\n\n| 说明 | 值 |\n| --- | --- |\n| ZZ-01 | 说明 |\n"
        self.assertEqual(validate(text), [])

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
