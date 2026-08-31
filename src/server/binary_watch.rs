//! B105: notice that the executable we were launched from has been replaced
//! on disk, and get the editor onto the new one.
//!
//! # Why polling, and why the PATH rather than the open file
//!
//! [`crate::update::replace_executable`] is the swap this watcher exists to
//! see. It stages a sidecar next to the target (`<name>.new.<pid>`) and then
//! `fs::rename`s it over the target path — on Windows after renaming the
//! running image aside to `<name>.old.<pid>`, because Windows will not let a
//! running executable be overwritten. A package manager's install does the
//! same shape of thing.
//!
//! Two consequences drive the whole design:
//!
//! - **The running image is never modified.** On unix `/proc/self/exe` keeps
//!   resolving to the *old* inode long after the path points at a new one, and
//!   on Windows the old image survives under a different name. So asking "has
//!   my own open file changed?" answers "no" forever. The question that has a
//!   useful answer is "is the file *at my path* still the one I started from?"
//!   — which is why [`ExeStamp::of_path`] stats the path and never the handle.
//! - **The swap is a rename, never an in-place write.** There is no window in
//!   which the path holds a half-written binary, so a single stat is a sound
//!   observation and a debounce would buy nothing. A filesystem watcher would
//!   buy nothing either, at the cost of a watcher per platform; a ~30s poll
//!   ([`POLL_INTERVAL`]) is well inside "the editor should not still be on the
//!   old version" and costs one `stat` per poll.
//!
//! # Known limitation: symlink-swap installs
//!
//! `std::env::current_exe()` resolves symlinks — on Linux it reads
//! `/proc/self/exe`, which is already the real path, so the name the user
//! actually launched is not recoverable from inside the process. An install
//! scheme that upgrades by repointing a *symlink* at a new immutable file
//! (Nix, and anything else content-addressed) therefore leaves the path we
//! watch untouched, and this watcher will not fire. Package managers that
//! rewrite the file in place or rename over it — which is npm, cargo-install,
//! the distro packages, and our own self-updater — are all seen.
//!
//! This is a gap, not a bug to fix here: closing it needs the launcher's path,
//! which only the client knows. It is written down so nobody concludes from a
//! silent watcher that detection is broken.
//!
//! # The seam
//!
//! [`BinaryWatch::poll`] is pure: it takes the stamp the caller observed and
//! whether the server is idle, and returns the action to take. It does no I/O,
//! reads no clock, and touches no global state, so every rule below is a plain
//! unit test rather than a race. The server-side adapter
//! (`Backend::spawn_binary_watch`) is a thin loop that supplies
//! [`ExeStamp::of_path`] and an idle bit and performs whatever action comes
//! back.
//!
//! # The rules it encodes
//!
//! - **Identity, not existence, is the trigger.** A stamp equal to the
//!   baseline is not a replacement, even if the file was deleted and recreated
//!   in between (we cannot see that, and on unix a recreation gets a new inode
//!   anyway, so in practice it *is* seen).
//! - **A missing file is never a replacement.** Signalling on absence would
//!   exit the process toward a binary that is not there — a crash loop instead
//!   of an upgrade. Absence is ignored; if a file reappears with a new
//!   identity, *that* is the signal.
//! - **Exactly one signal, ever.** [`BinaryWatch`] latches after it fires.
//!   Further replacements are logged by the adapter but never re-signalled, so
//!   a server and a client cannot ping-pong.
//! - **Deferred until idle.** A detected replacement is held until the server
//!   reports idle; a further replacement while held updates the stamp we will
//!   report, not the number of signals.

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tower_lsp::lsp_types::notification::Notification;

/// How often the adapter re-stamps the executable path.
///
/// Slow on purpose: the cost of noticing 30s late is nil, and the cost of a
/// tight loop is a `stat` per tick for the life of an editor session.
pub const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// `initializationOptions` key selecting what happens on a replacement.
pub const INIT_OPTION_KEY: &str = "on_binary_replaced";

