# Game probe corpora

Verbatim verdicts from the **live game compiler** on names this crate has to
decide about. Committed because re-probing costs a running Trackmania +
Openplanet session with RemoteBuild on port 30000, and because several open
tickets cite these files as their starting corpus.

Produced by the B052/B053 lane, 2026-08-30, against **Trackmania + Openplanet
1.29.5 (next, Public, 1234ad9e)**, type DB `tests/fixtures/typedb` (Nadeo dump
dated 2026-02-03), via `scripts/probe_enum_corpus.py`.

| file | rows | what it answers |
|---|---|---|
| `b052-qualified-enum-spellings.tsv` | 1901 | for a qualified *type* spelling (`Scene::ESceneCharVisNotice`, `UI::VirtualKey`, `CGameMenu::EMenuOrder`), does the game accept it, and if not, what does it say? |
| `b053-bare-enum-values.tsv` | 2989 | for a bare *value* name of a named enum (`Turbo`, `GlassSmash`), does the game accept it, reject it as unknown, or reject it as ambiguous? |

Both are TSV with a `#`-comment header. The `game_message` column is the
compiler's own text, unedited.

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
  are named in that test rather than silently skipped: the 13 spellings that
  compiled only because of the probing machine's installed plugins (see the
  asymmetry below — `GameData::EGameInventoryItemClass` is the subtle one,
  since `GameData` is *also* a Nadeo module name), and the 4 `string::` rows,
  which are a lexer gap rather than a resolution one.
- **B057** (landed) — the 2373 bare value names the LSP used to accept and the
  game answers `No matching symbol` for. Filter `b053-bare-enum-values.tsv`
  for `verdict == NO_SYMBOL`.

  **`b053-bare-enum-values.tsv` is now read by the test suite, not only by
  people.** `global_scope::tests::every_probed_bare_value_matches_the_games_verdict`
  parses every row and asserts the crate's answer matches the game's, so the
  file is load-bearing for `cargo test`: editing a verdict, a value name or the
  row count changes what the suite demands. Re-probe before you edit it, and
  say so in the commit.

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
the probe fixture — it came from the game's own loaded plugin set. A plugin
declaring `namespace Foo` was installed when the corpus was generated and is
not installed now. That is precisely the plugin-namespace effect documented
above, and precisely why those three spellings are already the first three
entries of `PLUGIN_NAMESPACE_ARTEFACTS` in `resolver.rs`, which forces them to
"must not resolve" whatever the corpus says. **No test expectation and no
documented conclusion moves.**

The whole twelve-entry artefact list splits along the same line, which is what
identifies the mechanism rather than merely asserting it: the three `Foo::*`
spellings flipped, while all nine `GameData::*`, `MLFeed::*`, `MLHook::*`,
`MathX::*` and `VehicleState::*` spellings still accept — and the matching
`::ZZZNoSuch` probes confirm those namespaces still exist. The re-probe
therefore CONFIRMS an exclusion list a previous lane had reasoned out rather
than measured.

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
