#!/usr/bin/env python3
"""Measure content margins on a screenshot and pad to even borders.

Detects the content bounding box against a background color (sampled from
corners by default), reports L/R/T/B padding, then writes an image with
uniform padding on all sides.

Examples:
  # measure only
  python3 scripts/pad_screenshot.py docs/images/check-demo.png --measure

  # pad in place (backup .bak) so all sides match the largest current margin
  python3 scripts/pad_screenshot.py docs/images/check-demo.png --in-place

  # force 32px on every side
  python3 scripts/pad_screenshot.py docs/images/check-demo.png -o out.png --pad 32

  # match vertical padding onto horizontal only (keep T/B, equalize L/R to max(T,B))
  python3 scripts/pad_screenshot.py docs/images/check-demo.png -o out.png --match-vertical
"""

from __future__ import annotations

import argparse
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path

try:
    from PIL import Image
except ImportError as e:  # pragma: no cover
    sys.stderr.write("Pillow required: pip install Pillow\n")
    raise SystemExit(1) from e


@dataclass(frozen=True)
class Margins:
    left: int
    top: int
    right: int
    bottom: int

    @property
    def min(self) -> int:
        return min(self.left, self.top, self.right, self.bottom)

    @property
    def max(self) -> int:
        return max(self.left, self.top, self.right, self.bottom)

    def is_even(self) -> bool:
        return self.left == self.right == self.top == self.bottom


@dataclass(frozen=True)
class ContentBox:
    left: int
    top: int
    right: int  # exclusive
    bottom: int  # exclusive

    @property
    def width(self) -> int:
        return self.right - self.left

    @property
    def height(self) -> int:
        return self.bottom - self.top


def sample_background(im: Image.Image, inset: int = 0) -> tuple[int, ...]:
    """Sample corner pixels and pick the most common as background."""
    w, h = im.size
    pts = [
        (inset, inset),
        (w - 1 - inset, inset),
        (inset, h - 1 - inset),
        (w - 1 - inset, h - 1 - inset),
    ]
    counts: dict[tuple[int, ...], int] = {}
    for x, y in pts:
        px = im.getpixel((x, y))
        if isinstance(px, tuple):
            key: tuple[int, ...] = tuple(int(v) for v in px)
        else:
            key = (int(px),)
        counts[key] = counts.get(key, 0) + 1
    return max(counts.items(), key=lambda kv: kv[1])[0]


def color_distance(a: tuple[int, ...], b: tuple[int, ...]) -> float:
    n = min(len(a), len(b), 3)  # ignore alpha for distance
    return sum((a[i] - b[i]) ** 2 for i in range(n)) ** 0.5


def find_content_bbox(
    im: Image.Image,
    bg: tuple[int, ...],
    tolerance: float,
) -> ContentBox | None:
    """Tight bbox of pixels that differ from background by > tolerance."""
    w, h = im.size
    px = im.load()
    min_x, min_y = w, h
    max_x, max_y = -1, -1
    for y in range(h):
        for x in range(w):
            p = px[x, y]
            if isinstance(p, tuple):
                pt: tuple[int, ...] = tuple(int(v) for v in p)
            else:
                pt = (int(p),)
            if color_distance(pt, bg) > tolerance:
                if x < min_x:
                    min_x = x
                if y < min_y:
                    min_y = y
                if x > max_x:
                    max_x = x
                if y > max_y:
                    max_y = y
    if max_x < 0:
        return None
    return ContentBox(min_x, min_y, max_x + 1, max_y + 1)


def margins_from_box(size: tuple[int, int], box: ContentBox) -> Margins:
    w, h = size
    return Margins(
        left=box.left,
        top=box.top,
        right=w - box.right,
        bottom=h - box.bottom,
    )


def pad_image(
    im: Image.Image,
    box: ContentBox,
    target: Margins,
    bg: tuple[int, ...],
) -> Image.Image:
    """Crop to content box and re-pad with target margins."""
    content = im.crop((box.left, box.top, box.right, box.bottom))
    new_w = content.width + target.left + target.right
    new_h = content.height + target.top + target.bottom
    # Match mode; expand bg to mode length
    if im.mode == "RGBA" and len(bg) == 3:
        fill: tuple[int, ...] = (*bg, 255)
    elif im.mode == "RGB" and len(bg) >= 3:
        fill = bg[:3]
    else:
        fill = bg
    out = Image.new(im.mode, (new_w, new_h), fill)
    out.paste(content, (target.left, target.top))
    return out


def choose_target_margins(
    current: Margins,
    pad: int | None,
    match_vertical: bool,
    match_horizontal: bool,
    min_pad: int | None,
) -> Margins:
    if pad is not None:
        p = pad
        return Margins(p, p, p, p)

    if match_vertical:
        # Keep T/B; set L/R to the larger of current vertical (or max L/R if bigger)
        v = max(current.top, current.bottom)
        h = max(current.left, current.right, v)
        return Margins(h, current.top, h, current.bottom)

    if match_horizontal:
        h = max(current.left, current.right)
        v = max(current.top, current.bottom, h)
        return Margins(current.left, v, current.right, v)

    # Default: uniform = max of all current sides
    p = current.max
    if min_pad is not None:
        p = max(p, min_pad)
    return Margins(p, p, p, p)


