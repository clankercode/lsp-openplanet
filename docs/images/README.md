# README screenshots

| File | Use |
|------|-----|
| `check-demo.png` | Hero: pretty `check` on `tests/fixtures/showcase-diags` (even pad) |
| `watch-demo.png` | Hero: `check --watch` TUI, **relaxed** density, same fixture (MakeTint detail) |
| `cli-demo.png` | Optional: version + update status collage |
| `tui-review/showcase-*.png` | Full TUI review set (compact/relaxed × sizes) from real check |

## Regenerate TUI showcase shots

```bash
cargo test --test tui_export_showcase -- --nocapture
for f in docs/images/tui-review/showcase-*.json; do
  python3 scripts/tui_frame_to_png.py "$f" -o "${f%.json}.png"
  python3 scripts/pad_screenshot.py "${f%.json}.png" --in-place --pad 16
done
# pick hero → docs/images/watch-demo.png
cp docs/images/tui-review/showcase-relaxed-hero.png docs/images/watch-demo.png
```

Or: `just tui-showcase-shots`

Pad any capture:

```bash
python3 scripts/pad_screenshot.py docs/images/check-demo.png --measure
python3 scripts/pad_screenshot.py docs/images/NEW.png --in-place   # or --pad 24
```
