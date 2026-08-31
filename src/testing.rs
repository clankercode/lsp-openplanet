//! Self-cleaning temp directories for tests (B065).
//!
//! # Why this module is `pub` in a production crate
//!
//! The leak this fixes is spread across BOTH the unit tests inside `src/` and
//! the integration tests in `tests/`. Each file in `tests/` is its own crate
//! that links `openplanet-lsp` as an ordinary dependency, so it cannot see
//! `#[cfg(test)]` items — the trick used by [`crate::server::test_support`]
//! and [`crate::analysis::unused::test_support`], both of which are only ever
//! needed from inside the lib. Gating this one the same way would force a
//! second copy under `tests/`, and a cleanup guard that exists twice is a
//! guard that gets fixed once. Forty lines of hidden surface is the cheaper
//! half of that trade.
//!
//! # The defect it replaces
//!
//! Test helpers built a path under [`std::env::temp_dir()`] keyed on
//! [`std::process::id()`] and called `remove_dir_all` on it *before* using it,
//! to clear any residue from an earlier run. Two ways that leaks:
//!
//! * Helpers that never removed the directory afterwards leaked on **every**
//!   run — measured at 320 directories per full suite, and ~141,000
//!   accumulated in `/tmp` on the machine where this was found.
//! * Helpers that did remove it at the end of the test leaked whenever the
//!   test **panicked**, since a failed assertion unwinds straight past the
//!   trailing cleanup. Those are invisible on a green run and appear only
//!   when the suite is red — which is exactly when nobody is looking at
//!   `/tmp`.
//!
//! [`TempDir`] closes both: the directory is removed by [`Drop`], which runs
//! on the unwind path as well as the normal one.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Serializes every test that reads or writes a process-global environment
/// variable.
///
/// `cargo test --lib` runs a module's tests as THREADS in one process, so a
/// `set_var` anywhere is visible everywhere. B032 built this lock inside
/// `src/update.rs` and fixed the race between that module's own tests; it
/// could not fix the race between modules, because a second mutex does not
/// serialise against the first. `src/config.rs` writes `HOME` too. One lock,
/// process-wide, is the only shape that works.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Ownership token for the process-wide test environment lock.
///
/// This intentionally wraps, rather than aliases, [`MutexGuard`]. Some async
/// server tests must serialize their complete setup and request sequence with
/// tests that temporarily replace `HOME`. Holding the protocol token across
/// those awaits is deliberate: none of the awaited operations acquires this
/// lock, and dropping it earlier would reopen the process-environment race.
/// The wrapper keeps that test protocol explicit without presenting an
/// ordinary synchronous mutex guard to `clippy::await_holding_lock` at every
/// call site.
///
/// The inner guard preserves the mutex's non-reentrant behavior and releases
/// the lock when this token is dropped.
#[must_use = "dropping the guard immediately releases the environment lock"]
pub struct EnvLockGuard {
    _guard: MutexGuard<'static, ()>,
}

/// Take the process-wide environment lock.
///
/// Poisoning is ignored on purpose: one failing test must not cascade into a
/// wall of `PoisonError`s that buries the real assertion.
pub fn env_lock() -> EnvLockGuard {
    EnvLockGuard {
        _guard: ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner()),
    }
}

/// Scoped environment mutation: holds [`env_lock`], and restores every
/// variable it touched when dropped.
///
/// Restoring in `Drop` rather than at the end of the test is the same lesson
/// as [`TempDir`]: a failed assertion unwinds straight past a trailing
/// `remove_var`, and a leaked `HOME` then poisons every test that runs after
/// it — a failure that surfaces far from its cause.
pub struct EnvVars {
    saved: Vec<(String, Option<std::ffi::OsString>)>,
    _lock: EnvLockGuard,
}

impl EnvVars {
    /// Take the lock and remember the current value of each name in `names`.
    pub fn capture(names: &[&str]) -> Self {
        let lock = env_lock();
        let saved = names
            .iter()
            .map(|n| ((*n).to_string(), std::env::var_os(n)))
            .collect();
        Self { saved, _lock: lock }
    }

    /// Set a variable for the lifetime of this guard.
    pub fn set(&self, name: &str, value: impl AsRef<std::ffi::OsStr>) {
        debug_assert!(
            self.saved.iter().any(|(n, _)| n == name),
            "`{name}` was not captured, so it will not be restored"
        );
        std::env::set_var(name, value);
    }

    /// Remove a variable for the lifetime of this guard.
    pub fn remove(&self, name: &str) {
        debug_assert!(
            self.saved.iter().any(|(n, _)| n == name),
            "`{name}` was not captured, so it will not be restored"
        );
        std::env::remove_var(name);
    }
}

