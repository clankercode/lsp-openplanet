# showcase-diags

Deliberately broken OpenPlanet plugin fixture for **openplanet-lsp** screenshots and CI.

## Purpose

- **Screenshot-friendly** count: aim **~8–14** diagnostics (not 20+)
- Diverse kinds spread across files (not one wall in a single source)
- Typecheck issues with balanced braces — not parse cascades

## Layout

| Path | Demo kinds |
|------|------------|
| `info.toml` | fake `optional_dependencies` → `DEPENDENCY_*` |
| `src/Main.as` | undefined id, unknown type, arity, arg type |
| `src/Helpers.as` | string-by-value **warning**, missing return, no member |
| `src/Overlay.as` | nvg arg types, const, handle `@=`, DEPENDENCY unknown type |

## Check

```bash
FORCE_COLOR=1 openplanet-lsp check --format pretty \
  --typedb-dir tests/fixtures/typedb \
  tests/fixtures/showcase-diags
```

Plain / CI:

```bash
NO_COLOR=1 openplanet-lsp check --format plain \
  --typedb-dir tests/fixtures/typedb \
  tests/fixtures/showcase-diags
```
