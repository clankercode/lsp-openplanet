# Multi-version Openplanet typedb — Implementation Plan (GH #18)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Check plugins against one or several recorded Openplanet versions (default: the installed one), with per-version diagnostic annotations and added-in/removed-in enrichment derived from recorded snapshots.

**Architecture:** A new `VersionCatalog` enumerates typedb sources (installed dump + snapshot dirs under a versions root) and resolves `--openplanet-version` requests. `run_check` mirrors the GH #57 define-matrix pattern: the parse snapshot is built once per combo, the typecheck pass runs once per target `TypeIndex`, and diagnostics merge with per-version labels exactly like combo labels. Version enrichment is data handed to the checker (a per-target map of qualified name → note), never version logic inside it.

**Tech Stack:** Rust (existing crate, no new deps), serde_json for light snapshot scans.

**Spec:** `docs/superpowers/specs/2026-08-31-multi-version-typedb-design.md`

## Global Constraints

- Builds/tests limited to 2 threads: `cargo test --locked -j 2 -- --test-threads=2` (Max's system etiquette).
- `tests/fixtures/typedb/` (1.29.5 primary fixture) is untouched; the B00x suite must stay green.
- With no `--openplanet-version` and no `--typedb-versions-dir`, behavior is byte-for-byte today's (single installed/`--typedb-dir` index, no labels).
- Removed symbols are ERROR severity (game hard-fails; verified `string::Join` on 1.29.14).
- Unknown requested version → hard error naming known versions; a request matching the installed dump always resolves to it.
- TDD red/green per task; commit per task. Test-variant generalization per repo CLAUDE.md.
- Snapshots live at `tests/fixtures/typedb-versions/<version>/{OpenplanetCore.json,OpenplanetNext.json,Openplanet.h}`; dirname must equal the parsed `x.y.z` of the `op` stamp inside.

---

### Task 1: `OpVersion` — parse and order Openplanet version stamps

**Files:**
- Create: `src/typedb/catalog.rs` (module start)
- Modify: `src/typedb/mod.rs` (add `pub mod catalog;` + re-exports)
- Test: inline `#[cfg(test)]` in `src/typedb/catalog.rs` (repo convention: unit tests live in-module, see `src/typedb/index.rs`)

**Interfaces:**
- Produces: `pub struct OpVersion { pub major: u32, pub minor: u32, pub patch: u32 }` implementing `Ord`, `Display` (renders `1.29.14`), and `pub fn parse(stamp: &str) -> Option<OpVersion>` accepting both bare (`"1.29.14"`) and full Core stamps (`"1.29.14 (next, Public, 6fd4200f)"`); `pub fn parse_meta(stamp: &str) -> Option<(OpVersion, Option<String>)>` where the second element is the parenthesized metadata (`"next, Public, 6fd4200f"`).

- [ ] **Step 1: Write the failing tests**

```rust
// src/typedb/catalog.rs
//! GH #18: the version catalog — which Openplanet typedbs are available
//! (installed + recorded snapshots), and which one(s) a check targets.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_version_parses_bare_and_full_stamps() {
        assert_eq!(OpVersion::parse("1.29.14"), Some(OpVersion { major: 1, minor: 29, patch: 14 }));
        assert_eq!(
            OpVersion::parse("1.29.5 (next, Public, 1234ad9e)"),
            Some(OpVersion { major: 1, minor: 29, patch: 5 })
        );
    }

    #[test]
    fn op_version_rejects_malformed() {
        for bad in ["", "1.29", "next", "1.29.x", "1.29.5.2"] {
            assert_eq!(OpVersion::parse(bad), None, "should reject {bad:?}");
        }
    }

    #[test]
    fn op_version_orders_numerically_not_lexically() {
        let v5 = OpVersion::parse("1.29.5").unwrap();
        let v14 = OpVersion::parse("1.29.14").unwrap();
        assert!(v5 < v14, "1.29.5 < 1.29.14 (lexical order would invert this)");
        assert_eq!(v14.to_string(), "1.29.14");
    }

    #[test]
    fn op_version_meta_extracts_parenthetical() {
        let (v, meta) = OpVersion::parse_meta("1.29.14 (next, Public, 6fd4200f)").unwrap();
        assert_eq!(v.to_string(), "1.29.14");
        assert_eq!(meta.as_deref(), Some("next, Public, 6fd4200f"));
        let (_, none) = OpVersion::parse_meta("1.29.14").unwrap();
        assert_eq!(none, None);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --locked -j 2 --lib typedb::catalog -- --test-threads=2`
Expected: compile error — `OpVersion` not defined.

- [ ] **Step 3: Implement**

```rust
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl OpVersion {
    /// Parse `"1.29.14"` or a full Core stamp `"1.29.14 (next, Public, …)"`.
    /// Exactly three numeric dot-separated components; anything else is None.
    pub fn parse(stamp: &str) -> Option<Self> {
        Self::parse_meta(stamp).map(|(v, _)| v)
    }

    pub fn parse_meta(stamp: &str) -> Option<(Self, Option<String>)> {
        let stamp = stamp.trim();
        let (head, meta) = match stamp.split_once(" (") {
            Some((h, rest)) => (h, rest.strip_suffix(')').map(str::to_string)),
            None => (stamp, None),
        };
        let mut parts = head.split('.');
        let (a, b, c) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() {
            return None;
        }
        let v = OpVersion {
            major: a.parse().ok()?,
            minor: b.parse().ok()?,
            patch: c.parse().ok()?,
        };
        Some((v, meta))
    }
}

impl fmt::Display for OpVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}
```

Derived `Ord` on `(major, minor, patch)` field order gives numeric ordering.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --locked -j 2 --lib typedb::catalog -- --test-threads=2`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/typedb/catalog.rs src/typedb/mod.rs
git commit -m "feat(catalog): OpVersion stamp parsing and numeric ordering (GH #18)"
```

---

### Task 2: Catalog discovery — recorded snapshots + installed entry

**Files:**
- Modify: `src/typedb/catalog.rs`
- Test: inline module tests (tmp-dir fixtures, mirroring the tiny-JSON pattern of `src/typedb/index.rs` tests, e.g. line 2948)

**Interfaces:**
- Consumes: `OpVersion` (Task 1); `crate::typedb::index::TypeDbMeta::probe(&Path) -> Result<TypeDbMeta, String>` (existing, index.rs:~226) for stamps.
- Produces:

```rust
pub struct CatalogEntry {
    pub version: OpVersion,
    pub label: String,          // "1.29.14" — what annotations/flags use
    pub full_stamp: String,     // "1.29.14 (next, Public, 6fd4200f)"
    pub core_json: PathBuf,
    pub game_json: PathBuf,
    pub source: EntrySource,    // Installed | Recorded
}
pub enum EntrySource { Installed, Recorded }
pub struct VersionCatalog { /* entries sorted by version */ }
impl VersionCatalog {
    /// `installed`: the (core_json, game_json) the existing discovery/--typedb-dir
    /// resolved, if any. `versions_root`: --typedb-versions-dir, if given.
    pub fn build(
        installed: Option<(&Path, &Path)>,
        versions_root: Option<&Path>,
    ) -> Result<Self, String>;
    pub fn entries(&self) -> &[CatalogEntry];
    pub fn known_versions(&self) -> Vec<String>; // sorted labels, installed marked
}
```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
fn write_snapshot(root: &std::path::Path, dir: &str, core_op: &str, game_op: &str) {
    let d = root.join(dir);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("OpenplanetCore.json"),
        format!(r#"{{"op":"{core_op}","functions":[],"classes":[],"enums":[]}}"#),
    )
    .unwrap();
    std::fs::write(
        d.join("OpenplanetNext.json"),
        format!(r#"{{"op":"{game_op}","mp":"2026-02-03 03:51:19","ns":{{}}}}"#),
    )
    .unwrap();
}

#[test]
fn catalog_discovers_recorded_snapshots_sorted() {
    let tmp = tempfile::tempdir().unwrap();
    write_snapshot(tmp.path(), "1.29.14", "1.29.14 (next, Public, 6fd4200f)", "1.29.14");
    write_snapshot(tmp.path(), "1.29.5", "1.29.5 (next, Public, 1234ad9e)", "1.29.5");
    let cat = VersionCatalog::build(None, Some(tmp.path())).unwrap();
    let labels: Vec<_> = cat.entries().iter().map(|e| e.label.as_str()).collect();
    assert_eq!(labels, ["1.29.5", "1.29.14"], "numeric order, not lexical");
}

#[test]
fn catalog_rejects_dirname_stamp_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    write_snapshot(tmp.path(), "1.29.5", "1.29.14 (next, Public, 6fd4200f)", "1.29.14");
    let err = VersionCatalog::build(None, Some(tmp.path())).unwrap_err();
    assert!(err.contains("1.29.5") && err.contains("1.29.14"), "names both sides: {err}");
}

#[test]
fn catalog_includes_installed_entry_from_paths() {
    let tmp = tempfile::tempdir().unwrap();
    write_snapshot(tmp.path(), "install", "1.29.9 (next, Public, abcd1234)", "1.29.9");
    let d = tmp.path().join("install");
    let cat = VersionCatalog::build(
        Some((&d.join("OpenplanetCore.json"), &d.join("OpenplanetNext.json"))),
        None,
    )
    .unwrap();
    assert_eq!(cat.entries().len(), 1);
    assert!(matches!(cat.entries()[0].source, EntrySource::Installed));
    assert_eq!(cat.entries()[0].label, "1.29.9");
}

#[test]
fn catalog_empty_when_no_sources() {
    let cat = VersionCatalog::build(None, None).unwrap();
    assert!(cat.entries().is_empty());
}

#[test]
fn catalog_ignores_non_snapshot_dirs_but_fails_on_broken_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    // A README or stray file at the root is ignored…
    std::fs::write(tmp.path().join("README.md"), "docs").unwrap();
    write_snapshot(tmp.path(), "1.29.5", "1.29.5 (next, Public, 1234ad9e)", "1.29.5");
    // …but a version-named dir with a truncated dump is an error, never
    // silently skipped (mirrors TypeDbMeta::probe's truncated-harvest rule).
    let broken = tmp.path().join("1.29.6");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("OpenplanetCore.json"), r#"{"op":"1.29.6","functions":[{"na"#).unwrap();
    let err = VersionCatalog::build(None, Some(tmp.path())).unwrap_err();
    assert!(err.contains("1.29.6"), "error names the broken snapshot: {err}");
}
```

Note: if `tempfile` is not already a dev-dependency, use the repo's existing tmp-dir helper instead — grep `fn tmp` / `TempDir` in `src/typedb/index.rs` tests and reuse that pattern; do not add a dependency.

- [ ] **Step 2: Run to verify failure** — same command as Task 1; expect compile errors on missing types.

- [ ] **Step 3: Implement**

Sketch — read dir entries of `versions_root`; for each subdirectory whose name parses as `OpVersion`, `TypeDbMeta::probe` both JSON files (propagating its error strings prefixed with the snapshot dir), verify `OpVersion::parse(meta.op)` equals the dirname's version (else error naming both), and push a `CatalogEntry`. Non-version-named entries (README.md) are skipped. The installed pair, when given, probes the core file for its stamp and becomes an `EntrySource::Installed` entry. Sort entries by `version`. `known_versions()` renders labels, suffixing the installed entry with `" (installed)"`.

- [ ] **Step 4: Run to verify pass** — all catalog tests green.

- [ ] **Step 5: Commit**

```bash
git add src/typedb/catalog.rs
git commit -m "feat(catalog): discover recorded snapshots + installed entry, stamp-verified (GH #18)"
```

---

### Task 3: Target resolution — `--openplanet-version` semantics

**Files:**
- Modify: `src/typedb/catalog.rs`
- Test: inline module tests

**Interfaces:**
- Produces: `impl VersionCatalog { pub fn resolve_targets(&self, requested: Option<&str>) -> Result<Vec<&CatalogEntry>, String> }`
  - `None` → the installed entry alone (or empty vec when catalog has no installed entry — caller keeps today's "no typedb" path).
  - `Some("1.29.5")` → matching entry (installed match preferred over recorded when both carry the version).
  - `Some("1.29.5,1.29.14")` → both, deduplicated, input order preserved.
  - Unknown version → `Err` listing `known_versions()`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn resolve_default_is_installed() { /* build catalog w/ installed 1.29.9 + recorded 1.29.5;
    resolve_targets(None) == vec![installed entry] */ }

#[test]
fn resolve_matches_installed_before_recorded() { /* installed 1.29.5 AND recorded 1.29.5:
    resolve_targets(Some("1.29.5")) yields the Installed-source entry */ }

#[test]
fn resolve_comma_list_dedups_preserving_order() { /* "1.29.14,1.29.5,1.29.14" →
    labels ["1.29.14", "1.29.5"] — input order, first occurrence wins */ }

#[test]
fn resolve_unknown_version_errors_naming_known() {
    // catalog: installed 1.29.9, recorded 1.29.5 + 1.29.14
    let err = cat.resolve_targets(Some("1.29.9999")).unwrap_err();
    for known in ["1.29.5", "1.29.9", "1.29.14"] {
        assert!(err.contains(known), "error must name {known}: {err}");
    }
}

#[test]
fn resolve_rejects_malformed_version_string() {
    let err = cat.resolve_targets(Some("banana")).unwrap_err();
    assert!(err.contains("banana"));
}
```

