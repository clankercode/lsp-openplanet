//! GH #18: the version catalog — which Openplanet typedbs are available
//! (installed install + recorded snapshots), and which one(s) a check
//! targets. Version LOGIC lives here; the checker only renders strings
//! this module hands it.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::typedb::index::TypeDbMeta;

/// A parsed Openplanet version — the ordering key of every catalog entry.
/// Derived `Ord` on field order gives numeric ordering (`1.29.5 < 1.29.14`,
/// which lexical comparison inverts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl OpVersion {
    /// Parse `"1.29.14"` or a full Core stamp `"1.29.14 (next, Public, …)"`.
    /// Exactly three numeric dot-separated components; anything else is
    /// `None`.
    pub fn parse(stamp: &str) -> Option<Self> {
        Self::parse_meta(stamp).map(|(v, _)| v)
    }

    /// [`Self::parse`] plus the parenthesized build metadata
    /// (`"next, Public, 6fd4200f"`), when the stamp carries it.
    pub fn parse_meta(stamp: &str) -> Option<(Self, Option<String>)> {
        let stamp = stamp.trim();
        let (head, meta) = match stamp.split_once(" (") {
            Some((h, rest)) => (h, Some(rest.strip_suffix(')')?.to_string())),
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

/// Where a catalog entry's database files came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntrySource {
    /// The live install the existing discovery / `--typedb-dir` resolved.
    Installed,
    /// A recorded snapshot under the versions root
    /// (`tests/fixtures/typedb-versions/<version>/` in-repo).
    Recorded,
}

/// One loadable typedb: a version, its stamps, and the file pair
/// [`crate::typedb::TypeIndex::load`] takes.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub version: OpVersion,
    /// `"1.29.14"` — what `--openplanet-version`, annotations, and labels
    /// use.
    pub label: String,
    /// The Core dump's full `op` stamp, e.g.
    /// `"1.29.14 (next, Public, 6fd4200f)"`.
    pub full_stamp: String,
    pub core_json: PathBuf,
    pub game_json: PathBuf,
    pub source: EntrySource,
}

/// Every Openplanet version this machine can check against, sorted by
/// version. Built once per run; entries are the installed dump (when
/// discovery found one) plus each recorded snapshot directory.
#[derive(Debug, Clone, Default)]
pub struct VersionCatalog {
    entries: Vec<CatalogEntry>,
}

impl VersionCatalog {
    /// `installed`: the `(core_json, game_json)` pair the existing
    /// discovery / `--typedb-dir` resolved, if any. `versions_root`:
    /// `--typedb-versions-dir`, if given. Either source failing to parse is
    /// an error naming the file — a truncated harvest must never look like
    /// an absent one.
    pub fn build(
        installed: Option<(&Path, &Path)>,
        versions_root: Option<&Path>,
    ) -> Result<Self, String> {
        let mut entries = Vec::new();
        if let Some((core, game)) = installed {
            entries.push(Self::entry_from_pair(core, game, EntrySource::Installed, None)?);
        }
        if let Some(root) = versions_root {
            let dir = std::fs::read_dir(root)
                .map_err(|e| format!("failed to read versions dir {}: {e}", root.display()))?;
            for item in dir {
                let item =
                    item.map_err(|e| format!("failed to read versions dir {}: {e}", root.display()))?;
                let path = item.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                // Only version-named directories are snapshots; READMEs and
                // stray files are not. But a version-named dir that fails to
                // load IS an error — silently skipping a broken snapshot
                // would report against fewer versions than the user asked
                // this root to provide.
                let Some(dir_version) = OpVersion::parse(name) else {
                    continue;
                };
                if !path.is_dir() {
                    continue;
                }
                let entry = Self::entry_from_pair(
                    &path.join("OpenplanetCore.json"),
                    &path.join("OpenplanetNext.json"),
                    EntrySource::Recorded,
                    Some(dir_version),
                )?;
                entries.push(entry);
            }
        }
        entries.sort_by_key(|e| e.version);
        Ok(Self { entries })
    }

