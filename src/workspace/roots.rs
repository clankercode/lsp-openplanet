//! Plugin-root discovery from workspace folders (t2 hybrid mechanism).
//!
//! Walks each workspace folder downward looking for directories that contain
//! an `info.toml` plugin manifest. Walk policy (locked in ticket t2):
//!
//! - First-match stop: a dir containing `info.toml` is a plugin root and the
//!   walk does NOT descend into it (a nested `info.toml` under a root is
//!   unreachable — the outer root wins).
//! - Skip-list: hidden dirs, `.git`, `node_modules`.
//! - Hard depth cap 5 relative to each start folder (pathological trees).
//!
//! Pure discovery only — the caller applies the zero-found fallback (single
//! root on the opened folder itself).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Maximum directory depth (relative to each start folder) the walk visits.
/// Depth 0 = the start folder itself.
const MAX_DEPTH: usize = 5;

/// Directories never entered during the walk.
const SKIPPED_DIRS: &[&str] = &[".git", "node_modules"];

/// Is `name` on the discovery skip-list (hidden, `.git`, `node_modules`)?
fn is_skipped_dir(name: &str) -> bool {
    name.starts_with('.') || SKIPPED_DIRS.contains(&name)
}

/// Discover plugin roots under the given workspace folders.
///
/// Per folder: downward walk; a dir containing `info.toml` is a root and is
/// not descended into. Results are canonicalized and deduplicated across all
/// folders. Nonexistent / unreadable folders are skipped gracefully. Returns
/// ONLY the discovered roots — empty when nothing was found (the caller
/// decides the fallback).
pub fn discover_plugin_roots(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut roots: Vec<PathBuf> = Vec::new();
    for folder in folders {
        walk_folder(folder, 0, &mut seen, &mut roots);
    }
    roots
}

