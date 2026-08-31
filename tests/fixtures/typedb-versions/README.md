# Versioned Openplanet typedb snapshots

Permanent record of Openplanet type-database dumps, one directory per
Openplanet version. These exist so multi-version support (GH #18) can
load, diff, and test against any recorded version — and so refreshing
the primary fixture (`tests/fixtures/typedb/`, via `just
typedb-refresh-apply`) never loses the previous version's surface.

Each `<version>/` holds the three files a real install produces, copied
byte-for-byte from `~/OpenplanetNext/` (or an equivalent install) at
that version:

| File | `op` stamp shape |
|------|------------------|
| `OpenplanetCore.json` | full build string, e.g. `1.29.14 (next, Public, 6fd4200f)` |
| `OpenplanetNext.json` | bare version, e.g. `1.29.14`; `mp` is the game dump timestamp |
| `Openplanet.h` | build string in the header comment |

## Recorded versions

| Version | Build | Recorded | Notes |
|---------|-------|----------|-------|
| 1.29.5 | next, Public, 1234ad9e | 2026-08-31 (copied from `tests/fixtures/typedb/`, byte-identical to the install it was harvested from) | baseline for the B00x suite |
| 1.29.14 | next, Public, 6fd4200f | 2026-08-31 | adds `Text::Join`/`Text::Repeat` (replacing `string::Join`/`string::Repeat`, which **fail to compile** on this version), `Meta::UrlProtocol` family, `Net::ParseUrlEncodedForm`/`WriteUrlEncodedForm`, `Net::FormPair`, `Time::get_MilliStamp`/`get_MicroStamp`, 13 UI widget functions; removes class `Net::SecureSocket`. No overload-set changes on shared names. |

## Adding a new version

1. Let the game regenerate the dumps (they rewrite on Openplanet update).
2. `mkdir tests/fixtures/typedb-versions/<version>` and copy
   `OpenplanetCore.json`, `OpenplanetNext.json`, `Openplanet.h` from the
   install directory unmodified.
3. Verify the `op` stamps inside the files match the directory name.
4. Add a row to the table above with the API delta (a symbol-set diff
   against the previous version).

Snapshots are immutable once recorded — a re-harvest of the same
version with a different build hash gets a suffixed directory
(`<version>-<hash>`), not an overwrite.
