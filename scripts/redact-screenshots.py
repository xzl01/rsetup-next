#!/usr/bin/env python3
"""
Redact the NVMe disk serial number from the captured web screenshots.

The screenshots are edited in place at pixel level rather than recaptured, so
every other pixel stays byte-identical to the artefact that was visually
verified; only the serial's glyph run is covered.

Why a partial mask is NOT used: a serial number is a unique device identifier,
so disclosing even a prefix or suffix narrows it down. The whole value is
covered. The mask is drawn as a solid bar exactly the width of the original
glyph run, which keeps the "value is not truncated" property demonstrable -
the covered run still spans the full 130 device px the 18 characters need.

The matching .json sidecar is rewritten so the raw serial does not survive in
the committed evidence. The identifier is never hardcoded here: whatever value
follows the 序列号 label in the sidecar is replaced generically, so this
repository carries no copy of the serial.

Usage:
  python3 scripts/redact-screenshots.py [--dir docs/testing/screenshots]

Measurements come from the live page (getBoundingClientRect on the serial
value element) and are recorded in MASK_RECTS below.
"""

import re
import sys
from pathlib import Path

from PIL import Image, ImageDraw

DEFAULT_DIR = Path("docs/testing/nvme-2026-09-12/screenshots")

MASKED = "[已打码]"

# In the sidecar text the serial value sits on the line after its label.
SERIAL_IN_SIDECAR = re.compile(r"(序列号\n)([A-Za-z0-9\-]{6,})")

# Device-pixel boxes of the serial glyph run, measured per capture configuration.
# Re-measure with the capture script's --eval probe if the layout changes.
MASK_RECTS = {
    "web-nvme-rock5b-desktop.png": [(1086, 302, 1086 + 130, 302 + 15)],
    "web-nvme-rock5b-mobile.png": [(136, 896, 136 + 259, 896 + 30)],
    "web-nvme-rock5b-mobile-metrics.png": [(136, 426, 136 + 259, 426 + 30)],
}

MASK_FILL = (41, 50, 70)  # the serial's own text colour: reads as redacted


def redact_png(path, boxes):
    img = Image.open(path).convert("RGB")
    draw = ImageDraw.Draw(img)
    for (x0, y0, x1, y1) in boxes:
        draw.rectangle([x0, y0, x1, y1], fill=MASK_FILL)
    img.save(path, "PNG")
    return img.size


def redact_sidecar(path, seen):
    """Replace the serial value in the metadata; record what it was."""
    text = path.read_text()
    match = SERIAL_IN_SIDECAR.search(text)
    if not match:
        return False
    seen.add(match.group(2))
    path.write_text(SERIAL_IN_SIDECAR.sub(rf"\g<1>{MASKED}", text))
    return True


def main():
    args = sys.argv[1:]
    directory = Path(args[args.index("--dir") + 1]) if "--dir" in args else DEFAULT_DIR

    failures = 0
    redacted_values = set()
    for name, boxes in MASK_RECTS.items():
        png = directory / name
        sidecar = png.with_suffix(".json")

        if not png.exists():
            print(f"FAIL  missing {png}")
            failures += 1
            continue

        size = redact_png(png, boxes)
        if not sidecar.exists():
            sidecar_state = "no sidecar to redact"
        else:
            sidecar_state = "sidecar redacted" if redact_sidecar(sidecar, redacted_values) else "sidecar had no serial"
        print(f"OK    {name}: masked {len(boxes)} box(es) on a {size[0]}x{size[1]} image; {sidecar_state}")

    # Confirm no serial value survives in any committed text artefact.
    leaked = []
    for path in sorted(directory.iterdir()):
        if path.suffix not in (".json", ".md", ".txt"):
            continue
        if SERIAL_IN_SIDECAR.search(path.read_text(errors="replace")):
            leaked.append(path.name)
    if leaked:
        print(f"FAIL  a serial value still present in: {leaked}")
        failures += 1
    else:
        print(f"OK    no serial value left in any text artefact "
              f"({len(redacted_values)} distinct value(s) redacted)")

    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