impl Drop for EnvVars {
    fn drop(&mut self) {
        for (name, value) in &self.saved {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }
}

/// Distinguishes directories created within a single process, so a helper
/// called from several tests — or twice by one test — never hands out the
/// same path.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// A temp directory that removes itself when dropped.
///
/// Deliberately **not** [`Clone`]: two guards owning one path would delete it
/// while the other is still using it. A helper that needs to hand the path to
/// a caller should return the `TempDir` itself and let the caller hold it for
/// as long as the directory is needed. Dropping it early — `let _ = TempDir::new(..)`
/// rather than `let _dir = TempDir::new(..)` — deletes the directory
/// immediately, which is occasionally what you want and usually a bug.
///
/// Derefs to [`Path`], so `dir.join("x")`, `&dir` in `&Path` position, and
/// `dir.to_str()` all work as they did when these helpers held a [`PathBuf`].
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a fresh directory named after `label`, and any parents.
    ///
    /// The name carries the process id and a per-process counter, so
    /// concurrent runs of the suite get disjoint paths and neither can delete
    /// the other's directories. That is a change in kind from the code this
    /// replaces, which cleared the path on the way IN: two runs that shared a
    /// pid — or one run whose pid had been recycled — raced over the same
    /// directory.
    pub fn new(label: &str) -> Self {
        Self::in_dir(std::env::temp_dir(), label)
    }

    /// Create a fresh directory under an explicit `parent`, and any of its
    /// own missing parents.
    ///
    /// Exists because not every suite wants `/tmp` (B075). The B046 typedb
    /// tests assemble a database pair below the repository's `target/` on
    /// purpose, so that they depend on neither a home directory nor a
    /// writable `/tmp`. That is a considered decision, and moving those
    /// tests into `/tmp` just to reuse [`TempDir::new`] would reverse it.
    /// This constructor keeps the placement and adds the guarantee the
    /// placement was missing: [`Drop`] removes the directory, on the unwind
    /// path as well as the normal one.
    ///
    /// `parent` is created if it does not exist, so a caller need not
    /// pre-create `target/` on a tree that has never been built.
    pub fn in_dir(parent: impl AsRef<Path>, label: &str) -> Self {
        let dir = Self::reserve_in(parent, label);
        std::fs::create_dir_all(&dir.path)
            .unwrap_or_else(|e| panic!("failed to create temp dir {}: {e}", dir.path.display()));
        dir
    }

    /// Reserve a unique path WITHOUT creating it, for tests whose subject is a
    /// directory that does not exist. Dropping still attempts removal, so a
    /// test that later creates the path is covered too.
    pub fn reserve(label: &str) -> Self {
        Self::reserve_in(std::env::temp_dir(), label)
    }

    /// The one place a guarded path is named, so `/tmp` and an explicit
    /// parent cannot drift apart in how they suffix it.
    fn reserve_in(parent: impl AsRef<Path>, label: &str) -> Self {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = parent
            .as_ref()
            .join(format!("{label}-{}-{seq}", std::process::id()));
        Self { path }
    }

