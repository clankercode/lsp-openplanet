# PIRFL evidence: group `check` diagnostics by severity

## Goal
Human `openplanet-lsp check` output groups diagnostics so a flood of
warnings does not bury errors. Default: errors last. Config + CLI flag.
JSON order unchanged.

## Delivered
- `ErrorGroup { Last, First, Off }` in `src/config.rs`, same resolve
  ladder as `UnusedLevel` (CLI > workspace > user > default last).
  Aliases: `end`→last, `start`/`begin`→first, `none`/`false`/`interleaved`→off.
- Config key `errors = "last"|"first"|"off"`; CLI `--errors last|first|off`
  and `--errors=last`. Flag wins.
- Grouping applied once in `format_check_report_with` for plain/pretty only.
  Missing severity counts as error. Unknown severity stays with non-errors.
  JSON keeps analysis order. Summary + mismatch trailer stay last.
- `CheckReport.errors` carries the resolved group so `main.rs` did not
  need a signature change.
- SETTINGS catalog entry (`errors` last so the 80x24 config TUI snap
  still matches). `configs_equivalent` includes `a.errors == b.errors`.
- README + CHANGELOG + CHECK_HELP.

## Tests
Red first: `ErrorGroup` parse/resolve tests, then human grouping tests
(last/first/off × plain/pretty/json, missing + unknown + info/hint),
then `--errors` parse/resolve, then integration variants.

Ran:
```
cargo test --locked --lib -- --test-threads=2 cli:: config:: user_config::
# 136 passed
cargo test --locked --test cli_check_tests -- --test-threads=2
# 66 passed
cargo test --locked --test tui_config_snapshots
# 1 passed (no snap update)
```

## Review
- Goal-fit: default last, `--errors first`/`off`, workspace config,
  CLI override, JSON untouched, trailer last.
- Correctness: one grouping site; relative order inside each bucket
  preserved; `run_check` stores analysis order on the report.
- Edge: empty / single-severity already covered by existing reports;
  watch TUI out of scope (uses its own list).
- Snapshot: `errors` appended to SETTINGS so default config TUI view
  is unchanged. Scroll to the end to edit it.

## Assumptions / leftover
- `--watch` list order is unchanged (explicitly out of scope).
- `errors = true` in TOML is ignored (only `false` aliases to off).
- No known blocker against the acceptance list.
