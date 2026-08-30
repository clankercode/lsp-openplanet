# Changelog

All notable user-facing changes to openplanet-lsp are documented here.

Format inspired by [Keep a Changelog](https://keepachangelog.com/).
Versions follow [SemVer](https://semver.org/).

<!--
Release agents: add a new ## [X.Y.Z] - YYYY-MM-DD section at the top when
cutting a release. See RELEASE.md. After release.yml succeeds, update the
GitHub Release body to match this section (gh release edit).
-->

## [Unreleased]

### Added
- **`openplanet.regenerateTypeDb` reports what changed** (B039) — the command
  now answers with the type-database build stamps BEFORE and AFTER the
  reload (`before` / `after` / `changed`, using the same `op` / `mp` /
  `game_op` shape the `check --json` report already uses), not just an entry
  count. `before` is what the server had LOADED, not a re-probe of disk, so an
  unchanged pair means the reload picked up nothing new — the ordinary case
  when the game has not written a fresh dump yet, and one a bare "done" hides.
  Reachable from VS Code since the extension's B039 change; before that it was
  advertised and no client could invoke it.
- **`openplanet/resolvedDefines` — config introspection** (B050) — a custom
  LSP request returning the EFFECTIVE preprocessor define set with an origin
  per entry (`target` / `signature` / `os` / `arch` / `build` / `manifest` /
  `dependency` / `dependency-missing` / `user`), whether each is currently
  active, and the state it would have without the user's `defines` overrides.
  Until now the set was computed entirely server-side by the five-layer ladder
  (B017/B019/B024/B028) and no client could read any of it, so the VS Code
  Manage Defines list could only show the user's own settings array — a
  partial truth with no indication it was partial.

  Two things the flat set could not express are now reported. A define the
  user turned off with `-NAME` comes back present-but-disabled, keeping its
  derived origin, so "remove my override" and "disable this define" stay
  different edits. And a dependency declared in `info.toml` that does NOT
  resolve on this machine is reported inactive under `dependency-missing` —
  it has no define (B019, matching the game), and that row is the whole
  explanation for a dead `#if DEPENDENCY_X` branch.

  The answer also carries the resolved `game_target` and the platform-family
  table, so a client can stop duplicating `LspConfig::PLATFORM_FAMILIES`.
  Read-only, and a query rather than a fifth `executeCommand`. Registered in
  `server::build_service`, the single constructor `run_stdio` uses.

- **Quick fix for the `WithRunContext` migration** (B048) — `awaitable.WithRunContext`
  compiles clean but discards the run context at runtime, and the warning for
  it now comes with an edit. The migration is a call restructure, not a
  rename: `startnew(Worker).WithRunContext(ctx)` becomes
  `Meta::StartWithRunContext(ctx, Worker)`, with the run context first and
  every `startnew` argument following in its original order (signature taken
  from the shipped Openplanet type DB, not from an example).

  The fix is withheld — the warning still fires — wherever the coroutine
  cannot be read off the source: a receiver that is not a `startnew(...)`
  call (a stored handle, a function result), a named argument in either call,
  an unexpected argument count, and a boolean userdata, for which
  `Meta::StartWithRunContext` has no overload. A wrong rewrite here would
  silently drop the coroutine, so anything unrecognised offers nothing.
  Never applied by `source.fixAll.openplanet`.
- **Quick fixes for unused declarations** (B036) — an unused local or
  parameter now offers an edit, not just a squiggle. Prefixing the name with
  `_` is offered for both and is the preferred fix for a **parameter**, where
  deleting it would change the signature and every call site. A **local** also
  offers removal, and neither fix is preferred there because removal is lossy.

  Removal is withheld wherever it could lose something: an initializer that is
  not a literal gets no removal action at all, except a **call**, which is kept
  as a statement (`int x = Foo();` becomes `Foo();`) rather than silently
  dropped. A statement declaring several names, a `for` initializer, and an
  unbraced branch body are never removed either — in the last two the
  declaration's `;` is structural, so deleting it would break the code. A name
  that already starts with `_` is offered nothing, and file-level declarations
  and class fields are out of scope.

- **Inactive `#if` branches are greyed out in the editor (`inactive_regions`)**
  — code the current defines switch off now gets a HINT diagnostic tagged
  `Unnecessary` over each dead body, so an editor fades it the way it fades
  `#if 0` in C (VS Code gates this on `editor.showUnused`). Default `hint`;
  `info` also lists them in the problems panel, `ignore` marks nothing.
  Configurable via `inactive_regions` in `.openplanet-lsp.toml`, the user
  config, or `initializationOptions`.

  **LSP-only** — `check` and the TUI never emit these at any level, so a
  plugin full of `#if MP4` does not drown the CLI output. There is
  deliberately no warn/error level: a branch that is off for your target is
  dead by design, not a defect.

  One diagnostic per contiguous dead body: consecutive dead lines coalesce,
  the `#if` / `#elif` / `#else` / `#endif` lines stay at normal weight, and a
  nested block inside an already-dead body folds into the outermost span
  rather than splitting it.
- **Quiet levels for unused declarations: `unused = "hint" | "info"`** — "never
  used" is advisory, not a defect, so it no longer has to be a warning.
  `info` keeps the findings listed in an editor's problems panel; `hint` drops
  them out of it and leaves only the greyed-out range. Both are non-fatal:
  `check` exits 0 even under `--warnings-as-errors`, which still promotes
  WARNING only. Available everywhere the key already worked — `--unused
  hint|info` on the CLI (aliases `grey`/`faded` and `note`/`information`),
  `unused = "hint"` in `.openplanet-lsp.toml` or the user config, and
  `initializationOptions` from an editor.
- **Unused findings carry the LSP `Unnecessary` diagnostic tag** — at every
  level, including `warn` and `error`. That tag, not the severity, is what
  makes an editor fade the declaration (VS Code gates the grey-out on
  `editor.showUnused`), and it is what keeps `unused = "hint"` visible when a
  client omits HINT diagnostics from its problems list. Clients without
  `publishDiagnostics.tagSupport` ignore it.
- **`signature_mode` — choose which `SIG_*` defines are live (#64)** — the LSP
  used to define `SIG_OFFICIAL`, `SIG_REGULAR`, `SIG_SCHOOL` and
  `SIG_DEVELOPER` unconditionally, so a plugin could never be analysed the way
  it compiles once signed. Openplanet defines a signature macro "when the
  current signature level is equal or below their respective level", so the
  levels are cumulative: `official` → `SIG_OFFICIAL`; `regular` → + 
  `SIG_REGULAR`; `school` → + `SIG_SCHOOL`; `developer` → all four. The new
  `signature_mode` key (`.openplanet-lsp.toml`, user config,
  `initializationOptions`, config TUI) defaults to `developer` — exactly the
  old set — so nothing moves until you opt in. Aliases `dev` / `reg` / the
  `SIG_*` token itself are accepted; an unknown value warns and keeps
  `developer`, falling back permissive because masking code out is what
  manufactures undefined-identifier errors. `defines` overrides still get the
  last word, so `defines = ["-SIG_DEVELOPER"]` works on top of any level.
- **`check --game-target NAME` and `check --signature MODE` (#64)** — the two
  selectors are now settable per run, for CI that asks "does this still
  compile as Regular on MP4?". Both ride the `initializationOptions` layer, so
  the define set is re-derived AND the typedb variant / game JSON re-probe
  follows the target (GH #45); both reject an unknown value with a usage error
  naming the valid set, and neither can swallow a following flag (GH #62).
- **`os_target` / `arch_target` selectors + `check --os` / `check --arch`
  (B028)** — the OS and architecture defines are selector keys now,
  resolved through the same ladder as `game_target` / `signature_mode`
  instead of being hand-listed. `os_target` is `windows` (default) |
  `wine` | `linux`: a Wine run defines BOTH `WINDOWS` and `WINDOWS_WINE`,
  as the live game does (verified with a RemoteBuild probe, 2026-08-29);
  `arch_target` is `64` (default) | `32`. The build-shape flags are
  selector keys too — `developer_build`, `has_dev`, `logs`, `server`
  (all ON by default, see the Changed entry) — with matching
  `--developer-build` / `--no-developer-build`, `--has-dev` /
  `--no-has-dev`, `--logs` / `--no-logs` and `--server` / `--no-server`
  flag pairs. Every selector is honored in `.openplanet-lsp.toml`, the
  user config, `initializationOptions` and the CLI, where the flag wins;
  `defines` overrides still get the last word. An unknown `--os` /
  `--arch` value is a usage error (exit 2); an unknown config value warns
  and keeps the previous setting.

### Fixed
- **Openplanet-injected engine methods no longer report as missing (B059)** —
  `player.GetCurrentEntityID()` on a `CSmPlayer@`, and
  `net.GetManialinkPages()` on the network types, compile in the game but were
  reported as `type ... has no member ...`. Openplanet registers a small set of
  methods on engine classes at runtime, and the Nadeo type dump — which is what
  `openplanet-lsp` reads — does not list them. `MwAddRef` / `MwRelease` were
  already special-cased; these two were not, so plugins calling them had to
  whitelist the message to keep their own LSP gate useful (tm-dips-plus-plus
  did exactly that).

  Both now carry their real return type rather than being merely silenced, so
  `auto entId = player.GetCurrentEntityID()` is a `uint` and
  `net.GetManialinkPages()` is an `array<CGameManialinkPage@>@` you can index
  and take `.Length` of. Genuinely missing members stay reported: the six
  registrations are matched exactly, so a typo, a wrong receiver type, or a
  call with arguments is still an error.

  The registration list is taken from `Openplanet.dll` itself rather than from
  API docs, and every receiver and both return types were then confirmed
  against the live game compiler — see
  `docs/probes/openplanet-injected-methods.md`.

||||||| parent of 3ec242b (fix B056: a type answers to its SOURCE NAME, not to the loader's index key)
- **Qualified engine names are checked against the spelling the game actually
  accepts (B056)** — `Game::CGameCtnApp` used to resolve, and the game answers
  `Namespace 'Game' doesn't exist.` So did `Plot::Marker` (the enum is
  `UI::Plot::Marker`) and a bare `EMenuOrder`. 1012 spellings the game rejects
  were accepted in silence.

  The `Game`, `Scene`, `Audio`, … prefixes are how `openplanet-lsp` keys the
  Nadeo dump, not namespaces the game has, and the index accepted its own key.
  Every type and enum now answers to its SOURCE NAME instead — and that name
  depends on where the entry came from, which is the part worth knowing:

  - a Nadeo entry DROPS the module prefix: write
    `CGameMenu::EMenuOrder`, `CAudioScriptManager::ELibSound`;
  - a Core entry keeps its namespace IN FULL: write `UI::Plot::Marker`,
    `UI::ButtonFlags`, `nvg::Align`;
  - an `Openplanet.h` enum is bare: `ESceneCharVisNotice`.

  A global enum still resolves behind any real scope, so `UI::VirtualKey`,
  `CGameMenu::VirtualKey` and `Meta::ESceneCharVisNotice` are unaffected, and
  so is referring to an engine class by its short name (`CGameCtnApp`). Your
  own declarations still win: a plugin's `enum Axis` keeps shadowing
  `UI::Plot::Axis`.

  All 1901 spellings were compiled in the live game to establish the rule, and
  the verdicts are committed at
  `docs/probes/b052-qualified-enum-spellings.tsv`.

- **A bare enum value only a nested or namespaced enum defines is now an error
  (B057)** — the game answers `No matching symbol 'North'`; `openplanet-lsp`
  accepted it and said nothing, so the problem only appeared when you compiled
  the plugin. 2373 engine value names are affected.

  Being *named* is what lets you write an enum's own name; it is not what puts
  the enum's values in scope for a bare reference. Only enums the engine puts
  in the global namespace do that. So `North` (from
  `CGameCtnBlock::ECardinalDirections`) and `Appearing` (from `UI::Cond`) are
  now flagged, while `Jump`, `GlassSmash`, `Space` and the other 592 the game
  accepts are untouched — as is any name your own plugin declares.

  This is B044 one level more general: an anonymous enum was already excluded
  for having no name at all. Every one of the 2989 bare value names in the
  shipped type DB was compiled in the live game to establish the rule, and the
  verdicts are committed at `docs/probes/b053-bare-enum-values.tsv`.
- **An unknown `workspace/executeCommand` is now a JSON-RPC error** (B039) —
  it used to answer `Ok(null)`, which a client cannot tell apart from a
  command that ran and had nothing to report. That is precisely how an
  advertised-but-undispatched command stays invisible, so the advertised list
  and the handler's dispatch now come from one place
  (`server::commands::ADVERTISED`) and `tests/execute_command_tests.rs` boots
  a real service, reads the advertised list back off its own `initialize`
  response, and executes every entry — a gate that fails for ANY future
  advertised-but-unimplemented command, not only this one.
- **A bare enum value two enums define is now an error, as in the game
  (B053)** — the game refuses to choose between them (`Found multiple matching
  enum values`); `openplanet-lsp` took the first match and said nothing, so the
  problem only appeared when you compiled the plugin. The diagnostic names the
  candidate enums, so qualifying it — `ESceneCharVisNotice::Turbo` — is the
  obvious fix.

  Which enums are in scope for a BARE name is narrower than it looks, and this
  follows the game exactly: only enums the engine puts in the global namespace
  take part. A nested enum's values (`CGameMenu::EMenuOrder`) and a namespaced
  one's (`UI::Cond`) are not reachable bare at all, so they cannot collide.
  Your own plugin counts too: a global `enum` of yours JOINS the pool and can
  make a previously fine name ambiguous, while a global variable or function
  of that name simply wins and nothing is reported.

  22 engine value names are affected, `Turbo`, `None`, `High`, `Medium` and
  `Up` among them.

- **Qualified paths to a global engine enum are checked against real scopes
  (B052)** — `Scene::ESceneCharVisNotice` used to resolve, and the game
  answers `Namespace 'Scene' doesn't exist.` The `Game`, `Scene`, `Plug`,
  `System`, … prefixes are how `openplanet-lsp` keys the engine dump, not
  namespaces the game has, and a qualified enum reference was accepted on the
  strength of its last segment alone.

  It now models what AngelScript actually does: look the name up in the named
  scope, then walk that scope's ancestors out to the global namespace. So a
  global enum still resolves behind any REAL scope — `UI::VirtualKey`,
  `Meta::ESceneCharVisNotice`, `CGameMenu::VirtualKey`, or a namespace your own
  plugin declares — while a path through a scope that does not exist is now the
  error the game reports. An engine class counts as a scope exactly when it
  declares an enum, which is how Openplanet registers one.

  The same change teaches `has_enum` a genuine suffix match, so a nested enum
  spelled the way the game wants it — `CGameMenu::EMenuOrder`, class scope and
  no module prefix — is recognised as an enum instead of falling through.

- **Rename and find-references stop at the strict-export boundary (B055)** —
  a plugin with strict `[script] exports` compiles as two units that cannot
  see each other, and the same name may legally be declared on both sides.
  Rename and find-references used the whole plugin, so a rename of the
  own-module `SendEvent` also rewrote the unrelated `SendEvent` in the export
  file, and each was reported as a reference to the other. Each request now
  classifies against the module the requesting file belongs to, and narrows
  the files it may rewrite only when the name really is declared on both
  sides — so an ordinary rename stays complete. `shared_exports` are in both
  modules and stay visible from either side. Verified by live game probe, not
  by argument; the fixtures under `tests/fixtures/issue-repros/b055-*` record
  what the game does.

  Renaming a strict-export declaration at its declaration site is ALLOWED.
  There can be no own-file caller to leave broken — the game rejects one —
  and refusing would have made the most common legitimate rename of a
  plugin's public API impossible while claiming a cross-plugin safety
  property the server has never had for `shared_exports` either.

- **Interface members are visible to every language feature (B054)** — the
  symbol table registered an interface as one symbol and never descended into
  its methods, so no `I::method` symbol existed at all. Hover on an
  interface-typed receiver reported `(local) <error> Go` instead of the
  method, completion after `i.` offered nothing, and workspace symbols could
  not find `I::Go`. All three now work, and interface methods never warn as
  unused (they are contract declarations reached by virtual dispatch).

- **Go to Definition on `receiver.member` (B054)** — it had no receiver step
  at all: it tried the bare name as a global, then a `this.` / implicit-`this`
  fallback, so `c.Go()` on a class-typed receiver resolved nothing just as
  `i.Go()` on an interface-typed one did. Member accesses now resolve against
  the receiver's type and its inheritance chain, and no longer jump to an
  unrelated global that merely shares the member's spelling.

- **A method override no longer renders as two overloads** — hover on a class
  method that implements an interface method, or overrides a parent's, listed
  both declarations as a choice of signatures. An override shadows the
  declaration it satisfies; a parent overload with a *different* signature is
  still offered.

- **Module-level engine enums resolve (B046)** — the engine's JSON dump maps
  `namespace -> type name -> type`, so it can only carry an enum nested in a
  class. It reaches most module-level enums anyway, because a class member
  typed by one carries the enum inline, but `ESceneCharVisNotice` and
  `ESceneVehicleVisEvent` have no such member and were invisible: 55 of their
  value names got `No matching symbol` from the LSP on code the game compiles
  fine. `openplanet-lsp` now also reads `Openplanet.h` from your Openplanet
  folder, which is the only source that names them.

  The header is **optional** — an install without one behaves exactly as
  before — and the dump stays the authority for everything it can express, so
  the 57 module-level enums it already reached are untouched. `typedb refresh`
  now copies `Openplanet.h` alongside the two JSON files.

  Checked against the live game (RemoteBuild probe, Openplanet 1.29.5): it
  accepts the bare value names, and the enum name used as a type, exactly as
  the LSP now does. It rejects `Scene::`- and `Enums::`-qualified spellings
  (`Namespace 'Scene' doesn't exist.`), so no namespace is invented for these
  enums. `check` output across the false-positive fixture corpus is
  byte-identical.

- **Rename Symbol (F2) no longer rewrites unrelated identifiers (B049)** —
  `textDocument/rename` was a workspace-wide *textual* token match with no
  scope analysis, so renaming a local `x` also rewrote the outer binding it
  shadowed, every unrelated local, parameter and global named `x`, every
  `obj.x`, both sides of `A::x`, and every named-argument label `f(x: 1)`, in
  every open file. On a loop counter that silently corrupted a plugin.

  Rename now classifies the symbol under the cursor and uses one occurrence
  rule per class: a local or parameter is confined to its lexical scope and its
  own file; a free function, global, class, interface, enum or funcdef is
  renamed across the workspace but never through a `.` member access, never
  through a shadowing local, and never in a named-argument label. Class fields,
  methods, namespaced names and enum values are **refused** rather than guessed
  at, as is any new name that would be captured or collide.

  `textDocument/prepareRename` is now implemented, so an editor greys out F2
  and shows the reason instead of offering a rename that cannot be done
  correctly.
- **Find All References is scope-aware too (B049)** — it shared the same
  token scan, so it reported every same-spelled identifier in the workspace.
  It now uses the same classification and precise occurrence sets as rename.
  When resolution is indeterminate (members, qualified names, ambiguous named
  argument labels, lambdas, or an incomplete workspace closure), it returns an
  empty result rather than presenting an unmarked approximation as exact.
- **A bare value name of an unnamed engine enum is now an error (B044)** — the
  engine dump declares 132 enums inline on a member and gives them no name, so
  nothing in source can spell such an enum and nothing can reach its values by a
  bare name. The game answers `No matching symbol`; the LSP used to say nothing.
  It now rejects all 258 of those names — every value of those 132 enums except
  the ones that also belong to a NAMED enum, where a bare reference is
  legitimate and still resolves (`VCenter2`). Reaching the same values through
  the declaring member's type is untouched, and a name with a real declaration
  of its own — a type, function, virtual property, or anything you declare —
  still resolves through that.

  Checked against the live game (RemoteBuild probe, Openplanet 1.29.5): it
  rejects all 258. Dogfood: byte-identical `check` output across a 150-plugin
  sweep, so no real plugin was using one of these names. (The set was 261 when
  B044 landed; the 3 the game accepted turned out to belong to a module-level
  enum and left the set when B046 taught the loader to read `Openplanet.h`.)

  The 3-name difference is a known false positive with a separate cause.
  `DoubleJump`, `Jump` and `TechJump` belong to `Scene::ESceneCharVisNotice`,
  one of 47 MODULE-LEVEL engine enums the Nadeo dump cannot represent at all —
  52 further value names of those enums already failed to resolve, and these 3
  were resolving only by accident, through the scan this change narrows. See
  `todo-false-positives.txt`.
- **Hover shows the full signature and every overload (B033)** — hovering a
  function or method used to render `SomeType name()` with a hardcoded empty
  parameter list, or `(local) void name` with no signature at all, because the
  checked-view fast path returned before the formatter that builds a real
  signature ever ran. Hover now renders `rt Name(const string &in a, int b)`
  for workspace functions, methods (including ones inherited from a base class),
  namespaced functions, funcdefs and `receiver.Method` member calls — including
  receivers that are calls, indexing or parenthesised expressions.

  When a name has several overloads, **all** of them are listed — one signature
  per line under an `*N overloads*` header — for both workspace functions and
  type-database functions and methods; a single overload is unchanged. The
  listing is ordered by arity then text, so it does not depend on symbol-table
  or type-database insertion order.

  Type-database signatures are taken from the database's own declaration text,
  so `const` on a handle parameter survives. External type hover lists 20
  properties and 20 methods with their full signatures and says how many more
  there are, instead of silently cutting the list at 6. Enum hover shows the
  numeric values on both the workspace and the external path, and workspace
  parameter types now keep a leading `const` on a handle (`const Foo@`), which
  the parser's span dropped.

  Two limits worth knowing: an optional parameter of a workspace function
  renders as `int b = ...`, because the symbol table records *that* a
  parameter is optional but not its default expression (type-database
  signatures do show the real default); and workspace symbols still carry no
  doc comment, because symbol extraction reads the comment-masked source.
- **TUI diagnostic messages are readable at narrow widths (B029)** — the detail
  box used to hard-clip long messages at the pane edge with no `…`, so you
  could not even tell text was missing, and a long `path:line:col` could squeeze
  a compact list row's message down to `abc…`. The detail box now soft-wraps
  the message itself (the source line and caret row keep their column alignment,
  which is why the pane is still unwrapped), grows to fit, and scrolls with
  **Shift+J / Shift+K** when the message runs past the box — the title shows the
  visible range. List rows shrink the location before the message, and in
  relaxed density the selected row spills its message onto a second line. All
  widths are now measured in display columns and truncation respects grapheme
  clusters, so wide/CJK text no longer overflows and emoji are never split
  mid-sequence. A wide character before the underlined span no longer pushes the
  detail box's caret row out of alignment with it, and a message containing
  newlines now renders as separate rows instead of one run-together line.
- **Engine enums declared inline on a member are no longer dropped (B016)** —
  the game dump gives such an enum no name of its own, so the loader keyed
  every one in a module as `<Module>::UnnamedEnum` and kept only the first:
  132 enums in the shipped type DB collapsed to 14, `Control` losing 14 of its
  15. They are now keyed by their declaring context
  (`<Module>::<Class>::<Member>`), so `typedb diff` now sees drift in every
  one of them instead of 14.

  Nothing became stricter: the new keys are synthesized, name nothing the
  game accepts in source, and are held out of every source-facing lookup,
  completion and namespace query, so diagnostics on the five plugin fixtures
  are byte-identical. The one visible change is that a handful of bare
  identifiers which used to report `undefined identifier` — `MinimalReadOnly`,
  `TimeHhMmSsMil`, `Text_Icon` — no longer do. That is a *known false
  negative* being made uniform rather than fixed: the game rejects a bare
  unnamed-enum value (`No matching symbol`, RemoteBuild probe 2026-08-30, OP
  1.29.5), but the equivalent names from the 14 enums that used to win their
  module's collision were already accepted, so suppressing them all is the
  only change that does not newly reject working code. Making the checker
  match the game here is a separate change.
- **`check` no longer undoes `defines` removals of manifest defines (B026)** —
  `run_check` re-applied `info.toml`'s `[script].defines` and the
  `DEPENDENCY_*` defines AFTER `LspConfig::load` had already applied them and
  run the user's `-NAME` overrides, so `defines = ["-MANIFEST_DEF"]` worked in
  the editor but not on the command line. The duplicate apply is gone; the
  config ladder has one owner. `--define` still wins over a config removal.

- **Custom `defines` no longer wipe the game-target defines (B024)** — a
  `.openplanet-lsp.toml` (or `initializationOptions`) `defines = ["DEV"]` used
  to REPLACE the whole set, so `TMNEXT`, `WINDOWS`, `MANIA64` and the `SIG_*`
  flags vanished, every `#if TMNEXT` branch went dead and the masked-out code
  flooded undefined-identifier errors. Custom defines are now ADDITIVE over the
  target-derived set and over `info.toml` `[script].defines`, and they
  accumulate across config layers instead of the last file discarding the rest.
  To turn a define OFF, prefix it with `-` or `!` (`defines = ["-LOGS"]`);
  removals apply after additions. Listing a platform selector for another game
  (`MP4` under a TMNEXT target) still replaces the target's selectors — exactly
  one `#if <platform>` family stays live (GH #36) — and now warns that
  `game_target` is the intended way to pick a game.

### Changed
- **`source.fixAll.openplanet` now prefixes unused LOCALS with `_` (B047)** —
  previously no unused-declaration fix could be applied on save at all,
  because that producer sat outside the fix registry. Now that it is a
  registry family it can be batched, and the `_` prefix for a local is:
  mechanical, reversible, and changes no API surface. Two things are still
  never applied on save — **removing** a declaration, which is lossy, and
  prefixing a **parameter**, which renames part of the callable API since
  AngelScript has named arguments. Both remain available as ordinary quick
  fixes. Only affects users who opted into `source.fixAll.openplanet` in
  `editor.codeActionsOnSave`; it is never applied by default.
- **Quick fixes for unused declarations and misspelled names now follow the
  same rules as every other quick fix (B047)** — they are scoped to the range
  the editor asked about, and their edit is computed when the action is picked
  rather than for every action in the menu. Previously these two were wired
  into the request handler by hand, outside the fix registry, so they ignored
  the requested range and always computed their edits up front. Same fixes,
  same edits; only when and where they are offered has changed.
- **DEFAULT CHANGE: the analysis profile is now a windows/64 client (B028)** —
  the LSP used to define `MANIA64`, `MANIA32`, `WINDOWS`, `WINDOWS_WINE`,
  `LINUX`, `SERVER`, `LOGS`, `HAS_DEV` and `DEVELOPER` ALL at once, so
  mutually exclusive `#if` branches were analyzed together: a plugin
  declaring the same function under `#if MANIA32` and `#if MANIA64` got a
  phantom "A function with the same name and parameters already exists"
  error, and `#if LINUX` / `#if SERVER` code was checked on a Windows client
  that would never compile it. The defaults now model a real client — one
  OS (`windows`), one arch (`64`) — verified against the live game under
  Wine (RemoteBuild probe 2026-08-29: `WINDOWS` + `WINDOWS_WINE` +
  `MANIA64` on; `LINUX`, `MANIA32`, `SERVER`, `LOGS`, `HAS_DEV`,
  `DEVELOPER` off). If you rely on checking code that only compiles on
  Linux, on 32-bit, or in a server/logs/dev build, opt back in with
  `os_target` / `arch_target` / the build-flag keys in
  `.openplanet-lsp.toml`, pass `--os` / `--arch` to `check`, or add the
  define back with `defines = ["LINUX"]` (B024 additive overrides). The
  four build flags themselves stay ON by default (no default churn — only
  the OS/arch pairs got realistic defaults, because their exclusivity is
  the reported failure mode).
- **Config TUI names the accepted values for every setting** — a new footer
  line under the help text says how the selected value is typed in:
  `comma-separated · e.g. DEV, DEBUG_BROWSER, -LOGS` for `defines` (nothing
  previously said the editor splits on commas, let alone showed an example),
  `comma-separated groups · e.g. [A, B] | [C, D]` for the nested lists,
  `one of: …` with every choice for enums (invisible until you cycled through
  them), plus true/false, number and path hints with their defaults. The line
  is shown while browsing and while editing, and wraps when the choices are
  wider than the terminal. The `defines` help text describes the new additive
  semantics.

## [0.6.0] - 2026-08-29

### Added
- **One-time first-run modal on a bare-TTY launch (#19)** — running the bare
  binary in a real terminal with no subcommand and no saved preference used to
  jump straight into the watch TUI, surprising anyone who expected the
  language server. A centered modal now explains the default and asks:
  `t`/Enter keeps the TUI and pins `default_mode = "tui"`, `l` starts the LSP
  server on stdio and pins `default_mode = "lsp"`, `Esc`/`q` keeps the TUI
  for this session only, Ctrl-C quits and asks again next time. Every
  answered outcome also records `first_run_prompt_done` in the user-global
  config (atomic, comment-preserving), so the modal is truly one-time.
  Editors on non-TTY stdio never see it; `--lsp` / `lsp` always force the
  server; an explicit workspace `default_mode` suppresses it.
- **`errors` config + `--errors last|first|off`** — human `check` output
  (plain and pretty) groups diagnostics so a flood of warnings does not bury
  errors. Default `last`: warnings/info/hints first, then errors. `first`
  puts errors first; `off` keeps analysis order. JSON is never regrouped.
  Workspace `.openplanet-lsp.toml` wins over the user config; the flag wins
  over both.
- **`unused` config + `--unused` / `--no-unused`** — unused functions, vars,
  parameters, and class members can be `ignore`, `warn` (default), or `error`.
  `unused = "ignore"` (or `false`) in `.openplanet-lsp.toml` / the user config
  turns the noisy pass off; `--unused error` makes them fail `check` without
  `--warnings-as-errors`. Workspace wins over user; the flag wins over both.
- **`openplanet-lsp config` TUI** — edit workspace `.openplanet-lsp.toml`
  (next to `info.toml`) interactively. `--global` edits
  `~/.config/openplanet-lsp/config.toml`; `--path FILE` still wins.
  Outside a plugin and without `--global`/`--path` is a usage error
  (exit 2). Every persisted setting is listed; changing a value writes
  the file immediately (comments and unknown keys are kept).
- **Config files reload live** — editing `$HOME/.config/openplanet-lsp/config.toml`
  or a workspace `.openplanet-lsp.toml` retriggers `check --watch` and makes a
  running language server re-load the config ladder, rebuild snapshots, and
  republish diagnostics. Invalid TOML is ignored (last-good settings stay,
  with a warning) instead of crashing the session.
- **Type-DB freshness is a visible, managed pipeline** (GH #59) — every
  `check` run now states which Openplanet type database it judged the code
  against (`type db: op=1.29.5 (next, Public, 1234ad9e)  game op=…  mp=…`),
  the JSON report carries the same stamps as an additive `typedb` object,
  and the language server logs the line at init. New
  `openplanet-lsp typedb status|diff|refresh` inspects the resolved DBs,
  reports symbol-level drift between two DB directories, and harvests both
  files out of a local install — dry-run by default, `--yes` to write,
  `--into` always explicit, and never over the network. `just typedb-status`
  / `typedb-diff` / `typedb-refresh` / `typedb-refresh-apply` /
  `typedb-guard` wrap the maintainer flow, and a named CI step guards the
  shipped fixture DBs against a corrupt or truncated harvest.

### Changed
- **`.as` discovery honors `.gitignore` (GH #60)** — files excluded by
  `.gitignore` rules at or below the plugin root (plus a root-local
  `.git/info/exclude`) are no longer parsed or diagnosed. This matches
  rust-analyzer / gopls / tsc: a file you believe is excluded should not
  produce squiggles, and in real trees it drops second copies of a plugin's
  own sources (`.worktrees/`, scratch probe dirs) that collided with the
  real ones. Rules above the plugin root are never read, and a global
  gitignore / `.ignore` file is not consulted, so nothing invisible to the
  repo can shrink the checked set. `.git` directory contents are never
  discovered on either path. Opt out with `honor_gitignore = false` in
  `.openplanet-lsp.toml` (workspace wins over the user config) or
  `check --no-gitignore` (wins over both) when gitignored files are still
  compile-critical. `RUST_LOG=debug` logs how many files were excluded.

### Fixed
- **Funcdef constructors from the type DB type-check (GH #63)** —
  `UI::InputTextCallback(NameFilterCallback(function...))` — constructing a
  funcdef value from a function reference — was a false
  `No matching symbol 'UI::InputTextCallback'`. Core funcdefs now deserialize
  into the type index and `Ns::FuncdefName(fn)` is checked as a constructor
  yielding a funcdef value, matching the game.
- **`check` value-taking flags no longer swallow a following flag (GH #62)** —
  e.g. `check --typedb-dir --json` consumed `--json` as the directory (five
  flags affected, some silently). A separate value starting with `--` is now
  a usage error; the `--flag=value` form still accepts values that merely
  look like options. The same guard covers `typedb status` / `typedb refresh`.
- **Reserved keywords, same-frame duplicate locals, nested plugin dirs (game
  parity)** — `in`/`out`/`inout` used as identifiers now produce the game's
  reserved-keyword error (legal use as parameter modifiers still parses); a
  second local of the same name in the same function frame errors
  `'name' is already declared` (the first binding keeps its type so later
  member accesses cascade against it; inner-block shadowing remains a
  warning); and workspace discovery skips descendant directories that have
  their own `info.toml`, so a nested plugin no longer leaks its sources into
  the outer module.
- **"Never used" no longer fires when the only caller is in a file we
  skipped** (GH #61) — a declaration whose callers all live in own-module
  files left out of the checked set (excluded by `.gitignore`, or filtered
  out by `source_paths` / `ignore_paths`) was reported "never used". The
  game compiles those files, so the warning was wrong. They are now read
  for that one purpose: the unused pass records what they reference. Their
  declarations stay invisible, they still produce no diagnostics of their
  own, and the namespace rule above stays disarmed for them. Cost is
  proportional to how much was omitted and nothing at all when nothing was
  (`check` on a 262-file plugin: unchanged at ~0.5s, ~0.74s with a
  gitignored second copy of the whole tree beside it).
- **Unknown members of your own namespaces are now flagged** (GH #58) —
  `TournamentStation::SendEvent(...)` where nothing named `SendEvent`
  exists under that namespace is a game compile error
  (`No matching symbol 'TournamentStation::SendEvent'`) that used to pass
  silently. Fires for any namespace declared in the plugin's own module or
  in a resolved dependency's export surface, with a near-name hint when
  one is close. Stays silent whenever the symbol set is not authoritative:
  a dependency that did not resolve (optional ones included), a
  `source_paths` / `ignore_paths` filter, a `.gitignore` that actually
  excluded a source file, an engine namespace, a nested path (`A::B::Fn`),
  or a name a sibling module contributes.
- **`check` no longer joins the unused report once per file** — the
  per-file diagnostics path asked for the whole workspace's findings and
  filtered, an O(files²) join. It now asks per file: `check` on the
  tm-editor-plus-plus fixture (≈300 files) drops from ~5.0s to ~0.6s.
- **Namespace-shortcut completions no longer shuffle between runs** — the
  `UI::`/`Math::` shortcut items all share one `sortText`, so the order the
  server builds them in is what the editor displays, and it was inheriting
  `HashMap` iteration order: the same file could offer the same completions
  in a different order on each request. Function and enum shortcuts are now
  emitted in sorted qualified-name order.
- **Loading the same type DB twice yields the same index** — 66 inline
  member-enum type names collide within a Nadeo module (15 distinct
  anonymous enums are all `Control::UnnamedEnum`) and were resolved by
  `HashMap` iteration order, so two loads of identical bytes could produce
  different enum registrations. Registration now walks modules and types in
  sorted name order.
- **Workspace function return types are real types, not `<error>`** —
  free-function symbol extraction left `return_type` empty, so hovering
  a call to a workspace function (e.g. `ts_log(...)` declared in another
  file) rendered `(local) <error> name`. Extraction now records the
  declared return type and `GlobalScope::lookup_function_return` parses
  it, so hover shows `(local) void ts_log`.
- **Saving a file refreshes sibling diagnostics** — the server now
  handles `textDocument/didSave` (advertised in capabilities): the
  owning shard rebuilds and every open document in it republishes
  diagnostics, so declaring a symbol in one file clears "undefined
  identifier" squiggles in the files that use it without needing an
  edit there.
- **Plugin-root discovery follows symlinks** — opening a folder whose
  plugin dirs are symlinks (e.g. `dips++-all`, where every entry links to
  a plugin living elsewhere) discovered zero roots and degraded to a
  single manifest-less workspace: externals from the typedb still worked
  but no local types, globals, or functions resolved. Discovery now
  classifies symlinked entries via `stat` (not `lstat`) and guards
  symlink cycles with a canonical-path seen set.
- **Symlink-alias document URIs resolve to snapshot files** — a document
  opened through a symlinked path now matches the snapshot entry loaded
  via the canonical plugin root (exact URI match first, canonical-path
  fallback), so type-recorded hover, unused-declaration warnings, and
  cross-file diagnostics work for symlinked workspaces too.
- **Manifest diagnostics publish to the open document's URI** — when
  `info.toml` is open through a symlink-alias path, manifest squiggles
  now land on that URI (using the live buffer content) instead of the
  canonical alias the client may never have opened.
- **Engine callback list completed** — `OnDisabled`, `OnEnabled`,
  `OnError`, `OnMouseButton`, `OnMouseMove`, `OnKeyRelease`,
  `OnSettingsLoad`, `OnSettingsSave`, `RenderEarly`, `RenderMenuMain`,
  and `Update` are now recognized as Openplanet engine entry points and
  no longer flagged as unused declarations.

### Added
- **`unused_callbacks` config key** (GH #56) — names the
  unused-declaration pass should treat as runtime-invoked callbacks, for
  frameworks that dispatch by name reflection (MLHook's
  `OnCounterUpdate`), which a fixed list cannot cover. Entries are
  literal names or a single `*` wildcard (`On*`, `*Update`, `On*Update`,
  bare `*`) — not a glob, not a regex. Additive to the built-in
  Openplanet callback list, never a replacement; matches unqualified
  names of free functions at file top level and exempts their parameters
  too. Workspace `.openplanet-lsp.toml` wins over the user config; lists
  do not merge across levels.
- **Unused locals and parameters** (GH #55) — the unused-declaration pass
  now covers function-body bindings — "a local variable `x` is never
  used", "a parameter `p` is never used" — with the same suppressions as
  before (a leading `_`, or a trailing `// lsp: ignore unused`). Usage is
  syntactic — any later mention in the enclosing function body counts, so
  a write with no read is *used* — and the scan reads the unmasked source
  so a mention inside an inactive `#if` branch still counts. Parameters
  stay silent where the signature is imposed rather than chosen:
  Openplanet callbacks, operator overloads, both halves of a virtual pair
  (overrides and the base they override), interface implementations
  (including inherited and partially-qualified contracts), mixin members,
  lambdas, functions handed to a funcdef (handler tables, `startnew`
  coroutines), no-op stubs whose body has zero statements, public-API
  export files, and any declaration the pass already reports as unused.
  Dogfooded over the `tm-*` fixtures across three rounds — 63 sites
  classified by hand plus a mechanical screen over every emitted warning
  — with no residual false positives, so the pass is on by default.
- **Define matrix for `check`** — `define_matrix` in
  `.openplanet-lsp.toml` lists preprocessor define axes that `check`
  cross-multiplies on top of the base define set, running every combo
  through the full analysis pipeline. Diagnostics are labeled with the
  combo they fired in (`[DEV]`, `[seen in: {} | DEV]` in plain/pretty
  output; a `"combo"` array in the new `--format json` report), and
  duplicates across combos are merged into one entry. Supporting keys:
  `define_groups` (mutually-exclusive defines — combos defining two
  members of one group are skipped; the platform selectors
  `{TMNEXT, MP4, TURBO}` form an implicit group whenever ≥2 are axes),
  `define_combinations` (explicit pin list that replaces the
  cross-product entirely), and `define_matrix_max_combinations`
  (cap, default 16 — exceeding it is a hard error pointing at
  `define_combinations`). Any `--define` CLI flag collapses the matrix
  back to a single-combo run. LSP mode is untouched (GH #57).
- **Unused-declaration warnings** — never-used declarations are flagged
  with `warning` severity across the whole workspace: file-level decls
  (functions, classes, interfaces, enums and their values, funcdefs,
  globals, `import ... from` aliases) and class members (fields, methods,
  properties). Usage is name-resolved, not text-matched, so sibling-file
  references count; usage in dependency exports counts too. Exemptions:
  Openplanet engine callbacks (`Main`, `Render*`, `OnKeyPress`, ...),
  constructors/destructors, operator overloads (`op*`), `override`
  methods (any ancestor declares the member), mixin classes and their
  members, and declarations in the plugin's own `info.toml`
  `exports`/`shared_exports` files (public API). Suppress per declaration
  with a leading `_` in the name or a trailing
  `// lsp: ignore unused` comment. This initial declaration-level slice
  excluded locals and parameters; the GH #55 addition above now covers
  function-body bindings too.
- **`warnings_as_errors` config option** — persist the
  `check --warnings-as-errors` / `-Werror` behaviour in
  `.openplanet-lsp.toml` (workspace) or
  `~/.config/openplanet-lsp/config.toml` (user). Precedence: CLI flag
  (including explicit `=false`) > workspace > user > default `false`.
  Together with the new unused warnings this pins code standards per
  project (GH #48 follow-up).

- **Namespace-shortcut completions** — at identifier positions, functions
  from a configurable set of namespaces are offered fully qualified:
  typing `Beg` suggests `UI::Begin` (old vscode-openplanet-angelscript
  QoL parity). Items are labelled `NS::Member`, filter on the bare member
  name, and sort after in-scope completions. Defaults: `UI` and `Math`
  built in, plus `UX` and `MathX` as custom defaults. Config (dotfile or
  init options): `shortcut_namespaces` (replaces the UX/MathX default),
  `shortcut_disable_default_namespaces` (drop UI/Math entirely),
  `shortcut_match_enums` (also suggest enums and enum values from those
  namespaces; off by default). Namespace variables are not matchable —
  the typedb has no global-variable data source. Shortcut items carry
  `(` as a commit character, label details (params marker + return
  type), markdown documentation with all overload signatures, and a
  dedicated `zz_shortcut_` sort prefix so they always rank below
  in-scope completions.

### Fixed
- **Single-arg constructor conversions in call arguments (GH #54)** — a
  value type's one-argument constructor is an implicit conversion in
  AngelScript, so `mat4::Inverse(someMat3)` compiles through
  `mat4(const mat3&in)`. `check` no longer reports
  `argument 1 of 'Inverse': expected 'mat4', got 'mat3'` for it. The
  accepted pairs are read from the typedb's own `asBEHAVE_CONSTRUCT`
  behaviors (copy constructors excluded), so every registered conversion
  is covered — `mat3`/`mat4`/`iso3`/`iso4`/`quat`, `string`/`wstring`,
  `RGBAColor` from `vec3`/`vec4`, `IO::File` from `string`, and the rest.
  Argument types with no registered constructor still fail
  (`mat4::Inverse(someVec3)`), game-verified by RemoteBuild probe.
- **Ternary branch unification (GH #52 repro 2)** — a ternary whose
  branches have no unifiable common type now exits 1 with the game's
  exact error `Can't find unambiguous implicit conversion to make both
  expressions have the same type`. Game-verified via RemoteBuild probes
  (2026-08-18) for: `string` vs `wstring` (`c ? "" : map.MapInfo.FileName`),
  non-numeric primitive pairs (`c ? 1 : true`, `c ? "" : 5`), distinct
  math value types (`c ? nat3v : int3v`), `string` vs math value types
  (`c ? "" : vec3(1,2,3)`), and numeric vs `null` (`c ? 1 : null`).
  Ternaries now also carry their unified result type, so numeric mixes
  keep feeding downstream checks (float-unified ternaries warn on `int`
  init like the game; mixed int×uint ternaries promote to `uint` and warn
  in relational mixes like the game), while the `Signed/Unsigned
  mismatch` `+` flavor stays silent through ternary operands — both
  game-verified.
- **False-negative batch (GH #47, #49, #50, #51, #52 repro 1)** — four
  error classes the game's AngelScript compiler rejects but `check`
  silently accepted (exit 0, zero diagnostics). All game-verified via
  RemoteBuild probes; every one now exits 1 in non-watch mode:
  - Member access on engine container properties (`root.Childs[i].Visible`
    on `MwFastArray<CControlBase@>` and friends) — indexing engine array
    generics now yields the element type, so own + base-chain member
    lookups run and `'X' is not a member of 'Y'` fires (GH #50, #52).
  - Member access on `cast<T>(x)` results — the cast now types as `T@`
    instead of unknown (GH #51).
  - `int3 v = nat3expr;` and `int3(nat3expr)` — distinct math value types
    (nat2/3, int2/3, vec2/3/4, iso3/4, quat) never convert implicitly
    (GH #49).
  - Overloaded engine calls where no overload matches — e.g.
    `UI::InputText(label, str, false)` (a `bool` value binds neither the
    `int flags` nor the `bool&out changed` parameter) → `No matching
    signatures to 'UI::InputText(const string, string, const bool)'`
    (GH #47). The same rule covers unique overloads: a **bool** literal
    into an `&out` slot (`UI::Begin("w", true)`) is rejected with the
    game's qualified-name wording, while numeric literals bind `&out` via
    temporaries (`Text::TryParseFloat("1.5", 0.0)` stays legal) — both
    game-verified via RemoteBuild probes.
- **Parser parity:** `class @Name` declarations (as shipped in PluginManager)
  parse as plain `Name` — the game accepts them; we previously emitted a
  parse-error cascade (19→1 pre-existing errors on that plugin).
- **Warning parity:** `Signed/Unsigned mismatch` no longer fires when a
  relational operand contains an explicit conversion (`u < int(E::LAST)` is
  silent in-game; only variable-vs-variable mixes warn).

### Added
- **Multi-root workspaces in the language server** — an opened folder that
  contains several plugin directories (side by side or nested) is now served
  as one isolated plugin workspace per root instead of a single
  manifest-less merge. Roots are discovered by a bounded, deterministic
  downward walk per workspace folder (hidden dirs, `.git`, `node_modules`
  skipped; depth-capped), with a `plugin_roots` config override. Each shard
  carries its own config ladder and treats sibling plugins as dependencies;
  requests route to the owning shard by document URI; and `**/info.toml`
  watcher events add/remove shards at runtime, so creating or deleting a
  plugin directory updates the server without a restart. The type database
  stays global.
- **`openplanet-lsp tui [PATH]` subcommand and `help` alias** — `tui`
  launches the live watch TUI directly for the plugin root at/above PATH
  (default `.`; no plugin found is the usage-error path). `help` is a
  byte-identical alias for `--help`. Both surfaces were implied by existing
  help text but previously died as unknown commands.
- **`check --define NAME[=VALUE]` / `-D` (GH #53)** — repeatable preprocessor
  define injection on top of the manifest/config ladder. Source-tree
  `info.toml` often leaves `DEV`/`UNIT_TEST` out (build scripts inject them
  into the staged copy only), so `check` skipped `#if DEV` files entirely
  and staged-build errors stayed hidden; `openplanet-lsp check -D DEV .` now
  sees what the staged compile sees. Any `--define` also collapses a
  `define_matrix` back to a single-combo run.
- **`check --warnings-as-errors` / `-Werror` (GH #48)** — the CLI
  counterpart of the `warnings_as_errors` config option (documented below):
  any warning makes `check` exit 1.
- **Game warning-parity diagnostics (GH #37)** — RemoteBuild-verified slices
  matching the AngelScript compiler, all previously silent:
  duplicate top-level free-function declarations (same namespace-qualified
  name and parameter list) are an ERROR with the game's exact wording;
  float/double → integer initializations warn about truncation
  (`int ms = 3.7;`) and compile-time-known negative → unsigned conversions
  warn about a sign change (`uint u = -1;`); locals shadowing a name from an
  enclosing frame of the same function and unreachable code after a
  terminating statement warn like the game.
- **Mismatch-report ask** — a one-shot `check` that produced diagnostics
  ends with a dim trailer pointing at the repo issues URL ("Did Openplanet
  have different compile errors or warnings compared to openplanet-lsp
  output? Please log an issue"), and `--help` / `--version` /
  unknown-command surfaces print the repo URL with the same ask.

### Changed
- **Own `exports` files are consumer-only in the defining plugin** —
  game-verified: a file listed in `info.toml` `[script] exports` compiles
  into consumers only, so symbols it declares are not part of the defining
  plugin's own module (the game fails such cross-file uses with
  `No matching symbol`). The LSP's own-module symbol view now mirrors this;
  export files remain the public API for consumers and stay exempt from
  unused-declaration warnings.

### Fixed
- **A workspace class shadows an engine type of the same short name** — a
  plugin-declared `Status` resolved member lookups against
  `Discord::Status`, false-positiving ``type `Status` has no member X``; the
  game prefers the plugin's own declaration, and now so does the checker.
- **`@arr[i] = expr` into an indexed value-type slot is rejected (GH #44)** —
  a handle-assign whose indexed receiver is a value type (`Json::Value@ arr;
  @arr[0] = ...`) has no handle slot; the game rejects it and `check` now
  reports an invalid assignment target instead of accepting it.
- **Unresolved bare identifiers inside mixin class bodies stay silent** — a
  mixin body compiles in the consuming class's context, so bare references
  to consumer members (E++ `tabs` inside mixin `HasGroupMeta`) resolve
  in-game but were flagged `undefined identifier`; the existing bare-call
  exception now covers bare idents too.
- **Navigation through inheritance, namespaces, and imports** — `this.x`
  and implicit-this calls resolve through the base-class chain
  (goto-definition, hover); bare references inside namespaces resolve for
  goto-definition, hover, and call hierarchy; goto-definition prefers the
  real definition over an import alias; hover on implicit-this method
  callees shows the real return type and a `(method)` label instead of
  `<error>`.
- **`Ns::Fn(...)` calls that resolve nowhere diagnose with hints (GH #27,
  #28)** — a missing function in a known engine API namespace errors with
  near-name candidates (`UI::GetIO`); a namespace absent from the typedb but
  listed in the removed-API table gets the replacement hint
  (`Draw::GetWidth` → `Display::GetWidth`,
  `Draw::MeasureString` → `UI::MeasureString`); unknown user namespaces stay
  silent.
- **Writes to get-only properties and const-handle rebinds are rejected
  (game parity)** — a `get_X` without `set_X` on a typedb value type
  synthesizes a read-only property (`iso4.tx` and friends), so writes and
  compound writes report `The property has no set accessor`; re-binding a
  `const` handle is likewise an error.
- **Non-const method on a `const` receiver is an error (game parity)** —
  invoking a non-const method through a `const T &in` receiver reports
  `No matching signatures to 'T::M() const'` like the game compiler.
- **Unknown members on parentless script classes are flagged; removed-API
  calls diagnosed** — the game compiler rejects unknown fields/methods on
  workspace classes that declare no base class; `check` was silent on both
  paths (field reads and method calls) and also accepted
  `awaitable.WithRunContext(...)`, which no longer exists in the game API.
- **`game_target` drives folder and type-DB selection (GH #45)** — the
  configured target now maps to the matching install folder
  (`OpenplanetNext`/`OpenplanetMP4`/`OpenplanetTurbo`/United; MP3 and
  Nations Forever warn and fall back), auto-detection re-probes the variant
  folder after the config ladder settles the target (an explicit
  `openplanet_dir` still wins), and the JSON report derives per-variant
  typedb files (`OpenplanetTurbo.json` etc.).
- **Own manifest `exports` paths resolve through search paths** — export
  entries are module-dir relative in practice (`Exports/Callbacks.as` for
  `src/Exports/Callbacks.as`); resolving them like dependency exports makes
  the unused-declaration public-API exemption actually match (E++:
  export-file warnings 98 → 2).
- **Virtual properties are honored in member gates** — `obj.prop` is legal
  when the class declares `T get_prop()` / `set_prop(T v)`; 16 false
  positives across the real plugin fixtures are gone.
- **Qualified unknown types note the likely missing plugin export (GH #26)**
  — ``unknown type `MLFeed::PlayerCpInfo` `` now carries a note when the
  leading segment is not a known engine namespace: ensure the dependency is
  installed and visible and that its exports loaded. Bare names and
  engine-namespace typos keep the plain error.
- **Unused warnings label class members as `field`** — `Class::field`
  symbols were labeled `global`.

## [0.5.0] - 2026-08-17

Architecture batch from the `improve-codebase-architecture` review (GH #39–#43).
No new diagnostics by design — this release is about the LSP being fast and
consistent — but request latency and memory behavior improve, and hover /
completion / signature help now answer from recorded checker state instead of
re-derived parses.

### Added
- **Queryable checker interface (GH #42):** `Checker` now records
  span→type results during its normal expression walk
  (`with_type_recording` / `type_at_span` / `recorded_expr_types`), giving
  server features a single authoritative type source. Hover, completion, and
  signature help consume the recorded types (stage 2) instead of re-parsing
  and re-inferring per request.

### Changed
- **One workspace view, not N re-derivations (GH #39):** new
  `AnalysisSnapshot` module — the backend caches a single snapshot per
  document change instead of re-loading and re-parsing the whole plugin per
  keystroke and per request; the CLI `check` uses the same snapshot. Cuts
  redundant work across all 13 request handlers.
- **Every feature through the `DocumentAnalysis` seam (GH #40):** hover,
  completion, signature, inlay hints, folding, highlights, symbols,
  navigation, call hierarchy and semantic tokens all consume the same
  `&DocumentAnalysis` view (same preprocessing + AST), ending the
  "one query, three answers" divergence. (Formatter intentionally stays on
  the raw AST so `#if 0`-disabled code is never deleted on format.)
- **Deeper `GlobalScope`, closed pub-field seam (GH #41):** the
  `workspace`/`external` fields are now private; the name-resolution ladder
  and class-member/inheritance walks that server features and the checker
  previously duplicated now live behind the `GlobalScope` interface.
  `SymbolTable` lookups are indexed instead of linear scans — faster on
  large multi-file plugins.
- **Tests cross the production seam (GH #43):** test-only `Option` fallbacks
  in `WorkspaceFiles` are gone; tests exercise the same
  `TestWorkspace` → `DocumentAnalysis` → checker path production uses.

### Breaking (library API)
- `GlobalScope::workspace` / `GlobalScope::external` are private; use the
  interface methods instead.
- Feature entry points now take `&DocumentAnalysis` (and the backend cache is
  `Arc<AnalysisSnapshot>`), so downstream code calling the old signatures
  must adapt. CLI behavior is unchanged.

### Distribution
- GitHub Release binaries, npm (meta + 6 platform packages), crates.io —
  all at 0.5.0, published by `release.yml` from tag `v0.5.0`.

## [0.4.1] - 2026-08-16

### Fixed
- **#34 follow-up (review "Arm B"):** the inherited-overload augmentation in
  0.4.0 fixed a false positive but introduced a false negative — once the
  overload set held 2+ entries (own + inherited), a call whose arg count was
  outside **every** overload's range went silent (the pre-0.4.0 single-overload
  path had caught it). Now the multi-overload no-match arm diagnoses when the
  count exceeds/undershoots every overload's arity, while type-driven no-match
  and genuinely-ambiguous calls stay silent (the conservative anti-FP policy).

## [0.4.0] - 2026-08-16

A large compiler-parity and dependency-resolution batch driven by a full
plugin-fleet dogfood sweep (129 real plugins compared LSP-vs-game). Most fixes
kill false positives the LSP reported on code the game compiles clean.

### Added
- **`.op` archive dependency exports (#20):** load dependency export scripts
  directly from installed `.op` (ZIP) archives in memory, and walk
  `export_dependencies` transitively. Cross-file shared-function imports from
  `.op` deps now resolve (kills the `undefined identifier 'tabs'` class of FP).
- **`game_target`-derived preprocessor defines:** `default_defines()` no longer
  defines every ManiaPlanet platform at once. Defines now come from the game
  target (default `TMNEXT`), overridable via config file / init options /
  repo-local `.openplanet-lsp.toml`, so the LSP only compiles the `#if` branches
  the real game would (#36).
- **`source_paths` / `ignore_paths` config:** new `.openplanet-lsp.toml` keys to
  restrict which `.as` files are checked when a repo keeps non-compiled scripts
  (asset packs, fixtures, experiments) alongside the real sources. `source_paths`
  is an allowlist (wins if set), `ignore_paths` a blocklist; with neither,
  everything under the plugin root is checked (unchanged default).
- **Parser diagnostics:** error on `shared` applied to non-class items, and on
  unary `!` applied to a non-bool operand.

### Fixed
- **Dependency id resolution (#33):** resolve a dependency whose id is the
  provider's normalized display name (e.g. `BetterRoomManager` ← "Better Room
  Manager") when dir-name / `.op` / module-name all miss. Fixes `dependency not
  found` + cascading unknown-type FPs (tm-bosslike, tm-simple-room-admin).
- **Inherited method overloads (#34):** a `Ns::Class::Method(...)` call now
  counts overloads inherited from the workspace class's parent chain, so a call
  matching a parent-declared overload is no longer flagged for arity
  (tm-mlfeed-race-data).
- **Dead `#if` platform branches (#36):** TM2020 treats `#if MP4` / `#elif TURBO`
  as dead; the LSP no longer type-checks them (tm-skids-magician).
- **Typecheck FP silencing:** accept subclass args where a base param is expected
  (#22); silence `ArgTypeMismatch` on `auto`/workspace-local args (#23); silence
  spurious `MwAddRef`/`MwRelease` on `CMwNod`-derived types (#21); sibling class
  fields no longer silence undefined identifiers (#30); strip the `const`
  wrapper before the unary `!` bool check; allow `shared` on functions while
  keeping the var-decl rejection.
- **OP 1.29.5 parity (#29):** better-totd / dashboard typecheck FPs and
  catch/string handling.
- **Dependency-resolution visibility (#20 review):** surface silent
  dependency-resolution failures instead of swallowing them.

### Tooling
- **Plugin-fleet dogfood driver** (`scripts/fleet_dogfood.py`) + curated ledger
  (`docs/plugin-fleet-ledger.md`) for systematic LSP-vs-game parity sweeps.

## [0.3.4] - 2026-08-12

### Added
- **Watch TUI polish (product-ready):** pretty caret detail pane, compact/relaxed
  density (`c`), right-aligned `› fragment ‹` on relaxed rows, content-sized detail box.
- **Showcase screenshots:** real `tests/fixtures/showcase-diags` frames; README hero
  `docs/images/watch-demo.png` (relaxed + MakeTint detail).
- **Async watch checks:** background worker so `checking…` paints and the UI stays
  navigable during analysis; dirty edits coalesce to one follow-up run.
- **Stable selection** across refresh (path/range/message identity).
- **Watch health / stale:** last-good list labeled stale while checking or after
  failure; `watch off · r to refresh` when notify fails.
- **justfile** for local build/install/test/check/watch/TUI export recipes.
- README **Watch TUI** keys, watched files (`*.as`/`*.inc`/`info.toml`), exit codes.

### Changed
- Status wording: `checked in N ms` (was `last: Nms`).
- Brighter error/warning palette for dark terminals; `▸` selection marker.
- Message ellipsis; location column width capped to ~40% of list width.
- Page size follows viewport × density; Ctrl-C quits; plain keys ignore Ctrl/Alt.
- Info/Hint counts appear in the header when non-zero.
- TUI export tests are `#[ignore]` — regenerate with `just tui-frames` /
  `just tui-showcase-shots` (normal `cargo test` no longer rewrites docs).

### Fixed
- Duplicate startup check (`first_poll` + `dirty`) — exactly one initial run.
- Detail pane blank line restored between path header and source gutter.
- Fragments right-align and only truncate when they would collide with the LHS.

## [0.3.3] - 2026-08-12

### Fixed
- **crates.io**: fold the watch TUI into the main `openplanet-lsp` package
  (`src/tui/`) so Trusted Publishing does not need a separate first-time crate
  create. `cargo publish` dry-run clean.

### Changed
- Removed workspace crate `openplanet-lsp-tui` (code lives under `src/tui/`).


## [0.3.2] - 2026-08-12

### Fixed
- Attempted crates.io packaging for the watch TUI as a path dependency (incomplete —
  Trusted Publishing cannot create a new crate name). Superseded by **0.3.3**.

### Notes
- GitHub Release + npm for **0.3.2** match 0.3.1 feature set; crates.io needs **0.3.3**.


## [0.3.1] - 2026-08-12

### Added
- **Pretty `check` output** (`--format plain|pretty|auto`): source excerpts with
  gutter line numbers and caret spans under the diagnostic range. Auto uses
  pretty when stdout is color-capable; plain stays gcc-style for pipes/CI.
  No outer box frame in the CLI (chrome lives in the watch TUI).
- **`check --watch`**: live diagnostics TUI (ratatui). Re-checks the plugin on
  `*.as` / `info.toml` changes (notify + debounce). Keys: `q` quit, `j`/`k`
  scroll, `PgUp`/`PgDn`, `g`/`G` top/end, `r` refresh. Detail pane for the
  selected diagnostic.
- **Bare TTY entrypoints**: with no subcommand, a TTY inside a plugin root
  (`info.toml`) starts the watch TUI; non-TTY (editors) still starts the LSP.
  Force the server with `openplanet-lsp --lsp` or `openplanet-lsp lsp`. No
  plugin nearby → short help (exit 2). Optional path for `check` defaults to `.`.
- **Config `default_mode`**: `tui` (default) or `lsp` in
  `~/.config/openplanet-lsp/config.toml` or workspace `.openplanet-lsp.toml`.
- **Showcase fixture** `tests/fixtures/showcase-diags/`: curated multi-file
  typecheck diagnostics for screenshots and CI (~11 diags).
- **Workspace crate** `crates/openplanet-lsp-tui`: mock `TuiDataSource`,
  TestBackend + insta snapshots.
- **README hero** `docs/images/check-demo.png` (even padding via
  `scripts/pad_screenshot.py`).
- **crates.io CI**: Trusted Publishing via `rust-lang/crates-io-auth-action@v1`
  in a dedicated `publish-crates` job (no bare OIDC).

### Changed
- Interactive CLI color via `src/term.rs` (`FORCE_COLOR` / `NO_COLOR` /
  TTY). Color decisions are not OnceLock-sticky across tests.
- CI-stable optional-dependency define test (missing Editor on runners).

### Notes
- First release that exercises crates.io Trusted Publishing end-to-end when
  the publisher is configured for `release.yml`.
- Full multi-pane IDE TUI remains out of scope (see GH #9).


## [0.3.0] - 2026-08-12

### Added
- **Standalone self-update** (shipped): non-npm/cargo installs download the matching GitHub Release archive and atomically replace the running binary.
- **`update --source npm|crate|github`**: choose which channel to query for the latest version (default: npm).
- Status UX: `current: X.Y.Z (install type: standalone)` and `latest: X.Y.Z (source checked: npm)`.
- **crates.io** packaging metadata, dual Unlicense OR CC0-1.0, and CI publish via Trusted Publishing (OIDC) on release tags.

### Changed
- Cargo self-update install no longer passes `--locked`.


## [0.2.9] - 2026-08-12

### Added
- **Standalone self-update**: installs outside npm/cargo (e.g. `~/.local/bin`) download the matching GitHub Release archive, extract the binary, and atomically replace the running path. Override archive with `OPENPLANET_LSP_RELEASE_ARCHIVE` for tests.
- **crates.io**: package metadata + dual license (Unlicense OR CC0-1.0); optional CI publish via `CARGO_REGISTRY_TOKEN`.

### Changed
- Cargo self-update install no longer passes `--locked` (uses latest compatible deps from the git install path).


## [0.2.8] - 2026-08-12

### Architecture (I7 → I1 → I2+I3)

- **Shared plugin workspace load** (`workspace::load`): CLI `check` and LSP Backend share one path for plugin sources + dependency exports; required/`export_dependencies` load export symbols; optional deps when present; missing required deps reported even without configured plugin dirs.
- **DocumentAnalysis** seam: single preprocess→lex→parse module consumed by diagnostics and key handlers.
- **CallSite + callables**: `typecheck::call_site` owns arg bind and unique-arity pick; `GlobalScope::callables_free` / `callables_method` shared by checker and signature help.
- Review hardening: consistent unresolved-dependency reporting; shared workspace symbol pipeline.

### Notes

- Builds on 0.2.7 self-update work; no release-pipeline change required.


## [0.2.7] - 2026-08-12

### Added
- Self-update CLI hardening and multi package-manager support:
  `openplanet-lsp update` / `update --check` / `update --status`
- Install-method detection for **npm / pnpm / yarn / bun** (global + local),
  plus cargo / development / standalone
- Status file: `~/.config/openplanet-lsp/update-status.json`
  (`XDG_CONFIG_HOME` / `%APPDATA%` / `OPENPLANET_LSP_CONFIG_DIR`)
- Dev/CI overrides: `OPENPLANET_LSP_VERSION`, `OPENPLANET_LSP_LATEST_VERSION`,
  `OPENPLANET_LSP_UPDATE_PACKAGE`, `OPENPLANET_LSP_PACKAGE_MANAGER`,
  `OPENPLANET_LSP_EXE`
- Smoke scripts: `smoke-self-update.sh` (local tarballs) and
  `smoke-self-update-registry.sh` (registry multi-PM loop)
- CI workflow **`self-update-matrix`** (manual `workflow_dispatch` + post-tag
  release); not run on every PR

### Fixed
- Post-apply status truthfulness (`pending_restart` / `installed_version`)
- LSP auto-check refreshes on running-binary version skew; notify only on
  fresh network checks
- Windows path classification (Program Files nodejs, `\` separators)
- Atomic status write; exit code `3` when update cannot be auto-applied
- Release workflow only publishes on real non-deleted tags

### Distribution
- Multi-platform GitHub Release + npm OIDC publish

## [0.2.6] - 2026-08-12
