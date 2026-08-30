//! B064: a rendezvous that stops one type-DB regeneration at a time.
//!
//! Why test plumbing is in production code at all — read this before deleting
//! it. Both defects B064 closed are *windows*: one between parsing the dump and
//! a second read of the same two files, one between publishing an index and
//! publishing its stamps. A window you cannot stop the clock inside can only be
//! tested by racing it, and a racing test is a probabilistic one. This seam is
//! what makes the reproductions deterministic instead.
//!
//! [`wait_if_armed`] sits at ONE point in
//! [`crate::server::Backend::regenerate_type_db`]: the new index has been
//! parsed and NOTHING has been published yet. That point exists whatever shape
//! the publish takes, which is what makes it a fair test seam rather than a
//! mould of today's implementation.
//!
//! Disarmed in every normal path: [`wait_if_armed`] returns immediately unless
//! a test armed the barrier. An arm reserves exactly the next regeneration;
//! later concurrent regenerations do not wait behind it.
//!
//! Process-global, and deliberately so — `Backend::new` takes only a `Client`,
//! and threading a barrier handle through `build_service` would put test
//! plumbing in the one constructor `run_stdio` uses. Each integration-test
//! binary links its own copy of this library and runs in its own process, so
//! arming it in one test file cannot reach another. Within a file, hold
//! [`lock`] for the whole scenario: the barrier is one global rendezvous and
//! two tests using it at once would trade generations.

use std::sync::{Mutex as StdMutex, MutexGuard, OnceLock};

use tokio::sync::{Mutex, Notify};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Armed(u64),
    Waiting(u64),
}

struct BarrierState {
    next_generation: u64,
    phase: Phase,
}

fn state() -> &'static StdMutex<BarrierState> {
    static STATE: OnceLock<StdMutex<BarrierState>> = OnceLock::new();
    STATE.get_or_init(|| {
        StdMutex::new(BarrierState {
            next_generation: 0,
            phase: Phase::Idle,
        })
    })
}

/// Keep the test seam useful after a panic: a panic while manipulating the
/// state must not poison every later test in this integration-test process.
fn lock_state() -> MutexGuard<'static, BarrierState> {
    state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

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

/// Arm the barrier for exactly the next regeneration.
///
/// The returned guard controls only its own generation. Releasing or dropping
/// a stale guard cannot release a later arm; dropping the live guard both
/// disarms an unreached generation and wakes a generation already waiting.
#[must_use = "the barrier disarms and releases its generation when this guard drops"]
pub fn arm() -> Armed {
    let generation = {
        let mut state = lock_state();
        assert!(
            matches!(state.phase, Phase::Idle),
            "a regeneration barrier is already armed or waiting"
        );
        state.next_generation = state.next_generation.wrapping_add(1);
        let generation = state.next_generation;
        state.phase = Phase::Armed(generation);
        generation
    };
    Armed { generation }
}

/// A generation-specific barrier arm. See [`arm`].
pub struct Armed {
    generation: u64,
}

impl Armed {
    /// Wait until this arm's regeneration has reached the publish seam.
    pub async fn wait_until_reached(&self) {
        loop {
            let reached = reached().notified();
            tokio::pin!(reached);
            reached.as_mut().enable();

            match lock_state().phase {
                Phase::Waiting(generation) if generation == self.generation => return,
                Phase::Armed(generation) if generation == self.generation => {}
                phase => panic!(
                    "barrier generation {} was cancelled or replaced before it reached the seam: {phase:?}",
                    self.generation
                ),
            }

            reached.await;
        }
    }

    /// Release this arm's generation, if it is still armed or waiting.
    ///
    /// The state check makes a stale guard inert. `notify_waiters` follows the
    /// state change; waiters first register with `enable`, then check this
    /// state, so neither ordering can lose a release.
    pub fn release(&self) {
        let released_generation = {
            let mut state = lock_state();
            match state.phase {
                Phase::Armed(generation) | Phase::Waiting(generation)
                    if generation == self.generation =>
                {
                    state.phase = Phase::Idle;
                    true
                }
                _ => false,
            }
        };
        if released_generation {
            released().notify_waiters();
        }
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        self.release();
    }
}

/// Production side. Returns immediately unless a test armed the barrier.
pub(crate) async fn wait_if_armed() {
    let generation = {
        let mut state = lock_state();
        match state.phase {
            Phase::Armed(generation) => {
                state.phase = Phase::Waiting(generation);
                generation
            }
            Phase::Idle | Phase::Waiting(_) => return,
        }
    };
    reached().notify_waiters();

    loop {
        let released = released().notified();
        tokio::pin!(released);
        released.as_mut().enable();

        if !matches!(lock_state().phase, Phase::Waiting(waiting) if waiting == generation) {
            return;
        }

        released.await;
    }
}
