use std::collections::HashSet;
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

/// What a discovery walk found: the `.as` files to check, plus how many
/// `.as` files the gitignore filter removed from that set.
///
/// The count is load-bearing, not diagnostic. Gitignore is a git concept, not
/// a compile concept: the game compiles a gitignored `.as` file like any other
/// (generated sources in a dev loop are the motivating case), so once even one
/// file is dropped the pooled symbol table is no longer the plugin's full
/// module surface. Diagnostics that read the absence of a name as a
/// game-compiler rejection must treat a non-zero count as loss of authority
/// (GH #58 x GH #60) — see `PluginWorkspaceLoad::dependency_closure_complete`.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    /// Sorted `.as` files to check.
    pub files: Vec<PathBuf>,
    /// Sorted `.as` files the gitignore filter removed. Always empty when
    /// `honor_gitignore` is off — nothing is filtered on that path.
    ///
    /// The PATHS (not just the count) are load-bearing since GH #61: the
    /// game compiles these files, so the unused-declaration pass reads
    /// them for usage recording. They come free — the count already
    /// required the unfiltered walk, and taking the set difference costs
    /// no extra `read_dir`.
    pub gitignore_excluded_files: Vec<PathBuf>,
}

impl Discovery {
    /// How many `.as` files the gitignore filter removed — the number the
    /// GH #58 namespace-authority fence gates on. Derived from
    /// [`Self::gitignore_excluded_files`] so the two can never drift.
    pub fn gitignore_excluded(&self) -> usize {
        self.gitignore_excluded_files.len()
    }
}

/// Discover all .as source files under the workspace root, with gitignore
/// honoring at its default (enabled).
pub fn discover_source_files(root: &Path) -> Vec<PathBuf> {
    discover_source_files_with(root, DiscoveryOptions::default())
}

/// Discover all .as source files under the workspace root. Callers that need
/// to know whether anything was filtered out want [`discover`] instead.
///
/// With `honor_gitignore`, `.gitignore` files at or below `root` (plus a
/// root-local `.git/info/exclude`) remove files from the set; otherwise every
/// `.as` file under the root is returned. Either way the result is sorted, and
/// symlinked directories are followed — plugin trees are symlink farms
/// (dips++-all), so a linked source dir must still be discovered.
pub fn discover_source_files_with(root: &Path, options: DiscoveryOptions) -> Vec<PathBuf> {
    discover(root, options).files
}

/// [`discover_source_files_with`] plus the number of `.as` files gitignore
/// removed (see [`Discovery`] for why that number matters).
///
/// Cost: when honoring is on, the walk runs TWICE — once with git ignore
/// sources enabled and once with them disabled — because the `ignore` walker
/// never reports the entries it filtered out. Both passes use the same
/// `WalkBuilder` policy, so the baseline cannot drift from the kept set the
/// way an independently written walk could. The extra pass is `read_dir`
/// traffic only, and discovery is a rounding error next to parsing every
/// discovered file, so it is paid unconditionally rather than left behind a
/// DEBUG gate that would make the count unavailable to the checker. When
/// honoring is off there is nothing to compare and only one walk runs.
pub fn discover(root: &Path, options: DiscoveryOptions) -> Discovery {
    let mut files = Vec::new();
    if options.honor_gitignore {
        discover_respecting_gitignore(root, &mut files);
    } else {
        discover_recursive(root, &mut files);
    }
    files.sort();
    let gitignore_excluded_files = if options.honor_gitignore {
        let mut unfiltered = Vec::new();
        walk_as_files(root, false, &mut unfiltered);
        unfiltered.sort();
        // Applying git ignore sources can only REMOVE entries, so the kept
        // set is a subset of the unfiltered walk and this difference has
        // exactly the length the old `unfiltered - kept` subtraction gave.
        let kept: HashSet<&PathBuf> = files.iter().collect();
        unfiltered
            .into_iter()
            .filter(|path| !kept.contains(path))
            .collect()
    } else {
        Vec::new()
    };
    log_excluded_count(root, gitignore_excluded_files.len(), files.len());
    Discovery {
        files,
        gitignore_excluded_files,
    }
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
    walk_as_files(root, true, files);
}

/// The shared `ignore`-crate walk. `apply_git_ignores` toggles ONLY the two
/// git ignore sources, so passing `false` yields the same traversal with
/// nothing filtered — that is the baseline the excluded count is measured
/// against, and using one builder for both keeps them from drifting apart.
fn walk_as_files(root: &Path, apply_git_ignores: bool, files: &mut Vec<PathBuf>) {
    let walker = ignore::WalkBuilder::new(root)
        .parents(false)
        .require_git(false)
        .git_ignore(apply_git_ignores)
        .git_exclude(apply_git_ignores)
        .git_global(false)
        .ignore(false)
        .hidden(false)
        .follow_links(true)
        .filter_entry(|entry| entry.file_name() != GIT_DIR)
        .build();
    for entry in walker.flatten() {
        // The walker already resolved the file type (following links, since
        // `follow_links` is on), so reuse it rather than paying another stat.
        if entry.file_type().is_none_or(|ft| ft.is_dir()) {
            continue;
        }
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "as") {
            files.push(path.to_path_buf());
        }
    }
}

