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

One lane, closing out, put the whole thing better than this introduction does:

> Every claim of mine that mattered and was wrong was wrong **in the direction
> of looking clean**, and each was caught by a check that could **observe
> failure** — never by re-reading the reasoning.

Both halves earn their keep. The errors are not randomly distributed: a broken
probe reports "nothing found", a crashed binary reports a lower total, a
suppressed stderr reports silence. They fail toward the answer you were hoping
for. And re-reading your own reasoning cannot catch them, because the reasoning
is usually correct — it is the instrument that is lying.

---

## Mechanism 1: the probe cannot see what it was asked about

The check runs, finds nothing, and reports "nothing" — which is
indistinguishable from "I am structurally incapable of seeing it".

- **`find <cargo-target> -maxdepth 2 -newermt '-6 hours' 2>/dev/null | head`**
  returned empty on a live, actively-written 4 GB build tree. Read as "stale
  scratch, safe to delete". It was another session's live reviewer target dir,
  deleted mid-run.

  **This entry originally blamed the wrong mechanism, and the correction is the
  more useful lesson.** It said cargo's files sit at depth 3+ so `-maxdepth 2`
  could not see them. That is true but *never reached*. Measured: `find` on this
  machine is not GNU findutils — it is a shell function from the Claude Code
  shell snapshot routing to **bfs 4.1.1**, and bfs **rejects** GNU-style
  relative timestamps outright:

  ```
  bfs: error: … -maxdepth 2 -newermt "-6 hours"
  bfs: error:                        ~~~~~~~~~~
  bfs: error: Invalid timestamp.
  ```

  Exit 1, on stderr, with a caret under the offending argument. **The traversal
  never started.** The tool diagnosed the mistake precisely and both channels it
  used were silenced: `2>/dev/null` discarded the message, `| head` discarded
  the status. Use `-mmin -N` for relative time under bfs — on a tree reporting
  "0 files newer than 10 days", `-mmin -30` found 732.

  Why the misdiagnosis mattered: anyone acting on the original entry would
  "fix" it to `-maxdepth 5 -newermt '-6 hours'` and get the same silent empty
  answer. **A wrong diagnosis that explains the symptom is worse than no
  diagnosis, because it closes the investigation.**

  Two consequences. This is a *second* instance of the pipeline-masking hazard
  below, not an independent story — which makes masking the highest-frequency
  hazard in this batch: two lanes, two tools, one shape. And **`2>/dev/null`
  deserves its own rule**, because every agent reflexively appends it to keep
  output tidy:

  > **Never suppress stderr on a command whose *absence of output* is the thing
  > you are about to act on.** It converts "I refused, and here is exactly why"
  > into "no results".

  Found by running the positive control this document prescribes, before a
  deletion — the document's own countermeasure catching the document's own
  error.
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

- **A shell pipeline hid a command's failure.** `bl` refuses to mutate the
  backlog from a git worktree, exiting 1 with a clear `ERROR:` line. Invoked as
  `bl bug --title "…" 2>&1 | tail -3` it appears to succeed, because a pipeline
  reports the exit status of its **last** command — `tail`'s. A lane filed a
  ticket that was never created and the command looked like it worked. Note the
  near-miss: the coordinator had been using that exact pattern all batch, and
  was saved only by running in the main checkout where the command succeeded
  anyway. Countermeasure: run mutating commands bare; if you must pipe, test
  `${PIPESTATUS[0]}` (bash) or `$pipestatus[1]` (zsh), never `$?`; and verify
  the artifact exists rather than trusting either. **Diagnose before filing:**
  the first read of this was "the tool exits 0 when it refuses", which would
  have been a defect in `bl`. `bl` was blameless.
- **A grep pattern that could never match.** A gate counted errors with
  `grep '"severity":"error"'` against a **pretty-printed** report, which writes
  `"severity": "error"` with a space. It matched zero on both sides and printed
  a confident `errors 0 -> 0` for every row — a perfect score produced by a
  pattern structurally incapable of matching anything. Fixed by parsing the
  JSON instead of pattern-matching its serialisation.

  **Two independent lanes wrote this same bug, and one of them wrote it
  twice.** A gate first counted `grep -c "error:"`, which matches nothing in
  JSON output, so every JSON corpus read as zero errors. The replacement
  grepped `"severity":"error"` — and missed for the *different* reason above.
  What caught the second attempt was a guard added while fixing the first:
  *warn when a corpus reports zero diagnostics.* Hence the rule, which is the
  most transferable thing in this document:

  > **When you fix a broken measurement, add the guard that would have caught
  > it — because your replacement is probably broken the same way.**

  A correction is written by the same person, in the same session, under the
  same wrong mental model that produced the original. It deserves more
  suspicion than the code that prompted it, not less.

  **Limit, found by a lane the day the rule was written.** The rule assumes a
  *witness* exists — some case in the data the broken measurement got wrong.
  Sometimes there is none. A collision test was tightened from comparing enum
  member names to comparing `name=number` pairs, which is strictly the correct
  equivalence; but no enum in the shipped databases differs *only* by number,
  so both versions return the same answer on every real input and **no standing
  assertion can tell them apart.** There is no guard to add.

  Two things follow, and they generalise past enums:

  - **When the corpus cannot discriminate two candidate rules, an assertion
    over it is testing the corpus, not the rule.** The honest artifact is a
    **mutation test that injects the discriminating case** — here, a duplicate
    enum with identical member names and every number shifted — plus a recorded
    note that the data contains no natural witness.
  - **Say so.** The lane's first report described the tightening as making the
    property "genuinely stronger than I had established", which implied the
    verification had changed. It had not: not one result moved. That is a
    correctness improvement with no evidentiary consequence, and calling it
    that is more useful than letting it read as new confirmation.

