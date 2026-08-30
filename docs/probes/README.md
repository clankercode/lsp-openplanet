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

- **B056** — 1010 qualified spellings the LSP accepts and the game rejects.
  Filter `b052-qualified-enum-spellings.tsv` for `REJECTED`; most are the
  synthetic Nadeo module prefix (`Game::CGameCtnApp`) or a bare nested-enum
  name (`EMenuOrder`).
- **B057** — 2373 bare value names the LSP accepts and the game answers
  `No matching symbol` for. Filter `b053-bare-enum-values.tsv` for
  `verdict == NO_SYMBOL`.

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

## Reproducing one name by hand

```
scripts/probe_enum_corpus.py --names -            # read names on stdin
echo -e 'Scene::ESceneCharVisNotice\nUI::VirtualKey' | \
    scripts/probe_enum_corpus.py --names - --template TYPE
```

`--template TYPE` submits `<name> v;` (type position); `--template VALUE`
submits `print("" + <name>);` inside a function (value position). The
distinction matters — see the ancestor-walk trap above.
