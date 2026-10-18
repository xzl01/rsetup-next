# scripts/check_controller_acceptance.py（账本区域定位 + 行合同校验 + CLI）
"""报告格式/完整性校验器：只验账本格式合同，不证明外部证据真实。"""
import os
import re
import sys
from collections import Counter

HEADER = "| ID | status | executed_at | layer | command_or_steps | evidence |"
SEPARATOR = "| --- | --- | --- | --- | --- | --- |"
TITLE = "# Controller V1 集成验收报告"
T4_IDS = [f"T4-{i:02d}" for i in range(1, 13)]
AT_IDS = [f"AT-{i:02d}" for i in range(1, 19)]
ALL_IDS = T4_IDS + AT_IDS
STATUSES = {"PASS", "FAIL", "未验证", "BLOCKED"}
PASS_FIELDS = ("executed_at", "layer", "command_or_steps", "evidence")
ROW_ID_RE = re.compile(r"^(T4|AT)-\d{2}$")


def default_report() -> str:
    lines = ["# Controller V1 集成验收报告", "Spec: 06 §6", "", HEADER, SEPARATOR]
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


def _ledger_header_index(lines: list[str]) -> int:
    """定位真实账本：分隔线的紧邻上一行必须恰为固定表头，否则返回 -1。

    正文引用、行内代码或说明文字中出现同一合同文本不算账本表头。
    """
    for index, line in enumerate(lines):
        if line.strip() != SEPARATOR or index == 0:
            continue
        if lines[index - 1].strip() == HEADER:
            return index - 1
    return -1


def validate(text: str) -> list[str]:
    errors: list[str] = []
    lines = text.splitlines()
    if not lines or TITLE not in lines[0]:
        errors.append("title missing")
    header_index = _ledger_header_index(lines)
    if header_index < 0:
        errors.append("table header missing")
    else:
        counts: Counter = Counter()
        for line in lines[header_index + 2:]:
            stripped = line.strip()
            if not stripped.startswith("|"):
                break  # 账本数据区域到此结束；文档其他表格不参与行合同校验。
            if stripped in (HEADER, SEPARATOR):
                continue  # 仅真正的表头/分隔行豁免
            cells = _cells(line)
            rid = cells[0]
            if rid not in ALL_IDS:
                # 未定义 ID 一律拒绝；只报固定去敏信息，不回显未知行内容。
                errors.append("out of range id" if ROW_ID_RE.match(rid) else "malformed id")
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
