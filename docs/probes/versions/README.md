# Versioned probe-corpus snapshots (B151)

Retained per-version copies of the **SURFACE** corpora — the ones that name
what the game EXPOSES, where the diff between two versions *is* the product.
Same idea as `tests/fixtures/typedb-versions/`, and deliberately the same
shape, because that store already worked and B151 asked for it to be extended
rather than reinvented.

## Why only some corpora are here

`docs/probes/MANIFEST.tsv` gives every artifact a `volatility`:

| volatility | retention | why |
|---|---|---|
| `SURFACE` | **kept per version, here** | a member added or removed between versions is the finding; you can only see it by holding both |
| `SEMANTICS` | one current copy | nothing diffs an old language rule, and a live stale copy invites someone citing it |
| `HYBRID` | one current copy | a language rule measured over a game-surface enumeration — see `b067` |

`just regen-golds` checks this rather than trusting it: a `SURFACE` artifact
that **nothing archives** is reported as a retention problem on every run. It
found three on its first run, which is why this directory exists.

## Recorded versions

| Version | Build | Corpora | Notes |
|---------|-------|---------|-------|
| 1.29.5 | next, Public, 1234ad9e | `b052-qualified-enum-spellings.tsv`, `b053-bare-enum-values.tsv` | copied byte-for-byte from `docs/probes/` at the 1.29.5 stamp, before any 1.29.14 regeneration. Corroborated by `tests/fixtures/typedb-versions/1.29.5`, which is the type DB these were probed against. |

## Not here, and why

`b052-scope-provenance.tsv` is `engine_version=UNKNOWN`. Filing it under
`1.29.5/` would ASSERT a version nobody measured — the precise error B148
found in `b067`, where a plausible stamp had been copied from a sibling
corpus and nothing could check it afterwards. It stays unarchived, and
`just regen-golds` keeps reporting it UNSTAMPED and NOT RETAINED, until a run
stamps it by machine.

## Adding a version

1. Regenerate the corpus against the live game (`just regen-golds-apply`, or
   the artifact's own generator). The stamp is read from `Openplanet.log`,
   never typed.
2. **Before committing the new file**, copy the OUTGOING one into
   `docs/probes/versions/<its stamped version>/`.
3. Add a row above with the symbol-set delta against the previous version.

Snapshots are immutable once recorded: a re-harvest of the same version with a
different build hash gets a suffixed directory (`<version>-<hash>`), never an
overwrite.
