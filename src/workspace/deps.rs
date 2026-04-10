use std::path::{Path, PathBuf};

use super::manifest::Manifest;

#[derive(Debug)]
pub struct ResolvedDependency {
    pub id: String,
    pub root: PathBuf,
    pub manifest: Manifest,
    pub export_files: Vec<PathBuf>,
}

/// Resolve a dependency by ID from the plugins directory.
/// Supports both directory plugins (with info.toml) and .op archives (ZIP).
pub fn resolve_dependency(
    dep_id: &str,
    plugins_dir: &Path,
) -> Option<ResolvedDependency> {
    // Try directory first
    let dir_path = plugins_dir.join(dep_id);
    if dir_path.is_dir() {
        return resolve_directory_plugin(dep_id, &dir_path);
    }

    // Try .op archive
    let op_path = plugins_dir.join(format!("{}.op", dep_id));
    if op_path.exists() {
        return resolve_op_archive(dep_id, &op_path);
    }

    None
}

fn resolve_directory_plugin(id: &str, root: &Path) -> Option<ResolvedDependency> {
    let manifest_path = root.join("info.toml");
    let manifest = Manifest::load(&manifest_path).ok()?;
    let export_files = collect_export_files(root, &manifest);
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

    // For .op archives, export files would need to be extracted or read on demand.
    // For now, return empty — the caller can extract as needed.
    Some(ResolvedDependency {
        id: id.to_string(),
        root: archive_path.to_path_buf(),
        manifest,
        export_files: Vec::new(),
    })
}

fn collect_export_files(root: &Path, manifest: &Manifest) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(script) = &manifest.script {
        for export in &script.exports {
            let path = root.join(export);
            if path.exists() {
                files.push(path);
            }
        }
        for export in &script.shared_exports {
            let path = root.join(export);
            if path.exists() {
                files.push(path);
            }
        }
    }
    files
}
