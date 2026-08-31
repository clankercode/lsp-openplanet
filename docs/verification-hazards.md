# Verification hazards: checks that return confident, self-consistent, wrong answers

Agent-facing. Not user documentation.

This is the transferable finding from the B029-B068 bug batch (2026-08-29 to 08-31).
It grows as new instances turn up, so it carries no running total — the point
is the three shapes, not how many stories are filed under them.

Across that batch, the recurring cost was never a check that *errored* — errors
get investigated. It was checks that returned a clean, plausible, internally
consistent answer that happened to be false. Every hidden defect cost more
time than the bug it was hiding.

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

- **A connectivity check that never opened a socket.** `(exec 3<>/dev/tcp/127.0.0.1/30000) || echo closed`
  reported the game's RemoteBuild port CLOSED while it was serving compiles.
  `/dev/tcp` is a **bash builtin, not a kernel feature**, and this session's
  shell is zsh — the redirection simply fails, so the `||` branch fires for a
  check that never tested anything. A `socket.connect` from python said OPEN.
  Worse than the `find`/bfs case below, and in the same family: bfs *errored*,
  so its traversal visibly never ran, whereas this returned a plausible,
  confident answer **in the direction of "nothing is there"** — which is the
  answer that stops you looking. Caught only because a log showed the game
  alive seven minutes earlier, making "closed" implausible. The lane that hit
  it had written the `find`/bfs correction hours before: **knowing a hazard by
  name does not protect you from it; only a control does.** Its countermeasure
  is the right one — confirm the method against a port you know is open
  (`:22`) before believing a negative.

- **A glob that matched nothing aborted the gate, and the exit code belonged to
  the wrong command.** `rm -f .../sha<SHA>-*.log && cargo test ...` — the glob
  matched nothing, zsh's `nomatch` aborted the `&&` chain, and **cargo test
  never ran**. The captured `TEST_EXIT=1` was the failed `rm`. A careless read
  reports a red suite that was never measured; a slightly less careless one
  re-runs and reports the second attempt without noticing the first never
  happened. This is pipeline masking with the polarity reversed — not a failure
  hidden by a later success, but a *success never attempted* wearing a failure's
  exit code.

- **A backup that never happened, guarding a destructive edit.** About to
  rewrite a ticket's title in place, I did the responsible thing first:
  `F=.backlog/bugs/B078*.todo; cp $F "$SD/B078.todo.bak"`. **zsh does not
  glob unquoted variable expansions** — bash does, which is why the habit
  exists — so `cp` was handed the literal string `B078*.todo` and failed. The
  very next line printed `backup: $(wc -c < ...) bytes`, which rendered as
  `backup:  bytes`, and the step after that was a scripted `sed -i` on the file
  the backup was meant to protect.

  **The near-miss is the point.** Every individual habit here was correct: back
  up before editing, keep the backup outside the repo, print what you did. The
  hazard is that the *safety* step is exactly as susceptible to silent failure
  as the dangerous one, and it fails first and quietly, leaving the dangerous
  step fully armed. A guard that runs before the thing it guards will, when it
  breaks, break invisibly.

  What actually caught it was printing the backup's **byte count** rather than
  a success message. `backup:  bytes` with the number missing is unmistakable;
  `echo "backup done"` after a failed `cp` would have been a confident lie.
  **A backup step whose success you do not measure is not a backup — it is a
  sentence about a backup.** Same shape as the empty-md5 and empty-awk
  vacuities elsewhere in this document: the absence rendered as blank rather
  than as an error, and blank reads as fine.

- **Four shell divergences, one cause.** `/dev/tcp` (bash builtin, absent in
  zsh), `nomatch` aborting an `&&` chain, `$rev:s` eaten as a history modifier,
  and now glob-in-variable. Each was diagnosed separately and each cost a real
  investigation. The common factor is not any zsh feature: it is that **I write
  bash by reflex into a zsh shell**, and zsh's divergences overwhelmingly
  produce *plausible wrong answers* rather than errors — a closed port, an
  empty diff, a blank byte count, a skipped test wearing someone else's exit
  code. An error would be a gift; what you get instead is a clean-looking
  result.

  The countermeasure is not "learn zsh". It is that a shell one-liner is a
  **measurement instrument**, and instruments get calibrated before they get
  trusted: check the method against a case whose answer you already know
  (`:22` for the port, a file you know exists for the glob), and prefer
  python for anything whose failure would be silent. Four instances is enough
  to stop treating each as a surprise.

- **Two lanes, two clippy baselines, both correct, and comparing them would
  have invented a regression.** One lane reported clippy **77** on master; a
  second reported **129** on a master differing only by docs. Neither number is
  wrong. The first counted unique diagnostics
  (`--message-format=json | jq | sort -u`); the second counted raw warnings,
  which include the same diagnostic reported once per target (lib, test, each
  integration binary). 129 raw against 77 unique is the expected shape.

  The hazard is not the numbers. It is that **a baseline is meaningless without
  its method attached**, and the method was never stated by either lane —
  because to each of them it was obvious. Merge the two branches in sequence
  and the next lane inherits whichever baseline it happens to read, then
  reports a 52-warning regression or a 52-warning improvement, neither of which
  happened. It would then hunt for a cause that does not exist, and the hunt
  would look completely reasonable.

  **A count is a measurement, and a measurement without its instrument is not
  comparable to anything.** This is the counterpart to the "equal totals do not
  prove equal content" rule elsewhere in this document, and it runs the other
  way: **unequal totals do not prove a difference either.** Both directions
  have the same fix, which is to compare by SET DIFF — which diagnostics
  appeared, which disappeared — rather than by total. A set diff is immune to
  the instrument, because a duplicate reported twice under one method and once
  under another is the same member of the set either way.

  **But a set diff keyed on line numbers manufactures its own phantom
  regression, and following the advice above literally produces one.** The lane
  that resolved this ran the set diff as instructed and got 5 branch-only and 5
  master-only rows. They were **the same five** pre-existing warnings, shifted
  down a few lines because the branch had inserted code above them. Any
  diagnostic below an insertion point moves, so every one of them reads as
  "removed at the old line, added at the new". Key the comparison on
  `(file, column, message)` with line numbers normalised out; then both diffs
  came back empty against 57 unique warnings a side. **A diff is only as
  meaningful as the identity you diff on, and line numbers are not identity —
  they are position, and position is exactly what an unrelated edit changes.**

  Caught here only because one coordinator happened to read two lanes' reports
  in the same sitting. **That is not a control, it is a coincidence** — the
  general fix is that any number handed across a lane boundary carries the
  command that produced it.

- **A private predicate behind a public wrapper that embeds it is not
  private.** Designing the fix for B079 (callers reaching for the wrong member
  of a set of near-identical predicates), the obvious move was to make the
  dangerous one, `TypeIndex::scope_exists`, `pub(crate)` so distant modules
  could not reach it. A lane measured the callers before doing it and found
  that **the move would not have prevented the defect it was designed to
  prevent**: hover never called `scope_exists` at all. It reached the same
  index-only scope test *through* `source_reference_key`, which is separately
  public and embeds it.

  So the encapsulation boundary was drawn around the *name* rather than around
  the *capability*, and a second public function handed that capability out
  from behind it. Both had to be demoted; demoting either alone accomplishes
  nothing.

  **Transferable: when you restrict access to a function, enumerate what else
  exposes its behaviour, not just what else calls it by name.** A wrapper, a
  convenience helper, a `Deref`, a trait impl, or a public field can each
  re-export a capability you believe you have just closed off. The check is
  cheap — ask which other public entry points produce the same observable
  effect — and skipping it yields a fix that looks structural, reviews well,
  and changes nothing.

  Note the shape it shares with the rest of this document: the failure would
  have been **silent and congratulatory**. The demotion compiles, the tests
  pass, the diff reads as real hardening, and the hole stays open. Nothing
  would have contradicted it. It was caught only because the lane measured the
  call sites instead of reasoning about them — the same move that killed three
  wrong diagnoses in B078.

- **The tool written to prevent contamination contaminated the live directory,
  while being written.** B077's fix exists because an interrupted probe run
  strands a `.as` fixture in the game's `Plugins` directory, where it silently
  contaminates every later ACCEPT. The lane's first selftest passed a
  `plugins_dir` to the staging class — but the staging call always wrote to
  `fleet.OPN_PLUGINS`, so the test staged into the **real**
  `~/OpenplanetNext/Plugins` and left three fixtures there, two of them
  declaring `namespace ZZZProbeOwnedNamespace`.

  A stranded, namespace-declaring fixture in the live plugins directory,
  created by the tooling written to prevent exactly that, during the writing of
  it. **The strongest possible evidence that a ticket was worth filing is the
  ticket's own defect reproducing itself in the hands of the person fixing it.**

  **What caught it is the part to copy.** The selftest asserted the staged file
  existed *at the path it had asked for* — an assertion about **location**, not
  merely existence — and the file was not there. The lane had written that
  assertion for tidiness, not as a trap. Had it asserted only "the file exists
  somewhere" or "the context manager returned without raising", the test would
  have **passed while contaminating the game**, and the fix would have shipped
  with a live-directory leak inside its own test suite.

  Generalise it: **assert the value, not the presence.** "A file was created"
  and "a file was created *where I asked*" cost the same to write and differ
  completely in what they can catch. The same holds for "a config was loaded"
  vs "loaded from the path I specified", and "the probe returned a verdict" vs
  "returned a verdict for the name I asked about". Every vacuity in this
  document is some form of a presence check standing in for a value check.

- **`try/finally` is not cleanup, and four separate holes prove it.** The
  obvious shape for the same fix — wrap the staging in `try/finally` — leaves
  all of these open, each demonstrated by a test that fails without its
  specific remedy:

  - `dest = stage(...)` **binds only on success**, so a stage that dies partway
    leaves a half-populated directory the `finally` never learns the name of.
    That is precisely the case that strands a file.
  - `__exit__` is **not called when `__enter__` raises**, so moving the lifetime
    into a context manager does not by itself close that hole; `__enter__` must
    clean up after itself.
  - `try/finally` does **not** run on SIGTERM — the process terminates without
    unwinding, so a `kill`, a session teardown, or an OOM leaves the fixture
    installed. Not hypothetical: this session died twice in one day.
  - Nothing **refused to measure beside contamination**. Cleanup is not enough
    on its own; a probe that finds foreign state must decline to produce a
    verdict rather than emit one it cannot stand behind.

  The layering that follows is worth stating as a rule: **the lifetime is the
  mechanism, the sweep is only the backstop.** The sweep exists for SIGKILL and
  power loss, which nothing in-process can catch — and a later run tidying up
  is not protection, because *the later run is the one being contaminated*. The
  lane put that reasoning in the sweep's own docstring so the next reader
  cannot mistake the backstop for the mechanism.

- **A guard that survives every mutation is not strong; it is unmeasured.** In
  the same work, an `atexit` hook survived every mutation the lane applied,
  which meant it was either unreachable or untested. The lane's response —
  *"I don't ship a guard no test can make fire"* — is the right one, and note
  that both available conclusions are actionable: if unreachable, delete it; if
  untested, construct the case. It found the hook reachable (an `__enter__`
  without a `with`, then a normal exit), built the selftest to produce that
  path, and thereby also established that the hook and the signal handlers are
  **not** redundant, since that path is the one the handlers cannot see.

  A mutation that nothing detects is usually read as "that code is fine". It
  means the opposite: **no test is looking at it.**

- **Twice in one day, a lane committed the exact defect it was fixing, while
  fixing it.** Not a coincidence, and worth naming as a pattern rather than
  filed as two anecdotes:

  - Writing the guard against stranded probe fixtures, a lane **stranded three
    probe fixtures in the live game directory** — two of them declaring a
    namespace, which is the contaminating shape the ticket exists to prevent.
  - Designing the fix for "callers reach for the wrong member of a set of
    near-identical predicates", a lane proposed a `pub(crate)` demotion that
    **closes nothing**, because these are sibling modules in a single crate.
    It had, hours earlier, produced the finding that the same fix's boundary
    was drawn around the *name* rather than the *capability* — and then drew
    it around the name again, one level up.

  A third, from the same batch: a coordinator who had just written "never
  suppress stderr on a probe you intend to believe" added `2>/dev/null` to a
  probe and believed its output.

  **The mechanism is not carelessness — it is that the fix for a class of error
  is written by someone holding that class of error in their head, which is
  also the state in which they are most likely to commit it.** Attention spent
  on the abstraction is attention not spent on the instance. This is why "the
  author understands the hazard" is worth nothing as a control, and why the
  countermeasures that actually worked in all three cases were mechanical and
  external: an assertion about a path rather than existence, checking whether a
  keyword binds before building on it, and a contradiction visible in the same
  output.

  Practical consequence: **the moment you are most confident about a hazard is
  the moment to add a control against it, not to rely on knowing it.** Every
  instance above was caught by something written down, never by the
  understanding that had just been demonstrated in prose.

- **A refactor inverted a cleanup ordering that the original had right, and
  `ignore_errors=True` hid the consequence.** Master's probe unloaded the
  plugin from the game and *then* removed its directory. Wrapping the probe
  body in a `with` block moved removal into the context manager's exit — which
  runs **before** the inner unload — so the code removed a directory the game
  still held open. Paired with `shutil.rmtree(ignore_errors=True)`, a removal
  that failed because of those open handles was **indistinguishable from one
  that succeeded**. The first version of the fix could therefore produce the
  exact stranded fixture the ticket exists to prevent, silently.

  Three separate lessons, and they compound:

  - **`with` / RAII changes *when* cleanup runs, not just whether it runs.**
    Moving an operation into a context manager reorders it against every other
    teardown in the function. Scope exit is reverse order of acquisition, which
    is correct for resources and wrong whenever teardown has an external
    dependency ordering — here, "tell the game to let go" must precede "delete
    the files".
  - **`ignore_errors=True` is not error handling; it is the deletion of
    evidence.** It converts a failed destructive operation into a successful-
    looking one, which is the single worst direction for a failure to point.
    The fix retries, verifies, and **downgrades the probe to `skipped` rather
    than reporting a verdict it cannot stand behind** — a cleanup failure that
    is merely logged is not acted upon.
  - **The original was right and the improvement broke it.** Refactoring for
    tidiness silently traded a correctness property for a structural one. When
    restructuring code that already works, the ordering it happens to have may
    be load-bearing and undocumented — which is an argument for a comment at
    the time you discover why, not for never refactoring.

- **Prefix matching is not an ownership test.** The same sweep deleted
  directories matching `lsp-issue-repro-*`. A real third-party plugin happening
  to carry that name would have been destroyed — and the sweep ran *before*
  argument validation, so an invocation with no fixture at all deleted things
  on its way to an arg error.

  **Deleting someone else's data is worse than refusing to run**, so the rule
  became: removal requires an explicit `.lsp-probe-owned` marker, or an empty
  directory (which declares nothing and is the observed shape of the strays
  this ticket found). Anything else is left in place with a warning that names
  it.

  The marker also carries the owning **pid**, and a directory owned by a live
  process is never swept — deleting it would corrupt that run's measurement,
  which is the same damage the ticket is about, pointed the other way. **A
  cleanup mechanism is itself a contamination mechanism when it runs beside
  something it does not know about.**

  General form: **a name is a hint, not a title deed.** Any destructive sweep
  keyed on a naming convention will eventually meet something that matches by
  coincidence, and the failure is unrecoverable and belongs to someone else.
  Require a positive ownership claim the sweeper itself wrote.

- **Review value scales with blast radius, not with change size.** Earlier in
  this batch a killed review was deliberately *not* re-run, on the reasoning
  that the outstanding question was better answered by an enforcement scan than
  by a reviewer. That was right for a config-loading change with strong measured
  evidence and no destructive operations. It would have been badly wrong here:
  this change deletes directories in a live application's plugin folder, and an
  independent review returned FAIL with four blocking findings, two of which
  could destroy a user's unrelated data. **The question is never "is this
  change big?" but "what is the worst thing this code can do to state it does
  not own?"**

- **The equal-totals hazard fired for real, and the numbers collided by
  coincidence rather than by staleness.** A lane measured master's baseline
  instead of deriving it, and master came back at **2341 — identical to its own
  branch total** at that moment. Reading that as "no change, therefore
  agreement" would have been completely wrong: master had moved twice during
  the gate, and a merge landed underneath adding **exactly 11 tests** while the
  branch adds **exactly 11**. Two independent +11s.

  This is the sharper form of the recorded rule. The earlier instance was equal
  totals over *stale* content; this one is equal totals over genuinely
  different content, where the collision is arithmetic luck. No amount of care
  about freshness would have caught it.

  **What saved it was measuring the base rather than deriving it from an
  earlier reported number.** The prediction "master must be 2330" was correct
  about the commit it was formed on and useless about the commit the lane
  actually had. A baseline is a property of a specific tree, not a fact about
  the project, and it expires the moment anything merges.

- **`git checkout -- <file>` on uncommitted work is destructive with no undo,
  and it silently produced a vacuous measurement several steps later.**
  Building a half-fix variant matrix, a lane reverted each variant with
  `git checkout -- <file>`. The work was **uncommitted**, so the revert went to
  HEAD and destroyed the entire fix — implementation and all 11 tests.

  The rework was not the expensive part. The next variant's patch script
  targeted an anchor that no longer existed, so **variant 3 was never applied
  and the run reported "all green"** — which would have been written into the
  matrix as "no test pins the parent argument", the exact opposite of the truth
  (four do). **An unapplied variant fails green, and green is the answer you
  were hoping for.**

  Two rules, both cheap:

  - **Commit before building half-fix variants.** The revert step is
    destructive by construction, and a variant matrix runs it repeatedly.
  - **Assert that each variant actually applied.** The lane's patch script
    asserted `count == 1` on its anchor and threw, which is the only reason
    this was caught. A variant that no-ops discharges the obligation without
    doing the work — the same shape as a test comment claiming more than the
    test pins.

  Note the compounding: a destructive mistake produced a **silent, plausible,
  wrong measurement** several steps downstream, pointing the opposite way from
  the truth. The deleted files were never the damage.

