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

    #[test]
    fn op_version_meta_extracts_parenthetical() {
        let (v, meta) = OpVersion::parse_meta("1.29.14 (next, Public, 6fd4200f)").unwrap();
        assert_eq!(v.to_string(), "1.29.14");
        assert_eq!(meta.as_deref(), Some("next, Public, 6fd4200f"));
        let (_, none) = OpVersion::parse_meta("1.29.14").unwrap();
        assert_eq!(none, None);
    }
}