/// The identity of the file sitting at the executable's path.
///
/// Deliberately three independent fields. `len` alone misses a same-size
/// rebuild; `mtime_ns` alone misses a swap that preserves timestamps (`cp -p`,
/// some installers); `inode` catches both but does not exist on Windows, which
/// is the one platform where the rename-swap is mandatory rather than merely
/// conventional. Any one of them differing is a replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExeStamp {
    /// File size in bytes.
    pub len: u64,
    /// Modification time in nanoseconds since the Unix epoch.
    ///
    /// Nanoseconds rather than the more usual milliseconds because a rename
    /// swap can land in the same millisecond as the stamp it replaces; on a
    /// platform with no inode that would be an invisible replacement.
    pub mtime_ns: Option<u64>,
    /// unix inode number; always `None` on other platforms.
    pub inode: Option<u64>,
}

impl ExeStamp {
    /// Stamp the file **at this path**, or `None` when it cannot be stated.
    ///
    /// `None` covers both "deleted" and "we lack permission to look", and
    /// [`BinaryWatch::poll`] treats both the same way: not a replacement.
    pub fn of_path(path: &Path) -> Option<Self> {
        std::fs::metadata(path).ok().map(|md| Self::from_metadata(&md))
    }

    /// The stamp of already-fetched metadata. Split out so the adapter can
    /// stat once and so tests can build stamps without a filesystem.
    pub fn from_metadata(md: &std::fs::Metadata) -> Self {
        Self {
            len: md.len(),
            mtime_ns: md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .and_then(|d| u64::try_from(d.as_nanos()).ok()),
            inode: platform_inode(md),
        }
    }
}

#[cfg(unix)]
fn platform_inode(md: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(md.ino())
}

#[cfg(not(unix))]
fn platform_inode(_md: &std::fs::Metadata) -> Option<u64> {
    None
}

/// What to do when the binary under us is replaced.
///
/// A tri-state string rather than a bool because the two useful behaviours are
/// genuinely different *and* a kill switch matters for a feature that can end
/// the process. Matches the house shape of `unused` / `inactive_regions` /
/// `signature_mode`, which are also string levels in `initializationOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplacedMode {
    /// The client told us it handles `openplanet/serverBinaryReplaced`: send
    /// the notification and keep serving. The client decides when to restart
    /// us, which is the only way it can do so without racing its own retry
    /// counter.
    Notify,
    /// Default. Nothing is listening for the notification, so exit cleanly
    /// once idle and rely on the editor respawning the server — which VS
    /// Code's `LanguageClient` does, with bounded retries.
    #[default]
    Exit,
    /// Watch nothing. For a client that would rather keep a known-good process
    /// than be restarted under it.
    Off,
}

impl ReplacedMode {
    /// Read [`INIT_OPTION_KEY`] out of `initializationOptions`.
    ///
    /// Anything unrecognised — absent options, a non-object, a missing key, a
    /// wrong type, an unknown level — falls back to [`ReplacedMode::Exit`].
    /// Falling back to the *fallback* rather than to `Notify` matters: a
    /// typo'd option must not leave the server sending a notification into a
    /// client that has no handler for it, which would look exactly like the
    /// stale-server bug this ticket exists to fix.
    pub fn from_init_options(opts: Option<&serde_json::Value>) -> Self {
        let Some(raw) = opts.and_then(|opts| opts.get(INIT_OPTION_KEY)) else {
            return Self::Exit;
        };
        match raw.as_str() {
            Some("notify") => Self::Notify,
            Some("exit") => Self::Exit,
            Some("off") => Self::Off,
            _ => {
                tracing::warn!(
                    "ignoring unrecognised {INIT_OPTION_KEY} = {raw}; expected \"notify\", \"exit\" or \"off\""
                );
                Self::Exit
            }
        }
    }
}

/// The facts about a detected replacement, as sent to the client and logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BinaryReplaced {
    /// Absolute path whose contents changed under the running process.
    pub path: String,
    /// Version of the process that is *still running* — the stale one.
    ///
    /// The new binary's version is deliberately absent: reading it would mean
    /// executing an untrusted file we have not been asked to run. The client
    /// learns the new version by restarting us, which is the point.
    pub running_version: String,
    /// The stamp the server started from.
    pub previous: ExeStamp,
    /// The stamp now at the path.
    pub current: ExeStamp,
}

