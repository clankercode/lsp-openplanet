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

        let export_files = collect_export_files(&path, &manifest, plugin_files_search_paths);
        return Some(ResolvedDependency {
            id: id.to_string(),
            root: path,
            manifest,
            export_files,
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
    let export_files = collect_export_files(root, &manifest, plugin_files_search_paths);
    Some(ResolvedDependency {
        id: id.to_string(),
        root: root.to_path_buf(),
        manifest,
        export_files,
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
    let export_files = collect_op_export_files(archive_path, &mut archive, &manifest);

    Some(ResolvedDependency {
        id: id.to_string(),
        root: archive_path.to_path_buf(),
        manifest,
        export_files,
    })
}

fn collect_op_export_files(
    archive_path: &Path,
    archive: &mut zip::ZipArchive<std::fs::File>,
    manifest: &Manifest,
) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    let Some(script) = &manifest.script else {
        return files;
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
            continue;
        };
        let mut buf = String::new();
        if let Err(e) = std::io::Read::read_to_string(&mut entry, &mut buf) {
            tracing::warn!(
                "op export `{}` unreadable in {}: {e}",
                export,
                archive_path.display()
            );
            continue;
        }
        // Pseudo-path: `<archive>.op::<entry>` — unambiguous, non-FS, unique
        // per archive so the seen-files dedup can't collide across deps.
        let pseudo = PathBuf::from(format!("{}::{}", archive_path.display(), export));
        files.push((pseudo, buf));
    }
    files
}

fn collect_export_files(
    root: &Path,
    manifest: &Manifest,
    plugin_files_search_paths: &[PathBuf],
) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
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
                    }
                }
            }
        }
    }
    files
}

fn resolve_plugin_file(
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

    None
}
