# bl-fix-many batch state — 2026-08-31, updated 21:50 AEST

## Now

**Read this line before you read any number below.** These figures go stale
within the hour and have twice nearly turned a real delta into an apparent null
result — once for lane-enum on B081, once for lane-resolver on B096. **Treat
every total here as a hint and MEASURE YOUR OWN BASE by gating before you write
code.** The gate is cheap; a false null is not.

Last written 2026-08-31 23:25 AEST, at master `1614fc2`.

- **Suite:** `SUITE-VERIFIED: 2807 passed; 0 failed; 12 ignored; 33/33 binaries`
  on x-alpha, from the main checkout.
- **Clippy:** `CLIPPY-DELTA-VERIFIED` on x-tuf. The baseline was **re-recorded
  at `6ea004ea`** and is now tight: 130 raw / 39 fingerprints, with **no FIXED
  entries left to absorb a regression.** Any new lint will show.

### Where each gate runs, and why it is not interchangeable

- **Suite -> x-alpha.** 48 cores, 64s against 206s local. Correctness only; its
  rustc is 1.97.1.
- **Clippy delta -> x-tuf.** Its rustc 1.97.0 MATCHES local. x-alpha's 1.97.1
  emits a different lint set, and numbers taken there are not comparable to this
  baseline. The gate refuses to compare them rather than reporting a difference
  that is not one.

### B097 is CLOSED — game probing is unblocked

Max fixed `Main.as`'s read loop himself; it drains on `Available()` instead of
handing each `ReadRaw` return to `Json::Parse`. Verified against the running
game, not read off the diff: `load_plugin` at 90 bytes now reaches its real
handler. It had NEVER worked before tonight — its compact minimum is 72 bytes
with an empty id, against a 64-byte ceiling.

**The fix is UNCOMMITTED in Max's tree** (`~/src/tm-remote-build`, branch
`fix/readraw-64-byte-short-read`). Nobody from this repo commits it. The "do not
touch that path" rule stands, and it stands for the fix too.

This closes ONE of three contamination channels. **B068 (one name per
compilation unit) and B077 (registration-before-copy sweep) still apply
unchanged.** Do not read a working transport as a clean room.

### Open, and who has them

| ticket | lane | note |
|---|---|---|
| B096 claim 2 | lane-resolver | shared interface/enum/funcdef; needs a compile-only oracle |
| B115 | lane-unused | field visibility dropped in the PARSER — one layer below B114 |
| B116 | lane-enum | const receiver, gated on `file_classes` not the const set |
| B113 | peer session | hover accessors |
| B114 | peer session | assigned by Max; WIP handed over on `lane/resolver-b114-wip` |

Nothing is blocked on Max.