- **A relayed attribution acquires the relayer's authority without acquiring
  their verification.** One lane reported a counting hazard and attributed the
  observed figures to a second lane's gate: "that is what produced
  lane-X's 2190/2218 against a true 2223". The numbers were real. **The
  attribution was not**, and the coordinator wrote it into two tickets
  unchecked.

  The accused lane checked its own records and corrected it: its reported
  totals were 2330/2331, and it had reported binary counts. The figures came
  from the first lane's own crash investigation — and **the original record,
  written hours earlier, names no lane at all.** It was in the same ticket the
  coordinator was editing, a couple of hundred lines above the text being
  added.

  Three things make this worth recording rather than filing as a slip:

  - **The measurement and its provenance travel together and are verified
    separately.** "2190 against 2223" was checkable and true; "lane-X produced
    it" was checkable and false. Accepting the first is no reason to accept the
    second, and they arrive in one sentence.
  - **Relaying launders confidence.** The second lane had no way to know the
    claim was unverified once it appeared in a ticket under the coordinator's
    name. A first-hand report carries its author's uncertainty; a relayed one
    usually loses it.
  - **It was an accusation.** The claim was that a specific, named lane had
    reported an undercount. Getting that wrong is not a neutral error, and it
    is the category where a relayer should verify *before* writing, not after
    being corrected.

  General form: **when a report about you turns out to be wrong, the error is
  usually in the part nobody thought to check — the part that was not the
  point.** The counting hazard was the point and it was right. The lane name
  was scenery, and scenery is what goes unverified.

- **A protocol about a shared resource is only as good as your knowledge of
  what your code touches.** A lane's probe selftest was documented as offline,
  described as offline to the coordinator and to its reviewer, and **was not
  offline**. Its anti-vacuity check calls `probe()` a second time; with nothing
  stranded, that call passes the contamination guard and falls through to a
  liveness check that opens a socket to the live game, then to
  `load_plugin`/`unload_plugin`. A mutation harness running the selftest twelve
  times therefore made twelve round-trips to a shared resource the lane had
  explicitly released, and the game's own log recorded every one.

  **The violation was not a decision — it was the absence of one.** Nobody
  chose to take the port; the code took it while its docstring said it did not.
  That is the part worth generalising: **you cannot comply with a protocol
  governing a resource you do not know you are using**, so "I did not intend
  to" and "I did not" are different claims, and only the second is checkable.

  The remedy the lane chose is the right shape and the general one: **assert
  that the call is never made, not that the outcome looks right.** An outcome
  assertion cannot distinguish "did not touch the resource" from "touched it
  and got the expected answer". Only an assertion on the *call* can.

- **An assertion that passes in both states proves nothing, and a negative
  substring check is the easiest way to write one.** From the same selftest:

      check("with nothing stranded, that refusal does not fire",
            "already installed" in rec.get("skipped", ""), False)

  This passes when the game is up. It also passes when the game is **down**,
  because `probe()` then returns `skipped="RemoteBuild not alive"` — a
  different string, so the substring is still absent, so the assertion is still
  satisfied. Half of an anti-vacuity *pair* therefore established nothing,
  while looking like the careful half.

  **Negative assertions are especially prone to this.** "X is not in the
  output" is satisfied by an output that lacks X for the intended reason, and
  equally by an output produced by a subsystem that never ran. Pair every
  "should not appear" with a positive assertion that the mechanism was actually
  exercised — otherwise deleting the whole feature makes the test greener, not
  redder.

- **Two lanes independently converged on the same fix: a mutation harness must
  refuse to score a mutation it could not apply.** One lane's variant patch
  silently no-opped when its anchor moved and reported "all green"; another's
  mutation runner hit a wrong-indentation anchor and reported **10/11 with an
  explicit MISS** rather than a clean 10/10. Had it skipped silently, the run
  would have shown full marks **while never testing the contamination guard at
  all** — the harness becoming the vacuous thing it exists to detect.

  The convergence matters more than either instance: **a mutation that cannot
  be applied and a mutation that is not detected are indistinguishable in the
  result, and they mean opposite things.** One says the guard is untested; the
  other says the guard is absent. A harness that does not separate them reports
  a number with two incompatible meanings.

  Note the lane's own framing when it retracted an earlier figure: *read the
  previous "10 of 10" as the one I cannot reproduce, and this one as the one
  you can.* Reproducibility, not the number, is what makes a verification claim
  worth anything. The retracted figure came from an ad-hoc harness that no
  longer exists and had been running against a live game, so what it measured
  is unknown. The figure that replaced it is **11/11 via
  `scripts/check_probe_cleanup_nonvacuous.py` (`just probe-cleanup-mutants`)**,
  named here so a reader can re-run it rather than trust a transcript. **Cite
  the target, not the number** — a verification figure with no reproducible
  command attached is a rumour with a decimal point.

- **A green test is not evidence until you know what made it green.** The lane
  that hit the port slip drew the sharpest form of this document's own rule,
  and it is an extension rather than a restatement. The recorded rule is
  *assert the value, not the presence* — "a file was created" versus "a file
  was created where I asked". Pointed at side effects instead of outputs, it
  becomes: the lane asserted that its selftest **passed**; it never asserted
  that the selftest passed **without touching the game**. So the test went
  green for a reason nobody had checked, twelve times a run, against a shared
  resource.

  **The presence/value distinction generalises from "what did the test assert
  about its output" to "what was the test allowed to touch".** Both halves of a
  green result matter: the answer, and the means by which it was reached. A
  suite that only checks the first will happily certify code that reaches
  outside its sandbox, because reaching outside usually makes things *work*.

- **A running dependency makes the wrong code work, so the symptom lands in
  someone else's logs and never in your test results.** This is why the port
  slip survived documentation, a coordinator's reading, and a reviewer's pass.
  There was no red test to find, and there could not have been: with the game
  up, the accidental round-trip **succeeded**. The only trace was noise in the
  game's log — a system the lane does not own and does not read.

  Two consequences worth carrying:

  - **A defect whose only symptom appears in a system you do not own is
    invisible to every gate you control.** Test suites, linters and reviews all
    look inward. Nothing in that set can see "this correctly-passing test also
    wrote to a shared service".
  - The fix must therefore be **structural, not a patch to the call site**. The
    lane's chosen remedy — the selftest *revokes* `rb_alive` / `rb_route` /
    `read_log_window` outright, so any future path reaching for the game fails
    loudly — is right for exactly this reason: patching the one known site
    leaves the next one to reintroduce the bug silently and for the same
    reason. **When the failure mode is "nothing goes red", the fix has to be
    the thing that makes something go red.**

- **When a total matches your prediction, check that it matches for the
  predicted reason.** A lane predicted its branch would total 2341 — master's
  2352 minus 11, since an `#[ignore]`d canary adds no passing test. It came
  back **2352**. The lane refused to report the match until it understood it:
  a merge had landed underneath, so the branch already contained the 11, and
  canary-adds-zero made the totals correctly equal. Right number, and right for
  a different reason than the one that would have been cited.

  This is the third member of the equal-totals family and the most general.
  The first was equal totals over **stale** content; the second was equal
  totals colliding by **arithmetic coincidence**; this is a total agreeing with
  a **prediction whose derivation had silently changed underneath it**. A
  prediction that comes true is evidence only if the mechanism you predicted is
  the mechanism that operated — otherwise it is a coincidence wearing a
  confirmation's clothes, and it is *more* dangerous than a mismatch, because a
  mismatch prompts investigation and a match ends it.

- **A positive control must fail for the reason you are guarding against, and
  `panic` is the wrong failure.** Building the crashed-binary gate, the canary
  had to **abort**, not panic. libtest *catches* a panic, marks the test FAILED
  and still prints a `test result:` summary — so the count stays complete and
  the failure is plainly visible. Only an aborted process leaves no summary and
  disappears from the total silently. **A panicking canary would have proved the
  wrong thing entirely** while looking like a rigorous control.

  Generalise it: a control that fails *somehow* is not a control. It has to
  fail through the exact mechanism the guard exists to catch, and the two are
  easy to conflate precisely when the guard is about silence, because almost
  any failure is louder than the one you care about.

- **The check already existed — on the gate people ran second.** The
  crashed-binary reconciliation was already implemented in the project's
  hermetic test runner (`ran = bool(summaries)`). It could not substitute for
  the new one: it runs each binary directly under an isolated HOME, so it
  covers neither doctests nor the ordinary `cargo test` path every lane
  actually gated on.

  So the hazard was never that nobody had thought of the check. It was that
  **the check lived on the secondary gate while the primary one went
  unprotected** — and a safeguard's coverage is determined by which command
  people habitually run, not by whether the safeguard exists somewhere in the
  repository. When you find a good check, the next question is which entry
  point lacks it.

- **"My change is inert" and "the gate can be skipped" are different claims,
  and only the first is yours to make.** The coordinator told a lane its gate
  could carry if the delta since its base were docs-only. The lane measured and
  refused: its base was old, and master had since taken **~1,500 lines of Rust
  across nine files from five other lanes**. Its own change touched no Rust at
  all — and that is exactly the distinction. **The gate's job after a rebase is
  to establish that the rebased TIP is green**, and the tip contains everyone
  else's work. Carrying it forward would mean asserting something about code the
  lane had never run.

  The coordinator's rule was sound but its precondition is narrow: a gate
  carries only when nothing compilable changed **since that gate's own base** —
  not since some more recent point, and never merely because the change under
  test is inert.

- **`None` from a canonicalizer means "already canonical", not "not found".** A
  lane replaced a deleted helper with what it called a "strictly wider"
  resolver path. It was not wider. `canonical_enum_key` returns `None` when the
  spelling **already is** the key — true of a Nadeo module-level enum whose
  synthetic module coincides with a real Core scope — so a caller written as
  `if let Some(key) = …` silently dropped exactly the inputs that needed no
  work. Ten `Meta::*` spellings stopped resolving.

  **The two meanings of `None` are opposites here**: "I could not do this" and
  "this needed nothing done". A function that conflates them forces every
  caller to guess, and the guess fails on precisely the well-behaved inputs —
  the ones least likely to appear in a test fixture, because nothing about them
  is interesting.

  Prefer a type that cannot be misread: `Cow`-like "borrowed means unchanged",
  or an explicit `enum { Canonical, Rewritten(String) }`, or return the input
  unchanged so absence genuinely means absence. **If a sentinel has two
  readings and both are plausible, the API is the defect, not the caller.**

  Note what caught it: a **1403-spelling corpus sweep**, which failed and named
  all ten. Not review, not the type system, not the suite — the totals were
  identical before and after, which is the correct signature for a refactor and
  also the signature of this bug.

- **Comparing two sets requires the same extraction on both sides, and an
  implausible number is the only alarm you get.** The same lane generated a
  clippy baseline with `sort -u` over five fields and the branch with plain
  `sort` over four — deduplicated against raw, four fields against five — and
  the comparison reported **27 spurious additions**. Nothing errored. It was
  caught because "39 unique" was implausible beside a 77 baseline.

  This is the third distinct clippy-comparison failure recorded here (raw vs
  unique totals; a set diff keyed on line numbers; now mismatched extraction),
  and they share one cause: **the instrument was fine and the two sides were
  not comparable.** A diff is a claim about two things measured the same way,
  and nothing in the tooling checks that they were. Generate both sides with
  **one script, in one run**, or the comparison is unverified regardless of how
  careful the diff is.

- **An acceptance criterion derived with the method the ticket exists to
  replace.** B061's defect is that a **property-syntax read** (`Off`) does not
  credit its accessor (`get_Off`), so accessors are reported unused. Its
  acceptance criterion — "24 warnings must become 18" — was assembled by
  searching the tree for `get_X(` **call syntax**.

  That search cannot see a property-syntax read. By construction. It is the
  exact blindness the ticket was filed to fix. So the expected survivor count
  was **guaranteed to be too high by precisely the number of property-syntax
  reads** — which is the population under test. The criterion could not have
  been right, and it disagreed with the correct fix by 2.

  **The fix looked like an over-fix.** A lane implementing it correctly landed
  on 24 → 16 and had to argue against its own ticket, with the tempting
  alternative being to *stop crediting two real production reads in order to
  satisfy the arithmetic* — failing the ticket's purpose to pass its own test.

  Three rows were misclassified this way, not one. **That is not three
  mistakes; it is one broken method with three outputs**, and it is why finding
  the second should have prompted re-deriving the whole population rather than
  patching the row. The first correction was made and the method behind it was
  never questioned.

  **Transferable, and it generalises past this codebase: when you write the
  expected result for a bug fix, check that the method producing your
  expectation is not the method the bug is about.** Any measurement blind to
  the phenomenon under test will produce a confident number in the direction of
  "the bug is smaller than it is" — and unlike a wrong fix, a wrong criterion
  is *enforced*, so it actively pushes the implementer toward reproducing the
  defect.

  What settled it was reading the two disputed rows at source — `RenderMark`,
  `IsSafeToInterrupt`, `IsSafeForDirective`, all production functions — and
  the crossed pair, which no over-broad fix could pass by luck: every
  production read of `Showing` is `Directives::Showing` and every production
  read of `PendingCount` is `Messages::PendingCount`, so one of each pair must
  be credited and the other must survive, **in opposite directions**. Same
  short name, opposite outcome, both ways.

  A coordinator near-miss while checking that, worth its own line: an initial
  `grep … | head -6` on the `Showing` reads returned only test files, which
  reads exactly like "no production read exists". It was a truncation limit,
  not a finding — the full listing has three. **`head` is not a filter, and a
  truncated list is not evidence of absence.**

- **A stale test binary hid a real regression, and the binary-count gate could
  not see it — because no binary was missing, only a test inside one.** Two
  disjoint branches were merged and gated once. The gate reported **25/25
  binaries reported, 2358 passed, 0 failed**. Green, reconciled, and wrong: the
  `cli_check_tests` binary was stale and held **52** issue-repro tests where the
  source had **54**. A newly merged test simply was not in it, so the regression
  it would have caught never fired.

  **The counting gate is exactly the wrong shape for this.** It verifies that
  every expected *binary* produced a summary — and every binary did. A test that
  vanishes *inside* a present, healthy, summary-producing binary is invisible to
  it. The two failure modes look nothing alike and only one was guarded.

  What caught it was **a prediction**: 2353 + 6 = 2359, against a reported 2358.
  One test. The converse of the rule recorded above did the work — **a total
  that misses your prediction by one deserves the same investigation as a
  crash.** Without the prediction, "2358 passed, 0 failed, 25/25" would have
  been accepted by anyone, including the person who wrote the gate.

  Forcing a rebuild took the binary from 52 tests to 54, and the merged tree
  then went **red**. **CORRECTION (same day): there was no regression, and the
  merge I reverted was innocent.** The lane that owned it demonstrated the
  failure being manufactured without its change present at all.

  **There were TWO staleness modes and I found only one.** The green 2358 was a
  stale *test binary*. The subsequent red was a stale **bin dependency**: the
  test spawns `CARGO_BIN_EXE_openplanet-lsp` as a **child process**, so the
  harness can be freshly built while the binary whose behaviour it measures is
  not. Touching the test file rebuilt the harness and not the binary, and the
  reported `left: 13, right: 6` was precisely the pre-fix warning count of a
  change that *was* in the source and *was not* in the binary being run.

  **And my bisect was uncontrolled.** The two passing runs used a fresh target
  dir; the failing run used a warm one. I varied the commit and the build cache
  **simultaneously** and attributed the difference entirely to the commit. A
  bisect is a controlled experiment or it is nothing, and a shared warm
  `CARGO_TARGET_DIR` is a variable — `git checkout` writes older mtimes, cargo's
  fingerprint reads them as fresh, and the build is skipped.

  Four rules follow, and the last two are the ones I actually needed:

  - **A total that drops, or fails to rise as predicted, is a defect signal
    rather than noise.** It is the only symptom the test-binary mode produces.
  - **Merge two branches separately, or predict the combined total exactly and
    treat any deviation as blocking.**
  - **Bisect in a fresh target dir, or you are bisecting the cache.** Change
    one variable. A warm shared cache is a variable.
  - **When a test spawns a binary, rebuilding the test proves nothing about
    the binary.** Ask what artifacts the assertion actually depends on —
    harness, library, *and* any child process — and confirm each is current.
    `touch`ing the test file is the most natural thing to do and it refreshes
    exactly the one that was not the problem.

  The cost of getting this wrong was a correct merge reverted on the strength
  of an artifact, and a lane sent to rework something that was never broken.
  **The revert was the confident action; the stale binary was invisible in
  every output I looked at.**

- **A ticket's preferred fix was already implemented, and inert.** B062 asked
  for "whole-file rooting: exempt declarations in a manifest `exports` file",
  and deprioritised signature matching as the fallback. A lane checked before
  building and found whole-file rooting **already exists** — and is a **no-op
  for this defect**, because the manifest-named file contains only
  `import … from` stubs while the declarations needing rooting are bodies in
  ordinary files.

  So the ticket's picture of the defect was wrong, and its preferred remedy
  could not have worked. Both errors came from the same assumption — that the
  exports file holds implementations — which the repo's **own fixture**
  explicitly contradicts: *"the strict export file carries ONLY import
  declarations … Bodies live in ordinary source files."*

  **The failure this avoided is specific: implementing something already
  present, finding no behaviour change, and concluding the analysis needs
  widening.** The natural next move after an inert fix is to make it less
  precise, which here would have exempted whole files that should not be —
  turning a false-positive ticket into a false-negative one. A wrong diagnosis
  does not merely waste the fix; **it aims the next fix in the wrong
  direction.**

  Two checks did the work, both cheap and both skippable: reading the existing
  implementation before adding to it, and reading the repo's own fixture
  comments, which had recorded the correct model all along. **A ticket is a
  report of a symptom by someone who did not have to make it work.** Its
  proposed remedy is a hypothesis with no more standing than the reporter's
  understanding of the subsystem — which is exactly the "Work section as
  hypothesis, not specification" rule, applied one level up to the *diagnosis*
  rather than the fix.

- **A correct measurement of the wrong thing.** Sequencing two lanes, I
  measured where `is_external_derived_from` is called (`checker.rs` 2292, 2309,
  **2999**, 3245) and where `funcdef_types_compatible` lives (2988-3005),
  found that 2999 sits inside it, and serialised the tickets on that basis.
  Every number was right. The conclusion was wrong, because **the premise that
  made those coordinates relevant came from the ticket, not from
  measurement**: B076 would not edit `funcdef_types_compatible` at all. That
  predicate is already correct and would be *called*, not changed; the edits
  belong ~1100 lines away in `check_var_decl_local` / `check_var_decl_global`.

  So I verified the coordinates and not the assumption that made them matter.
  **Measuring carefully inside a borrowed frame produces confident, checkable,
  irrelevant numbers** — and they persuade more than a guess would, because
  they carry file and line. Ask what would have to be true for this measurement
  to answer the question, and check that too.

