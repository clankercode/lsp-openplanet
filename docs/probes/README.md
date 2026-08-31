# Game probe corpora

Verbatim verdicts from the **live game compiler** on names this crate has to
decide about. Committed because re-probing costs a running Trackmania +
Openplanet session with RemoteBuild on port 30000, and because several open
tickets cite these files as their starting corpus.

Produced by the B052/B053 lane, 2026-08-30, against **Trackmania + Openplanet
1.29.5 (next, Public, 1234ad9e)**, type DB `tests/fixtures/typedb` (Nadeo dump
dated 2026-02-03), via `scripts/probe_enum_corpus.py`.

## The manifest is `MANIFEST.tsv`, not this table (B129)

`docs/probes/MANIFEST.tsv` is the authoritative, machine-readable list of every
engine-derived artifact in this repo, and `just regen-golds` reads it. This
README stays as the prose companion — the reasoning, the caveats, and the
asymmetries a row cannot carry — but it is no longer the list.

That split exists because this table WAS the de-facto manifest and had quietly
stopped covering the directory: `b067-class-position-scopes.tsv` (63 rows),
`b071-enum-conversion.tsv` (8 rows) and `b096-namespaced-export-binding.tsv`
(6 rows) were all committed corpora absent from it. A hand-maintained list
does not fail when it falls behind; it just covers less while continuing to
look complete. `tests/b129_gold_manifest.rs` now parses this DIRECTORY and
fails if a `.tsv` here is missing from the manifest, so the same drift cannot
recur silently.

## Retention: which corpora are kept per version (B151)

Every manifest row carries a `volatility` saying **how to read its drift**, and
that decides whether old versions are kept live:

| volatility | retention | the reading |
|---|---|---|
| `SURFACE` | kept per version under `versions/` | the artifact says what the game EXPOSES. A member appearing or disappearing between versions IS the finding, and you can only see it holding both. |
| `SEMANTICS` | one current copy | the artifact says how the COMPILER BEHAVES. Nothing diffs an old language rule, git holds the history, and a live stale copy invites someone citing it. |
| `HYBRID` | one current copy | a language rule measured over a game-surface enumeration. Its row set moves for SURFACE reasons and its verdicts for SEMANTICS ones, so the two axes are read differently. |

`b067-class-position-scopes.tsv` is the only `HYBRID` today, and it is why one
label per file would have lied: it asks a language question — can a class name
qualify a type? — about **41 specific game class names**. A game update that
removes `CGxLightBall` changes a row for a reason that has nothing to do with
the language.

**Volatility is per-artifact, not per-column, because the columns are already
separated where it matters.** `scripts/diff_probe_corpus.py` splits a diff into
`BEHAVIOUR-CHANGED` (verdict), `WORDING-CHANGED` (message), `ROW-ADDED`/
`ROW-REMOVED` (key set) and `INSTRUMENT-BROKEN` (unclassifiable) at comparison
time. The label does not classify anything; it says which of those categories
is EXPECTED for this artifact and which is a finding. Pass it with
`--volatility` and the report says so in words.

`just regen-golds` CHECKS the policy rather than trusting it: a `SURFACE`
artifact that nothing archives is reported every run. It found three on its
first run, which is why `versions/` exists.

Two findings the manifest records rather than hides:

* **`b067` and `b071` have no generator anywhere in this repo.** Both are
  fully stamped engine gold with their method written out in their own
  preambles, and reproducing either means a human rebuilding the probe by
  hand. `just regen-golds` reports them as `UNCHECKED (no-generator)` on every
  run, because an artifact that cannot be regenerated is a finding.
* **Five corpora carry no engine version inside the file**, and two corpora
  probed on the SAME DAY record different versions (1.29.5 and 1.29.14). The
  manifest carries the version per artifact and the runner refuses to diff an
  `UNKNOWN` or moved one, rather than reporting a version change as drift.

| file | rows | what it answers |
|---|---|---|
| `b052-qualified-enum-spellings.tsv` | 1901 | for a qualified *type* spelling (`Scene::ESceneCharVisNotice`, `UI::VirtualKey`, `CGameMenu::EMenuOrder`), does the game accept it, and if not, what does it say? |
| `b053-bare-enum-values.tsv` | 2989 | for a bare *value* name of a named enum (`Turbo`, `GlassSmash`), does the game accept it, reject it as unknown, or reject it as ambiguous? |
| `b086-export-stub-binding.tsv` | 18 | given a `[script] exports` stub and a body, does the engine BIND them? Produced 2026-08-31 against **Openplanet 1.29.14** by `scripts/probe_export_binding.py`, driving the game through `tm-control-mcp`. 18 variants, 0 ambiguous. |
| `b119-const-receiver.tsv` | 23 | for a CONST receiver, does the engine reject a non-const method that is INHERITED, and does `const T@[]@` make its ELEMENTS const? Produced 2026-09-01 by `scripts/probe_const_receiver.py`. 23 variants, 6 controls, 0 ambiguous. Answers BOTH: the engine walks parents (depths 1/2/4, every receiver spelling), and a const handle-array's elements are mutable pointees behind read-only slots, while a const VALUE array's elements are const. Re-run unchanged after B118 tightened `build_status`. |
| `b117-member-visibility.tsv` | 39 | at which ACCESS SITES does the engine allow a `private` / `protected` member? Produced 2026-09-01 by `scripts/probe_member_visibility.py`. 39 variants, 15 controls, 0 ambiguous. Own class: all. Derived class: `protected` yes, `private` no. Unrelated: public only. The FILE BOUNDARY is irrelevant (a class is module-scoped), privacy is CLASS-scoped rather than object-scoped, and `this.<inherited private field>` is an engine hole accepted in that one spelling while the bare name and a `Base@` handle are both rejected. |
| `b096-shared-modifiers.tsv` | 24 | do `shared interface` / `shared enum` / `shared funcdef` / `shared` function compile, at top level and namespaced, in a `shared_exports` file and in a plain one — and is `shared` semantically live? Produced 2026-08-31 against **Openplanet 1.29.14** by `scripts/probe_shared_modifiers.py` over RemoteBuild. 16 variants, 5 controls, 2 ambiguous. |

The B086 corpus answers a question the other two do not, and needs a different
instrument for it: binding is a RUNTIME property, so the probe has to CALL the
import. An unmatched stub compiles fine on both sides and only dies, with
`Unbound function called`, when reached. What it found:

- an EXACT signature match binds in every shape tried — by value, `const &in`,
  `&out`, `int &out`, `T@`, `T@ &in`, and a defaulted parameter;
- ANY parameter-type difference does not bind; **`const` alone is enough**
  (a `string &in` stub over a `const string &in` body fails);
