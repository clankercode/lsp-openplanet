# Verification hazards: checks that return confident, self-consistent, wrong answers

Agent-facing. Not user documentation.

This is the transferable finding from the B029-B068 bug batch (2026-08-29 to 08-31).
It grows as new instances turn up, so it carries no running total — the point
is the three shapes, not how many stories are filed under them.

Across that batch, the recurring cost was never a check that *errored* — errors
get investigated. It was checks that returned a clean, plausible, internally
consistent answer that happened to be false. Every one of them cost more time
than the bug it was hiding.

They fall into three mechanisms. Learn the shapes, not the stories.

---

## Mechanism 1: the probe cannot see what it was asked about

The check runs, finds nothing, and reports "nothing" — which is
indistinguishable from "I am structurally incapable of seeing it".

- **`find <cargo-target> -maxdepth 2 -newermt '-6 hours'`** returned empty on a
  live, actively-written 4 GB build tree, because cargo's files sit at depth 3+.
  Read as "stale scratch, safe to delete". It was another session's live
  reviewer target dir, deleted mid-run.
- **`pgrep -f <pattern>`** matched its own command line, so "is that process
  running?" answered yes when only the check itself was running.
- **The probe tool reported diagnostics from other installed plugins**, so an
  ACCEPT could leak from code that was not under test. (See
  `probes/README.md` — a game REJECT is evidence, an ACCEPT may not be.)
- **A batched probe reported a name the coordinator had invented as ACCEPTED
  by the game.** `probe_enum_corpus.py` compiles a whole batch as one unit and
  records ACCEPTED for any line that drew no diagnostic; a parse error anywhere
  aborts the unit before semantic analysis, so *every other name* comes back
  ACCEPTED. Filed as **B068**. This one is worth singling out: it happened
  about an hour after this document was written, to its author, and it was
  caught **only** because a negative control was run — the fabricated name
  `ZZZNoSuchClass::` came back accepted, which is impossible, and that single
  impossible row invalidated a run that otherwise looked complete and would
  have "disproved" an established rule (B056).

**Countermeasure — positive control.** Before trusting a probe that reports
absence, confirm it can see a thing you *know* is present. A staleness check
that cannot find a file you just touched is not reporting staleness.

**And a negative control.** Include an input whose answer you are certain of
and which should come back negative. If your fabricated, impossible input comes
back positive, the run is void — that is not a surprising result to reason
about, it is a broken instrument. Writing this rule down did not stop its author
from needing it; running it is what worked.

**Second countermeasure — implausibility is a signal.** 4 GB that nobody has
touched in six hours should read as implausible. Implausible is a reason to
re-measure, not to proceed. The most expensive moment in this batch was a
suspicious signal that had a ready innocent explanation available.

---

## Mechanism 2: the environment lied, and the tooling reported success anyway

- **Disk exhaustion, three times.** A full `/tmp` does not fail cleanly here: it
  produced 44 spurious `StorageFull` test failures in one lane, and silently
  **lost** another lane's command output — no error, just absent results.
- **A green measured in the wrong worktree.** `cd` at the start of a compound
  command does not carry to the next bash invocation. This produced a clippy run
  comparing master to master, and a "base" binary built from the branch source.
  It happened four times.
- **A stale base binary.** A comparison gate reported the base and branch
  binaries byte-identical. Plausible — the source delta was comment-only. The
  real cause was building the base into a target dir that already held branch
  artifacts; cargo's `Finished in 0.15s` was reuse, not a build.
- **A build racing a rebase**, so the artifact under test was not the tree
  under review.

**Countermeasures.** Echo `pwd` immediately before any gate, and bind
`cd <abs-path> && <cmd>` into a single invocation. Build comparison baselines
in a **clean target dir** — a fast "build" is a claim to check, not a saving.
Keep build/target dirs on `/home`
(`/home/xertrov/.cache/lsp-openplanet-builds/<lane>/`), never on `/tmp` and
never inside a session scratchpad (see Conventions below).

