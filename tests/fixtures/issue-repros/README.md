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
- When adding a fixture for an *open* issue, write the test asserting the
  **desired** (fixed) outcome and mark it `#[ignore = "GH #NN open — …"]`;
  un-ignore as part of the fix commit. `cargo test -- --ignored` then shows
  exactly which issues are still unfixed.
- Gate tests live in `tests/cli_check_tests.rs` and invoke the real binary
  via `env!("CARGO_BIN_EXE_openplanet-lsp")` with `NO_COLOR=1` and color env
  scrubbed. Use `run_issue_repro(slug)`; use `run_issue_repro_typedb(slug)`
  when the trigger involves engine API types (typedb fixtures).

## Game ground truth

A fixture's LSP assertion is only meaningful against the *game compiler's*
verdict on the same bytes. `scripts/issue_repro_game.py` stages a fixture
into `OpenplanetNext/Plugins`, RemoteBuild-loads it in the live game,
captures the fresh `Openplanet.log` compile window, then unloads and removes
it (leave-as-found; never touches the fleet ledger):

```bash
scripts/issue_repro_game.py --all                 # every fixture
scripts/issue_repro_game.py 44-indexed-handle-assign --json /tmp/out.json
```

Requires Trackmania + Openplanet running with RemoteBuild on 127.0.0.1:30000.
RemoteBuild flaps intermittently — the script retries; if a probe reports
`compiled_clean=false` with an empty window, retry (the load itself wedged).
Record the game verdict in the fixture's gate-test doc comment.

## Current fixtures

| Fixture | Issue | Game verdict | Status |
|---------|-------|--------------|--------|
| `46-mixin-consumer-member` | #46 | clean | fixed (8c13ef5) — active |
| `44-indexed-handle-assign` | #44 | `not an l-value` (line 11 only) | open — ignored |
| `30-sibling-field-bare-ident` | #30 | `No matching symbol 'nod'` | fixed — active |
| `28-removed-draw-api` | #28 | `No matching symbol 'Draw::GetWidth/GetHeight'` | open — ignored (blocked on #18) |
| `38-typedb-shadowed-class` | #38 | clean | open — ignored |

- These fixtures are **not** part of the `showcase-diags` demo band — do not
  add screenshot-worthy output here.