(Write these as real tests with the Task 2 helpers, not comments.)

- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement** — parse each comma-separated element with `OpVersion::parse` (malformed → error echoing the input); match against entries by `version`, preferring `EntrySource::Installed`; dedup by version keeping first occurrence.
- [ ] **Step 4: Run to verify pass.**
- [ ] **Step 5: Commit**

```bash
git add src/typedb/catalog.rs
git commit -m "feat(catalog): resolve --openplanet-version targets, hard error on unknown (GH #18)"
```

---

### Task 4: Symbol presence + `VersionNotes` (added_in / removed_in, derived)

**Files:**
- Modify: `src/typedb/catalog.rs`
- Test: inline module tests + two spot-check tests against the real recorded fixtures (path: `tests/fixtures/typedb-versions/`, resolved relative to `env!("CARGO_MANIFEST_DIR")` — the pattern `src/server/completion.rs:1477` uses for real-typedb tests)

**Interfaces:**
- Produces:

```rust
/// Qualified names (functions, classes, enums — Core dump only) present in
/// one snapshot. Light scan: serde_json::Value, names only, no TypeIndex.
fn symbol_set(core_json: &Path) -> Result<HashSet<String>, String>;

pub struct VersionNotes { notes: HashMap<String, String> }
impl VersionNotes {
    pub fn get(&self, qualified: &str) -> Option<&str>;
}
impl VersionCatalog {
    /// Notes for symbols ABSENT from `target` but present in some recorded
    /// version: "added in Openplanet X" (earliest containing version > target)
    /// or "removed in Openplanet Y" (earliest version lacking it after a
    /// containing one ≤ target… computed from the sparse recorded set).
    pub fn notes_for_target(&self, target: &CatalogEntry) -> Result<VersionNotes, String>;
}
```