- the RETURN type is part of the signature — identical parameters with `string`
  vs `void` does not bind;
- a DEFAULT ARGUMENT is **not** — present on only one side it still binds, in
  both directions;
- the resolved type matters, not its spelling: a `uint` stub over a `uint32`
  body binds.

Its offline `--selftest` needs no game and is worth reading as the shape a
probe's oracle should have: every way a run can fail to measure is a distinct
`AMBIGUOUS`, never a verdict, and the variant table is itself checked for the
ability to discriminate — the two controls must expect OPPOSITE verdicts, and
the `exact`/`lax` pair must differ in the stub's parameter type alone. B086
exists because an earlier probe's two variants failed identically.
| `b052-scope-provenance.tsv` | 233 | for each SCOPE an ACCEPTED b052 row depends on: did it exist on the probing machine, and did the ENGINE supply it or that machine's plugins? |

`scripts/probe_export_binding.py` (B086) answers a different question — given a
`[script] exports` stub and a body, does the engine BIND them — and writes no
corpus yet, because it is blocked (see below). Its offline
`--selftest` runs without a game and is worth reading as the shape a probe's
oracle should have: every way a run can fail to measure is a distinct
`AMBIGUOUS`, never a verdict.

Both corpora are TSV with a `#`-comment header. The `game_message` column is
the compiler's own text, unedited.

The third file is a **sidecar, not a measurement**: it is how a reader tells an
environment-dependent row from a stable one without re-probing. Documented in
full under "Which rows are the probing machine's?" below.

## Who depends on these

- **B056** (landed) — the qualified and bare spellings the LSP used to accept
  and the game rejects. Filter `b052-qualified-enum-spellings.tsv` for
  `REJECTED`. Re-measured after B052 landed: **1012**, in three families —
  the synthetic Nadeo module prefix (`Game::CGameCtnApp`), a Core namespace
  with a segment dropped (`Plot::Marker` for `UI::Plot::Marker`), and a
  non-global enum written bare (`EMenuOrder`, `ButtonFlags`).

  **`b052-qualified-enum-spellings.tsv` is now read by the test suite**, like
  its B053 sibling. `resolver::tests::every_probed_type_spelling_matches_the_games_verdict`
  parses every row and drives the real `TypeResolver` over it, so editing a
  verdict or a spelling changes what the suite demands. Re-probe before you
  edit, and say so in the commit.

  Two groups of rows are deliberately NOT expected to resolve here, and both
  are named in that test rather than silently skipped: the 13 spellings whose
  leading scope this crate does not have, and the 4 `string::` rows, which are
  a lexer gap rather than a resolution one.

  Those 13 are no longer hand-listed. They are DERIVED from
  `b052-scope-provenance.tsv` (B080), which splits them by reason: **5** whose
  scope a NAMED installed plugin declares (`MathX`, `MLFeed`, `MLHook`), **4**
  whose scope was not the engine's and whose supplier is not identified (`Foo`,
  `Foo::Bar`, `VehicleState`), and **4** whose scope the engine really has while
  neither shipped database records it (all four `GameData::*`). The
  hand-maintained
  lists had that split wrong — they called three of the four `GameData` rows
  plugin artefacts while the same file's prose called `GameData` an engine
  scope — which is the drift that motivated the sidecar.
- **B057** (landed) — the 2373 bare value names the LSP used to accept and the
  game answers `No matching symbol` for. Filter `b053-bare-enum-values.tsv`
  for `verdict == NO_SYMBOL`.

  **`b053-bare-enum-values.tsv` is now read by the test suite, not only by
  people.** `global_scope::tests::every_probed_bare_value_matches_the_games_verdict`
  parses every row and asserts the crate's answer matches the game's, so the
  file is load-bearing for `cargo test`: editing a verdict, a value name or the
  row count changes what the suite demands. Re-probe before you edit it, and
  say so in the commit.

## Which rows are the probing machine's? (B080)

A corpus records what the game said. It does not, by itself, record which of
those verdicts were only true on the machine that ran it — and the damage from
that is not the flip, it is a later reader citing such a row as evidence of
something it never showed. Three `Foo::*` rows of b052 were cited as
workspace-declared witnesses; they were a foreign plugin's namespace.

### Half the answer is already in the corpus, in the MESSAGE FORM

The `<prefix>::ZZZNoSuch` context pins carry it, and they carry it in their
wording rather than their verdict — **both forms are `REJECTED`**, so a reader
keying off the verdict column learns nothing:

    Identifier 'ZZZNoSuch' is not a data type in namespace 'P' or parent
        -> the compiler resolved the scope `P` and failed INSIDE it, which it
           can only do when `P` exists.
    Namespace 'P' doesn't exist.
        -> it does not.

Read the prefix from the SPELLING, never from the message. The message is not
consistent about it: `Meta::Plugin::ZZZNoSuch` answers "in namespace `'Plugin'`"
while `UI::Font::ZZZNoSuch` answers "in namespace `'UI::Font'`".

### The other half cannot be derived, so it is recorded

Whether an existing scope came from the ENGINE or from an installed plugin is
not in the corpus and no amount of re-reading will put it there. `Foo::VirtualKey`
and `UI::VirtualKey` are the same shape to the probe: namespace-qualified names
that resolved. That half lives in `b052-scope-provenance.tsv`, one row per
scope prefix:

| column | |
|---|---|
| `prefix` | a leading scope some ACCEPTED row depends on |
| `pin` | `SCOPE` / `ABSENT` / `UNPINNED` — **recomputed** from the corpus's own pin rows by `scripts/probe_enum_corpus.py --scope-provenance`; do not hand-edit |
| `origin` | `ENGINE` / `HOST_PLUGIN` / `HOST_FIXTURE` / `HOST_UNATTRIBUTED` / `ENGINE_UNMODELLED` / `UNKNOWN` — recorded, and carried forward across regenerations |
| `evidence` | what supports `origin`, opening `MEASURED:` (an instrument answered) or `INFERRED:` (someone reasoned). No row currently needs `INFERRED:`, and a test holds that |

A consumer reads it as:

- `ENGINE` → environment-independent.
- `ENGINE_UNMODELLED` → environment-INdependent; the engine has the scope but
  neither shipped database records it, so this crate cannot resolve the row
  either. Kept separate from the host cases because conflating them is how a
  list of twelve spellings came to carry three that the same file calls engine
  facts.
- `HOST_*` → **environment-dependent.** A valid measurement of that session,
  saying nothing about the engine.
- `UNKNOWN` → nobody has classified it. The generator exits non-zero.

### Three host channels, and why they are not one value

A probe compile can reach a scope the engine does not have by three routes,
and they do not behave the same on RE-MEASUREMENT:

