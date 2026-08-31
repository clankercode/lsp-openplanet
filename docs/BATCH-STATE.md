# bl-fix-many batch state — 2026-08-31, updated 21:50 AEST

## Now

Master **42c6544**, **SUITE-VERIFIED 2653 passed; 0 failed; 12 ignored; 30/30
binaries**, gated on **x-tuf** (see the ENOSPC note below — do not gate on
x-alpha until B106 lands).

Closed since the 21:05 restart: **B091, B093, B099, B101**. Suite 2601 -> 2653.

Lanes running: enum (B100), tooling (B106 -> B097 -> B096), unused (B107
rebasing, then B108). Two tickets were FILED from lane findings tonight —
B107 and B108 — both found while doing something else and deliberately not
folded in.

## Gate discipline, hard-won today

Three distinct false-green mechanisms hit this gate in one day. None produced a
bad merge, and the reason is worth keeping: the gate's PASS signal is expensive
to forge — one `SUITE-VERIFIED` line, printed at one place, only on a fully
clean run, carrying totals that must reconcile against the previous run.

1. `-- --test <name>` goes to LIBTEST as a name substring, matches nothing, and
   still runs and reports every binary: `SUITE-VERIFIED: 0 passed` over 30/30
   coverage. Fixed in 586d18b.
2. ENOSPC on x-alpha — exit 0 over a failed build. **B106, unfixed.**
3. Piping the gate to `tail` destroys the exit code entirely.

**So: gate unpiped, redirect to a file, capture `$status`, and RECONCILE the
total.** `SUITE-VERIFIED` on its own is a word; 2653 + N is the check.

## Never `git pull` here

`origin` is **782 commits behind** — sessions share one local checkout rather
than syncing through GitHub. `git pull --rebase` starts replaying the entire
history onto a months-old base. To pick up another session's work it is already
in your checkout; in a worktree use `git rebase master`. Full account in
docs/verification-hazards.md.

## The method finding worth carrying past this batch

When a test's subject is an ABSENCE, red/green cannot validate it — the pre-fix
state is absence everywhere, so every "offers nothing" assertion passes for
free, and such a test is indistinguishable by inspection from one that holds.
This unifies B088's vacuous silence, B097's non-discriminating control and
B071's test written in a harness that ran no analysis.

Operational rule: **after a red run, mutation-test every test that PASSED.**
The red run hands you that list for free.

## Original entry — 20:35 AEST

Written to disk because all four lanes died on a spend limit and the batch
resumes from here, not from anyone's memory.

## Why work stopped

All four lane subagents (enum, resolver, tooling, unused) failed at **20:11
AEST** with `You've hit your individual spend limit ... your session limit
resets 11pm (Australia/Sydney)`. Not a crash and not a defect — nothing is
wrong with the tree. The coordinator session was unaffected and kept working.

**23:00 is inside quiet hours (21:00–08:00), so the sensible resume is the
morning, not 23:00.**

## Master

`ba0dc65`, **SUITE-VERIFIED: 2601 passed; 0 failed; 12 ignored; 30/30
binaries** on x-alpha. Working tree clean.

## Closed today

B071, B073, B076, B077, B080, B083, B084, B085, B086, B087, B088, B089, B090,
B092, B094, B095, B098, B102 — plus GH #18 from another session.
Suite 2320 → 2601, binaries 28 → 30.

## What each lane was doing when it died

| lane | ticket | state |
|---|---|---|
| enum | B102 | **RESCUED AND MERGED** — see below. B091 was next, not started. |
| resolver | B101 | "Rebasing onto master and reading B101." Nothing written. |
| unused | B093 | "Four emission sites, and they split into two confidence classes. Let me check the coordinator's derivability question against GH #18's delta machinery." Nothing committed. |
| tooling | B097 | No output. Nothing committed. |

Only lane-enum had uncommitted work. It was committed verbatim, rebased,
gated at 2601 and merged as B102 (`ba0dc65`).

## ONE THING THAT IS LOST UNLESS SOMEONE LOOKS

lane-enum's final words were: *"Confirmed and minimal. That is a separate
defect — I'll report it, not fold it in. Now the tests."*

**That defect is recorded nowhere.** It is not in the B102 diff, not in a
ticket, and lane transcripts are not stored as separate files (`teams/` holds
only 4K of metadata), and **the coordinator's transcript was checked and
holds nothing**: `"isSidechain":true` appears zero times in it, so a
teammate's internal reasoning was never captured there — only the one-line
idle notification quoted above. Do not spend time re-searching; it is gone.

What is known about it: it was found while threading `elem` through the
external-call arg-type checks for B102, the lane called it "confirmed and
minimal", and it judged the defect separate enough not to fold into B102. That
area is `check_external_call_arg_types` / `check_external_overload_set` /
`walk_args_and_check_external_param_types` in `src/typecheck/checker.rs`.
Re-deriving it from scratch is likely cheaper than hunting for the record.

## Open tickets (11)

B081 (shadow-blind `is_external_derived_from`; `in_progress` under `cli-user`,
orphaned — `.worktrees/b081` predates every current session, needs Max's call),
B091, B093, B096, B097, B099, B100, B101, B103, B104, B105.

## Sequencing decisions to preserve

- **B091 behind B102** — same question (does this handle match this funcdef
  slot), same files. B102 is now merged, so B091 is unblocked and inherits its
  fixture shape.
- **B096 behind B097** — B096 needs game probes; B097 is what restores them.
- **B100 held back** — lands in the same `checker.rs` funcdef-slot region B102
  touched. Now that B102 has merged it can be scheduled.
- **B101 is what lets B094 close its deliberate hole**, not just a completeness
  fix.

## Probing is BLOCKED repo-wide, and that is now measured

`load_plugin` has never worked (64-byte `ReadRaw` short-read, B097). The
fallback theory — that Openplanet's file watcher compiles a staged folder —
was **refuted by measurement** with a positive control: a staged folder left
8 seconds produced nothing; the same folder through `tm-control-mcp` compiled
immediately. `UI::VirtualKey`, recorded ACCEPTED in the shipped b052 corpus,
returns UNDETERMINED today.

The shipped b052/b053 corpora remain a valid record of what the game said on
2026-08-30. **How they were produced is an open question.**

B068's sentinel is why this surfaced as UNDETERMINED rather than a false
ACCEPT. Treat any ticket asking for a fresh probe verdict as blocked on B097.

## Gate hazard fixed today — do not reintroduce

`just rtest -- --test <name>` sends `--test` to *libtest*, which reads it as a
name substring, matches nothing, and still runs and reports every binary. That
printed `SUITE-VERIFIED: 0 passed` over 30/30 coverage — a green zero. Fixed in
`586d18b`; detection now comes from libtest's own `filtered out` count. To
select a binary use `just rrun cargo test --test <name>`.

`remote-build.py` flags go **before** the action word, or they are shipped to
the remote shell.
