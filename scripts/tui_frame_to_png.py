#!/usr/bin/env python3
"""Render openplanet-lsp TUI TestBackend JSON frames to a dark-terminal PNG.

Input JSON shape (from scripts/tui_export_frames.rs / cargo run):
{
  "width": 100,
  "height": 30,
  "cells": [ {"ch":"x","fg":"#ff5555","bg":"#1e1e22","bold":false}, ... ]  # row-major
}

Or plain text mode: --text-frame path.txt with fixed-width lines.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError as e:
    sys.stderr.write("Pillow required\n")
    raise SystemExit(1) from e

# Dark terminal palette
BG = (30, 30, 34)
FG = (220, 223, 228)
CELL_W = 9
CELL_H = 18
PAD = 16


def parse_color(s: str | None, default: tuple[int, int, int]) -> tuple[int, int, int]:
    if not s:
        return default
    s = s.strip()
    if s.startswith("#") and len(s) == 7:
        return int(s[1:3], 16), int(s[3:5], 16), int(s[5:7], 16)
    return default


def render_json(data: dict, out: Path) -> None:
    w = int(data["width"])
    h = int(data["height"])
    cells = data["cells"]
    assert len(cells) == w * h, f"cells {len(cells)} != {w}*{h}"

    img_w = PAD * 2 + w * CELL_W
    img_h = PAD * 2 + h * CELL_H
    im = Image.new("RGB", (img_w, img_h), BG)
    draw = ImageDraw.Draw(im)
    try:
        font = ImageFont.truetype(
            "/usr/share/fonts/TTF/DejaVuSansMono.ttf", 14
        )
    except OSError:
        try:
            font = ImageFont.truetype(
                "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 14
            )
        except OSError:
            font = ImageFont.load_default()

    for y in range(h):
        for x in range(w):
            c = cells[y * w + x]
            ch = c.get("ch") or " "
            if isinstance(ch, int):
                ch = chr(ch) if ch else " "
            fg = parse_color(c.get("fg"), FG)
            bg = parse_color(c.get("bg"), BG)
            px = PAD + x * CELL_W
            py = PAD + y * CELL_H
            if bg != BG:
                draw.rectangle([px, py, px + CELL_W - 1, py + CELL_H - 1], fill=bg)
            if ch not in (" ", "\u0000"):
                draw.text((px, py + 1), ch, fill=fg, font=font)

    out.parent.mkdir(parents=True, exist_ok=True)
    im.save(out)
    print(f"wrote {out} ({img_w}x{img_h})")


def render_text(path: Path, out: Path) -> None:
    lines = path.read_text().splitlines()
    if not lines:
        raise SystemExit("empty text frame")
    w = max(len(l) for l in lines)
    h = len(lines)
    cells = []
    for line in lines:
        line = line.ljust(w)
        for ch in line:
            cells.append({"ch": ch, "fg": None, "bg": None})
    render_json({"width": w, "height": h, "cells": cells}, out)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("input", type=Path)
    ap.add_argument("-o", "--output", type=Path, required=True)
    ap.add_argument("--text", action="store_true", help="input is plain text frame")
    args = ap.parse_args()
    if args.text:
        render_text(args.input, args.output)
    else:
        data = json.loads(args.input.read_text())
        render_json(data, args.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