    /// Probe one Core/game pair into an entry. `expect`: the version the
    /// snapshot's directory name promises — a stamp that disagrees is an
    /// error naming both, so a mislabeled snapshot cannot silently answer
    /// for the wrong version.
    fn entry_from_pair(
        core_json: &Path,
        game_json: &Path,
        source: EntrySource,
        expect: Option<OpVersion>,
    ) -> Result<CatalogEntry, String> {
        let core = TypeDbMeta::probe(core_json)?;
        // Parse errors in the game dump surface now, not at load time.
        let _game = TypeDbMeta::probe(game_json)?;
        let stamp = core.op.ok_or_else(|| {
            format!("{}: no `op` version stamp", core_json.display())
        })?;
        let version = OpVersion::parse(&stamp).ok_or_else(|| {
            format!("{}: unparseable `op` stamp {stamp:?}", core_json.display())
        })?;
        if let Some(expected) = expect {
            if version != expected {
                return Err(format!(
                    "{}: directory names version {expected} but the dump inside is stamped {version} ({stamp})",
                    core_json.display()
                ));
            }
        }
        Ok(CatalogEntry {
            version,
            label: version.to_string(),
            full_stamp: stamp,
            core_json: core_json.to_path_buf(),
            game_json: game_json.to_path_buf(),
            source,
        })
    }

    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    /// Resolve a `--openplanet-version` request to catalog entries.
    ///
    /// `None` → the installed entry alone, or no targets when nothing is
    /// installed (the caller keeps its existing no-typedb path). A
    /// comma-separated request resolves each element, deduplicated by
    /// version with input order preserved. An element matching both the
    /// installed dump and a recorded snapshot resolves to the installed
    /// dump — what is on disk is always usable for its own version. An
    /// element matching nothing is a hard error naming every known
    /// version (Max, 2026-08-31: never silently substitute).
    pub fn resolve_targets(&self, requested: Option<&str>) -> Result<Vec<&CatalogEntry>, String> {
        let Some(requested) = requested else {
            return Ok(self
                .entries
                .iter()
                .filter(|e| matches!(e.source, EntrySource::Installed))
                .collect());
        };
        let mut targets: Vec<&CatalogEntry> = Vec::new();
        for element in requested.split(',') {
            let element = element.trim();
            let version = OpVersion::parse(element).ok_or_else(|| {
                format!("invalid Openplanet version {element:?} (expected e.g. 1.29.14)")
            })?;
            let matched = self
                .entries
                .iter()
                .filter(|e| e.version == version)
                .max_by_key(|e| matches!(e.source, EntrySource::Installed));
            let Some(entry) = matched else {
                return Err(format!(
                    "unknown Openplanet version {version}; known versions: {}",
                    if self.entries.is_empty() {
                        "none (no installed typedb found and no --typedb-versions-dir given)"
                            .to_string()
                    } else {
                        self.known_versions().join(", ")
                    }
                ));
            };
            if !targets.iter().any(|t| t.version == entry.version) {
                targets.push(entry);
            }
        }
        Ok(targets)
    }

    /// Sorted labels for error messages, the installed entry marked.
    pub fn known_versions(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|e| match e.source {
                EntrySource::Installed => format!("{} (installed)", e.label),
                EntrySource::Recorded => e.label.clone(),
            })
            .collect()
    }
}

/// Version notes for symbols a target lacks: qualified name →
/// `"added in Openplanet X"` / `"removed in Openplanet Y"`. Pure data —
/// the checker renders these strings verbatim and performs no version
/// comparison of its own (spec §4).
#[derive(Debug, Clone, Default)]
pub struct VersionNotes {
    notes: std::collections::HashMap<String, String>,
}

impl VersionNotes {
    pub fn get(&self, qualified: &str) -> Option<&str> {
        self.notes.get(qualified).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }
}