---

## Mechanism 3: the oracle cannot discriminate the thing it is asserting

The test passes, and would have passed either way.

- **`tests/fixtures/typedb` is byte-identical to the real `~/OpenplanetNext`** —
  same md5 on `OpenplanetCore.json`, `OpenplanetNext.json`, and `Openplanet.h`,
  because the fixture is generated from a real install. So **no assertion on
  payload content can tell you which database loaded.** B066 swaps one install
  for another and every content assertion is blind to it by construction. The
  only thing that keeps a real install out of a test is an explicit
  `openplanet_dir` pin, and the only thing that proves the pin works is
  `just test-hermetic` under an isolated HOME. That makes B045's hermeticity
  gate load-bearing, not a nice-to-have.
- **The same blindness produced a second, independent defect the same day:** a
  lane asserted a symbol count believing it discriminated fixture from real
  install, while its fixture copy silently omitted the `Openplanet.h` that
  `TypeIndex::load` folds in from beside the game database. A quietly smaller
  database, accepted by a `> 0` assertion.
- **A comparison harness diffed against files it never wrote**, comparing two
  copies of the same thing.
- **A gate counted the wrong population.** B065's specified gate was "count
  `/tmp/oplsp-*` across two runs and assert it does not grow". Those helpers
  clean up at end of test and leak only on the unwind path, so the count reads
  24 → 24 → 24 on green runs — while ~320 dirs per run land under eight *other*
  prefixes nobody was watching. The gate would have passed, on a defect
  producing 141,000 directories. The population was chosen by name, from the
  ticket's assumption, rather than by measurement.
- **The measurement was taken on shared state.** A raw `/tmp` count could not
  attribute anything: sibling lanes run the same suite into the same `/tmp`, and
  two snapshots minutes apart moved 142,041 → 143,835 with no run by the
  measuring process at all. Fixed by re-running under an isolated `TMPDIR`,
  which turns an un-attributable diff into an exact one that starts at 0 and
  must return to 0.

**Countermeasures.**

- **Choose the population by measurement, not by name.** Before trusting a
  count-based gate, confirm the number *moves* on a known-bad case. A baseline
  that reads identically before and after a run is not a passing gate, it is an
  instrument pointed somewhere else.
- **Never measure on state other processes write.** If the thing you count
  lives in a shared location, isolate it first (`TMPDIR`, a private directory,
  a container) or your before/after diff measures your neighbours.
- **Count reconciliation.** After adding tests, check the suite total moved by
  the number of tests you added. A green suite whose total did not move means
  your tests are not running. This is cheap and it has caught real problems.
- **Mutation proof.** Break the thing on purpose and confirm the gate goes red.
  A gate never observed failing is not known to be a gate.
- **Anti-vacuity guards.** A loop over an empty collection passes. Assert the
  collection is non-empty, in the same test, with a message saying the gate
  would otherwise be vacuous (see `tests/execute_command_tests.rs`).
- **Ask of every new test: would this still pass if only half the fix were
  applied?** If yes, it does not pin what it claims to.

---

## Conventions this batch established

- **Reviewer and lane target dirs live under
  `/home/xertrov/.cache/lsp-openplanet-builds/<lane>/`.** Never under a session
  scratchpad. Session scratchpads are single-owner; a path that looks like
  disposable scratch to one agent may be another's live build root. Pinning a
  reviewer's `CARGO_TARGET_DIR` inside another session's scratchpad caused a
  cross-lane deletion incident on 2026-08-30.
- **An adversarial review is not a formality to run in parallel with the merge.**
  It is the only check that reads intent rather than behaviour. Merging while it
  runs has twice landed real defects on master that the review then found.
- **Test helpers own their temp dirs via `Drop`,** so cleanup happens on the
  unwind path — a panicking test is exactly when a dir gets left behind. Verify
  by counting `/tmp/oplsp-*` across two consecutive runs; a `Drop` impl that
  reads correctly and never fires is the failure mode (B065).