**Countermeasure — positive control.** Before trusting a probe that reports
absence, confirm it can see a thing you *know* is present. A staleness check
that cannot find a file you just touched is not reporting staleness. A counter
that reports zero should be shown a case it must count.

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
- **Cargo replayed cached diagnostics in a stale format.** A clippy comparison
  reused a base target dir from an earlier run that had used a *different*
  `--message-format`, so cargo replayed the cached diagnostics rather than
  re-emitting them. Rebuilt in empty dirs with identical flags, both sides
  agreed. This is the stale-target-dir hazard wearing a new coat — it is not
  only *staleness* that poisons a reused dir, it is any flag that changes what
  cargo caches.

  **A correction worth keeping, because getting it wrong is instructive.** The
  coordinator first attributed a *second* lane's clippy discrepancy to this
  same cache mechanism. It was not: that one was the epilogue-counting bug
  below. Two plausible mechanisms existed for one symptom, the first was
  reached for, and it fitted badly — it could not explain a third lane's
  stable, self-consistent `81/81`. **A hypothesis that explains your case but
  not the neighbouring case is not finished.**

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

  **But know how it fails.** A test binary that **SIGSEGVs** takes its whole
  suite with it, and the reported total silently *drops* rather than reporting
  a failure — 2190 and 2218 were observed against a true 2223. So a total that
  does not reconcile has two causes, not one: your tests are not running, or a
  binary crashed. Chasing the first when it was the second wastes the
  investigation. Check for a crashed binary before concluding anything about
  your tests. (Observed: `multi_root_tests`, stack `Backend::new` →
  `LspConfig::load` → `read_config_file` → toml_edit's recursive parser, under
  load with ~2G free RAM — a stack overflow in a recursive parser, not a defect
  in any branch.)

- **An intermittent failure will let you bisect it to any answer you like.**
  The same lane saw that crash three times in a row on its branch, once green
  on merge-base, and began bisecting its own change: disable half — passes;
  re-enable partially — passes; restore in full — **passes**. Each step was a
  single run of a flaky test, and single observations were being read as
  causality. Re-measured properly: 10/10 green, then 25 interleaved runs of
  both binaries, 0 failures each. **Before bisecting, establish the failure is
  deterministic.** A bisect over noise terminates and hands you a confident
  culprit.
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

  **This is now measured, not asserted.** On B060, two successive review passes
  each returned FAIL and each was right — four regressions the lane had
  introduced, every one reproduced against the merge-base binary. **None was
  visible to any gate.** The suite, `test-hermetic`, clippy and the
  false-positive fixture gate were all green on both rejected commits. The
  fixture gate in particular *could not* see them: the defect needed a plugin
  declaring a type named after a Core funcdef, and no fixture does. That is
  Mechanism 3 at the level of the whole strategy — the measurement this batch
  treats as most authoritative was structurally blind to the class of bug being
  introduced.

  The lesson is not that the gates are bad. It is that **a gate can only find
  what its corpus contains**, so a green gate bounds the blast radius on
  *existing* code and says nothing about a shape no fixture exercises. Reviews
  and hand-built probes cover a different space, and the two are not
  substitutes. When a change invents a new interaction, the corpus by
  definition has no case for it — that is the moment to build one, not to lean
  on the fixtures.
- **Test helpers own their temp dirs via `openplanet_lsp::testing::TempDir`,**
  whose `Drop` cleans up on the unwind path too — a panicking test is exactly
  when a dir gets left behind. Verify by running under a **private `TMPDIR`**
  and counting before/after. Never count a prefix in the shared `/tmp`: sibling
  lanes write it concurrently, so the number moves on its own. And never count
  `oplsp-*` specifically — those helpers clean up at end of test and leak only
  on red runs, so they read flat across two green ones and the check cannot see
  the defect (B065).

  This bullet previously prescribed exactly that broken check. It was written
  from the ticket's assumption rather than from a measurement, and the lane
  that had to use it proved it vacuous: `oplsp-*` read 26 → 25 → 25 while the
  real leak was 320 per run under eight unmentioned prefixes. **A countermeasure
  in this document is a claim like any other, and this one was wrong.**

- **A guard that drops too early turns a passing test into a vacuous one.**
  Migrating ~101 call sites to `TempDir`, helpers that returned a *derived*
  path (`dir/config.toml`) dropped the guard on the way out and deleted the
  tree before the caller looked at it. Eight tests went red — loudly, which is
  the lucky case. A test whose assertion cannot see its own fixture goes
  **green**. `TempPath` exists to carry the guard with the path. When a
  mechanical migration touches fixtures, the thing to hunt is not the tests
  that broke but the ones that now pass without testing anything.
