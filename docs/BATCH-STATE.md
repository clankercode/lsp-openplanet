# bl-fix-many batch state — 2026-08-31, updated 21:50 AEST

## Now

**Read this line before you read any number below.** These figures go stale
within the hour and have nearly turned a real delta into an apparent null result
three times. **Treat every total here as a hint and MEASURE YOUR OWN BASE by
gating before you write code.** Three separate lanes have caught a moved master
by measuring instead of quoting; nobody has been bitten by measuring.

Last written 2026-09-01 03:15 AEST.

- **Suite:** `SUITE-VERIFIED: 2958 passed; 0 failed; 12 ignored; 34/34 binaries`
  on x-alpha, from the main checkout.
- **Clippy:** `CLIPPY-DELTA-VERIFIED` on x-tuf, baseline `6ea004ea`,
  130 raw / 39 fingerprints. **Tight — no FIXED entries remain to absorb a
  regression.** It has caught a real new lint in four separate lanes tonight.
- **`cargo fmt` is CLEAN repo-wide** (B122) and the restriction on running it is
  lifted. `.git-blame-ignore-revs` carries the reformat SHA.

### Where each gate runs, and why it is not interchangeable

- **Suite -> x-alpha.** 48 cores. Correctness only; its rustc is 1.97.1.
- **Clippy delta -> x-tuf.** Its rustc 1.97.0 MATCHES local. x-alpha's 1.97.1
  emits a different lint set; the gate refuses to compare them rather than
  reporting a difference that is not one.

### Gates now refuse an unresolved git operation — B132

`MERGE_HEAD`, `REBASE_HEAD`, `rebase-merge/`, `rebase-apply/`,
`CHERRY_PICK_HEAD`, `REVERT_HEAD` -> **refused by name, with the unmerged files
listed.** A merely DIRTY tree still proceeds; the banner reports `dirty=N`.
Operation state is read **per-worktree** — a merge in the main checkout must not
stop five lanes.

If a gate refuses with a git-operation message, that is the guard working.

### Probe harness contract — B118, B123, B127

`build_status` is four-valued: **CLEAN / REJECTED / BUILD-ERR / NOT-RUN**. The
original defect was that *"the engine refused this construct"* and *"this build
did not happen"* were the same value, so the caller had nowhere to put the
distinction. `compiled_clean` survives as `build_status == "CLEAN"`.

**Load success is NOT build success.** RemoteBuild replies
`{"error":"","data":"Plugin loaded"}` even when the engine cannot resolve
dependencies. Route build decisions through `build_status`, never through the
load reply.

### Numbers that turned out to be wrong, so nobody re-cites them

- **B116's "807 engine receivers would fire" was an ARTEFACT.** With the real
  qualifier parsed (B124/B125/B130): **807 const_ok, 0 would-fire, 0 unknown.**
  It measured a gate reading "not known const" as "non-const", not a population.
- **`lookup_method_return`'s "no const-qualifier metadata yet" was STALE** — 301
  of 1002 Core decls end in ` const`.
- **B115's "completion is half-blind" was wrong** — it was totally blind; nothing
  above the parser read visibility, for fields OR methods.

### Open

| ticket | lane | note |
|---|---|---|
| B129 | lane-unused | one-command regen + drift report; inventory measurement first |
| B137 | lane-enum | Nadeo loader hardcodes `is_const: false`, so false means UNKNOWN |
| B139 | lane-resolver | sweep every gate for a missing pre-flight |
| B140 | lane-resolver | `--sync-back` resolves under CARGO_TARGET_DIR |
| B126 | unclaimed | double-emit on `c.Get().x = 1` |
| B133 | unclaimed | enum value doc strings dropped (206/929) |
| B134 | unclaimed | GH #18 lead: migration hints are prose, not metadata |
| B138 | unclaimed | audit loader-side hardcoded values, not just dump field presence |

Nothing is blocked on Max. The peer session has closed out.