| origin | supplier | re-measurable? |
|---|---|---|
| `HOST_PLUGIN` | an installed plugin, NAMED in the evidence | yes — install it and the row returns |
| `HOST_FIXTURE` | the probe run's own fixture | yes — stage it and the row returns |
| `HOST_UNATTRIBUTED` | not the engine, supplier not identified | unknown |

`HOST_FIXTURE` is **provably empty** for these corpora: `build_source` emits no
`namespace` in either template, checked by `probe_enum_corpus.py --selftest`
rather than asserted in prose. That negative is what lets `Foo` be attributed
away from the fixture — and what leaves the remaining two routes
indistinguishable, which is why `Foo` is `HOST_UNATTRIBUTED` rather than
`HOST_PLUGIN`.

The third route is the one worth naming out loud: **another client sharing the
RemoteBuild port**, whose staged fixture is an installed plugin declaring the
names your run is asking about. A verdict cannot distinguish it, and unlike the
other two it cannot be reproduced. Spelling all three `HOST` would say a row is
environment-dependent while hiding whether anyone could ever check it again.

### These verdicts require EXCLUSIVE port access

Not a scheduling nicety. The RemoteBuild plugin's read loop has been observed
splitting one request across two socket reads and parsing each half as its own
payload while a second client was active. Two failure modes, and the second is
much worse:

- a contaminated **fixture** leaves the transaction intact and only makes an
  ACCEPT untrustworthy — which is what the whole "a REJECT is evidence, an
  ACCEPT may not be" rule above is for;
- a mangled **payload** can have the game answer a question nobody asked, and a
  truncated script is still a script, so it can compile. The result is a clean,
  plausible, WRONG verdict with nothing in the output marking it as damaged.

B068's sentinel proves analysis reached the end of a unit; B077's guard proves
no fixture was stranded. Neither watches this door. **The corpus cannot express
whether a row was measured under exclusive access**, so that claim belongs in
the commit that writes the row, and a re-measured row is only as good as the
operator's word that nothing else held the port.

### `UNPINNED` is a measured gap, not a tidying job

**Only 38 of the 233 scopes b052's ACCEPTED rows depend on carry a pin**, and
two of the scopes the sidecar calls `HOST` — `MLHook` and `VehicleState` — have
no pin row at all. So provenance derived from the pins ALONE would read both as
engine facts. That is why `UNPINNED` is a value rather than an absence: a format
that spells the unmeasured case the same as the measured-safe one is how the
hazard gets in.

Closing the gap means probing `<prefix>::ZZZNoSuch` for the unpinned scopes.
`every_scope_an_accepted_row_depends_on_is_classified_with_evidence` asserts the
current gap size, so closing it shows up as a test change rather than as nothing.

### What guards the labels

Not review. Four tests, each mutation-checked:

| a wrong label | what goes red |
|---|---|
| a `HOST_*` scope labelled `ENGINE` | `every_probed_type_spelling_matches_the_games_verdict` — the row stays asserted, and the crate rejects it |
| an `ENGINE` scope labelled `HOST_*` | the same sweep (the row is excluded but the crate resolves it) **and** `no_scope_the_engine_declares_is_labelled_host` |
| a hand-edited `pin` cell | `the_recorded_pins_match_the_corpuss_own_pin_rows`, which recomputes them from the corpus |
| a new or unevidenced scope | `every_scope_an_accepted_row_depends_on_is_classified_with_evidence` |

`the_derived_exclusion_set_reproduces_the_hand_maintained_one` pins the whole
switch: the set derived from the sidecar is byte-identical to the two
hand-maintained Rust constants it replaced, which are now frozen and read by
nothing else.

### b053 already had this, in a column nobody had named

`b053-bare-enum-values.tsv` records `global_enum_owners` beside the game's
verdict — the shipped DB's own reason, written at probe time by a different
instrument. An ACCEPTED row with an EMPTY owners cell would be a bare value the
game took and the model cannot explain: the value-position twin of the `Foo::`
rows, since a plugin declaring a *global* enum with that value makes it compile.
**There are none**, which is why b053 re-probed 592/592 unchanged. Asserted by
`every_accepted_bare_value_is_explained_by_the_shipped_type_db`, so a corpus
regenerated on a machine with such a plugin fails and names the values.

b052 needs a sidecar for the same question only because its rows turn on a
SCOPE, and a scope's origin appears nowhere in that file.

### Regenerating the sidecar

    scripts/probe_enum_corpus.py --scope-provenance b052

Offline — no game. It recomputes `pin` only. `origin` and `evidence` are carried
forward by prefix, so a re-probe on another machine cannot silently relabel a
scope, and a prefix the sidecar has never seen is written `UNKNOWN` and the run
exits non-zero. Defaulting an unseen prefix to `ENGINE` would be exactly the
failure the file exists to prevent.


## RemoteBuild cannot receive a `load_plugin` request. Drive `tm-control-mcp` instead.

Everything on this page needs `load_plugin`, and **`load_plugin` currently
cannot be delivered at all**. Measured against Openplanet 1.29.14 with
`RemoteBuild.op` 1.1.0 — and note it does **not** date from that update:
`Openplanet-Old.log` caps at 64 too, so **`load_plugin` has never worked** and
anything ever seen loaded was loaded at game start, not by the call:

- RemoteBuild's `HandleClient` calls `Json::Parse` on **each raw socket read**,
  and `Net::Socket::ReadRaw` returns at most **64** bytes per call. Every longer
  request is split into fragments and each fragment fails to parse.
- The threshold is exact and deterministic: **<=64 passes, >=65 splits**.
  `Available()` reports the true size; `ReadRaw(n)` returns at most 64.

  An earlier 66/67 figure here was wrong, and **not** because of framing bytes —
  requests are unframed. It came from padding a `get_status` until it stopped
  parsing, which measures whether *that payload* survives being cut rather than
  where the cut falls. Padded to 65 and 66, the first 64 bytes happened to end
  in `"}` and `"`, still leniently parseable with `route` intact, so both
  answered `Alive`; at 67 the cut landed mid-string. The cut was at 64
  throughout. **Read the cap off the game's own `Read payload:` trace**, which
  reports the read length directly —
  `scripts/probe_export_binding.py:measure_read_cap()` does. Both readings were
  reproducible over repeated trials and only one was accurate.
- A minimal `load_plugin` request is **72 bytes with an empty id**, so no
  shorter plugin id fixes it.
- The reply is `{"error":"Not implemented: ","data":""}` — the router's message
  for an **unknown route**. That is the expensive part: it points the reader at
  route spellings rather than at size.
- `get_status` is 35 bytes and works, so `rb_alive()` returns True. Every probe
  passes its liveness gate and then fails on the route that matters.