Rule, precisely: for each qualified name Q present in at least one entry but absent from `target` (version T):
- Let `C` = versions containing Q. If `min(C) > T` → note `"added in Openplanet {min(C)}"`.
- Else if `max(C) < T` → note `"removed in Openplanet {V}"` where V = the smallest catalog version > `max(C)` that lacks Q (with only {1.29.5, 1.29.14} recorded, `string::Join` on target 1.29.14 → "removed in Openplanet 1.29.14").
- Q present in versions both below and above T but not at T (odd, possible with sparse data) → no note (silence beats a wrong claim).

- [ ] **Step 1: Write the failing tests** — tiny-fixture tests for each rule branch (function added-later, function removed-earlier, class removed, enum added, present-both-sides → no note), plus real-fixture spot checks:

```rust
#[test]
fn real_fixtures_derive_text_join_added_and_string_join_removed() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/typedb-versions");
    let cat = VersionCatalog::build(None, Some(&root)).unwrap();
    let v5 = cat.entries().iter().find(|e| e.label == "1.29.5").unwrap();
    let v14 = cat.entries().iter().find(|e| e.label == "1.29.14").unwrap();
    let notes5 = cat.notes_for_target(v5).unwrap();
    assert_eq!(notes5.get("Text::Join"), Some("added in Openplanet 1.29.14"));
    let notes14 = cat.notes_for_target(v14).unwrap();
    assert_eq!(notes14.get("string::Join"), Some("removed in Openplanet 1.29.14"));
    assert_eq!(notes14.get("Net::SecureSocket"), Some("removed in Openplanet 1.29.14"));
    assert_eq!(notes14.get("Text::Join"), None, "present on target → no note");
}
```

- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement** — `symbol_set` parses the Core JSON once (serde_json::Value), collecting `ns::name` (or bare `name` when `ns` is empty/absent) from `functions`, `classes`, `enums` arrays. `notes_for_target` builds each entry's set lazily (cache in a `OnceCell`/`RefCell<HashMap<version, HashSet>>` or just compute per call — N is tiny), then applies the rule above.
- [ ] **Step 4: Run to verify pass.**
- [ ] **Step 5: Commit**

```bash
git add src/typedb/catalog.rs
git commit -m "feat(catalog): derive added_in/removed_in notes by diffing recorded snapshots (GH #18)"
```

---

### Task 5: CLI flags — `--openplanet-version`, `--typedb-versions-dir`

**Files:**
- Modify: `src/cli/mod.rs` — `CheckOptions` (line ~183, next to `typedb_dir`/`no_typedb`), the arg parser (the `--typedb-dir` arm is at ~line 566; mirror it), and the conflict check at ~608 (`--no-typedb` + `--typedb-dir` is already rejected; extend: `--no-typedb` conflicts with both new flags).
- Test: existing parse-test cluster in `src/cli/mod.rs` (~1708 onward; mirror `parse_check_args_typedb_dir_rejects_following_flag`).

**Interfaces:**
- Produces: `CheckOptions { pub openplanet_version: Option<String>, pub typedb_versions_dir: Option<PathBuf>, … }` — raw string kept; resolution happens in `run_check` (Task 6).

