use std::path::{Path, PathBuf};

use super::manifest::Manifest;

#[derive(Debug)]
pub struct ResolvedDependency {
    pub id: String,
    pub root: PathBuf,
    pub manifest: Manifest,
    /// `(path, source)` pairs. For directory plugins the path is a real file
    /// and the source was read from disk eagerly. For `.op` archives the path
    /// is a pseudo-path `Archive.op::<entry>` (never read from disk) and the
    /// source was read from the ZIP in-memory.
    pub export_files: Vec<(PathBuf, String)>,
    /// Manifest-declared exports that could not be included because they were
    /// missing or unreadable. A consumer cannot treat this dependency's
    /// namespace surface as authoritative while any entry is present.
    pub unloaded_exports: Vec<String>,
}

/// Resolve a dependency by ID from the plugins directory.
/// Supports both directory plugins (with info.toml) and .op archives (ZIP).
pub fn resolve_dependency(
    dep_id: &str,
    plugins_dir: &Path,
    plugin_files_search_paths: &[PathBuf],
) -> Option<ResolvedDependency> {
    // Try directory first
    let dir_path = plugins_dir.join(dep_id);
    if dir_path.is_dir() {
        return resolve_directory_plugin(dep_id, &dir_path, plugin_files_search_paths);
    }

    // Try .op archive
    let op_path = plugins_dir.join(format!("{}.op", dep_id));
    if op_path.exists() {
        return resolve_op_archive(dep_id, &op_path);
    }

    resolve_by_manifest_module(dep_id, plugins_dir, plugin_files_search_paths)
        .or_else(|| resolve_by_normalized_name(dep_id, plugins_dir, plugin_files_search_paths))
}