/// Qualified names (functions, classes, enums) present in one Core dump.
/// A light scan — names only, no [`crate::typedb::TypeIndex`] build.
fn symbol_set(core_json: &Path) -> Result<std::collections::HashSet<String>, String> {
    let contents = std::fs::read_to_string(core_json)
        .map_err(|e| format!("failed to read {}: {e}", core_json.display()))?;
    let value: serde_json::Value = serde_json::from_str(&contents)
        .map_err(|e| format!("failed to parse {}: {e}", core_json.display()))?;
    let mut set = std::collections::HashSet::new();
    for section in ["functions", "classes", "enums"] {
        let Some(items) = value.get(section).and_then(|v| v.as_array()) else {
            continue;
        };
        for item in items {
            let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let qualified = match item.get("ns").and_then(|v| v.as_str()) {
                Some(ns) if !ns.is_empty() => format!("{ns}::{name}"),
                _ => name.to_string(),
            };
            set.insert(qualified);
        }
    }
    Ok(set)
}

impl VersionCatalog {
    /// Notes for every symbol ABSENT from `target` but present in some
    /// other catalog version. Rules over the sparse recorded set (T =
    /// target version, C = versions containing the symbol):
    ///
    /// - `min(C) > T` → `added in Openplanet {min(C)}`
    /// - `max(C) < T` → `removed in Openplanet {V}`, V = the smallest
    ///   catalog version above `max(C)` (every version above `max(C)`
    ///   lacks the symbol by construction, so the smallest is the
    ///   earliest recorded sighting of the removal)
    /// - presence straddles T → no note; with sparse data there is no
    ///   honest single claim, and silence beats a wrong one
    pub fn notes_for_target(&self, target: &CatalogEntry) -> Result<VersionNotes, String> {
        use std::collections::{BTreeMap, HashSet};
        // Union sets per version: an installed dump and a recorded
        // snapshot of the same version answer as one.
        let mut per_version: BTreeMap<OpVersion, HashSet<String>> = BTreeMap::new();
        for entry in &self.entries {
            let set = symbol_set(&entry.core_json)?;
            per_version.entry(entry.version).or_default().extend(set);
        }
        let Some(target_set) = per_version.get(&target.version).cloned() else {
            return Ok(VersionNotes::default());
        };
        let mut notes = std::collections::HashMap::new();
        let versions: Vec<OpVersion> = per_version.keys().copied().collect();
        let mut containing: std::collections::HashMap<&str, Vec<OpVersion>> =
            std::collections::HashMap::new();
        for (&version, set) in &per_version {
            if version == target.version {
                continue;
            }
            for symbol in set {
                if !target_set.contains(symbol) {
                    containing.entry(symbol.as_str()).or_default().push(version);
                }
            }
        }
        for (symbol, mut c) in containing {
            c.sort();
            let (min, max) = (c[0], *c.last().expect("non-empty by construction"));
            if min > target.version {
                notes.insert(symbol.to_string(), format!("added in Openplanet {min}"));
            } else if max < target.version {
                let removed_at = versions
                    .iter()
                    .copied()
                    .find(|&v| v > max)
                    .expect("target.version > max is such a version");
                notes.insert(
                    symbol.to_string(),
                    format!("removed in Openplanet {removed_at}"),
                );
            }
        }
        Ok(VersionNotes { notes })
    }
}

