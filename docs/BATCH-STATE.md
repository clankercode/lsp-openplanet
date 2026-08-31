# bl-fix-many batch state — 2026-08-31, updated 21:50 AEST

## Now

**Read this line before you read any number below.** These figures go stale
within the hour and have twice nearly turned a real delta into an apparent null
result. **Treat every total here as a hint and MEASURE YOUR OWN BASE by gating
before you write code.** The gate costs 30-60s on x-alpha; a false null costs a
night.

Last written 2026-09-01 00:20 AEST.

- **Suite:** `SUITE-VERIFIED: 2839 passed; 0 failed; 12 ignored; 34/34 binaries`
  on x-alpha, from the main checkout.
- **Clippy:** `CLIPPY-DELTA-VERIFIED` on x-tuf, baseline `6ea004ea`,
  130 raw / 39 fingerprints. **Tight — no FIXED entries remain to absorb a
  regression.** Two lanes have already had it catch a real lint tonight.

### Where each gate runs, and why it is not interchangeable

- **Suite -> x-alpha.** 48 cores. Correctness only; its rustc is 1.97.1.
- **Clippy delta -> x-tuf.** Its rustc 1.97.0 MATCHES local. x-alpha's 1.97.1
  emits a different lint set and its numbers are not comparable to this
  baseline. The gate refuses to compare them rather than reporting a difference
  that is not one.

### Closed this stretch

B081, B096 (both claims), B097, B115, B116, B118, B120.

### The probe harness contract CHANGED — B118

`build_status` is now four-valued: **CLEAN / REJECTED / BUILD-ERR / NOT-RUN**.
It used to be a boolean, and the defect was not three parsing bugs: **"the
engine refused this construct" and "this build did not happen" were the same
value**, so the caller had nowhere to put the distinction. The log is now sliced
by each build's own start/end markers rather than by severity or time, which
makes attribution structural — a concurrent session's plugin cannot appear
between another build's two markers. `compiled_clean` survives as
`build_status == "CLEAN"`.

**If you collected a probe verdict before this landed, re-read it.** A NOT-RUN
previously read as ACCEPT, which is the worst polarity on any question whose
subject is acceptance.

### Game probing works

B097 is closed. `load_plugin` reaches its handler at 90 bytes; it had never
worked before 2026-08-31. **The fix is UNCOMMITTED in Max's tree**
(`~/src/tm-remote-build`, branch `fix/readraw-64-byte-short-read`) — nobody from
this repo commits it.

B068 (one name per compilation unit) and B077 (registration-before-copy sweep)
**still apply unchanged.** B118 closed a third channel, not all of them.

### Open, and who has them

| ticket | lane | note |
|---|---|---|
| B119 | lane-unused | inherited non-const method; needs a game probe to choose between two readings |
| B121 | lane-enum | AC19 propagates through fresh return values; 10 measured FPs across 7 plugins |
| B123 | lane-resolver | last consumer of the filtered log reader |
| B113 | peer session | hover accessors |
| B114 | peer session | pooling; rework required pre-merge, see below |
| B117 | BLOCKED on B114 | member visibility has no consumer at any layer |
| B122 | HELD | `cargo fmt --check` red repo-wide; a reformat is a merge-conflict generator, so it waits until no lane is in flight |

### B114 had a tripwire that could not fire

Its field arm hardcoded `DeclModifiers::default()` while the test watching for
the gap read that same hardcoded value. It would have merged green reporting
"still broken", leaving a doc comment blaming a parser layer B115 had already
fixed. Caught pre-merge; the peer session has redirected its lane to thread both
field AND property visibility through pooling and invert the pin.

Nothing is blocked on Max.