def format_report(
    path: Path,
    size: tuple[int, int],
    bg: tuple[int, ...],
    box: ContentBox,
    current: Margins,
    target: Margins | None = None,
) -> str:
    lines = [
        f"file:     {path}",
        f"size:     {size[0]}×{size[1]}",
        f"bg:       {bg}",
        f"content:  ({box.left},{box.top})–({box.right},{box.bottom})  "
        f"{box.width}×{box.height}",
        f"margins:  L={current.left}  T={current.top}  "
        f"R={current.right}  B={current.bottom}",
        f"even:     {current.is_even()}  (min={current.min}, max={current.max})",
    ]
    if target is not None:
        lines.append(
            f"target:   L={target.left}  T={target.top}  "
            f"R={target.right}  B={target.bottom}"
        )
        new_w = box.width + target.left + target.right
        new_h = box.height + target.top + target.bottom
        lines.append(f"new size: {new_w}×{new_h}")
        delta = (
            f"ΔL={target.left - current.left:+d}  "
            f"ΔT={target.top - current.top:+d}  "
            f"ΔR={target.right - current.right:+d}  "
            f"ΔB={target.bottom - current.bottom:+d}"
        )
        lines.append(f"delta:    {delta}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("image", type=Path, help="input PNG/JPEG")
    ap.add_argument("-o", "--output", type=Path, help="output path (default: <stem>-padded.png)")
    ap.add_argument("--in-place", action="store_true", help="overwrite input (writes .bak backup)")
    ap.add_argument("--measure", action="store_true", help="only print measurements; do not write")
    ap.add_argument("--pad", type=int, default=None, metavar="PX", help="uniform padding in pixels")
    ap.add_argument(
        "--min-pad",
        type=int,
        default=None,
        metavar="PX",
        help="when auto-uniform, never go below this many px",
    )
    ap.add_argument(
        "--match-vertical",
        action="store_true",
        help="keep T/B; set L and R equal to max(T,B,L,R) so sides match vertical feel",
    )
    ap.add_argument(
        "--match-horizontal",
        action="store_true",
        help="keep L/R; equalize T/B to max horizontal",
    )
    ap.add_argument(
        "--tolerance",
        type=float,
        default=12.0,
        help="max RGB distance from background still treated as padding (default 12)",
    )
    ap.add_argument(
        "--bg",
        type=str,
        default=None,
        help="background as R,G,B or R,G,B,A (default: sample corners)",
    )
    args = ap.parse_args(argv)

    path: Path = args.image
    if not path.is_file():
        sys.stderr.write(f"not found: {path}\n")
        return 2

    im = Image.open(path)
    # Work in RGBA for consistent paste; convert back if needed
    work = im.convert("RGBA") if im.mode not in ("RGB", "RGBA") else im.copy()

    if args.bg:
        parts = tuple(int(x.strip()) for x in args.bg.split(","))
        bg = parts
    else:
        bg = sample_background(work)

    box = find_content_bbox(work, bg, args.tolerance)
    if box is None:
        sys.stderr.write("no content found (all background?)\n")
        return 1

    current = margins_from_box(work.size, box)
    target = choose_target_margins(
        current,
        pad=args.pad,
        match_vertical=args.match_vertical,
        match_horizontal=args.match_horizontal,
        min_pad=args.min_pad,
    )

    print(format_report(path, work.size, bg, box, current, None if args.measure else target))

    if args.measure:
        # Evaluate evenness
        if current.is_even():
            print("eval:     PASS — already evenly padded")
            return 0
        print("eval:     NEEDS PAD — margins uneven")
        return 0

    if (
        target.left == current.left
        and target.right == current.right
        and target.top == current.top
        and target.bottom == current.bottom
    ):
        print("eval:     already at target; no write needed")
        if args.output:
            work.save(args.output)
            print(f"wrote:    {args.output} (copy)")
        return 0

    out_im = pad_image(work, box, target, bg)
    # Preserve original mode when possible
    if im.mode != out_im.mode:
        out_im = out_im.convert(im.mode)

    if args.in_place:
        bak = path.with_suffix(path.suffix + ".bak")
        shutil.copy2(path, bak)
        out_path = path
        print(f"backup:   {bak}")
    elif args.output:
        out_path = args.output
    else:
        out_path = path.with_name(f"{path.stem}-padded{path.suffix}")

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_im.save(out_path)
    print(f"wrote:    {out_path}  ({out_im.size[0]}×{out_im.size[1]})")

    # Re-measure output for eval
    verify = Image.open(out_path)
    vwork = verify.convert("RGBA") if verify.mode not in ("RGB", "RGBA") else verify
    vbox = find_content_bbox(vwork, bg, args.tolerance)
    assert vbox is not None
    vm = margins_from_box(vwork.size, vbox)
    print(
        f"verify:   L={vm.left} T={vm.top} R={vm.right} B={vm.bottom}  even={vm.is_even()}"
    )
    if not vm.is_even():
        print("eval:     WARN — output margins not perfectly even (tolerance/bg?)")
        return 1
    print("eval:     PASS — evenly padded")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