**Corollary, and it invalidates a tempting finding: do not conclude that a route
is unimplemented from a >64-byte probe.** Every route name answers
`Not implemented` at that size, a deliberately bogus one included — so a
bogus-route control fails through the *same mechanism* as the positives and
discriminates nothing. Nine routes were probed this way in one afternoon and all
nine "failed"; none of that was evidence. A control that shares a failure mode
with the test is not a control, it is a second test.

Two consequences worth knowing before you spend an afternoon on it:

1. **Both `scripts/issue_repro_game.py` AND `scripts/probe_enum_corpus.py` are
   blocked.** `issue_repro_game.py`'s `--selftest` is offline-only, so it stays
   green regardless.

   `probe_enum_corpus.py` was believed NOT blocked, on the reasoning that it
   ignores `load_plugin`'s result, sleeps, and reads the game log — so
   Openplanet's own file watcher would compile the staged folder. **Measured
   2026-08-31, and that is false.** A fresh folder staged into `Plugins/` and
   left for 8 seconds with no load call produced nothing in the log; the same
   folder loaded explicitly through `tm-control-mcp` produced its deliberate
   compile error immediately. The positive control is what makes the negative
   readable — without it, "no diagnostic" is indistinguishable from "the probe
   cannot see diagnostics".

   Confirmed end to end: `echo 'UI::VirtualKey' | scripts/probe_enum_corpus.py
   --names - --template TYPE` — a spelling the shipped b052 corpus records as
   ACCEPTED — comes back **UNDETERMINED** today.

   **B068's sentinel is what makes that visible rather than catastrophic.** The
   probe did not record a false ACCEPT; it reported "analysis did not reach the
   end", re-probed in isolation, and refused to certify. This is the guard
   working in production, on exactly the failure it was built for.

   What this does NOT say: the shipped b052/b053 corpora are still a valid
   record of what the game said on 2026-08-30. What it does say is that the
   EXPLANATION above for how they were produced is wrong, so how they were
   produced is an open question — and that re-probing does not work today
   without routing through `tm-control-mcp`.

   **Why the harness could not tell you this itself** — verified statically by
   lane-resolver, and still true; it is the mechanism behind the measurement
   above rather than a competing account. `fleet.rb_route`
   (`scripts/fleet_dogfood.py`) wraps its whole body in `try/except Exception`
   and RETURNS an error dict; it never raises. `probe_enum_corpus.py`'s
   `run_batch` then calls it for `load_plugin` and **discards the return value
   entirely** — no assignment, no check — before sleeping and reading the log
   window. So a split payload, a `Not implemented:` reply and a refused
   connection are all indistinguishable from success to this harness. That is
   why the watcher theory survived as long as it did: nothing in the code path
   could contradict it.

   **The two findings compose, and the order they landed in is the lesson.**
   The static read established that the harness is blind to its own failure;
   the measurement established that the thing it is blind to is actually
   happening. Neither alone is the answer — a code path that cannot see a
   failure is not evidence the failure occurs, and lane-resolver said so at the
   time, labelling the watcher question as unverifiable from disk. Reasoning
   found the blindness; only the probe found the dark.
2. The `unload_plugin` that follows a failed load is split the same way, so
   `holder.preserve()` fires — correctly — and **leaves the fixture installed**.
   That blocks every later probe from every lane until someone removes it by
   hand. Two such strays appeared in one afternoon.

`scripts/probe_export_binding.py` preflights this and names it in one line
rather than reporting a route error. Copy that preflight into any new probe.

A fix would be in the RemoteBuild plugin itself (`HandleClient` must accumulate
reads until `Json::Parse` succeeds), not in any client here.

### Status update, 2026-08-31 evening: the CLIENT half is fixed. The server is not.

**B097 is NOT closed and this does not close it.** RemoteBuild still truncates
at 64 bytes, `load_plugin` is still unreachable over it, and the fix for that
lives in Max's game plugin, which no lane may touch. What changed is our half.

- **One wire client, not two.** `probe_export_binding.py` carried its own
  length-prefixed reader and its own `tm-control-mcp` client, with a docstring
  asserting `fleet.rb_route` was the broken one — which stopped being true when
  fleet's was fixed. Two readers of one wire format, one right, one stale, is
  the "a rule at one of N sites" shape this batch keeps hitting. Both now live
  in `fleet_dogfood.py` and `probe_export_binding.py` names them.
- **`fleet.game_route(route, data, timeout=)`** picks the transport from a
  MEASURED property of the request — its encoded size against `READ_CAP` —
  rather than from a flag. RemoteBuild carries what it can; `load_plugin`,
  `unload_plugin` and `reload_plugin` go over `tm-control-mcp` when they cannot
  fit. Same signature and reply shape as `rb_route`, so every call site and
  every `rb_route_fn` injection seam is unchanged. **The day RemoteBuild's read
  loop is fixed, this starts using it again with no edit.**
- **A request that fits no transport now returns an error naming its size**,
  rather than falling back to something that cannot deliver it. A silent
  fallback is how a probe reports on a plugin the game never compiled.
- **`fleet.rb_encode` is the single request encoder**, and `rb_request_size`
  measures through it — a size predicate that serialises its own way is a check
  on a request nobody sends. It also switched to compact separators, which is
  not cosmetic against a 64-byte ceiling: `unload_plugin` for a short id is 56
  bytes compact and 65 with Python's defaults, i.e. over the cap. It does not
  rescue `load_plugin` (73 bytes compact with a one-character id, 72 with an
  empty one), and nothing will.
- **`probe_enum_corpus.run_batch` no longer discards the load result.** The
  paragraph above explains why it did; the watcher theory it rested on is
  refuted, so a failed load now stops the batch loudly instead of returning
  UNDETERMINED for every name and reading as merely inconclusive.
- **`fleet.game_alive`** replaces `rb_alive` at the probes' liveness gate.
  `get_status` is 32 bytes, fits inside `READ_CAP`, and answers `Alive` on an
  install where every route that matters is unreachable — a positive control
  that does not exercise the failing path.

**The guard that had to be fixed inside this fix.**
`issue_repro_game._forbid_game_access` — the revocation that makes that
selftest offline BY CONSTRUCTION — listed `fleet.rb_alive` and `fleet.rb_route`
by hand. Adding `game_route` as the default path would have left the selftest
reaching a live game through the new door: the exact one-of-N-call-sites
failure that function exists because of, recurring inside its own repair. It
now DERIVES the door set from a name pattern (`rb_*`, `game_*`, `control_mcp`,
`measure_read_cap`), refuses to run if the pattern matches too few, and the
selftest plants a synthetic `game_zzz_future_transport` to prove a door added
later is closed with no edit there. A hand-written list passes every other
check and fails that one.

