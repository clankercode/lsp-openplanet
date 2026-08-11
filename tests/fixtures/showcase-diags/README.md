# showcase-diags

Deliberately broken OpenPlanet plugin fixture for **openplanet-lsp** screenshots and CI.

## Purpose

- Curated, **stable** typecheck diagnostics (target band: **12–30** total)
- Demo-worthy mix: unknown types, undefined identifiers/members, arg count/type
  mismatches, handle/value and const issues, string-by-value **warnings**, and
  `DEPENDENCY_*` preprocessor paths from `optional_dependencies`
- **Not** a pile of parse failures from broken braces

## Layout

| Path | Role |
|------|------|
| `info.toml` | Meta + fake `optional_dependencies` (`ShowcaseFakeHook`, `ShowcaseFakeVehicle`) |
| `src/Main.as` | Entry / UI calls, `DEPENDENCY_SHOWCASEFAKEHOOK` branch |
| `src/Overlay.as` | nvg / colors / const+handle, `DEPENDENCY_SHOWCASEFAKEVEHICLE` branch |
| `src/Helpers.as` | Workspace helpers, warnings, arrays, class methods |

## Check command

From the repo root:

```bash
cargo build -q --release
NO_COLOR=1 target/release/openplanet-lsp check \
  --typedb-dir tests/fixtures/typedb \
  tests/fixtures/showcase-diags
```

Color screenshot variant: `FORCE_COLOR=1` and omit `NO_COLOR`.

Fake optional dep names keep resolution from accidentally loading real plugins.
`openplanet-lsp` still defines `DEPENDENCY_<NAME>` for each listed optional dep
(see `Config::apply_manifest`), so the `#if DEPENDENCY_*` demo branches stay
active without a Plugins dir.
