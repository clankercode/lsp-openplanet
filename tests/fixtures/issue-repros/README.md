# Issue reproduction fixtures

Minimal reproductions of GitHub issues, as tiny standalone plugins. One
directory per issue, named `<number>-<slug>` (e.g. `46-mixin-consumer-member`).

Purpose: end-to-end TDD for checker/parser fixes. Unit tests next to the layer
changed are the primary regression net; these fixtures prove the fix holds
through the real CLI (`openplanet-lsp check`) against a realistic plugin
layout — the same path users hit.

## Layout

Each fixture is a complete plugin:

```
tests/fixtures/issue-repros/46-mixin-consumer-member/
    info.toml     # minimal manifest
    Main.as       # the minimal repro, commented with the issue link
```

## Rules

- Keep each fixture **minimal** — strip everything not needed to reproduce.
- Reference the issue (`GH #NN`) in a comment at the top of the source file
  and in the test.
- A fixture for a *fixed* issue asserts the clean outcome (usually
  `0 diagnostics`), so a checker regression fails CI.
- When adding a fixture for an *open* issue, gate it `#[ignore]` with a note
  and un-ignore as part of the fix, or assert the current (buggy) diagnostic
  and flip the assertion in the fix commit.
- Gating tests live in `tests/cli_check_tests.rs` and invoke the real binary
  via `env!("CARGO_BIN_EXE_openplanet-lsp")` with `NO_COLOR=1` and color env
  scrubbed (see `check_command_issue_repro_*` tests).
- These fixtures are **not** part of the `showcase-diags` demo band — do not
  add screenshot-worthy output here.
