#!/usr/bin/env python3
"""
Verify the generated web screenshots for the NVMe hardware panel.

The check is OCR-driven: it proves the committed PNGs really contain the
expected labels and values, are not blank or mis-rendered, leak no disk data on
the machine without NVMe, and keep the serial redacted.

Scope limit, stated honestly: tesseract cannot reliably read the smallest
monospace values (it reads the desktop firmware as "ZTA22666", and the mobile
layout drops the 综合温度 label line entirely). Those exact field values are
therefore NOT asserted from the raster here; they are covered by the device's
own /api/v1/hardware/nvme response and CLI output captured during the hardware
test. What this script asserts is what a reader can actually see in the image.

Usage:
  python3 scripts/verify-web-screenshots.py [screenshot_dir]
"""

import importlib.util
import re
import subprocess
import sys
from pathlib import Path

from PIL import Image

DEFAULT_DIR = Path("docs/testing/nvme-2026-09-12/screenshots")
REDACT_SCRIPT = Path(__file__).with_name("redact-screenshots.py")


def load_redaction_masks():
    """Reuse the redaction script's mask table, so it has one source of truth."""
    spec = importlib.util.spec_from_file_location("redact_screenshots", REDACT_SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.MASK_RECTS, module.MASK_FILL


MASK_RECTS, MASK_FILL = load_redaction_masks()

# A serial value would look like a long alphanumeric run; none may be legible.
SERIAL_IN_RASTER = re.compile(r"\b[A-Z]{2,4}[0-9]{2,}[A-Z0-9]{6,}\b")


def ocr(path, lang="chi_sim+eng", psm="6"):
    result = subprocess.run(
        ["tesseract", str(path), "-", "-l", lang, "--psm", psm],
        capture_output=True,
        text=True,
        check=False,
    )
    return result.stdout


def squeeze(text):
    """Drop all whitespace: OCR pads CJK runs with spaces."""
    return re.sub(r"\s+", "", text)


def ink_ratio(path):
    img = Image.open(path).convert("L")
    data = list(img.get_flattened_data())
    return sum(1 for v in data if v < 200) / len(data), img.size


def mask_is_solid(path, boxes, fill):
    """The redaction bar must be a uniform rectangle of the mask colour."""
    img = Image.open(path).convert("RGB")
    for box in boxes:
        pixels = list(img.crop(box).get_flattened_data())
        if not pixels:
            return False
        if any(p != fill for p in pixels):
            return False
    return True


# Each case: file, OCR substrings that must be legible, forbidden substrings.
CASES = [
    (
        "web-nvme-rock5b-desktop.png",
        ["NVMe存储", "ZHITAI", "格式化容量", "序列号", "固件版本", "综合温度",
         "可用备用空间", "已用寿命消耗", "累计读取量", "累计写入量", "通电时间",
         "不安全关机", "介质与完整性错误", "错误日志项", "953.9", "43.9", "100%"],
        ["此设备未检测到"],
        "Rock 5B desktop (NVMe present, drawer open)",
    ),
    (
        "web-nvme-rock5b-mobile.png",
        ["NVMe存储", "ZHITAI", "格式化容量", "序列号", "固件版本", "健康",
         "可用备用空间", "已用寿命消耗", "953.9", "43.9", "100%", "ZTA22006"],
        ["此设备未检测到"],
        "Rock 5B mobile 390x844 (drawer top)",
    ),
    (
        "web-nvme-rock5b-mobile-metrics.png",
        ["NVMe存储", "序列号", "固件版本", "可用备用空间", "已用寿命消耗",
         "累计读取量", "累计写入量", "通电时间", "不安全关机",
         "介质与完整性错误", "错误日志项", "41.5", "135.3", "100%", "ZTA22006"],
        ["此设备未检测到"],
        "Rock 5B mobile, drawer scrolled to bottom",
    ),
    (
        "web-nvme-rock3a-desktop.png",
        ["NVMe存储", "此设备未检测到", "当前不可用"],
        ["ZHITAI", "综合温度", "953.9", "可用备用空间"],
        "Rock 3A desktop (no NVMe, card disabled)",
    ),
    (
        "web-nvme-rock3a-mobile.png",
        ["NVMe存储", "此设备未检测到", "当前不可用"],
        ["ZHITAI", "综合温度", "953.9", "可用备用空间"],
        "Rock 3A mobile 390x844 (no NVMe, card disabled)",
    ),
]


def main():
    directory = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_DIR
    failures = 0

    print(f"{'case':44} {'size':>11} {'ink':>7} {'mask':>5}  result")
    print("-" * 90)

    for name, must, must_not, label in CASES:
        path = directory / name
        if not path.exists():
            print(f"{label:44} {'-':>11} {'-':>7} {'-':>5}  FAIL image missing")
            failures += 1
            continue

        ratio, size = ink_ratio(path)
        raster = squeeze(ocr(path))
        problems = []

        missing = [s for s in must if s not in raster]
        if missing:
            problems.append(f"missing={missing}")

        leaked = [s for s in must_not if s in raster]
        if leaked:
            problems.append(f"unexpected={leaked}")

        if ratio <= 0.01:
            problems.append("image looks blank")

        serial = SERIAL_IN_RASTER.search(raster)
        if serial:
            problems.append(f"serial-shaped text legible: {serial.group(0)}")

        boxes = MASK_RECTS.get(name)
        if boxes:
            if not mask_is_solid(path, boxes, MASK_FILL):
                problems.append("redaction bar is not a solid uniform mask")
            mask_cell = "yes"
        else:
            mask_cell = "n/a"

        ok = not problems
        failures += 0 if ok else 1
        style = "PASS" if ok else "FAIL"
        print(f"{label:44} {size[0]}x{size[1]:<6} {ratio*100:5.2f}% {mask_cell:>5}  {style}"
              + ("" if ok else " " + "; ".join(problems)))

    print("-" * 90)
    print("OVERALL:", "PASS" if failures == 0 else f"FAIL ({failures} case(s))")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
