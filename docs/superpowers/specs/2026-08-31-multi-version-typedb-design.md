# Multi-version Openplanet typedb support — design (GH #18)

Approved by Max 2026-08-31 ("LGTM"). Supersedes the overlay/api-deltas
sketch in the issue body; the issue carries a comment with the updated
decisions.

## Problem

The LSP holds one typedb snapshot and has no notion of "API available
since X". Plugins straddle Openplanet versions (prod players and
next/beta players at once): code migrated for 1.29.14 (`Text::Join`)
false-positives against a 1.29.5 typedb, and un-migrated code
(`string::Join`) false-positives against 1.29.14 — where it is a hard
compile error game-side, verified 2026-08-31.

## Decisions (from Max, 2026-08-31)

- Default target version = the **installed** Openplanet, read from the
  discovered install's `OpenplanetCore.json` `op` stamp. Not "latest
  stable".
- `--openplanet-version` overrides. A comma-separated list selects a
  **compatibility set**: the check must hold on every listed version.
- Unknown requested version → **hard error** naming known versions —
  unless it matches the installed dump, which is always usable for its
  own version.
- Removed symbols diagnose as **errors** (hard compile failures
  game-side), not deprecation warnings.
- Snapshots are **repo-only** for now (`tests/fixtures/typedb-versions/`).
  Distribution (fetch-on-demand, compressed or binary-encoded bundles —
  most of the JSON size is wasted space) is future work behind the
  catalog seam.
- `added_in`/`removed_in` are **derived by diffing recorded snapshots**,
  not hand-maintained. Precision is bounded by the recorded set; that is
  the contract.

## Architecture

### 1. Version catalog — `src/typedb/catalog.rs` (new)

Enumerates available typedb sources:

- the **installed** dump (existing discovery, unchanged), keyed by its
  `op` stamp;
- **recorded snapshots**: subdirectories of a versions root given by
  `--typedb-versions-dir` (in-repo: `tests/fixtures/typedb-versions/`).
  No root configured → catalog holds the installed entry only.

Catalog entry: parsed `x.y.z` version (the ordering key), branch/hash
metadata from the full stamp, paths to `OpenplanetCore.json`,
`OpenplanetNext.json`, optional `Openplanet.h`. Directory names must
match the `op` stamps inside the files; mismatch is a load error naming
the file. Storage details stay behind the catalog interface so a
compressed/binary snapshot format can slot in later without touching
consumers.

### 2. Target resolution

Default target = installed (today's behavior, byte-for-byte, when no
flag and no versions root are given). `--openplanet-version X`:

1. X matches the installed version → installed dump.
2. X matches a recorded snapshot → that snapshot.
3. Otherwise → hard error listing known versions (installed + recorded).

A comma-separated value resolves each element the same way and yields a
target set.

### 3. Multi-target checking

The `AnalysisSnapshot` (parse) is version-independent and built once.
The typechecker runs once per target `TypeIndex`. Diagnostics merge by
(file, span, kind):

- identical across all targets → emitted once, unannotated;
- fires on a strict subset → annotated with those versions, e.g.
  `` `Text::Join` not found [op 1.29.5] ``.

Any error on any target fails the check — that is the compatibility
guarantee. CLI `check` gets the full set support; the LSP server stays
single-target initially but calls the same resolution/check path.

### 4. Version-aware enrichment (post-pass)

The checker stays version-agnostic. A post-pass with catalog access
decorates merged diagnostics:

- Symbol missing on target T, present in recorded versions ≥ V →
  "added in V" (earliest recorded version containing it).
- Symbol present in older recorded versions, absent in T → "removed in
  V" (earliest recorded version lacking it), severity error.
- A small curated rename map supplies migration hints that diffs cannot:
  `string::Join` → `Text::Join`, `string::Repeat` → `Text::Repeat`,
  `Net::SecureSocket` → `Net::Socket` (`secure=true`). Where the
  migration is a plain rename it feeds `TypeDiagnosticKind::
  RemovedApiUsage { replacement }` so the existing rename-quickfix
  family works unchanged.

### 5. Surface

- CLI: `--openplanet-version <v>[,<v>...]`, `--typedb-versions-dir
  <dir>`; the existing check stamp line gains the resolved target(s).
- LSP config: keys following the existing typedb-setting convention,
  wired to the same resolution path.
- justfile: recipe running the multi-version check against the repo
  fixtures.

### 6. Error handling

- Unknown version → hard error naming known versions.
- Versions root set but a snapshot is unreadable/malformed → error
  naming the file (mirroring `TypeDbMeta::probe` style). A truncated
  dump must never look like an empty one.
- No versions root and no `--openplanet-version` → exactly today's
  behavior.

## Testing

TDD (red/green) in this worktree; generalized variants per repo
convention. Layers:

- **Catalog**: installed-match, recorded-match, unknown → error,
  no-root, stamp/dirname mismatch → error.
- **Merge**: diagnostic on all targets (unannotated) vs subset
  (annotated); exit code fails on any-target error.
- **Enrichment**: `Text::Join` on 1.29.5 target → "added in 1.29.14";
  `string::Join` on 1.29.14 target → removed-API error with
  `Text::Join` replacement and working quickfix; same for
  `string::Repeat` and `Net::SecureSocket` (type, not function —
  exercises the class path).
- **Acceptance**: fixture plugin using `Text::Join` passes on 1.29.14,
  is flagged with "added in 1.29.14" on 1.29.5, union-annotated on the
  pair. **B00x suite untouched and green** on the 1.29.5 primary
  fixture (`tests/fixtures/typedb/`).

## Out of scope (future work)

- Fetch-on-demand snapshot distribution; compressed/binary snapshot
  format (behind the catalog seam).
- Server-side multi-target UI.
- Automation for harvesting prod-branch dumps when prod ≠ the locally
  installed branch (operational: needs a prod install to copy from;
  prod caught up to 1.29.14 on 2026-08-30, so currently covered).
- Full multi-version typedb matrix CI (per the issue).
