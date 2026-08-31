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