- **Test the axis the title names AND a control on the axis it says works.**
  B076 reads "parameter lists are compared; the return type is not". Measured
  per position, the return type **is** compared — `func_matches_funcdef` checks
  it first, and the constructor form proves both halves work today. What is
  missing is a *call* to that predicate at three further positions, and **at
  those positions the parameter check is missing too** — the very thing the
  ticket reports as working.

  That surfaced only because the lane added a **param-mismatch control at each
  position** rather than testing the axis the title pointed at. Testing only
  return types would have found three silent positions and concluded the return
  check was unreached, leaving the parameter hole in place and invisible — a
  ticket "fixed" with half its defect surviving.

  **A ticket's claim about what already works is an untested assertion in the
  same breath as its claim about what does not.** Both come from one reporter
  and one misunderstanding. Control the working axis at every position you
  probe: it costs one extra case, and it is where the surprise was.

- **Once a commit has been reported as ready, never amend it — make a new
  commit.** A lane amended a commit it had already handed over, twice, while
  the coordinator was merging quickly. Both times the amended branch became a
  *replacement* for merged work rather than a descendant, which would have
  merged as a conflict or a duplicate; both were caught only by a base check.

  **Amend is the one operation that makes a branch incompatible with what was
  merged rather than merely stale**, because it rewrites the very commit the
  merge already contains. Every other operation degrades gracefully into a
  rebase.

- **A directory unheld at an instant is not a directory nobody is using.** I
  pruned 85 build directories from a shared cache root, having first validated
  the holding scan with a real positive control — I started a process inside a
  target dir and confirmed the scan detected it, precisely because my first
  run's "nothing is held" proved nothing. The scan was correct. It found one
  directory genuinely held and protected it.

  **It still collided with a lane's in-flight gate.** That lane's artifacts and
  two of its target dirs vanished *between two of its commands*, because a gate
  is a sequence — build, measure, compare — with gaps in which nothing holds an
  open handle or a cwd. The scan measured a **moment**; the lane occupied an
  **interval**.

  So the control was necessary and not sufficient, which is the interesting
  part: validating an instrument does not validate the *question* you asked it.
  "Is anything holding this right now?" and "is anyone using this?" are
  different questions, and only the first is answerable from `/proc`.

  Worse, the failure would not have looked like a collision. A vanished target
  dir surfaces as a build error or an inexplicable rebuild, so a lane would
  reasonably suspect its own change. **Cross-lane destruction disguises itself
  as local breakage.**

  What was actually needed was to ask rather than to detect — the lane had
  told me it was gating. For shared mutable state, **announce before you
  reclaim**, or reclaim only what is provably finished (a merged branch's
  dirs), never what is merely idle at the instant you looked. Note the lane
  itself had refused to delete other lanes' directories for exactly this
  reason, and deferred the decision to the coordinator; the coordinator then
  made it with a better tool and a worse question.

  Related: `du` reported 117.8 GB reclaimable where the actual free-space gain
  was **39.4 GB** — `/home` is btrfs and reflink sharing counts shared blocks
  once per reference. Measure the before and after of free space; never quote
  `du`'s total as the reclaim.

- **A red test written against an unverified expectation is an assumption in a
  test's clothing.** B071 says enum type mismatches "are not diagnosed" and its
  own text marks the expected behaviour **UNVERIFIED**, because the game was
  down when it was filed. AngelScript implicitly converts enums to integers, so
  some of the silent lines the ticket lists may be **correctly** silent.

  Implementing to that ticket would produce a red test, a fix that turns it
  green, a clean gate — and **false errors on code the game accepts**. For a
  type checker that is the damaging direction, and the one that teaches users
  to add suppressions.

  **A ticket that honestly labels its expectation as unverified is in one way
  more dangerous than one that does not**: the label is prose and gets skimmed,
  while the expectation is a list of file:line and gets implemented. Every
  downstream artifact — the red test, the fixture, the acceptance count —
  inherits the assumption and then *looks* like evidence, because a passing
  test is indistinguishable from a correct one when nobody measured the
  premise.

  The order that fixes it is cheap and non-negotiable: **probe for ground
  truth, write the expectation from the engine's verdicts, then implement.**
  Not the reverse, and not in parallel. This is the B061 hazard one level
  worse — there the acceptance criterion was derived by a method blind to the
  phenomenon; here there is no measurement at all and the ticket says so.

- **`2>/dev/null` converts the loud failure mode into the silent one.** This
  document already recorded the bfs case — `find -newermt '-45 minutes'`, GNU
  relative timestamps that bfs rejects — and explicitly credited it as the
  *milder* hazard, because "bfs **errored**, so its traversal visibly never
  ran". Hours later I wrote the same probe again to check whether six lanes
  were still working, and this time I appended `2>/dev/null`. Every lane came
  back **"0 files touched"**, including two that had committed 80 seconds and 2
  minutes earlier.

  So the redirect did not merely hide an error. It **moved the failure from the
  documented-mild class into the documented-severe one**, and produced the
  answer that stops you looking — six idle lanes — about work that was visibly
  in progress. The contradiction is what saved it: a commit 80 seconds old is
  proof that files were written, so "0 files touched" could not be true.
  Without an independent signal in the same output, it would have read as six
  stalled lanes and I would have started killing and restarting them.

  **The rule this yields is narrow and worth obeying literally: never suppress
  stderr on a probe whose output you intend to believe.** Suppress it on
  something whose *success* you will verify by other means; never on the check
  that is itself the evidence. A probe's error text is not noise around the
  result — for a probe that cannot run, it **is** the result, and it is the only
  part that distinguishes "I looked and found nothing" from "I never looked".

  Note also how thin the protection was to begin with. The earlier entry's
  reassurance ("at least it errored") was true, but it was one redirect away
  from being gone, and the person who removed it was the one who had written
  the sentence. **A hazard that is only survivable because a tool happens to be
  loud is not mitigated; it is merely lucky.** The real mitigation is the
  positive control — point the probe at a file you just touched and require it
  to come back 1 — which is what finally produced the true answer (13, 23 and 2
  files across three live lanes).

- **A flag that was accepted, documented, and ignored.** B068's fix added a
  `--batch` knob so a corpus could be re-measured one name per compilation
  unit. The flag parsed, was documented as a speed control — and **both corpus
  writers ignored it**, so `--b052 --batch 1` silently ran at 200. Found by the
  second review pass, and it is the sharpest near-miss in this batch: the
  re-probe exists precisely to find rows that batching corrupted, so a
  re-probe that silently batched would have **reproduced the original defect
  and reported no flips**. A clean bill of health, produced by the very bug
  under investigation, on a corpus five tickets cite as ground truth. Nothing
  about the output would have looked wrong.

  **This is the add-a-guard rule in its most expensive form.** The replacement
  measurement was broken the same way as the thing it replaced, and the only
  reason it surfaced is that someone tested the *flag* rather than the feature.
  When a fix introduces a switch that changes how a measurement is taken,
  **the switch needs its own test per call site** — a knob wired to nothing is
  indistinguishable from a knob that works, from the outside.

**Countermeasure — and the way it fails.** A corroborating check is only worth
running if it can fail independently. **A second check that inherits the
first's population assumption is not an independent check.**

Worked example, from the coordinator, in one sitting. Question: does engine
class `CGameCtnApp` declare a member enum? Check one counted the class-level
`e` array: **0**. Check two asked a structurally different question — does the
class have a member whose *type* is an enum? — and also said **no**. Two
methods, two agreeing answers, one wrong conclusion.

They were not independent. Check two built its set of enum names from the same
`e` arrays check one counted, and the enum in question is **anonymous**, typed
with a shared `UnnamedEnum` placeholder that appears in no named-enum set. One
blind spot, inherited, wearing the costume of corroboration. The agreement felt
like confirmation and was a single measurement counted twice.

Two rules follow:

- **State the population each check reads, not just its method.** Two checks
  over the same population are one check. The differing *question* is what
  makes methods independent only if the *data* also differs.
- **Read the source before measuring it.** The rule under investigation was
  already documented in the code — `src/typedb/index.rs`, "named or anonymous
  — either is enough" — naming the exact class in dispute. Two people spent an
  hour deriving, from live-game probes and dump arithmetic, a fact a doc
  comment stated outright. Grep the codebase for the answer before building an
  instrument to discover it.

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
- **A stale file of the same name read as a fresh result.** A gate polled for
  `final-hermetic.log` to become non-empty. A **20-hour-old file of that exact
  name**, from a previous session, was already sitting in the shared scratchpad,
  and "17 binaries, 1493 passed" was read as this run's result. It was caught
  only because the reader knew the tree has 23 binaries and ~2255 tests — by
  implausibility, not by any check. Countermeasure used afterwards: **stamp the
  filename with the commit SHA and verify mtime before trusting it.** A shared
  scratch directory is shared across *time* as well as across agents.
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
- **A warning count counted a different population than its name claimed.** A
  clippy gate summed every output line matching `^(warning|error)`, which also
  matches cargo's own per-target epilogue — ``warning: `openplanet-lsp` (lib)
  generated 53 warnings``. With six build targets that is a silent, stable `+6`
  on every measurement. It is self-consistent, so it never looks wrong; it only
  surfaces when one number is compared against another taken a different way,
  and then it reads as a regression that does not exist. Two measurements of
  the same tree disagreed (75 vs 81) and the tree was innocent.
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
  your tests. (Observed: `multi_root_tests`. **The cause was diagnosed wrong
  twice, and the coredump refuted both.** First reading: a stack overflow in
  toml_edit's recursive parser, from deeply nested input. Second: memory
  pressure, from "~2G free" — which read the `free` column where `available`
  governs, and `available` was 21–22G throughout.

  **RESOLVED (2026-08-31). All three diagnoses were wrong, and the true answer
  was not a software defect at all.** Recorded in full because the sequence is
  the lesson.

  - **First:** stack overflow from parser recursion. Refuted by stack DEPTH —
    ~32-40 frames, frame #0 the parser's entry point, before any recursion.
  - **Second:** stack overflow from large values on a tokio worker, read from
    `si_code=SEGV_ACCERR` as a guard-page hit. Refuted by `rsp`: **36 KiB used
    of a 2052 KiB stack**, 98% untouched, against a config-load path bisected
    to need 44-48 KiB. A 42x margin.
  - **Third:** a jump to a bad address at a fixed module offset. Refuted by
    reading the pushed return address out of five cores: `rip` is
    `toml_edit::parser::prelude::new_input` and `[rsp]` is `parse_document`,
    **exactly frame #1 in all five**. The `call` executed correctly. Nothing
    was corrupted.

  What it actually is: an **instruction-fetch permission fault on a present,
  executable page**, verified from the cores' own PT_LOAD flags. That cannot
  happen in correct operation, in any program — so there is nothing in this
  codebase to fix. Six further mechanisms were each refuted by measurement
  rather than argument (NUMA balancing, THP collapse, CET/IBT, a rewritten
  binary, load, hardware error). The `ETXTBSY` one is the model: the lane's
  leading hypothesis was that the binary was rewritten underneath the running
  process, and instead of reasoning about it, it copied a binary, ran it, and
  tried to truncate the file — `OSError: [Errno 26] Text file busy`. Refuted in
  one command.

  **`SEGV_ACCERR` does not mean "guard page".** It means mapped-but-wrong-
  permission. The guard page is only its most common cause, and reading the
  common cause as the meaning is what produced diagnosis two — mine.

  **The fourth cause was deliberately left unasserted.** The evidence has a
  clear shape — one file, one text page, a 70-second window, three different
  tests, no other process on the machine faulting — which is consistent with a
  transiently bad page-cache page or PTE, clearing on eviction. It cannot be
  confirmed on this box (no memory controller registered under
  `/sys/devices/system/edac/mc/`, so ECC errors go unreported). The lane wrote
  it up as *the shape of the evidence, not a diagnosis*, and stopped. **After
  three refuted causes, declining to name a fourth is the disciplined move, not
  an incomplete one.**

  **What this cost, and the actual transferable lesson.** Three confident
  diagnoses, each self-consistent, each pointing at a fix that would have
  addressed nothing — a recursion limit, a bigger stack, a hunt for a corrupted
  pointer. Every one was refuted by a cheap measurement that nobody had taken:
  frame count, `rsp`, and the pushed return address. All three were sitting in
  a coredump the whole time. **The gap was never analysis; it was that each
  diagnosis explained the evidence already in hand well enough to stop
  collecting more.** A plausible mechanism is a reason to take the next
  measurement, not a substitute for it.

  And the practical residue outlived the mystery: the crash's real cost was
  never the crash. It was that **a crashed binary makes `cargo test` exit 101
  and emit ZERO `test result` lines**, so a gate that sums those lines drops
  the tests silently and stays green — which is exactly what produced a
  reported 2190/2218 against a true 2223. Fixing the count made every later run
  honest; diagnosing the SIGSEGV would have fixed nothing.

  **Transferable: when a fault is blamed on a mechanism, find the number that
  mechanism PREDICTS and read it.** Recursion predicts deep stacks — read the
  frame count. Exhaustion predicts a full stack — read `rsp`. A corrupted jump
  predicts a wrong return address — read `[rsp]`. Each took one command and
  each killed a diagnosis outright. Evidence: B078.)

**The strongest datapoint in this batch for that claim.** One lane's change
  went through **six** adversarial review passes. Every pass found a real
  defect the lane had introduced — and **all six were invisible to every gate**:
  the full suite, the hermetic run, clippy, and a false-positive fixture gate
  over six real plugins were green on all six rejected commits. Worse, **each
  round's fix introduced the next round's defect** (a rule taught in round 5
  was found in round 6 to have been tested against the wrong domain), a
  measured 5-for-5 rate.

  Two conclusions, and the second is the uncomfortable one:

  - A green gate on a subtle semantic change means the change did not break
    anything **the corpus contains**. On a change to name resolution, no
    fixture declares the colliding shapes, so the gate is structurally blind.
  - **An unreviewed fix to a subtle defect is more dangerous than the defect.**
    Where fixes have a demonstrated history of introducing the next defect, the
    cheap-feeling move — patch it and ship without re-review — is the one with
    the worst record.

- **Silence can mean "cannot tell", not "yes" — and a test that reads it as
  assent asserts the opposite of what it claims.** A capstone test drove
  completion and the checker end-to-end and asserted the two agree: what is
  offered must be exactly what is accepted. It failed on
  `CGameManiaPlanetNetwork.GetCurrentEntityID`, where the checker "accepts".
  It does not accept — that type carries **0 members** in the dump, and B006
  deliberately stays silent because an empty member list is *untrusted*, not
  authoritative. The test had read an absence of diagnostics as a positive
  verdict.

  Diagnosed rather than adjusted away: for such receivers the assertion now
  requires only the severe direction — completion must never offer what the
  checker rejects — and records why the other direction is unavailable there.
  **Before asserting that two components agree, establish that both can
  actually answer.** A component that abstains is not a component that agrees,
  and an equality over one that abstains is vacuous in exactly the region you
  most wanted covered.

- **Equal totals do not prove equal content.** A clippy comparison reporting
  77 on both sides is consistent with any number of diagnostics having been
  added and an equal number removed. The stronger check is cheap: diff the
  diagnostic **sets**, and confirm nothing added and nothing removed. Same for
  a suite total — reconciling to the expected number tells you the count moved
  correctly, not that the same tests ran. Prefer an identity over a count
  whenever one is available.

- **Measure each half of a multi-part fix as a standalone variant.** Twice in
  this batch a ticket's own Work section proposed the half that closes nothing,
  and in both cases the wrong half was the plausible-sounding one:

  - **B064.** Merging the two locks (a) was the headline; carrying the stamps
    out of the parse (b) was the fix. `(b)`-only left 0 tests red, `(a)`-only
    left 8.
  - **B072.** "Read both files' bytes first so the reads are close together"
    left **6 of 9 tests red** as a standalone change. Narrowing a race window
    closes nothing, because **the window is a position in a sequence, not a
    duration** — the writer is an unrelated process that can land anywhere in
    it. The identity check alone closed all 9.

  In both cases a commit doing only the proposed half would have **looked like
  a fix and been none**, and no amount of reading the diff distinguishes the
  two. The matrix does, cheaply: build each half, run the reproductions,
  record which stay red. **Treat a ticket's Work section as a hypothesis to be
  measured, not as a specification** — it was written before anyone built the
  thing, usually by someone reasoning from the symptom.

  The corollary matters for review: if a fix has two parts and only one is
  load-bearing, **say which**, in the commit. Otherwise the next person to
  touch it cannot tell defence-in-depth from the mechanism, and will delete
  the wrong one.

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
- **A count is a claim about a population — state which one.** Prefer a
  categorised diff of two runs over comparing two totals, and exclude the
  tool's own summary lines from any count of what the tool found. Totals taken
  by different methods are not comparable, and a stable offset between them
  looks exactly like a real delta.

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

- **A gate reconciles at one level, and is blind one level down.** `just test`
  (B078/B082) proves every expected test *binary* reported a summary. It
  cannot see a test that vanishes **inside** a present, healthy,
  summary-producing binary, nor a stale **bin dependency** — the
  `CARGO_BIN_EXE_openplanet-lsp` a test spawns as a child, which is not a test
  binary and so is not in the reconciled set at all. Both were live during the
  false B074 revert on 2026-08-31, and `left: 13, right: 6` on the B061
  fixture turned out to be the *pre-B061 binary* under a current harness, not
  a source change. It cost a correct merge being reverted.

  The mechanism is general: every lane here points `CARGO_TARGET_DIR` at a
  shared cache and bisects in detached worktrees, so cargo's fingerprint can
  read fresh over a tree that moved and skip the build. The only symptom
  either blind spot produces is **a total that fails to rise as predicted** —
  no error, no failed test, no missing binary. `just test-inventory` (B085)
  closes both by comparing each binary's `--list` against the `#[test]` count
  in the sources cargo's own dep-info says it was compiled from, and every
  artifact's mtime — the spawned bin included — against those same sources.

  The transferable half: **when a check reconciles a set, ask what lives one
  level inside its members and one level outside its membership rule.** Both
  are invisible to it by construction, and both fail toward looking clean.

