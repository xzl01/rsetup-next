# scripts/check_controller_acceptance.py（RED 前置：default_report 与 CLI 完整 + validate 空桩）
"""报告格式/完整性校验器：只验账本格式合同，不证明外部证据真实。"""
import os
import re
import sys
from collections import Counter

HEADER = "| ID | status | executed_at | layer | command_or_steps | evidence |"
TITLE = "# Controller V1 集成验收报告"
T4_IDS = [f"T4-{i:02d}" for i in range(1, 13)]
AT_IDS = [f"AT-{i:02d}" for i in range(1, 19)]
ALL_IDS = T4_IDS + AT_IDS
STATUSES = {"PASS", "FAIL", "未验证", "BLOCKED"}
PASS_FIELDS = ("executed_at", "layer", "command_or_steps", "evidence")
ROW_ID_RE = re.compile(r"^(T4|AT)-\d{2}$")
PREFIX_RE = re.compile(r"^(T4|AT)")


def default_report() -> str:
    lines = ["# Controller V1 集成验收报告", "Spec: 06 §6", "", HEADER,
             "| --- | --- | --- | --- | --- | --- |"]
    for rid in ALL_IDS:
        lines.append(f"| {rid} | 未验证 | | | | |")
    return "\n".join(lines) + "\n"


def _cells(line: str) -> list[str]:
    raw = line.strip()
    if raw.startswith("|"):
        raw = raw[1:]
    if raw.endswith("|"):
        raw = raw[:-1]
    return [c.strip() for c in raw.split("|")]


def validate(text: str) -> list[str]:
    errors: list[str] = []
    lines = text.splitlines()
    if not lines or TITLE not in lines[0]:
        errors.append("title missing")
    if HEADER not in text:
        errors.append("table header missing")
    counts: Counter = Counter()
    for line in lines:
        if not line.lstrip().startswith("|"):
            continue
        cells = _cells(line)
        if not cells or not PREFIX_RE.match(cells[0]):
            continue  # 表头 | ID |... / 分隔 | --- |... / 其他表格行忽略
        rid = cells[0]
        if rid not in ALL_IDS:
            if ROW_ID_RE.match(rid):
                errors.append("out of range id")
            else:
                errors.append("malformed id")
            continue
        counts[rid] += 1
        if len(cells) != 6:
            errors.append(f"{rid}: malformed row")
            continue
        status, executed_at, layer, command, evidence = cells[1:]
        if status not in STATUSES:
            errors.append(f"{rid}: illegal status")
        if status == "PASS":
            for name, value in zip(PASS_FIELDS, (executed_at, layer, command, evidence)):
                if not value:
                    errors.append(f"{rid}: PASS missing {name}")
    for rid in ALL_IDS:
        if counts[rid] == 0:
            errors.append(f"{rid}: missing")
        elif counts[rid] > 1:
            errors.append(f"{rid}: duplicate row")
    if not re.search(r"Spec:\s*06\s*§6", text):
        errors.append("Spec: 06 §6 reference missing")
    return errors


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) != 1:
        sys.stderr.write("invalid arguments\n")
        return 2
    try:
        with open(args[0], "r", encoding="utf-8") as f:
            text = f.read()
    except Exception:
        sys.stderr.write("cannot read report\n")
        return 1
    errors = validate(text)
    if errors:
        for err in errors:
            sys.stderr.write(f"{err}\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
