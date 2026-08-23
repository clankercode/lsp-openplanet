use std::path::{Path, PathBuf};

/// Directory never treated as plugin source, whatever the ignore rules say.
const GIT_DIR: &str = ".git";

/// Default for `honor_gitignore` (GH #60). Enabled: other language tooling
/// (rust-analyzer, gopls, tsc) honors gitignore, and a file the author
/// believes is excluded should not produce squiggles. The single source of
/// truth for the default across the config ladder.
pub const DEFAULT_HONOR_GITIGNORE: bool = true;

/// Find the workspace root by walking up from the given path to find info.toml.
pub fn find_workspace_root(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };

    loop {
        if current.join("info.toml").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Knobs for `.as` discovery under a plugin root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryOptions {
    /// Skip files excluded by `.gitignore` rules at or below the plugin root
    /// (GH #60; `honor_gitignore` config key, `--no-gitignore` CLI flag).
    /// `false` restores the plain extension walk.
    pub honor_gitignore: bool,
}

impl DiscoveryOptions {
    /// Build from a tri-state config value; `None` (unset anywhere in the
    /// ladder) takes [`DEFAULT_HONOR_GITIGNORE`].
    pub fn from_honor_gitignore(honor_gitignore: Option<bool>) -> Self {
        Self {
            honor_gitignore: honor_gitignore.unwrap_or(DEFAULT_HONOR_GITIGNORE),
        }
    }
}

impl Default for DiscoveryOptions {
    /// Matches the product default: gitignore honoring is ON.
    fn default() -> Self {
        Self::from_honor_gitignore(None)
    }
}

/// Discover all .as source files under the workspace root, with gitignore
/// honoring at its default (enabled).
pub fn discover_source_files(root: &Path) -> Vec<PathBuf> {
    discover_source_files_with(root, DiscoveryOptions::default())
}

/// Discover all .as source files under the workspace root.
///
/// With `honor_gitignore`, `.gitignore` files at or below `root` (plus a
/// root-local `.git/info/exclude`) remove files from the set; otherwise every
/// `.as` file under the root is returned. Either way the result is sorted, and
/// symlinked directories are followed — plugin trees are symlink farms
/// (dips++-all), so a linked source dir must still be discovered.
pub fn discover_source_files_with(root: &Path, options: DiscoveryOptions) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if options.honor_gitignore {
        discover_respecting_gitignore(root, &mut files);
    } else {
        discover_recursive(root, &mut files);
    }
    files.sort();
    log_excluded_count(root, options, files.len());
    files
}

/// Plain extension walk: every `.as` file under `dir`, no ignore rules.
/// `Path::is_dir` is stat-based, so symlinked dirs are followed.
fn discover_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == GIT_DIR) {
            continue;
        }
        if path.is_dir() {
            discover_recursive(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "as") {
            files.push(path);
        }
    }
}

/// Extension walk with git's own ignore semantics applied (GH #60).
///
/// The `ignore` crate owns the fiddly parts — per-directory `.gitignore`
/// stacking, anchored patterns, negations, and the rule that an excluded
/// directory is never descended into. Walker policy, all deliberate:
///
/// - `parents(false)`: ignore rules ABOVE the plugin root never apply. A
///   plugin root often sits inside a larger repo whose `.gitignore` says
///   nothing about which files the plugin compiles (B015 decision 7).
/// - `require_git(false)`: a `.gitignore` applies even when the root is not
///   itself a git repo (unpacked plugin dirs, symlink farms).
/// - `git_global(false)` / `ignore(false)`: only in-tree gitignore sources
///   count. A user's global gitignore or a `.ignore`/`.rgignore` file is
///   invisible to the plugin author reading their own repo, and a silent
///   exclusion is the failure mode that hurts here.
/// - `hidden(false)`: the pre-GH-#60 walk had no hidden-dir skip, so hidden
///   dirs stay in the set unless a gitignore rule excludes them. `.git`
///   itself is filtered out separately — git never tracks its own metadata.
/// - `follow_links(true)`: preserves the stat-based symlinked-dir walk.
fn discover_respecting_gitignore(root: &Path, files: &mut Vec<PathBuf>) {
    let walker = ignore::WalkBuilder::new(root)
        .parents(false)
        .require_git(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .ignore(false)
        .hidden(false)
        .follow_links(true)
        .filter_entry(|entry| entry.file_name() != GIT_DIR)
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "as") {
            files.push(path.to_path_buf());
        }
    }
}