- **A gate that correctly refuses to answer still has to say why.** `just test`
  went INCOMPLETE on one merge and clean on an immediate re-run of the same
  tree. Refusing to report a total was right; rendering all three possible
  causes as the same line was not. A binary that produced no summary might have
  crashed on a real defect, been killed under machine load, or hit something
  else entirely, and **the failure mode of making a human guess is dismissing a
  real crash as "just load"** — which is how B078 nearly went.

  So the verdict now carries the post-mortem: the signal or exit status cargo
  reported, classified into "a fault inside the process" (SIGSEGV, SIGABRT,
  SIGILL) versus "something outside ended it" (SIGKILL, SIGTERM); how many of
  its declared tests it managed to print before stopping; and the machine's
  load, sampled DURING the run because a spike that starved a process has
  usually decayed by the time it ends. A SIGSEGV on an idle box and a SIGKILL
  at 41 on 16 cores must not render identically, and the selftest asserts they
  do not.

  **And it does not retry.** A gate that retries until green converts an
  intermittent into silence, which is strictly worse than a red run.

  **And it saves the whole run to a file.** B092 cannot name its own failing
  binary, because the one occurrence was captured with `| tail -6` and the
  verdict line naming it was above the cut. An intermittent is exactly the
  case you get one look at, so the gate now writes the complete output on
  failure and prints the path as its LAST line, where a tail will still find
  it.

- **Which machine a check runs on can be part of what it measures.** Two of
  this repo's harnesses moved to a remote build host, and they do not move the
  same way. `check_clippy_delta_nonvacuous.py` splits fine — clippy's verdict
  does not depend on the filesystem. `check_test_inventory_nonvacuous.py`
  cannot be split at all: its subject IS the mtime relationship between
  sources and the artifacts built from them, and an rsync rewrites exactly
  that. Run it across two machines and it measures the sync.

  Two guards had to change for the move, both for the same reason. The
  restore check verified itself with `git status` — but `.git` is deliberately
  not synced to the build host, so that check would have **silently stopped
  checking** the moment the harness ran there. It now compares a sha256 of
  each file it touched, which is stronger anyway and works everywhere. And the
  clippy baseline recorded a blank commit remotely, quietly disabling the
  staleness report; recording without a commit is now refused.

  The transferable question: **when you move a check to another machine, ask
  which of its inputs came from the machine.** Filesystem timestamps, `.git`,
  the toolchain version and the core count all did here.

- **`-D warnings` makes clippy report FEWER lints, not more.** It turns the
  first warning into an error, so compilation stops early and the diagnostic
  set is truncated. Measured on this tree, rustc 1.97.0:
  `cargo clippy --all-targets` reports **129** diagnostics / 39 fingerprints;
  the same command with `-- -D warnings` reports **115** / 33. A gate built on
  the stricter-looking flag silently permits the 14 it cannot see, and the
  flag's name argues the opposite. `just clippy-delta` (B083) refuses to run
  with it.

  The general shape is Mechanism 1 wearing a reassuring label: **a flag that
  makes a check stricter can make its INSTRUMENT blinder.** Anything that
  aborts early — `-D`, `--fail-fast`, `set -e`, a first-error `exit` — narrows
  the population before the measurement is complete.

  Two more rules for a differential lint gate, both learned the same way:
  compare **fingerprint identities with line numbers normalised out**, never
  totals (equal totals collided by coincidence in this batch, and an unchanged
  total with a swapped member has to fail); and keep the baseline's **method**
  — exact command, toolchain versions, features, profile — in the same file as
  its numbers, or the next reader compares 129 against 115 and concludes
  something moved.

  **A recorded method that is not enforced is a comment.** Measured on this
  tree: `cargo clippy --all-targets` yields 39 fingerprints, and dropping
  `--all-targets` yields 28. All 11 of the difference exist only WITH the flag,
  and two of them are `unused_imports` that exist only under `cfg(test)`
  (`src/server/symbols.rs`, `src/typedb/index.rs`). Difference an all-targets
  run against a baseline captured without it and you are handed **11 added
  lints attributed to a code change nobody made** — and nothing in either
  number says which capture was which. So the gate REFUSES to compare across
  methods and shows no delta at all, rather than printing a warning above a
  plausible-looking list. The reader acts on the list.

- **A fixture comment asserting engine behaviour is a claim, not a
  measurement — and everyone downstream reads it as a measurement.**
  `tests/fixtures/.../60-exports-correct-pattern` paired a by-value `import`
  stub with a `const string &in` body and called it the CORRECT pattern. It
  had never been game-probed. A consumer calling that shape dies at runtime
  with `Unbound function called`. Before anyone measured it, the fixture had
  been cited as evidence twice — in a shipped source comment and in the
  approval of B062's matching rule — and both conclusions were wrong
  *because the fixture said so* (B086).

  What caught it is the reusable part. The lane's first probe had only the
  mismatched variant and a control naming a function that does not exist, and
  **both failed identically** — an oracle that cannot discriminate, which it
  would have been entitled to read as "the engine is strict" without having
  measured anything. Adding the `exact` positive control made it a
  measurement, because `exact` binds and the only difference from the failing
  case is the parameter type. **A negative control alone proves nothing: it
  cannot distinguish "the thing is false" from "the probe cannot see".**

  The same trap sprang again the same afternoon, one layer down, and this time
  it caught a control rather than a test. Nine RemoteBuild routes were probed
  with a long plugin id — eight real ones and `zzz_no_such_route` as the
  control — and all nine answered `Not implemented`. The conclusion on offer
  was "these routes are unimplemented". They are not: every request over 64
  bytes is truncated before it parses, so **the control failed through the
  same mechanism as the positives**. A control that shares a failure mode with
  the test is not a control, it is a second test. The discriminator was a size
  sweep, which no amount of re-reading the route names would have suggested.

- **Measure the thing, not a consequence of it.** The read cap above was first
  put at 66 bytes by padding a request until it stopped parsing. That measures
  whether a *particular payload* survives being cut, not where the cut falls:
  padded to 65 and 66 the first 64 bytes still ended in `"}` and `"`, parsed
  leniently with `route` intact, and answered normally. The cap was 64
  throughout. The game traces every socket read verbatim, so the length of the
  first read was available directly the whole time — the wrong number came
  from choosing an indirect instrument when a direct one existed.

  Both readings were self-consistent, both were reproducible over repeated
  trials, and the wrong one was off by exactly the amount that made a
  structural boundary look like a fencepost. **Reproducibility is not
  accuracy.** When a measured constant lands near a suspicious number, check
  whether the instrument is reporting the quantity or a proxy for it.

---

## The scoping hazard: a narrowing chosen for one failure mode blinds you to another

The three mechanisms above are about instruments that lie. This one is about a
*correct* instrument pointed at a deliberately reduced population — and it is
harder to see, because every step of the reasoning is sound.

**Three instances in one day, by three different people, none caught by
re-reading the reasoning.**

1. **A re-probe narrowed to ACCEPTED rows.** B068 let a batched parse abort
   mark rows ACCEPTED that the game never accepted. Re-measuring 742 ACCEPTED
   rows and skipping the REJECTED ones is correct *for that defect*: a REJECT
   is self-evidencing and cannot be fabricated by an abort. It also excluded
   the `*::ZZZNoSuch` **control** rows — the only rows that pin what the probe
   fixture declared. So when the harness's fixture turned out to differ from
   the one that produced the corpus, three rows "flipped" and the run could not
   tell a fabricated ACCEPT from a changed question. The controls were dropped
   *because* they were redundant for the defect being hunted.

2. **Two checks sharing a population.** "Does this class declare a member
   enum?" was answered by counting the class-level named-enum array (0) and,
   as corroboration, by asking whether any member's *type* is an enum (also
   no). Different questions, same underlying data, and the enum in question was
   **anonymous** — invisible to both. Agreement read as confirmation; it was one
   measurement counted twice.

3. **A defence against the wrong failure mode.** The `Foo::` corpus rows above
   were argued safe from B068 because interleaved REJECTED controls proved the
   probe was still discriminating across that region. **That argument was
   correct, and the re-probe confirmed it** — B068 never fired on this corpus.
   It answered a question about *batching* while the rows were compromised by
   *environment*: a plugin declaring `namespace Foo` was installed when the
   corpus was measured and is not now. Defending a claim against the failure
   mode you have in mind, and treating that as a general defence, is the same
   error one level up.

4. **A one-way law blind to a wrong answer.** A sweep asserting *hover answers
   ⇒ checker accepts* catches over-acceptance, which was the defect in hand. It
   cannot see a **wrong-but-present** answer, which satisfies the law while
   still being wrong. Closed separately, by a construction argument plus a
   measurement in an adversarial workspace.

**The rule.**

> A narrowing chosen for one failure mode makes another invisible, and **the
> check that would have caught it looks redundant right up until it isn't.**

**And the failures are silent.** In one delete-safety check, three successive
methods all reported "safe" for a directory with three live processes in it:
`find -newermt` (bfs rejects GNU relative timestamps, so it errored and matched
nothing), directory mtime (not the question asked — a build writes deep, not at
the top), and an open-handle scan (zsh's `nomatch` aborted the loop before it
printed). **None of them said "I could not check."** They said "nothing found",
which is the same string a successful check produces. That is why a positive
control is not optional before a destructive action: it is the only thing that
distinguishes the two.

**Countermeasures.**

- **Write down what your narrowing assumes, not just what it excludes.**
  "REJECTs can't be fabricated" is a claim about *one* defect. State the defect
  it is scoped to, so the next reader knows what the scope does not cover.
- **Keep the controls even when they are redundant.** Controls are cheap and
  they are the only rows that describe the *environment* rather than the data.
  A run that drops its controls cannot detect that its own conditions moved.
- **When two checks agree, name the population each read.** Two checks over one
  population are one check. Independence comes from differing *data*, not from
  differing questions.
- **Ask what your assertion cannot express.** A one-way implication cannot see
  a wrong answer on the permitted side; a count cannot see a substitution; an
  equality cannot see that both sides are empty.

