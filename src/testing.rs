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

/// Take the process-wide environment lock.
///
/// Poisoning is ignored on purpose: one failing test must not cascade into a
/// wall of `PoisonError`s that buries the real assertion.
pub fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
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
    _lock: MutexGuard<'static, ()>,
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
        let dir = Self::reserve(label);
        std::fs::create_dir_all(&dir.path)
            .unwrap_or_else(|e| panic!("failed to create temp dir {}: {e}", dir.path.display()));
        dir
    }

    /// Reserve a unique path WITHOUT creating it, for tests whose subject is a
    /// directory that does not exist. Dropping still attempts removal, so a
    /// test that later creates the path is covered too.
    pub fn reserve(label: &str) -> Self {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{label}-{}-{seq}", std::process::id()));
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
}
