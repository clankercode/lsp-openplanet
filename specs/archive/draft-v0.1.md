# OpenPlanet LSP — Specification

**Status:** draft-v0.1 (internal, pre-interview)
**Stable path:** `specs/draft-v1.md`
**Archive:** `specs/archive/` | index: `.7/spec-drafting/archive-index.md`

## Source Inputs

- `~/scrape/openplanet/` — 858 scraped OpenPlanet doc files (API, reference, tutorials)
- `~/scrape/angelcode/` — 319 scraped AngelScript language docs
- `~/src/openplanet/` — 154 real TM plugin source trees (ecosystem survey)
- `~/src/openplanet/vscode-openplanet-angelscript/` — existing VS Code LSP implementation
- User input (this conversation)

**Source basis note:** Primary source evidence comes from scraped OpenPlanet developer docs and real plugin codebases. Claims about behavior are confirmed from source unless labeled otherwise.

---

## Section Status Table

| # | Section | Status | Notes |
|---|---------|--------|-------|
| 1 | Problem and goals | drafting | |
| 2 | Scope + non-goals | needs-user | Language choice, fork vs. fresh TBD |
| 3 | Users and actors | pending | |
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
| 14 | Assumptions / deferred / unresolved | pending | |
| 15 | Provenance and evidence | pending | |
| 16 | Risks | pending | |

---

## Decision Notes (summary)

- **D-001** (needs-user): Implementation language — unresolved
- **D-002** (needs-user): Fork vs. fresh — vscode-openplanet-angelscript exists; relationship TBD
- **D-003** (needs-user): Target editor clients — VS Code? Neovim? Both?
- **D-004** Confirmed (user): Real plugin source files + API docs used as test fixtures; all errors must be accounted for

---

## 1. Problem and Goals

**Status:** drafting | Provenance: Confirmed (source) + Assumption (inferred)

### Problem

OpenPlanet plugin developers write AngelScript code targeting a large, evolving game API
(TrackMania / ManiaPlanet). The language and API surface have no well-supported LSP:

- AngelScript is not a mainstream language — general-purpose language servers do not cover it.
- OpenPlanet extends AngelScript with a custom preprocessor, plugin lifecycle callbacks,
  attribute syntax (`[Setting ...]`), coroutine primitives (`startnew`, `yield`),
  and ~25 API namespaces spanning 500+ documented types.
- Plugins are multi-file modules; cross-file type resolution is necessary.
- Dependencies between plugins (via `info.toml` exports/imports) create additional
  resolution scope that no existing tool handles well.
- The ecosystem has a partial VS Code extension (`vscode-openplanet-angelscript`) but its
  completeness, maintainability, and client compatibility are unconfirmed.

### Goals

1. Provide accurate, context-aware code completion for OpenPlanet API symbols.
2. Provide type-aware hover documentation.
3. Provide go-to-definition and find-references across plugin source files.
4. Surface diagnostics (type errors, undefined symbols, illegal API usage) against the
   actual OpenPlanet API — not a generic AngelScript type system.
5. Support the OpenPlanet preprocessor (`#if`/`#elif`/`#else`/`#endif` with game/platform
   defines) such that symbol visibility is correct per active define set.
6. Support plugin metadata (`info.toml`) to scope dependency resolution.
7. Be validated against real TM plugin source trees and documented API; all errors must be
   accounted for via a fixture test suite.

### Non-Goals (draft — needs confirmation)

_UNRESOLVED: Scope not yet confirmed by user. The following are inferred._

- Web Services API (Nadeo REST endpoints): large separate surface; likely deferred.
- Live game state introspection (hooking into running game): out of scope for LSP.
- Code formatting / style enforcement: not mentioned; assume deferred.
- ManiaPlanet (non-TM) game targets: may be deferred if user focuses on TMNEXT.

---

## 2. Scope + Non-Goals

**Status:** needs-user

**Blocking questions for round 1 (see interview-log.md):**

- Q1: What implementation language do you want to use?
- Q2: Fork/extend `vscode-openplanet-angelscript`, or build from scratch?
- Q3: Which editor client(s) are required — VS Code? Neovim? Both?
- Q4: Which game target is primary — TMNEXT? MP4? All?
- Q5: Is the Web Services API in scope?

---

## 3–16. [Pending]

_Remaining sections pending Round 1 user input._

---

## Assumptions / Deferred / Unresolved

### Assumptions (inferred, not yet confirmed)

- A-01: The LSP will follow the Language Server Protocol specification (JSON-RPC over stdio or TCP).
- A-02: `info.toml` presence defines the project root.
- A-03: All `.as` files under the project root are part of the compilation unit.
- A-04: The primary game target is TMNEXT (TrackMania 2020).
- A-05: `startnew()` and `yield()` are OpenPlanet-specific coroutine primitives (not standard AngelScript).

### Confirmed

- C-01: Test fixtures must use real TM plugin source files and real OpenPlanet API docs. (Confirmed (user))
- C-02: All LSP errors against fixtures must be accounted for — no silent misses. (Confirmed (user))
- C-03: OpenPlanet attribute syntax `[Setting ...]`, `[Persistent ...]`, `[SettingsTab ...]` exists in real plugins. (Confirmed (source))
- C-04: Plugin lifecycle callbacks: `Main()`, `Render()`, `RenderInterface()`, `RenderMenu()`, `RenderEarly()`, `Update(float dt)`, `OnKeyPress()`, `OnMouseButton()`, `OnMouseMove()`, `OnMouseWheel()`, `OnEnabled()`, `OnDisabled()`, `OnDestroyed()`, `OnSettingsChanged()`, `OnSettingsSave()`, `OnSettingsLoad()`, `OnLoadCallback(CMwNod@)`. (Confirmed (source))
- C-05: The `funcdef` keyword is used for first-class function type declarations. (Confirmed (source))
- C-06: `#if`/`#elif`/`#else`/`#endif` preprocessor with known defines (TMNEXT, DEV, MANIA64, etc.) (Confirmed (source))

### Deferred

- D-01: Web Services API (Nadeo endpoints) — pending scope confirmation.
- D-02: ManiaPlanet game variants beyond TMNEXT.
- D-03: Plugin signature/security mode enforcement in LSP.
