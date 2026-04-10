# OpenPlanet LSP — Specification

**Status:** draft-v0.2 (foundation confirmed)
**Stable path:** `specs/draft-v1.md`
**Archive:** `specs/archive/` | index: `.7/spec-drafting/archive-index.md`

## Source Inputs

- `~/scrape/openplanet/` — 858 scraped OpenPlanet doc files (API, reference, tutorials)
- `~/scrape/angelcode/` — 319 scraped AngelScript language docs
- `~/src/openplanet/` — 154 real TM plugin source trees (ecosystem survey)
- `~/src/openplanet/vscode-openplanet-angelscript/` — existing VS Code LSP (reference only; not forking)
- User input (this conversation)

**Source basis note:** Primary source evidence comes from scraped OpenPlanet developer docs and real plugin codebases. The existing VS Code LSP is used as a reference for what features exist, but not as an architectural or algorithmic influence. Claims about behavior are confirmed from source unless labeled otherwise.

---

## Section Status Table

| # | Section | Status | Notes |
|---|---------|--------|-------|
| 1 | Problem and goals | reviewed | Foundation confirmed Round 1 |
| 2 | Scope + non-goals | reviewed | All blocking decisions resolved |
| 3 | Users and actors | reviewed | Straightforward from scope |
| 4 | Core workflows | pending | |
| 5 | Concrete syntax (AngelScript/OpenPlanet) | pending | |
| 6 | Domain-specific semantics | pending | |
| 7 | Functional requirements | pending | |
| 8 | Data and state | pending | |
| 9 | Interfaces and integrations | pending | |
| 10 | UX / operator experience | pending | |
| 11 | Constraints | pending | |
| 12 | Edge cases and failure modes | pending | |
| 13 | Acceptance criteria | pending | |
| 14 | Assumptions / deferred / unresolved | drafting | Ongoing |
| 15 | Provenance and evidence | drafting | Ongoing |
| 16 | Risks | pending | |

---

## Decision Notes (summary)

- **D-001** Confirmed (user): Implementation language is **Rust**. Design priorities: fast, accurate, low memory/CPU.
- **D-002** Confirmed (user): **Fresh build**. Existing VS Code LSP is "algorithmically bad" — reference only, not a fork target.
- **D-003** Confirmed (user): **Generic stdio LSP** — editor-agnostic, works with any LSP client.
- **D-004** Confirmed (user): Real plugin source files + API docs used as test fixtures; all errors must be accounted for.
- **D-005** Confirmed (user): Primary game target **TMNEXT**. Type info JSON files share the same format across all games — LSP should support swapping game target. Preprocessor `#if` handling is the key difficulty.
- **D-006** Confirmed (user): **Web Services API is out of scope** — no native support in OpenPlanet AngelScript.
- **D-007** Confirmed (user): **info.toml must be parsed by the LSP** with error feedback (diagnostics on the TOML itself).

---

## 1. Problem and Goals

**Status:** reviewed | Provenance: Confirmed (source) + Confirmed (user)

### Problem

OpenPlanet plugin developers write AngelScript code targeting a large, evolving game API
(TrackMania / ManiaPlanet). The development experience lacks a fast, accurate language server:

- AngelScript is not a mainstream language — no general-purpose language servers cover it.
- OpenPlanet extends AngelScript with a custom preprocessor, plugin lifecycle callbacks,
  attribute syntax (`[Setting ...]`), coroutine primitives (`startnew`, `yield`),
  and ~25 API namespaces spanning 500+ documented types.
- Plugins are multi-file modules; cross-file type resolution is necessary.
- Dependencies between plugins (via `info.toml` exports/imports) create additional
  resolution scope.
- An existing VS Code extension (`vscode-openplanet-angelscript`) exists but is
  algorithmically poor — slow, resource-heavy, and not a good foundation to build on.

### Goals

1. Provide accurate, context-aware code completion for OpenPlanet API symbols.
2. Provide type-aware hover documentation.
3. Provide go-to-definition and find-references across plugin source files.
4. Surface diagnostics (type errors, undefined symbols, illegal API usage) against the
   actual OpenPlanet API.
5. Support the OpenPlanet preprocessor (`#if`/`#elif`/`#else`/`#endif` with game/platform
   defines) such that symbol visibility is correct per active define set.
6. Parse `info.toml` and provide diagnostics on it (malformed TOML, invalid fields, etc.).
7. Resolve plugin dependencies declared in `info.toml` and load their exported types.
8. Be validated against real TM plugin source trees and documented API; all errors must be
   accounted for via a fixture test suite.
9. Be fast, accurate, and low on memory and CPU usage — a principled Rust implementation.

---

## 2. Scope + Non-Goals

**Status:** reviewed | Provenance: Confirmed (user)

### In Scope

- **Language:** OpenPlanet's AngelScript dialect (not standard AngelScript, not Unreal AngelScript).
- **Implementation:** Rust, fresh build. No fork of existing tools.
- **Transport:** Generic LSP over stdio. Editor-agnostic.
- **Primary game target:** TMNEXT (TrackMania 2020).
- **Game target swapping:** The type info JSON files (`OpenplanetCore.json`, `OpenplanetNext.json`)
  share the same format across all game targets. The LSP should accept a game target configuration
  and load the corresponding type database. Swapping game targets should be straightforward.