/// Debug-log how many `.as` files the gitignore filter removed. Silently
/// dropping a compile-critical file is the failure mode that matters here, so
/// the exclusion is inspectable (`RUST_LOG=debug`).
///
/// The LOG LINE stays DEBUG-gated; the number itself no longer is. [`discover`]
/// computes it for every load because the checker needs it (a dropped file
/// costs the namespace rule its authority — GH #58), and passes it in here
/// rather than re-deriving it.
fn log_excluded_count(root: &Path, excluded: usize, kept: usize) {
    if excluded == 0 || !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    tracing::debug!(
        root = %root.display(),
        excluded,
        kept,
        "gitignore excluded .as files from discovery \
         (disable with honor_gitignore = false or --no-gitignore)"
    );
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
    /// GH #58 x GH #60: the excluded count is what the checker gates its
    /// namespace-authority on, so it must be exact — and must count only
    /// `.as` files, since a gitignored `.md` costs no module surface.
    #[test]
    fn excluded_count_reports_dropped_as_files_only() {
        let root = temp_tree("excluded-count");
        write(&root, ".gitignore", "generated/\nnotes.md\n");
        write(&root, "src/Main.as", "void Main() {}\n");
        write(&root, "generated/Gen.as", "void Gen() {}\n");
        write(&root, "generated/More.as", "void More() {}\n");
        write(&root, "notes.md", "not angelscript\n");

        let discovery = discover(&root, DiscoveryOptions::default());
        assert_eq!(discovery.files.len(), 1, "{:?}", discovery.files);
        assert_eq!(
            discovery.gitignore_excluded(),
            2,
            "both generated .as files count, the .md does not"
        );
    }

    /// Honoring off: nothing is filtered, so the count is 0 even though a
    /// `.gitignore` exists and would have matched.
    #[test]
    fn excluded_count_is_zero_when_not_honoring() {
        let root = temp_tree("excluded-count-off");
        write(&root, ".gitignore", "generated/\n");
        write(&root, "src/Main.as", "void Main() {}\n");
        write(&root, "generated/Gen.as", "void Gen() {}\n");

        let discovery = discover(
            &root,
            DiscoveryOptions {
                honor_gitignore: false,
            },
        );
        assert_eq!(discovery.files.len(), 2);
        assert_eq!(discovery.gitignore_excluded(), 0);
    }

    /// The overwhelmingly common tree: honoring is on (the default) but no
    /// rule matches any `.as` file. The count must be 0 — a non-zero count
    /// here would disarm the GH #58 namespace rule almost everywhere.
    #[test]
    fn excluded_count_is_zero_when_gitignore_matches_no_source() {
        let root = temp_tree("excluded-count-noop");
        write(&root, ".gitignore", "*.log\nbuild/\n");
        write(&root, "src/Main.as", "void Main() {}\n");
        write(&root, "src/deep/Nested.as", "void Nested() {}\n");

        let discovery = discover(&root, DiscoveryOptions::default());
        assert_eq!(discovery.files.len(), 2);
        assert_eq!(discovery.gitignore_excluded(), 0);
    }

    /// No `.gitignore` at all — the baseline walk and the honoring walk see
    /// the same tree, so nothing is reported as excluded.
    #[test]
    fn excluded_count_is_zero_without_a_gitignore() {
        let root = temp_tree("excluded-count-none");
        write(&root, "src/Main.as", "void Main() {}\n");

        assert_eq!(
            discover(&root, DiscoveryOptions::default()).gitignore_excluded(),
            0
        );
    }
    /// The invariant the GH #58 authority fence rests on: `kept + excluded`
    /// must equal what the non-honoring walk returns, so a dropped file is
    /// always visible in the count. Exercised on a tree with a nested
    /// `.gitignore`, a negation, an ignored directory the walker never
    /// descends into, a hidden dir and a `.git/info/exclude` entry — the
    /// cases where a hand-written baseline walk would drift from the
    /// `ignore` crate's semantics.
    #[test]
    fn kept_plus_excluded_equals_the_unfiltered_walk() {
        let root = temp_tree("count-invariant");
        write(&root, ".gitignore", "generated/\n*.gen.as\n!Keep.gen.as\n");
        write(&root, ".git/info/exclude", "Excluded.as\n");
        write(&root, "src/Main.as", "void Main() {}\n");
        write(&root, "src/Keep.gen.as", "void Keep() {}\n");
        write(&root, "src/Drop.gen.as", "void Drop() {}\n");
        write(&root, "src/Excluded.as", "void Excluded() {}\n");
        write(&root, "src/.gitignore", "Nested.as\n");
        write(&root, "src/Nested.as", "void Nested() {}\n");
        write(&root, "generated/A.as", "void A() {}\n");
        write(&root, "generated/deep/B.as", "void B() {}\n");
        write(&root, ".hidden/Hidden.as", "void Hidden() {}\n");

        let honoring = discover(&root, DiscoveryOptions::default());
        let unfiltered = discover(
            &root,
            DiscoveryOptions {
                honor_gitignore: false,
            },
        );
        assert_eq!(
            honoring.files.len() + honoring.gitignore_excluded(),
            unfiltered.files.len(),
            "kept {:?} + excluded {} must account for every unfiltered file {:?}",
            honoring.files,
            honoring.gitignore_excluded(),
            unfiltered.files
        );
        assert!(
            !honoring.gitignore_excluded_files.is_empty(),
            "this tree really does exclude files: {:?}",
            honoring.files
        );
        // The negation must survive: `Keep.gen.as` stays, `Drop.gen.as` goes.
        let kept: Vec<String> = honoring
            .files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(kept.contains(&"Keep.gen.as".to_string()), "{kept:?}");
        assert!(!kept.contains(&"Drop.gen.as".to_string()), "{kept:?}");
    }
}