/// The action the adapter should take. Mirrors [`ReplacedMode`], minus `Off`,
/// which produces no action at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartSignal {
    /// Send [`ServerBinaryReplaced`] and keep serving.
    Notify(BinaryReplaced),
    /// Log and exit cleanly so the editor respawns us.
    ExitForRespawn(BinaryReplaced),
}

/// `openplanet/serverBinaryReplaced` — server → client, one-way.
///
/// Namespaced under `openplanet/` like every other custom method here. The
/// client opts in by sending `on_binary_replaced: "notify"` in
/// `initializationOptions`; a client that does not gets the clean-exit
/// fallback instead and never sees this method.
pub enum ServerBinaryReplaced {}

impl Notification for ServerBinaryReplaced {
    type Params = BinaryReplaced;
    const METHOD: &'static str = "openplanet/serverBinaryReplaced";
}

/// The polling state machine. See the module docs for the rules it encodes.
#[derive(Debug, Clone)]
pub struct BinaryWatch {
    path: PathBuf,
    running_version: String,
    mode: ReplacedMode,
    /// The identity we consider "ours". `None` until the first successful
    /// stamp, so a server started while its own path is briefly unstateable
    /// adopts a baseline later instead of treating the first sighting as a
    /// replacement.
    baseline: Option<ExeStamp>,
    /// A detected replacement waiting for the server to go idle.
    pending: Option<ExeStamp>,
    /// Latched once a signal has been produced. Terminal.
    signalled: bool,
}

impl BinaryWatch {
    pub fn new(
        path: PathBuf,
        running_version: String,
        mode: ReplacedMode,
        baseline: Option<ExeStamp>,
    ) -> Self {
        Self {
            path,
            running_version,
            mode,
            baseline,
            pending: None,
            signalled: false,
        }
    }

    /// The path being watched.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// True once a signal has been emitted; the watch is inert from then on.
    pub fn has_signalled(&self) -> bool {
        self.signalled
    }