**And read the source first.** Three times in that same day, the answer someone
was deriving from live-game probes and dump arithmetic was already written in a
doc comment: `index.rs` on class namespaces ("named or anonymous — either is
enough", naming the disputed class), `index.rs` on the module-name scope rule,
and `global_scope.rs` on the `Foo::` provenance ("Probed on a plugin declaring
`namespace Foo { namespace Bar { … } }`"). Grep for the answer before building
an instrument to discover it.

## Resolution of instance 1: the narrowing did blind the run, and widening it is what proved the mechanism

Recorded 2026-08-31, when the re-probe above was actually carried out. Both
shipped corpora were re-measured at `batch=1`: b052's 742 ACCEPTED rows and
b053's 592. **Zero rows were fabricated by the batching defect** — b053 did not
move at all, and b052's three movers were environmental.

The prediction held, and it held **for the reason stated** rather than by luck,
which is the thing to check before treating a confirmed prediction as evidence
(see the entry on predictions that come true for the wrong reason). The
ACCEPTED-only pass genuinely could not distinguish a fabricated ACCEPT from a
changed question. What settled it was going **outside** the narrowing and
re-probing the `*::ZZZNoSuch` context pins the original scope had excluded:

    Foo::ZZZNoSuch       then: Identifier 'ZZZNoSuch' is not a data type in namespace 'Foo' or parent
                         now : Namespace 'Foo' doesn't exist.
    MathX::ZZZNoSuch     then: Identifier 'ZZZNoSuch' is not a data type in namespace 'MathX' or parent
                         now : Identifier 'ZZZNoSuch' is not a data type in namespace 'MathX' or parent

The **message form** carries the fact, not the verdict. "… in namespace 'Foo'
or parent" can only be produced when the namespace exists. `Foo` produced that
form then and the namespace-absent form now; `MathX` produced the same form in
both runs. So the namespace vanished between the two runs — and since
`build_source` declares no namespace anywhere, `Foo` was never the fixture's
to begin with. It came from the machine's installed plugin set.

Three consequences worth carrying:

- **A REJECT row can be evidence about the environment even when it is worthless
  as evidence about the defect.** The narrowing was right that an abort cannot
  manufacture a REJECT. It did not follow that the REJECTs were uninformative —
  they were the only rows describing the conditions the ACCEPTs were measured
  under. *Uninformative about the defect* and *uninformative* are different.
- **Two buckets, reported separately.** "Rows that moved" is not a finding. Split
  it into *the defect fabricated this* (b052: 0) and *the environment changed*
  (b052: 3), because a single delta count silently mixes a corpus error with a
  corpus-provenance fact, and only one of them is a reason to touch the file.
  Those three rows must NOT be "corrected" — they were valid measurements of a
  machine where `Foo` existed.
- **The shape of a delta is itself an argument.** An aborted batch fabricates
  ACCEPTED for *every* surviving name in its unit, so a 200-name batch would
  leave ~199 spurious rows. Three isolated spellings sharing one namespace
  family is not that shape. Reason about the failure's signature, not only its
  count — the signature discriminates causes the count cannot.

The re-probe is now `probe_enum_corpus.py --reprobe`, with controls at start,
every 100 names, and end, and `UNDETERMINED` kept distinct from `ACCEPTED`, so
the next person re-measures rather than re-argues.

## Mechanism 4: the presentation dropped the rows, and the presentation looked fine

Recorded 2026-08-31. Four instances in one afternoon, three of them caught, one
of them mine and not caught until someone else measured it.

**1. A `Counter` truncated to its twelve most common entries.** (lane-unused,
self-caught.) Attributing 117 removed warnings, an intermediate read concluded
the `shared` rule had NO live corpus witness — and it was wrong, because the
shared removals were in the tail that `most_common(12)` dropped. The report
would have said "fixture-only, no live instance" with complete confidence.

The framing that lane insisted on is the right one and worth preserving: **it
was a readability choice, not a shortcut.** "Do not take shortcuts" teaches
nothing, because nobody thinks they are taking one. `most_common(12)` looked
like tidy output right up to the moment the rows that mattered were the ones it
dropped. The transferable question is not *did I cut corners* but **what does
this presentation make invisible?** — and it applies to `head`, `tail`, `[:20]`,
`most_common`, a truncated diff, and a summary table alike.

What saved it: re-reading every one of the 117 declaration lines rather than
trusting the aggregate. An aggregate is a lossy view of the data, and which
rows it loses is chosen by the view, not by relevance.

**2. `head -6` on a file-read listing** nearly reversed a ruling earlier in this
batch — the first six reads were all test files; the full listing had three
production reads.

**3. `| tail -6` on a failing gate** destroyed the one piece of evidence that
mattered. The verdict line names the failing binary; the tail did not include
it, which is why B092 cannot say which binary failed. **When capturing a
FAILURE, redirect the whole thing to a file. You do not know yet which line is
the evidence** — that is what makes it a failure rather than a result.

**4. A `grep -i oom` over the journal reported OOM kills that had not
happened** — it was matching "r-oom-s" in an unrelated service's log line. A
substring match on a three-letter token is not a search for that event. Mine,
and I reported it before checking.

## An error message that names the wrong dimension costs more than a silent failure

B097. RemoteBuild parses each raw socket read as a complete request, and reads
cap at ~64-66 bytes, so any payload over that splits and no fragment parses.
The reply is:

    {"error":"Not implemented: ","data":""}

That is the router's UNKNOWN-ROUTE message, with an **empty** route name. So a
size fault renders as a routing fault, and it points every reader at route
spellings. Two independent sessions lost time to it on the same day, and one
had already written it off as flakiness.

**A liveness check that does not exercise the failing path is a positive control
that controls nothing.** `get_status` is 32-35 bytes, so it fits, so
`rb_alive()` returns Alive — every probe passes its gate and then dies on the
route that matters. A health check must be representative of the work, not
merely of the connection; a short one cannot detect a length fault by
construction.

## A system can be broken and safe, and fixing half of it removes the safety

The most important line in the B097 reports, from two lanes independently:

**Today the harness fails CLOSED.** B077's teardown sees the unload fail,
preserves the fixture, and marks the run SKIP — so no verdict escapes. **Fix
`unload_plugin` without fixing `load_plugin` and the harness begins emitting
confident ACCEPTs for plugins the game never compiled.**

That is Mechanism 1 with the polarity that stops you looking: everything would
appear to start working. The repair that makes the symptom disappear is the one
that converts a loud outage into a silent wrong answer.

**Before repairing a broken pipeline, work out what is currently containing the
damage, and make sure your fix does not remove the containment first.** Order
the repair so the detecting half is fixed before the reporting half, and verify
against the system's own log rather than against a success return.

## A prediction recorded before the measurement is worth more than the same idea after

I filed B097 as "concurrent clients corrupt each other's payloads" — the
intuitive reading, and the one the reporting session had too. It was wrong.

The only reason that cost nothing is that the filing also wrote down the
discriminating test *and* what each outcome would mean: "it should reproduce
with a single client if the payload is big enough... that would turn 'concurrent
clients corrupt each other' into 'the reader does not honour its own length
prefix'." lane-tooling ran it and measured 66 OK / 67 FAIL deterministically,
single client, idle port.

Two things this bought that a later realisation would not have:

- **The concurrency story could be DISCARDED rather than kept as an
  alternative.** A hypothesis with a pre-registered refutation condition is
  falsifiable; the same hypothesis without one tends to survive as "well, both
  could be happening".
- It made the wrong framing cheap to admit, because admitting it was executing
  the plan rather than abandoning one.

State what would change your mind, in writing, at the moment you form the
belief — not when it is challenged.

## Shipped code can be stronger evidence than a probe

lane-unused could not probe which `shared` forms the engine accepts (B097). It
cited real declarations inside a published plugin instead — `shared class`,
`shared interface`, `shared enum`, `shared funcdef`, `shared` function, each at
file:line in Editor++ (siteid 377).

**This is not a fallback. By this repo's own written standard it is better.**
`docs/probes/README.md` warns that a probe's ACCEPT can be supplied by another
installed plugin — and `shared` forms are exactly that risk. A shipped plugin's
OWN declaration cannot leak from elsewhere, and a plugin that fails to compile
does not ship. The probe would have produced the weaker of the two.

Generalise: when the question is "does the engine accept X", a corpus of code
that demonstrably runs is a population of measurements someone else already
paid for. Look there before building an instrument. And when you do cite it,
say which forms have a witness and which do not — the same lane recorded
`shared abstract class` and `shared mixin class` as NOT MEASURED, with the
reason, which is what stops the artefact being cited later for a composition
nobody checked.

## "It was never compiled" is not visible in the filesystem

Clearing a stranded probe fixture, lane-unused ran four checks: marker PID dead,
exactly the three files it staged, byte-identical to source, and **zero mentions
of the fixture's symbols in the game's log.**

The fourth is the one to copy. A stranded fixture that WAS compiled is a
contaminating installed plugin — it declared names into the environment other
probes then measured against. One that never compiled is only litter. Those are
different severities requiring different follow-up, and nothing on disk
distinguishes them; only the log does.

## The same rule, at one of N call sites — now three times in one batch

- **B076**: a correct check reached from ONE of four initializer positions.
- **B073**: `awaitable` is hardcoded in `is_angelscript_builtin_type`, a second
  path beside `lookup_funcdef` with the identical hole; guarding one left four
  cells red.
- **B098**: B056 fixed bare-name enum resolution in `resolve_unqualified`;
  `external_enum_by_short_name` survived doing it, reachable for 436 names.

Three is a pattern, not a coincidence. **When you fix a rule in a resolver, grep
for every other function that answers the same question before believing it is
closed.** The ticket names the site someone noticed; the codebase reaches the
question from more places than the ticket names. B073's lane found its second
site by testing the BEHAVIOUR rather than the function — that is the technique
that generalises.

Corollary already recorded but now reinforced: a helper reached from one of N
positions is a check that exists at one of N positions, and it LOOKS from the
call site like it exists everywhere.

## A silence test written before the fix passes for free, and stays green for reasons that are not the fix

B088, 2026-08-31, caught by lane-unused with a mutation and worth the space
because the failure survives the standard red/green discipline entirely.

The ticket needed required-silences alongside its positives: a real namespace
must still offer members and not enum values; an anonymous member-enum key must
not leak; an unknown qualifier must offer nothing. Written before the fix, as
TDD asks. Red was 9 failed / 7 passed — **and the 7 that passed were exactly the
silences.** That much is expected and fine: nothing was being offered at all, so
every "must offer nothing" assertion passed for free.

The trap is what happens next. After the fix they are still green, and the habit
is to read that as confirmation. **It is the same population it always was.**
Nothing in the observation distinguishes

- *still correctly silent* — the guard works, from
- *still structurally incapable of speaking* — the assertion could never fail.

One of them was the second case, and remained so after the fix.
`an_anonymous_member_enum_key_offers_nothing` probed `CControlEntry::Type::`,
asserting a value was absent. That spelling **resolves to nothing in the index at
all** — the real key is `Control::CControlEntry::Type` — so the assertion could
not have failed for any implementation whatsoever. It was vacuous twice over: a
silence test that passed for free, naming a key that did not exist.

It was found only by a SECOND mutation: removing the anonymous filter from the
code under test left every test green. The guard the comment described was never
being exercised.

**Countermeasures.**

- **A silence test's green tells you nothing until you have made it go red.**
  Mutate the guard it names — delete the filter, invert the condition — and
  require that specific test to fail. A silence assertion that no mutation can
  break is a comment with a `#[test]` on it.
- **Check that the subject of a negative assertion EXISTS.** "X is not offered"
  is vacuous when X could never be offered. Assert the lookup resolves first,
  then assert what is done with it. This is the same shape as a `grep -v` over a
  file whose name is misspelled.
- **Prefer a structural property to an instance** where one is available. The
  replacement asserts over the whole shipped typedb that *no* anonymous enum is
  reachable by any source spelling — which cannot be satisfied by naming a key
  that does not exist, and which found the real mechanism: `source_names`
  excludes anonymous entries entirely, so `lookup_source_enum`'s own
  `!anonymous` filter is a second line that currently CANNOT FIRE. The comment
  had been wrong about which line was doing the work.
- **Give the structural assertion a positive control.** Aimed at named enums the
  same code reports 630 reachable and fails with the list, so the instrument is
  known to be able to observe a failure.

Note how this differs from the ordinary red/green rule. Red-then-green already
covers the POSITIVE cases — they were 9 genuine reds. Silences are the half
where red-then-green gives no signal by construction, because their red and
their green look identical. **Every required-silence assertion needs its own
mutation, or it is unverified no matter how disciplined the surrounding
workflow.**

Related and reinforcing, from the same day: a negative CONTROL must fail for a
different reason than the positive case would (B097 — `zzz_no_such_route`
failed through the same 64-byte truncation as every route it was controlling
for, so it could not discriminate). Both are the same underlying error — an
assertion that cannot distinguish the world where the code is right from the
world where it is broken.

## A gate can measure coverage and totals on different axes, and pass on both

The suite gate withholds `SUITE-VERIFIED` from a filtered run, on the correct
reasoning that a subset is not a suite total. It detects filtering with
`names_a_target`, which inspects **cargo's** flags.

`just rtest -- --test b089_funcdef_call_site` passes no cargo flags at all.
Everything after `--` goes to **libtest**, which reads `--test` as a test-NAME
substring. Nothing matches it. Every one of the 30 binaries still launches,
still runs zero tests, and still prints a summary. So:

- coverage: 30/30 binaries reported — complete
- failures: 0 — clean
- exit code: 0 — clean
- total: **0 passed**

and the gate printed `SUITE-VERIFIED: 0 passed; 0 failed; 12 ignored; 30/30
binaries reported`. Every individual check was correct. The run was complete,
was clean, and was worth nothing.

**The mechanism is that completeness and the total were measured on different
axes.** Coverage was measured per-binary (did each one report?) and the total
was measured per-test (how many ran?). A name filter moves only the second.
Any check that reads one axis is blind to a failure that lives in the other,
and `names_a_target` was reading a third axis again — the caller's *intent*,
inferred from cargo's argv — rather than either measurement.

The honest signal was already in the output and unparsed: libtest prints
`37 filtered out` on every summary line, and the gate's `SUMMARY` regex stopped
at `ignored`. The data that contradicted the verdict was in the same string as
the verdict.

**Why this one is worth more than an ordinary bug.** A gate's whole job is to
be the thing you trust when you cannot check for yourself, and this failure is
in the direction that gets trusted. A red gate gets investigated. A gate that
says `0 passed` in the same breath as `SUITE-VERIFIED` gets quoted into a merge
commit — and would have been, had a lane not mentioned it as an aside after
declining to fix it.

**Generalisation.** Wherever a gate reports both a *scope* and a *magnitude*,
check that a single defect cannot move only one of them. Ask specifically: what
input makes the magnitude zero while the scope stays complete? If the answer is
"a filter the scope check cannot see", the scope check is measuring intent
rather than outcome.

**Countermeasure that survives.** Detection now comes from libtest's own
`filtered out` count — an *outcome* of the run, not an inference from its argv
— so it cannot be defeated by a caller that filters through a channel the gate
does not know about. The selftest carries the mutation (disable the detection,
the check goes red with the exact message) and a positive control (an
unfiltered clean run must still EARN the marker), so a future change cannot
make the check pass by rejecting every run — the failure mode this document
records under vacuity.

Related: "Mechanism 4: the presentation dropped the rows, and the presentation
looked fine" — same family, one level up. There the rows existed and were not
shown; here the number existed and was not read.

## Piping a gate to `tail` destroys its exit code — and why that did not cost us

Found by lane-unused during B093, self-reported. Its first RED run was

    just rrun ... 2>&1 | tail -60

and the shell reported the exit status of `tail`, which is 0, over a cargo run
that had actually exited **101**. It caught this only because it read the body
rather than the status.

That is the **third** distinct false-green mechanism in one day, and each was a
different axis:

1. `--test <name>` went to libtest as a name filter — coverage complete, total
   zero, every check passed (fixed in 586d18b).
2. ENOSPC on the build box — `just rtest` exited 0 over a build that failed
   (B106).
3. This one — the exit code never survived the pipeline at all.

**The rule.** Never pipe a gate command. Redirect to a file and capture the
status, then read the file:

    LSP_REMOTE_HOST=x-tuf python3 scripts/remote-build.py test > /tmp/gate.txt 2>&1
    echo "exit: $status"        # zsh; $? in bash
    tail -3 /tmp/gate.txt

**But the interesting half is why this batch survived it anyway**, including in
the coordinator's own merges, every one of which piped `just rtest` to `tail`.

The answer is that the evidence was never the exit code. It was the printed
`SUITE-VERIFIED` line, which `scripts/test-suite.py` emits *only* on a run that
passed every check, and which carries the totals and the binary count. A run
that dies produces no marker, so a pipeline that has thrown the status away
still gets **nothing** rather than a smaller number. That is not luck — it is
the documented design intent, stated in that file:

> That is what makes the gate survive the second mechanism: a lane whose
> pipeline discards the exit code still gets NOTHING from a broken run rather
> than a smaller number, because the line it greps for is absent.

**Generalisation, and it is the durable part.** Three independent false-green
mechanisms landed on this gate in a single day, from three directions nobody
enumerated in advance. None produced a bad merge. What held was not that we
predicted them — we predicted none of them — but that the gate was built so its
*positive* signal is expensive to forge: a single line, printed at one place,
only on a fully clean run, carrying numbers that have to reconcile against the
previous run.

So when you build a check, ask which direction is cheap to fake. A check whose
"pass" is the ABSENCE of something — a zero exit, an empty diff, no output —
can be forged by anything that breaks the machinery, and every one of the three
mechanisms above did exactly that. A check whose pass is a specific positive
assertion that must reconcile with an independent number cannot be forged by
breakage, only by a working system telling the truth.

The corollary for reviewers: **reconcile the total, do not just read the word.**
2601 + 15 = 2616 is the check. `SUITE-VERIFIED` on its own is a word.

## A partial instrument looks exactly like a clean result

Found by lane-unused during B099, self-reported, and distinct from the pipe
mistake though they landed in the same session.

It grepped a clippy log that was **still being written**, counted 4 warnings,
and nearly reported "only 4 warnings, all pre-existing". The finished run had
**79**.

**Why this is nastier than a truncation.** The output was not corrupted and not
visibly cut off. It was just *smaller* — and for a warning count, smaller is
exactly what a good result looks like. There is no shape to the wrong answer
that distinguishes it from the right one. Compare a crashed test binary, which
produces no summary line and is therefore loud.

Same family as two earlier entries; it now has three members:

- reading a log mid-write (this one)
- `grep -i oom` matching `r-oom-s` in an unrelated service name, producing a
  confident report of OOM kills that never happened
- `head -6` / `| tail -6` over a result set whose interesting rows were outside
  the window

In every case the instrument answered a question **narrower than the one being
asked**, and the narrow answer was well-formed.

**The countermeasure that worked, and it beats "wait longer".** Waiting is
unfalsifiable — you cannot tell from the output whether you waited enough. The
lane compared warning **distributions** between two *complete* runs: same 79
warnings, same 25 files, none in the touched lines. That is a differential, and
a differential cannot be satisfied by an instrument that simply had less to say,
because "less" moves the two sides apart.

**Generalisation.** When a measurement's failure mode is *fewer rows*, an
absolute count cannot detect it and no amount of care in reading it will. Reach
for a comparison against a second complete measurement, or a number that must
reconcile against an independent one — the same principle the suite gate relies
on. A pass that must RECONCILE is expensive to forge; a pass that is merely a
small number is free.

## Never `git pull` in this repository — I did, and it rewrote 782 commits

My own mistake, same session, recorded because the recovery is not obvious and
the next agent will be tempted by the same reflex.

Other sessions commit to this master **in the same local checkout**, not through
a remote. `origin` is a GitHub repo that is **782 commits behind**. So `git pull
--rebase`, run reflexively to "catch up" before committing, does not sync
anything — it begins replaying the entire local history onto a months-old
`origin/master`, starting from commits as old as B023. It conflicted on
`tests/cli_check_tests.rs` immediately.

**The compounding error was mine and worth naming separately:** I had chained
`git pull --rebase --quiet 2>/dev/null` with `&& git add -A && git commit` in a
single command. `--quiet` and the `2>/dev/null` hid the conflict, and the
unconditional `git add -A` then committed a file containing `<<<<<<<` markers as
though it were my documentation change. Two habits that are individually
harmless — silencing a command's chatter, and staging everything — combined into
committing a broken tree under a message that described something else entirely.

Recovery, which was clean: `git rebase --abort` restored the pre-pull HEAD
exactly, markers and all gone, tree clean. Nothing was lost except the commit I
had just made, which was trivially re-applied.

**Rules.**
- Do not `git pull` here. To pick up another session's work, it is already in
  your checkout; in a worktree use `git rebase master`.
- Never silence a command whose failure should stop the chain, and never follow
  a merge/rebase with an unconditional `git add -A`. Check for markers first —
  `git status` reports `both modified`, and `git diff --check` finds them.

## When the subject is an ABSENCE, the red run cannot validate the test

Found by lane-unused during B107, and it unifies three things this document had
been recording as separate incidents.

Red/green is the repo's standard defence against a test that proves nothing: if
the test does not fail before the fix, it is not testing the fix. That defence
has a hole, and the hole is exactly the class of test we write most often here.

`a_workspace_class_is_not_a_namespace` PASSED during the red run. Not because
it held — because before the fix, `Holder::` offered nothing, `Ns::` offered
nothing, and *everything* offered nothing. An assertion that a class does not
spill its members into namespace completion is satisfied for free by a world
where nothing completes at all.

**The mechanism, stated generally.** When a test's subject is an absence — "no
diagnostic", "offers nothing", "this name is not in the list" — the pre-fix
state is absence *everywhere*, so the assertion passes for a reason that has
nothing to do with the property it names. And a test that passes in both the
red and the green state is indistinguishable, by inspection, from one that
genuinely holds. Red/green cannot separate them, because red/green's whole
signal is the transition, and these tests never transition.

That is the same mechanism as:

- **B088's vacuous silence** — the ctor/dtor filter could be deleted with the
  whole suite staying green, because no discriminating input existed.
- **B097's non-discriminating control** — `zzz_no_such_route` failed through the
  same 64-byte truncation as every route it was controlling for.
- **B071's first regression test** — written where `compute_diagnostics` runs no
  unused analysis at all, so it could not fail either way.

Three incidents that read as three coincidences are one defect in the method.

**The operational rule, which is the valuable half.** Do not try to judge which
absence-assertions are suspicious — that requires the insight you are trying to
check. Instead:

> **After a red run, mutation-test every test that PASSED.**

The red run hands you the list for free. Those are precisely the tests whose
teeth are undemonstrated, and the set is usually small. For each, break the
thing it guards and confirm that test — specifically that one — goes red. In
B107 the mutation was replacing the `namespace_is_declared` gate with `true`,
which made `Holder::` spill `[("Method", Function), ("field", Variable)]`, and
exactly one test caught it: the one that had passed vacuously.

This is mechanical, needs no judgement about which assertions look weak, and
costs one mutation per free-passing test. That is why it will actually get done,
which is more than can be said for "think harder about your silence tests".

**Corollary for reviewers.** A red/green report saying "11 failed, 4 passed" is
not fully good news. The 4 are the ones to ask about.

## The pre-flight sweep: every gate, asked what its silence means (B139)

Three infrastructure conditions in one batch were reported as code defects —
B106's full disk as "your code does not compile", a name-filtered run as a
green summary, B132's peer merge as diff-marker noise. **The fix was never in
the checker that fired.** So every gate and probe entry point was swept against
one question: *what condition could make this measure nothing, or the wrong
thing, and still return a verdict?*

Recording the entry points that turned out FINE matters as much as the fixes:
without it the next sweep re-derives the same conclusions, and "we checked" is
not a durable artefact.

| entry point | verdict | basis |
|---|---|---|
| `clippy_delta.py` | **no finding** | already refuses on zero workspace units, an empty baseline, zero findings against a non-empty baseline, a command mismatch, and a toolchain mismatch — the toolchain is *enforced*, not merely recorded |
| `test-suite.py` | **no finding** | zero-binary exit, green-zero refusal, per-binary reconciliation against an enumeration from the same run, `filtered out` detection. HOME isolation is `test-hermetic.py` by design, and the boundary is documented |
| the Rust suite | **no finding** | grepped for tests that self-skip on a missing fixture (`if !path.exists() { return }`): **zero**. A silent skip would be invisible in a green total |
| `remote-build.py` | fixed | ssh's own `255` carried no note, so a connection failure read as a verdict from the remote command |
| `probe_const_receiver` | fixed | run-level gate |
| `probe_member_visibility` | fixed | run-level gate; also a **copied** `classify` whose docstring claimed to delegate |
| `probe_engine_const_receiver` | fixed | run-level gate; **no post-run stranded-fixture sweep at all** |
| `probe_shared_modifiers` | fixed | run-level gate (same shape; mine) |
| `issue_repro_game`, `probe_enum_corpus`, `probe_export_binding` | already hardened | B118 / B123 / B127 |

### The row-level oracle was fine; the RUN-level gate was not

All four probes are exempt per-row: `build_verdict` requires two positive
markers, so a build that never happened yields AMBIGUOUS, never ACCEPT. **The
exposure was one level up.** Each hand-rolled its run gate as

    bad = [r for r in rows if r["expect"] != "UNKNOWN"
           and r["verdict"] != r["expect"]]
    return 1 if bad else 0

which exits 0 for a run that measured nothing, two ways:

- **`--only <subset>` can filter the controls out.** `bad` is then computed
  over an empty control set, is empty, and the run writes its corpus and exits
  0 having verified no channel at all.
- **The transport can die midway.** Every later row becomes AMBIGUOUS,
  *no control mismatches because controls that never ran cannot mismatch*, and
  a full column of AMBIGUOUS is exported as the measured answer with a success
  status.

That second clause is the sharp one: **a control proves nothing by not
failing.** An absent control and a passing control are the same value in that
expression.

Now one shared `fleet.probe_run_problems`, refusing on: no rows, no control
rows, a mismatched control, an ambiguous control, or every measured row
ambiguous.

### What it deliberately does NOT refuse

**A single ambiguous row beside passing controls proceeds.** B096 claim 2
shipped two of them and they were honest results. Making AMBIGUOUS fatal would
have been the cure-worse-than-disease trap: it passes any suite that only
checks that bad runs get refused, and it destroys every legitimate partial
result. Both halves are pinned, and an over-condemn mutation for each is in
`check_fleet_verdict_nonvacuous.py`.

The same judgement elsewhere in this sweep: ssh's `255` gets a **note, not a
refusal**, because 255 is a status a remote command may legitimately choose —
data for the reader, not a decision taken from them.

### One lane's infrastructure failure was another lane's measurement

`probe_engine_const_receiver` had no post-run stranded-fixture sweep, so it
could exit 0 having left a `lsp-issue-repro-*` fixture installed — which turns
every *subsequent* probe's rows into skipped/AMBIGUOUS (B077). The failure and
the false reading were in different processes, hours apart, which is why no
single run looked wrong.

## The remote gate cannot see the git index — it copies FILES (B132)

Found by incident, not review. A peer session began merging its lane into the
shared main checkout while the coordinator ran `just rclippy-delta` on what it
believed was its own post-merge master. The gate rsynced the peer's
**conflicted working tree** and returned:

    ADDED src/typecheck/checker.rs (new): encountered diff marker
    clippy checked zero workspace units, so it found nothing because it looked
    at nothing.

**The verdict was right** — B083's zero-units guard refused correctly. **The
diagnosis was the defect.** Nothing in that message says another session is
mid-merge in your checkout, so it reads as *your code does not compile*.

**This is the third infrastructure condition in one batch reported as a code
defect**, after B106's full disk on x-alpha and a name-filtered run printing a
green summary. The shape is identical every time: *the tool reports what it
saw rather than why it could not see.* Worth naming as a class, because the fix
is never in the checker that fired — it is a missing pre-flight that would have
named the real condition first.

The mechanism, so it is not re-derived: **the remote gate does not and cannot
see the git index. It copies the FILES.** An unresolved merge, a half-written
edit, any dirty tree is measured as source, and `<<<<<<<` arrives at the
compiler as syntax.

### Refuse on the OPERATION, never on the tree

The guard refuses on `MERGE_HEAD` / `REBASE_HEAD` / `rebase-merge/` /
`rebase-apply/` / `CHERRY_PICK_HEAD` / `REVERT_HEAD`, and **never on a dirty
tree**. Gating uncommitted work is the normal lane workflow — every lane does
it on every run — so a guard that fired on dirty would be a far worse
regression than the defect it fixed. `BISECT_LOG` is deliberately excluded: a
bisect checks out real commits and cannot produce a conflict marker, so
refusing there costs a legitimate workflow to prevent an impossible failure.

**Operation state is PER-WORKTREE and resolving it by hand would have been an
outage.** A merge in the main checkout lives in `.git/`; a linked worktree's
lives in `.git/worktrees/<name>/`. Joining `root/.git` by hand attributes the
coordinator's merge to every lane and stops all of them at once. Asking
`git rev-parse --absolute-git-dir` is what gets this right, and a mutation
swapping it for `--git-common-dir` is in the suite precisely because that
change looks harmless.

### Two vacuity shapes this ticket produced, both found only by mutation

**1. A test block placed after the check that reads its results.** The B132
cases appended to `problems`, but the block landed BELOW `if problems: return
1`, so nothing ever examined them. **Every one of ~120 lines of new test passed
unconditionally.** The mutation run scored 0/10 with a green baseline — and
*that* pattern, everything missed while the baseline passes, is the signature
of a suite whose assertions are not reached at all, as against the usual one or
two vacuous guards.

**2. A test that derives its expectations from the table under test.** The
sentinel loop iterated `GIT_OPERATION_SENTINELS` itself, so deleting
`CHERRY_PICK_HEAD` from the table did not fail the suite — it simply stopped
testing it. **A test that reads its expectations from the thing under test
cannot see an omission**, only a malfunction. The required operations are now
asserted by the filename git actually writes, independent of the table.

Both were invisible by inspection; both were found by mutation, in a ticket
whose author had written the mutation-testing rule two tickets earlier.

## The parity tool reported PARITY-CLEAN for builds that never happened (B127)

The B123 finding, followed to every consumer that had been deriving build
health privately. Four were, and **measuring each one rather than migrating
them all is what found the one that mattered.**

| consumer | exposed? | why |
|---|---|---|
| `probe_enum_corpus.run_batch` | yes | its fatal guard read `load["error"]`, which is empty for a failed build, so a broken run looked *inconclusive* rather than *broken* |
| `fleet_dogfood.probe_one` + `verdict` | **yes, worst** | see below |
| `probe_export_binding.classify` | no | its affirmative answer is the consumer PRINTING a start token — a positive control |
| `probe_export_binding` exporter | no | same, an `exporter ready` token |

**The worst one was the tool that generates the parity findings.** `verdict()`
computes `gerr == 0` from the diagnostic list, and an empty list satisfies that
whether the build was clean or never ran. So a build that did not happen was
reported `PARITY-CLEAN` — *the LSP and the game agree, both clean.* And with
the LSP holding findings it reported `FP?`, **manufacturing false-positive
evidence against the LSP out of a build that produced nothing.** That is the
B118 polarity landing in the one place whose entire output is claims about
where the LSP and the engine disagree.

### Four values, three policies — which is why it could not be a boolean

The same `build_verdict` serves three different callers precisely because
`REJECTED` and `BUILD-ERR` are distinct:

- `probe_enum_corpus` **expects** REJECTED every run — its sentinel is a
  deliberate error — and treats only NOT-RUN/BUILD-ERR as fatal;
- `probe_export_binding`'s exporter must be **CLEAN**, since every variant
  lives in that one plugin;
- `issue_repro_game` reports all four, because for it the verdict *is* the
  answer.

A boolean `build_ok` cannot express those three policies, and any single choice
of where to draw its line would have been wrong for two of the callers. **When
one predicate needs different thresholds per caller, that is evidence the
predicate is under-modelled, not that the callers should compromise.**

### A positive control is what makes a consumer exempt, and it is worth naming

Two of the four consumers were never exposed, each because its affirmative
answer requires a token the plugin has to *print*. That is the same structural
exemption as `probe_enum_corpus`'s sentinel (B123): **ask what an oracle's
silence means.** Where silence is the negative answer, no failure mode can
impersonate success, and no migration is needed. Both exemptions are now stated
in the code and pinned by a case, because "it is exempt" is exactly the claim
that rots once the next reader has to re-derive it.

### The empty-window trap is recurrent, not a one-off

B118 was caught testing "the build never ran" with an EMPTY window, which lets
a broken build-slicer look correct for free. Writing fresh fixtures for B127
**reintroduced the same two gaps** — the empty window, and
`Script compilation failed!` tested only alongside a located error that had
already decided the verdict — and mutation testing caught both again.

Knowing a hazard does not stop you writing it; only the mutation run does. The
real log is shared and always carries other traffic, so the correct fixture for
"our build is absent" is **another lane's build**, never nothing.

## A success reply from the transport is not a report on the build (B123)

Follow-up to B118, and the more useful half of it. `probe_enum_corpus.run_batch`
guards its batch with

    if loaded.get("error"): raise SystemExit(...)

added by B097 with exactly the right reasoning: a failed load must stop the run,
"otherwise every name comes back UNDETERMINED and the run looks merely
inconclusive rather than broken."

**Measured 2026-09-01: that guard cannot see a failed build.** For a plugin
whose dependency could not be resolved, RemoteBuild answers

    {"error": "", "data": "Plugin loaded"}

while the engine logs `Couldn't find required plugin dependency "..."` and then
`Loaded plugin` anyway. So `loaded["error"]` is empty, the batch proceeds, no
name is analysed, and the run produces the precise outcome the guard exists to
prevent — through the one door it does not cover.

**The general shape: the transport answering "I delivered your request" is a
different proposition from the engine answering "your build succeeded", and a
guard written against the first cannot enforce the second.** The two were
conflated because, before B097, the load call was inert and its reply carried
no information at all — so the reply became load-bearing the day the transport
started working, without anybody revisiting what it actually asserts. *A value
that was decoration becoming meaningful is a change in the contract, even
though no line of the consumer changed.*

### An oracle that demands a positive control is exempt, and it is worth saying why

B123 opened as "the last consumer still on the severity-filtered reader, so it
cannot notice a build that never ran". Measuring it first showed that
conclusion is wrong, for a structural reason worth generalising:

- B118's oracle asked *"did anything complain?"*, so **silence was its
  affirmative answer** and every failure mode impersonated ACCEPT.
- This oracle asks *"did the SENTINEL complain?"* (B068). The sentinel is a
  name the engine must reject, so its diagnostic is a **positive control that
  has to be present before any silence counts**. A build that never ran
  produces no sentinel, every name degrades to UNDETERMINED, and the writers
  already refuse to emit a corpus containing one.

**Silence is the negative answer here, and that is the whole difference.** The
filtered reader could drop the sentinel entirely and the failure would still be
safe — noisy and wrong-looking, never a confident wrong row. Which is the test
to apply to any oracle: *ask what its silence means.* If silence is the
affirmative, every outage in the stack is indistinguishable from success.

**Correction (B127):** I reported that this left `read_log_window` with no
production callers, and B128 was filed on that. It was wrong —
`fleet_dogfood.probe_one` was still calling it, and that site turned out to
hold the worst instance of the very defect (see B127 below). A claim that a
grep found "the last" of something is worth re-running before it is filed as a
ticket; mine had matched a local variable of the same name.

`run_batch` moved to the raw reader anyway, but for a different and much
narrower reason than the ticket assumed: attribution never needed it (both
readers yield the same ERR set, pinned by a test asserting identical verdicts),
while the **build-health check is impossible without it** — `Starting build for`
and `Loaded plugin` are both `[ TRAC]`, so the build bracket is literally
invisible in the filtered window and `build_slice` returns zero lines from it.

The reusable rule: **when a ticket says a consumer is unsafe, measure the
consumer's oracle before migrating it.** The migration here was right and the
stated reason for it was wrong, and shipping it under the wrong reason would
have left the next reader believing attribution had been at risk.

## Three ways a probe's silence was scored as a passing answer (B118)

Found by lane-resolver during B096 claim 2, and it is the ABSENCE hazard above
wearing an instrument instead of a test. Same mechanism, one level down: not "a
test that passes for the wrong reason", but **an oracle that cannot tell its
own failure from the answer it is looking for.**

`issue_repro_game.probe` asks the live game whether a construct compiles. Three
independent channels each turned *no signal* into ACCEPT:

1. **The window it read dropped every `[ TRAC]` line** — and both build markers,
   `Starting build for "<id>"` and `Loaded plugin '<id>'`, are TRAC. So the
   oracle could see whether anything *complained*, never whether the build
   *happened*. A build that never ran and a build that ran clean were the same
   observation.
2. **A location-less engine error matched no bucket at all.** The diagnostic
   parser matches `path (line, col) : SEV : msg`; an error without a location
   matches nothing, so it missed `game_errors`, `foreign_errors` AND
   `unattributed_errors` — which left `contended` False too. Every guard
   downstream was derived from that one parse, so no guard anywhere fired.
3. **`Loaded plugin` is logged even after `Script compilation failed!`**, so
   its presence is necessary and *not sufficient*. This is the trap laid for
   whoever fixes channel 1: un-filter TRAC, test for `Loaded plugin`, and you
   have swapped one false green for another.

**The polarity is what makes this severe.** On a question whose entire subject
is ACCEPTANCE, every one of these failure modes is indistinguishable from the
affirmative answer. A transport outage, a typo in a plugin id, a missing
dependency — all report "accepted".

It was caught only because a fixture with a deliberately wrong dependency id
scored ACCEPT. Nothing else in that run looked wrong, and the run had passing
controls: a positive control and a negative control both behaved correctly
*while the oracle was blind*, because neither exercised a build that failed to
run. **Controls prove the instrument can discriminate the cases they contain,
and nothing about the cases they omit.**

### The conceptual fix: two different answers must not share a verdict

The defect is not really three parsing bugs. It is that **"the engine refused
this construct" and "this build did not happen" were the same value.** Once
they are the same value, no amount of parsing care helps, because the caller
has nowhere to put the distinction. So the fix is a third verdict —
`BUILD-ERR`, kept deliberately distinct from `REJECTED` — plus slicing the log
by the build's **own start/end markers** rather than by severity or by time,
which is what makes the attribution structural instead of a guess.

Generally: *when an instrument's failure mode and one of its answers would be
recorded identically, that is a design defect in the verdict set, not a
detection problem.*

### Testing a MISSING signal: measure the pair, then mutate

A red run cannot validate any of this, for the reason the section above gives —
the pre-fix state is silence everywhere. Two rules, both learned the hard way
here:

**Assert both halves of a pair and that they DIFFER.** A guard that condemns
everything passes a one-sided assertion just as well as a correct one. Every
B118 case measures the good half and the bad half and asserts the verdicts are
not equal.

**Then mutation-test, because the pairs were not enough.** Restoring each half
of the original defect in a throwaway copy found that **4 of the 9 new guards
were vacuous**, all for the same undramatic reason: an *earlier* check in the
verdict chain already decided those cases, so the guard under test never ran.

- "never ran" was tested with an EMPTY window, so a broken build-slicer looked
  correct for free — the real log is shared and always has other traffic in it.
- `Script compilation failed!` was only ever tested alongside a located error,
  which had already decided the verdict.
- "started but never loaded" was not tested at all.
- a located error being double-counted into the raw bucket was not tested at
  all.

None of those were visible by inspection, and all four are now cases.
`just probe-cleanup-mutants` runs 21 mutations, 0 missed. **The lesson is that
a suite of pair-tests is still only a claim until each guard has been shown to
fire on its own.**

## A merge run inside a lane's worktree does nothing and reports success

My own mistake, and the second false-green I authored in one night while
coordinating the effort to eliminate them.

I ran this as a single compound command from the main checkout:

    cd .worktrees/unused && git rebase master && git merge --ff-only lane/unused

The `cd` persists across the `&&` chain, so the **merge also executed inside the
lane's worktree**. Git reported:

    Already up to date.

Which is true, and useless. That worktree *is* `lane/unused`, so merging the
branch into itself is a no-op. Master never received the commit. The command
exited 0, printed a reassuring sentence, and did nothing.

I caught it only because the next gate's **binary count** did not match what I
believed master contained — 31/31 where I expected 30/30, because a peer
session's merge had landed in between. The arithmetic caught it, not the git
output.

**Two things make this worth recording separately from the `git pull` entry.**

First, the failure mode is a *success message*. "Already up to date" is what a
correct no-op looks like, so there is no shape to distinguish it from the case
where you genuinely had nothing to merge. Same family as the rest of this
document: the wrong answer is well-formed.

Second, and worse: the lane had **uncommitted work in that worktree** — a red
test for its next ticket. It survived only because `git rebase` refuses on a
dirty tree touching the affected path. That is luck, not design, and the lane
was right to raise it rather than let it pass.

**Rules for anyone coordinating lanes.**

- **Merge only from the main checkout.** Never `cd` into a lane's worktree to
  merge. If you must inspect one, do it in a separate command so a stale `cd`
  cannot leak into a mutating one.
- **Never rebase inside a lane's tree while that lane is running.** Ask the lane
  to rebase; it knows what it has uncommitted and can park in seconds. Or merge
  with `--no-ff`, which needs no rebase at all — that is what B107 finally
  landed as, specifically so the lane's in-flight B108 work was never at risk.
- **Verify the merge landed rather than trusting the message.** `git merge-base
  --is-ancestor <branch> HEAD` answers it directly, and the gate's reconciling
  total answers it independently.

The general lesson is the same one the gate teaches: a mutating command that can
succeed by doing nothing needs a check on the *outcome*, not on its exit status
or its output.

## A missing `cd` does not fail — it builds a different repository

Found by lane-unused during B108, and retracted-into rather than reported: it
first told me my merge had destroyed its uncommitted test, then traced the
mechanism and withdrew the accusation. The withdrawal is what makes this entry
possible.

An agent's cwd **resets between tool calls**. Three of its commands omitted the
leading `cd` into the lane worktree, so they ran from the main checkout instead:

- `grep -c check_b108 tests/cli_check_tests.rs` read the *main checkout's* copy,
  which of course lacks the lane's uncommitted test. Count 0. It read that as
  "my work was destroyed" — and it was not; the file was intact the whole time.
- Worse: `just rrun cargo test --test cli_check_tests b108` also ran from the
  main checkout. It synced a **different tree** to the build box, where the test
  does not exist, and reported `test result: ok. 0 passed; 0 failed; 127
  filtered out` with **exit 0**.

**That is a fourth false-green route in one day**, and the same signature as the
others: a green exit over a run that tested nothing. It was caught only because
"0 passed" is nonsense for a filter that had matched 1 test five minutes
earlier. The exit code was 0 and would have satisfied any gate trusting it.

**The mechanism, stated generally.** A missing `cd` produces no error. It
silently retargets the work at whatever tree the shell is in, and every
downstream signal stays plausible — the sync succeeds, the build succeeds, the
tests compile, the count is merely smaller. This is the "partial instrument"
mechanism again, except the instrument is pointed at the **wrong repository**
rather than being incomplete.

Two false conclusions follow from it, and the lane drew both: that its file was
gone, and that a green run had verified something. The first is loud and wrong;
the second is quiet and wrong, which is worse.

**Countermeasures, cheapest first.**

1. **Read the run banner.** `remote-build.py` prints the tree on every run, and
   it now names the **branch** as well as the slug precisely because of this
   incident: `remote-build: x-tuf [enum-189cb63a] branch=lane/enum j=8 :: ...`.
   A hash mismatch is easy to skim past; `branch=master` when you meant
   `lane/enum` is not. Checking it costs nothing and makes the class impossible.
2. **Use absolute paths.** The lane's own environment notes say to, for exactly
   this reason, and it used relative ones anyway. So did I, in the entry above
   this one. Knowing the rule is not the same as following it, which is why
   countermeasure 1 exists — it works without anyone remembering anything.

**And the retraction is the durable part.** Two separate faults happened to look
like one, and the lane attributed its own to me before checking. It then went
and found the evidence — the slug line in its own logs, `[unused-3616122f]` on
the two correct runs and `[lsp-openplanet-bf0363fb]` on the bad one — and
withdrew the specific claim while keeping the unrelated real finding about my
merge. Both halves matter: a retraction that also abandoned the true half would
have been as unhelpful as never retracting.
||||||| b6584a0

||||||| parent of 14bd3fb (docs(b101): a consumer that parses the rendered string is outside the compiler's net)

## "Add the field and see what breaks" cannot see a consumer that parses the rendered string

Found by lane-resolver during B101, whose whole method was the one the ticket
prescribed: add `is_const` to `SymbolKind::Function`, then let the breakages
tell you which consumers had been relying on the conflation.

The compiler found **26 construction sites and 2 exhaustive destructures**, all
of them mechanical, none load-bearing. The one consumer that actually broke was
**invisible to that method entirely**: B095's `implement_interface` code action
does not read the enum at all — it parses the *rendered* signature B094 emits.
Its `parse_signature` did `signature.split_once('(')` then
`rest.strip_suffix(')')`, which returns `None` on `void I::M() const`, and a
`None` there means **no code action is offered at all**. Not an error, not a
wrong stub — the menu entry silently disappears for every const method.

**Measured, not argued.** Reverting only that parser (mutation M2) and running
the family's tests: 5 failed, 14 passed. The 5 were the ones written for B101;
all 14 pre-existing tests were green. So neither the compiler nor the existing
suite could have surfaced it — the breakage was found by *reading the consumer
list* before writing any code, and only afterwards confirmed by mutation.

**Generalisation.** A field added to a type is checked by the compiler at every
site that *names the type*. A consumer one serialisation away — it takes a
string another module rendered — is outside that net, and its failure mode is
"produces nothing" rather than "produces something wrong", which no assertion
about existing behaviour will catch. When a change alters what a module
**renders**, the consumer list is everything that parses that rendering, and it
has to be enumerated by hand. Grep for the render function's callers, not for
the type.

**Corollary for scope discipline.** "Find out what breaks" is a *lower bound*
on the affected set, not the set. Treating a clean build plus a green suite as
the completion criterion would have shipped B101 with a silently dead code
action.

## The tool that swallowed the reason: a full disk rendered as "your code does not compile"

B106 was filed as the second of the three false greens above: *`just rtest`
exited 0 while the remote build had failed with ENOSPC*. The first move was to
reproduce it rather than to fix the wrapper it accused, and **the accusation
did not survive the measurement.**

Five paths were driven with a real, isolated ENOSPC — a loop-mounted 12 GiB
ext4 image standing in for the build box's filesystem, so the box itself was
never at risk and the free space was set exactly:

| input | local exit | marker |
|---|---|---|
| build from cold, 374 MiB free | **1** | withheld |
| fully built tree, 712 KiB free | 0 | printed — and CORRECTLY: a fresh tree writes almost nothing |
| one source touched, 105 MiB free | **1** | withheld |
| rsync into a filesystem with 0 bytes | **11**, propagated by `die` | n/a |
| remote script exiting 0/1/42/101, on both boxes | **0/1/42/101** | n/a |

So `ssh` returns the remote status, `run_ssh` returns `ssh`'s, and every layer
above passes it up. **The reported symptom is mechanism 3, not mechanism 2** —
piping the gate to `tail` and reading `tail`'s status — which is
indistinguishable from an ENOSPC false green at the point of observation and
needs a completely different fix. Recording that here because the ticket will
be re-read: *measure which of two mechanisms you have before you change the
code one of them accuses.*

**But the reproduction found a different defect, and a worse one.** This is the
whole output a lane got from the third row above:

    Compiling openplanet-lsp v0.6.0
    error: could not compile `openplanet-lsp` (lib) due to 1 previous error
    error: `cargo test --no-run` failed (exit 101)

The words `No space left on device` appear nowhere. They were not missing from
the machine — rustc emitted them, into the `compiler-message` records on
cargo's stdout, which the gate reads for `compiler-artifact` records and drops
everything else. **The one line that named the real dimension was discarded by
the tool whose job was to report it**, and what reached the reader was a
statement about their code.

That is the "an error message that names the wrong dimension" hazard, and note
how much it costs *here* specifically: it does not merely waste time, it
manufactures the false-green report. A lane that is told its tree does not
compile, on a box where the tree compiled ten minutes ago, is being pointed
away from the machine at exactly the moment it should look at it.

**Generalisation.** A parser that reads a structured stream for one record type
is silently discarding the others. Ask what is in the records you skip — and
ask it especially on the FAILURE path, where a parser written for the success
case is running for the first time and nobody has looked at its output.

**The disk check that was added, and the one that was rejected.** The obvious
countermeasure is a free-space floor that refuses the run. It was rejected for
the gate: row two above is a *genuine* `SUITE-VERIFIED: 2601 passed ... 30/30`
earned with 712 KiB free, and refusing it would be a false negative on a total
that was complete and correct. The gate warns instead, and its hard failure is
an OUTCOME — `No space left on device` in cargo's own output — in the same way
the filtered-run check reads libtest's `filtered out` count rather than the
caller's argv. The floor lives in `remote-build.py`, before anything is synced,
where refusing is cheap and the alternative is filling someone's build box.

**THREE of this fix's own controls were vacuous, and mutation testing is what
said so.** None would have been found by reading.

1. The disk-full check excluded libtest's lines by shape, and the control was
   "a passing test named `writes_fail_with_no_space_left_on_device` must not
   fail the suite". Removing the exclusion left the selftest **green**: a test
   name cannot contain spaces, so `No space left on device` never matched it
   in either direction. The control could not discriminate — the same defect
   this document records for `zzz_no_such_route`. The check is now scoped to
   the build preamble (everything before the first `Running` line, which is
   cargo's own output because `cargo test` builds before it runs), and the
   control is the *byte-identical* string moved to after that line.
2. The remote wrapper's slug check asserted that `/a/b` and `/a/c` get
   different slugs. Deleting the path hash left it **green** — those differ by
   basename alone. The collision the hash exists for is *same basename,
   different parent*, which is every lane worktree under `.worktrees/`. The
   assertion now names that case.
3. The clean-run control for "the disk check must not reject everything"
   started at the `Running` line, so its build preamble was **empty**.
   Mutating the check to flag every preamble line left the selftest green:
   there were no lines to flag. The control now carries the ordinary
   `Compiling` / `Finished` lines a real run has.

A fourth, of a different kind and worth its own note: the preamble scoping was
added and the selftest was NOT re-run before the mutation harness went off, so
the harness reported `baseline not green` and nine mutation results were
wasted. **Re-establish green after every change to the thing under test, before
mutating it** — a mutation run against a red baseline measures nothing.

A fifth, in git rather than in code, and the same shape as this document's
`git pull` entry: a resolution script raised an `AssertionError` on a line
number, and the `&&`-chained `git checkout --theirs .` after it ran anyway and
overwrote four of this file's sections with the branch's own copy. Recovered by
extracting the new section, `git checkout master -- <file>`, and appending.
**Never chain a recovery command after a step that can fail on its own terms.**

This is the operational half of "When the subject is an ABSENCE, the red run
cannot validate the test", above. B106's subject is a gate that wrongly
SUCCEEDS, so every assertion it can make has the form *the marker is absent* or
*the gate refuses* — the exact shape that passes for free. Three of them did.
The rule that found them is that entry's: **after a red run, mutation-test
every test that passed**, and the transferable refinement is the one below.

**The transferable rule.** A positive control has to be the negative case with
ONE thing changed, and that one thing has to be the thing the guard reads. Both
failures above were controls that differed from their negative case in some
*other* respect as well, so they passed with the guard removed. Write the
control by editing the negative input, not by writing a new one.

## A ticket that pre-authorises fixture churn makes its reds more dangerous

Found by lane-unused during B108, and it is a defect in how *I* write tickets
rather than in any code.

B108's body said, correctly and helpfully:

> Expect to update those fixtures' expected output as part of this ticket; that
> is the design, not a regression.

That sentence is true. B091 had deliberately pinned the current wording so a
later improvement would surface as a visible test change, and saying so spares
the next reader a needless investigation.

It also **pre-authorises the wrong response to every red the ticket produces.**

The lane's first cut over-reached: B108 reads as "a callable argument prints as
its return type", which sounds like it covers every callable, so the fix
rewrote all three callable shapes. Measured, only one of them ever lost
information:

| argument | types as | verdict |
|---|---|---|
| `this.Tick` — method reference | `void`, the **return type** | identity gone → fix it |
| `WrongArityOne` — bare function name | `Funcdef("WrongArityOne")` | already the author's name |
| `badHandle` — funcdef-typed value | `StringLess@` | already the author's name |

`b102_a_funcdef_handle_of_the_wrong_shape_is_reported` went red — and B102's
own comment records that pin as **load-bearing**, because it is what makes the
sibling fixture's correct-handle row falsifiable. The suite was doing its job.

**The trap is structural.** Once a ticket says fixture churn is expected,
"just update the pin" becomes the *frictionless* reading of any red under it,
including the reds that are the suite refusing an over-reach. The sentence that
was meant to save an investigation instead removes the friction that would have
forced one.

**So: a red under a churn-authorising ticket deserves MORE scrutiny than a
normal red, not less.**

**The discriminator that worked**, and it is cheap: ask whether the **old text
was wrong** — not whether the new text is defensible. `StringLess@` was never
the method's return type, so nothing licensed changing it. "Defensible" is
satisfied by almost any coherent output; "the old one was wrong" is a claim you
have to be able to support.

**A second-order instance in the same ticket.** The lane's own first boundary
test then asserted that a bare function name *should* render as a signature —
the same over-reach restated as an expectation, and it failed for the same
reason. Caught by running it, not by re-reading it. Its corrected version draws
its positive control from the *other* fixture, since only method references
changed; without that cross-fixture control, the "names are kept" assertions
would be satisfied by reverting the whole change — which is the vacuity failure
this document keeps returning to.

**When writing a ticket that expects pinned text to move**, name the *specific*
lines you expect to change and why, rather than granting blanket permission.
A blanket grant cannot distinguish the churn you predicted from the churn that
means you were wrong.

## A comment that says "before the fix" is a record of the past, not a report

B109 was filed, assigned, and cancelled without a line of code changing. The
whole ticket existed because two of us read a historical note as a live defect.

The b025 fixture carries:

    // Before the fix the LSP typed these expressions as the method's return
    // type and emitted
    //   error: argument 1 of `CoroutineFunc`: expected `void ()`, got `void`

That is B025's own record of the bug **B025 fixed**. A lane cited it as evidence
of a live flattening on the `ArgTypeMismatch` path; I filed a ticket on that
report without checking the code; the next lane measured and found the path
correct. Two people reasoned from prose, and the prose was describing the past.

**Why this comment shape is a trap specifically.** A good bug-fix comment quotes
the OLD, WRONG output — that is what makes it useful, because it lets a future
reader recognise the defect if it returns. So the most valuable comments in the
codebase are exactly the ones that contain a verbatim rendering of broken
behaviour, sitting in the file, indistinguishable at a glance from a
description of what the code does now. The two-word tense marker is the only
thing separating them, and it is easy to skim past when the quoted error looks
like the thing you are hunting.

**The measurement that settled it**, and it took minutes: dump the real
diagnostics from the fixture.

    b025:49  expected `void ()`     got `bool ()`   <- method ref, SIGNATURE
    b025:51  expected `void (ref)`  got `void ()`   <- method ref, SIGNATURE
    b025:53  expected `void ()`     got `string`    <- property read; type IS the fact

**Rules.**

- A comment is evidence about the code only if you have checked its tense. When
  quoting one as a defect report, quote the sentence BEFORE the error text too —
  that is where "before the fix", "used to", and "previously" live.
- Prefer running the thing over reading about it. The fixture that carried the
  misleading comment would have produced the true answer in one command.
- When writing such a comment, put the tense where it cannot be skimmed past:
  lead with `FIXED IN B025:` rather than burying "before the fix" in prose above
  a verbatim error string.

**A second-order note on how it propagated.** The lane reported it with enough
confidence that I filed on its word alone. Neither of us was reckless
individually — a lane reporting an incidental finding is behaving correctly, and
a coordinator filing a reported finding is too. But confidence is not evidence,
and it survives being passed between people while the evidence does not. A
report worth filing on is one that says what was MEASURED; "I noticed X" is a
lead. This document's other entries all end with a measurement, and this ticket
is what the file looks like when one is missing.

## A shared status file is a stale baseline with a plausible number in it

`docs/BATCH-STATE.md` carried `2745` while master was at `2802`. The gap was 57
tests from another session's B103 and B104, merged in the interval.

Two lanes hit it on the same night, and the failure is worse than "an old number
in a doc":

- **lane-enum (B081)** would have reconciled `2745 + 5` and reported `2750`
  against a measured `2807`. The arithmetic is right and the conclusion is
  garbage.
- **lane-resolver (B096)** would have compared a probe run against `2745` and
  seen a delta of `+57` for a change that touched only Python and docs.

**The sharp edge is the direction of the error.** A stale base makes a REAL
delta look like noise and makes NO delta look like a real change, and it does it
while every downstream step behaves impeccably. The reconciliation ritual this
batch relies on — old total + N new tests = new total, exactly — is what caught
the moved master both times, but only because both lanes had been told to gate
first and reconcile against **what they measured**, not against what they read.

**The rule: a number written by another process at another time is a hint, not a
baseline.** Gate before you write code, and reconcile against your own measured
base. The gate costs 30-60s on x-alpha. A false null costs a night.

The status file now says so in its first paragraph, above the numbers, because a
warning printed after the figure it qualifies is a warning nobody reads.

**The general shape, which is not specific to test counts:** any artifact that
records a measurement taken elsewhere is only as good as the interval since it
was written, and nothing in the artifact itself reveals that interval has
elapsed. This is the same defect as the stale clippy baseline, the same defect
as `game_route`'s docstring promising behaviour its own branch condition could
no longer deliver, and the same defect as a comment saying "before the fix". In
every case the text was TRUE when written and stayed plausible after it stopped
being true.

## A soft reset onto a moving ref is `git add -A` by another name

lane-unused squashed its B115 work with `git reset --soft master`. Master had
moved since the lane rebased — two docs commits, `71b13c5` and `5f54f85`. The
reset dutifully staged the difference between the lane's tree and the NEW
master, which meant `docs/BATCH-STATE.md` and `docs/verification-hazards.md`
were swept into the lane's commit **as reversions to their older content**.

The lane caught it on `git show --stat`, restored both from master, amended, and
reported it against itself. Final commit was exactly three files.

**Why this is the `git add -A` hazard wearing a different face.** Both are one
command that stages "everything different from here" against a reference the
author has not re-checked. `add -A` sweeps whatever is in the working tree;
`reset --soft <branch>` sweeps whatever the branch has done since you last
looked. Neither errors, both produce a commit that applies cleanly, and the
damage is *deletions of other people's work*, which no test in this repo would
catch — the reverted files were documentation.

**The tell is the file count.** A lane commit whose stat lists files the lane
never opened is wrong, and that check takes one command. `git show --stat` before
you report, every time, and reconcile the file list against what you actually
edited — the same discipline as reconciling a test total, applied to a diff.

**Prefer `git reset --soft $(git merge-base HEAD master)`** over
`git reset --soft master`. The merge base is where the lane actually diverged and
it does not move when master does.

## Design the instrument to report EXPOSURE, or a zero is uninterpretable

lane-enum widened the const-receiver rule to reach cross-file classes (B116),
instrumented the change across 151 plugins, and measured **zero** additional
emits. Read naively that is a broken instrument. It was not, and the reasoning
generalises to every error-class rule measured against shipped code:

**Shipped plugins compile in the game.** So for a rule whose subject is a
compile ERROR, the corpus CANNOT CONTAIN A TRUE POSITIVE. The fleet is not a
coverage measurement at all — it can only ever answer the FALSE-POSITIVE
question, and "zero would-fire" is that question's answer rather than a failure
to find anything.

**What made the zero non-vacuous was a second number.** The instrument also
recorded that the widened gate was genuinely REACHED by 17 real cross-file
workspace classes, and correctly declined on 17 of 17 using pooled `is_const`.
Reach without emits is a working gate. Zero reach with zero emits is a gate that
does not run, and the two are indistinguishable if you only count emits.

The same run reported 807 sites excluded by provenance (dominated by `string` at
582 raw and `Json::Value` at 334) — the population a naive gate reading
"not known const" as "non-const" would have fired on. That turned a standing
caution in a comment into a quantity.

**The rule: an instrument that counts only what changed cannot tell "nothing
should have changed" from "nothing was measured".** Record what the new code
REACHED, what it DECLINED, and why, not just what it EMITTED. Then a zero is a
result you can defend instead of one you have to apologise for.

Note the polarity difference from a normal sweep, because it inverts the
expectation: for a rule that TIGHTENS, a non-zero fleet delta is a reason to
stop and read. For a rule that WIDENS, a zero delta is the reason to stop and
check the instrument — and the check is exposure, not emits.

## A shell feature that does not exist reads exactly like a negative result

lane-unused reported the game as down and the RemoteBuild port as closed. Both
readings were wrong, and both instruments failed **toward absence**:

1. **`/dev/tcp/127.0.0.1/30000` is a bash feature and does not exist in zsh.**
   The redirect failed, so the `|| echo CLOSED` fired unconditionally. *A shell
   feature that is not there is indistinguishable from a closed port.*
2. **`ps aux | grep -i trackmania | head -5`** — the five lines shown were
   node/herdr/lsp processes and the real hit (`Z:\...\Trackmania.exe`) was below
   the cut.

`python3 socket.connect` said OPEN and `pgrep -af Trackmania` found the process.

**Two lessons, and the second is the less obvious one.**

The pipe-to-`head`/`tail` hazard is already in this catalogue for GATES. Here it
bit **discovery** instead — the same mechanism, a context nobody had thought to
apply it to, and the consequence was a lane standing down from work it could
have done. The rule is not "do not pipe gates"; it is **do not truncate any
output you are about to draw a conclusion from.**

And this shell is zsh. Bash-only constructs — `/dev/tcp`, `mapfile`, `$?` where
`$status` is meant, `${PIPESTATUS[0]}` — do not error usefully; they fail in
whatever direction the surrounding logic assumes. The same lane later had two
fleet sweeps silently produce `0 dirs / 0 results` (a stray path, then `mapfile`
not existing) and **both would have read as "no delta"**. It caught them only by
proving the instrument could report a difference first: a synthetic case that
must give 1, run before trusting any 0.

**Never accept a negative result from an instrument you have not seen produce a
positive one.**

## The pipe-eats-the-status trap applies to your EVIDENCE, not only to your gates

lane-enum caught itself checking "are my files fmt-clean?" with

    grep ... | sort -u; echo $?

which reads `sort`'s exit status, not `grep`'s. Same mechanism as the piped-gate
hazard already in this catalogue. It re-ran with `grep -c` and got a real answer.

**The reason this deserves its own entry is that it is QUIETER.** A piped gate at
least gets read as a gate; someone eventually asks whether that green was real. A
piped *verification step* produces a number you then cite as evidence in a
report, and nothing anywhere goes red. The lie propagates into the writeup, into
the merge decision, and into whatever the next lane builds on it.

This is now the fourth context in which the same mechanism has bitten this batch:
gates (`| tail` destroying the exit code), discovery (`grep | head` cutting off
the real process), a fleet sweep (zsh missing `mapfile`, yielding `0 results`),
and now evidence-gathering.

**The rule is not "do not pipe gates".** It is: **any command whose output or
status you are about to draw a conclusion from must not be piped, truncated, or
have its status read through another process.** Gate, probe, sanity check, or
one-off — the mechanism does not care which you called it.

## A remote gate rsyncs FILES, not the git index — so a peer's in-flight merge reads as a compile error

Two sessions share the main checkout of this repo. A peer began merging its lane
while this session ran `just rclippy-delta` on what it believed was its own
post-merge master. The gate rsynced the peer's **conflicted working tree** to
x-tuf and returned:

    ADDED [(uncoded)] src/typecheck/checker.rs (new): encountered diff marker
    ADDED [(uncoded)] src/typecheck/checker.rs (new): expected at least one digit in exponent
    clippy checked zero workspace units, so it found nothing because it looked at
    nothing. An empty result is not a clean result.

**The gate refused correctly, and for the right reason** — the zero-units guard
did its job. But nothing in that message says "another session is mid-merge in
your checkout". It reads as *your code does not compile*, which is the third time
this batch that an infrastructure condition has been reported as a code defect
(see also the ENOSPC entry).

**The mechanism, stated once so it is not re-derived:** the remote gate does not
and cannot see the git index. It copies the FILES. So an unresolved merge, a
half-written edit, or any dirty tree is measured as source, and conflict markers
arrive at the compiler as syntax.

**The fix is a pre-gate cleanliness probe.** A `MERGE_HEAD` check plus
`git status --porcelain` in the rsync wrapper turns a confusing compile failure
into a named refusal: *"the working tree has an unresolved merge; gating it would
measure conflict markers."* Cheap, and it converts the worst diagnostic in the
system into the clearest.

**The coordination lesson is separate and just as important.** The collision was
resolved without loss because neither side touched the other's index: this
session did not run `merge --abort`, `checkout` or `reset` on a merge it did not
start, and the peer announced a bounded, pre-announced hold-gates window for its
retry. **In a shared checkout, the index belongs to whoever started the
operation.** Abort is not a cleanup command when the mess is someone else's.

## A test that derives its expectations from the thing under test cannot see an omission

Two shapes from B132, both written by the lane that had authored the mutation
rule two tickets earlier, and both invisible by inspection:

**1. The whole test block was unreachable.** ~120 lines of assertions landed
below an early `if problems: return 1`, appending to a list nothing read. The
mutation harness scored **0/10 against a green baseline** — and that signature is
worth recognising on its own: *everything missed while the baseline passes* means
the assertions are never REACHED, as opposed to the usual one or two guards that
run but cannot discriminate.

**2. The sentinel loop iterated the constant under test.** The test walked
`GIT_OPERATION_SENTINELS` to decide what to check. Deleting `CHERRY_PICK_HEAD`
from that list did not fail the suite — **it just stopped testing it.** The suite
happily verified whatever remained.

**The general rule: a test that derives its expectations from the thing under
test can detect a MALFUNCTION but never an OMISSION.** It answers "does what is
listed work?" when the question was "is the list complete?". The fix is to assert
against an external authority — here, the filenames git itself writes.

This is the same family as the `read_log_window` deletion caught in B128: removing
the function left both mutation harnesses still scoring their revert-to-filtered
mutations as CAUGHT, but caught by an `AttributeError` at import rather than by
the guard. **A quiet downgrade into a crash-detector reports a full score while
one mutation has silently stopped testing anything.** A loud break would have
been fine.

**Watch for the score that stays perfect while the thing being scored changes.**

## A control proves nothing by not failing

B139's sweep found every probe's ROW-LEVEL oracle was sound — `build_verdict`
demands two positive markers, so a build that never happened yields AMBIGUOUS and
never ACCEPT. The exposure was one level up, in a run gate four probes had each
hand-rolled:

    bad = [r for r in rows if r["expect"] != "UNKNOWN" and r["verdict"] != r["expect"]]
    return 1 if bad else 0

**That exits 0 for a run that measured nothing, two different ways.**

1. `--only` can filter the CONTROLS out. `bad` is then computed over an empty
   set, and the run writes its corpus and exits 0 having verified no channel at
   all.
2. The transport can die midway. Every later row goes AMBIGUOUS, **no control
   mismatches — because controls that never ran cannot mismatch** — and a full
   column of AMBIGUOUS is exported as the measured answer with a success status.

**In that expression an ABSENT control and a PASSING control are the same
value.** That is B118's absence defect wearing a different hat, and it is the
general form: a check written as "no failures" cannot distinguish "everything
passed" from "nothing ran".

**The rule: assert on the presence of the control, not only on its verdict.** A
run gate must refuse when there are no rows, no controls, a mismatched control,
or an AMBIGUOUS control — the last because a control that could not be measured
has not been satisfied.

**And note what it must NOT refuse.** A single ambiguous row beside passing
controls is often honest — B096 claim 2 shipped two, correctly. Making AMBIGUOUS
fatal would destroy every legitimate partial result while passing any suite that
only checks bad runs get refused. **The over-condemn mutation is not optional
here; it is the one that distinguishes a gate from a wall.**

### Two corollaries from the same sweep

**A copy whose docstring claims to delegate is outside the coverage it appears to
have.** `probe_member_visibility` carried a duplicated `classify` documented as
delegating to the shared one. A future fix to the shared classifier would have
silently missed it, and nothing would have gone red.

**A failure and the false reading it causes can live in different processes hours
apart.** `probe_engine_const_receiver` had no post-run stranded-fixture sweep, so
it could exit 0 leaving a fixture installed and turn every SUBSEQUENT probe's
rows into AMBIGUOUS. No single run looked wrong. Cleanup is not hygiene; it is
part of the next run's correctness.

### Record the entry points that were FINE

B139 wrote down that `clippy_delta.py` already enforces its toolchain rather than
merely recording it, that `test-suite.py` has the zero-binary exit, green-zero
refusal and per-binary reconciliation, and that a grep for Rust tests that
self-skip on a missing fixture found zero. **A sweep that only records defects
guarantees the next sweep re-derives the clean cases.**

## The backup and the revert have to cover the same file set

lane-enum saved file copies of **two** files, then ran `git checkout HEAD --` on
**three**, silently discarding its `global_scope.rs` work. Recoverable only
because the text was still in its session transcript.

**`git checkout` reports success either way.** Nothing warns you that the set you
protected is smaller than the set you are about to overwrite, because from git's
point of view nothing went wrong.

This is the "never let a stash be the only copy" hazard in a new costume, and the
generalisation is worth stating on its own: **whenever a destructive command
takes a file list, the protective step must take THE SAME LIST** — ideally in the
same command, built from the same variable, so they cannot drift apart while you
are editing one of them.

The related family already in this catalogue: `git add -A` (stages more than you
edited), `git reset --soft master` on a moved ref (stages other people's
commits), and here `git checkout HEAD -- <list>` (reverts more than you backed
up). All three are one command whose scope the author has not re-checked, all
three apply cleanly, and none of them fails.

## A value that is right by coincidence is not knowledge

B130 excluded Nadeo engine receivers because the loader hardcoded
`is_const: false`, and argued that consuming it would fire on every correct const
Nadeo call. B137 then probed the engine and found **that population is empty** —
`const CMwNod@` cannot be read through at all, so the hardcoded `false` was
factually right on every shape tested.

**The exclusion still stands, and the reason it stands is the point.** Five
probed members do not license a claim about 3084, and the dump does not say. A
`bool` that happens to match reality on the cases anyone has looked at is
indistinguishable, in the type, from one that was measured — which is exactly the
defect B125 and B131 fixed on the Core side.

So the fix was not to flip the value; it was to make the ABSENCE REPRESENTABLE:
`Option<bool>`, `Some` for Core, `None` for Nadeo. **And the type change did work
no review could have done — two existing tests stopped compiling, forcing every
reader to confront the third state.** A `bool` had been preventing that
conversation.

It also removed a proxy: the consumer had been branching on `TypeSource` to
decide whether constness was trustworthy. Right answer, wrong reason, and quietly
wrong the moment a Core method arrives without constness or a Nadeo one with it.
**Read the datum's own "I do not know" rather than inferring it from where the
datum came from.**

## A detector that cannot see the counterexample gives a confident wrong answer

B129's inventory needed to know whether any in-source engine table is PRIMARY
ground truth or merely a restatement of a corpus — the fact the whole design
question turned on. A pipe-table detector found 9 tables, all restatements, and
the lane was one step from reporting **"0 of 9 are primary."** Clean, decisive,
and wrong.

It only matched **markdown pipe tables**. Re-run over fenced doc-comment blocks
it found `src/typecheck/interface_impl.rs:8` — five compile attempts with the
engine's exact error strings, six prose bullets of measured behaviour, and a
deliberate `NOT MEASURED` gap. Its own text says *"GROUND TRUTH is the measured
table at the top of this module."*

**The instrument could not represent the thing it was built to look for**, so its
zero was a statement about the detector, not the repo — and a zero is exactly the
result nobody interrogates, because it looks like a finished search.

**The rule: before believing a survey's zero, ask what shape the instrument can
see, and go find one instance BY HAND.** One confirmed positive, located
independently, converts a zero from "nothing exists" to "nothing else exists".
That is the same discipline as the exposure entry, applied to search rather than
measurement.

This is the fourth time in this batch that re-DERIVING caught what re-READING
would have missed: a `cut -d/ -f2` that concentrated 53 plugins into 6, two fleet
sweeps returning `0 dirs / 0 results` from a stray path and a missing `mapfile`, a
grep matching a local variable of the same name and producing a whole ticket on a
false premise, and now this. **Re-reading finds typos. Re-deriving finds wrong
questions.**

## Making a field `Option` is only half the work — the other half is writing `None`

B138 built an audit for loader-side hardcoded values: a `bool` initialised to a
literal ASSERTS something, an `Option` initialised to `None` merely says
"absent". The audit's first real run found B142 by itself, which is the outcome
you want from a structural fix.

**Then its own red check passed.** Reverting the fixed field to `Some(false)` was
accepted by the audit — because a literal wrapped in `Some` asserts exactly as
much as a bare `false` while LOOKING like the fix.

An audit that cannot tell `Some(false)` from `None` would have made every future
occurrence invisible: **the exact defect class it exists to catch, reintroduced
inside the catcher.** `Some(false)` and `Some(true)` are now findings.

It surfaced only because the author re-ran the red check after changing the
scanner and asked WHY it passed. That is the third time in this batch that *a
passing red check looks identical to a working fix* has been the thing that
mattered.

**The general rule: changing a type does not change a claim.** `Option<bool>`
gives you somewhere to put "I do not know"; it does not put it there. Any audit
of this kind must score the VALUE WRITTEN, not the type declared.

A second, smaller shape from the same scanner: it silently skipped
`String::new(), // comment` — a trailing comment defeated the value parse — and
that was found only because the expected table disagreed with the count. **A
scanner needs a table it must match, not just a list it produces.**

## A verification step whose INPUT silently did not update

lane-enum's first before/after fleet comparison reported **IDENTICAL** while both
sweeps ran **the same stale binary**. `--sync-back` had failed, did not exit
non-zero, and the sweep ran anyway on the old artefact.

The comparison was perfectly honest: two identical inputs give an identical
answer. **Nothing told it the inputs were supposed to differ.**

It was caught because the diagnostic count did not match a number seen an hour
earlier — luck, not process. The fix the lane applied by hand, and the one worth
building in: **hash the fetched artefact and assert the two differ before
comparing their output.**

This is the pipe-eats-the-status family one layer further out. That family says a
command's STATUS can be lost; this says a command's EFFECT can be lost, with the
downstream step reporting success on stale state. Both produce a green that
describes something other than what you asked about.

## A correct verdict with a wrong reason attached is still a false green

B129's mutation harness produced three defects in itself, each of which would
have shipped as a green guard:

**1. A vacuous guard hidden by a NEIGHBOURING guard.** The manifest column check
survived its mutation — but only because its fixture had no data rows, so the
mutation tripped the *empty-manifest* guard instead. The column guard never ran.
A mutation killed, the score was perfect, and the thing under test was never
exercised. **Fixed with a fixture valid in every respect except the one being
tested** — which is the general shape of a fixture that isolates a guard.

**2. A mutation red for the wrong reason.** Removing the empty check raised an
uncaught `IndexError`, so the selftest CRASHED rather than failing its named
check. Caught only because the harness verifies WHICH check failed, not merely
that something did. A harness that scores "did it go red?" cannot tell a guard
from a stack trace.

**3. An anchor that matched three sites.** The mutation anchor `    return found`
also prefixes `return found[-1][0]`, so applying it silently rewrote the version
reader as well. **And the harness's summary folded that SETUP-FAIL into "killed
for the wrong reason"** — a correct verdict with a wrong reason attached, which is
precisely the defect the ticket exists to prevent, occurring inside the tool
built to prevent it. It is now its own category, reported as "these proved
NOTHING".

**The rule: a mutation score is only meaningful if the harness knows WHY each
mutation died.** "Killed" without a reason is the same class of evidence as a
green gate that measured nothing — and this batch has now found that shape in
five separate tools, including three that were built specifically to detect it.

The lane also found two vacuity holes in its own selftest BEFORE writing any
mutants: `check_row_count` returning always-`None` and `classify_without_running`
returning always-`ERROR` would each have passed it. Positive and over-condemn
controls were added for both. **Auditing your own harness before you trust its
score is now the third time tonight that step has paid.**

## A lossy renderer does not just hide a bug — it gets written into the test as a reason not to look

B144: `Foo@ const` (a const HANDLE) was parsed out of a stored signature string
as `Named("Foo@ const")` — a type whose NAME contains its own qualifiers, and
which is therefore equal to nothing. Every funcdef signature comparison against
a const-handle parameter failed, and Max found it as red squiggles on a shipped
plugin the game compiles.

There WAS a test. It was named `parse_type_string_handle_trailing_const`, it
was the only test in the file named for that exact case, and here is what it
asserted:

```rust
// `Foo@ const` → const handle to a mutable object. The parser produces
// `Const(Handle(Foo))`; the canonical type-string form printed back by
// `display()` is `const Foo@`, so we assert the distinction at the parser
// level instead (see parser tests). Here we just verify the default handle
// path stays lossless.
assert_eq!(parse_type_string("Foo@"), Handle(Named("Foo")));
```

It states the correct answer in prose, then asserts a DIFFERENT input. So it
passed for the whole life of the defect, and it did so while carrying the
subject's own name.

**The mechanism is the interesting part, and it is not carelessness.** The
reason the author gave for not asserting is true and load-bearing:
`display()` rendered `Const(Handle(T))` and `Handle(Const(T))` — the const
handle and the handle-to-const, two genuinely different types — to the same
string. Faced with a renderer that cannot distinguish the two shapes, the
author concluded the distinction could not be asserted HERE and deferred it
elsewhere. The deferral was never picked up.

So a lossy rendering does more damage than hiding one comparison. It becomes
a **documented, plausible reason to write a weaker assertion**, and the
weaker assertion inherits the strong name. B136 already says a test comparing
types by rendered string cannot detect a structural type bug. This is the
sharper version:

> **When the renderer cannot express the distinction, do not weaken the test —
> fix the renderer, or assert on the constructed tree.** "It does not round-trip,
> so I asserted something else" is how a defect acquires a green test named
> after it.

The fix asserts trees (`Const(Handle(Named))` vs `Handle(Const(Named))` vs
`Const(Handle(Const(Named)))`), asserts the three are pairwise DISTINCT so one
shape cannot satisfy all three, repairs `display()` so the const handle prints
as `Foo@ const`, and pins `parse(t.display()) == t` for all three. The
round-trip property is what makes the renderer safe to assert through again.

**A second-order finding from the same ticket, worth its own line.** The
engine was probed before fixing, per the batch rule, and the probe did not
confirm the ticket's model — it inverted it. The ticket assumed the engine was
LENIENT about const handles and the checker too strict. The engine is exact on
every const position in both directions; what it actually does is synthesise
`array<T>::Sort`'s comparator as `const T@ const &in` whenever the element type
is a HANDLE, so the spelling the checker DEMANDED (`const T@ &in`) is the one
the engine rejects, and the spelling it rejected is the one the engine wants.
Both facts were extracted by a trick worth reusing: **make the compiler print
its own expected signature by deliberately mismatching the call**, rather than
inferring the rule from a table of accept/reject. Eleven element types, eleven
verbatim signatures, one run — and it retired a "this is UNMEASURED" note that
two source files had been restating since B102.

## Make the compiler print its own answer instead of guessing what it wants

B144 was filed on the model that the checker was too STRICT: it rejected
`const Json::Value@ const &in` where the engine accepted it, so the fix would be
to relax. The probe inverted that. **The engine is exact on every const position
in both directions — what makes the shipped code compile is that its spelling is
the one the engine WANTS.** The checker had been demanding
`const Json::Value@ &in`, which the engine REJECTS.

The technique that settled it is worth copying: **deliberately mismatch the call
so the compiler prints the signature it was looking for.**

    Where 'less' is 'bool Json::Value@[]::less(const Json::Value@const&in, const Json::Value@const&in)'

One run, eleven element types, and the engine enumerated its own expectation —
`const T&in` for value types, `const T@const&in` for handles. No inference, no
model, no reading of AngelScript documentation that may not match this build.

**Where an error message names the expected form, an intentional failure is a
better instrument than a designed experiment**, because it makes the subject
state its answer in its own words rather than answering the yes/no question you
thought to ask. The same run also retired a standing UNMEASURED note and found
that `T@ const &inout` is not declarable at all — neither of which was on the
question list.

**And note which way the correction ran.** Two independent things pointed at
"too strict": a user reporting a false positive, and shipped code that compiles.
Both were true, and the conclusion drawn from them was still backwards. A
symmetric relaxation — the obvious fix — would have been wrong in both
directions, and only asking the reverse question (`T@ &in` at a `T@ const &in`
funcdef: also REJECT) exposed that.