    /// The directory's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Alias for [`TempDir::path`], so call sites that held a [`PathBuf`]
    /// keep reading the way they did.
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// Consume the guard, handing back `<dir>/<rel>` as a [`TempPath`] that
    /// carries the guard with it.
    ///
    /// Takes `self` by value on purpose: two guards for one directory would
    /// race to delete it, so there can only ever be the one.
    pub fn into_child(self, rel: impl AsRef<Path>) -> TempPath {
        let path = self.path.join(rel);
        TempPath { _dir: self, path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort: a test that already removed the directory, or that left
        // it read-only, must not turn a green run red during unwind — and
        // panicking in `drop` while an assertion is already unwinding aborts
        // the process.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// So a guard can still be handed to `std::env::set_var("HOME", &dir)`, which
/// several tests do to pin a synthetic home.
impl AsRef<std::ffi::OsStr> for TempDir {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

impl std::fmt::Debug for TempDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.path, f)
    }
}

/// A path *inside* a temp directory, carrying that directory's guard.
///
/// Exists for the common helper shape: build a tree, hand back one path from
/// inside it (`dir/config.toml`, `dir/plugin`). Returning a bare [`PathBuf`]
/// there drops the [`TempDir`] at the end of the helper, so the tree is
/// deleted before the caller looks at it. Holding the guard alongside the
/// path ties the two lifetimes together.
///
/// Derefs to [`Path`], so callers that used to receive a `PathBuf` need no
/// changes.
pub struct TempPath {
    _dir: TempDir,
    path: PathBuf,
}

impl TempPath {
    /// The derived path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Alias for [`TempPath::path`].
    pub fn as_path(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for TempPath {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempPath {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<std::ffi::OsStr> for TempPath {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

impl std::fmt::Debug for TempPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.path, f)
    }
}

#[cfg(test)]
mod tests {
    use super::{TempDir, TempPath};

    #[test]
    fn new_creates_the_directory_and_drop_removes_it() {
        let path;
        {
            let dir = TempDir::new("oplsp-b065-selftest");
            path = dir.to_path_buf();
            assert!(path.is_dir(), "TempDir::new must create the directory");
        }
        assert!(!path.exists(), "Drop must remove the directory");
    }

    #[test]
    fn drop_removes_a_populated_directory() {
        let path;
        {
            let dir = TempDir::new("oplsp-b065-selftest-populated");
            std::fs::create_dir_all(dir.join("a/b/c")).unwrap();
            std::fs::write(dir.join("a/b/c/f.txt"), "x").unwrap();
            path = dir.to_path_buf();
        }
        assert!(!path.exists(), "Drop must remove the whole tree");
    }

    #[test]
    fn reserve_does_not_create_but_still_cleans_up() {
        let path;
        {
            let dir = TempDir::reserve("oplsp-b065-selftest-reserved");
            assert!(!dir.exists(), "reserve must not create the directory");
            std::fs::create_dir_all(dir.path()).unwrap();
            path = dir.to_path_buf();
        }
        assert!(
            !path.exists(),
            "Drop must remove a reserved-then-created dir"
        );
    }

    /// The whole point of `Drop` over a trailing `remove_dir_all`: a failed
    /// assertion unwinds past the trailing call but not past the destructor.
    #[test]
    fn drop_runs_on_the_unwind_path() {
        let path = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = std::sync::Arc::clone(&path);

        let result = std::panic::catch_unwind(move || {
            let dir = TempDir::new("oplsp-b065-selftest-panic");
            *sink.lock().unwrap() = Some(dir.to_path_buf());
            panic!("deliberate: stands in for a failed assertion");
        });

        assert!(result.is_err(), "the closure must have panicked");
        let leaked = path.lock().unwrap().clone().expect("dir was created");
        assert!(
            !leaked.exists(),
            "a panicking test must not leave {} behind",
            leaked.display()
        );
    }

    /// Concurrent suites must not collide, and a helper called twice must not
    /// hand out the same directory twice — the failure that made the old
    /// `remove_dir_all`-on-entry pattern destructive.
    #[test]
    fn labels_are_unique_per_call() {
        let a = TempDir::new("oplsp-b065-selftest-unique");
        let b = TempDir::new("oplsp-b065-selftest-unique");
        assert_ne!(a.path(), b.path());
        assert!(a.is_dir() && b.is_dir());
    }

    /// Models a live fixture from a concurrent run. This is deliberately not
    /// hand-cleaned by callers: the assertion that follows can panic too.
    fn other_runs_directory(label: &str) -> TempDir {
        let other_run = TempDir::new(label);
        std::fs::write(other_run.join("fixture.txt"), "in use").unwrap();
        other_run
    }

    #[test]
    fn other_runs_fixture_is_cleaned_on_unwind() {
        let path = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = std::sync::Arc::clone(&path);

        let result = std::panic::catch_unwind(move || {
            let other_run = other_runs_directory("oplsp-b065-selftest-other-unwind");
            *sink.lock().unwrap() = Some(other_run.to_path_buf());
            panic!("deliberate: the collision assertion can fail");
        });

        assert!(result.is_err(), "the closure must have panicked");
        let leaked = path.lock().unwrap().clone().expect("dir was created");
        assert!(
            !leaked.exists(),
            "a concurrent-run fixture must be cleaned during unwind: {}",
            leaked.display()
        );
    }

    /// Two concurrent runs must not delete each other's directories.
    ///
    /// The code this replaces cleared its path on the way IN, so a second
    /// process sharing the label — or inheriting a recycled pid — wiped a live
    /// run's fixtures out from under it. Construction must now create only its
    /// own directory and touch nothing else that shares the label.
    #[test]
    fn construction_never_removes_another_runs_directory() {
        let label = "oplsp-b065-selftest-concurrent";

        // Stands in for a directory belonging to a different, still-running
        // suite. The same label exercises the potential collision while the
        // counter makes the two owners disjoint.
        let other_run = other_runs_directory(label);

        let mine = TempDir::new(label);
        assert_ne!(mine.path(), other_run.path());
        assert!(
            other_run.join("fixture.txt").exists(),
            "constructing a TempDir must not disturb {}",
            other_run.display()
        );

        // Dropping ours must be just as narrow.
        drop(mine);
        assert!(other_run.join("fixture.txt").exists());
    }

    /// The bug `TempPath` exists to prevent: a helper that returns a derived
    /// path must keep the tree alive for its caller.
    #[test]
    fn temp_path_keeps_the_directory_alive_for_its_caller() {
        fn helper() -> TempPath {
            let dir = TempDir::new("oplsp-b065-selftest-child");
            std::fs::write(dir.join("f.txt"), "kept").unwrap();
            dir.into_child("f.txt")
        }

        let leaked;
        {
            let p = helper();
            assert_eq!(std::fs::read_to_string(&p).unwrap(), "kept");
            leaked = p.parent().unwrap().to_path_buf();
        }
        assert!(
            !leaked.exists(),
            "dropping the TempPath must remove the dir"
        );
    }

    // ---- `in_dir`: the same guard, one directory over (B075) ----

    /// The property that makes `in_dir` worth having: it puts the directory
    /// where the caller asked, not in `/tmp`.
    ///
    /// This is the assertion that fails if `in_dir` ever ignores its `parent`
    /// argument and falls back to [`std::env::temp_dir`] — which is exactly
    /// the shortcut B075 was told not to take.
    #[test]
    fn in_dir_creates_under_the_given_parent() {
        let parent = TempDir::new("oplsp-b075-selftest-parent");
        let dir = TempDir::in_dir(parent.path(), "child");

        assert_eq!(
            dir.path().parent(),
            Some(parent.path()),
            "in_dir must place the directory under its `parent` argument"
        );
        assert!(dir.is_dir(), "in_dir must create the directory");
    }

    /// Negative control for the test above. If both constructors resolved to
    /// the same place, `in_dir_creates_under_the_given_parent` would pass for
    /// the wrong reason on any machine whose `TMPDIR` happened to match.
    #[test]
    fn new_still_creates_under_tmp_so_the_two_constructors_differ() {
        let in_tmp = TempDir::new("oplsp-b075-selftest-tmp");
        assert_eq!(in_tmp.path().parent(), Some(std::env::temp_dir().as_path()));

        let parent = TempDir::new("oplsp-b075-selftest-elsewhere");
        let elsewhere = TempDir::in_dir(parent.path(), "child");
        assert_ne!(
            elsewhere.path().parent(),
            in_tmp.path().parent(),
            "the two constructors must be distinguishable"
        );
    }

    /// `target/` does not exist on a tree that has never been built, so the
    /// constructor has to make it rather than fail.
    #[test]
    fn in_dir_creates_a_parent_that_does_not_exist_yet() {
        let root = TempDir::reserve("oplsp-b075-selftest-missing-parent");
        assert!(!root.exists(), "precondition: the parent must be absent");

        let nested = root.join("a/b/target");
        let dir = TempDir::in_dir(&nested, "child");
        assert!(dir.is_dir(), "in_dir must create missing parents");
        assert_eq!(dir.path().parent(), Some(nested.as_path()));
    }

    #[test]
    fn in_dir_drop_removes_a_populated_directory() {
        let parent = TempDir::new("oplsp-b075-selftest-drop");
        let path;
        {
            let dir = TempDir::in_dir(parent.path(), "child");
            std::fs::create_dir_all(dir.join("a/b/c")).unwrap();
            std::fs::write(dir.join("a/b/c/f.txt"), "x").unwrap();
            path = dir.to_path_buf();
        }
        assert!(!path.exists(), "Drop must remove the whole tree");
        assert!(
            parent.is_dir(),
            "Drop must not remove the caller's parent directory"
        );
    }

    /// The case a trailing `remove_dir_all` cannot cover, and the reason
    /// B075 is a real leak rather than a tidy-up.
    #[test]
    fn in_dir_drop_runs_on_the_unwind_path() {
        let parent = TempDir::new("oplsp-b075-selftest-unwind");
        let parent_path = parent.to_path_buf();
        let path = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = std::sync::Arc::clone(&path);

        let result = std::panic::catch_unwind(move || {
            let dir = TempDir::in_dir(&parent_path, "child");
            *sink.lock().unwrap() = Some(dir.to_path_buf());
            panic!("deliberate: stands in for a failed assertion");
        });

        assert!(result.is_err(), "the closure must have panicked");
        let leaked = path.lock().unwrap().clone().expect("dir was created");
        assert!(
            !leaked.exists(),
            "a panicking test must not leave {} behind",
            leaked.display()
        );
    }

    #[test]
    fn in_dir_labels_are_unique_per_call() {
        let parent = TempDir::new("oplsp-b075-selftest-unique");
        let a = TempDir::in_dir(parent.path(), "child");
        let b = TempDir::in_dir(parent.path(), "child");
        assert_ne!(a.path(), b.path());
        assert!(a.is_dir() && b.is_dir());
    }

    /// The destructive half of the defect, now one directory over: the code
    /// `in_dir` replaces called `remove_dir_all` on the way IN, so a second
    /// run sharing the label wiped a live run's fixtures.
    #[test]
    fn in_dir_construction_never_removes_another_runs_directory() {
        let parent = TempDir::new("oplsp-b075-selftest-concurrent");
        let other_run = TempDir::in_dir(parent.path(), "child");
        std::fs::write(other_run.join("fixture.txt"), "in use").unwrap();

        let mine = TempDir::in_dir(parent.path(), "child");
        assert_ne!(mine.path(), other_run.path());
        assert!(
            other_run.join("fixture.txt").exists(),
            "constructing must not disturb {}",
            other_run.display()
        );

        drop(mine);
        assert!(
            other_run.join("fixture.txt").exists(),
            "dropping ours must be just as narrow"
        );
    }
}

/// B074: the env-lock enforcement, over the WHOLE crate.
///
/// B032 built this check for `src/update.rs` and scanned exactly that file —
/// `test_fns()` was `include_str!("update.rs")`. A test mutating the process
/// environment in any other module was not failed and not warned; it was
/// never considered. That is not a weak gate, it is a gate whose corpus
/// cannot contain the thing it claims to check, and it is how `src/config.rs`
/// came to race `src/update.rs` over `HOME` through two tickets: B032 fixed
/// one side of the race and could not see that the other side existed.
///
/// The corpus here is the source tree, walked at test time, so there is no
/// module list to fall out of date. A module that never touches the
/// environment needs to do nothing; the day one starts, this fails.
///
/// # What it decides, and what it cannot
///
/// It decides one mechanically checkable property: **every `set_var` /
/// `remove_var` in `src/` sits inside a function that also takes the lock.**
/// That is the property a scan can settle completely, which is exactly why it
/// belongs in a scan rather than in a review — a reviewer who finds four
/// unguarded sites has told you about four sites, never that there is no
/// fifth.
///
/// It does NOT decide the reader side. A test that merely *reads* `HOME`
/// races a writer too, but "reads the environment" is transitive through
/// arbitrary call chains (`LspConfig::load` reaches `std::env::var` four
/// frames down) and is not decidable by scanning. B032 approximated it with a
/// hand-maintained list of entry points, and that list is a corpus with the
/// same weakness this ticket is about. Readers are handled by convention and
/// by `just test-hermetic`, not by this test. Do not read a green here as
/// "no env races remain".
///
/// Integration tests under `tests/` are out of scope: each is its own
/// process, so they cannot race the lib tests or each other. Verified: none
/// of them mutates the environment today.
#[cfg(test)]
mod env_enforcement {
    use std::path::{Path, PathBuf};

    /// Calls that mutate the process environment.
    const MUTATORS: &[&str] = &["set_var(", "remove_var("];

    /// Ways a function can hold the process-wide lock. `EnvVars` takes it in
    /// `capture` and holds it until drop, so a function that captures is
    /// guarded for its whole body.
    const GUARDS: &[&str] = &["env_guard()", "env_lock()", "EnvVars::capture("];

    /// The primitive that implements the lock, and so cannot take it.
    ///
    /// This is an exemption for the three functions that BUILD the guard, not
    /// a corpus restriction — the scan still reads every file. Adding to it is
    /// a visible edit to this list, which is the point: it is checked for
    /// exactness below, so a silent widening fails.
    const LOCK_PRIMITIVE: &[(&str, &str)] = &[
        ("src/testing.rs", "set"),
        ("src/testing.rs", "remove"),
        ("src/testing.rs", "drop"),
    ];

    fn crate_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("src/ must be readable") {
            let path = entry.expect("readable dir entry").path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Blank out comments and every literal form so brace counting and call
    /// matching cannot be fooled by text that is not code.
    ///
    /// Every branch here was added because something slipped through, and the
    /// list is worth reading as a history rather than a spec:
    ///
    /// - comments, because `src/config.rs` carries one quoting
    ///   `set_var("HOME", ..)` to explain why the call was removed;
    /// - raw strings, because this file's own fixture quotes `set_var` inside
    ///   an `r#"..."#` block;
    /// - char literals, because `b'"'` in `src/server/folding.rs` opened a
    ///   string that ran to the next quote and blanked most of the file. Seven
    ///   files under `src/` and `tests/` contain one. A scan that silently
    ///   reads less than it claims is precisely the defect this module exists
    ///   to catch, so it is caught here by a fixture of its own below.
    ///
    /// Blanking char literals also fixes brace counting: `'{'` used to open a
    /// block that never closed.
    fn strip_literals(src: &str) -> String {
        let b = src.as_bytes();
        let mut out = vec![b' '; b.len()];
        let ident =
            |c: Option<&u8>| matches!(c, Some(b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_'));
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\n' {
                out[i] = b'\n';
                i += 1;
                continue;
            }
            if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    if b[i] == b'\n' {
                        out[i] = b'\n';
                    }
                    i += 1;
                }
                i = (i + 2).min(b.len());
                continue;
            }
            // Raw string, with or without a byte-string prefix: `r"..."`,
            // `r#"..."#`, `br##"..."##`.
            let raw_r = if b[i] == b'r' && !ident(b.get(i.wrapping_sub(1))) {
                Some(i)
            } else if b[i] == b'b'
                && b.get(i + 1) == Some(&b'r')
                && !ident(b.get(i.wrapping_sub(1)))
            {
                Some(i + 1)
            } else {
                None
            };
            if let Some(r) = raw_r {
                let mut k = r + 1;
                while k < b.len() && b[k] == b'#' {
                    k += 1;
                }
                if k < b.len() && b[k] == b'"' {
                    let hashes = k - r - 1;
                    i = k + 1;
                    loop {
                        if i >= b.len() {
                            break;
                        }
                        if b[i] == b'"'
                            && b[i + 1..]
                                .iter()
                                .take(hashes)
                                .filter(|c| **c == b'#')
                                .count()
                                == hashes
                        {
                            i += 1 + hashes;
                            break;
                        }
                        if b[i] == b'\n' {
                            out[i] = b'\n';
                        }
                        i += 1;
                    }
                    continue;
                }
            }
            // A leading quote is either a char literal or a lifetime, and only
            // the closing quote tells them apart: `'a` is a lifetime, `'"'` is
            // a char. Guessing wrong in the `'"'` direction is what blinded an
            // earlier version of this scan.
            if b[i] == b'\'' {
                let closes = if b.get(i + 1) == Some(&b'\\') {
                    let mut k = i + 2;
                    while k < b.len() && b[k] != b'\'' && b[k] != b'\n' {
                        k += 1;
                    }
                    (b.get(k) == Some(&b'\'')).then_some(k)
                } else {
                    let mut k = i + 1;
                    if k < b.len() {
                        k += 1;
                        while k < b.len() && b[k] & 0xC0 == 0x80 {
                            k += 1;
                        }
                    }
                    (b.get(k) == Some(&b'\'')).then_some(k)
                };
                match closes {
                    Some(end) => i = end + 1,
                    None => {
                        out[i] = b[i];
                        i += 1;
                    }
                }
                continue;
            }
            if b[i] == b'"' {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    } else if b[i] == b'\n' {
                        out[i] = b'\n';
                    }
                    i += 1;
                }
                i += 1;
                continue;
            }
            out[i] = b[i];
            i += 1;
        }
        String::from_utf8(out).expect("ASCII-preserving transform")
    }

