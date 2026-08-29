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
| `b028-os-arch-defines-probe` | B028 | clean (2026-08-29 probe) ⇒ WINDOWS + WINDOWS_WINE + MANIA64 on, LINUX + MANIA32 off in the live Wine client | fixed — active |
| `b028-build-flag-probe` | B028 | clean (2026-08-29 probe) ⇒ SERVER / LOGS / HAS_DEV / DEVELOPER all off on a normal client | fixed — active |
| `b028-dup-decl-arch-os` | B028 | clean (2026-08-29 probe) — the game compiles one branch per pair; the LSP defaulted to windows/64 must match | fixed — active |
| `63-inputtext-callback-funcdef` | #63 | clean (2026-08-29 probe, OP 1.29.5) | fixed (8e49f81) — active |
| `b025-this-method-funcdef-ctor` | B025 | legal half clean; 3 diagnosed lines ERR (`Can't create delegate` ×2, `No matching signatures to 'CoroutineFunc(string)'`; 2026-08-29 probe) | fixed (342a10a) — active |
| `46-mixin-consumer-member` | #46 | clean | fixed (8c13ef5) — active |
| `44-indexed-handle-assign` | #44 | `not an l-value` (@-into-Json-index only) | fixed (70f3f97) — active |
| `30-sibling-field-bare-ident` | #30 | `No matching symbol 'nod'` | fixed — active |
| `28-removed-draw-api` | #28 | `No matching symbol 'Draw::GetWidth/GetHeight'` | fixed — active (see new row below for the 2026-08-23 extension) |
| `38-typedb-shadowed-class` | #38 | clean | fixed (db6e46b) — active |
| `52-engine-member-miss` | #52 repro 1 | `'FileName' is not a member of 'CGameCtnChallenge'` (line 11) | fixed (a970f2d) — active |
| `52b-ternary-type-ambiguity` | #52 repro 2 | `Can't find unambiguous implicit conversion…` ×3 (13:19, 14:16, 15:17; 2026-08-18 probe) | fixed — active |
| `54-ctor-implicit-conversion` | #54 | `No matching signatures to 'mat4::Inverse(vec3&)'` at 18:16 only — every mat3↔mat4 ctor conversion clean (2026-08-22 probe) | fixed — active |
| `28-removed-draw-api` | #28 + #27 | `No matching symbol 'Draw::GetWidth/GetHeight'` (2026-08-22, todo-cover-cases) + `UI::GetIO` known-ns rule | fixed — active |
| `28b-user-namespace-silent` | #28 | negative guard: namespaces with no authoritative surface must stay silent (narrowed by #58) | fixed — active |
| `58b-ns-unknown-symbol` | #58 | `No matching symbol 'TournamentStation::SendEvent'` (tournament-station, 2026-08-23) | fixed — active |
| `58c-ns-relative-nested` | #58 | FP fences: relative namespace resolution + nested paths stay silent | fixed — active |
| `58d-ns-engine-fence` | #58 | FP fence: engine namespaces stay silent with no typedb loaded (run via `--no-typedb`) | fixed — active |
| `55-getonly-prop-assign` | todo-cover-cases (kinematic-ao) | `The property has no set accessor` ×2 on `iso4.tx` (Tools.as 242:16, 2026-08-22) | fixed — active |
| `56-readonly-handle-rebind` | todo-cover-cases (kinematic-ao) | `Reference is read-only` on `ty =` value-assign (Scene.as 258:28, 2026-08-22) | fixed — active |
| `53-define-flag` | #53 | `--define DEV` activates `#if DEV` branch (staged-compile parity) | fixed — active |
| `dup-local-same-scope` | todo-cover-cases 2026-08-25 | `'cp0' is already declared` + cascade member type | fixed — active |
| `reserved-keyword-local` | todo-cover-cases 2026-08-24 | `Instead found reserved keyword 'out'` (`in`/`inout` too) | fixed — active |

GH #37 batch (game verdicts from the 2026-08-17 live probe):

| Fixture | Class | Game verdict |
|---------|-------|--------------|
| `37a-signed-unsigned-mismatch` | `i < u` | WARN `Signed/Unsigned mismatch` |
| `37b-float-truncation` | `int x = 3.7` / `int g = f` | WARN `…not exact` / `…truncated…` |
| `37c-unreachable-code` | stmt after `return` | WARN `Unreachable code` |
| `37d-variable-shadow` | inner `int x` | WARN `Variable 'x' hides…` |
| `37e-duplicate-function` | exact dup decl | ERR `…same name and parameters already exists` |
| `37f-sign-change` | `uint u = -1` | WARN `Implicit conversion changed sign of value` |

- These fixtures are **not** part of the `showcase-diags` demo band — do not
  add screenshot-worthy output here.
