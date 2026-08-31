//! B078 — the positive control for `scripts/test-suite.py`.
//!
//! A gate that has only ever been observed passing is not known to be a gate.
//! This binary is the deliberate crash that `just test-selftest` runs to prove
//! the suite gate goes red on a test binary killed mid-run.
//!
//! Why an abort rather than a panic. They are the two failure modes the gate
//! has to tell apart, and only one of them is dangerous:
//!
//! * A **panicking** test is visible. libtest catches the unwind, marks the
//!   test `FAILED`, and still prints `test result:` — the count is complete
//!   and the failure is in it.
//! * An **aborted** process is invisible. There is no summary at all, so its
//!   tests do not appear as failures; they simply are not in the total. cargo
//!   then stops, so every later binary is missing too. The number goes DOWN
//!   and nothing anywhere says "failed".
//!
//! The second is what a SIGSEGV does, and what made lane-b060-b062 report
//! 2190/2218 against a true 2223. So the control has to abort: a panicking
//! canary would still emit a summary and would prove the wrong thing.
//!
//! `#[ignore]`d so an ordinary `cargo test` never runs it — the binary still
//! reports a normal summary (`0 passed; 0 failed; 1 ignored`) and so satisfies
//! the very reconciliation it exists to test. Same shape as
//! `tests/hermeticity_canary.rs`.

/// Kills the process without a libtest summary. Run only by
/// `just test-selftest`, which asserts the gate rejects the resulting run
/// *because no summary was produced*, not merely because cargo exited
/// non-zero.
#[test]
#[ignore = "aborts the process on purpose — run by `just test-selftest`"]
fn crashes_without_reporting_a_result() {
    // Not `panic!`: libtest would catch it, print a summary, and the run would
    // stay countable — the opposite of the case under test. Not a null deref
    // either; abort is the portable way to leave no summary behind.
    std::process::abort();
}