**What is unblocked, and what is not.**

| | before | now |
|---|---|---|
| `probe_enum_corpus.py` | silently produced UNDETERMINED for every name | drives the game through `tm-control-mcp`; a failed load is loud |
| `issue_repro_game.py` | unload split, `holder.preserve()` fired, fixture stranded | unload routes over `tm-control-mcp`, so teardown can complete |
| `probe_export_binding.py` | already routed via `tm-control-mcp` | unchanged behaviour, one fewer copy of the client |
| RemoteBuild `load_plugin` | unreachable | **still unreachable** — B097 stays open |

Unverified against a live game as of this writing: the routing is exercised by
stubs and the three offline selftests, not by a game run. The first real probe
should confirm from the game's own log that the plugin compiled, not merely
that the call returned — that is the check the `load_plugin` history says to
make.


**Working alternative, no game restart needed:** `tm-control-mcp` is loaded and
its socket reassembles correctly, with RemoteBuild parity plus `ListPlugins` /
`GetPlugin`:

    cd ~/src/openplanet/my-plugins/tm-control-mcp
    python3 tools/call.py ControlPlugin '{"id":"<plugin>","action":"reload"}'

It returns the compile log inline, attributed to the build you triggered, which
also removes the read-the-shared-log-by-severity hazard. Prefer it over waiting
for a RemoteBuild fix. Tracked as B097.
## Regenerating

```
scripts/probe_enum_corpus.py --b052 docs/probes/b052-qualified-enum-spellings.tsv
scripts/probe_enum_corpus.py --b053 docs/probes/b053-bare-enum-values.tsv
```

Needs the game running with RemoteBuild alive. The script builds the candidate
list from `tests/fixtures/typedb` via the crate's **own loader** (see
`--dump-enums`), never by re-deriving keys in Python — re-deriving is what
produced two confidently wrong counts in this repo's history.

**Coordinate before running.** There is one game on one port and lanes share
it; ask whoever is coordinating rather than taking it.

---

# Probe hygiene: a REJECT is evidence, an ACCEPT may not be

**Read this before trusting any "verified against the game" claim, in this
repo or in a ticket.**

Namespaces declared by **other installed plugins** are visible to a probe
compile. On the machine this corpus was produced on, `MLFeed::VirtualKey` and
`VehicleState::VirtualKey` both compiled — not because the engine has those
namespaces, but because `MLFeedRaceData.op` and `vehicle-state-list-dev` were
installed. `MathX` and `GameData` behaved the same way and were excluded from
the model for that reason.

So:

- A game **REJECT** is solid evidence about the engine. If the compiler says
  `Namespace 'Scene' doesn't exist.` with 44 plugins installed, no plugin
  declared it and the engine does not have it.
- A game **ACCEPT** on an unrecognised prefix is **not** automatically an
  engine fact. It may be an artefact of that machine's plugin set.

The cheap discriminator is a **paired probe**: submit `Prefix::RealName` and
`Prefix::ZZZNoSuchName` together.

- `Namespace 'Prefix' doesn't exist.` for both → the prefix is not a scope.
- `Identifier 'ZZZNoSuchName' is not a data type in namespace 'Prefix' or
  parent` → the prefix **is** a scope, and the first result is meaningful.

Two more traps found the same day:

- **Count distinct NAMES, not index keys.** The loader keys one engine enum
  under several modules — `Hms::EHmsLightMapQuality` and
  `MetaNotPersistent::EHmsLightMapQuality` are one enum. Counting keys
  inflated a B053 estimate 34x (758 vs the true 22) and a B046 estimate
  before it. If a number comes from `len(keys)`, it is probably wrong.
- **Type lookup walks ancestor scopes; value lookup does not.** `UI::VirtualKey`
  compiles (the walk reaches the global enum) but `UI::GlassSmash` does not
  (`No matching symbol`), even though `GlassSmash` compiles bare. Do not
  generalise a type-position result to value position, or the reverse.

## Pattern: make the compiler state the fact you need

The hygiene rule above says a REJECT is evidence and an ACCEPT may not be. That
reads as a constraint on what you can learn. It isn't — it's a constraint on how
you have to ask.

**If the fact you need sits in the weak direction, find a wrong program whose
rejection has to state it.**

B059 needed the return type of an Openplanet-injected method. The obvious probe
is to compile a legal call and see it accepted — but an ACCEPT only says the
call is legal, and says nothing at all about what it returns. Silence cannot
name a type. So instead, submit a program that is wrong *because* of the type
you're asking about:

```angelscript
string s = player.GetCurrentEntityID();
```

```
ERR: Can't implicitly convert from 'uint' to 'string'.
```

The compiler has to name the source type in order to explain the refusal. The
answer arrives in the REJECT direction, where the evidence is solid, and it is
the compiler's own type name rather than your reading of a header, a dump, or a
disassembly.

The same probe shape settles other invisible facts:

| Fact you want | Wrong program that makes the compiler say it |
|---|---|
| return type of `f()` | `string s = f();` — names the real type in the conversion error |
| whether `T` is a handle | `if (expr is null)` — legal only on a handle; CLEAN answers yes |
| arity of `f()` | `f(1)` — `No matching signatures to 'C::f(const int)'` prints the signature |
| whether a receiver resolves at all | `recv.ZZZNoSuchMember()` — a `No matching symbol` proves lookup ran |

That last row is the **paired discriminator** already described above, and it
generalises: an ACCEPT is only meaningful once you have shown that the same
receiver, in the same position, *can* produce a rejection. Otherwise you cannot
distinguish "the compiler checked and approved" from "the compiler never
looked".

Two limits worth stating, so the pattern isn't over-trusted:

- The compiler prints **expression** types, not declaration text. B059's
  `GetManialinkPages` came back as `CGameManialinkPage@[]@&` where the
  registered declaration is `array<CGameManialinkPage@>@` — same type, and the
  trailing `&` is a by-reference return being rendered, not part of the
  declaration. If the distinction matters, confirm it with a second probe
  (the `is null` row above) rather than by decoding the spelling.
- A rejection tells you what the compiler believes *today*, on *this* install.
  It bounds behaviour, not intent. For "what is registered, and is that the
  whole set", static analysis of the binary is the stronger tool — see
  `openplanet-injected-methods.md`. The two are complements: the binary says
  what exists, a probe says what the compiler does with it.

## Reproducing one name by hand

```
scripts/probe_enum_corpus.py --names -            # read names on stdin
echo -e 'Scene::ESceneCharVisNotice\nUI::VirtualKey' | \
    scripts/probe_enum_corpus.py --names - --template TYPE
```