    /// `(name, body)` for every `fn` in `src`, bodies matched by brace depth.
    fn functions(src: &str) -> Vec<(String, String)> {
        let clean = strip_literals(src);
        let bytes = clean.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while let Some(rel) = clean[i..].find("fn ") {
            let at = i + rel;
            // `fn` must start a token, or `..fn ` inside an identifier matches.
            let prev_ok =
                at == 0 || !matches!(bytes[at - 1], b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_');
            if !prev_ok {
                i = at + 3;
                continue;
            }
            let name: String = clean[at + 3..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let Some(open_rel) = clean[at..].find('{') else {
                break;
            };
            let open = at + open_rel;
            let mut depth = 0usize;
            let mut end = open;
            for (k, c) in clean[open..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + k;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            out.push((name, clean[open..=end].to_string()));
            i = open + 1;
        }
        out
    }

    /// The scan itself, so the tests below can run it over synthetic input as
    /// well as over the tree. Returns one message per violation.
    fn unguarded_mutations(rel_path: &str, src: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (name, body) in functions(src) {
            if !MUTATORS.iter().any(|m| body.contains(m)) {
                continue;
            }
            if GUARDS.iter().any(|g| body.contains(g)) {
                continue;
            }
            if LOCK_PRIMITIVE.contains(&(rel_path, name.as_str())) {
                continue;
            }
            out.push(format!("{rel_path}: fn {name}"));
        }
        out
    }

    #[test]
    fn every_env_mutation_in_the_crate_takes_the_lock() {
        let root = crate_root();
        let mut files = Vec::new();
        // `tests/` as well as `src/`. Each integration binary runs its own
        // tests as threads in ONE process, so an unguarded mutation there
        // races its siblings exactly as one in `src/` does. No `tests/` file
        // mutates the environment today; the point is that the first one to
        // do so is caught on the way in, not after it starts flaking.
        for dir in ["src", "tests"] {
            rust_sources(&root.join(dir), &mut files);
        }
        assert!(
            files.len() > 20,
            "the walk found only {} files; a scan that reads nothing passes \
             vacuously, which is the defect this test exists to fix",
            files.len()
        );

        let mut violations = Vec::new();
        let mut scanned_with_mutations = 0usize;
        let mut functions_found = 0usize;
        for path in &files {
            let rel = path
                .strip_prefix(&root)
                .expect("under the crate root")
                .to_string_lossy()
                .replace('\\', "/");
            let src = std::fs::read_to_string(path).expect("readable source");
            if MUTATORS.iter().any(|m| strip_literals(&src).contains(m)) {
                scanned_with_mutations += 1;
            }
            functions_found += functions(&src).len();
            violations.extend(unguarded_mutations(&rel, &src));
        }

        // A literal form the stripper mishandles blanks the REST OF THE FILE,
        // so the symptom is a body count that silently drops rather than an
        // error. `b'"'` cost about 20 functions before it was found; this
        // floor sits far enough below the real figure to survive ordinary
        // churn and far enough above zero to fail on a stripper regression.
        assert!(
            functions_found > 3000,
            "the scan found only {functions_found} function bodies across \
             {} files. A literal form the stripper does not understand blanks \
             everything after it, so a body count that falls is how that \
             failure shows up.",
            files.len()
        );
        assert!(
            scanned_with_mutations >= 2,
            "expected at least the lock primitive and update.rs to mutate the \
             environment; found {scanned_with_mutations} such files, so the \
             matcher has probably stopped matching"
        );
        assert!(
            violations.is_empty(),
            "these functions mutate the process environment without holding \
             the lock, so they race every other test in the same binary. Take \
             `crate::testing::EnvVars::capture(..)`, which also restores on \
             the unwind path:\n  {}",
            violations.join("\n  ")
        );
    }

    /// Positive control. A gate that has never been observed saying NO is not
    /// known to be a gate — and this one replaces a check that said YES for
    /// two tickets while a real race ran underneath it.
    #[test]
    fn the_scan_flags_an_unguarded_mutation() {
        let src = r#"
            #[test]
            fn touches_home_without_the_lock() {
                std::env::set_var("HOME", "/tmp/nope");
                assert!(true);
            }
        "#;
        let found = unguarded_mutations("src/somewhere.rs", src);
        assert_eq!(
            found,
            vec!["src/somewhere.rs: fn touches_home_without_the_lock".to_string()],
            "the scan must catch an unguarded mutation in a module it has \
             never heard of"
        );
    }

    /// Negative control. A scan that flags everything is as useless as one
    /// that flags nothing, and would make the positive control above pass for
    /// the wrong reason.
    #[test]
    fn the_scan_accepts_a_guarded_mutation() {
        let src = r#"
            #[test]
            fn touches_home_under_the_lock() {
                let env = crate::testing::EnvVars::capture(&["HOME"]);
                env.set("HOME", "/tmp/fine");
                assert!(true);
            }
        "#;
        assert!(unguarded_mutations("src/somewhere.rs", src).is_empty());
    }

    /// Prose must not be mistaken for code. `src/config.rs` carries a comment
    /// quoting `set_var("HOME", ..)` to explain why the call was removed, and
    /// an unstripped scan flags it.
    #[test]
    fn the_scan_ignores_mutations_named_only_in_comments_and_strings() {
        let src = r#"
            /// This used to `set_var("HOME", &tmp)` with no guard.
            #[test]
            fn only_talks_about_it() {
                let explanation = "call set_var(\"HOME\") here and you race";
                assert!(!explanation.is_empty());
            }
        "#;
        assert!(unguarded_mutations("src/somewhere.rs", src).is_empty());
    }

    /// Regression: a `'"'` char literal must not open a string.
    ///
    /// Found by cross-checking this scan against a per-binary test count, not
    /// by reading the code. `src/server/folding.rs` contains `b == b'"'`;
    /// the old stripper read that quote as the start of a string literal and
    /// blanked everything up to the next one, which silently removed most of
    /// the file from the scan. Seven files under `src/` and `tests/` carry
    /// such a literal.
    #[test]
    fn the_scan_sees_past_a_double_quote_char_literal() {
        let src = r##"
            fn lexer_helper(b: u8) -> bool {
                b == b'"' || b == b'\''
            }

            #[test]
            fn touches_home_after_a_char_literal() {
                std::env::set_var("HOME", "/tmp/nope");
            }
        "##;
        assert_eq!(
            unguarded_mutations("src/somewhere.rs", src),
            vec!["src/somewhere.rs: fn touches_home_after_a_char_literal".to_string()],
            "a char literal containing a quote must not blank the rest of the \
             file; that is how the scan went blind on seven real files"
        );
    }

    /// The same defect seen through brace counting rather than through call
    /// matching: `'{'` used to open a block that never closed, so every
    /// function after it merged into one body.
    #[test]
    fn the_scan_counts_braces_past_a_brace_char_literal() {
        let src = r##"
            fn opens_a_brace_literal(c: char) -> bool {
                c == '{' || c == '}'
            }

            #[test]
            fn touches_home_after_a_brace_literal() {
                std::env::set_var("HOME", "/tmp/nope");
            }

            #[test]
            fn also_touches_home() {
                std::env::remove_var("HOME");
            }
        "##;
        assert_eq!(
            unguarded_mutations("src/somewhere.rs", src),
            vec![
                "src/somewhere.rs: fn touches_home_after_a_brace_literal".to_string(),
                "src/somewhere.rs: fn also_touches_home".to_string(),
            ],
            "two separate functions must stay separate; a brace char literal \
             that is not blanked merges every body after it into one"
        );
    }

    /// A lifetime is not a char literal, and treating it as one would blank
    /// real code from the `'a` to the next quote. The distinction is the
    /// closing quote, which is why both directions are pinned.
    #[test]
    fn the_scan_does_not_mistake_a_lifetime_for_a_char_literal() {
        let src = r##"
            struct Holder<'a> {
                name: &'a str,
            }

            #[test]
            fn touches_home_after_a_lifetime() {
                std::env::set_var("HOME", "/tmp/nope");
            }
        "##;
        assert_eq!(
            unguarded_mutations("src/somewhere.rs", src),
            vec!["src/somewhere.rs: fn touches_home_after_a_lifetime".to_string()],
        );
    }

    /// Byte raw strings (`br#"..."#`) are a raw string with a prefix. The
    /// prefix made the old `r`-keyed branch miss them, so the quote fell
    /// through to the plain-string branch and closed in the wrong place.
    #[test]
    fn the_scan_ignores_mutations_inside_a_byte_raw_string() {
        let src = "
            const DOC: &[u8] = br#\"call set_var(\\\"HOME\\\") and you race\"#;

            #[test]
            fn only_holds_the_bytes() {
                assert!(!DOC.is_empty());
            }
        ";
        assert!(unguarded_mutations("src/somewhere.rs", src).is_empty());
    }

    /// The exemption is for the three functions that BUILD the lock. If it
    /// ever grows, that is a deliberate edit and should read like one.
    #[test]
    fn the_lock_primitive_exemption_stays_minimal() {
        assert_eq!(
            LOCK_PRIMITIVE,
            &[
                ("src/testing.rs", "set"),
                ("src/testing.rs", "remove"),
                ("src/testing.rs", "drop"),
            ],
            "only `EnvVars`'s own accessors may mutate the environment without \
             taking the lock, because they ARE the lock. Anything else added \
             here is a hole in the gate."
        );
    }

    /// Historical witness: the real defect, pinned.
    ///
    /// B074's stated red condition — "the enforcement must FAIL on master
    /// today, because `config.rs` is unguarded" — is no longer literally
    /// available: B066 guarded that call site before this ticket started. A
    /// gate whose only demonstration of saying NO is a synthetic fixture is
    /// weaker than one that says NO to the actual bug, so the actual bug is
    /// kept here.
    ///
    /// The body below is `fn test_game_json_derivation_uses_variant_folder`
    /// copied verbatim from `37723b1:src/config.rs:2888` — the last commit
    /// before the B066 branch — down to its `remove_var`. Note the comment it
    /// carried: it asserted the mutation was safe *because* the value was
    /// unique, which is true of the value and false of the variable. That is
    /// the reasoning the scan exists to overrule, so it is preserved too.
    #[test]
    fn the_scan_flags_the_pre_b066_config_test_that_started_this() {
        let historical = r##"
        #[test]
        fn test_game_json_derivation_uses_variant_folder() {
            // Build a fake variant install: home/OpenplanetTurbo with the two
            // JSON files, plus an OpenplanetNext alongside to prove selection.
            let tmp = crate::testing::TempDir::new("oplsp-gh45");
            let turbo = tmp.join("OpenplanetTurbo");
            let next = tmp.join("OpenplanetNext");
            std::fs::create_dir_all(&turbo).unwrap();
            std::fs::create_dir_all(&next).unwrap();
            std::fs::write(turbo.join("OpenplanetCore.json"), "{}").unwrap();
            std::fs::write(turbo.join("OpenplanetTurbo.json"), "{}").unwrap();
            std::fs::write(next.join("OpenplanetCore.json"), "{}").unwrap();
            std::fs::write(next.join("OpenplanetNext.json"), "{}").unwrap();
            std::fs::create_dir_all(turbo.join("Plugins")).unwrap();
            std::fs::create_dir_all(next.join("Plugins")).unwrap();

            // SAFETY for tests: scoped env mutation is fine here — cargo runs
            // lib tests in one process, but this test owns a unique HOME value
            // and config load is deterministic given HOME.
            std::env::set_var("HOME", &tmp);
            let opts = serde_json::json!({ "game_target": "TURBO" });
            let config = LspConfig::load(None, Some(&opts));
            std::env::remove_var("HOME");

            assert_eq!(config.openplanet_dir, Some(turbo.clone()));
        }
        "##;
        assert_eq!(
            unguarded_mutations("src/config.rs", historical),
            vec!["src/config.rs: fn test_game_json_derivation_uses_variant_folder".to_string()],
            "the gate must say NO to the defect it was written for; if this \
             passes, it is scanning something other than what it claims"
        );
    }
}