- [ ] **Step 1: Write failing parse tests** — value captured; missing value rejected; following-flag-as-value rejected (mirror the `--typedb-dir` variants, including the `--weird`/`-cache` literal-value cases at ~1776/1816); `--no-typedb --openplanet-version X` → usage error; same for `--typedb-versions-dir`.
- [ ] **Step 2: Run** `cargo test --locked -j 2 --lib cli -- --test-threads=2` — expect failures.
- [ ] **Step 3: Implement** the two parser arms + conflict checks, copying the existing `--typedb-dir` arm's value-handling exactly.
- [ ] **Step 4: Run to verify pass.**
- [ ] **Step 5: Commit**

```bash
git add src/cli/mod.rs
git commit -m "feat(cli): parse --openplanet-version and --typedb-versions-dir (GH #18)"
```

---

### Task 6: Multi-target `run_check` — load N indexes, tag + merge versions

**Files:**
- Modify: `src/cli/mod.rs` — `CliDiagnostic` (~295: add `pub versions: Vec<String>`, default empty; every existing constructor site sets `versions: Vec::new()` — grep `combos:` to find them all), `run_check` (~686), `load_type_index` (~1143).
- Modify: `src/cli/matrix.rs` — `merge_combo_diagnostics` (~155): the merge Key stays (path, line, col, severity, message); union `versions` exactly as `combos` are unioned.
- Test: new integration test `tests/gh18_multi_version.rs` using two tiny typedb snapshots + a one-file plugin fixture (follow the tmp-plugin pattern used by existing `tests/b0*.rs` suites — grep `fn write_plugin` in `tests/` and reuse the closest helper).