    /// Feed one observation.
    ///
    /// `current` is the stamp at [`Self::path`] right now (`None` if it could
    /// not be stated) and `idle` is whether the server has no work in flight.
    /// Returns the action to take, at most once in the life of the watch.
    pub fn poll(&mut self, current: Option<ExeStamp>, idle: bool) -> Option<RestartSignal> {
        if self.signalled {
            // Latched. A further replacement is somebody else's problem;
            // re-signalling is how a restart loop starts.
            return None;
        }

        // A stamp we could not take tells us nothing, so this tick does
        // nothing at all — it neither starts a replacement nor releases one
        // already seen, and it never cancels one.
        //
        // BOTH halves matter, and the second is the one that is easy to miss:
        // a replacement detected earlier must not be acted on during a tick
        // that says the file is gone. Restarting toward a path with no binary
        // is a crash loop rather than an upgrade, and this is exactly the
        // instant a Windows swap passes through — the running image renamed
        // aside, its replacement not yet in place. Waiting costs one more
        // tick; getting it wrong costs the editor its language server.
        let Some(current) = current else {
            return None;
        };

        match &self.baseline {
            // First stamp we have ever managed to take: that is what we are
            // running, not evidence that it changed.
            None => self.baseline = Some(current),
            Some(baseline) if *baseline != current => self.pending = Some(current),
            Some(_) => {}
        }

        let pending = self.pending.clone()?;
        if !idle {
            // Something is in flight. Hold the observation; the client is
            // waiting on an answer we can still give.
            return None;
        }
        // `pending` is only ever set in the arm above, which requires a
        // baseline, so this cannot be `None` here.
        let previous = self.baseline.clone()?;

        let replaced = BinaryReplaced {
            path: self.path.display().to_string(),
            running_version: self.running_version.clone(),
            previous,
            current: pending,
        };
        let signal = match self.mode {
            ReplacedMode::Notify => RestartSignal::Notify(replaced),
            ReplacedMode::Exit => RestartSignal::ExitForRespawn(replaced),
            // Watching is disabled: keep observing (so `Off` costs nothing but
            // also hides nothing) and never act.
            ReplacedMode::Off => return None,
        };
        self.signalled = true;
        Some(signal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const VERSION: &str = "0.6.0";

    fn stamp(len: u64, mtime_ns: u64, inode: u64) -> ExeStamp {
        ExeStamp {
            len,
            mtime_ns: Some(mtime_ns),
            inode: Some(inode),
        }
    }

    /// The stamp a server would have started from, in every test below.
    fn baseline() -> ExeStamp {
        stamp(1_000, 500, 42)
    }

    fn watch(mode: ReplacedMode) -> BinaryWatch {
        BinaryWatch::new(
            PathBuf::from("/opt/bin/openplanet-lsp"),
            VERSION.to_string(),
            mode,
            Some(baseline()),
        )
    }

    fn temp_dir(label: &str) -> crate::testing::TempDir {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        crate::testing::TempDir::new(&format!(
            "openplanet-lsp-binwatch-{label}-{}",
            SEQ.fetch_add(1, Ordering::Relaxed)
        ))
    }

    // ---- negative control -------------------------------------------------

    #[test]
    fn unchanged_binary_never_signals_over_many_polls() {
        let mut watch = watch(ReplacedMode::Exit);
        for tick in 0..200 {
            assert_eq!(
                watch.poll(Some(baseline()), true),
                None,
                "an unchanged binary must never signal (tick {tick})"
            );
        }
        assert!(!watch.has_signalled());
    }

    // ---- the three independent identity fields ----------------------------
    //
    // One test per field, each changing ONLY that field, so a detector that
    // happens to compare just one of them cannot pass all three.

    #[test]
    fn mtime_only_change_signals() {
        let mut watch = watch(ReplacedMode::Exit);
        let changed = ExeStamp {
            mtime_ns: Some(999),
            ..baseline()
        };
        let signal = watch.poll(Some(changed.clone()), true);
        let Some(RestartSignal::ExitForRespawn(replaced)) = signal else {
            panic!("an mtime-only change is a replacement, got {signal:?}");
        };
        assert_eq!(replaced.previous, baseline());
        assert_eq!(replaced.current, changed);
    }

    #[test]
    fn length_only_change_signals() {
        let mut watch = watch(ReplacedMode::Exit);
        let changed = ExeStamp {
            len: 2_000,
            ..baseline()
        };
        assert!(
            watch.poll(Some(changed), true).is_some(),
            "a size change is a replacement"
        );
    }

    #[test]
    fn inode_only_change_signals() {
        let mut watch = watch(ReplacedMode::Exit);
        let changed = ExeStamp {
            inode: Some(43),
            ..baseline()
        };
        assert!(
            watch.poll(Some(changed), true).is_some(),
            "a same-size same-mtime rename swap is a replacement (unix)"
        );
    }

    #[test]
    fn losing_the_inode_field_is_still_a_change() {
        // A stamp taken where inodes exist compared against one where they do
        // not should not silently read as "unchanged".
        let mut watch = watch(ReplacedMode::Exit);
        let changed = ExeStamp {
            inode: None,
            ..baseline()
        };
        assert!(watch.poll(Some(changed), true).is_some());
    }

    // ---- exactly once -----------------------------------------------------

    #[test]
    fn replacement_signals_exactly_once_when_idle() {
        let mut watch = watch(ReplacedMode::Notify);
        let replacement = stamp(2_000, 900, 43);

        assert!(
            watch.poll(Some(replacement.clone()), true).is_some(),
            "the first idle poll after a replacement signals"
        );
        assert!(watch.has_signalled());

        for tick in 0..50 {
            assert_eq!(
                watch.poll(Some(replacement.clone()), true),
                None,
                "the signal must not repeat per poll (tick {tick})"
            );
        }
    }

    #[test]
    fn a_second_replacement_after_signalling_is_inert() {
        // Loop safety: if the new binary is itself replaced (a bad update
        // rolled back, say) we must not fire again and start a restart
        // ping-pong. Backoff is the client's job; not re-firing is ours.
        let mut watch = watch(ReplacedMode::Notify);
        assert!(watch.poll(Some(stamp(2_000, 900, 43)), true).is_some());
        for tick in 0..20 {
            assert_eq!(
                watch.poll(Some(stamp(3_000 + tick, 1_000 + tick, 44 + tick)), true),
                None,
                "a further replacement must never re-signal (tick {tick})"
            );
        }
    }

    // ---- deferral until idle ---------------------------------------------

    #[test]
    fn replacement_is_deferred_while_requests_are_in_flight() {
        let mut watch = watch(ReplacedMode::Notify);
        let replacement = stamp(2_000, 900, 43);

        for tick in 0..25 {
            assert_eq!(
                watch.poll(Some(replacement.clone()), false),
                None,
                "a busy server must not be restarted (tick {tick})"
            );
            assert!(!watch.has_signalled());
        }

        let signal = watch.poll(Some(replacement.clone()), true);
        let Some(RestartSignal::Notify(replaced)) = signal else {
            panic!("going idle must release the deferred signal, got {signal:?}");
        };
        assert_eq!(replaced.current, replacement);
        assert_eq!(replaced.previous, baseline());
    }

    #[test]
    fn a_change_while_deferred_reports_the_latest_stamp_and_still_signals_once() {
        let mut watch = watch(ReplacedMode::Notify);
        watch.poll(Some(stamp(2_000, 900, 43)), false);
        watch.poll(Some(stamp(3_000, 1_100, 44)), false);
        let latest = stamp(4_000, 1_200, 45);

        let signal = watch.poll(Some(latest.clone()), true);
        let Some(RestartSignal::Notify(replaced)) = signal else {
            panic!("expected one signal after the churn, got {signal:?}");
        };
        assert_eq!(
            replaced.current, latest,
            "the reported stamp is the newest seen, not the first"
        );
        assert_eq!(
            replaced.previous,
            baseline(),
            "the baseline stays the version we are actually running"
        );
        assert_eq!(watch.poll(Some(latest), true), None, "still only once");
    }

    #[test]
    fn returning_to_the_original_stamp_while_deferred_cancels_nothing() {
        // Whether this "should" cancel is a real question; pinning it. Once we
        // have observed a different binary we have no way to know the current
        // one is byte-identical to what we loaded, so we restart rather than
        // guess. Cheap when wrong, correct when right.
        let mut watch = watch(ReplacedMode::Exit);
        assert_eq!(watch.poll(Some(stamp(2_000, 900, 43)), false), None);
        assert!(
            watch.poll(Some(baseline()), true).is_some(),
            "a replacement already seen still restarts"
        );
    }

    // ---- absence ----------------------------------------------------------

    #[test]
    fn a_missing_binary_never_signals() {
        // The safety rule: exiting toward a path with no file is a crash loop,
        // not an upgrade.
        let mut watch = watch(ReplacedMode::Exit);
        for tick in 0..50 {
            assert_eq!(
                watch.poll(None, true),
                None,
                "an absent binary must never signal (tick {tick})"
            );
        }
        assert!(!watch.has_signalled());
    }

    #[test]
    fn a_missing_then_restored_binary_signals_on_the_new_identity() {
        let mut watch = watch(ReplacedMode::Exit);
        assert_eq!(watch.poll(None, true), None);
        assert_eq!(watch.poll(None, true), None);
        let restored = stamp(2_000, 900, 43);
        assert!(
            watch.poll(Some(restored), true).is_some(),
            "the reappearance with a new identity is the signal"
        );
    }

    #[test]
    fn deleted_then_recreated_identical_does_not_signal() {
        // Pinned behaviour: identity is the trigger, not existence. We never
        // observed the gap, and what is there now is what we are running.
        // (On unix a real recreation changes the inode, so this is the
        // forged-stamp case rather than a likely one — but the rule it pins
        // is what keeps a flaky `stat` from restarting the server.)
        let mut watch = watch(ReplacedMode::Exit);
        assert_eq!(watch.poll(None, true), None);
        assert_eq!(
            watch.poll(Some(baseline()), true),
            None,
            "a byte-identical file at the same identity is not a replacement"
        );
        assert!(!watch.has_signalled());
    }

    #[test]
    fn a_binary_that_vanishes_while_deferred_still_signals_when_it_returns() {
        let mut watch = watch(ReplacedMode::Exit);
        let replacement = stamp(2_000, 900, 43);
        assert_eq!(watch.poll(Some(replacement.clone()), false), None);
        assert_eq!(watch.poll(None, true), None, "nothing to restart into yet");
        assert!(watch.poll(Some(replacement), true).is_some());
    }

    // ---- baseline adoption ------------------------------------------------

    #[test]
    fn an_absent_baseline_is_adopted_from_the_first_sighting() {
        let mut watch = BinaryWatch::new(
            PathBuf::from("/opt/bin/openplanet-lsp"),
            VERSION.to_string(),
            ReplacedMode::Exit,
            None,
        );
        let first = stamp(1_000, 500, 42);
        assert_eq!(
            watch.poll(Some(first.clone()), true),
            None,
            "the first stamp we ever get is the baseline, not a replacement"
        );
        assert_eq!(watch.poll(Some(first), true), None);

        let replacement = stamp(2_000, 900, 43);
        let signal = watch.poll(Some(replacement.clone()), true);
        let Some(RestartSignal::ExitForRespawn(replaced)) = signal else {
            panic!("a change from the adopted baseline signals, got {signal:?}");
        };
        assert_eq!(replaced.previous, stamp(1_000, 500, 42));
        assert_eq!(replaced.current, replacement);
    }

    // ---- modes ------------------------------------------------------------

    #[test]
    fn each_mode_produces_its_own_action() {
        let replacement = stamp(2_000, 900, 43);

        let mut notify = watch(ReplacedMode::Notify);
        assert!(matches!(
            notify.poll(Some(replacement.clone()), true),
            Some(RestartSignal::Notify(_))
        ));

        let mut exit = watch(ReplacedMode::Exit);
        assert!(matches!(
            exit.poll(Some(replacement.clone()), true),
            Some(RestartSignal::ExitForRespawn(_))
        ));

        let mut off = watch(ReplacedMode::Off);
        for tick in 0..20 {
            assert_eq!(
                off.poll(Some(replacement.clone()), true),
                None,
                "an off watch signals nothing (tick {tick})"
            );
        }
        assert!(!off.has_signalled());
    }

    #[test]
    fn the_signal_names_the_running_version_and_path() {
        let mut watch = watch(ReplacedMode::Notify);
        let signal = watch.poll(Some(stamp(2_000, 900, 43)), true);
        let Some(RestartSignal::Notify(replaced)) = signal else {
            panic!("expected a notify signal, got {signal:?}");
        };
        assert_eq!(replaced.running_version, VERSION);
        assert_eq!(replaced.path, "/opt/bin/openplanet-lsp");
    }

    // ---- init-option parsing ----------------------------------------------

    #[test]
    fn mode_is_read_from_init_options() {
        let cases = [
            (serde_json::json!({"on_binary_replaced": "notify"}), ReplacedMode::Notify),
            (serde_json::json!({"on_binary_replaced": "exit"}), ReplacedMode::Exit),
            (serde_json::json!({"on_binary_replaced": "off"}), ReplacedMode::Off),
        ];
        for (opts, want) in cases {
            assert_eq!(
                ReplacedMode::from_init_options(Some(&opts)),
                want,
                "parsing {opts}"
            );
        }
    }

    #[test]
    fn unusable_init_options_fall_back_to_exit() {
        // Every one of these must land on the FALLBACK, never on Notify: a
        // client that never said it handles the notification must not be sent
        // one, or the server goes on serving stale answers forever.
        let cases = [
            serde_json::json!({}),
            serde_json::json!({"on_binary_replaced": "Notify"}),
            serde_json::json!({"on_binary_replaced": "restart"}),
            serde_json::json!({"on_binary_replaced": true}),
            serde_json::json!({"on_binary_replaced": 1}),
            serde_json::json!({"on_binary_replaced": null}),
            serde_json::json!({"on_binary_replaced": ["notify"]}),
            serde_json::json!({"onBinaryReplaced": "notify"}),
            serde_json::json!("notify"),
            serde_json::json!(null),
        ];
        for opts in cases {
            assert_eq!(
                ReplacedMode::from_init_options(Some(&opts)),
                ReplacedMode::Exit,
                "unusable options must fall back to Exit: {opts}"
            );
        }
        assert_eq!(ReplacedMode::from_init_options(None), ReplacedMode::Exit);
    }

    // ---- the wire contract the extension will code against ----------------

    #[test]
    fn the_notification_method_and_payload_shape_are_pinned() {
        assert_eq!(
            <ServerBinaryReplaced as Notification>::METHOD,
            "openplanet/serverBinaryReplaced"
        );

        let payload = BinaryReplaced {
            path: "/opt/bin/openplanet-lsp".into(),
            running_version: "0.6.0".into(),
            previous: stamp(1_000, 500, 42),
            current: stamp(2_000, 900, 43),
        };
        assert_eq!(
            serde_json::to_value(&payload).unwrap(),
            serde_json::json!({
                "path": "/opt/bin/openplanet-lsp",
                "runningVersion": "0.6.0",
                "previous": {"len": 1000, "mtimeNs": 500, "inode": 42},
                "current": {"len": 2000, "mtimeNs": 900, "inode": 43},
            }),
            "the extension codes against these camelCase keys"
        );
    }

    #[test]
    fn a_stamp_without_mtime_or_inode_still_serialises() {
        let payload = BinaryReplaced {
            path: r"C:\Program Files\openplanet-lsp.exe".into(),
            running_version: "0.6.0".into(),
            previous: ExeStamp {
                len: 1,
                mtime_ns: None,
                inode: None,
            },
            current: ExeStamp {
                len: 2,
                mtime_ns: None,
                inode: None,
            },
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["previous"]["mtimeNs"], serde_json::Value::Null);
        assert_eq!(json["current"]["inode"], serde_json::Value::Null);
        let round_tripped: BinaryReplaced = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped, payload);
    }

    // ---- the real filesystem, through the real swap shape -----------------

    #[test]
    fn stamping_a_missing_path_is_none() {
        let dir = temp_dir("missing");
        assert_eq!(ExeStamp::of_path(&dir.join("nope")), None);
    }

    #[test]
    fn a_rename_swap_changes_the_stamp_but_not_the_open_file() {
        // Reproduces `update::replace_executable`: stage a sidecar next to the
        // target, then rename it over the target. This is the observation the
        // whole watcher rests on, so it is checked against a real filesystem
        // rather than assumed.
        let dir = temp_dir("swap");
        let target = dir.join("openplanet-lsp");
        std::fs::write(&target, b"old binary").unwrap();

        let before = ExeStamp::of_path(&target).expect("target exists");

        // Hold the old file open, as a running process does.
        let held = std::fs::File::open(&target).unwrap();
        let held_stamp = ExeStamp::from_metadata(&held.metadata().unwrap());
        assert_eq!(held_stamp, before);

        let staging = dir.join(format!("openplanet-lsp.new.{}", std::process::id()));
        std::fs::write(&staging, b"a considerably newer binary").unwrap();
        std::fs::rename(&staging, &target).unwrap();

        let after = ExeStamp::of_path(&target).expect("target still exists");
        assert_ne!(
            after, before,
            "a rename swap must be visible in the PATH stamp"
        );

        // The point of stamping the path and not the handle: the open file is
        // untouched by the swap, so a process asking about itself sees
        // nothing.
        assert_eq!(
            ExeStamp::from_metadata(&held.metadata().unwrap()),
            before,
            "the running image is unchanged — this is why we stat the path"
        );

        // And the watcher agrees.
        let mut watch = BinaryWatch::new(
            target.clone(),
            VERSION.to_string(),
            ReplacedMode::Exit,
            Some(before),
        );
        assert!(
            watch.poll(ExeStamp::of_path(&target), true).is_some(),
            "the watcher must see a real rename swap"
        );
    }

    #[test]
    fn an_atomic_write_of_identical_bytes_is_still_a_replacement() {
        // A package manager that writes in place rather than renaming: the
        // length can be identical, so this leans on mtime/inode.
        let dir = temp_dir("inplace");
        let target = dir.join("openplanet-lsp");
        std::fs::write(&target, b"same-length-bytes").unwrap();
        let before = ExeStamp::of_path(&target).unwrap();

        // Re-create via rename so the inode definitely changes, the way an
        // installer's atomic write does.
        let staging = dir.join("staging");
        std::fs::write(&staging, b"same-length-bytes").unwrap();
        std::fs::rename(&staging, &target).unwrap();

        let after = ExeStamp::of_path(&target).unwrap();
        assert_eq!(after.len, before.len, "same size, by construction");
        assert_ne!(
            after, before,
            "identical contents at a new identity is still a new file"
        );
    }
}