/// Debug-log how many `.as` files the gitignore filter removed. Silently
/// dropping a compile-critical file is the failure mode that matters here, so
/// the exclusion is inspectable (`RUST_LOG=debug`). The comparison walk only
/// runs when DEBUG is actually enabled.
fn log_excluded_count(root: &Path, options: DiscoveryOptions, kept: usize) {
    if !options.honor_gitignore || !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    let mut all = Vec::new();
    discover_recursive(root, &mut all);
    let excluded = all.len().saturating_sub(kept);
    if excluded > 0 {
        tracing::debug!(
            root = %root.display(),
            excluded,
            kept,
            "gitignore excluded .as files from discovery \
             (disable with honor_gitignore = false or --no-gitignore)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_find_workspace_root() {
        // Uses a real plugin directory if available
        let counter = PathBuf::from(env!("HOME")).join("src/openplanet/tm-counter");
        if counter.exists() {
            let root = find_workspace_root(&counter.join("src/Main.as"));
            assert_eq!(root, Some(counter.clone()));
        }
    }

    fn temp_tree(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "openplanet-lsp-ws-project-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn write(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// Root-relative discovered paths, sorted, for readable assertions.
    fn discovered(root: &Path, options: DiscoveryOptions) -> Vec<String> {
        discover_source_files_with(root, options)
            .into_iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn gitignored_file_is_dropped_when_honoring() {
        let root = temp_tree("honor");
        write(&root, ".gitignore", "generated/\n");
        write(&root, "src/Main.as", "void Main() {}\n");
        write(&root, "generated/Gen.as", "void Gen() {}\n");

        assert_eq!(
            discovered(&root, DiscoveryOptions::default()),
            vec!["src/Main.as".to_string()]
        );
    }

    #[test]
    fn gitignored_file_is_kept_when_not_honoring() {
        let root = temp_tree("no-honor");
        write(&root, ".gitignore", "generated/\n");
        write(&root, "src/Main.as", "void Main() {}\n");
        write(&root, "generated/Gen.as", "void Gen() {}\n");

        assert_eq!(
            discovered(
                &root,
                DiscoveryOptions {
                    honor_gitignore: false
                }
            ),
            vec!["generated/Gen.as".to_string(), "src/Main.as".to_string()]
        );
    }

    /// Both walks agree when no ignore rules exist — the flag must not change
    /// the discovered set on the overwhelmingly common tree.
    #[test]
    fn both_walks_agree_without_a_gitignore() {
        let root = temp_tree("agree");
        write(&root, "src/Main.as", "void Main() {}\n");
        write(&root, "src/deep/Nested.as", "void Nested() {}\n");
        write(&root, "Root.as", "void Root() {}\n");
        write(&root, "src/notes.md", "not angelscript\n");

        let honoring = discovered(&root, DiscoveryOptions::default());
        let plain = discovered(
            &root,
            DiscoveryOptions {
                honor_gitignore: false,
            },
        );
        assert_eq!(honoring, plain);
        assert_eq!(
            honoring,
            vec![
                "Root.as".to_string(),
                "src/Main.as".to_string(),
                "src/deep/Nested.as".to_string(),
            ]
        );
    }

    /// `.git` internals are not plugin source on either walk.
    #[test]
    fn git_dir_is_never_discovered() {
        let root = temp_tree("git-dir");
        write(&root, "src/Main.as", "void Main() {}\n");
        write(&root, ".git/hooks/Hook.as", "void Hook() {}\n");

        for honor_gitignore in [true, false] {
            assert_eq!(
                discovered(&root, DiscoveryOptions { honor_gitignore }),
                vec!["src/Main.as".to_string()],
                "honor_gitignore = {honor_gitignore}"
            );
        }
    }

    /// Hidden dirs are still walked (pre-GH-#60 behavior) unless gitignored.
    #[test]
    fn hidden_dirs_stay_discovered_unless_gitignored() {
        let root = temp_tree("hidden");
        write(&root, "src/Main.as", "void Main() {}\n");
        write(&root, ".hidden/Hidden.as", "void Hidden() {}\n");

        assert_eq!(
            discovered(&root, DiscoveryOptions::default()),
            vec![".hidden/Hidden.as".to_string(), "src/Main.as".to_string()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_dirs_are_followed_on_both_walks() {
        let base = temp_tree("symlink");
        let root = base.join("plugin");
        fs::create_dir_all(&root).unwrap();
        write(&root, "src/Main.as", "void Main() {}\n");
        let elsewhere = base.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("Linked.as"), "void Linked() {}\n").unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("linked")).unwrap();

        for honor_gitignore in [true, false] {
            assert_eq!(
                discovered(&root, DiscoveryOptions { honor_gitignore }),
                vec!["linked/Linked.as".to_string(), "src/Main.as".to_string()],
                "honor_gitignore = {honor_gitignore}"
            );
        }
    }

    /// Ignore rules above the plugin root never apply (B015 decision 7).
    #[test]
    fn parent_gitignore_is_not_consulted() {
        let base = temp_tree("parent");
        write(&base, ".gitignore", "generated/\n");
        let root = base.join("plugin");
        fs::create_dir_all(&root).unwrap();
        write(&root, "src/Main.as", "void Main() {}\n");
        write(&root, "generated/Gen.as", "void Gen() {}\n");

        assert_eq!(
            discovered(&root, DiscoveryOptions::default()),
            vec!["generated/Gen.as".to_string(), "src/Main.as".to_string()]
        );
    }
}