/// GH #18, server path: the single typedb pair an LSP server should load.
///
/// The server checks ONE version (multi-target is a `check` feature), so a
/// comma list resolves to its first element with a warning naming the
/// choice. Any catalog or resolution failure falls back to the installed
/// pair and returns the warning to log — a server must not refuse to
/// start over a config typo. No flags at all → the installed pair,
/// silently (today's behavior).
pub fn resolve_single_pair(
    installed: Option<(&Path, &Path)>,
    versions_root: Option<&Path>,
    requested: Option<&str>,
) -> (Option<(PathBuf, PathBuf)>, Option<String>) {
    let installed_pair =
        || installed.map(|(c, g)| (c.to_path_buf(), g.to_path_buf()));
    if versions_root.is_none() && requested.is_none() {
        return (installed_pair(), None);
    }
    let resolved = VersionCatalog::build(installed, versions_root).and_then(|catalog| {
        catalog.resolve_targets(requested).map(|targets| {
            targets
                .into_iter()
                .map(|e| (e.label.clone(), e.core_json.clone(), e.game_json.clone()))
                .collect::<Vec<_>>()
        })
    });
    match resolved {
        Err(e) => (
            installed_pair(),
            Some(format!(
                "openplanet_version/typedb_versions_dir ignored ({e}); using the installed type database"
            )),
        ),
        Ok(targets) => match targets.as_slice() {
            [] => (installed_pair(), None),
            [(_, core, game)] => (Some((core.clone(), game.clone())), None),
            [(label, core, game), ..] => (
                Some((core.clone(), game.clone())),
                Some(format!(
                    "openplanet_version lists several versions; the server checks one — using {label} (run `openplanet-lsp check --openplanet-version ...` for multi-version checking)"
                )),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_version_parses_bare_and_full_stamps() {
        assert_eq!(
            OpVersion::parse("1.29.14"),
            Some(OpVersion {
                major: 1,
                minor: 29,
                patch: 14
            })
        );
        assert_eq!(
            OpVersion::parse("1.29.5 (next, Public, 1234ad9e)"),
            Some(OpVersion {
                major: 1,
                minor: 29,
                patch: 5
            })
        );
    }

    #[test]
    fn op_version_rejects_malformed() {
        for bad in ["", "1.29", "next", "1.29.x", "1.29.5.2", "1.29.5 next"] {
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

    fn tmp(tag: &str) -> crate::testing::TempDir {
        crate::testing::TempDir::new(&format!("openplanet-lsp-catalog-{tag}"))
    }

    #[test]
    fn catalog_discovers_recorded_snapshots_sorted() {
        let dir = tmp("discover");
        write_snapshot(dir.path(), "1.29.14", "1.29.14 (next, Public, 6fd4200f)", "1.29.14");
        write_snapshot(dir.path(), "1.29.5", "1.29.5 (next, Public, 1234ad9e)", "1.29.5");
        let cat = VersionCatalog::build(None, Some(dir.path())).unwrap();
        let labels: Vec<_> = cat.entries().iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["1.29.5", "1.29.14"], "numeric order, not lexical");
        assert!(cat
            .entries()
            .iter()
            .all(|e| matches!(e.source, EntrySource::Recorded)));
        assert_eq!(
            cat.entries()[1].full_stamp,
            "1.29.14 (next, Public, 6fd4200f)"
        );
    }

    #[test]
    fn catalog_rejects_dirname_stamp_mismatch() {
        let dir = tmp("mismatch");
        write_snapshot(dir.path(), "1.29.5", "1.29.14 (next, Public, 6fd4200f)", "1.29.14");
        let err = VersionCatalog::build(None, Some(dir.path())).unwrap_err();
        assert!(
            err.contains("1.29.5") && err.contains("1.29.14"),
            "names both sides: {err}"
        );
    }

    #[test]
    fn catalog_includes_installed_entry_from_paths() {
        let dir = tmp("installed");
        write_snapshot(dir.path(), "install", "1.29.9 (next, Public, abcd1234)", "1.29.9");
        let d = dir.path().join("install");
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
        let dir = tmp("broken");
        // A README or stray file at the root is ignored…
        std::fs::write(dir.path().join("README.md"), "docs").unwrap();
        write_snapshot(dir.path(), "1.29.5", "1.29.5 (next, Public, 1234ad9e)", "1.29.5");
        // …but a version-named dir with a truncated dump is an error, never
        // silently skipped (a truncated harvest must not look empty —
        // TypeDbMeta::probe's rule).
        let broken = dir.path().join("1.29.6");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(
            broken.join("OpenplanetCore.json"),
            r#"{"op":"1.29.6","functions":[{"na"#,
        )
        .unwrap();
        let err = VersionCatalog::build(None, Some(dir.path())).unwrap_err();
        assert!(err.contains("1.29.6"), "error names the broken snapshot: {err}");
    }

    #[test]
    fn catalog_known_versions_marks_installed() {
        let dir = tmp("known");
        write_snapshot(dir.path(), "1.29.5", "1.29.5 (next, Public, 1234ad9e)", "1.29.5");
        write_snapshot(dir.path(), "install", "1.29.9 (next, Public, abcd1234)", "1.29.9");
        let d = dir.path().join("install");
        let cat = VersionCatalog::build(
            Some((&d.join("OpenplanetCore.json"), &d.join("OpenplanetNext.json"))),
            Some(dir.path()),
        )
        .unwrap();
        assert_eq!(cat.known_versions(), ["1.29.5", "1.29.9 (installed)"]);
    }

    /// Catalog with installed 1.29.9 and recorded 1.29.5 + 1.29.14.
    fn full_catalog(tag: &str) -> (crate::testing::TempDir, VersionCatalog) {
        let dir = tmp(tag);
        write_snapshot(dir.path(), "1.29.5", "1.29.5 (next, Public, 1234ad9e)", "1.29.5");
        write_snapshot(dir.path(), "1.29.14", "1.29.14 (next, Public, 6fd4200f)", "1.29.14");
        write_snapshot(dir.path(), "install", "1.29.9 (next, Public, abcd1234)", "1.29.9");
        let d = dir.path().join("install");
        let cat = VersionCatalog::build(
            Some((&d.join("OpenplanetCore.json"), &d.join("OpenplanetNext.json"))),
            Some(dir.path()),
        )
        .unwrap();
        (dir, cat)
    }

    #[test]
    fn resolve_default_is_installed() {
        let (_g, cat) = full_catalog("resolve-default");
        let targets = cat.resolve_targets(None).unwrap();
        assert_eq!(targets.len(), 1);
        assert!(matches!(targets[0].source, EntrySource::Installed));
        assert_eq!(targets[0].label, "1.29.9");
    }

    #[test]
    fn resolve_default_without_installed_is_empty() {
        let dir = tmp("resolve-noinstall");
        write_snapshot(dir.path(), "1.29.5", "1.29.5 (next, Public, 1234ad9e)", "1.29.5");
        let cat = VersionCatalog::build(None, Some(dir.path())).unwrap();
        assert!(cat.resolve_targets(None).unwrap().is_empty());
    }

    #[test]
    fn resolve_matches_installed_before_recorded() {
        let dir = tmp("resolve-prefer");
        write_snapshot(dir.path(), "1.29.5", "1.29.5 (next, Public, 1234ad9e)", "1.29.5");
        write_snapshot(dir.path(), "install", "1.29.5 (next, Public, 9999ffff)", "1.29.5");
        let d = dir.path().join("install");
        let cat = VersionCatalog::build(
            Some((&d.join("OpenplanetCore.json"), &d.join("OpenplanetNext.json"))),
            Some(dir.path()),
        )
        .unwrap();
        let targets = cat.resolve_targets(Some("1.29.5")).unwrap();
        assert_eq!(targets.len(), 1);
        assert!(
            matches!(targets[0].source, EntrySource::Installed),
            "installed dump wins for its own version"
        );
    }

    #[test]
    fn resolve_comma_list_dedups_preserving_order() {
        let (_g, cat) = full_catalog("resolve-list");
        let targets = cat
            .resolve_targets(Some("1.29.14,1.29.5,1.29.14"))
            .unwrap();
        let labels: Vec<_> = targets.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["1.29.14", "1.29.5"], "input order, first wins");
    }

    #[test]
    fn resolve_unknown_version_errors_naming_known() {
        let (_g, cat) = full_catalog("resolve-unknown");
        let err = cat.resolve_targets(Some("1.29.9999")).unwrap_err();
        for known in ["1.29.5", "1.29.9", "1.29.14"] {
            assert!(err.contains(known), "error must name {known}: {err}");
        }
        assert!(err.contains("1.29.9999"), "error names the request: {err}");
    }

    #[test]
    fn resolve_rejects_malformed_version_string() {
        let (_g, cat) = full_catalog("resolve-malformed");
        let err = cat.resolve_targets(Some("banana")).unwrap_err();
        assert!(err.contains("banana"), "{err}");
    }

    /// Snapshot whose Core dump carries the given function/class/enum
    /// symbol JSON fragments verbatim.
    fn write_snapshot_with(
        root: &std::path::Path,
        dir: &str,
        version: &str,
        functions: &str,
        classes: &str,
        enums: &str,
    ) {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("OpenplanetCore.json"),
            format!(
                r#"{{"op":"{version} (next, Public, 00000000)","functions":[{functions}],"classes":[{classes}],"enums":[{enums}]}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            d.join("OpenplanetNext.json"),
            format!(r#"{{"op":"{version}","mp":"2026-02-03 03:51:19","ns":{{}}}}"#),
        )
        .unwrap();
    }

    fn notes_catalog(tag: &str, snapshots: &[(&str, &str, &str, &str)]) -> (crate::testing::TempDir, VersionCatalog) {
        let dir = tmp(tag);
        for (version, functions, classes, enums) in snapshots {
            write_snapshot_with(dir.path(), version, version, functions, classes, enums);
        }
        let cat = VersionCatalog::build(None, Some(dir.path())).unwrap();
        (dir, cat)
    }

    fn target<'c>(cat: &'c VersionCatalog, label: &str) -> &'c CatalogEntry {
        cat.entries().iter().find(|e| e.label == label).unwrap()
    }

    #[test]
    fn notes_added_in_later_version() {
        let (_g, cat) = notes_catalog(
            "notes-added",
            &[
                ("1.0.0", "", "", ""),
                ("2.0.0", r#"{"ns":"Text","name":"Join"}"#, "", ""),
            ],
        );
        let notes = cat.notes_for_target(target(&cat, "1.0.0")).unwrap();
        assert_eq!(notes.get("Text::Join"), Some("added in Openplanet 2.0.0"));
        // Present on target → no note.
        let notes2 = cat.notes_for_target(target(&cat, "2.0.0")).unwrap();
        assert_eq!(notes2.get("Text::Join"), None);
    }

    #[test]
    fn notes_removed_names_earliest_lacking_version() {
        let (_g, cat) = notes_catalog(
            "notes-removed",
            &[
                ("1.0.0", r#"{"ns":"string","name":"Join"}"#, "", ""),
                ("2.0.0", "", "", ""),
                ("3.0.0", "", "", ""),
            ],
        );
        let notes = cat.notes_for_target(target(&cat, "3.0.0")).unwrap();
        assert_eq!(
            notes.get("string::Join"),
            Some("removed in Openplanet 2.0.0"),
            "earliest lacking version after the last containing one"
        );
    }

    #[test]
    fn notes_cover_classes_enums_and_bare_names() {
        let (_g, cat) = notes_catalog(
            "notes-kinds",
            &[
                (
                    "1.0.0",
                    r#"{"name":"GlobalFn"}"#,
                    r#"{"ns":"Net","name":"SecureSocket"}"#,
                    "",
                ),
                ("2.0.0", "", "", r#"{"ns":"UI","name":"NewCond","values":{}}"#),
            ],
        );
        let old = cat.notes_for_target(target(&cat, "1.0.0")).unwrap();
        assert_eq!(old.get("UI::NewCond"), Some("added in Openplanet 2.0.0"));
        let new = cat.notes_for_target(target(&cat, "2.0.0")).unwrap();
        assert_eq!(new.get("Net::SecureSocket"), Some("removed in Openplanet 2.0.0"));
        assert_eq!(new.get("GlobalFn"), Some("removed in Openplanet 2.0.0"));
    }

    #[test]
    fn notes_silent_when_presence_straddles_target() {
        // Present below AND above the target but not at it: sparse data,
        // no honest single claim — stay silent.
        let (_g, cat) = notes_catalog(
            "notes-straddle",
            &[
                ("1.0.0", r#"{"ns":"Ns","name":"Flaky"}"#, "", ""),
                ("2.0.0", "", "", ""),
                ("3.0.0", r#"{"ns":"Ns","name":"Flaky"}"#, "", ""),
            ],
        );
        let notes = cat.notes_for_target(target(&cat, "2.0.0")).unwrap();
        assert_eq!(notes.get("Ns::Flaky"), None);
    }

    #[test]
    fn real_fixtures_derive_text_join_added_and_string_join_removed() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb-versions");
        let cat = VersionCatalog::build(None, Some(&root)).unwrap();
        let v5 = target(&cat, "1.29.5");
        let v14 = target(&cat, "1.29.14");
        let notes5 = cat.notes_for_target(v5).unwrap();
        assert_eq!(notes5.get("Text::Join"), Some("added in Openplanet 1.29.14"));
        let notes14 = cat.notes_for_target(v14).unwrap();
        assert_eq!(notes14.get("string::Join"), Some("removed in Openplanet 1.29.14"));
        assert_eq!(
            notes14.get("Net::SecureSocket"),
            Some("removed in Openplanet 1.29.14")
        );
        assert_eq!(notes14.get("Text::Join"), None, "present on target → no note");
    }

    #[test]
    fn resolve_single_pair_defaults_to_installed_without_flags() {
        let dir = tmp("pair-default");
        write_snapshot(dir.path(), "install", "1.29.9 (next, Public, abcd1234)", "1.29.9");
        let d = dir.path().join("install");
        let installed = (d.join("OpenplanetCore.json"), d.join("OpenplanetNext.json"));
        let (pair, warning) =
            resolve_single_pair(Some((&installed.0, &installed.1)), None, None);
        assert_eq!(pair, Some(installed));
        assert_eq!(warning, None);
    }

    #[test]
    fn resolve_single_pair_picks_recorded_version() {
        let dir = tmp("pair-recorded");
        write_snapshot(dir.path(), "1.29.5", "1.29.5 (next, Public, 1234ad9e)", "1.29.5");
        let (pair, warning) = resolve_single_pair(None, Some(dir.path()), Some("1.29.5"));
        let (core, _) = pair.expect("recorded snapshot resolves");
        assert!(core.ends_with("1.29.5/OpenplanetCore.json"), "{core:?}");
        assert_eq!(warning, None);
    }

    #[test]
    fn resolve_single_pair_falls_back_to_installed_on_unknown_version() {
        let dir = tmp("pair-unknown");
        write_snapshot(dir.path(), "1.29.5", "1.29.5 (next, Public, 1234ad9e)", "1.29.5");
        write_snapshot(dir.path(), "install", "1.29.9 (next, Public, abcd1234)", "1.29.9");
        let d = dir.path().join("install");
        let installed = (d.join("OpenplanetCore.json"), d.join("OpenplanetNext.json"));
        let (pair, warning) = resolve_single_pair(
            Some((&installed.0, &installed.1)),
            Some(dir.path()),
            Some("9.9.9"),
        );
        assert_eq!(pair, Some(installed), "server must not refuse to start");
        let warning = warning.expect("a config typo is logged, not fatal");
        assert!(
            warning.contains("9.9.9") && warning.contains("1.29.5"),
            "warning names request and known versions: {warning}"
        );
    }

    #[test]
    fn resolve_single_pair_takes_first_of_a_list_with_warning() {
        let dir = tmp("pair-list");
        write_snapshot(dir.path(), "1.29.5", "1.29.5 (next, Public, 1234ad9e)", "1.29.5");
        write_snapshot(dir.path(), "1.29.14", "1.29.14 (next, Public, 6fd4200f)", "1.29.14");
        let (pair, warning) =
            resolve_single_pair(None, Some(dir.path()), Some("1.29.14,1.29.5"));
        let (core, _) = pair.expect("first target resolves");
        assert!(core.ends_with("1.29.14/OpenplanetCore.json"), "{core:?}");
        let warning = warning.expect("multi-version request on the server warns");
        assert!(warning.contains("1.29.14"), "{warning}");
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