`--template TYPE` submits `<name> v;` (type position); `--template VALUE`
submits `print("" + <name>);` inside a function (value position). The
distinction matters — see the ancestor-walk trap above.

# Engine behaviour established by live probe

Facts about the game's AngelScript runtime that are not written down anywhere
else, recorded here so they are not re-derived. An engine fact in a doc goes
stale invisibly, so each entry names the version it was measured against.

## `double()` of a `Json::Type::Null` value THROWS

Measured in-game 2026-08-30 on Openplanet Next by session
`tm-tournament-station-f7`, which hit it for real:

```angelscript
Json::Value@ v = Json::Parse("this is not json");   // a Json::Type::Null VALUE
bool threw = false;
double got = -1;
try { got = double(v); } catch { threw = true; }
// threw == true; got untouched at -1
```

It does **not** degrade to `0`.

This completes a two-part trap rather than sitting beside it. `Json::Parse`
returns a Null *value* on junk input, not a null *handle*, so the obvious
`if (v is null)` guard is **dead code** and never fires. The conversion you
naturally reach for once that guard disappoints you then throws instead of
yielding a usable zero. Both halves have to be known together: fix only the
first and you walk directly into the second.

**When probing this yourself, the `try`/`catch` is load-bearing, not
defensive habit.** An uncaught throw in a check body aborts that suite *and
every suite after it*, which a runner reports as silence — i.e. as success.
That is the same "absence read as a pass" shape as the `grep` and
foreign-diagnostic traps above, and it is worth recognising as a recurring
class rather than three separate accidents.

## Every known TYPE is a scope; only four MODULE names are

Measured 2026-08-31 on Openplanet 1.29.5 (next, Public, 1234ad9e) for B067.
Corpus: `b067-class-position-scopes.tsv`.

    X is a live scope for `X::Y`  <=>  X is a declared namespace OR X is a known data type

41 engine classes across all 20 modules, split between those with nested enums
and those without, **all** answered scope. Having members registered under the
name makes no difference — a class with nothing in it is still a scope, because
`Type::Nested` is ordinary AngelScript scope-resolution syntax. `string` is a
scope too, so this is about types in general, not engine classes.

The module-position rule from B056 is unchanged and *different*: of the 20
Nadeo module names only `Audio`, `Meta`, `Net` and `GameData` are scopes; the
rest give `Namespace 'X' doesn't exist.` And `GameData` remains the one
unexplained case — it is a scope while being neither a Core-declared namespace
nor, as measured here, a data type (`Identifier 'GameData' is not a data type
in global namespace`). The obvious unification is therefore refuted; do not
re-derive it.

Practical consequence: `CMwNod` being a scope is not an anomaly to special-case,
it is the general rule for type names.

# Harness hazard: never trust a batched ACCEPT

`probe_enum_corpus.py` puts every name in a batch (default 200) into ONE
compilation unit and attributes diagnostics by line, recording ACCEPTED when a
line drew no message. **A parse error anywhere in the unit aborts compilation
before semantic analysis, so no line draws a message and every other name in
the batch is recorded ACCEPTED.** Demonstrated: `CMwNod::ZZZNoSuchName` flips
REJECT -> ACCEPT purely by sharing a batch with `int::ZZZNoSuchName`, which
cannot parse because `int` is a keyword. Tracked as **B068**.

This manufactures precisely the verdict this document says not to lean on, and
it is silent — the run reports success and emits a complete-looking corpus. It
briefly convinced the coordinator that a name they had *invented*
(`ZZZNoSuchClass::`) was accepted by the game, which would have "disproved"
B056.

Two rules follow:

- **Probe with `batch=1` when the answer matters**, or otherwise ensure a
  parse-class diagnostic forces a name-by-name re-run.
- **Always run a negative control** — a name you are certain is not a scope,
  in the same run. If your fabricated name comes back ACCEPTED, the run is
  void, not surprising. A batch that returns a distinct message for *every*
  name is sound, because an abort yields zero messages rather than some.

## Fixed in the harness (B068), and what it means for the corpora

The script now emits a **sentinel** on the last probed line of every unit — a
name the game must reject semantically. Silence on a name counts as ACCEPTED
only when the sentinel's own diagnostic came back, which proves analysis
reached the end of the unit. Otherwise the name is `UNDETERMINED` and is
re-probed on its own. That makes batch size a speed knob rather than a
correctness one, and `--batch` is now settable; `just probe-selftest` checks
the logic offline, with no game.

The negative-control rule above still stands. The sentinel is a *positive*
control and they answer different questions: the sentinel proves the compiler
got to the end of your unit, a fabricated name proves your ACCEPTs are not
being manufactured by the machine's installed plugins.

### b052 has been re-probed. The batching defect never fired on it.

All **742 ACCEPTED rows** of `b052-qualified-enum-spellings.tsv` were
re-measured at `batch=1` against the live game on 2026-08-31, with the three
known-answer controls re-run every 100 names. Result:

| | |
|---|---|
| re-probed | 742 |
| unchanged (still ACCEPTED) | 739 |
| flipped to REJECTED | 3 |
| UNDETERMINED | 0 |

The delta MUST be read in two buckets, because they mean opposite things:

| bucket | rows | meaning |
|---|---|---|
| **A — batching fabricated an ACCEPT** | **0** | the real B068 result |
| **B — the probing environment changed** | **3** | a corpus-provenance fact, not a corpus error |

**Bucket A is empty. Not one row was fabricated by the batching defect.**

The three bucket-B rows are `Foo::Bar::VirtualKey`, `Foo::ESceneCharVisNotice`
and `Foo::VirtualKey`. **Do not "correct" them.** They were valid measurements
of a machine on which the namespace `Foo` existed; it does not exist here.

That is proven, not inferred, by the REJECTED rows that pin the context — and
those are exactly the rows an ACCEPTED-only re-probe would never look at:

```
Foo::ZZZNoSuch       then: Identifier 'ZZZNoSuch' is not a data type in namespace 'Foo' or parent
                     now : Namespace 'Foo' doesn't exist.
Foo::Bar::ZZZNoSuch  then: Identifier 'ZZZNoSuch' is not a data type in namespace 'Foo::Bar' or parent
                     now : Namespace 'Foo::Bar' doesn't exist.
MathX::ZZZNoSuch     then: Identifier 'ZZZNoSuch' is not a data type in namespace 'MathX' or parent
                     now : Identifier 'ZZZNoSuch' is not a data type in namespace 'MathX' or parent
```

The *is-a-scope* message form ("… in namespace 'Foo' or parent") can only be
produced when the namespace exists. `Foo` produced it then and produces the
namespace-absent form now; `MathX` produces the same form in both runs. The
namespace, not the verdict logic, is what changed.