/// Openplanet's canonical plugin id is the plugin's display `name` with all
/// non-alphanumeric characters stripped ("Better Room Manager" →
/// `BetterRoomManager`, "MLFeed: Race Data" → `MLFeedRaceData`). This is also
/// the basis for the exported `.op` filename. A dependency declared by that id
/// must resolve to the dir plugin whose `meta.name` normalizes to it, even when
/// the folder name and `script.module` differ (GH #33).
fn normalize_plugin_id(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

fn resolve_by_normalized_name(
    id: &str,
    plugins_dir: &Path,
    plugin_files_search_paths: &[PathBuf],
) -> Option<ResolvedDependency> {
    let entries = std::fs::read_dir(plugins_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("info.toml");
        if !manifest_path.exists() {
            continue;
        }
        let Ok(manifest) = Manifest::load(&manifest_path) else {
            continue;
        };
        let Some(name) = manifest.meta.name.as_deref() else {
            continue;
        };
        if normalize_plugin_id(name) != id {
            continue;
        }
        let (export_files, unloaded_exports) =
            collect_export_files(&path, &manifest, plugin_files_search_paths);
        return Some(ResolvedDependency {
            id: id.to_string(),
            root: path,
            manifest,
            export_files,
            unloaded_exports,
        });
    }
    None
}

fn resolve_by_manifest_module(
    id: &str,
    plugins_dir: &Path,
    plugin_files_search_paths: &[PathBuf],
) -> Option<ResolvedDependency> {
    let entries = std::fs::read_dir(plugins_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("info.toml");
        if !manifest_path.exists() {
            continue;
        }
        let manifest = Manifest::load(&manifest_path).ok()?;
        let module = manifest.script.as_ref().and_then(|s| s.module.as_deref());
        if module != Some(id) {
            continue;
        }

        let (export_files, unloaded_exports) =
            collect_export_files(&path, &manifest, plugin_files_search_paths);
        return Some(ResolvedDependency {
            id: id.to_string(),
            root: path,
            manifest,
            export_files,
            unloaded_exports,
        });
    }

    None
}

fn resolve_directory_plugin(
    id: &str,
    root: &Path,
    plugin_files_search_paths: &[PathBuf],
) -> Option<ResolvedDependency> {
    let manifest_path = root.join("info.toml");
    let manifest = Manifest::load(&manifest_path).ok()?;
    let (export_files, unloaded_exports) =
        collect_export_files(root, &manifest, plugin_files_search_paths);
    Some(ResolvedDependency {
        id: id.to_string(),
        root: root.to_path_buf(),
        manifest,
        export_files,
        unloaded_exports,
    })
}

fn resolve_op_archive(id: &str, archive_path: &Path) -> Option<ResolvedDependency> {
    let file = std::fs::File::open(archive_path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;

    // Find and read info.toml from the archive
    let toml_contents = {
        let mut entry = archive.by_name("info.toml").ok()?;
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut entry, &mut buf).ok()?;
        buf
    };

    let manifest = Manifest::parse(&toml_contents).ok()?;

    // GH #20: read export entries straight from the ZIP in-memory. No extract
    // to disk; the path stored is a pseudo-path so the workspace dedup and
    // provenance still work without touching the filesystem.
    let (export_files, unloaded_exports) =
        collect_op_export_files(archive_path, &mut archive, &manifest);

    Some(ResolvedDependency {
        id: id.to_string(),
        root: archive_path.to_path_buf(),
        manifest,
        export_files,
        unloaded_exports,
    })
}

fn collect_op_export_files(
    archive_path: &Path,
    archive: &mut zip::ZipArchive<std::fs::File>,
    manifest: &Manifest,
) -> (Vec<(PathBuf, String)>, Vec<String>) {
    let mut files = Vec::new();
    let mut unloaded = Vec::new();
    let Some(script) = &manifest.script else {
        return (files, unloaded);
    };
    // Dedup names: a file listed in both exports and shared_exports would
    // otherwise be read+decoded twice from the ZIP (seen_files drops the
    // second push, but the IO already happened).
    let mut seen_names = std::collections::HashSet::new();
    let names: Vec<&str> = script
        .exports
        .iter()
        .chain(script.shared_exports.iter())
        .map(|s| s.as_str())
        .filter(|n| seen_names.insert(*n))
        .collect();
    for export in names {
        // Warn (don't silently drop) on missing/unreadable entries — otherwise
        // a corrupt archive reproduces the exact "unknown type" false-positive
        // class GH #20 set out to fix, with no signal. Parity with dir path.
        let Ok(mut entry) = archive.by_name(export) else {
            tracing::warn!(
                "op export `{}` not found in {}",
                export,
                archive_path.display()
            );
            unloaded.push(export.to_string());
            continue;
        };
        let mut buf = String::new();
        if let Err(e) = std::io::Read::read_to_string(&mut entry, &mut buf) {
            tracing::warn!(
                "op export `{}` unreadable in {}: {e}",
                export,
                archive_path.display()
            );
            unloaded.push(export.to_string());
            continue;
        }
        // Pseudo-path: `<archive>.op::<entry>` — unambiguous, non-FS, unique
        // per archive so the seen-files dedup can't collide across deps.
        let pseudo = PathBuf::from(format!("{}::{}", archive_path.display(), export));
        files.push((pseudo, buf));
    }
    (files, unloaded)
}

fn collect_export_files(
    root: &Path,
    manifest: &Manifest,
    plugin_files_search_paths: &[PathBuf],
) -> (Vec<(PathBuf, String)>, Vec<String>) {
    let mut files = Vec::new();
    let mut unloaded = Vec::new();
    if let Some(script) = &manifest.script {
        for export in script.exports.iter().chain(script.shared_exports.iter()) {
            if let Some(path) = resolve_plugin_file(root, export, plugin_files_search_paths) {
                // A listed export that exists but can't be read is unusual.
                // The old push_export_sources path hard-errored the whole
                // load; warn instead so one bad dep doesn't sink the workspace.
                match std::fs::read_to_string(&path) {
                    Ok(source) => files.push((path, source)),
                    Err(e) => {
                        tracing::warn!("failed to read dep export {}: {e}", path.display());
                        unloaded.push(export.clone());
                    }
                }
            } else {
                unloaded.push(export.clone());
            }
        }
    }
    (files, unloaded)
}

pub(crate) fn resolve_plugin_file(
    root: &Path,
    file: &str,
    plugin_files_search_paths: &[PathBuf],
) -> Option<PathBuf> {
    let direct = root.join(file);
    if direct.exists() {
        return Some(direct);
    }

    for search_root in plugin_files_search_paths {
        let candidate = root.join(search_root).join(file);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // B143, the reverse move. Max's plugin dev norm shuffles sources between
    // `<root>/src/` and `<root>/`, and an `info.toml` entry written for the
    // old layout goes stale in whichever direction it was written. The loop
    // above rescues a bare entry whose file sits under a search root; this one
    // rescues the mirror case — an entry that still carries the `src/` prefix
    // after the file moved up to the root. Only a CONFIGURED search root is
    // ever stripped, and only its leading component: any deeper path the entry
    // carries is preserved (`src/Ex/Shared.as` -> `Ex/Shared.as`). Runs LAST,
    // so every layout that already resolved keeps resolving to the same file.
    for search_root in plugin_files_search_paths {
        let Ok(rest) = Path::new(file).strip_prefix(search_root) else {
            continue;
        };
        // An entry equal to the search root strips to nothing; joining that
        // would offer the plugin root directory itself as an export file.
        if rest.as_os_str().is_empty() {
            continue;
        }
        let candidate = root.join(rest);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Write a directory plugin with the given info.toml body under
    /// `plugins_dir/<folder>` and return its root.
    fn write_dir_plugin(plugins_dir: &Path, folder: &str, info_toml: &str) -> PathBuf {
        let root = plugins_dir.join(folder);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("info.toml"), info_toml).unwrap();
        root
    }

    fn temp_plugins(tag: &str) -> crate::testing::TempDir {
        let base = crate::testing::TempDir::new(&format!("opl-deps-{}", tag));
        base
    }

    /// GH #33: a dependency id that matches the provider's *display name*
    /// (normalized: non-alphanumeric stripped) must resolve even though the
    /// provider's folder and `module` differ. Openplanet's canonical plugin id
    /// is the normalized display name (which is also what the exported `.op`
    /// filename is derived from).
    #[test]
    fn resolves_dep_id_by_normalized_display_name() {
        let plugins = temp_plugins("dispname");
        write_dir_plugin(
            &plugins,
            "tm-better-room-manager",
            "[meta]\nname = \"Better Room Manager\"\n[script]\nmodule = \"BRM\"\nexports = [\"BRM_Exports/Exports.as\"]\n",
        );
        let resolved = resolve_dependency("BetterRoomManager", &plugins, &[]);
        assert!(
            resolved.is_some(),
            "dep id `BetterRoomManager` should resolve to the plugin named \"Better Room Manager\" (module BRM)"
        );
    }

    /// Control: the module-name path still works (no regression), and a
    /// totally-unknown id still fails.
    #[test]
    fn still_resolves_by_module_and_rejects_unknown() {
        let plugins = temp_plugins("module");
        write_dir_plugin(
            &plugins,
            "tm-mlfeed-race-data",
            "[meta]\nname = \"MLFeed: Race Data\"\n[script]\nmodule = \"MLFeed\"\n",
        );
        assert!(
            resolve_dependency("MLFeed", &plugins, &[]).is_some(),
            "module-name resolution must keep working"
        );
        assert!(
            resolve_dependency("NoSuchPlugin", &plugins, &[]).is_none(),
            "unknown id must not resolve"
        );
    }

    // ---------------------------------------------------------------
    // B143: export-file resolution across Max's `src/` <-> root moves.
    //
    // Max's plugin dev norm moves sources between `<root>/src/` and
    // `<root>/`. `info.toml` export entries are written one way or the
    // other and go stale on the move, so resolution must survive BOTH
    // directions while keeping any deeper path the entry carries.
    // ---------------------------------------------------------------

    /// Lay out a plugin root containing exactly `files` (root-relative,
    /// parents created) and return the root.
    fn plugin_with_files(tag: &str, files: &[&str]) -> (crate::testing::TempDir, PathBuf) {
        let base = crate::testing::TempDir::new(&format!("opl-resolve-{tag}"));
        let root = base.path().join("plugin");
        for rel in files {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "// content\n").unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        (base, root)
    }

    fn search(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    /// Resolve `entry` against a root holding only `on_disk`, and return the
    /// resolved path RELATIVE to the root (so expectations read as layout,
    /// not as absolute temp paths).
    fn resolved_rel(tag: &str, entry: &str, on_disk: &str, paths: &[&str]) -> Option<String> {
        let (_guard, root) = plugin_with_files(tag, &[on_disk]);
        let got = resolve_plugin_file(&root, entry, &search(paths))?;
        let canon_root = root.canonicalize().unwrap();
        let canon_got = got.canonicalize().unwrap();
        Some(
            canon_got
                .strip_prefix(&canon_root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/"),
        )
    }

    /// The full entry x layout matrix, with the default `["src"]` search
    /// path. Rows 3 and 5 are the B143 bug: an entry that still carries its
    /// `src/` prefix after the file moved up to the root.
    #[test]
    fn resolves_export_entries_across_src_and_root_layouts() {
        let cases: &[(&str, &str, &str, &str)] = &[
            // (case name, info.toml entry, file on disk, expected resolution)
            (
                "no prefix, file at root",
                "Export.as",
                "Export.as",
                "Export.as",
            ),
            (
                "no prefix, file under src",
                "Export.as",
                "src/Export.as",
                "src/Export.as",
            ),
            (
                "src/ prefix, file under src",
                "src/Export.as",
                "src/Export.as",
                "src/Export.as",
            ),
            (
                "src/ prefix, file MOVED to root",
                "src/Export.as",
                "Export.as",
                "Export.as",
            ),
            (
                "nested no prefix, file under src",
                "Ex/Shared.as",
                "src/Ex/Shared.as",
                "src/Ex/Shared.as",
            ),
            (
                "nested src/ prefix, file MOVED to root",
                "src/Ex/Shared.as",
                "Ex/Shared.as",
                "Ex/Shared.as",
            ),
        ];
        for (i, (name, entry, on_disk, expected)) in cases.iter().enumerate() {
            let got = resolved_rel(&format!("matrix{i}"), entry, on_disk, &["src"]);
            assert_eq!(
                got.as_deref(),
                Some(*expected),
                "{name}: entry `{entry}` with `{on_disk}` on disk"
            );
        }
    }

    /// The same matrix for a custom search root: nothing about the rule is
    /// special to the literal name `src`.
    #[test]
    fn custom_search_root_works_in_both_directions() {
        assert_eq!(
            resolved_rel("custom-fwd", "Exports.as", "Source/Exports.as", &["Source"]).as_deref(),
            Some("Source/Exports.as"),
            "a bare entry must find the file under a custom search root"
        );
        assert_eq!(
            resolved_rel("custom-rev", "Source/Exports.as", "Exports.as", &["Source"]).as_deref(),
            Some("Exports.as"),
            "a `Source/`-prefixed entry must survive the file moving to the root"
        );
        // And the default `src` list does NOT rescue a `Source/` prefix.
        assert_eq!(
            resolved_rel("custom-neg", "Source/Exports.as", "Exports.as", &["src"]),
            None,
            "only a CONFIGURED search root may be stripped from an entry"
        );
    }

    /// Several search roots are tried in the order given, in both
    /// directions, and the first hit wins.
    #[test]
    fn multiple_search_roots_are_tried_in_order() {
        // Forward: the file lives under the SECOND root.
        let (_g1, root) = plugin_with_files("multi-fwd", &["second/Export.as"]);
        assert_eq!(
            resolve_plugin_file(&root, "Export.as", &search(&["first", "second"])),
            Some(root.join("second/Export.as")),
            "a later search root must still be tried"
        );

        // Forward order: with the file under BOTH, the first root wins.
        let (_g2, root) =
            plugin_with_files("multi-order", &["first/Export.as", "second/Export.as"]);
        assert_eq!(
            resolve_plugin_file(&root, "Export.as", &search(&["first", "second"])),
            Some(root.join("first/Export.as")),
            "search roots must be honored in the order given"
        );

        // Reverse: only the matching prefix is stripped.
        let (_g3, root) = plugin_with_files("multi-rev", &["Export.as"]);
        assert_eq!(
            resolve_plugin_file(&root, "second/Export.as", &search(&["first", "second"])),
            Some(root.join("Export.as")),
            "the prefix to strip may be any configured root, not just the first"
        );
    }

    /// A direct hit always wins over the search roots, in either direction:
    /// resolution never prefers a same-named file elsewhere in the tree.
    #[test]
    fn a_direct_hit_beats_every_search_root() {
        let (_g, root) = plugin_with_files("direct-wins", &["Export.as", "src/Export.as"]);
        assert_eq!(
            resolve_plugin_file(&root, "Export.as", &search(&["src"])),
            Some(root.join("Export.as")),
            "a bare entry must take the root file, not the src/ one"
        );
        assert_eq!(
            resolve_plugin_file(&root, "src/Export.as", &search(&["src"])),
            Some(root.join("src/Export.as")),
            "a src/-prefixed entry must take the src/ file, not the root one"
        );
    }

    /// Negative control: a search root that matches nothing on disk must not
    /// perturb a resolution that already worked — in either direction. The
    /// control's own premise is asserted (the extra root really is absent).
    #[test]
    fn an_unmatched_search_root_changes_nothing() {
        let (_g, root) = plugin_with_files("neg-control", &["src/Export.as"]);
        assert!(
            !root.join("nonexistent").exists(),
            "control premise: the extra search root must not exist on disk"
        );
        let without = resolve_plugin_file(&root, "Export.as", &search(&["src"]));
        let with = resolve_plugin_file(&root, "Export.as", &search(&["nonexistent", "src"]));
        assert_eq!(without, Some(root.join("src/Export.as")));
        assert_eq!(
            with, without,
            "a dead search root must not change resolution"
        );

        let (_g2, root2) = plugin_with_files("neg-control-rev", &["Export.as"]);
        let rev_without = resolve_plugin_file(&root2, "src/Export.as", &search(&["src"]));
        let rev_with =
            resolve_plugin_file(&root2, "src/Export.as", &search(&["nonexistent", "src"]));
        assert_eq!(rev_without, Some(root2.join("Export.as")));
        assert_eq!(rev_with, rev_without);
    }

    /// A file present in NEITHER place still resolves to nothing, and an
    /// entry that IS a search root (no remainder) must not resolve to the
    /// root directory itself.
    #[test]
    fn nothing_resolves_when_the_file_is_absent_anywhere() {
        let (_g, root) = plugin_with_files("absent", &["Other.as"]);
        assert_eq!(
            resolve_plugin_file(&root, "Export.as", &search(&["src"])),
            None
        );
        assert_eq!(
            resolve_plugin_file(&root, "src/Export.as", &search(&["src"])),
            None
        );
        // An entry that IS the search root strips to an empty remainder.
        // The root directory exists, so a naive strip-and-join would hand
        // back the plugin root as if it were an export file.
        assert!(root.exists(), "control premise: the plugin root exists");
        assert!(
            !root.join("src").exists(),
            "control premise: no `src` dir, so only the strip rule could fire"
        );
        assert_eq!(resolve_plugin_file(&root, "src", &search(&["src"])), None);
    }

    /// With no search roots at all, only the direct join is tried — the
    /// stripping rule is opt-in via configuration, never implicit.
    #[test]
    fn no_search_roots_means_no_fallback_in_either_direction() {
        let (_g, root) = plugin_with_files("empty-search", &["Export.as"]);
        assert_eq!(resolve_plugin_file(&root, "src/Export.as", &[]), None);
        assert_eq!(
            resolve_plugin_file(&root, "Export.as", &[]),
            Some(root.join("Export.as"))
        );
    }
}
