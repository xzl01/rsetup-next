#!/usr/bin/env python3
"""
Render a captured ANSI/VT100 terminal stream into a PNG screenshot.

The renderer is CJK-aware:
  * it prefers a monospace font that actually contains CJK glyphs
    (Noto Sans Mono CJK SC), so Chinese text renders as real characters
    instead of tofu boxes;
  * it treats East-Asian wide characters as occupying two terminal cells,
    matching how a real terminal lays them out.

Usage:
  capture-tui-screenshot.py <raw_ansi_input> <output_png> [cols] [rows]
"""

import re
import sys
import unicodedata

from PIL import Image, ImageDraw, ImageFont

BG_COLOR = (16, 18, 15)
DEFAULT_FG = (232, 227, 213)

# ANSI SGR colour indices used by the rsetup-next TUI theme.
ANSI_COLOR_MAP = {
    30: (16, 18, 15),       # INK
    31: (255, 90, 73),      # CORAL
    32: (199, 255, 74),     # SIGNAL
    33: (255, 179, 65),     # AMBER
    34: (103, 214, 255),
    35: (200, 120, 255),
    36: (100, 220, 220),
    37: (232, 227, 213),    # BONE
    90: (139, 145, 128),    # MUTED
    91: (255, 120, 100),
    92: (220, 255, 100),
    93: (255, 200, 100),
    94: (140, 230, 255),
    95: (220, 150, 255),
    96: (150, 240, 240),
    97: (255, 255, 255),
}

# Monospace fonts that contain CJK glyphs, most preferred first.
CJK_MONO_CANDIDATES = [
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 7),  # Noto Sans Mono CJK SC
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 5),  # Noto Sans Mono CJK JP
    ("/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc", 0),
    ("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc", 0),
]

