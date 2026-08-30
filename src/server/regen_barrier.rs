//! B064: a rendezvous that stops the clock inside a type-DB regeneration.
//!
//! Both defects B064 closes are *windows*, and a window you cannot stop the
//! clock inside can only be tested by racing it:
//!
//! * the window between `TypeIndex::load` parsing the dump and a second read
//!   of the same files taking its stamps (a filesystem TOCTOU, single
//!   threaded), and
//! * the window between publishing a new index and publishing its stamps
//!   (shared-state interleaving).
//!
//! [`wait_if_armed`] sits at ONE point in [`crate::server::Backend::regenerate_type_db`]:
//! the new index has been parsed and NOTHING has been published yet. That
//! point exists whatever shape the publish takes, which is what makes it a
//! fair test seam rather than a mould of today's implementation — a test that
//! pauses there and rewrites the dump proves whether the reported stamps came
//! out of the parse or out of a second read, and the answer does not depend on
//! how the publish is spelled.
//!
//! Disarmed in every normal path: [`wait_if_armed`] is one relaxed atomic load
//! and a return. Only a test ever calls [`arm`].
//!
//! Process-global, and deliberately so — `Backend::new` takes only a `Client`,
//! and threading a barrier handle through `build_service` would put test
//! plumbing in the one constructor `run_stdio` uses. Each integration-test
//! binary links its own copy of this library and runs in its own process, so
//! arming it in one test file cannot reach another. Within a file, hold
//! [`lock`] for the whole scenario: the barrier is one global rendezvous and
//! two tests using it at once would trade releases.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use tokio::sync::{Mutex, Notify};

static ARMED: AtomicBool = AtomicBool::new(false);

fn reached() -> &'static Notify {
    static REACHED: OnceLock<Notify> = OnceLock::new();
    REACHED.get_or_init(Notify::new)
}

fn released() -> &'static Notify {
    static RELEASED: OnceLock<Notify> = OnceLock::new();
    RELEASED.get_or_init(Notify::new)
}

/// Serialises whole barrier scenarios within one test binary.
pub fn lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Arm the barrier: the next regeneration to reach the publish point stops
/// there until [`release`]. Disarms when the returned guard drops.
///
/// A guard rather than a matching `disarm()` call because the thing that goes
/// wrong otherwise is not a leak but a HANG: an assertion that fires between
/// arming and disarming unwinds past the disarm, and the next test in the
/// binary to regenerate parks at a barrier nobody will ever release. One test
/// failing would take the rest of the binary with it, and the symptom — a
/// timeout, somewhere else — would say nothing about the cause. Unwinding runs
/// `Drop`, so the guard turns that into the single honest failure it should be.
#[must_use = "the barrier disarms when this guard drops"]
pub fn arm() -> Armed {
    ARMED.store(true, Ordering::SeqCst);
    Armed(())
}

/// Disarms the barrier on drop. See [`arm`].
pub struct Armed(());

impl Drop for Armed {
    fn drop(&mut self) {
        ARMED.store(false, Ordering::SeqCst);
    }
}

/// Production side. Returns immediately unless a test armed the barrier.
pub(crate) async fn wait_if_armed() {
    if !ARMED.load(Ordering::SeqCst) {
        return;
    }
    reached().notify_one();
    released().notified().await;
}

/// Test side: resolve once a regeneration has stopped at the publish point.
pub async fn wait_until_reached() {
    reached().notified().await;
}

/// Test side: let the paused regeneration publish.
pub fn release() {
    released().notify_one();
}