- **Preprocessor:** `#if`/`#elif`/`#else`/`#endif` with C-like semantics. Primarily `#if` is used
  in practice. Defines are game/platform/signature-specific (e.g., `TMNEXT`, `DEV`, `MANIA64`,
  `SIG_DEVELOPER`). The LSP must evaluate these to determine symbol visibility.
- **info.toml:** Full parsing with diagnostic feedback. Validates structure, dependencies,
  exports, defines, and other fields.
- **Plugin dependencies:** Resolve `dependencies` and `optional_dependencies` from `info.toml`,
  locate dependency plugins, and load their `exports`/`shared_exports` for type resolution.
- **Test fixtures:** Built from real TM plugin source trees and real OpenPlanet API documentation.
  Every diagnostic the LSP emits against fixtures must be accounted for (true positive or
  documented known issue).

### Non-Goals

- **Web Services API (Nadeo REST endpoints):** No native OpenPlanet AngelScript support for these.
  Out of scope entirely.
- **Live game state introspection:** Out of scope for an LSP.
- **Code formatting / style enforcement:** Not requested. Deferred.
- **Forking or wrapping the existing VS Code extension:** Explicitly rejected.
- **Debugger / DAP integration:** Not mentioned. Deferred.
- **Plugin packaging / publishing:** Not an LSP concern.

---

## 3. Users and Actors

**Status:** reviewed | Provenance: Decision (drafted from confirmed inputs)

### Primary User

**OpenPlanet plugin developer** — writes AngelScript code in an LSP-capable editor
(VS Code, Neovim, Helix, etc.) targeting TrackMania via OpenPlanet.

Typical characteristics:
- Works in a plugin project directory containing `info.toml` and `.as` source files.
- May depend on other installed plugins via `info.toml` dependencies.
- Uses OpenPlanet API namespaces (`UI::`, `Net::`, `Math::`, `nvg::`, etc.) heavily.
- Uses preprocessor `#if` for conditional compilation across game targets.
- Expects fast feedback: completion as they type, instant diagnostics, quick navigation.

### Secondary Actors

- **Editor client:** Any LSP-capable editor communicating via stdio. The LSP makes no
  assumptions about the client beyond standard LSP protocol support.
- **OpenPlanet type database:** Pre-generated JSON files describing the game API.
  The LSP reads these at startup; they are not generated by the LSP itself.
- **Installed plugin ecosystem:** Other plugins on disk whose exports may need to be
  resolved for dependency type checking.

---

## 4–16. [Pending — next: Core workflows, Concrete syntax]

---

## Assumptions / Deferred / Unresolved

### Assumptions (inferred, not yet confirmed)

- A-01: The LSP follows the Language Server Protocol specification (JSON-RPC over stdio).
- A-02: `info.toml` presence defines the project root for workspace detection.
- A-03: All `.as` files under the project root are part of the compilation unit (unless excluded by config).
- A-05: `startnew()` and `yield()` are OpenPlanet-specific coroutine primitives (not standard AngelScript).
- A-06: The OpenPlanet type JSON files are provided by an OpenPlanet installation, not bundled with the LSP.

### Confirmed

- C-01: Test fixtures must use real TM plugin source files and real OpenPlanet API docs. (Confirmed (user))
- C-02: All LSP errors against fixtures must be accounted for — no silent misses. (Confirmed (user))
- C-03: OpenPlanet attribute syntax `[Setting ...]`, `[Persistent ...]`, `[SettingsTab ...]` exists in real plugins. (Confirmed (source))
- C-04: Plugin lifecycle callbacks: `Main()`, `Render()`, `RenderInterface()`, `RenderMenu()`, `RenderEarly()`, `Update(float dt)`, `OnKeyPress()`, `OnMouseButton()`, `OnMouseMove()`, `OnMouseWheel()`, `OnEnabled()`, `OnDisabled()`, `OnDestroyed()`, `OnSettingsChanged()`, `OnSettingsSave()`, `OnSettingsLoad()`, `OnLoadCallback(CMwNod@)`. (Confirmed (source))
- C-05: The `funcdef` keyword is used for first-class function type declarations. (Confirmed (source))
- C-06: `#if`/`#elif`/`#else`/`#endif` preprocessor with known defines (TMNEXT, DEV, MANIA64, etc.) (Confirmed (source))
- C-07: Implementation language is Rust. (Confirmed (user))
- C-08: Fresh build, not a fork. (Confirmed (user))
- C-09: Generic stdio LSP transport. (Confirmed (user))
- C-10: Primary target TMNEXT; game target swappable via JSON type DB. (Confirmed (user))
- C-11: Web Services API out of scope. (Confirmed (user))
- C-12: info.toml parsed with diagnostic feedback. (Confirmed (user))
- C-13: Type info JSON format is the same across all game targets. (Confirmed (user))
- C-14: Preprocessor `#if` is the primary directive used in practice. (Confirmed (user))

### Deferred

- D-01: ManiaPlanet game variants beyond TMNEXT — supported by design (swappable JSON), but not primary test target.
- D-02: Plugin signature/security mode enforcement in LSP.
- D-03: Code formatting.
- D-04: Debugger/DAP integration.