/// Walk one directory (at `depth` below its start folder), appending any
/// discovered roots. Errors (unreadable / vanished dirs) are swallowed —
/// discovery is best-effort.
fn walk_folder(dir: &Path, depth: usize, seen: &mut HashSet<PathBuf>, roots: &mut Vec<PathBuf>) {
    // First-match stop at depth 0: the start folder itself may BE a plugin
    // root (single-plugin workspace opened directly).
    if depth == 0 && dir.join("info.toml").is_file() {
        let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        if seen.insert(canonical.clone()) {
            roots.push(canonical);
        }
        return;
    }
    // Mark this dir visited before reading it so symlink cycles terminate
    // (a → b → a: walking b finds link-a whose canonical target is a,
    // already seen).
    let dir_canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if !seen.insert(dir_canonical) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // Entries of a dir at `depth` sit at depth+1; the cap applies to them.
    let entry_depth = depth + 1;
    let mut subdirs: Vec<(PathBuf, PathBuf, bool)> = Vec::new();
    for entry in entries.flatten() {
        // DirEntry::file_type() and DirEntry::metadata() are both
        // lstat-based: a symlink to a directory reports is_dir() == false.
        // Plugin trees like dips++-all are symlink farms (opened folder →
        // plugins living elsewhere), so classify symlinked entries with
        // std::fs::metadata (stat — follows links).
        let is_dir = match entry.file_type() {
            Ok(ft) if ft.is_dir() => true,
            Ok(ft) if ft.is_symlink() => std::fs::metadata(entry.path())
                .map(|m| m.is_dir())
                .unwrap_or(false),
            _ => false,
        };
        if !is_dir {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if is_skipped_dir(name) {
            continue;
        }
        if entry_depth > MAX_DEPTH {
            continue;
        }
        // Loop guard: a symlink cycle (a → b → a) would recurse forever.
        // Skip any dir whose canonical target we already visited/queued.
        // (Canonicalizing every entry costs one stat per dir; discovery
        // runs once per resharding and trees are shallow — acceptable.)
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if seen.contains(&canonical) {
            continue;
        }
        // First-match stop: a dir containing info.toml is a root; do not
        // descend into it (nested info.toml under a root is unreachable).
        let is_root = path.join("info.toml").is_file();
        subdirs.push((path, canonical, is_root));
    }
    // Deterministic order regardless of readdir ordering: collect first, then
    // sort by path, then classify (roots appended in sorted order; non-roots
    // recursed into in sorted order).
    subdirs.sort();
    for (path, canonical, is_root) in subdirs {
        if is_root {
            if seen.insert(canonical.clone()) {
                roots.push(canonical);
            }
        } else {
            walk_folder(&path, entry_depth, seen, roots);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_tree(name: &str) -> crate::testing::TempDir {
        let base = crate::testing::TempDir::new(&format!("openplanet-lsp-ws-roots-{}", name));
        base
    }

    fn write_info_toml(dir: &Path) {
        fs::write(dir.join("info.toml"), "[meta]\n").unwrap();
    }

    fn canonic(p: &Path) -> PathBuf {
        p.canonicalize().unwrap()
    }

    #[test]
    fn info_toml_at_folder_root_is_found() {
        let base = temp_tree("at-root");
        write_info_toml(&base);
        assert_eq!(
            discover_plugin_roots(&[base.to_path_buf()]),
            vec![canonic(&base)]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_plugin_dirs_are_discovered() {
        // dips++-all layout: the opened folder contains ONLY symlinks to
        // plugin dirs elsewhere. DirEntry::file_type() is lstat-based, so
        // symlink-to-dir must be followed explicitly or no roots are found.
        let base = temp_tree("symlinked");
        let real = base.join("elsewhere").join("real-plugin");
        fs::create_dir_all(&real).unwrap();
        write_info_toml(&real);
        let opened = base.join("opened");
        fs::create_dir_all(&opened).unwrap();
        std::os::unix::fs::symlink(&real, opened.join("linked-plugin")).unwrap();

        assert_eq!(
            discover_plugin_roots(std::slice::from_ref(&opened)),
            vec![canonic(&real)]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loops_do_not_hang_discovery() {
        // A symlink cycle (a -> b -> a) must not recurse forever: the
        // canonical-path seen-set dedupes revisited dirs.
        let base = temp_tree("symlink-loop");
        let a = base.join("a");
        let b = base.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        std::os::unix::fs::symlink(&b, a.join("link-b")).unwrap();
        std::os::unix::fs::symlink(&a, b.join("link-a")).unwrap();
        let plugin = a.join("plugin");
        fs::create_dir_all(&plugin).unwrap();
        write_info_toml(&plugin);

        let roots = discover_plugin_roots(&[base.to_path_buf()]);
        assert_eq!(roots, vec![canonic(&plugin)]);
    }

    #[cfg(unix)]
    #[test]
    fn same_plugin_reachable_via_two_links_dedupes() {
        let base = temp_tree("symlink-dup");
        let real = base.join("real");
        fs::create_dir_all(&real).unwrap();
        write_info_toml(&real);
        std::os::unix::fs::symlink(&real, base.join("link-one")).unwrap();
        std::os::unix::fs::symlink(&real, base.join("link-two")).unwrap();

        assert_eq!(
            discover_plugin_roots(&[base.to_path_buf()]),
            vec![canonic(&real)]
        );
    }

    #[test]
    fn one_subdir_plugin_is_found() {
        let base = temp_tree("one-subdir");
        let plugin = base.join("my-plugin");
        fs::create_dir_all(&plugin).unwrap();
        write_info_toml(&plugin);
        assert_eq!(
            discover_plugin_roots(&[base.to_path_buf()]),
            vec![canonic(&plugin)]
        );
    }

    #[test]
    fn two_sibling_plugins_are_found_sorted() {
        let base = temp_tree("two-siblings");
        for name in ["plugin-b", "plugin-a"] {
            let p = base.join(name);
            fs::create_dir_all(&p).unwrap();
            write_info_toml(&p);
        }
        assert_eq!(
            discover_plugin_roots(&[base.to_path_buf()]),
            vec![
                canonic(&base.join("plugin-a")),
                canonic(&base.join("plugin-b"))
            ]
        );
    }

    #[test]
    fn nested_outer_root_wins() {
        let base = temp_tree("nested");
        let outer = base.join("outer");
        let inner = outer.join("inner");
        fs::create_dir_all(&inner).unwrap();
        write_info_toml(&outer);
        write_info_toml(&inner);
        // First-match stop: outer is a root, inner is unreachable.
        assert_eq!(
            discover_plugin_roots(&[base.to_path_buf()]),
            vec![canonic(&outer)]
        );
    }

    #[test]
    fn skipped_dirs_are_not_entered() {
        let base = temp_tree("skipped");
        for name in [".git", "node_modules", ".hidden"] {
            let p = base.join(name).join("plugin");
            fs::create_dir_all(&p).unwrap();
            write_info_toml(&p);
        }
        // A plugin directly under a hidden dir name is still skipped.
        let hidden_root = base.join(".also-hidden");
        fs::create_dir_all(&hidden_root).unwrap();
        write_info_toml(&hidden_root);
        assert_eq!(
            discover_plugin_roots(&[base.to_path_buf()]),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn depth_six_is_not_reached() {
        let base = temp_tree("depth-six");
        let mut deep = base.to_path_buf();
        for i in 0..6 {
            deep = deep.join(format!("d{i}"));
        }
        fs::create_dir_all(&deep).unwrap();
        write_info_toml(&deep);
        assert_eq!(
            discover_plugin_roots(&[base.to_path_buf()]),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn depth_five_is_reached() {
        let base = temp_tree("depth-five");
        let mut deep = base.to_path_buf();
        for i in 0..5 {
            deep = deep.join(format!("d{i}"));
        }
        fs::create_dir_all(&deep).unwrap();
        write_info_toml(&deep);
        assert_eq!(
            discover_plugin_roots(&[base.to_path_buf()]),
            vec![canonic(&deep)]
        );
    }

    #[test]
    fn nonexistent_folder_is_skipped() {
        // `reserve` hands back a unique path it does NOT create, so nothing
        // another test leaves in the temp dir can make this one green by
        // accident.
        let ghost = crate::testing::TempDir::reserve("openplanet-lsp-ws-roots-nonexistent");
        assert!(!ghost.exists());
        assert_eq!(
            discover_plugin_roots(&[ghost.to_path_buf()]),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn duplicate_folders_are_deduped() {
        // Isolated parent so the walk can't see unrelated /tmp fixtures.
        let parent = temp_tree("dedup-parent");
        let base = parent.join("dedup-base");
        fs::create_dir_all(&base).unwrap();
        let plugin = base.join("plugin");
        fs::create_dir_all(&plugin).unwrap();
        write_info_toml(&plugin);
        // Same folder twice + a parent folder that also finds it.
        assert_eq!(
            discover_plugin_roots(&[base.clone(), base.clone(), parent.to_path_buf()]),
            vec![canonic(&plugin)]
        );
    }
}