# Fallback fonts: good Latin metrics but NO CJK coverage.
LATIN_MONO_CANDIDATES = [
    ("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 0),
    ("/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf", 0),
]

PLACEHOLDER = "\0"  # cell consumed by a preceding wide glyph


def char_width(ch):
    """Terminal display width of a single character: 0, 1 or 2."""
    if not ch:
        return 0
    if unicodedata.combining(ch):
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1


def load_font(size):
    """Load a CJK-capable monospace font, falling back only if none exists.

    Returns (font, has_cjk).
    """
    for path, index in CJK_MONO_CANDIDATES:
        try:
            return ImageFont.truetype(path, size, index=index), True
        except Exception:
            continue
    for path, index in LATIN_MONO_CANDIDATES:
        try:
            return ImageFont.truetype(path, size, index=index), False
        except Exception:
            continue
    return ImageFont.load_default(), False


def parse_vt100_screen(raw_text, cols=100, rows=30):
    """Replay the ANSI stream and return a rows x cols grid of cells.

    Each cell is (char, fg, bg, bold, width). A cell whose width is 0 is a
    placeholder consumed by the wide glyph to its left and must not be drawn.
    """
    blank = (" ", DEFAULT_FG, BG_COLOR, False, 1)
    grid = [[blank for _ in range(cols)] for _ in range(rows)]

    state = {"r": 0, "c": 0, "fg": DEFAULT_FG, "bg": BG_COLOR, "bold": False}

    def clear_wide_overlap(row, col):
        """Drop a wide glyph that would be split by a write at (row, col)."""
        if row >= rows:
            return
        if col > 0 and grid[row][col - 1][4] == 2:
            grid[row][col - 1] = blank
        if grid[row][col][0] == PLACEHOLDER:
            grid[row][col] = blank

    def put(ch):
        width = char_width(ch)
        if width == 0:
            return
        if state["c"] >= cols:
            state["c"] = 0
            state["r"] += 1
        if state["r"] >= rows:
            return
        clear_wide_overlap(state["r"], state["c"])
        grid[state["r"]][state["c"]] = (ch, state["fg"], state["bg"], state["bold"], width)
        if width == 2 and state["c"] + 1 < cols:
            grid[state["r"]][state["c"] + 1] = (PLACEHOLDER, state["fg"], state["bg"], state["bold"], 0)
        state["c"] += width

    def tab():
        state["c"] = min(cols - 1, (state["c"] + 8) & ~7)

    ansi_re = re.compile(r"\x1b\[([0-9;?]*)([a-zA-Z])|\r|\n")
    pos = 0
    while pos < len(raw_text):
        m = ansi_re.search(raw_text, pos)
        if not m:
            for ch in raw_text[pos:]:
                tab() if ch == "\t" else (put(ch) if (ch >= " " or char_width(ch)) else None)
            break

        for ch in raw_text[pos:m.start()]:
            if ch == "\t":
                tab()
            elif ch >= " " or char_width(ch):
                put(ch)

        if m.group(0) == "\r":
            state["c"] = 0
        elif m.group(0) == "\n":
            state["r"] = min(rows - 1, state["r"] + 1)
            state["c"] = 0
        else:
            params_str, cmd = m.group(1), m.group(2)
            params = [int(p) for p in params_str.split(";") if p.isdigit()] if params_str else []

            if cmd == "m":
                for p in params or [0]:
                    if p == 0:
                        state["fg"], state["bg"], state["bold"] = DEFAULT_FG, BG_COLOR, False
                    elif p == 1:
                        state["bold"] = True
                    elif p == 22:
                        state["bold"] = False
                    elif p in ANSI_COLOR_MAP:
                        state["fg"] = ANSI_COLOR_MAP[p]
                    elif p == 39:
                        state["fg"] = DEFAULT_FG
                    elif p == 49:
                        state["bg"] = BG_COLOR
            elif cmd in ("H", "f"):
                r = params[0] - 1 if len(params) > 0 and params[0] > 0 else 0
                c = params[1] - 1 if len(params) > 1 and params[1] > 0 else 0
                state["r"] = min(max(r, 0), rows - 1)
                state["c"] = min(max(c, 0), cols - 1)
            elif cmd == "A":
                state["r"] = max(0, state["r"] - (params[0] if params else 1))
            elif cmd == "B":
                state["r"] = min(rows - 1, state["r"] + (params[0] if params else 1))
            elif cmd == "C":
                state["c"] = min(cols - 1, state["c"] + (params[0] if params else 1))
            elif cmd == "D":
                state["c"] = max(0, state["c"] - (params[0] if params else 1))
            elif cmd == "J" and (params or [0])[0] == 2:
                grid = [[blank for _ in range(cols)] for _ in range(rows)]
                state["r"] = state["c"] = 0
            elif cmd == "K":
                mode = (params or [0])[0]
                start = 0 if mode == 1 else state["c"]
                end = state["c"] + 1 if mode == 1 else cols
                for c in range(start, min(end, cols)):
                    grid[state["r"]][c] = blank

        pos = m.end()

    return grid


def grid_to_text(grid):
    """Flatten the grid back to plain text (one line per terminal row)."""
    lines = []
    for row in grid:
        lines.append("".join(cell[0] for cell in row if cell[4] != 0).rstrip())
    return "\n".join(lines)


def render_to_png(grid, out_path, cols=100, rows=30, font_size=15):
    font, has_cjk = load_font(font_size)

    # Derive the cell box from the font's own metrics so text cannot drift.
    try:
        latin_advance = font.getlength("M")
        cjk_advance = font.getlength("漢")
    except Exception:
        latin_advance, cjk_advance = font_size * 0.6, font_size * 1.2
    char_w = max(1, int(round(latin_advance)))
    # A monospace CJK font advances CJK glyphs exactly two Latin cells; if the
    # chosen font does not, scale the CJK glyph so columns still line up.
    cjk_scale = (2 * char_w / cjk_advance) if cjk_advance else 1.0

    ascent, descent = font.getmetrics()
    char_h = ascent + descent
    pad_x = pad_y = 16
    img_w = cols * char_w + pad_x * 2
    img_h = rows * char_h + pad_y * 2

    img = Image.new("RGB", (img_w, img_h), BG_COLOR)
    draw = ImageDraw.Draw(img)

    for r in range(rows):
        for c in range(cols):
            ch, fg, bg, _bold, width = grid[r][c]
            if width == 0:
                continue
            x = pad_x + c * char_w
            y = pad_y + r * char_h
            if bg != BG_COLOR:
                draw.rectangle([x, y, x + char_w * width - 1, y + char_h - 1], fill=bg)
            if ch == " ":
                continue
            if width == 2 and abs(cjk_scale - 1.0) > 0.01:
                # Render the wide glyph into its own layer, then scale it to
                # exactly two cells so the monospace grid stays aligned.
                layer = Image.new("RGBA", (max(1, int(round(cjk_advance))), char_h), (0, 0, 0, 0))
                ImageDraw.Draw(layer).text((0, 0), ch, fill=fg + (255,), font=font)
                layer = layer.resize((char_w * 2, char_h), Image.LANCZOS)
                img.paste(layer, (x, y), layer)
            else:
                draw.text((x, y), ch, fill=fg, font=font)

    img.save(out_path, "PNG")
    return img_w, img_h, has_cjk


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 1
    raw_file, out_file = sys.argv[1], sys.argv[2]
    cols = int(sys.argv[3]) if len(sys.argv) > 3 else 100
    rows = int(sys.argv[4]) if len(sys.argv) > 4 else 30

    with open(raw_file, "r", encoding="utf-8", errors="replace") as handle:
        text = handle.read()

    grid = parse_vt100_screen(text, cols=cols, rows=rows)
    img_w, img_h, has_cjk = render_to_png(grid, out_file, cols=cols, rows=rows)
    print(f"Saved screenshot to {out_file} ({img_w}x{img_h}, cjk_font={has_cjk})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