**Interfaces:**
- Consumes: `VersionCatalog::build`, `resolve_targets` (Tasks 2-3); existing `TypeIndex::load`.
- Produces: `run_check` behavior —
  - targets resolved once: installed pair from config (as today) + versions root → catalog → `resolve_targets(options.openplanet_version.as_deref())`;
  - **one `TypeIndex` loaded per target** (installed target loads exactly the paths it does today, preserving B064's same-read stamp rule);
  - the per-combo loop gains an inner per-target loop: `collect_pass_diagnostics(...)` is called with each target's index; when >1 target, each diagnostic appended in that pass gets `versions.push(target.label.clone())` (mirror the `matrix_run` combo-tagging block at ~778);
  - single-target runs leave `versions` empty → formatters render nothing (exact today's output);
  - `CheckReport.type_db` stays the FIRST target's stamps (back-compat); add `pub targets: Vec<String>` (labels) to `CheckReport`.

- [ ] **Step 1: Write the failing integration test**

```rust
// tests/gh18_multi_version.rs — two snapshots differing by one function:
// v1 lacks Ns::Gone and has Ns::Old; v2 has Ns::Gone and lacks Ns::Old.
// Plugin source calls both. Checked against "1.0.0,2.0.0":
//  - `Ns::Old`  → diagnostic labeled with 2.0.0 only;
//  - `Ns::Gone` → diagnostic labeled with 1.0.0 only;
//  - a call to a function in NEITHER (Ns::Never) → one unlabeled diagnostic
//    (fires on all targets → merged, no annotation).
// Also: single-target run (no flag, --typedb-dir at v1) has empty
// `versions` on every diagnostic.
```

Write it concretely with the fixture helpers; assert on `CheckReport.diagnostics[..].versions` (call `run_check` directly — the pattern `src/cli/mod.rs`'s own tests use — rather than shelling out).

- [ ] **Step 2: Run** `cargo test --locked -j 2 --test gh18_multi_version -- --test-threads=2` — expect failure (no `versions` field / flags unwired).
- [ ] **Step 3: Implement** per the Produces block. Wire order inside `run_check`: resolve catalog+targets immediately after `apply_typedb_dir` (so `--typedb-dir` feeds the installed pair), before the combo loop; error path uses `CliError::Check` with the catalog's known-versions message. `--no-typedb` bypasses the catalog entirely (existing early return in `load_type_index`).
- [ ] **Step 4: Run the new test AND the full lib+integration suite** — `cargo test --locked -j 2 -- --test-threads=2`. B00x must be green.
- [ ] **Step 5: Commit**

```bash
git add src/cli/mod.rs src/cli/matrix.rs tests/gh18_multi_version.rs
git commit -m "feat(check): run typecheck per target version, merge with per-version labels (GH #18)"
```

---

### Task 7: Surface the labels — text/pretty/JSON output + stamp line

**Files:**
- Modify: `src/cli/mod.rs` — the text formatter and JSON report builder (~991-1036: `type_db` already lands in the JSON doc; add per-diagnostic `versions` array when non-empty, and top-level `targets`).
- Modify: `src/cli/pretty.rs` — render `[op 1.29.5]` after the message, exactly where combo labels render (grep `combos` in pretty.rs and mirror).
- Modify: the stderr stamp line call site (grep `summary_line()` in `src/cli/mod.rs`): multi-target runs print one line per target or a joined `targets: 1.29.5, 1.29.14` suffix — match the existing line's format conventions.
- Test: extend `tests/gh18_multi_version.rs` asserting on rendered text output and JSON structure (the existing JSON-shape test at ~1464 `type_db_versions_to_json_keys` shows the assertion style).

- [ ] **Step 1: Write failing assertions** — labeled diagnostic renders `[op 1.0.0]`; unlabeled renders no bracket; JSON diagnostic objects carry `"versions": ["1.0.0"]` only when labeled; report JSON carries `"targets"`.
- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run to verify pass (full suite).**
- [ ] **Step 5: Commit**

```bash
git add src/cli/mod.rs src/cli/pretty.rs tests/gh18_multi_version.rs
git commit -m "feat(check): render per-version labels in text/pretty/JSON output (GH #18)"
```

---

### Task 8: Version notes reach diagnostics — checker hook

**Files:**
- Modify: `src/typecheck/checker.rs` — `NoMatchingSymbol` render (~376) and its producers; the checker's context struct (locate with `grep -n "type_index\|TypeIndex" src/typecheck/checker.rs | head` — thread `version_notes: Option<&VersionNotes>` alongside however the index reaches the checker; `src/typecheck/global_scope.rs` is the likely seam since it wraps the index).
- Modify: `src/cli/mod.rs` — `collect_pass_diagnostics` gains `version_notes: Option<&VersionNotes>`; `run_check` computes `catalog.notes_for_target(target)` per target (only when the catalog has >1 entry — a lone installed dump has nothing to diff) and passes it through. LSP server call sites of the same collection path pass `None` for now.
- Test: checker-level unit test (in checker.rs's test module, mirroring the NoMatchingSymbol tests — grep `no_matching_symbol` there) + one end-to-end assertion in `tests/gh18_multi_version.rs`.

**Interfaces:**
- Consumes: `VersionNotes::get` (Task 4).
- Produces: `NoMatchingSymbol` message gains a parenthetical when a note exists:
  `No matching symbol 'Text::Join' (added in Openplanet 1.29.14; target is 1.29.5)` — note text from `VersionNotes`, target label appended by the checker from a `target_label: Option<&str>` passed with the notes. The checker performs NO version comparison — it renders supplied strings. Design rule (spec §4): version logic lives in the catalog.

- [ ] **Step 1: Write the failing checker test** — a typedb without `Text::Join` but with namespace `Text` populated (so trigger (a) fires), notes map containing `Text::Join → "added in Openplanet 1.29.14"`, target label `"1.29.5"`; assert the exact message above. Negative control: same setup, no notes → message unchanged from today.
- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement the threading + render.**
- [ ] **Step 4: Run to verify pass (full suite — every existing NoMatchingSymbol test must be untouched, proving the no-notes path is byte-identical).**
- [ ] **Step 5: Commit**

```bash
git add src/typecheck/checker.rs src/cli/mod.rs tests/gh18_multi_version.rs
git commit -m "feat(check): version-aware added-in/removed-in notes on missing symbols (GH #18)"
```

---

### Task 9: Curated migrations — `string::Join`/`string::Repeat` relocations

**Files:**
- Modify: `src/typecheck/checker.rs` — `removed_api_hint` (~4683) and the removed/relocated-namespace gate for trigger (b) (the `Draw` list — locate via ~3945 "removed-API relocation table"; add `string`).
- Test: checker test module (mirror the existing `Draw::GetWidth` relocation tests — grep `removed_api_hint\|Draw::GetWidth` in tests).

**Interfaces:**
- Produces: on a typedb lacking the `string` namespace (1.29.14+), `string::Join(parts, ", ")` → `NoMatchingSymbol` ERROR with hint `Text::Join`, rendered `No matching symbol 'string::Join' — use `Text::Join` instead` (plus Task 8's removed-in note when a catalog is present). Same for `string::Repeat` → `Text::Repeat`.

- [ ] **Step 1: Write the failing tests** — both functions, against a minimal typedb WITHOUT the `string` ns; negative control: same calls against the 1.29.5-shaped typedb (ns present with Join/Repeat) stay silent — this is the version straddle, and it's what keeps B00x green.
- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement** — two match arms in `removed_api_hint`; `string` added to the trigger-(b) namespace list.
- [ ] **Step 4: Verify pass + full suite.** Then check the quickfix: run the existing `did_you_mean`/fix-family tests (`cargo test --locked -j 2 --lib code_actions -- --test-threads=2`) and confirm whether NoMatchingSymbol hints already produce a rename action (src/server/code_actions/fixes/did_you_mean.rs). If they do, add one test asserting the `string::Join` → `Text::Join` action; if they don't, file a follow-up bl ticket instead of expanding this task (the spec's quickfix requirement is satisfied via hint text; note the deviation in the PR description).
- [ ] **Step 5: Commit**

```bash
git add src/typecheck/checker.rs
git commit -m "feat(check): string::Join/Repeat relocations to Text:: (removed in OP 1.29.14, GH #18)"
```

---

### Task 10: LSP server config — same resolution, single target

**Files:**
- Modify: `src/config.rs` — two new settings on the typedb ladder (grep the init-options key table near the "typedb settings stay global (resolved init > user > first)" comment at ~1509 and mirror an existing global string setting, e.g. whatever key feeds `core_json`/`game_json` overrides at ~2107): `openplanet_version: Option<String>`, `typedb_versions_dir: Option<PathBuf>`.
- Modify: `src/server/mod.rs` — where the server loads its `TypeIndex` (grep `TypeIndex::load\|load_with_header` in server/mod.rs, and the `loaded_type_db` accessor at ~537): resolve through `VersionCatalog` when either setting is present; unknown version → the server logs the catalog error and falls back to the installed index (a server must not refuse to start over a config typo — log line prefixed like the existing typedb init log).
- Test: config-ladder tests mirroring the B066 cluster (`src/config.rs` ~3064 `b066_home` helpers) for the two new keys; a server test asserting fallback-on-unknown (mirror the init-log test pattern near `loaded_type_db` tests).

- [ ] **Step 1: Write failing config tests** (init > user precedence, absent → None).
- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Verify pass + full suite.**
- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/server/mod.rs
git commit -m "feat(server): openplanet_version / typedb_versions_dir settings, single-target (GH #18)"
```

---

### Task 11: Acceptance fixture, justfile, docs

**Files:**
- Create: `tests/gh18_acceptance.rs` + `tests/fixtures/issue-repros/gh18-version-straddle/` (a minimal plugin: one file calling `Text::Join({"a","b"}, ",")` — mirror the layout of an existing `tests/fixtures/issue-repros/` entry, including info.toml).
- Modify: `justfile` — recipe `check-versions` running the built binary with `--typedb-versions-dir tests/fixtures/typedb-versions --openplanet-version 1.29.5,1.29.14` against the showcase fixture (mirror the `pretty-check` recipe at ~76).
- Modify: `README.md` (flag documentation, next to `--typedb-dir`), `CHANGELOG.md` (Unreleased: one line for the two flags + per-version labels).

**Interfaces:** consumes everything above; produces the spec's acceptance section.

- [ ] **Step 1: Write the failing acceptance test** — `run_check` on the fixture with versions root pointing at the REAL recorded snapshots:
  - target `1.29.14` → zero diagnostics for the `Text::Join` call;
  - target `1.29.5` → NoMatchingSymbol ERROR whose message contains `added in Openplanet 1.29.14`;
  - targets `1.29.5,1.29.14` → that diagnostic labeled `["1.29.5"]`;
  - unknown target `9.9.9` → `Err` containing `1.29.5` and `1.29.14`.
- [ ] **Step 2: Run to verify failure** (fixture doesn't exist yet → write fixture, re-run; test must fail only on unimplemented behavior, not missing files).
- [ ] **Step 3: Make it pass** (should already pass from Tasks 1-9; fix whatever doesn't).
- [ ] **Step 4: Full suite** `cargo test --locked -j 2 -- --test-threads=2` — everything green, B00x included. Run `just typedb-guard` to prove the primary fixture is untouched.
- [ ] **Step 5: Commit**

```bash
git add tests/gh18_acceptance.rs tests/fixtures/issue-repros/gh18-version-straddle justfile README.md CHANGELOG.md
git commit -m "feat: GH #18 acceptance — multi-version check straddling 1.29.5/1.29.14"
```

---

## Self-review notes (done at write time)

- **Spec coverage:** catalog (T1-4), resolution semantics incl. installed-preference and hard error (T3), multi-target + merge + any-target-fails (T6, exit code follows existing error handling on merged diagnostics), enrichment as data-not-logic (T8), curated renames (T9), CLI surface (T5, T7), server keys (T10), acceptance + B00x guard + justfile + docs (T11). Net::SecureSocket enrichment is covered by T4's notes (type names are in the symbol sets) and asserted in T4's real-fixture test; a dedicated unknown-type message hook is NOT planned — if the unknown-type diagnostic path lacks a note hook after T8, the note simply doesn't render for types yet; that gap, if hit, becomes a bl ticket rather than scope creep here.
- **Deviation from spec §4:** the `RemovedApiUsage { replacement }` quickfix route for `string::Join` is replaced by the existing NoMatchingSymbol relocation-hint route (same machinery as `Draw::*`); T9 Step 4 verifies whether a rename action already falls out of the did_you_mean family and files a follow-up if not. Rationale: `string::Join` sites produce NoMatchingSymbol (free function), not the member-path RemovedApiUsage, and reusing the Draw:: precedent is less new surface.
- **Type consistency check:** `CatalogEntry.label: String` everywhere labels appear (`CliDiagnostic.versions: Vec<String>`, `CheckReport.targets: Vec<String>`); `VersionNotes::get(&str) -> Option<&str>`; `resolve_targets(Option<&str>) -> Result<Vec<&CatalogEntry>, String>` — consistent across T3/T6/T8.