`build_source` declares no namespace anywhere, so `Foo` was never supplied by
the probe fixture. Those three spellings are excluded from what this crate is
asked to resolve — recorded in `b052-scope-provenance.tsv`, which is where that
decision lives now. **No test expectation and no documented conclusion moves.**

### Two claims here, and only one of them is proven (corrected 2026-08-31, B080)

An earlier revision of this paragraph — merged, and merged *because* it proved
rather than asserted — went on to say `Foo` "came from the game's own loaded
plugin set" and that "a plugin declaring `namespace Foo` was installed when the
corpus was generated". The first half of that is proven. The second is an
attribution the same evidence does not carry, and it is demoted.

**PROVEN, and it stands.** `Foo` existed as a namespace when the corpus was
generated and does not exist now. The message forms establish it directly: the
is-a-scope form cannot be produced when the namespace is absent, `Foo` gave that
form then and the namespace-absent form now, and `MathX` gives the same form in
both runs as the control.

**NOT PROVEN, and demoted.** That an *installed plugin* supplied it. What is
actually established is only that the engine did not and this repo's probe
templates did not — the templates emit no `namespace` in either form, and no
committed repo fixture declares one either. An installed plugin, a hand-staged
fixture, and **another client's staged probe** are indistinguishable from
anything recorded. That third one stopped being hypothetical when B097's
investigation showed this port has had two clients; for a scope literally named
`Foo::Bar`, it is the obvious candidate. So the sidecar records `Foo` and
`Foo::Bar` as `HOST_UNATTRIBUTED`, not `HOST_PLUGIN`.

**B068's conclusion is untouched, and read the demotion as narrowing the
attribution rather than weakening the result.** Those rows are still not
batching fabrications, and the pins are why: a fabricated ACCEPT means the
namespace never existed, in which case `Foo::ZZZNoSuch` in that same run would
have answered `Namespace 'Foo' doesn't exist.` It answered the is-a-scope form,
and a REJECT cannot be fabricated by an abort. **Zero rows fabricated stands.**
Only *whose* namespace it was loses strength.

The general shape, since it caught two readers including the one who wrote it:
**the message form is evidence about a namespace's EXISTENCE, not about its
SUPPLIER.** One instrument, two questions, and it only answers the first.

The whole twelve-entry artefact list splits along the same line, which is what
identifies the mechanism rather than merely asserting it: the three `Foo::*`
spellings flipped, while all nine `GameData::*`, `MLFeed::*`, `MLHook::*`,
`MathX::*` and `VehicleState::*` spellings still accept — and the matching
`::ZZZNoSuch` probes confirm those namespaces still exist. The re-probe
therefore CONFIRMS an exclusion list a previous lane had reasoned out rather
than measured.

**Two corrections to that paragraph, from B080.** It says the `::ZZZNoSuch`
probes confirm those namespaces: they confirm four of the six — `MLHook` and
`VehicleState` have no pin row in this corpus at all, so nothing in it witnesses
their scope-hood in either direction. And "artefact" is wrong for `GameData`,
which no installed plugin declares; all four of its rows are an engine scope
neither database records. Both are now recorded per scope in
`b052-scope-provenance.tsv` instead of in prose, and both are asserted.

**Carry this forward when re-measuring anything here.** Restricting a re-probe
to the rows a known defect could affect also blinds it to every defect you were
not thinking about. ACCEPTED-only was correct for B068 — an abort cannot
manufacture a REJECT — and it excluded the `*::ZZZNoSuch` rows that are the
only witnesses to the environment changing underneath the corpus. Re-probe the
context-pinning REJECTs too, whatever the narrowing.

The shape of the result is itself the argument that B068 never fired here: an
aborted batch fabricates ACCEPTED for *every* surviving name in the unit, so a
200-name batch would show ~199 spurious rows. Three isolated spellings sharing
one namespace family, each with a stated environmental cause, is not that shape.

REJECTED rows were never at risk and were not re-probed: an abort produces the
*absence* of diagnostics, and a REJECTED row required the game to name that
spelling, so the defect can only invent an ACCEPTED. Every "the LSP must reject
this" assertion built on these files was always sound, and B056's narrowing
claim is unaffected.

### b053 has been re-probed too. Nothing changed at all.

All **592 ACCEPTED rows** of `b053-bare-enum-values.tsv` were re-measured at
`batch=1` on the same day. **592 unchanged, 0 changed, 0 UNDETERMINED** —
both buckets empty, including the environmental one.

Bare value names have their own version of the `Foo` drift: a plugin declaring
a *global* enum with a value named `X` makes bare `X` compile, and if that
plugin goes away `X` stops compiling. Sampled `NO_SYMBOL` rows were re-probed
as context pins to detect it (`AAAA`, `Nadeo`, plus two fabricated names) and
all came back unchanged, so no value-level drift touched this corpus between
the two runs.

The run asserts its own batch size. `--batch 1` is an intention; the driver
wraps `run_batch` and records the unit size actually reached — `[1]` — into the
checkpoint artifact. That check exists because `--batch` was once accepted,
documented, and ignored by both corpus writers, so a re-probe could have run at
200 while reporting 1, reproducing the defect it was built to detect and
returning a clean bill of health. A knob wired to nothing looks identical from
the outside.

**Both corpora are now re-measured and sound.** Across 1334 ACCEPTED rows, zero
were fabricated by the batching defect.

### Re-measuring a corpus yourself

    scripts/probe_enum_corpus.py --reprobe b052   # or b053
    scripts/probe_enum_corpus.py --reprobe b052 --checkpoint path.jsonl

Resumable — every verdict is fsynced before the next probe, so an interrupted
run costs the remainder, not the whole. Needs the live game. It runs the
corpus's own controls at the start, every 100 names and at the end, aborting
rather than writing verdicts if any drifts; re-probes the context pins; scores
with the same `type_verdict` / `value_verdict` the writers use; and records the
unit size it actually achieved.

Sort any change into **two buckets and never merge them**: rows the batching
defect fabricated, and rows that changed because the probing environment did.
The context pins tell you which — see the `Foo` case above.

**A verdict this document does not yet contain: `UNDETERMINED`.** If a
regenerated corpus carries one, `global_scope.rs` panics on it by design, but
`resolver.rs` computes `verdict == "ACCEPTED"` and would silently read it as
REJECTED — an unknown asserted as a fact.

So a corpus containing one is never written to its real path. The generator
parks it at `<name>.partial`, leaves the existing file untouched, and exits
non-zero. Exiting alone was not enough: the file had already been written, and
an ignored exit code would have left a complete-looking TSV that the Rust
suite reads without complaint. Two things can make a run indeterminate — a
value that could not be probed, and an enum whose own type probe was
indeterminate, which silently shrinks `global_enum_owners`, the column that
decides the B053 verdict. Both are counted.

## B096 claim 1 — a namespaced body DOES bind, when the namespace is the module's

`b096-namespaced-export-binding.tsv`, 6 variants, 0 ambiguous, both of B086's
controls correct and B086's `lax-by-value` row reproduced — so the harness was
re-validated against a known result, not merely trusted.

**The answer: the engine binds by the FULLY-QUALIFIED name, not by simple name.**

    body in `namespace <module>`,  stub in `namespace <module>`   -> BOUND
    body in `namespace Other`,     stub in `namespace <module>`   -> UNBOUND
    body plain global,             stub in `namespace <module>`   -> BOUND

So **B087's assumption holds for the shape that ships.** tm-editor-plus-plus
declares its module as `Editor` AND puts its bodies in `namespace Editor`, so
the body's namespace *is* the module namespace its export file already wraps
the stubs in. Those exports are live, not dead.

**The near-miss is worth more than the verdict.** The first run of this probe
put the body in `namespace Editor` while the module was named something else,
measured UNBOUND, and would have been reported as "four shipped plugins have 51
dead exports each". The fixture did not match the plugins it was about. That is
exactly B086's failure — a fixture asserting engine behaviour, cited as a
measurement — and it was caught only by reading E++'s actual source to check
the shape. **A probe's fixture is a claim about the code it generalises to, and
that claim needs checking too.** Both variants are kept in the corpus so the
two cases can never be conflated again.

Consequence for the LSP: B087 roots a namespaced body from a namespaced stub by
SIMPLE name, which over-approximates — it would also root the `UNBOUND` row.
That is B062's deliberate direction (a missed warning, never a false positive),
so it stays, but it is now measured rather than assumed.

## B096 claim 2 — the engine accepts all four `shared` forms, and `shared` is LIVE

`b096-shared-modifiers.tsv`, 24 rows, produced 2026-08-31 against **Openplanet
1.29.14** by `scripts/probe_shared_modifiers.py`. 16 measured variants, 5
controls (all correct), 1 discriminator pair, 2 rows honestly AMBIGUOUS.

**The answer: ACCEPT, uniformly.** `shared interface`, `shared enum`,
`shared funcdef` and a `shared` function all compile and load — at top level
and inside a `namespace`, in a file listed under `[script] shared_exports`
and in an ordinary plugin file in no export list at all. So B087 records
`is_shared` for forms the game really does accept; nothing to fix.

**But "it compiles" is a weaker claim than "the engine implements it", and 16
uniform ACCEPTs is also the shape a modifier the parser SKIPS would produce.**
A pair differing only in the keyword settles it:

    shared enum BlockPlacementType { ZZZMismatch }  -> REJECT
           enum BlockPlacementType { ZZZMismatch }  -> ACCEPT

    Shared type 'BlockPlacementType' doesn't match the declaration in
    module 'Editor'

So `shared` is semantically live and engine-wide **by fully-qualified name,
with no `dependencies` entry required** — the type is visible across modules
that never declared any relationship. That makes a name collision with any
loaded plugin a REJECT that is *not* about the modifier, which is a
contamination channel unique to this claim; the corpus already contains
`IInputChange` and `CheckpointIxTime` declared shared in more than one plugin.
Every name and module in the fixtures is uniquely suffixed for that reason.

### Fixture fidelity: `shared` ships in TWO contexts, and only one is obvious

Reading the installed corpus BEFORE writing fixtures (the claim-1 lesson)
found that shipped `shared` declarations live both in `shared_exports` files
*and* in ordinary files that are in no export list:

- **in `shared_exports`** — `ai-api/AiApi_Interfaces.as`,
  `map-together/Export_Shared.as`, `dips++-dev/Ex/Shared.as`,
  `Editor/Editor/Shared_General.as`, `Editor/Exports/Callbacks_Shared.as`;
- **in no export list** — `Editor/FormatX.as` (`shared string Vec2`),
  `Editor/MathX.as` (`shared float Max`), `Editor/UI/UI_Util.as`,
  `Editor/Editor.as:591` (`shared enum EditorAction`), and
  `EppLines/Exports_Shared.as`, whose `shared_exports` line is **commented
  out** ("shared exports disabled until contract frozen") while the file still
  carries `shared interface IGeomLeased`.

A fixture covering only one context would have misreported the other. Both are
probed, every row names the shipped code it stands in for, and both are kept.

### Two false-green channels in the shared probe harness

Found by this run, and they affect `issue_repro_game.probe` generally — so
they are not local to this ticket. Both were caught only because a fixture
whose dependency id was wrong was scored **ACCEPT**:

1. **`fleet.read_log_window` drops every `[ TRAC]` line, and
   `Loaded plugin '<id>'` is a TRAC line.** So `compiled_clean` means "no ERR
   attributed to me" — absence of evidence, not proof the build ran. A build
   that never happened reads exactly like a clean one. This probe injects an
   UNFILTERED window through the `read_log_window_fn` seam and requires the
   positive `Starting build for "<fid>"` / `Loaded plugin '<fid>'` bracket.

2. **`fleet.parse_game_diagnostics` only matches
   `path (line, col) : SEV : msg`, so engine errors with NO source location
   are invisible to it** — invisible to `game_errors`, to `foreign_errors`
   AND to `unattributed_errors`, so `contended` stays False too. Measured:

       [ERROR] Couldn't find required plugin dependency "ghosts-pp" while
               loading plugin "<fid>"
       [ TRAC] Loaded plugin '<fid>' (version 0.1.0)

   Under the inherited oracle that scores a clean ACCEPT. `raw_build_errors`
   reads the bracketed slice directly and reports `BUILD-ERR`, deliberately a
   distinct verdict from `REJECT`: "your dependency is missing" must never be
   readable as "the engine rejects this form".

**`Loaded plugin` is logged even after `Script compilation failed!`** — so it
is necessary but not sufficient, and an oracle keying on it alone is a third
false green. This probe checks attributed errors first.

### The one thing path attribution cannot score

Two rows are AMBIGUOUS, correctly. When a fixture depends on `Editor`, the
dependency's `shared_exports` file is compiled INTO the probe's unit, so a
collision raises errors carrying **`Editor`'s** source path:

    Plugins/Editor/Editor/Shared_General.as (125, 13) : ERR : Name conflict.

Path-based attribution counts those as *foreign*, though the probe's own code
caused them. It fails SAFE — AMBIGUOUS, never a wrong verdict — but those rows
cannot be scored, which is why the decisive discriminator uses **no dependency
at all**. It does not need one: shared types are engine-wide already.
